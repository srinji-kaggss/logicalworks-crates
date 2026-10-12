//! The process-wide blocking pool behind spawn_blocking: its declared bounds and refusals, configuration and shutdown, and the PoolState transitions every submit and pool thread drives.

use std::collections::VecDeque;
use std::fmt;
use std::io;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use super::{lock, wait_timeout};
#[cfg(doc)]
use super::{spawn_blocking, try_spawn_blocking};

/// Most pool threads running jobs at once, process-wide, before any
/// [`configure_blocking_pool`] call fixes another ceiling.
///
/// Tokio's blocking pool uses the same default ceiling. It bounds what a burst
/// of blocking work can take from the OS: without it, ten thousand concurrent
/// calls were ten thousand threads, each reserving its own stack.
pub(super) const MAX_BLOCKING_THREADS: usize = 512;

/// The smallest ceiling [`configure_blocking_pool`] accepts.
///
/// A pool without a thread can run nothing: every job would wait forever.
pub(super) const MIN_BLOCKING_THREADS: usize = 1;

/// Most jobs [`try_spawn_blocking`] lets wait for a thread before it refuses.
pub(super) const MAX_QUEUED_BLOCKING_JOBS: usize = 16_384;

/// How long a pool thread with no work waits for some before it exits.
pub(super) const BLOCKING_KEEP_ALIVE: Duration = Duration::from_secs(10);

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
/// The pool is built on its first use at its default ceiling of 512
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
    // The whole `if` is behind the feature, so a build without `trace` never
    // binds the error at all.
    #[cfg(feature = "trace")]
    if let Err(ref error) = decision {
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
pub(super) fn configure_decision(pool: &Pool, threads: usize) -> Result<(), PoolConfigError> {
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
pub(super) type Work = Box<dyn FnOnce() + Send>;

/// A pool thread's own OS handle, as distinct from the awaitable
/// [`JoinHandle`] a caller holds: one is joined by a shutdown, the other is
/// awaited by the task the job's result belongs to.
pub(super) type ThreadHandle = thread::JoinHandle<()>;

/// The process-wide pool.
pub(super) struct Pool {
    /// Queue and thread accounting, under one lock.
    pub(super) state: Mutex<PoolState>,
    /// Signalled once per job handed to an idle thread, and by every thread
    /// that leaves the pool.
    pub(super) work_ready: Condvar,
    /// The most threads alive at once.
    pub(super) ceiling: usize,
    /// `Some(threads)` when [`configure_blocking_pool`] built this pool at
    /// that ceiling; `None` when first use built it at the default.
    pub(super) configured: Option<usize>,
    /// How long a thread with no work waits before it leaves. The process pool
    /// always uses [`BLOCKING_KEEP_ALIVE`]; the seeded lifetime family builds
    /// pools with a short one, so many burst/idle cycles cost milliseconds
    /// rather than the ten seconds each would really take.
    pub(super) keep_alive: Duration,
    /// Starts one thread running [`Pool::run`]. The process pool starts an OS
    /// thread and hands back its join handle; a test pool can refuse, which is
    /// how a refusal is exercised without exhausting the machine's threads.
    pub(super) start: fn(Arc<Pool>, Handoff) -> io::Result<ThreadHandle>,
}

/// What the pool's lock guards.
///
/// Generic over the job so the deterministic simulation (`sim_pool`) drives
/// these same transitions with numbered jobs instead of closures. Every
/// transition is a method below; [`Pool::submit`] and [`Pool::run`] only add
/// the lock, the condvar, and running the job.
pub(super) struct PoolState<W = Work> {
    /// Jobs waiting for a thread, oldest first.
    pub(super) queue: VecDeque<W>,
    /// Threads alive, running or idle.
    pub(super) live: usize,
    /// Threads waiting for work that no submitter has claimed yet.
    pub(super) idle: usize,
    /// Wakeups claimed by submitters and not yet taken by a thread. Counted
    /// rather than inferred from the condvar, which may wake spuriously.
    pub(super) wakeups: usize,
    /// Set by [`Pool::shutdown`]: admission is closed, and a thread with no
    /// job leaves instead of parking.
    pub(super) draining: bool,
    /// The join handle of every thread this pool started and has not joined.
    ///
    /// Registered under the same lock that counted the start, so a shutdown
    /// can never observe a thread it cannot join. What the list holds is
    /// exactly:
    ///
    /// ```text
    /// handles.len() == live + (threads that have left the accounting and not
    ///                          yet returned)
    /// ```
    ///
    /// A thread decrements `live` under the lock and only then returns, so
    /// every entry beyond `live` is a real OS thread still executing its last
    /// instructions. **There is no constant bound on that second group.** Each
    /// departure frees a slot immediately while the thread keeps running, so
    /// a scheduler that deschedules every departing thread in turn admits the
    /// next one; at a ceiling of one the list can hold several handles with a
    /// single thread live. What is true is the identity above and what the
    /// reap does with it: every thread that *has* returned is joined at the
    /// next start or at a shutdown, so the list never holds a
    /// finished-but-unjoined thread past one of those, and it does not grow
    /// across idle cycles.
    pub(super) handles: Vec<ThreadHandle>,
}

/// What admitting a job asks of the submitter.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Admitted<W> {
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
pub(super) enum Refused<W> {
    /// The wait queue is at its declared bound.
    AtCapacity(W),
    /// The pool no longer admits work.
    Draining(W),
}

/// What a parked thread does once it holds the lock again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Woken {
    /// It took a claimed wakeup: look for work.
    Resume,
    /// It waited out the keep-alive unclaimed: it has left the pool.
    Exit,
    /// A spurious wake: wait again.
    Wait,
}

impl<W> PoolState<W> {
    /// An empty pool.
    pub(super) const fn new() -> Self {
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
    pub(super) fn admit(
        &mut self,
        work: W,
        queue_limit: Option<usize>,
        ceiling: usize,
    ) -> Result<Admitted<W>, Refused<W>> {
        if self.draining {
            #[cfg(feature = "trace")]
            crate::trace::debug!(
                live = self.live,
                queued = self.queue.len(),
                "admit: the pool is shutting down and admits nothing"
            );
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
    pub(super) const fn started(&mut self) {
        self.live = self.live.saturating_add(1);
    }

    /// The thread [`Admitted::Start`] asked for could not start, and `work`
    /// was its first job. With a live thread the job is queued for it
    /// (`None`): no thread is idle, since admission chose to start one, and
    /// the lock has been held since. With none, nothing would ever run it, so
    /// it is handed back.
    pub(super) fn start_failed(&mut self, work: W) -> Option<W> {
        if self.live > 0 {
            self.queue.push_back(work);
            return None;
        }
        Some(work)
    }

    /// The next job for a thread that is free.
    pub(super) fn take(&mut self) -> Option<W> {
        self.queue.pop_front()
    }

    /// A free thread found no job: park it, or — while the pool drains —
    /// retire it instead. Returns `true` when the thread leaves the pool.
    pub(super) const fn park_or_retire(&mut self) -> bool {
        if self.draining {
            self.live = self.live.saturating_sub(1);
            return true;
        }
        self.idle = self.idle.saturating_add(1);
        false
    }

    /// A parked thread holds the lock again, its wait having `timed_out` or
    /// not.
    pub(super) const fn woken(&mut self, timed_out: bool) -> Woken {
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
    pub(super) fn drain_wake(&mut self) -> Woken {
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
    pub(super) const fn begin_drain(&mut self) {
        self.draining = true;
    }

    /// Join every thread that has already returned.
    ///
    /// A thread that exits on its keep-alive is still held by its handle until
    /// something joins it, so a start is where the exits of earlier bursts are
    /// released — the property the list's doc rests on. `is_finished` never
    /// blocks and a returned thread's `join` returns at once, so this is a reap
    /// and not a wait: a thread still in its last instructions keeps its handle
    /// and is reaped by a later start or by a shutdown.
    pub(super) fn reap_finished_threads(&mut self) {
        let mut kept = Vec::with_capacity(self.handles.len());
        let mut reaped = 0usize;
        for handle in std::mem::take(&mut self.handles) {
            if handle.is_finished() {
                join_pool_thread(handle);
                reaped = reaped.saturating_add(1);
            } else {
                kept.push(handle);
            }
        }
        self.handles = kept;
        #[cfg(feature = "trace")]
        if reaped > 0 {
            crate::trace::debug!(
                reaped,
                live = self.live,
                "submit: joined the pool threads that had already left"
            );
        }
        #[cfg(not(feature = "trace"))]
        let _ = reaped;
    }
}

/// The process-wide pool. [`configure_blocking_pool`] and first use race to
/// initialize it; whoever wins fixes the ceiling, and the `OnceLock` is the
/// whole of that arbitration.
pub(super) static POOL: OnceLock<Arc<Pool>> = OnceLock::new();

/// The pool, built on first use at the default ceiling.
///
/// A reference, not a clone: the pool outlives every caller by construction,
/// and the one place a thread needs its own reference — the `start` call
/// inside [`Pool::submit`] — clones from the `&Arc` it already holds.
pub(super) fn pool() -> &'static Arc<Pool> {
    POOL.get_or_init(|| Arc::new(Pool::new(MAX_BLOCKING_THREADS, start_os_thread)))
}

/// The body every pool thread runs: its handed-off first job, then the pool
/// loop until the thread is told to leave.
///
/// Shared so a starter that adds an epilogue — the seeded lifetime family's,
/// which holds a thread in the window between leaving the accounting and
/// being marked finished — composes this instead of repeating the sequence and
/// drifting from it.
pub(super) fn serve_pool(pool: &Pool, first: Handoff) {
    let work = lock(&first).take();
    drop(first);
    if let Some(work) = work {
        work();
    }
    pool.run();
}

/// Start one named OS thread serving `pool`, and hand back its join handle.
///
/// The thread owns its reference to the pool and runs the job in `first`
/// before it ever takes the lock. The handle is registered in the pool's state
/// under the same lock that counted the start, so a shutdown can never observe
/// a thread it cannot join.
pub(super) fn start_os_thread(pool: Arc<Pool>, first: Handoff) -> io::Result<ThreadHandle> {
    thread::Builder::new()
        .name("lgwks-blocking".into())
        .spawn(move || serve_pool(&pool, first))
}

/// A new thread's first job. Shared rather than moved into the thread's
/// closure because a thread that fails to start drops its closure, and the
/// job must come back to be queued or refused rather than vanish with it.
pub(super) type Handoff = Arc<Mutex<Option<Work>>>;

/// Join one pool thread, reporting a panic rather than resuming it.
///
/// A pool thread cannot panic through its jobs — `prepare` catches each job's
/// panic — so an unwind here is a defect outside any job. It is reported as a
/// trace event; the thread still counts as finished, because `join`
/// returning is the fact a shutdown waits on.
pub(super) fn join_pool_thread(handle: ThreadHandle) {
    if let Err(_payload) = handle.join() {
        #[cfg(feature = "trace")]
        crate::trace::debug!("shutdown: a pool thread exited on a panic outside any job");
    }
}

impl Pool {
    /// An empty pool of at most `ceiling` threads, started by `start`, built
    /// by first use rather than by [`configure_blocking_pool`]. Its threads
    /// leave after [`BLOCKING_KEEP_ALIVE`].
    pub(super) const fn new(
        ceiling: usize,
        start: fn(Arc<Self>, Handoff) -> io::Result<ThreadHandle>,
    ) -> Self {
        Self::tuned(ceiling, start, BLOCKING_KEEP_ALIVE)
    }

    /// [`Pool::new`] with the keep-alive the caller asks for. Private because
    /// the production pool's keep-alive is [`BLOCKING_KEEP_ALIVE`] and a
    /// ceiling is the only thing a caller may choose; the seeded lifetime
    /// family uses this to reach the idle-exit path in milliseconds.
    pub(super) const fn tuned(
        ceiling: usize,
        start: fn(Arc<Self>, Handoff) -> io::Result<ThreadHandle>,
        keep_alive: Duration,
    ) -> Self {
        Self {
            state: Mutex::new(PoolState::new()),
            work_ready: Condvar::new(),
            ceiling,
            configured: None,
            keep_alive,
            start,
        }
    }

    /// An empty pool whose ceiling a [`configure_blocking_pool`] call fixed.
    pub(super) const fn at_configured(
        ceiling: usize,
        start: fn(Arc<Self>, Handoff) -> io::Result<ThreadHandle>,
    ) -> Self {
        let mut pool = Self::tuned(ceiling, start, BLOCKING_KEEP_ALIVE);
        pool.configured = Some(ceiling);
        pool
    }

    /// Queue `work` and make sure a thread will run it.
    ///
    /// An idle thread is woken if there is one; otherwise a thread is started
    /// while the pool is under its ceiling; otherwise the job waits for the
    /// next thread to finish. `queue_limit` bounds the wait queue.
    pub(super) fn submit(
        self: &Arc<Self>,
        work: Work,
        queue_limit: Option<usize>,
    ) -> Result<(), SpawnError> {
        let mut state = lock(&self.state);
        let admitted = match state.admit(work, queue_limit, self.ceiling) {
            Ok(admitted) => admitted,
            Err(Refused::AtCapacity(_refused)) => {
                // `admit` refuses at capacity only for a named queue limit, so
                // that limit is the bound this refusal reports. If a caller ever
                // reaches here without one, the observed queue depth is the
                // nearest queue fact the refusal can still name.
                let queued = match queue_limit {
                    Some(limit) => limit,
                    None => state.queue.len(),
                };
                let refusal = Err(SpawnError::AtCapacity {
                    threads: self.ceiling,
                    queued,
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
                        // The reap comes before the count, not after it: the
                        // exited threads of earlier bursts are released here,
                        // while the new one is still unheld, which is what
                        // keeps the list within the ceiling.
                        state.reap_finished_threads();
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
    pub(super) fn run(&self) {
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
                let (guard, waited) = wait_timeout(&self.work_ready, state, self.keep_alive);
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
    pub(super) fn shutdown(self: &Arc<Self>, within: Duration) -> PoolShutdown {
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
                None => self.keep_alive,
                Some(until) => until.saturating_duration_since(now),
            };
            let (guard, _) = wait_timeout(&self.work_ready, state, wait);
            state = guard;
        }
    }
}
