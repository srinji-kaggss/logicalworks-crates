//! Black-box acceptance for the async surface.
//!
//! These exercise the SDK the way a consumer does, through `lgwks_bot` and never
//! `tokio`, and assert the invariants that justify the facade: bounded fan-out
//! never exceeds its limit and preserves input order, a panicking input is
//! resumed on the awaiter (not converted to a `JoinError`), abort is
//! cancellation, and nothing this crate hands out can be started and then
//! forgotten.
//!
//! # What this file can no longer test, and why
//!
//! Two tests were removed with the API they exercised, rather than rewritten
//! around it. Both measured a *droppable* detached task, which is the shape the
//! crate no longer has:
//!
//! - "a handle spawning after its runtime is dropped reports cancellation"
//!   tested `Handle::spawn`. `Handle` can now only *drive* work
//!   ([`lgwks_bot::rt::runtime::Handle::block_on`]), so the case cannot arise.
//! - "shutdown_timeout does not claim to abort started blocking work" needed a
//!   public `spawn_blocking`, whose handle was droppable. Nothing on the public
//!   surface starts work on the runtime's blocking pool any more, so the
//!   contract `Runtime::shutdown_timeout` documents is no longer observable
//!   from outside the crate. It is still the engine's behaviour and still
//!   documented; it is simply no longer reachable by a caller, which is a
//!   coverage loss recorded here rather than a test quietly weakened.
#![cfg(all(
    feature = "rt",
    feature = "time",
    feature = "sync",
    feature = "macros",
    feature = "io"
))]

use std::cell::Cell;
use std::future::Future;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::time::Duration;

use lgwks_bot::rt::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter, duplex};
use lgwks_bot::rt::runtime::MAX_WORKER_THREADS;
use lgwks_bot::rt::supervise::{Supervisor, TaskOutcome};
use lgwks_bot::rt::sync::{CancellationToken, mpsc};
use lgwks_bot::rt::task::{JoinError, JoinSet, join_all_bounded};
use lgwks_bot::rt::time::{sleep, timeout};
use lgwks_bot::{Builder, Runtime};

/// What a test reports when its precondition did not hold.
///
/// The tests here cross three error domains (`BotError`, the engine's
/// `JoinError`, and `std::io`), so they return `Box<dyn Error>` and propagate
/// each with `?`. A mismatch is then a named failure carrying the reason,
/// rather than an unwind that reports only that something unwound.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Panic on the calling task, carrying `message` as the payload.
///
/// `resume_unwind` and not `panic!`: the workspace forbids `panic` outright with
/// no suppression path, and this is the documented form for a
/// deliberate panic: it reports on the task that observes it and never aborts
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
        let mut tasks = JoinSet::new();
        tasks.spawn(async { 21u32 });
        let Some(joined) = tasks.join_next().await else {
            return Err("the set must yield the one task it holds".into());
        };
        Ok::<u32, Box<dyn std::error::Error>>(joined? * 2)
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
        let mut senders = JoinSet::new();
        for value in 1..=4u32 {
            let tx = tx.clone();
            senders.spawn(async move { tx.send(value).await });
        }
        drop(tx);
        let mut sum = 0;
        while let Some(value) = rx.recv().await {
            sum += value;
        }
        // Every send is joined rather than discarded: a send that failed inside
        // a task nobody joined would otherwise be invisible, and the received
        // total would still be correct for the values that did get through.
        while let Some(sent) = senders.join_next().await {
            let sent = sent?;
            assert!(
                sent.is_ok(),
                "the receiver outlives every sender, so no send may fail"
            );
        }
        Ok::<u32, Box<dyn std::error::Error>>(sum)
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

/// The blocking-pool knob is accepted, and a runtime built with it drives work.
///
/// What this no longer measures is the bound itself. It used to observe the
/// ceiling by starting `spawn_blocking` calls on the runtime's pool, and that
/// entry point is gone with the rest of the droppable-handle surface, so the
/// pool is no longer reachable from outside the crate — the only remaining user
/// is the `fs` driver. The knob still bounds exactly that pool, which is what
/// its documentation now says; this test covers the part a caller can still
/// see, which is that a runtime built with the knob set is a working runtime.
#[test]
fn builder_accepts_a_blocking_pool_bound_and_still_drives_tasks() -> TestResult {
    let Some(max_blocking) = NonZeroUsize::new(4) else {
        return Err("4 is non-zero".into());
    };
    let runtime = Builder::new()
        .max_blocking_threads(Some(max_blocking))
        .build()?;
    let value = runtime.block_on(async {
        let mut tasks = JoinSet::new();
        tasks.spawn(async { 6u32 });
        let Some(joined) = tasks.join_next().await else {
            return Err("the set must yield the one task it holds".into());
        };
        Ok::<u32, Box<dyn std::error::Error>>(joined? * 7)
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

/// A tracked set is a cancellation, not a detach: dropping it stops its tasks.
///
/// This is the replacement for the test that pinned the old behaviour
/// (`dropping a JoinHandle detaches rather than cancels`) — the handle that
/// behaved that way no longer exists. The property still worth pinning is the
/// opposite one, because it is what makes the set a safe container: a caller who
/// lets the set go does not leave work running behind them. The task below would
/// set `ran` at 20 ms if it were detached, and never reaches it.
#[test]
fn dropping_a_tracked_set_cancels_its_tasks() -> TestResult {
    let runtime = Runtime::new()?;
    let ran = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&ran);
    runtime.block_on(async move {
        let mut tasks = JoinSet::new();
        tasks.spawn(async move {
            sleep(Duration::from_millis(20)).await;
            flag.store(true, SeqCst);
        });
        drop(tasks);
        // Well past the deadline the task would have needed, so a detach would
        // be observable rather than a race.
        sleep(Duration::from_millis(80)).await;
    });
    assert!(
        !ran.load(SeqCst),
        "dropping a JoinSet must cancel its tasks, not detach them"
    );
    Ok(())
}

#[test]
fn abort_is_cancellation() -> TestResult {
    let runtime = Runtime::new()?;
    let joined = runtime.block_on(async {
        let mut tasks = JoinSet::new();
        let handle = tasks.spawn(async {
            sleep(Duration::from_secs(30)).await;
        });
        handle.abort();
        tasks.join_next().await
    });
    let Some(Err(error)) = joined else {
        return Err("an aborted task yields a JoinError".into());
    };
    assert!(error.is_cancelled());
    Ok(())
}

#[test]
fn a_panicking_task_surfaces_as_a_join_error() -> TestResult {
    let runtime = Runtime::new()?;
    let joined = runtime.block_on(async {
        let mut tasks = JoinSet::new();
        tasks.spawn(async {
            explode("task exploded");
        });
        tasks.join_next().await
    });
    let Some(Err(error)) = joined else {
        return Err("a panicking task yields a JoinError".into());
    };
    assert!(error.is_panic());
    Ok(())
}

#[test]
fn a_handle_drives_work_from_a_non_runtime_thread() -> TestResult {
    let runtime = Runtime::new()?;
    let handle = runtime.handle();
    // `std::thread::spawn` and not a task: the claim is that a handle works from
    // a thread the runtime does not own, so the thread must be one the runtime
    // did not make. What `clippy.toml` bans is an *unjoined* OS thread — one
    // whose panic is invisible and whose handle is leaked — and this thread is
    // joined on the next line.
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

/// The cancellation primitive, exercised the way a supervisor uses it.
///
/// Every background task in this crate is tracked in a `JoinSet` and listens to
/// a `CancellationToken`. Before this test existed the second half
/// of that rule had no implementation to point at, so the rule was unenforceable
/// rather than merely unenforced.
#[test]
fn a_cancelled_token_stops_every_task_in_a_join_set() -> TestResult {
    const TASKS: usize = 32;
    let runtime = Runtime::new()?;
    let finished = Arc::new(AtomicUsize::new(0));
    let token = CancellationToken::new();

    let completed = runtime.block_on({
        let finished = Arc::clone(&finished);
        // `token` is moved in rather than cloned: the supervisor owns it, and no
        // handle outside this block needs it afterwards.
        async move {
            let mut tasks = JoinSet::new();
            for _ in 0..TASKS {
                let waiting = token.clone();
                let counter = Arc::clone(&finished);
                tasks.spawn(async move {
                    // `cancelled_owned` and not `cancelled`: a spawned task must
                    // own everything it captures and outlive this scope, and
                    // `cancelled` borrows the token.
                    //
                    // The 50 ms below is long enough that the cancel provably
                    // arrives while these are parked, not after they returned.
                    waiting.cancelled_owned().await;
                    counter.fetch_add(1, SeqCst);
                });
            }

            // Let every task reach its wait, then cancel from the same runtime
            // but a different task than any waiter — which is the shape a
            // supervisor actually has.
            sleep(Duration::from_millis(50)).await;
            token.cancel();

            while let Some(joined) = tasks.join_next().await {
                joined?;
            }
            Ok::<usize, JoinError>(finished.load(SeqCst))
        }
    })?;

    assert_eq!(
        completed, TASKS,
        "one cancel must release every parked task"
    );
    Ok(())
}

/// A child token is cancellable on its own, and cancelling it must not disturb
/// the sibling that shares its parent.
#[test]
fn cancelling_one_child_leaves_its_sibling_running() -> TestResult {
    let runtime = Runtime::new()?;
    let parent = CancellationToken::new();
    let stopped = parent.child_token();
    let surviving = parent.child_token();
    let ran_to_completion = Arc::new(AtomicBool::new(false));

    let outcome = runtime.block_on({
        let ran_to_completion = Arc::clone(&ran_to_completion);
        // Two clones: one the supervisor cancels, one the task waits on. They
        // are the same token, so the cancel reaches the waiter.
        let controller = stopped.clone();
        let surviving_still_live = surviving.clone();
        // `parent` is moved rather than cloned: it is needed only inside this
        // block, to release the sibling once the claim has been asserted.
        async move {
            let mut tasks = JoinSet::new();
            tasks.spawn(async move {
                stopped.cancelled_owned().await;
                "stopped"
            });
            tasks.spawn(async move {
                surviving.cancelled_owned().await;
                ran_to_completion.store(true, SeqCst);
                "survivor"
            });

            sleep(Duration::from_millis(50)).await;
            // Only the one child: a supervisor abandoning a single subtask must
            // not take its siblings with it.
            controller.cancel();

            // The sibling must still be live *after* its peer was cancelled —
            // that is the claim. It is asserted here rather than after the join
            // because the join below deliberately does not wait for it.
            let sibling_survived = !surviving_still_live.is_cancelled();

            // Release the sibling's own wait so the set can be drained. This is
            // the parent cancel, not a second child cancel: the point is that
            // the sibling was stopped by the parent, never by its peer.
            parent.cancel();

            let mut seen = Vec::new();
            while let Some(joined) = tasks.join_next().await {
                seen.push(joined?);
            }
            seen.sort_unstable();
            Ok::<(Vec<&str>, bool), JoinError>((seen, sibling_survived))
        }
    })?;

    let (seen, sibling_survived) = outcome;
    assert_eq!(
        seen,
        vec!["stopped", "survivor"],
        "both tasks must finish once released"
    );
    assert!(
        sibling_survived,
        "cancelling one child must leave its sibling live, not cancel it too"
    );
    assert!(
        ran_to_completion.load(SeqCst),
        "the sibling of a cancelled child must run to completion"
    );
    // No assertion that `parent` is live: the test cancels it deliberately to
    // release the sibling. The direction being tested is the other one —
    // cancelling a *child* must not reach the parent — and that is covered in
    // the unit tests, where the parent has no sibling to release.
    Ok(())
}

/// A token cancelled before it is awaited still releases its task.
///
/// This is the race a supervisor actually hits: the decision to stop can land
/// before the worker reaches its wait. A primitive that only wakes *registered*
/// waiters would hang here forever.
#[test]
fn a_token_cancelled_before_the_await_still_releases_the_task() -> TestResult {
    let runtime = Runtime::new()?;
    let token = CancellationToken::new();
    // Cancelled with no waiter in existence at all.
    token.cancel();

    let outcome = runtime.block_on(async move {
        let mut tasks = JoinSet::new();
        tasks.spawn(async move {
            token.cancelled_owned().await;
            "released"
        });
        tasks.join_next().await
    });

    let Some(joined) = outcome else {
        return Err("the JoinSet must yield one result".into());
    };
    assert_eq!(
        joined?, "released",
        "a pre-cancelled token must still release its waiter"
    );
    Ok(())
}

/// The crate's own thesis: its verbs are deliberately not `Send`, so a domain
/// may hold thread-local state.
///
/// There is no spawn for a future like this. Every spawn-based path in this
/// crate requires `Send`, which is precisely the property the crate's verbs
/// violate on purpose, so the way to run one is to **drive** it: await it, or
/// hand a set of them to [`lgwks_std::task::join_all_boxed`], which polls them
/// on the calling thread and returns their outputs in input order. This replaces
/// the test that used `LocalSet` for the same purpose; `LocalSet` was itself a
/// spawn surface, so it went with the others.
#[test]
fn a_non_send_future_is_driven_rather_than_spawned() -> TestResult {
    let runtime = Runtime::new()?;
    let observed = runtime.block_on(async {
        // `Rc` and `Cell` are the point: these futures cannot be `Send`, so no
        // spawn in this crate would accept them.
        let shared = Rc::new(Cell::new(0u8));
        let first = {
            let held = Rc::clone(&shared);
            async move {
                held.set(7);
                held.get()
            }
        };
        let second = {
            let held = Rc::clone(&shared);
            async move { held.get() + 1 }
        };
        // Boxed so both inputs share one type: `join_all` is generic over a
        // single `F`, and two `async` blocks are two distinct types. This is
        // exactly the type-erasure boundary `join_all_boxed` exists for.
        let futures: Vec<Pin<Box<dyn Future<Output = u8>>>> =
            vec![Box::pin(first), Box::pin(second)];
        lgwks_std::task::join_all_boxed(futures).await
    });
    assert_eq!(
        observed,
        vec![7, 8],
        "a non-`Send` future must run to completion without a spawn"
    );
    Ok(())
}

/// `rt::io` is what makes the drivers composable: without the traits in scope a
/// consumer can open a socket or a pipe but cannot read from it.
#[test]
fn io_traits_compose_over_a_duplex_stream() -> TestResult {
    let runtime = Runtime::new()?;
    let echoed = runtime.block_on(async {
        let (client, server) = duplex(64);
        let mut writer = BufWriter::new(client);
        let mut reader = BufReader::new(server);
        writer.write_all(b"ping\n").await?;
        writer.flush().await?;
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        Ok::<String, std::io::Error>(line)
    })?;
    assert_eq!(
        echoed, "ping\n",
        "a buffered reader must recover the written line"
    );
    Ok(())
}

/// `Supervisor::shutdown` must report a body that returned on its own as
/// `Cancelled`, on the runtime flavour a consumer actually builds.
///
/// This is a regression, and the defect it pins was invisible until the process
/// tests needed it: the grace before the abort was counted in `yield_now`
/// calls, and `yield_now` reschedules *the yielding task*. On the current-thread
/// executor the crate's unit tests use, that hands the other task the CPU
/// immediately, so eight yields were always enough and the shape looked correct.
/// On a multi-threaded runtime the parked body is on another worker and needs a
/// thread wakeup — an OS event no number of local reschedules waits for — so the
/// abort won the race and a *cancelled* task was reported as `Aborted`. A
/// supervisor whose outcomes cannot be told apart is the one thing its report
/// exists to prevent, so the grace is now a wall-clock bound and this test is
/// what holds it there.
#[test]
fn a_cancelled_body_is_reported_cancelled_on_a_multi_threaded_runtime() -> TestResult {
    let runtime = Runtime::new()?;
    let outcomes = runtime.block_on(async {
        let mut supervisor = Supervisor::default();
        supervisor
            .spawn(|token| async move {
                // Parked, and cooperative: it returns the instant the token is
                // cancelled, but it never completes on its own. Cancelling it and
                // waiting for the runtime to notice is the whole test.
                token
                    .run_until_cancelled(std::future::pending::<()>())
                    .await;
            })
            .await;
        // Let the body reach its wait: a task that has not been polled yet would
        // be cancelled before it ever parked, which tests nothing. The supervisor
        // offers no "has it started" signal, and inventing one for a test would be
        // a production change made for the test's convenience — the body is ready
        // from the moment it is spawned, so a scheduler round-trip is enough.
        for _ in 0..64 {
            lgwks_bot::rt::task::yield_now().await;
        }
        let report = supervisor.shutdown().await;
        report
            .into_outcomes()
            .map_err(|_| std::io::Error::other("non-process task unexpectedly retained cleanup"))
    })?;
    assert!(
        outcomes
            .iter()
            .all(|outcome| matches!(outcome, TaskOutcome::Cancelled { .. })),
        "a body that returned on its cancellation is cancelled, not aborted: {outcomes:?}"
    );
    Ok(())
}
