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
use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::io;
use std::panic::{AssertUnwindSafe, resume_unwind};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

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

/// Most pool threads running jobs at once, process-wide, before any
/// [`configure_blocking_pool`] call fixes another ceiling.
///
/// Tokio's blocking pool uses the same default ceiling. It bounds what a burst
/// of blocking work can take from the OS: without it, ten thousand concurrent
/// calls were ten thousand threads, each reserving its own stack.
const MAX_BLOCKING_THREADS: usize = 512;

/// The smallest ceiling [`configure_blocking_pool`] accepts.
///
/// A pool without a thread can run nothing: every job would wait forever.
const MIN_BLOCKING_THREADS: usize = 1;

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
    Os(io::Error),
    /// The pool no longer admits work: [`shutdown_blocking_pool`] has been
    /// called. The job did not run.
    Shutdown,
}

impl fmt::Display for SpawnError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::AtCapacity { threads, queued } => write!(
                formatter,
                "the blocking pool is at capacity: {threads} threads busy and {queued} jobs waiting"
            ),
            Self::Os(ref error) => write!(formatter, "could not start a blocking thread: {error}"),
            Self::Shutdown => {
                write!(
                    formatter,
                    "the blocking pool is shut down and admits no jobs"
                )
            }
        }
    }
}

impl std::error::Error for SpawnError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Os(ref error) => Some(error),
            Self::AtCapacity { .. } | Self::Shutdown => None,
        }
    }
}

/// Why [`configure_blocking_pool`] did not fix the pool's ceiling.
///
/// The ceiling is decided once, by whatever builds the pool: a `configure`
/// call before first use, or the first job. Every later attempt is refused
/// with the pool's own numbers rather than silently ignored, so a caller
/// never believes a ceiling moved that did not.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PoolConfigError {
    /// The pool has already run work, so it was built at the default ceiling
    /// and cannot be rebuilt under another.
    InUse {
        /// The ceiling asked for.
        requested: usize,
        /// The ceiling the running pool already has.
        running: usize,
    },
    /// A `configure` call already fixed the ceiling at another value.
    AlreadyConfigured {
        /// The ceiling asked for now.
        requested: usize,
        /// The ceiling the earlier `configure` fixed.
        configured: usize,
    },
    /// A ceiling below one can never run a job.
    InvalidCeiling {
        /// The ceiling asked for.
        requested: usize,
        /// The smallest ceiling accepted.
        minimum: usize,
    },
}

impl fmt::Display for PoolConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::InUse { requested, running } => write!(
                formatter,
                "the blocking pool has already run work at {running} threads; \
                 configure_blocking_pool({requested}) must run before first use"
            ),
            Self::AlreadyConfigured {
                requested,
                configured,
            } => write!(
                formatter,
                "the blocking pool ceiling was already configured at {configured} threads, \
                 not {requested}"
            ),
            Self::InvalidCeiling { requested, minimum } => write!(
                formatter,
                "a blocking pool of {requested} threads can run nothing; \
                 the smallest ceiling is {minimum}"
            ),
        }
    }
}

impl std::error::Error for PoolConfigError {}

/// What [`shutdown_blocking_pool`] found when its wait ended.
///
/// The wait is bounded by the deadline the caller passes. When every pool
/// thread finished inside it, each one was joined and none outlives the call
/// ([`PoolShutdown::Drained`]). When the deadline expired first, the threads
/// still working are reported by count ([`PoolShutdown::DeadlineExceeded`]);
/// they keep draining on their own — shutdown closes admission, it never
/// cancels work in flight — and their join handles stay registered, so a
/// later `shutdown_blocking_pool` can wait for and join what is left of them.
#[must_use = "the report is the only record of threads still running"]
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PoolShutdown {
    /// Every pool thread finished and was joined within the deadline.
    Drained {
        /// Threads joined, including any that had already exited on their
        /// idle keep-alive.
        threads: usize,
    },
    /// The deadline expired with work still in the pool.
    DeadlineExceeded {
        /// Threads that had finished and were joined by the deadline.
        joined: usize,
        /// Threads still executing jobs at the deadline.
        running: usize,
        /// Admitted jobs still waiting for a thread at the deadline.
        queued: usize,
    },
}

/// Fix the blocking pool's thread ceiling, once, before the pool first runs.
///
/// The pool is built on its first use at [`MAX_BLOCKING_THREADS`] (512)
/// threads. This is the startup door on that choice: called before any
/// [`spawn_blocking`] or [`try_spawn_blocking`], it builds the pool at
/// `threads` instead. The arbitration is the pool's own creation — whichever
/// of a `configure` or a first use builds the pool fixes the ceiling — so a
/// ceiling is never silently ignored, whatever races the call.
///
/// The one retry that succeeds is asking again for the ceiling already in
/// force: the pool is at that ceiling, so the caller's belief matches the
/// pool. Every other later attempt is refused typed: [`PoolConfigError::InUse`]
/// once the pool has run work, [`PoolConfigError::AlreadyConfigured`] when an
/// earlier `configure` fixed a different ceiling, and
/// [`PoolConfigError::InvalidCeiling`] for a ceiling below one.
///
/// # Errors
///
/// Refuses with [`PoolConfigError::InvalidCeiling`] when `threads` is below
/// one, [`PoolConfigError::InUse`] when the pool has already run work, and
/// [`PoolConfigError::AlreadyConfigured`] when an earlier call configured a
/// different ceiling. A refusal leaves the pool exactly as it was.
///
/// # Examples
///
/// ```
/// use lgwks_std::task::configure_blocking_pool;
///
/// # fn main() -> Result<(), lgwks_std::task::PoolConfigError> {
/// configure_blocking_pool(8)?;
/// // The pool this process runs is bounded at eight threads from here on.
/// # Ok(())
/// # }
/// ```
pub fn configure_blocking_pool(threads: usize) -> Result<(), PoolConfigError> {
    if threads < MIN_BLOCKING_THREADS {
        let refusal = Err(PoolConfigError::InvalidCeiling {
            requested: threads,
            minimum: MIN_BLOCKING_THREADS,
        });
        #[cfg(feature = "trace")]
        crate::trace::debug!(
            requested = threads,
            minimum = MIN_BLOCKING_THREADS,
            "configure_blocking_pool: a ceiling below one can run nothing"
        );
        return refusal;
    }
    // Building the pool here is the arbitration: the OnceLock decides between
    // a configure and a first use, and neither can miss the other's write.
    let pool = POOL.get_or_init(|| Arc::new(Pool::at_configured(threads, start_os_thread)));
    let decision = configure_decision(pool, threads);
    if let Err(ref error) = decision {
        #[cfg(feature = "trace")]
        crate::trace::debug!(
            error = ?error,
            "configure_blocking_pool: the ceiling stands as it was"
        );
    }
    decision
}

/// The configure decision for a pool that already exists, shared by
/// [`configure_blocking_pool`] and the tests. The ceiling is whatever built
/// the pool; this only reports which of the two builders won.
fn configure_decision(pool: &Pool, threads: usize) -> Result<(), PoolConfigError> {
    match pool.configured {
        Some(configured) if configured == threads => Ok(()),
        Some(configured) => Err(PoolConfigError::AlreadyConfigured {
            requested: threads,
            configured,
        }),
        None => Err(PoolConfigError::InUse {
            requested: threads,
            running: pool.ceiling,
        }),
    }
}

/// Close the pool's admission, let its admitted jobs finish, and join every
/// pool thread — or report, inside `within`, the threads still running.
///
/// Queued and running jobs are never cancelled: the threads drain the queue
/// and exit instead of parking, so the pool empties by finishing its work.
/// Parked threads are woken to take their share or leave, and a thread that
/// has already exited on its idle keep-alive is still joined. The call waits
/// for the last thread to leave; when the deadline expires first it returns
/// [`PoolShutdown::DeadlineExceeded`] naming what is left, and those threads
/// finish and leave on their own — a second call waits for and joins them.
///
/// From the first call on, admission is closed: [`try_spawn_blocking`]
/// refuses every job as [`SpawnError::Shutdown`], and [`spawn_blocking`]
/// fails its awaiter with that refusal as the payload. The closure is never
/// run.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
///
/// use lgwks_std::task::{
///     PoolShutdown, SpawnError, block_on, shutdown_blocking_pool, spawn_blocking,
///     try_spawn_blocking,
/// };
///
/// let job = spawn_blocking(|| 40 + 2);
/// let report = shutdown_blocking_pool(Duration::from_secs(30));
/// // The job was still in the pool: shutdown let it finish before returning.
/// assert_eq!(block_on(job), 42);
/// assert_eq!(report, PoolShutdown::Drained { threads: 1 });
/// // Admission is closed from here on.
/// assert!(matches!(try_spawn_blocking(|| 0u32), Err(SpawnError::Shutdown)));
/// ```
pub fn shutdown_blocking_pool(within: Duration) -> PoolShutdown {
    pool().shutdown(within)
}

/// One job, with its result already wired to its handle.
type Work = Box<dyn FnOnce() + Send>;

/// A pool thread's own OS handle, as distinct from the awaitable
/// [`JoinHandle`] a caller holds: one is joined by a shutdown, the other is
/// awaited by the task the job's result belongs to.
type ThreadHandle = thread::JoinHandle<()>;

/// The process-wide pool.
struct Pool {
    /// Queue and thread accounting, under one lock.
    state: Mutex<PoolState>,
    /// Signalled once per job handed to an idle thread, and by every thread
    /// that leaves the pool.
    work_ready: Condvar,
    /// The most threads alive at once.
    ceiling: usize,
    /// `Some(threads)` when [`configure_blocking_pool`] built this pool at
    /// that ceiling; `None` when first use built it at the default.
    configured: Option<usize>,
    /// Starts one thread running [`Pool::run`]. The process pool starts an OS
    /// thread and hands back its join handle; a test pool can refuse, which is
    /// how a refusal is exercised without exhausting the machine's threads.
    start: fn(Arc<Pool>, Handoff) -> io::Result<ThreadHandle>,
}

/// What the pool's lock guards.
///
/// Generic over the job so the deterministic simulation (`sim_pool`) drives
/// these same transitions with numbered jobs instead of closures. Every
/// transition is a method below; [`Pool::submit`] and [`Pool::run`] only add
/// the lock, the condvar, and running the job.
struct PoolState<W = Work> {
    /// Jobs waiting for a thread, oldest first.
    queue: VecDeque<W>,
    /// Threads alive, running or idle.
    live: usize,
    /// Threads waiting for work that no submitter has claimed yet.
    idle: usize,
    /// Wakeups claimed by submitters and not yet taken by a thread. Counted
    /// rather than inferred from the condvar, which may wake spuriously.
    wakeups: usize,
    /// Set by [`Pool::shutdown`]: admission is closed, and a thread with no
    /// job leaves instead of parking.
    draining: bool,
    /// The join handle of every thread this pool started and has not joined.
    /// Registered under the same lock that counted the start, so a shutdown
    /// can never observe a thread it cannot join.
    handles: Vec<ThreadHandle>,
}

/// What admitting a job asks of the submitter.
#[derive(Debug, PartialEq, Eq)]
enum Admitted<W> {
    /// An idle thread was claimed: notify the condvar.
    Woke,
    /// No thread is idle and the pool is under its ceiling: start one with
    /// this job as its first, then report [`PoolState::started`] or
    /// [`PoolState::start_failed`]. The job never passes through the queue,
    /// so the new thread does not contend for the lock to reach it.
    Start(W),
    /// Every thread is busy at the ceiling: the job waits its turn.
    Queued,
}

/// Why admitting a job was refused. The job is handed back: it did not run.
#[derive(Debug, PartialEq, Eq)]
enum Refused<W> {
    /// The wait queue is at its declared bound.
    AtCapacity(W),
    /// The pool no longer admits work.
    Draining(W),
}

/// What a parked thread does once it holds the lock again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Woken {
    /// It took a claimed wakeup: look for work.
    Resume,
    /// It waited out the keep-alive unclaimed: it has left the pool.
    Exit,
    /// A spurious wake: wait again.
    Wait,
}

impl<W> PoolState<W> {
    /// An empty pool.
    const fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            live: 0,
            idle: 0,
            wakeups: 0,
            draining: false,
            handles: Vec::new(),
        }
    }

    /// Admit `work`, or hand it back when the pool takes no more.
    fn admit(
        &mut self,
        work: W,
        queue_limit: Option<usize>,
        ceiling: usize,
    ) -> Result<Admitted<W>, Refused<W>> {
        if self.draining {
            return Err(Refused::Draining(work));
        }
        if queue_limit.is_some_and(|limit| self.queue.len() >= limit) {
            #[cfg(feature = "trace")]
            crate::trace::debug!(queued = self.queue.len(), limit = ?queue_limit, "admit: the wait queue is at its bound");
            return Err(Refused::AtCapacity(work));
        }
        if self.idle == 0 && self.live < ceiling {
            return Ok(Admitted::Start(work));
        }
        self.queue.push_back(work);
        if self.idle > 0 {
            self.idle = self.idle.saturating_sub(1);
            self.wakeups = self.wakeups.saturating_add(1);
            return Ok(Admitted::Woke);
        }
        Ok(Admitted::Queued)
    }

    /// The thread [`Admitted::Start`] asked for is running.
    const fn started(&mut self) {
        self.live = self.live.saturating_add(1);
    }

    /// The thread [`Admitted::Start`] asked for could not start, and `work`
    /// was its first job. With a live thread the job is queued for it
    /// (`None`): no thread is idle, since admission chose to start one, and
    /// the lock has been held since. With none, nothing would ever run it, so
    /// it is handed back.
    fn start_failed(&mut self, work: W) -> Option<W> {
        if self.live > 0 {
            self.queue.push_back(work);
            return None;
        }
        Some(work)
    }

    /// The next job for a thread that is free.
    fn take(&mut self) -> Option<W> {
        self.queue.pop_front()
    }

    /// A free thread found no job: park it, or — while the pool drains —
    /// retire it instead. Returns `true` when the thread leaves the pool.
    const fn park_or_retire(&mut self) -> bool {
        if self.draining {
            self.live = self.live.saturating_sub(1);
            return true;
        }
        self.idle = self.idle.saturating_add(1);
        false
    }

    /// A parked thread holds the lock again, its wait having `timed_out` or
    /// not.
    const fn woken(&mut self, timed_out: bool) -> Woken {
        if self.wakeups > 0 {
            // The submitter already moved this thread out of `idle`.
            self.wakeups = self.wakeups.saturating_sub(1);
            return Woken::Resume;
        }
        if timed_out {
            self.idle = self.idle.saturating_sub(1);
            self.live = self.live.saturating_sub(1);
            return Woken::Exit;
        }
        Woken::Wait
    }

    /// A parked thread woken without a claimed wakeup: while the pool drains
    /// it takes a waiting job if there is one, and otherwise leaves. Its
    /// `idle` was never claimed by a submitter, so it retires that itself.
    fn drain_wake(&mut self) -> Woken {
        if !self.draining {
            return Woken::Wait;
        }
        self.idle = self.idle.saturating_sub(1);
        if self.queue.is_empty() {
            self.live = self.live.saturating_sub(1);
            return Woken::Exit;
        }
        Woken::Resume
    }

    /// Close admission: every later submit is refused, and a thread with no
    /// job leaves instead of parking.
    const fn begin_drain(&mut self) {
        self.draining = true;
    }
}

/// The process-wide pool. [`configure_blocking_pool`] and first use race to
/// initialize it; whoever wins fixes the ceiling, and the `OnceLock` is the
/// whole of that arbitration.
static POOL: OnceLock<Arc<Pool>> = OnceLock::new();

/// The pool, built on first use at the default ceiling.
///
/// A reference, not a clone: the pool outlives every caller by construction,
/// and the one place a thread needs its own reference — the `start` call
/// inside [`Pool::submit`] — clones from the `&Arc` it already holds.
fn pool() -> &'static Arc<Pool> {
    POOL.get_or_init(|| Arc::new(Pool::new(MAX_BLOCKING_THREADS, start_os_thread)))
}

/// Start one named OS thread serving `pool`, and hand back its join handle.
///
/// The thread owns its reference to the pool and runs the job in `first`
/// before it ever takes the lock. The handle is registered in the pool's state
/// under the same lock that counted the start, so a shutdown can never observe
/// a thread it cannot join.
fn start_os_thread(pool: Arc<Pool>, first: Handoff) -> io::Result<ThreadHandle> {
    thread::Builder::new()
        .name("lgwks-blocking".into())
        .spawn(move || {
            let work = lock(&first).take();
            drop(first);
            if let Some(work) = work {
                work();
            }
            pool.run();
        })
}

/// A new thread's first job. Shared rather than moved into the thread's
/// closure because a thread that fails to start drops its closure, and the
/// job must come back to be queued or refused rather than vanish with it.
type Handoff = Arc<Mutex<Option<Work>>>;

/// Join one pool thread, reporting a panic rather than resuming it.
///
/// A pool thread cannot panic through its jobs — `prepare` catches each job's
/// panic — so an unwind here is a defect outside any job. It is reported as a
/// trace event; the thread still counts as finished, because `join`
/// returning is the fact a shutdown waits on.
fn join_pool_thread(handle: ThreadHandle) {
    if let Err(_payload) = handle.join() {
        #[cfg(feature = "trace")]
        crate::trace::debug!("shutdown: a pool thread exited on a panic outside any job");
    }
}

impl Pool {
    /// An empty pool of at most `ceiling` threads, started by `start`, built
    /// by first use rather than by [`configure_blocking_pool`].
    const fn new(
        ceiling: usize,
        start: fn(Arc<Self>, Handoff) -> io::Result<ThreadHandle>,
    ) -> Self {
        Self {
            state: Mutex::new(PoolState::new()),
            work_ready: Condvar::new(),
            ceiling,
            configured: None,
            start,
        }
    }

    /// An empty pool whose ceiling a [`configure_blocking_pool`] call fixed.
    const fn at_configured(
        ceiling: usize,
        start: fn(Arc<Self>, Handoff) -> io::Result<ThreadHandle>,
    ) -> Self {
        Self {
            configured: Some(ceiling),
            state: Mutex::new(PoolState::new()),
            work_ready: Condvar::new(),
            ceiling,
            start,
        }
    }

    /// Queue `work` and make sure a thread will run it.
    ///
    /// An idle thread is woken if there is one; otherwise a thread is started
    /// while the pool is under its ceiling; otherwise the job waits for the
    /// next thread to finish. `queue_limit` bounds the wait queue.
    fn submit(self: &Arc<Self>, work: Work, queue_limit: Option<usize>) -> Result<(), SpawnError> {
        let mut state = lock(&self.state);
        let admitted = match state.admit(work, queue_limit, self.ceiling) {
            Ok(admitted) => admitted,
            Err(Refused::AtCapacity(_refused)) => {
                let refusal = Err(SpawnError::AtCapacity {
                    threads: self.ceiling,
                    queued: queue_limit.unwrap_or(usize::MAX),
                });
                #[cfg(feature = "trace")]
                crate::trace::debug!(error = ?refusal.as_ref().err(), "submit: the blocking pool refused a job");
                return refusal;
            }
            Err(Refused::Draining(_refused)) => {
                let refusal = Err(SpawnError::Shutdown);
                #[cfg(feature = "trace")]
                crate::trace::debug!(error = ?refusal.as_ref().err(), "submit: the blocking pool is shut down");
                return refusal;
            }
        };
        match admitted {
            Admitted::Woke => {
                self.work_ready.notify_one();
                Ok(())
            }
            Admitted::Queued => Ok(()),
            Admitted::Start(work) => {
                let first = Arc::new(Mutex::new(Some(work)));
                match (self.start)(Arc::clone(self), Arc::clone(&first)) {
                    Ok(handle) => {
                        state.started();
                        state.handles.push(handle);
                        Ok(())
                    }
                    Err(error) => {
                        // A starter reports failure only when its thread never
                        // ran, so the job is still in the slot.
                        let Some(work) = lock(&first).take() else {
                            return Ok(());
                        };
                        match state.start_failed(work) {
                            // A live thread will reach the job when it
                            // finishes its own.
                            None => Ok(()),
                            Some(_unrun) => {
                                let refusal = Err(SpawnError::Os(error));
                                #[cfg(feature = "trace")]
                                crate::trace::debug!(error = ?refusal.as_ref().err(), "submit: no blocking thread could be started");
                                refusal
                            }
                        }
                    }
                }
            }
        }
    }

    /// One pool thread: run queued jobs, wait for more, exit when idle for
    /// [`BLOCKING_KEEP_ALIVE`] — or at once once the pool drains.
    fn run(&self) {
        let mut state = lock(&self.state);
        loop {
            if let Some(work) = state.take() {
                drop(state);
                // `prepare` catches the job's panic, so a failing job cannot
                // take the thread down with it.
                work();
                state = lock(&self.state);
                continue;
            }
            if state.park_or_retire() {
                // A thread leaving can be the last one a shutdown waits for,
                // and this exit can race one: say so under its lock.
                self.work_ready.notify_all();
                return;
            }
            loop {
                let (guard, waited) = self
                    .work_ready
                    .wait_timeout(state, BLOCKING_KEEP_ALIVE)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state = guard;
                match state.woken(waited.timed_out()) {
                    Woken::Resume => break,
                    Woken::Exit => {
                        self.work_ready.notify_all();
                        return;
                    }
                    // Spurious, or the broadcast a draining pool makes when
                    // admission closes: the drain rules decide whether this
                    // thread works or leaves.
                    Woken::Wait => match state.drain_wake() {
                        Woken::Resume => break,
                        Woken::Exit => {
                            self.work_ready.notify_all();
                            return;
                        }
                        Woken::Wait => {}
                    },
                }
            }
        }
    }

    /// Close admission, wait for the drain, and join what the deadline let
    /// finish.
    ///
    /// Admission closes first, under the pool's lock, so a submit that races
    /// the shutdown is either admitted before the flag or refused as
    /// [`SpawnError::Shutdown`]; there is no third outcome. The wait that
    /// follows is bounded by `within`: the last thread to leave wakes this
    /// waiter, and a deadline that expires first reports, by count, the
    /// threads still running and the jobs still waiting for one. Handles of
    /// threads that had not finished stay registered, so a later shutdown
    /// joins them; a thread never outlives a `Drained` report.
    fn shutdown(self: &Arc<Self>, within: Duration) -> PoolShutdown {
        let deadline = Instant::now().checked_add(within);
        let mut state = lock(&self.state);
        state.begin_drain();
        // Every parked thread re-checks under this lock: it takes a waiting
        // job or leaves, so the drain needs nobody to submit again.
        self.work_ready.notify_all();
        loop {
            if state.live == 0 {
                let threads = state.handles.len();
                let handles = std::mem::take(&mut state.handles);
                drop(state);
                for handle in handles {
                    join_pool_thread(handle);
                }
                return PoolShutdown::Drained { threads };
            }
            let now = Instant::now();
            if deadline.is_some_and(|until| now >= until) {
                let running = state.live;
                let queued = state.queue.len();
                let mut joined = 0usize;
                let mut outstanding = Vec::new();
                for handle in std::mem::take(&mut state.handles) {
                    // `is_finished` never blocks: the deadline has passed, so
                    // this joins only what already ended and keeps the rest.
                    if !handle.is_finished() {
                        outstanding.push(handle);
                        continue;
                    }
                    join_pool_thread(handle);
                    joined = joined.saturating_add(1);
                }
                state.handles = outstanding;
                drop(state);
                return PoolShutdown::DeadlineExceeded {
                    joined,
                    running,
                    queued,
                };
            }
            let wait = match deadline {
                // No finite deadline: wait in keep-alive chunks, woken by
                // each thread that leaves.
                None => BLOCKING_KEEP_ALIVE,
                Some(until) => until.saturating_duration_since(now),
            };
            let (guard, _) = self
                .work_ready
                .wait_timeout(state, wait)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state = guard;
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
// The workspace ban list (`clippy.toml`) forbids `std::thread::sleep` because
// it blocks the executor thread. One scenario here is about the pause: a
// shutdown has to release a *parked* thread, and only a real sleep lets the
// thread reach the park first. That wait is the subject under test, in a
// test binary whose only executor is the one under test.
#[expect(
    clippy::disallowed_methods,
    reason = "the parked-thread scenario must let a real thread reach its park before the shutdown wakes it"
)]
#[path = "sim_pool_lifetime.rs"]
mod sim_pool_lifetime;

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
        for (waiters, wakes) in [
            (1usize, 0usize),
            (10, 3),
            (1_000, 50),
            (10_000, 20),
            (100_000, 5),
        ] {
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

    /// A starter that never starts a thread, as when the OS is out of them.
    fn refuse_to_start(_pool: Arc<Pool>, _first: Handoff) -> io::Result<ThreadHandle> {
        Err(io::Error::new(
            io::ErrorKind::OutOfMemory,
            "injected: no thread",
        ))
    }

    #[test]
    fn a_thread_that_cannot_start_refuses_the_job_and_it_never_runs() {
        fn refusing() -> &'static Arc<Pool> {
            static OWN: OnceLock<Arc<Pool>> = OnceLock::new();
            OWN.get_or_init(|| Arc::new(Pool::new(4, refuse_to_start)))
        }
        let pool = refusing();
        let ran = Arc::new(AtomicBool::new(false));
        let witness = Arc::clone(&ran);
        let (_handle, work) = prepare(move || witness.store(true, Ordering::SeqCst));
        match pool.submit(work, Some(8)) {
            Err(SpawnError::Os(error)) => assert_eq!(
                (error.kind(), error.to_string()),
                (io::ErrorKind::OutOfMemory, "injected: no thread".to_owned()),
                "the OS's own error is the source"
            ),
            other => unreachable!("expected an Os refusal, got {other:?}"),
        }
        let state = lock(&pool.state);
        assert!(
            state.queue.is_empty() && state.live == 0,
            "a refused job leaves nothing queued and no thread counted"
        );
        drop(state);
        assert!(!ran.load(Ordering::SeqCst), "a refused job never runs");
    }

    #[test]
    fn spawn_blocking_reports_a_refusal_to_its_awaiter_as_the_job_failing() {
        fn refusing() -> &'static Arc<Pool> {
            static OWN: OnceLock<Arc<Pool>> = OnceLock::new();
            OWN.get_or_init(|| Arc::new(Pool::new(4, refuse_to_start)))
        }
        let pool = refusing();
        let handle = spawn_blocking_on(pool, || 7u32);
        let unwound = std::panic::catch_unwind(AssertUnwindSafe(|| block_on(handle)));
        let payload = unwound
            .err()
            .map(|payload| payload.downcast::<SpawnError>());
        assert!(
            matches!(payload, Some(Ok(ref error)) if matches!(**error, SpawnError::Os(_))),
            "the awaiter unwinds with the SpawnError itself"
        );
    }

    #[test]
    fn a_failed_start_beside_a_live_thread_leaves_the_job_to_that_thread() {
        /// Starts the first thread, then refuses every later one.
        fn first_only(pool: Arc<Pool>, first: Handoff) -> io::Result<ThreadHandle> {
            static STARTS: AtomicUsize = AtomicUsize::new(0);
            if STARTS.fetch_add(1, AtomicOrdering::SeqCst) == 0 {
                start_os_thread(pool, first)
            } else {
                refuse_to_start(pool, first)
            }
        }
        fn single() -> &'static Arc<Pool> {
            static OWN: OnceLock<Arc<Pool>> = OnceLock::new();
            OWN.get_or_init(|| Arc::new(Pool::new(4, first_only)))
        }
        let pool = single();
        let gate = Arc::new(Gate::default());
        let held = Arc::clone(&gate);
        let first = spawn_blocking_on(pool, move || {
            held.pass();
            thread::current().id()
        });
        let second = spawn_blocking_on(pool, || thread::current().id());
        assert_eq!(lock(&pool.state).live, 1, "the second start was refused");
        gate.release();
        let (first, second) = (block_on(first), block_on(second));
        assert_eq!(
            first, second,
            "the live thread ran the job its sibling could not start"
        );
    }

    #[test]
    fn spawn_blocking_returns_value() {
        assert_eq!(block_on(spawn_blocking(|| 99u32)), 99);
    }

    /// A configure after the pool has run work names the ceiling that stands.
    #[test]
    fn a_configure_after_the_pool_has_run_is_refused_with_the_running_ceiling() {
        let pool = Arc::new(Pool::new(4, start_os_thread));
        assert_eq!(block_on(spawn_blocking_on(&pool, || 1u32)), 1);
        assert_eq!(
            configure_decision(&pool, 8),
            Err(PoolConfigError::InUse {
                requested: 8,
                running: 4
            }),
            "a configure after first use is refused, never silently ignored"
        );
        // Even the same number is `InUse`: this pool ran before it was asked
        // about, and the answer reports how its ceiling really came about.
        assert_eq!(
            configure_decision(&pool, 4),
            Err(PoolConfigError::InUse {
                requested: 4,
                running: 4
            })
        );
    }

    /// A configured pool answers a second configure by name: the same ceiling
    /// is the one in force, a different one is refused with both numbers.
    #[test]
    fn a_second_configure_is_refused_named_or_is_the_ceiling_already_in_force() {
        let pool = Arc::new(Pool::at_configured(6, refuse_to_start));
        assert_eq!(
            configure_decision(&pool, 6),
            Ok(()),
            "asking again for the ceiling already in force describes the pool as it is"
        );
        assert_eq!(
            configure_decision(&pool, 9),
            Err(PoolConfigError::AlreadyConfigured {
                requested: 9,
                configured: 6
            })
        );
    }

    /// A ceiling below one is refused before the pool is touched, so the arm
    /// is decided by the argument alone.
    #[test]
    fn a_ceiling_below_one_is_refused_before_the_pool_is_touched() {
        assert_eq!(
            configure_blocking_pool(0),
            Err(PoolConfigError::InvalidCeiling {
                requested: 0,
                minimum: 1
            })
        );
    }

    /// After a shutdown both entry points refuse, the job never runs, and the
    /// never-refusing one fails its awaiter with the typed refusal as the
    /// payload. The drain, deadline and parked-thread arms are the seeded
    /// journeys in `sim_pool_lifetime`.
    #[test]
    fn after_a_shutdown_admission_is_refused_and_the_job_never_runs() {
        let pool = Arc::new(Pool::new(2, start_os_thread));
        let warm = spawn_blocking_on(&pool, || 3u32);
        assert_eq!(block_on(warm), 3);
        assert_eq!(
            pool.shutdown(Duration::from_secs(10)),
            PoolShutdown::Drained { threads: 1 }
        );

        let ran = Arc::new(AtomicBool::new(false));
        let witness = Arc::clone(&ran);
        // The bounded arm, through the pool's own submit: a refusal it hands
        // back leaves the caller no handle, so the refused one is dropped
        // rather than awaited.
        let (refused, work) = prepare(move || witness.store(true, AtomicOrdering::SeqCst));
        assert!(
            matches!(pool.submit(work, Some(8)), Err(SpawnError::Shutdown)),
            "the bounded entry point refuses with the typed reason"
        );
        drop(refused);

        // The never-refusing arm, through the entry point that hands a handle
        // back whatever happened: the refusal is the job's failure.
        let handle = spawn_blocking_on(&pool, || 11u32);
        let unwound = std::panic::catch_unwind(AssertUnwindSafe(|| block_on(handle)));
        let payload = unwound
            .err()
            .map(|payload| payload.downcast::<SpawnError>());
        assert!(
            matches!(payload, Some(Ok(ref error)) if matches!(**error, SpawnError::Shutdown)),
            "the never-refusing entry point fails its awaiter with the refusal"
        );
        assert!(!ran.load(AtomicOrdering::SeqCst), "a refused job never ran");
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
