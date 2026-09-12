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

use std::any::Any;
use std::future::Future;
use std::panic::{AssertUnwindSafe, resume_unwind};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};

// ── block_on ───────────────────────────────────────────────────────────────

struct ThreadWaker {
    thread: Thread,
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
    let waker = Waker::from(signal.clone());
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
    let mut pending: Vec<Option<Pin<Box<F>>>> = futures
        .into_iter()
        .map(|future| Some(Box::pin(future)))
        .collect();
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
                Poll::Pending => remaining += 1,
            }
        }
        if remaining == 0 {
            Poll::Ready(
                done.drain(..)
                    .map(|output| output.expect("join_all resolves every future exactly once"))
                    .collect(),
            )
        } else {
            Poll::Pending
        }
    })
    .await
}

// ── spawn_blocking ─────────────────────────────────────────────────────────

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

struct Shared<T> {
    job: Job<T>,
    waker: Option<Waker>,
}

/// A future for the result of one [`spawn_blocking`] job.
///
/// Awaiting parks the current task until the dedicated thread finishes; the
/// thread wakes the task's waker exactly once. If the closure panicked, or the
/// thread could not be spawned, awaiting resumes that failure on the current
/// thread rather than hanging. Polling after completion panics with a named
/// message, matching the `Future` contract.
pub struct JoinHandle<T> {
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
            Job::Taken => panic!("spawn_blocking JoinHandle polled after completion"),
            Job::Running => {
                state.job = Job::Running;
                let replace = match &state.waker {
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
    // Opt-in trace: spawning a thread is an effect worth being able to
    // reconstruct after an incident, but one stderr line per job would flood
    // callers that treat stderr as a machine-readable channel. Set
    // LGWKS_TASK_TRACE to turn it on.
    if std::env::var_os("LGWKS_TASK_TRACE").is_some() {
        eprintln!("lgwks_std::task: spawn_blocking job started");
    }
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

    struct ChannelFuture {
        rx: std::sync::mpsc::Receiver<i32>,
        waker_slot: Arc<Mutex<Option<Waker>>>,
    }

    impl Future for ChannelFuture {
        type Output = i32;
        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            match self.rx.try_recv() {
                Ok(val) => Poll::Ready(val),
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    *lock(&self.waker_slot) = Some(cx.waker().clone());
                    Poll::Pending
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => panic!("disconnected"),
            }
        }
    }

    #[test]
    fn threaded_waker_unparks() {
        let (tx, rx) = std::sync::mpsc::channel();
        let handle: Arc<Mutex<Option<Waker>>> = Arc::new(Mutex::new(None));
        let handle_clone = handle.clone();

        thread::spawn(move || {
            thread::sleep(Duration::from_millis(5));
            tx.send(100).unwrap();
            let mut slot = lock(&handle_clone);
            if let Some(waker) = slot.take() {
                waker.wake();
            }
        });

        let res = block_on(ChannelFuture {
            rx,
            waker_slot: handle,
        });

        assert_eq!(res, 100);
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
            let n = self.polls.fetch_add(1, AtomicOrdering::SeqCst) + 1;
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
            let n = self.polls.fetch_add(1, AtomicOrdering::SeqCst) + 1;
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
        let _: u32 = block_on(spawn_blocking(|| -> u32 { panic!("worker exploded") }));
    }
}
