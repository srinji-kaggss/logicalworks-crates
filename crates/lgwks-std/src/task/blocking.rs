//! [`JoinHandle`] and the two ways to hand the blocking pool a job:
//! [`spawn_blocking`], which never refuses, and [`try_spawn_blocking`], which
//! refuses with a typed reason.

use std::any::Any;
use std::future::Future;
use std::panic::{AssertUnwindSafe, resume_unwind};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use super::{MAX_QUEUED_BLOCKING_JOBS, Pool, SpawnError, Work, lock, pool};
#[cfg(doc)]
use super::{configure_blocking_pool, join_all, shutdown_blocking_pool};

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

impl<T> JoinHandle<T> {
    /// Whether the job has finished and left its value for the first poll.
    ///
    /// Test-only: it lets a test reach the "ready before the first poll" state
    /// by observing it rather than by waiting a guessed amount of wall time.
    #[cfg(test)]
    pub(super) fn is_done(&self) -> bool {
        matches!(lock(&self.shared).job, Job::Done(_))
    }
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
pub(super) fn spawn_blocking_on<F, T>(pool: &Arc<Pool>, job: F) -> JoinHandle<T>
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
pub(super) fn prepare<F, T>(job: F) -> (JoinHandle<T>, Work)
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
