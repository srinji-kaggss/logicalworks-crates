//! `task` owns synchronous future execution and zero-dependency concurrency
//! for CLI, DAG interpreters, bot ticks, and test runners, enforcing
//! INV-TASK-ZERO-RUNTIME: futures are driven to completion on the current
//! thread using `std::task::Wake` and OS thread parking, with zero background
//! reactors, zero persistent worker threadpools, and zero external
//! dependencies.
//!
//! Three primitives compose:
//!
//! - [`block_on`] drives one future to completion on the calling thread.
//! - [`join_all`] drives many futures concurrently on the calling thread and
//!   returns their outputs in input order.
//! - [`spawn_blocking`] runs one blocking closure on a dedicated OS thread and
//!   returns a future for its result, so a blocking syscall (file read, HTTP)
//!   does not stall the sibling futures driven by [`join_all`] or
//!   [`block_on`].
//!
//! Together these replace `tokio`, `futures`, `pollster`, and `async-trait`
//! for applications that only need to await futures, await a bounded set of
//! them together, and keep blocking work off the driving thread.
//!
//! [`block_on`]: crate::task::block_on
//! [`join_all`]: crate::task::join_all
//! [`spawn_blocking`]: crate::task::spawn_blocking

use std::any::Any;
use std::future::Future;
use std::panic::{AssertUnwindSafe, resume_unwind};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};

// ── block_on ───────────────────────────────────────────────────────────────

/// The waker [`block_on`] hands to the future it drives.
///
/// It is the whole of the runtime: waking marks the flag and unparks the
/// driving thread, so there is no reactor and no worker to schedule onto.
struct ThreadWaker {
    /// The thread [`block_on`] parked, captured at construction; waking it is
    /// the only thing `wake` does to wake the caller.
    thread: Thread,
    /// Set by `wake` and cleared by the park loop with `swap`. The clearance is
    /// what makes a wake that races the pending poll non-lossy: a wake landing
    /// between the poll and the park leaves the flag set, so the loop sees it
    /// and skips the park instead of sleeping through the wakeup.
    notified: AtomicBool,
}

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.notified.store(true, Ordering::Release);
        self.thread.unpark();
    }
}

/// Synchronously polls a `Future` to completion on the current thread.
///
/// If the future is not immediately ready, the current thread is parked until
/// woken by the future's waker. A wake that races the transition into the park
/// is not lost: the notification flag is checked with `swap`, so a wake
/// delivered after the pending poll and before `park` skips the park.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let signal = Arc::new(ThreadWaker {
        thread: thread::current(),
        notified: AtomicBool::new(false),
    });
    let waker = Waker::from(Arc::clone(&signal));
    let mut cx = Context::from_waker(&waker);

    loop {
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(val) => return val,
            Poll::Pending => {
                while !signal.notified.swap(false, Ordering::Acquire) {
                    thread::park();
                }
            }
        }
    }
}

// ── join_all ───────────────────────────────────────────────────────────────

/// Drive `futures` concurrently to completion on the current thread and return
/// their outputs in input order.
///
/// Every poll of the returned future polls each incomplete child with the same
/// waker. When any child wakes the group, every incomplete child is polled
/// again, so one wake costs one scan of the incomplete set (`O(n)` for `n`
/// futures). That is the right trade for the small fan-outs a CLI, DAG
/// interpreter, or bot tick drives; it is not a work-stealing scheduler.
///
/// Completion order never affects the result: index `i` is the output of the
/// `i`-th input whichever finishes first. A future is dropped as soon as it
/// resolves, so the live-future count falls as work completes. An empty input
/// resolves immediately with an empty vector.
///
/// Cancellation: dropping the returned future drops every child that has not
/// resolved, which cancels a child only if that child is cancel-safe on drop.
/// It does not stop work already handed to [`spawn_blocking`]: a dropped
/// `JoinHandle` drops the handle, not the dedicated OS thread, which runs its
/// closure to completion. A caller that must stop in-flight blocking work has
/// to arrange that cooperatively inside the closure.
pub async fn join_all<F: Future>(futures: impl IntoIterator<Item = F>) -> Vec<F::Output> {
    join_all_boxed(futures.into_iter().map(Box::pin)).await
}

/// [`join_all`] over futures the caller has already boxed.
///
/// Same polling, ordering and cancellation semantics as [`join_all`]; the only
/// difference is the allocation model. `join_all` boxes each input once
/// (`n` boxes for `n` futures); this takes those boxes as given and adds none,
/// so a caller that already holds `Pin<Box<_>>` — the bot tick path boxes at
/// its type-erasure boundary — avoids a second box per future. The cost model
/// is `n` heap allocations either way, not a measured throughput claim.
pub async fn join_all_boxed<F: Future + ?Sized>(
    futures: impl IntoIterator<Item = Pin<Box<F>>>,
) -> Vec<F::Output> {
    let mut pending: Vec<Option<Pin<Box<F>>>> = futures.into_iter().map(Some).collect();
    let count = pending.len();
    let mut done: Vec<Option<F::Output>> = (0..count).map(|_| None).collect();

    std::future::poll_fn(move |cx| {
        let mut remaining = 0usize;
        for (slot, output) in pending.iter_mut().zip(done.iter_mut()) {
            let Some(future) = slot.as_mut() else {
                continue;
            };
            match future.as_mut().poll(cx) {
                Poll::Ready(value) => {
                    *output = Some(value);
                    *slot = None;
                }
                // Bounded by `pending.len()`, which is a live `Vec` length and
                // so cannot exceed `isize::MAX`: the saturating form states the
                // bound rather than relying on it, and is the identity
                // everywhere the count is reachable.
                Poll::Pending => remaining = remaining.saturating_add(1),
            }
        }
        if remaining == 0 {
            // `remaining` counts every slot still holding a future, and a slot
            // is cleared in the same arm that fills its output, so zero here
            // means every output slot was filled above. `flatten` therefore
            // drops nothing; it is the panic-free spelling of the invariant,
            // and it keeps the result the same length as the input.
            Poll::Ready(done.drain(..).flatten().collect())
        } else {
            Poll::Pending
        }
    })
    .await
}

// ── spawn_blocking ─────────────────────────────────────────────────────────

/// Take `mutex`, treating poisoning as non-fatal.
///
/// Poisoning means some thread panicked while holding the lock. Every critical
/// section behind this mutex moves a whole [`Job`] in or out and writes no
/// partial state, so a poisoned lock still guards a consistent value and the
/// panic itself is already being resumed on the awaiter. Recovering the guard
/// is therefore correct here, and it is why this is not an `unwrap`: a
/// `JoinHandle` must not turn a worker's failure into a deadlock for the task
/// awaiting it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One blocking job's completion state.
enum Job<T> {
    /// The dedicated thread is still running the closure.
    Running,
    /// The closure returned.
    Done(T),
    /// The closure panicked, or its thread could not be spawned; the payload is
    /// resumed on the task that awaits the handle.
    Panicked(Box<dyn Any + Send>),
    /// A completed poll already took the result.
    Taken,
}

/// Completion state shared between a [`spawn_blocking`] worker and its
/// [`JoinHandle`], behind one mutex.
///
/// The worker writes `job`; the awaiting task reads it and writes `waker`. They
/// are one struct under one lock so that a completion cannot land between the
/// poll's read of `job` and its registration of `waker` — the interleaving that
/// would lose a wakeup and hang the awaiter.
struct Shared<T> {
    /// The job's lifecycle state. `Running` until the worker thread records an
    /// outcome, `Taken` once the handle has moved the outcome out.
    job: Job<T>,
    /// The waker of the task awaiting this handle, replaced only when it would
    /// not already wake that task (`will_wake`). `None` until the first pending
    /// poll, and cleared by the worker after it wakes the task, so a later poll
    /// cannot wake a task that has already been resumed.
    waker: Option<Waker>,
}

/// A future for the result of one [`spawn_blocking`] job.
///
/// Awaiting parks the current task until the dedicated thread finishes; the
/// thread wakes the task's waker exactly once. If the closure panicked, or the
/// thread could not be spawned, awaiting resumes that failure on the current
/// thread rather than hanging. Polling after completion panics with a named
/// message, matching the `Future` contract.
///
/// The result is handed out exactly once: `T` is not `Clone`, so a second poll
/// has no value to return, and the alternatives to failing loudly are a silent
/// deadlock (`Pending` with nothing left to wake it) or a fabricated value.
/// Both are worse than the panic the `Future` contract already permits, so the
/// completed-but-polled state is resumed with `resume_unwind` — the same
/// mechanism, and the same "fail on the awaiter, never abort the process" rule,
/// that already carries a panicking closure's payload back to the caller.
pub struct JoinHandle<T> {
    /// The state this handle and its worker thread share. The `Arc` is held by
    /// both sides while the job runs and by neither once both are gone, so the
    /// job's result and its worker are released when the handle is dropped.
    shared: Arc<Mutex<Shared<T>>>,
}

impl<T> Future for JoinHandle<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        let mut state = lock(&self.shared);
        match std::mem::replace(&mut state.job, Job::Taken) {
            Job::Done(value) => Poll::Ready(value),
            Job::Panicked(payload) => {
                drop(state);
                resume_unwind(payload)
            }
            Job::Taken => resume_unwind(Box::new(
                "spawn_blocking JoinHandle polled after completion",
            )),
            Job::Running => {
                state.job = Job::Running;
                // `as_ref` rather than `&state.waker`: it yields an
                // `Option<&Waker>` the arm patterns match exactly, and it ends
                // its borrow of `state` before the assignment below.
                let replace = match state.waker.as_ref() {
                    Some(existing) => !existing.will_wake(cx.waker()),
                    None => true,
                };
                if replace {
                    state.waker = Some(cx.waker().clone());
                }
                Poll::Pending
            }
        }
    }
}

/// Run `job` on a dedicated OS thread and return a future for its result.
///
/// The thread is spawned immediately; the returned future is what carries the
/// result back to the driving task. Awaiting it inside [`join_all`] therefore
/// overlaps the blocking job with its siblings instead of serializing them.
///
/// Bound: one OS thread per call while the closure runs, reclaimed on
/// completion. There is no pooled or background thread between calls; callers
/// that need a ceiling on simultaneous threads (for example
/// `lgwks_bot::Bot::tick`) bound their own fan-out.
///
/// Failure: a panicking closure, or an OS refusal to spawn the thread, is
/// resumed as a panic on the task that awaits the handle. Success, panic, and
/// spawn failure all wake the task exactly once.
pub fn spawn_blocking<F, T>(job: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    // Nothing is written to stderr here, opt-in or otherwise. A trace line per
    // job would flood callers that treat stderr as a machine-readable channel,
    // and a library that prints what its caller did cannot be silenced by that
    // caller. A caller that wants to reconstruct the spawn already holds the
    // call site: it made the call, so it can trace it before making this one.
    let shared = Arc::new(Mutex::new(Shared {
        job: Job::Running,
        waker: None,
    }));
    let worker = Arc::clone(&shared);

    let spawned = thread::Builder::new()
        .name("lgwks-blocking".into())
        .spawn(move || {
            let outcome = std::panic::catch_unwind(AssertUnwindSafe(job));
            let mut state = lock(&worker);
            state.job = match outcome {
                Ok(value) => Job::Done(value),
                Err(payload) => Job::Panicked(payload),
            };
            if let Some(waker) = state.waker.take() {
                drop(state);
                waker.wake();
            }
        });

    if let Err(error) = spawned {
        let mut state = lock(&shared);
        state.job = Job::Panicked(Box::new(error));
    }

    JoinHandle { shared }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
// The workspace ban list (`clippy.toml`) forbids `std::thread::spawn` and
// `std::thread::sleep`, because reaching for a raw OS thread instead of the
// estate's surface is the anti-pattern. This module is the exception that
// proves the rule: it tests `lgwks_std::task` itself, which is a *thread
// parking* executor. Waking it requires a real second thread, and letting the
// driver park before that wake requires a real sleep. Both calls are the
// subject under test, not a reach for one.
#[expect(
    clippy::disallowed_methods,
    reason = "tests of a thread-parking executor must spawn a waker thread and sleep to let the driver park"
)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::time::Duration;

    #[test]
    fn immediate_future_returns_value() {
        let result = block_on(async { 42 });
        assert_eq!(result, 42);
    }

    #[test]
    fn yields_and_resumes() {
        async fn step() -> String {
            let first_part = async { "hello" }.await;
            let second_part = async { "world" }.await;
            format!("{first_part} {second_part}")
        }

        assert_eq!(block_on(step()), "hello world");
    }

    /// Resolves once another thread hands it a value through the shared slot.
    ///
    /// A value that arrives before the first poll is found on that poll; a
    /// value that arrives after it is delivered by the waker this future parks.
    struct DeferredValue {
        value: Arc<Mutex<Option<i32>>>,
        waker_slot: Arc<Mutex<Option<Waker>>>,
    }

    impl Future for DeferredValue {
        type Output = i32;
        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            if let Some(value) = lock(&self.value).take() {
                return Poll::Ready(value);
            }
            *lock(&self.waker_slot) = Some(cx.waker().clone());
            Poll::Pending
        }
    }

    #[test]
    fn threaded_waker_unparks() {
        let value: Arc<Mutex<Option<i32>>> = Arc::new(Mutex::new(None));
        let waker_slot: Arc<Mutex<Option<Waker>>> = Arc::new(Mutex::new(None));
        let sender_value = Arc::clone(&value);
        let sender_slot = Arc::clone(&waker_slot);

        // Scoped, so the handing-off thread is joined before the test returns
        // instead of outliving it.
        thread::scope(|scope| {
            scope.spawn(move || {
                // The pause is load-bearing: it lets `block_on` park first, so
                // the wake below is what resumes the future rather than a value
                // found on the opening poll.
                thread::sleep(Duration::from_millis(5));
                *lock(&sender_value) = Some(100);
                if let Some(waker) = lock(&sender_slot).take() {
                    waker.wake();
                }
            });

            let result = block_on(DeferredValue { value, waker_slot });
            assert_eq!(result, 100);
        });
    }

    #[test]
    fn join_all_empty_resolves_immediately() {
        let output: Vec<u8> = block_on(join_all(std::iter::empty::<std::future::Ready<u8>>()));
        assert!(output.is_empty());
    }

    #[test]
    fn join_all_preserves_input_order_across_completion_order() {
        let output = block_on(join_all(vec![
            spawn_blocking(|| {
                thread::sleep(Duration::from_millis(30));
                1u32
            }),
            spawn_blocking(|| 2u32),
            spawn_blocking(|| {
                thread::sleep(Duration::from_millis(10));
                3u32
            }),
        ]));
        assert_eq!(output, vec![1, 2, 3]);
    }

    #[test]
    fn join_all_runs_children_concurrently() {
        // Both jobs block on a shared barrier: the pair can only complete if
        // the two dedicated threads are alive at the same time.
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                spawn_blocking(move || {
                    barrier.wait();
                    7u32
                })
            })
            .collect();
        let output = block_on(join_all(handles));
        assert_eq!(output, vec![7, 7]);
    }

    struct CountPolls {
        polls: Arc<AtomicUsize>,
    }

    impl Future for CountPolls {
        type Output = usize;
        fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<usize> {
            // `fetch_add` yields the count before this poll, so the 1-based
            // poll ordinal is one past it. Saturating rather than `+`: the
            // counter is bounded by the test's poll count, far below
            // `usize::MAX`.
            let n = self
                .polls
                .fetch_add(1, AtomicOrdering::SeqCst)
                .saturating_add(1);
            Poll::Ready(n)
        }
    }

    /// Pends on first poll, wakes the group from another thread, then resolves.
    struct PendingThenReady {
        polls: Arc<AtomicUsize>,
        waker_slot: Arc<Mutex<Option<Waker>>>,
    }

    impl Future for PendingThenReady {
        type Output = usize;
        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<usize> {
            // See `CountPolls::poll`: the ordinal is `fetch_add`'s prior value
            // plus one, saturating against a bound the test cannot reach.
            let n = self
                .polls
                .fetch_add(1, AtomicOrdering::SeqCst)
                .saturating_add(1);
            if n > 1 {
                return Poll::Ready(n);
            }
            *lock(&self.waker_slot) = Some(cx.waker().clone());
            let slot = Arc::clone(&self.waker_slot);
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(5));
                if let Some(waker) = lock(&slot).take() {
                    waker.wake();
                }
            });
            Poll::Pending
        }
    }

    #[test]
    fn join_all_never_repolls_a_completed_future() {
        let fast_polls = Arc::new(AtomicUsize::new(0));
        let slow_polls = Arc::new(AtomicUsize::new(0));
        let fast: Pin<Box<dyn Future<Output = usize>>> = Box::pin(CountPolls {
            polls: Arc::clone(&fast_polls),
        });
        let slow: Pin<Box<dyn Future<Output = usize>>> = Box::pin(PendingThenReady {
            polls: Arc::clone(&slow_polls),
            waker_slot: Arc::new(Mutex::new(None)),
        });
        let output = block_on(join_all(vec![fast, slow]));
        assert_eq!(output, vec![1, 2]);
        // The fast child resolved on its first poll. When the slow child wakes
        // the group, only the incomplete set may be polled again, so the fast
        // child's count must stay at one.
        assert_eq!(fast_polls.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(slow_polls.load(AtomicOrdering::SeqCst), 2);
    }

    #[test]
    fn join_all_boxed_matches_join_all_and_accepts_boxes() {
        let plain = block_on(join_all(vec![std::future::ready(1), std::future::ready(2)]));
        let boxed: Vec<Pin<Box<dyn Future<Output = i32>>>> = vec![
            Box::pin(std::future::ready(1)),
            Box::pin(std::future::ready(2)),
        ];
        assert_eq!(plain, vec![1, 2]);
        assert_eq!(block_on(join_all_boxed(boxed)), vec![1, 2]);

        let empty: Vec<Pin<Box<dyn Future<Output = i32>>>> = Vec::new();
        assert_eq!(block_on(join_all_boxed(empty)), Vec::<i32>::new());
    }

    #[test]
    fn spawn_blocking_returns_value() {
        assert_eq!(block_on(spawn_blocking(|| 99u32)), 99);
    }

    #[test]
    fn spawn_blocking_result_ready_before_first_poll() {
        let handle = spawn_blocking(|| 1234u32);
        thread::sleep(Duration::from_millis(30));
        assert_eq!(block_on(handle), 1234);
    }

    #[test]
    #[should_panic(expected = "worker exploded")]
    fn spawn_blocking_panic_resumes_on_joiner() {
        // The job's panic is the subject under test: it must resume on the
        // joiner rather than vanish with the worker thread. The message rides
        // on a deliberately-false comparison because `clippy::panic` is
        // forbidden workspace-wide with no test carve-out.
        let _: u32 = block_on(spawn_blocking(|| -> u32 {
            assert_eq!(1, 2, "worker exploded");
            0u32
        }));
    }
}
