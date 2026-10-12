/// Owns a bounded set of background tasks and stops them when it goes away.
///
/// See the [module docs](self) for the four mechanisms that keep a task from
/// leaking, the one that keeps a loop from running away, and the terminal
/// outcome every task reports.
pub struct Supervisor {
    /// Parents every task's token. Cancelling it stops the lot; a task holding
    /// a child token does not keep this supervisor alive, because the link runs
    /// child-to-parent only.
    token: CancellationToken,
    /// Live tasks, tracked so `Drop` can abort them. Never detached.
    set: JoinSet<TaskEnd>,
    /// The in-flight ceiling. Shared with the tasks, each of which holds one
    /// permit for its lifetime and releases it on completion or abort.
    permits: Arc<Semaphore>,
    /// Process cleanup obligations transferred from terminal or aborted tasks.
    #[cfg(all(unix, feature = "process"))]
    cleanup_owners: Arc<CleanupOwners>,
    /// Process tasks that have not yet finished their job cleanup: the
    /// Windows counterpart of the Unix cleanup registry's live count, which is what
    /// lets [`Supervisor::shutdown`] wait for a job to settle. A job object
    /// kills its tree when closed, so no retained obligation exists beside it.
    #[cfg(all(windows, feature = "process"))]
    live_jobs: Arc<std::sync::atomic::AtomicUsize>,
    /// Maps the engine's task id to this module's, for the one path that cannot
    /// carry a [`TaskId`] in the value: a terminal `JoinError`, which is the
    /// error case and therefore has no value to carry it. Entries are removed
    /// as tasks are joined, so it is bounded by the in-flight ceiling.
    identities: BTreeMap<Id, TaskId>,
    /// Terminal outcomes not yet drained by the caller. Capped at the in-flight
    /// ceiling; see [`Stats::reports_dropped`].
    reports: VecDeque<TaskOutcome>,
    /// The retention cap for [`Self::reports`], and the in-flight ceiling this
    /// supervisor was built with.
    report_cap: usize,
    /// The in-flight ceiling as the caller declared it, retained so
    /// [`Supervisor::snapshot`] can report the bound the caller asked for rather
    /// than re-deriving it from the clamped permit pool.
    max_in_flight: usize,
    /// Next [`TaskId`] to hand out. Saturating, like the counters.
    next_task: u64,
    /// Tasks started. Saturating.
    spawned: u64,
    /// Tasks finished, however they ended. Saturating.
    completed: u64,
    /// Tasks that ran to completion. Saturating.
    succeeded: u64,
    /// Supervised processes that did not exit successfully. Saturating.
    failed: u64,
    /// Tasks that observed their token and returned. Saturating.
    cancelled: u64,
    /// Tasks dropped by the runtime before finishing. Saturating.
    aborted: u64,
    /// Tasks whose body panicked. Saturating.
    panicked: u64,
    /// Spawn attempts refused at the bound. Saturating.
    refused: u64,
    /// Terminal outcomes refused by the retention cap. Saturating.
    reports_dropped: u64,
    /// The clock that governs this supervisor's deadlines.
    ///
    /// Read by the budget loop and the process-deadline loop, and reported by
    /// [`Supervisor::snapshot`], so a reader can ask *which* clock a deadline was
    /// measured on. A wall clock by default, because production must not have to
    /// drive anything; a virtual one makes every budget in this supervisor exact
    /// and instant.
    clock: Clock,
    /// Per-tenant capacity, when the caller installed a [`TenancyPolicy`].
    ///
    /// `None` is the default and the exact behaviour this module always had: one
    /// pool, one ceiling, every caller sharing it. `Some` means admission runs
    /// through the shell, which charges each permit to a named tenant, bounds
    /// every tenant's in-flight share, and shares freed permits by deficit round
    /// robin. Both the untenanted and the tenant-scoped calls take the same path
    /// when it is `Some` — the untenanted ones as one implicit tenant — so there
    /// is no second admission implementation to drift.
    #[cfg(feature = "script")]
    tenancy: Option<Arc<tenancy_support::TenancyShell>>,
}

/// What a supervised body reported when it returned.
///
/// The supervisor's wrapper decides which of the two a body's normal return
/// means, because only the wrapper knows whether the body was built to observe
/// its token. A `JoinError` is the third and fourth case, and is not a `TaskEnd`
/// at all.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TaskEnd {
    /// The body ran to the end of its own work.
    Completed,
    /// A supervised process exited and its cleanup was checked.
    #[cfg(feature = "process")]
    CompletedWithCleanup {
        /// The receipt and the containment report, as one owned fact.
        cleanup: ProcessCleanup,
    },
    /// The body stopped because its token was cancelled.
    Cancelled,
    /// A supervised process was cancelled and its cleanup was checked.
    #[cfg(feature = "process")]
    CancelledWithCleanup {
        /// The receipt and the containment report, as one owned fact.
        cleanup: ProcessCleanup,
    },
    /// A supervised process ended without exiting successfully.
    ///
    /// Distinct from [`TaskEnd::Completed`] because the body cannot express it:
    /// a process body returns after its child exits, whether that exit was
    /// clean or not, so the exit status is the only thing that tells the two
    /// apart. `status` is `None` when the engine could not report one.
    ///
    /// Gated with the feature that can produce it: [`Supervisor::spawn_process`]
    /// is the only constructor, so a build without `process` has no value of
    /// this shape to describe. The public [`TaskOutcome::Failed`] it is absorbed
    /// into is unconditional, because a `#[non_exhaustive]` public enum may
    /// carry a variant a given build cannot produce.
    #[cfg(feature = "process")]
    Failed {
        /// The status the engine reported, if it reported one.
        status: Option<ExitStatus>,
        /// The receipt and the containment report, as one owned fact.
        cleanup: ProcessCleanup,
    },
}

/// What ended one wait at the bound in [`Supervisor::claim`].
#[derive(Debug)]
enum Wait {
    /// A task ended, so its permit is back in the pool; its outcome is carried
    /// here so it is counted, not dropped.
    Joined(Result<(Id, TaskEnd), super::task::JoinError>),
    /// A permit was taken; cancellation is still read on it before admission.
    Permit(OwnedSemaphorePermit),
    /// The pool was closed. This supervisor never closes it, so this is counted
    /// as a refusal rather than assumed away.
    Closed,
    /// The supervisor was cancelled while waiting.
    Cancelled,
    /// A permit may be held by a cleanup owner that only a reap releases.
    Recheck,
}

/// One supervised process's cleanup: the receipt and what it was based on.
///
/// A pair rather than two parameters because they are one fact: a receipt with no
/// containment report cannot say which mechanism produced it, and a containment
/// report with no receipt is a count of processes nobody stopped. Every producer
/// builds both, so neither can travel alone.
#[cfg(feature = "process")]
#[derive(Clone, Debug, PartialEq, Eq)]
struct ProcessCleanup {
    /// The terminal receipt.
    receipt: CleanupReceipt,
    /// What the cleanup read, signalled and could not account for.
    containment: Containment,
}

#[cfg(feature = "process")]
impl ProcessCleanup {
    /// Split the pair into the two public shapes a report carries.
    fn into_parts(self) -> (CleanupReceipt, Containment) {
        (self.receipt, self.containment)
    }
}

/// Whether a terminal outcome is subject to the retention cap.
///
/// [`Supervisor::reap`] runs on the caller's schedule with the caller free to
/// spawn afterwards, so its reports are capped. [`Supervisor::shutdown`] is
/// draining to completion, and the number of outcomes it can still produce is
/// bounded by the in-flight ceiling that is already in force, so capping there
/// could only lose evidence the caller is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Retention {
    /// Retain it only if the buffer has room; otherwise count it as dropped.
    Capped,
    /// Retain it: the caller is draining and the total is already bounded.
    Draining,
}

/// Longest panic message a [`TaskOutcome::Panicked`] retains.
///
/// The report buffer holds at most the in-flight bound of outcomes, so an
/// uncapped payload would make this module's memory bound a product of two
/// things the caller chooses. Truncation keeps the head of the message, which
/// is where a panic payload puts its point, and keeps the bound a sum.
const MAX_PANIC_MESSAGE_CHARS: usize = 512;

/// Wall-clock grace [`Supervisor::shutdown`] gives cooperative bodies.
///
/// A supervisor cannot wait for a body that never observes its token, so the
/// wait has to be finite. The unit is time, not yields, and that is the whole
/// point of the constant: `yield_now` reschedules *the yielding task*, so on a
/// current-thread runtime it lets another task run immediately, while on a
/// multi-threaded one it does nothing at all for a body parked on a different
/// worker — that body's return needs a thread wakeup, which is an OS-level
/// event no number of local reschedules waits for. A counted grace is therefore
/// long enough on one worker and far too short on several, and the failure is
/// silent: a cancelled body is reported as an *aborted* one, which is the exact
/// distinction [`Supervisor::shutdown`]'s return value exists to make.
///
/// This bounds the wait; it is not a cost. A body that returns is absorbed the
/// moment it does, so a cooperative body pays its own wakeup latency and
/// nothing more. Only a body that ignores its token spends the whole grace, and
/// it spends it on the shutdown path — the terminal one, where a delay is
/// cheaper than a misreported outcome.
const COOPERATIVE_DRAIN_GRACE: Duration = Duration::from_millis(50);

/// How many finished tasks one tenanted admission joins before it is placed.
///
/// Two, because every admission adds one task: joining up to two per call
/// drains a backlog rather than growing it, so the finished-but-unjoined set
/// stays proportional to the in-flight ceiling — only a task that was in flight
/// between two calls can finish between them. An unbounded reap made a call's
/// bookkeeping every task *any* tenant finished since the last call, so a tenant
/// whose bodies end at once charged its own joins to whichever neighbour
/// admitted next (#268's adversarial row). [`Supervisor::reap`],
/// [`Supervisor::wait_idle`] and shutdown still join everything.
#[cfg(feature = "script")]
const REAP_PER_ADMISSION: usize = 2;

/// How long [`Supervisor::shutdown`] waits, past [`COOPERATIVE_DRAIN_GRACE`],
/// for the next supervised process task to finish its cleanup.
///
/// A process task is cooperative by construction — its wait races the token —
/// but answering the token *is* its cleanup: up to [`CONTAINMENT_ROUNDS`]
/// captures of the process table, each a `ps` snapshot off Linux, then the reap
/// and the post-reap observation. One snapshot measured p50 16 ms, p99 45 ms and
/// max 68 ms over 200 reads on a host at load 9.6, so a whole drain is a few
/// hundred milliseconds under load and the 50 ms grace aborted it part-way:
/// `process_escape` then saw an `Aborted` outcome with no receipt for a task that
/// had already stopped its tree (10 of 1,000 iterations at load 15). Two
/// seconds is about six times the worst drain that measurement allows. It bounds
/// the wait rather than charging it: a drain that finishes is absorbed at once,
/// and only a process task still live when this runs out is aborted, its
/// drop-time fallback still signalling the group.
///
/// The grace runs from the last drain that finished, not from a fixed point.
/// Drains contend for one host, so thirty-two of them on a host also carrying
/// thirteen CI lanes and two builds outlasted a fixed two seconds while every
/// one was still finishing in turn, and shutdown aborted fifteen cooperating
/// drains part-way (main run 37933518033). Waiting while drains keep finishing,
/// and giving up only after two seconds in which none did, aborts a stalled
/// drain exactly as before and never a progressing one. The wait stays
/// bounded: shutdown owns the supervisor, so no task is spawned while it waits
/// and the live count only reaches a new low at most once per process task,
/// which caps the whole wait at the cooperative grace plus this grace once per
/// live process task.
///
/// Four seconds, because a quiet window has to outlast the longest wait one
/// drain makes on its own bounds, or a drain that is still inside a bound it
/// was given is aborted as stalled. On Linux that wait is the adoption reap,
/// [`ADOPTION_REAP_PASSES`] × [`ADOPTION_REAP_INTERVAL`] (two seconds), then
/// the confirming sweep: with the grace equal to the reap's bound alone, an
/// adoptee that died on the bound's last pass lost its drain to the abort.
#[cfg(all(any(unix, windows), feature = "process"))]
const PROCESS_CLEANUP_GRACE: Duration = Duration::from_secs(4);

/// Bounded capture-and-signal rounds one cleanup performs.
///
/// This is the bound the descendant capture replaced. A group signal reaches the
/// members that exist when it is sent, so a fork in flight can miss it; the old
/// answer was to repeat the signal 64 times and hope a scheduler had finished the
/// fork. The capture is the real answer — it names every process descended from
/// the leader, including one that has left the group — so what is left to bound is
/// how many times the cleanup re-reads the tree and re-signals what is still
/// running. Four rounds is the measured bound on the window this needs: the fork
/// race is a single `fork(2)` returning, and each round's re-capture admits the
/// forks the previous round's signals let through. Where no capture is available
/// the residual is recorded as [`ResidualRisk::TableUnreadable`] rather than
/// absorbed into a longer retry.
#[cfg(all(unix, feature = "process"))]
const CONTAINMENT_ROUNDS: usize = 4;

/// Group signals delivered on the drop-time path, while the unreaped leader still
/// pins the group id.
///
/// Bounded for the same reason the drain is: it covers the one window a capture
/// cannot — a member the leader forks after the capture and before it dies — and
/// the leader's death ends that window. It is a count of signals rather than a
/// sleep, so it costs nothing on the paths that stop early.
#[cfg(all(unix, feature = "process"))]
const GROUP_SIGNAL_ATTEMPTS: usize = 4;

/// What one group signal delivered and what the OS then said.
#[cfg(all(unix, feature = "process"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GroupSignal {
    /// Delivered; the group is present and its members were sent SIGKILL.
    Delivered,
    /// No group holds the id: the group is gone.
    Absent,
    /// The group is present but cannot be signalled again — an unreaped zombie
    /// leader, which macOS and the BSDs answer with `EPERM` (INV-BOT-19).
    Present,
    /// The operating system refused the termination.
    Refused,
}

/// Whether `error` is the OS's "no such process".
///
/// The errno and not the [`io::ErrorKind`]: macOS classifies `ESRCH` as
/// uncategorised where Linux calls it `NotFound`, so the kind is a property of the
/// platform and the errno is the fact both share.
#[cfg(all(unix, feature = "process"))]
fn is_absence(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(ESRCH)
}

/// `ESRCH`, "no such process", on every Unix this crate targets.
#[cfg(all(unix, feature = "process"))]
const ESRCH: i32 = 3;

/// `EPERM`, "operation not permitted", on every Unix this crate targets.
#[cfg(all(unix, feature = "process"))]
const EPERM: i32 = 1;

/// Whether `error` says the group is present but unsignalable, rather than that
/// the kill was refused. See [`GroupSignal::Present`].
#[cfg(all(unix, feature = "process"))]
fn is_present_but_unsignalable(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::PermissionDenied || error.raw_os_error() == Some(EPERM)
}

/// How often a spawn waiting at the bound re-drives the cleanup owners while one
/// of them holds a permit.
///
/// A cleanup owner keeps its task's permit until it observes the group gone, and
/// nothing wakes a waiter when that happens: the observation is a probe that
/// [`Supervisor::reap`] makes. So a waiter re-reaps on this interval, and only
/// while an owner is pending — with none, every permit is released by a task
/// ending, which wakes the waiter itself, and the wait arms no timer.
#[cfg(all(unix, feature = "process"))]
const CLEANUP_RECHECK: Duration = Duration::from_millis(100);

/// The in-flight ceiling [`Supervisor::default`] falls back to when the OS will
/// not report a usable processor count.
///
/// Four, not one: a single-slot supervisor is a serial executor wearing a
/// supervisor's name, and a bot whose background work is a heartbeat plus a
/// watcher would deadlock itself against its own bound. Not a large constant:
/// the bound is a resource ceiling, and a discovered count is always preferred
/// to this. It is reached only when
/// [`std::thread::available_parallelism`] itself fails.
const FALLBACK_MAX_IN_FLIGHT: usize = 4;

impl Default for Supervisor {
    /// A supervisor bounded by the machine it is on.
    ///
    /// The ceiling is discovered rather than required, because the caller who
    /// has no opinion is the common case and the one this constructor exists
    /// for: a bot that wants supervision — bounded, cancellable, reported —
    /// should not have to invent a number to get it. The number is
    /// [`std::thread::available_parallelism`], the same discovery
    /// [`Builder`](crate::rt::runtime::Builder) uses for its worker count, and
    /// `FALLBACK_MAX_IN_FLIGHT` only if the OS refuses to report one. It is
    /// still a real ceiling: there is no default that is unbounded.
    ///
    /// A caller who wants to choose uses [`Supervisor::new`].
    fn default() -> Self {
        let bound =
            std::thread::available_parallelism().map_or(FALLBACK_MAX_IN_FLIGHT, NonZeroUsize::get);
        Self::new(bound)
    }
}

