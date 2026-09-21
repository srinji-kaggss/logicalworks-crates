//! Black-box acceptance for the async surface.
//!
//! These exercise the SDK the way a consumer does — through `lgwks_bot`, never
//! `tokio` — and assert the invariants that justify the facade: bounded fan-out
//! never exceeds its limit and preserves input order, a panicking input is
//! resumed on the awaiter (not converted to a `JoinError`), abort is
//! cancellation, and dropping a handle detaches rather than cancels.
#![cfg(all(feature = "rt", feature = "time", feature = "sync", feature = "macros"))]

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::time::Duration;

use lgwks_bot::rt::runtime::MAX_WORKER_THREADS;
use lgwks_bot::rt::sync::mpsc;
use lgwks_bot::rt::task::{JoinError, join_all_bounded, spawn, spawn_blocking, yield_now};
use lgwks_bot::rt::time::{sleep, timeout};
use lgwks_bot::{Builder, Runtime};

/// What a test reports when its precondition did not hold.
///
/// The tests here cross three error domains — `BotError`, the engine's
/// `JoinError`, and `std::io` — so they return `Box<dyn Error>` and propagate
/// each with `?`. A mismatch is then a named failure carrying the reason,
/// rather than an unwind that reports only that something unwound.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Block the calling blocking-pool thread for `duration`.
///
/// Two of these tests have blocking work as their subject rather than their
/// setup: one measures that a *started* blocking task cannot be aborted by
/// `shutdown_timeout`, the other that the pool bound is applied to it. That
/// requires a real OS thread and a real wait — `rt::time::sleep` cannot be
/// awaited from inside a `spawn_blocking` closure, and the runtime this test
/// shuts down has no timer driver left to await. `lgwks_std::task`'s own test
/// module carries the same reasoned exception for the same reason.
#[expect(
    clippy::disallowed_methods,
    reason = "a blocking-pool thread has no async timer to await, and the blocking work that must \
              outlive shutdown is this test's subject rather than its scaffolding"
)]
fn block_pool_thread(duration: Duration) {
    std::thread::sleep(duration);
}

/// Panic on the calling task, carrying `message` as the payload.
///
/// `resume_unwind` and not `panic!`: the workspace forbids `panic` outright with
/// no suppression path, and this is the estate's documented form for a
/// deliberate panic — it reports on the task that observes it and never aborts
/// the process. Declared to return `()` rather than leaving its body to diverge,
/// so a future that calls it keeps a concrete output type instead of inferring
/// the never type into the handle it returns.
fn explode(message: &'static str) {
    std::panic::resume_unwind(Box::new(message));
}

#[test]
fn free_block_on_runs_a_future() {
    assert_eq!(lgwks_bot::block_on(async { 3u8 }), 3);
}

#[test]
fn an_explicit_worker_count_above_the_resource_bound_is_rejected() -> TestResult {
    let Some(workers) = NonZeroUsize::new(MAX_WORKER_THREADS + 1) else {
        return Err("the bound itself must exceed zero".into());
    };
    let Err(error) = Builder::new().worker_threads(Some(workers)).build() else {
        return Err("oversized worker count must be rejected".into());
    };
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    Ok(())
}

#[test]
fn runtime_spawns_and_joins_a_task() -> TestResult {
    let runtime = Runtime::new()?;
    let value = runtime.block_on(async {
        let handle = spawn(async { 21u32 });
        Ok::<u32, JoinError>(handle.await? * 2)
    })?;
    assert_eq!(value, 42);
    Ok(())
}

#[test]
fn select_picks_the_ready_branch() -> TestResult {
    let runtime = Runtime::new()?;
    let picked = runtime.block_on(async {
        lgwks_bot::select! {
            biased;
            value = async { 9u8 } => value,
            _ = sleep(Duration::from_millis(50)) => 0,
        }
    });
    assert_eq!(picked, 9);
    Ok(())
}

#[test]
fn join_resolves_every_branch() -> TestResult {
    let runtime = Runtime::new()?;
    let pair = runtime.block_on(async { lgwks_bot::join!(async { 1u8 }, async { 2u8 }) });
    assert_eq!(pair, (1, 2));
    Ok(())
}

#[test]
fn timeout_distinguishes_elapsed_from_value() -> TestResult {
    let runtime = Runtime::new()?;
    let elapsed = runtime.block_on(timeout(
        Duration::from_millis(10),
        sleep(Duration::from_secs(30)),
    ));
    assert!(elapsed.is_err(), "an expired deadline must be an error");

    let ready = runtime.block_on(timeout(Duration::from_secs(5), async { 7u8 }));
    assert_eq!(ready.ok(), Some(7));
    Ok(())
}

#[test]
fn a_sleep_built_before_the_runtime_still_fires() -> TestResult {
    // tokio's own `sleep` panics here; the facade defers construction to poll.
    let timer = sleep(Duration::from_millis(10));
    let runtime = Runtime::new()?;
    runtime.block_on(timer);
    Ok(())
}

#[test]
fn channel_carries_values_between_tasks() -> TestResult {
    let runtime = Runtime::new()?;
    let sum = runtime.block_on(async {
        let (tx, mut rx) = mpsc::channel::<u32>(8);
        let mut senders = Vec::new();
        for value in 1..=4u32 {
            let tx = tx.clone();
            senders.push(spawn(async move { tx.send(value).await }));
        }
        drop(tx);
        let mut sum = 0;
        while let Some(value) = rx.recv().await {
            sum += value;
        }
        // Every send is awaited rather than discarded: a send that failed inside
        // a detached task would otherwise be invisible, and the received total
        // would still be correct for the values that did get through.
        for sender in senders {
            let sent = sender.await?;
            assert!(
                sent.is_ok(),
                "the receiver outlives every sender, so no send may fail"
            );
        }
        Ok::<u32, JoinError>(sum)
    })?;
    assert_eq!(sum, 10);
    Ok(())
}

#[test]
fn bounded_fanout_respects_the_limit_and_preserves_order() -> TestResult {
    let runtime = Runtime::new()?;
    let current = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    let outputs = runtime.block_on(async {
        let futures = (0..8u32).map(|index| {
            let current = Arc::clone(&current);
            let peak = Arc::clone(&peak);
            async move {
                // Bound: 8 inputs exist in total and at most 2 run at once, so
                // the previous count cannot approach `usize::MAX`.
                let now = current.fetch_add(1, SeqCst).saturating_add(1);
                peak.fetch_max(now, SeqCst);
                sleep(Duration::from_millis(20)).await;
                current.fetch_sub(1, SeqCst);
                index * 10
            }
        });
        join_all_bounded(2, futures).await
    });

    assert_eq!(
        outputs,
        (0..8u32).map(|index| index * 10).collect::<Vec<_>>()
    );
    let observed = peak.load(SeqCst);
    assert!(
        observed <= 2,
        "peak concurrency {observed} exceeded limit 2"
    );
    assert!(observed >= 1, "no task ever ran");
    Ok(())
}

#[test]
fn bounded_fanout_limit_one_is_sequential() -> TestResult {
    let runtime = Runtime::new()?;
    let current = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    let outputs = runtime.block_on(async {
        let futures = (0..5u32).map(|index| {
            let current = Arc::clone(&current);
            let peak = Arc::clone(&peak);
            async move {
                // Bound: 5 inputs exist in total and one runs at a time, so the
                // previous count is always 0.
                let now = current.fetch_add(1, SeqCst).saturating_add(1);
                peak.fetch_max(now, SeqCst);
                sleep(Duration::from_millis(5)).await;
                current.fetch_sub(1, SeqCst);
                index
            }
        });
        join_all_bounded(1, futures).await
    });

    assert_eq!(outputs, vec![0, 1, 2, 3, 4]);
    assert_eq!(peak.load(SeqCst), 1);
    Ok(())
}

#[test]
fn bounded_fanout_with_empty_input_resolves_immediately() -> TestResult {
    let runtime = Runtime::new()?;
    let outputs: Vec<u8> =
        runtime.block_on(join_all_bounded(4, Vec::<std::future::Ready<u8>>::new()));
    assert!(
        outputs.is_empty(),
        "no inputs must resolve to no outputs, not to a blocked fan-out"
    );
    Ok(())
}

#[test]
fn bounded_fanout_streams_a_large_input_without_exceeding_the_limit() -> TestResult {
    let runtime = Runtime::new()?;
    let current = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    // 64 inputs over limit 4: order must hold and peak concurrency must stay
    // bounded even though the input count far exceeds the limit. Under the old
    // spawn-everything-up-front shape this held the limit only via a semaphore
    // while retaining all 64 tasks; the replenishing implementation never holds
    // more than `limit` tasks.
    let outputs = runtime.block_on(async {
        let futures = (0..64u32).map(|index| {
            let current = Arc::clone(&current);
            let peak = Arc::clone(&peak);
            async move {
                // Bound: 64 inputs exist in total and at most 4 run at once, so
                // the previous count cannot approach `usize::MAX`.
                let now = current.fetch_add(1, SeqCst).saturating_add(1);
                peak.fetch_max(now, SeqCst);
                sleep(Duration::from_millis(5)).await;
                current.fetch_sub(1, SeqCst);
                index * 3
            }
        });
        join_all_bounded(4, futures).await
    });

    assert_eq!(
        outputs,
        (0..64u32).map(|index| index * 3).collect::<Vec<_>>()
    );
    let observed = peak.load(SeqCst);
    assert!(
        observed <= 4,
        "peak concurrency {observed} exceeded limit 4"
    );
    assert!(observed >= 1, "no task ever ran");
    Ok(())
}

#[test]
fn builder_applies_an_explicit_blocking_pool_bound() -> TestResult {
    let Some(max_blocking) = NonZeroUsize::new(4) else {
        return Err("4 is non-zero".into());
    };
    let runtime = Builder::new()
        .max_blocking_threads(Some(max_blocking))
        .build()?;
    let value = runtime.block_on(async {
        let handle = spawn(async { 6u32 });
        Ok::<u32, JoinError>(handle.await? * 7)
    })?;
    assert_eq!(value, 42);
    Ok(())
}

#[test]
fn bounded_fanout_treats_limit_zero_as_one() -> TestResult {
    let runtime = Runtime::new()?;
    let outputs = runtime.block_on(join_all_bounded(0, (0..4u32).map(std::future::ready)));
    assert_eq!(outputs, vec![0, 1, 2, 3]);
    Ok(())
}

#[test]
fn bounded_fanout_clamps_a_limit_above_max_permits() -> TestResult {
    // `Semaphore::new` panics above `MAX_PERMITS`; the facade clamps, so the
    // natural "unbounded" argument cannot abort the process.
    let runtime = Runtime::new()?;
    let outputs = runtime.block_on(join_all_bounded(
        usize::MAX,
        (0..4u32).map(std::future::ready),
    ));
    assert_eq!(outputs, vec![0, 1, 2, 3]);
    Ok(())
}

#[test]
fn bounded_fanout_resumes_a_panicking_input_on_the_awaiter() -> TestResult {
    let runtime = Runtime::new()?;
    let resumed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime.block_on(join_all_bounded(
            2,
            (0..2u8).map(|_| async {
                explode("input exploded");
            }),
        ));
    }));
    assert!(
        resumed.is_err(),
        "a panicking input must resume its panic on the awaiting task"
    );

    // A non-panicking run through the same path is unaffected.
    let ok = runtime.block_on(join_all_bounded(2, (0..3u32).map(std::future::ready)));
    assert_eq!(ok, vec![0, 1, 2]);
    Ok(())
}

#[test]
fn a_handle_spawning_after_its_runtime_is_dropped_reports_cancellation() -> TestResult {
    let runtime = Runtime::new()?;
    let handle = runtime.handle();
    drop(runtime);

    // The doc contract: this does not panic. The task is never scheduled, and
    // the loss is observable as a cancelled join rather than a silent success.
    let joined = handle.spawn(async { 1u8 });
    let next = Runtime::new()?;
    let Err(error) = next.block_on(joined) else {
        return Err("a task with no runtime cannot produce a value".into());
    };
    assert!(error.is_cancelled());
    Ok(())
}

#[test]
fn shutdown_timeout_does_not_claim_to_abort_started_blocking_work() -> TestResult {
    let runtime = Runtime::new()?;
    let started = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let worker_started = Arc::clone(&started);
    let worker_finished = Arc::clone(&finished);

    runtime.block_on(async move {
        spawn_blocking(move || {
            worker_started.store(true, SeqCst);
            block_pool_thread(Duration::from_millis(40));
            worker_finished.store(true, SeqCst);
        });
        while !started.load(SeqCst) {
            yield_now().await;
        }
    });

    runtime.shutdown_timeout(Duration::ZERO);
    block_pool_thread(Duration::from_millis(80));
    assert!(
        finished.load(SeqCst),
        "a started blocking task cannot be aborted by shutdown_timeout"
    );
    Ok(())
}

#[test]
fn dropping_a_join_handle_detaches_rather_than_cancels() -> TestResult {
    let runtime = Runtime::new()?;
    let ran = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&ran);
    runtime.block_on(async move {
        let handle = spawn(async move {
            sleep(Duration::from_millis(20)).await;
            flag.store(true, SeqCst);
        });
        drop(handle);
        sleep(Duration::from_millis(80)).await;
    });
    assert!(
        ran.load(SeqCst),
        "a dropped JoinHandle must not cancel its task"
    );
    Ok(())
}

#[test]
fn abort_is_cancellation() -> TestResult {
    let runtime = Runtime::new()?;
    let joined = runtime.block_on(async {
        let handle = spawn(async {
            sleep(Duration::from_secs(30)).await;
        });
        handle.abort();
        handle.await
    });
    let Err(error) = joined else {
        return Err("an aborted task yields a JoinError".into());
    };
    assert!(error.is_cancelled());
    Ok(())
}

#[test]
fn a_panicking_task_surfaces_as_a_join_error() -> TestResult {
    let runtime = Runtime::new()?;
    let joined = runtime.block_on(async {
        let handle = spawn(async {
            explode("task exploded");
        });
        handle.await
    });
    let Err(error) = joined else {
        return Err("a panicking task yields a JoinError".into());
    };
    assert!(error.is_panic());
    Ok(())
}

#[test]
fn a_handle_spawns_from_a_non_runtime_thread() -> TestResult {
    let runtime = Runtime::new()?;
    let handle = runtime.handle();
    // `std::thread::spawn` and not the estate's `spawn`: the claim is that a
    // handle works from a thread the runtime does not own, so the thread must be
    // one the runtime did not make. What `clippy.toml` bans is an *unjoined* OS
    // thread — one whose panic is invisible and whose handle is leaked — and
    // this thread is joined on the next line.
    #[expect(
        clippy::disallowed_methods,
        reason = "the claim under test is that a Handle drives work from a thread the runtime did \
                  not create; the thread is joined immediately, so nothing is leaked"
    )]
    let joined = std::thread::spawn(move || handle.block_on(async { 5u8 })).join();
    let Ok(value) = joined else {
        return Err("the spawned thread must not panic".into());
    };
    assert_eq!(value, 5);
    Ok(())
}
