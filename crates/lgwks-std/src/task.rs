//! `task` owns synchronous future execution and zero-dependency concurrency
//! for CLI tools, DAG interpreters, polling loops, and test runners, enforcing
//! INV-TASK-ZERO-RUNTIME: futures are driven to completion on the current
//! thread using `std::task::Wake` and OS thread parking, with zero background
//! reactors and zero external dependencies. The one pool is for blocking work:
//! it is bounded (512 threads), created on first use, and its threads exit
//! after ten idle seconds, so a process with no blocking work holds no thread.
//!
//! Three primitives compose:
//!
//! - [`block_on`] drives one future to completion on the calling thread.
//! - [`join_all`] drives many futures concurrently on the calling thread and
//!   returns their outputs in input order.
//! - [`spawn_blocking`] runs one blocking closure on the bounded blocking pool
//!   and returns a future for its result, so a blocking syscall (file read,
//!   HTTP) does not stall the sibling futures driven by [`join_all`] or
//!   [`block_on`]. [`try_spawn_blocking`] is the same with a bounded queue
//!   and a typed [`SpawnError`] instead of an unbounded wait.
//!
//! Together these replace `tokio`, `futures`, `pollster`, and `async-trait`
//! for applications that only need to await futures, await a bounded set of
//! them together, and keep blocking work off the driving thread.
//!
//! [`block_on`]: crate::task::block_on
//! [`join_all`]: crate::task::join_all
//! [`spawn_blocking`]: crate::task::spawn_blocking

use std::any::Any;
use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::panic::{AssertUnwindSafe, resume_unwind};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};
use std::time::Duration;

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
/// Each child gets its own waker. A child's wake queues that child alone, and
/// the next poll of the group polls only the children that woke, so one wake
/// costs one child poll however many children are still pending. The first
/// poll polls every child once. It is still one thread: concurrency here is
/// interleaving, not parallelism, and it is not a work-stealing scheduler.
///
/// Completion order never affects the result: index `i` is the output of the
/// `i`-th input whichever finishes first. A future is dropped as soon as it
/// resolves, so the live-future count falls as work completes. An empty input
/// resolves immediately with an empty vector.
///
/// Cancellation: dropping the returned future drops every child that has not
/// resolved, which cancels a child only if that child is cancel-safe on drop.
/// It does not stop work already handed to [`spawn_blocking`]: a dropped
/// `JoinHandle` drops the handle, not the pool thread, which runs its closure
/// to completion. A caller that must stop in-flight blocking work has
/// to arrange that cooperatively inside the closure.
pub async fn join_all<F: Future>(futures: impl IntoIterator<Item = F>) -> Vec<F::Output> {
    join_all_boxed(futures.into_iter().map(Box::pin)).await
}

/// [`join_all`] over futures the caller has already boxed.
///
/// Same polling, ordering and cancellation semantics as [`join_all`]; the only
/// difference is the allocation model. `join_all` boxes each input once
/// (`n` boxes for `n` futures); this takes those boxes as given and adds none,
/// so a caller that already holds `Pin<Box<_>>` (for instance at a type-erasure
/// boundary) avoids a second box per future. The cost model
/// is `n` heap allocations either way, not a measured throughput claim.
pub async fn join_all_boxed<F: Future + ?Sized>(
    futures: impl IntoIterator<Item = Pin<Box<F>>>,
) -> Vec<F::Output> {
    let mut pending: Vec<Option<Pin<Box<F>>>> = futures.into_iter().map(Some).collect();
    let count = pending.len();
    let mut done: Vec<Option<F::Output>> = (0..count).map(|_| None).collect();
    let ready = Arc::new(ReadySet {
        inner: Mutex::new(ReadyInner {
            // Every child starts queued: the first poll polls each one once.
            order: (0..count).collect(),
            queued: vec![true; count],
            group: None,
        }),
    });
    let wakers: Vec<Waker> = (0..count)
        .map(|index| {
            Waker::from(Arc::new(ChildWaker {
                index,
                ready: Arc::clone(&ready),
            }))
        })
        .collect();
    let mut remaining = count;

    std::future::poll_fn(move |cx| {
        let woken = {
            let mut inner = lock(&ready.inner);
            let replace = inner
                .group
                .as_ref()
                .is_none_or(|existing| !existing.will_wake(cx.waker()));
            if replace {
                inner.group = Some(cx.waker().clone());
            }
            std::mem::take(&mut inner.order)
        };
        for index in woken {
            // Cleared before the poll, so a child that wakes itself while it
            // is being polled is queued again rather than lost.
            if let Some(flag) = lock(&ready.inner).queued.get_mut(index) {
                *flag = false;
            }
            let (Some(slot), Some(output), Some(waker)) = (
                pending.get_mut(index),
                done.get_mut(index),
                wakers.get(index),
            ) else {
                continue;
            };
            let Some(future) = slot.as_mut() else {
                continue;
            };
            if let Poll::Ready(value) = future.as_mut().poll(&mut Context::from_waker(waker)) {
                *output = Some(value);
                *slot = None;
                // Bounded by `count`: a slot is emptied once, here.
                remaining = remaining.saturating_sub(1);
            }
        }
        if remaining == 0 {
            // `remaining` falls by one exactly when a slot's output is filled,
            // so zero means every output slot was filled. `flatten` therefore
            // drops nothing; it is the panic-free spelling of the invariant,
            // and it keeps the result the same length as the input.
            Poll::Ready(done.drain(..).flatten().collect())
        } else {
            Poll::Pending
        }
    })
    .await
}

/// The children of one [`join_all_boxed`] that have woken since its last poll.
struct ReadySet {
    /// Guarded together, so a wake cannot land between the group reading the
    /// queue and registering its own waker.
    inner: Mutex<ReadyInner>,
}

/// The state behind [`ReadySet`]'s lock.
struct ReadyInner {
    /// Woken children, in wake order, each at most once.
    order: Vec<usize>,
    /// Whether each child is already in `order`, so a child that wakes twice
    /// before the next poll is polled once.
    queued: Vec<bool>,
    /// The waker of the task driving the group.
    group: Option<Waker>,
}

/// One child's waker: queue the child, then wake the group.
struct ChildWaker {
    /// The child's position in the input.
    index: usize,
    /// The group's queue.
    ready: Arc<ReadySet>,
}

impl Wake for ChildWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let group = {
            let mut inner = lock(&self.ready.inner);
            let newly = inner
                .queued
                .get_mut(self.index)
                .is_some_and(|flag| !std::mem::replace(flag, true));
            if newly {
                inner.order.push(self.index);
            }
            inner.group.clone()
        };
        // Outside the lock: the group's waker may run its task inline.
        if let Some(group) = group {
            group.wake();
        }
    }
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
/// poll's read of `job` and its registration of `waker`, the interleaving that
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
/// completed-but-polled state is resumed with `resume_unwind`, the same
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

/// Run `job` on the blocking pool and return a future for its result.
///
/// The job is handed to a pool thread at once; the returned future is what
/// carries the result back to the driving task. Awaiting it inside
/// [`join_all`] therefore overlaps the blocking job with its siblings instead
/// of serializing them.
///
/// Bound: at most 512 threads run jobs at once,
/// process-wide. A job submitted while all of them are busy waits in a queue
/// and runs when one frees up, in submission order. A thread that has had no
/// work for 10 seconds exits, so an idle process holds no pool
/// thread. Jobs that wait on *each other* must therefore number fewer than
/// the ceiling, or the waiters hold every thread the awaited job needs.
///
/// This entry point keeps its 1.0 contract: its queue is not bounded and it
/// never refuses. [`try_spawn_blocking`] is the bounded form, and the one to
/// use where callers are not already bounding their own fan-out.
///
/// Failure: a panicking closure, or an OS refusal to start the first pool
/// thread, is resumed as a panic on the task that awaits the handle. Success,
/// panic, and refusal all wake the task exactly once.
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
    let (handle, work) = prepare(job);
    if let Err(error) = pool().submit(work, None) {
        lock(&handle.shared).job = Job::Panicked(Box::new(error));
    }
    handle
}

/// Run `job` on the blocking pool, or refuse it with a typed reason.
///
/// The same pool and the same thread ceiling as [`spawn_blocking`], with a
/// bounded queue: when 16,384 jobs are already waiting,
/// the job is refused as [`SpawnError::AtCapacity`] and never runs. An OS
/// refusal to start a thread, when no pool thread is alive to run the job
/// later, is [`SpawnError::Os`]. A refused job is dropped without running.
///
/// # Errors
///
/// [`SpawnError::AtCapacity`] when the queue is full, and [`SpawnError::Os`]
/// when no thread could be started to run the job.
pub fn try_spawn_blocking<F, T>(job: F) -> Result<JoinHandle<T>, SpawnError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let (handle, work) = prepare(job);
    pool().submit(work, Some(MAX_QUEUED_BLOCKING_JOBS))?;
    Ok(handle)
}

/// The handle a caller awaits and the type-erased work that fills it.
fn prepare<F, T>(job: F) -> (JoinHandle<T>, Work)
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let shared = Arc::new(Mutex::new(Shared {
        job: Job::Running,
        waker: None,
    }));
    let worker = Arc::clone(&shared);
    let work: Work = Box::new(move || {
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
    (JoinHandle { shared }, work)
}

// ── the blocking pool ──────────────────────────────────────────────────────

/// Most pool threads running jobs at once, process-wide.
///
/// Tokio's blocking pool uses the same default ceiling. It bounds what a burst
/// of blocking work can take from the OS: without it, ten thousand concurrent
/// calls were ten thousand threads, each reserving its own stack.
const MAX_BLOCKING_THREADS: usize = 512;

/// Most jobs [`try_spawn_blocking`] lets wait for a thread before it refuses.
const MAX_QUEUED_BLOCKING_JOBS: usize = 16_384;

/// How long a pool thread with no work waits for some before it exits.
const BLOCKING_KEEP_ALIVE: Duration = Duration::from_secs(10);

/// Why [`try_spawn_blocking`] did not accept a job. The job did not run.
#[derive(Debug)]
#[non_exhaustive]
pub enum SpawnError {
    /// Every pool thread is busy and the wait queue is full.
    AtCapacity {
        /// The thread ceiling (512).
        threads: usize,
        /// The queue bound that was reached (16,384).
        queued: usize,
    },
    /// The OS refused to start a thread, and no pool thread was alive to run
    /// the job later.
    Os(std::io::Error),
}

impl fmt::Display for SpawnError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::AtCapacity { threads, queued } => write!(
                formatter,
                "the blocking pool is at capacity: {threads} threads busy and {queued} jobs waiting"
            ),
            Self::Os(ref error) => write!(formatter, "could not start a blocking thread: {error}"),
        }
    }
}

impl std::error::Error for SpawnError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Os(ref error) => Some(error),
            Self::AtCapacity { .. } => None,
        }
    }
}

/// One job, with its result already wired to its handle.
type Work = Box<dyn FnOnce() + Send>;

/// The process-wide pool.
struct Pool {
    /// Queue and thread accounting, under one lock.
    state: Mutex<PoolState>,
    /// Signalled once per job handed to an idle thread.
    work_ready: Condvar,
}

/// What the pool's lock guards.
struct PoolState {
    /// Jobs waiting for a thread, oldest first.
    queue: VecDeque<Work>,
    /// Threads alive, running or idle.
    live: usize,
    /// Threads waiting for work that no submitter has claimed yet.
    idle: usize,
    /// Wakeups claimed by submitters and not yet taken by a thread. Counted
    /// rather than inferred from the condvar, which may wake spuriously.
    wakeups: usize,
}

/// The pool, created on first use.
fn pool() -> &'static Pool {
    static POOL: OnceLock<Pool> = OnceLock::new();
    POOL.get_or_init(|| Pool {
        state: Mutex::new(PoolState {
            queue: VecDeque::new(),
            live: 0,
            idle: 0,
            wakeups: 0,
        }),
        work_ready: Condvar::new(),
    })
}

impl Pool {
    /// Queue `work` and make sure a thread will run it.
    ///
    /// An idle thread is woken if there is one; otherwise a thread is started
    /// while the pool is under its ceiling; otherwise the job waits for the
    /// next thread to finish. `queue_limit` bounds the wait queue.
    fn submit(&'static self, work: Work, queue_limit: Option<usize>) -> Result<(), SpawnError> {
        let mut state = lock(&self.state);
        if let Some(limit) = queue_limit
            && state.queue.len() >= limit
        {
            let refusal = Err(SpawnError::AtCapacity {
                threads: MAX_BLOCKING_THREADS,
                queued: limit,
            });
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "submit: the blocking pool refused a job");
            return refusal;
        }
        state.queue.push_back(work);
        if state.idle > 0 {
            state.idle = state.idle.saturating_sub(1);
            state.wakeups = state.wakeups.saturating_add(1);
            self.work_ready.notify_one();
            return Ok(());
        }
        if state.live >= MAX_BLOCKING_THREADS {
            return Ok(());
        }
        let started = thread::Builder::new()
            .name("lgwks-blocking".into())
            .spawn(move || self.run());
        match started {
            Ok(_detached) => {
                state.live = state.live.saturating_add(1);
                Ok(())
            }
            // A live thread will reach the job when it finishes its own.
            Err(_) if state.live > 0 => Ok(()),
            Err(error) => {
                drop(state.queue.pop_back());
                let refusal = Err(SpawnError::Os(error));
                #[cfg(feature = "trace")]
                crate::trace::debug!(error = ?refusal.as_ref().err(), "submit: no blocking thread could be started");
                refusal
            }
        }
    }

    /// One pool thread: run queued jobs, wait for more, exit when idle for
    /// [`BLOCKING_KEEP_ALIVE`].
    fn run(&self) {
        let mut state = lock(&self.state);
        loop {
            if let Some(work) = state.queue.pop_front() {
                drop(state);
                // `prepare` catches the job's panic, so a failing job cannot
                // take the thread down with it.
                work();
                state = lock(&self.state);
                continue;
            }
            state.idle = state.idle.saturating_add(1);
            loop {
                let (guard, waited) = self
                    .work_ready
                    .wait_timeout(state, BLOCKING_KEEP_ALIVE)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state = guard;
                if state.wakeups > 0 {
                    // The submitter already moved this thread out of `idle`.
                    state.wakeups = state.wakeups.saturating_sub(1);
                    break;
                }
                if waited.timed_out() {
                    state.idle = state.idle.saturating_sub(1);
                    state.live = state.live.saturating_sub(1);
                    return;
                }
            }
        }
    }
}

impl<T> core::fmt::Debug for JoinHandle<T> {
    /// Reports whether the job is still running, never the value.
    ///
    /// `T` carries no `Debug` bound here on purpose: the handle is awaited for
    /// its value, and a caller that only wants to log which job is outstanding
    /// should not have to make the payload printable to do it. The job state is
    /// the one fact the handle holds, so it is the whole of what is printed.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let state = lock(&self.shared);
        let state = match state.job {
            Job::Running => "running",
            Job::Done(_) => "done",
            Job::Panicked(_) => "panicked",
            Job::Taken => "taken",
        };
        f.debug_struct("JoinHandle").field("state", &state).finish()
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
// The workspace ban list (`clippy.toml`) forbids `std::thread::spawn` and
// `std::thread::sleep`, because reaching for a raw OS thread instead of the
// primitives this crate provides is the anti-pattern. This module is the
// exception that proves the rule: it tests `lgwks_std::task` itself, which is a
// *thread parking* executor. Waking it requires a real second thread, and letting the
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

    /// Wakers parked on one latch, released together when it opens.
    #[derive(Default)]
    struct Latch {
        open: AtomicBool,
        parked: Mutex<Vec<Waker>>,
    }

    /// Pends until the latch opens, counting every poll.
    struct Waiter {
        latch: Arc<Latch>,
        polls: Arc<AtomicUsize>,
    }

    impl Future for Waiter {
        type Output = ();
        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            self.polls.fetch_add(1, AtomicOrdering::SeqCst);
            if self.latch.open.load(AtomicOrdering::SeqCst) {
                return Poll::Ready(());
            }
            lock(&self.latch.parked).push(cx.waker().clone());
            Poll::Pending
        }
    }

    /// Wakes itself `wakes` times, then opens the latch and resolves.
    struct Busy {
        wakes: usize,
        latch: Arc<Latch>,
        polls: Arc<AtomicUsize>,
    }

    impl Future for Busy {
        type Output = ();
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            self.polls.fetch_add(1, AtomicOrdering::SeqCst);
            if self.wakes == 0 {
                self.latch.open.store(true, AtomicOrdering::SeqCst);
                for waker in lock(&self.latch.parked).drain(..) {
                    waker.wake();
                }
                return Poll::Ready(());
            }
            self.wakes = self.wakes.saturating_sub(1);
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }

    #[test]
    fn a_wake_polls_only_the_child_that_woke() {
        // One busy child wakes itself `wakes` times while `waiters` children
        // sit silent, then releases them. Polling only the woken child costs
        // `wakes + 1` busy polls plus two per waiter. A group that re-polls
        // every pending child on every wake costs about `waiters · wakes`,
        // which is what this count refuses.
        for (waiters, wakes) in [(1usize, 0usize), (10, 3), (1_000, 50), (10_000, 20)] {
            let latch = Arc::new(Latch::default());
            let polls = Arc::new(AtomicUsize::new(0));
            // The busy child goes last, so every waiter has parked before it
            // first runs, whatever `wakes` is.
            let mut children: Vec<Pin<Box<dyn Future<Output = ()>>>> = (0..waiters)
                .map(|_| -> Pin<Box<dyn Future<Output = ()>>> {
                    Box::pin(Waiter {
                        latch: Arc::clone(&latch),
                        polls: Arc::clone(&polls),
                    })
                })
                .collect();
            children.push(Box::pin(Busy {
                wakes,
                latch: Arc::clone(&latch),
                polls: Arc::clone(&polls),
            }));
            let output = block_on(join_all_boxed(children));
            assert_eq!(output.len(), waiters.saturating_add(1));
            assert_eq!(
                polls.load(AtomicOrdering::SeqCst),
                wakes
                    .saturating_add(1)
                    .saturating_add(waiters.saturating_mul(2)),
                "{waiters} waiters beside a child waking {wakes} times"
            );
        }
    }

    /// A gate every job waits on, and the peak number of jobs inside it.
    #[derive(Default)]
    struct Gate {
        open: Mutex<bool>,
        opened: std::sync::Condvar,
        inside: AtomicUsize,
        peak: AtomicUsize,
    }

    impl Gate {
        fn pass(&self) {
            let now = self
                .inside
                .fetch_add(1, AtomicOrdering::SeqCst)
                .saturating_add(1);
            self.peak.fetch_max(now, AtomicOrdering::SeqCst);
            let mut open = lock(&self.open);
            while !*open {
                open = self
                    .opened
                    .wait(open)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            drop(open);
            self.inside.fetch_sub(1, AtomicOrdering::SeqCst);
        }

        fn release(&self) {
            *lock(&self.open) = true;
            self.opened.notify_all();
        }
    }

    #[test]
    fn no_more_than_the_ceiling_run_at_once_and_every_job_completes() -> Result<(), SpawnError> {
        for jobs in [100usize, 1_000, 10_000] {
            let gate = Arc::new(Gate::default());
            let mut handles = Vec::with_capacity(jobs);
            for index in 0..jobs {
                let gate = Arc::clone(&gate);
                handles.push(try_spawn_blocking(move || {
                    gate.pass();
                    index
                })?);
            }
            // Let the pool fill every thread it may before the gate opens.
            thread::sleep(Duration::from_millis(200));
            gate.release();
            let output = block_on(join_all(handles));
            assert_eq!(output, (0..jobs).collect::<Vec<_>>(), "{jobs} jobs");
            let peak = gate.peak.load(AtomicOrdering::SeqCst);
            assert!(
                peak <= MAX_BLOCKING_THREADS,
                "{jobs} jobs: {peak} ran at once, over the ceiling"
            );
            assert!(
                peak >= jobs.min(MAX_BLOCKING_THREADS).min(64),
                "{jobs} jobs: only {peak} ran at once; the pool is not running in parallel"
            );
        }
        Ok(())
    }

    #[test]
    fn past_the_queue_bound_a_job_is_refused_with_a_typed_reason() {
        const OFFERED: usize = 100_000;
        let gate = Arc::new(Gate::default());
        let mut accepted = Vec::new();
        let mut refused = 0usize;
        for _ in 0..OFFERED {
            let gate = Arc::clone(&gate);
            match try_spawn_blocking(move || gate.pass()) {
                Ok(handle) => accepted.push(handle),
                Err(error) => {
                    assert!(
                        matches!(
                            error,
                            SpawnError::AtCapacity {
                                threads: MAX_BLOCKING_THREADS,
                                queued: MAX_QUEUED_BLOCKING_JOBS,
                            }
                        ),
                        "refused for the wrong reason: {error}"
                    );
                    refused = refused.saturating_add(1);
                }
            }
        }
        gate.release();
        let ceiling = MAX_QUEUED_BLOCKING_JOBS.saturating_add(MAX_BLOCKING_THREADS);
        assert!(
            (MAX_QUEUED_BLOCKING_JOBS..=ceiling).contains(&accepted.len()),
            "{} accepted, outside [{MAX_QUEUED_BLOCKING_JOBS}, {ceiling}]",
            accepted.len()
        );
        assert_eq!(accepted.len().saturating_add(refused), OFFERED);
        let completed = block_on(join_all(accepted)).len();
        assert!(
            completed >= MAX_QUEUED_BLOCKING_JOBS,
            "every accepted job ran"
        );
        assert!(gate.peak.load(AtomicOrdering::SeqCst) <= MAX_BLOCKING_THREADS);
    }

    #[test]
    fn a_panicking_job_does_not_cost_the_pool_its_thread() {
        // One thread's worth of panics, then work: the pool must still answer,
        // because the job's panic is caught inside the job, not the thread.
        for _ in 0..4 {
            let handle = spawn_blocking(|| -> u32 {
                assert_eq!(1, 2, "deliberate");
                0
            });
            let caught = std::panic::catch_unwind(AssertUnwindSafe(|| block_on(handle)));
            assert!(caught.is_err(), "the panic reaches the awaiter");
        }
        assert_eq!(block_on(spawn_blocking(|| 5u32)), 5);
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
