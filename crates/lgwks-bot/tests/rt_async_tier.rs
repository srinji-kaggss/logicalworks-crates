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
use lgwks_bot::rt::task::{join_all_bounded, spawn, spawn_blocking, yield_now};
use lgwks_bot::rt::time::{sleep, timeout};
use lgwks_bot::{Builder, Runtime};

#[test]
fn free_block_on_runs_a_future() {
    assert_eq!(lgwks_bot::block_on(async { 3u8 }), 3);
}

#[test]
fn an_explicit_worker_count_above_the_resource_bound_is_rejected() {
    let workers = NonZeroUsize::new(MAX_WORKER_THREADS + 1).expect("nonzero");
    let result = Builder::new().worker_threads(Some(workers)).build();
    let error = match result {
        Ok(_) => panic!("oversized worker count must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn runtime_spawns_and_joins_a_task() {
    let runtime = Runtime::new().expect("runtime");
    let value = runtime.block_on(async {
        let handle = spawn(async { 21u32 });
        handle.await.expect("task did not panic") * 2
    });
    assert_eq!(value, 42);
}

#[test]
fn select_picks_the_ready_branch() {
    let runtime = Runtime::new().expect("runtime");
    let picked = runtime.block_on(async {
        lgwks_bot::select! {
            biased;
            value = async { 9u8 } => value,
            _ = sleep(Duration::from_millis(50)) => 0,
        }
    });
    assert_eq!(picked, 9);
}

#[test]
fn join_resolves_every_branch() {
    let runtime = Runtime::new().expect("runtime");
    let pair = runtime.block_on(async { lgwks_bot::join!(async { 1u8 }, async { 2u8 }) });
    assert_eq!(pair, (1, 2));
}

#[test]
fn timeout_distinguishes_elapsed_from_value() {
    let runtime = Runtime::new().expect("runtime");
    let elapsed = runtime.block_on(timeout(
        Duration::from_millis(10),
        sleep(Duration::from_secs(30)),
    ));
    assert!(elapsed.is_err(), "an expired deadline must be an error");

    let ready = runtime.block_on(timeout(Duration::from_secs(5), async { 7u8 }));
    assert_eq!(ready.ok(), Some(7));
}

#[test]
fn a_sleep_built_before_the_runtime_still_fires() {
    // tokio's own `sleep` panics here; the facade defers construction to poll.
    let timer = sleep(Duration::from_millis(10));
    let runtime = Runtime::new().expect("runtime");
    runtime.block_on(timer);
}

#[test]
fn channel_carries_values_between_tasks() {
    let runtime = Runtime::new().expect("runtime");
    let sum = runtime.block_on(async {
        let (tx, mut rx) = mpsc::channel::<u32>(8);
        for value in 1..=4u32 {
            let tx = tx.clone();
            spawn(async move {
                tx.send(value).await.expect("receiver is alive");
            });
        }
        drop(tx);
        let mut sum = 0;
        while let Some(value) = rx.recv().await {
            sum += value;
        }
        sum
    });
    assert_eq!(sum, 10);
}

#[test]
fn bounded_fanout_respects_the_limit_and_preserves_order() {
    let runtime = Runtime::new().expect("runtime");
    let current = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    let outputs = runtime.block_on(async {
        let futures = (0..8u32).map(|index| {
            let current = Arc::clone(&current);
            let peak = Arc::clone(&peak);
            async move {
                let now = current.fetch_add(1, SeqCst) + 1;
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
}

#[test]
fn bounded_fanout_limit_one_is_sequential() {
    let runtime = Runtime::new().expect("runtime");
    let current = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    let outputs = runtime.block_on(async {
        let futures = (0..5u32).map(|index| {
            let current = Arc::clone(&current);
            let peak = Arc::clone(&peak);
            async move {
                let now = current.fetch_add(1, SeqCst) + 1;
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
}

#[test]
fn bounded_fanout_with_empty_input_resolves_immediately() {
    let runtime = Runtime::new().expect("runtime");
    let outputs: Vec<u8> =
        runtime.block_on(join_all_bounded(4, Vec::<std::future::Ready<u8>>::new()));
    assert!(outputs.is_empty());
}

#[test]
fn bounded_fanout_treats_limit_zero_as_one() {
    let runtime = Runtime::new().expect("runtime");
    let outputs = runtime.block_on(join_all_bounded(0, (0..4u32).map(std::future::ready)));
    assert_eq!(outputs, vec![0, 1, 2, 3]);
}

#[test]
fn bounded_fanout_clamps_a_limit_above_max_permits() {
    // `Semaphore::new` panics above `MAX_PERMITS`; the facade clamps, so the
    // natural "unbounded" argument cannot abort the process.
    let runtime = Runtime::new().expect("runtime");
    let outputs = runtime.block_on(join_all_bounded(
        usize::MAX,
        (0..4u32).map(std::future::ready),
    ));
    assert_eq!(outputs, vec![0, 1, 2, 3]);
}

#[test]
fn bounded_fanout_resumes_a_panicking_input_on_the_awaiter() {
    let runtime = Runtime::new().expect("runtime");
    let resumed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime.block_on(join_all_bounded(
            2,
            (0..2u8).map(|_| async { panic!("input exploded") }),
        ));
    }));
    assert!(
        resumed.is_err(),
        "a panicking input must resume its panic on the awaiting task"
    );

    // A non-panicking run through the same path is unaffected.
    let ok = runtime.block_on(join_all_bounded(2, (0..3u32).map(std::future::ready)));
    assert_eq!(ok, vec![0, 1, 2]);
}

#[test]
fn a_handle_spawning_after_its_runtime_is_dropped_reports_cancellation() {
    let runtime = Runtime::new().expect("runtime");
    let handle = runtime.handle();
    drop(runtime);

    // The doc contract: this does not panic. The task is never scheduled, and
    // the loss is observable as a cancelled join rather than a silent success.
    let joined = handle.spawn(async { 1u8 });
    let next = Runtime::new().expect("runtime");
    let error = next
        .block_on(joined)
        .expect_err("a task with no runtime cannot produce a value");
    assert!(error.is_cancelled());
}

#[test]
fn shutdown_timeout_does_not_claim_to_abort_started_blocking_work() {
    let runtime = Runtime::new().expect("runtime");
    let started = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let worker_started = Arc::clone(&started);
    let worker_finished = Arc::clone(&finished);

    runtime.block_on(async move {
        spawn_blocking(move || {
            worker_started.store(true, SeqCst);
            std::thread::sleep(Duration::from_millis(40));
            worker_finished.store(true, SeqCst);
        });
        while !started.load(SeqCst) {
            yield_now().await;
        }
    });

    runtime.shutdown_timeout(Duration::ZERO);
    std::thread::sleep(Duration::from_millis(80));
    assert!(
        finished.load(SeqCst),
        "a started blocking task cannot be aborted by shutdown_timeout"
    );
}

#[test]
fn dropping_a_join_handle_detaches_rather_than_cancels() {
    let runtime = Runtime::new().expect("runtime");
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
}

#[test]
fn abort_is_cancellation() {
    let runtime = Runtime::new().expect("runtime");
    let error = runtime.block_on(async {
        let handle = spawn(async {
            sleep(Duration::from_secs(30)).await;
        });
        handle.abort();
        handle
            .await
            .expect_err("an aborted task yields a JoinError")
    });
    assert!(error.is_cancelled());
}

#[test]
fn a_panicking_task_surfaces_as_a_join_error() {
    let runtime = Runtime::new().expect("runtime");
    let error = runtime.block_on(async {
        let handle = spawn(async {
            panic!("task exploded");
        });
        handle
            .await
            .expect_err("a panicking task yields a JoinError")
    });
    assert!(error.is_panic());
}

#[test]
fn a_handle_spawns_from_a_non_runtime_thread() {
    let runtime = Runtime::new().expect("runtime");
    let handle = runtime.handle();
    let value = std::thread::spawn(move || handle.block_on(async { 5u8 }))
        .join()
        .expect("thread did not panic");
    assert_eq!(value, 5);
}
