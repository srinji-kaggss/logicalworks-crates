//! `task` owns synchronous future execution and zero-dependency concurrency
//! for CLI tools, DAG interpreters, polling loops, and test runners, enforcing
//! INV-TASK-ZERO-RUNTIME: futures are driven to completion on the current
//! thread using `std::task::Wake` and OS thread parking, with zero background
//! reactors and zero external dependencies. The one pool is for blocking work:
//! it is bounded (512 threads, or the one ceiling
//! [`configure_blocking_pool`](crate::task::configure_blocking_pool) fixes
//! once before first use), created on first use, and its threads exit after
//! ten idle seconds, so a process with no blocking work holds no thread.
//! [`shutdown_blocking_pool`](crate::task::shutdown_blocking_pool) closes
//! admission, lets the admitted jobs finish, and joins every pool thread
//! within a caller-given deadline.
//!
//! Three primitives compose:
//!
//! - [`block_on`] drives one future to completion on the calling thread.
//! - [`join_all`] drives many futures concurrently on the calling thread and
//!   returns their outputs in input order.
//! - [`spawn_blocking`] runs one blocking closure on the bounded blocking pool
//!   and returns a future for its result, so a blocking syscall (file read,
//!   HTTP) does not stall the sibling futures driven by [`join_all`] or
//!   [`block_on`]. [`try_spawn_blocking`](crate::task::try_spawn_blocking) is
//!   the same with a bounded queue and a typed
//!   [`SpawnError`](crate::task::SpawnError) instead of an unbounded wait.
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
use std::sync::{Arc, Condvar, Mutex, MutexGuard, WaitTimeoutResult};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};
use std::time::Duration;
// The queue, formatting, I/O and clock imports moved to `pool` with the code
// that uses them. These lines keep `spawn_blocking`'s signature at the line
// the background-work guide cites.

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
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Wait on `condition` until woken, treating poisoning as non-fatal.
///
/// The same guarded state `lock` recovers, through the same argument: every
/// critical section behind these primitives moves a whole [`Job`] in or out and
/// writes no partial state, so a poisoned guard still names a consistent value
/// and the panic that poisoned it is already being resumed on the awaiter. One
/// helper per lock shape rather than one recovery per call site, because a
/// fourth spelling of the same recovery is a fourth place to get it wrong.
///
/// Untimed because that is the shape a test gate needs: the pool itself only
/// ever parks against a deadline, and carries no wait that has no bound.
#[cfg(test)]
fn wait<'a, T>(condition: &Condvar, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
    match condition.wait(guard) {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Wait on `condition` for at most `timeout`, treating poisoning as non-fatal.
///
/// [`wait`]'s argument applies unchanged to the timed form: the guard it returns
/// names the same consistent state, and the wait outcome is reported beside it.
fn wait_timeout<'a, T>(
    condition: &Condvar,
    guard: MutexGuard<'a, T>,
    timeout: Duration,
) -> (MutexGuard<'a, T>, WaitTimeoutResult) {
    match condition.wait_timeout(guard, timeout) {
        Ok(waited) => waited,
        Err(poisoned) => poisoned.into_inner(),
    }
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
/// process-wide — the one ceiling [`configure_blocking_pool`] can fix once,
/// before first use. A job submitted while all of them are busy waits in a
/// queue and runs when one frees up, in submission order. A thread that has
/// had no work for 10 seconds exits, so an idle process holds no pool
/// thread. Jobs that wait on *each other* must therefore number fewer than
/// the ceiling, or the waiters hold every thread the awaited job needs.
///
/// This entry point keeps its 1.0 contract: its queue is not bounded and it
/// never refuses. [`try_spawn_blocking`] is the bounded form, and the one to
/// use where callers are not already bounding their own fan-out.
///
/// Failure: a panicking closure, an OS refusal to start the first pool
/// thread, or a pool already closed by [`shutdown_blocking_pool`], is resumed
/// as a panic on the task that awaits the handle, the refusal carrying the
/// [`SpawnError`] as its payload. Success, panic, and refusal all wake the
/// task exactly once.
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
    spawn_blocking_on(pool(), job)
}

/// [`spawn_blocking`] on `pool`: a refusal reaches the awaiter as the job's
/// failure, carrying the [`SpawnError`] as the unwind payload.
fn spawn_blocking_on<F, T>(pool: &Arc<Pool>, job: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let (handle, work) = prepare(job);
    if let Err(error) = pool.submit(work, None) {
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
/// later, is [`SpawnError::Os`]. Once [`shutdown_blocking_pool`] has been
/// called, every job is refused as [`SpawnError::Shutdown`]. A refused job is
/// dropped without running.
///
/// # Errors
///
/// [`SpawnError::AtCapacity`] when the queue is full, [`SpawnError::Os`]
/// when no thread could be started to run the job, and
/// [`SpawnError::Shutdown`] when the pool no longer admits work.
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

// Its own file; the public surface is re-exported so every path stays
// `lgwks_std::task::<Name>`.
mod pool;

use pool::{MAX_QUEUED_BLOCKING_JOBS, Pool, Work, pool};
pub use pool::{
    PoolConfigError, PoolShutdown, SpawnError, configure_blocking_pool, shutdown_blocking_pool,
};
// The simulation families and the tests drive the pool's own transitions, and
// they reach them through `super::*` from here, as they did before the split.
#[cfg(test)]
use pool::{
    Admitted, BLOCKING_KEEP_ALIVE, Handoff, MAX_BLOCKING_THREADS, PoolState, Refused, ThreadHandle,
    Woken, configure_decision, serve_pool, start_os_thread,
};

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

// The seeded generator and replay harness the simulation families share,
// declared here once: a file loaded as a module twice is two copies of every
// type in it, which clippy refuses. Each family uses them under the names they
// already had, so a new family cannot diverge in how it seeds or replays.
#[cfg(test)]
#[path = "../tests/support/rng.rs"]
mod rng;
#[cfg(test)]
#[path = "../tests/support/seeded_sweep.rs"]
mod seeded_sweep;

#[cfg(test)]
#[path = "sim_task_pool.rs"]
mod sim_pool;

#[cfg(test)]
// This module is about the pause: a shutdown has to release a *parked* thread,
// and only a real wait lets the thread reach its park first. It waits with
// `thread::park_timeout`, the synchronous wait this workspace sanctions in
// place of the banned `thread::sleep`, in a test binary whose only executor is
// the one under test.
#[path = "sim_pool_lifetime.rs"]
mod sim_pool_lifetime;

#[cfg(test)]
// Tests of `lgwks_std::task` itself, which is a *thread-parking* executor.
// Waking it requires a real second thread, and letting the driver park before
// that wake requires a real wait. Both are the subject under test, so they are
// the sanctioned forms: `thread::Builder::spawn` with the handle owned by the
// test that joins it, and `thread::park_timeout` in place of the banned
// `thread::sleep`.
mod tests;
