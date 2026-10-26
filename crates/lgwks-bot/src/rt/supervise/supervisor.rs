//! The supervisor itself: its state, its constructors and its task doors.

#[cfg(all(any(unix, windows), feature = "process"))]
use super::ended::RunBounds;
#[cfg(feature = "process")]
use super::outcome::CleanupReceipt;
#[cfg(feature = "process")]
use super::outcome::Containment;
use super::outcome::TaskOutcome;
#[cfg(all(unix, feature = "process"))]
use super::owners::CleanupOwners;
use super::repeat::repeat_on;
use super::report::{LiveTask, Stats, SupervisorSnapshot, TaskState};
#[cfg(all(target_os = "linux", feature = "process"))]
use super::start::sweep_adopted;
#[cfg(feature = "script")]
use super::tenancy_support;
use super::{Budget, FALLBACK_MAX_IN_FLIGHT, Outcome, TaskId, TrySpawnRefusal};
use crate::rt::cancel::CancellationToken;
use crate::rt::clock::Clock;
#[cfg(feature = "process")]
use crate::rt::process::ProcessSpec;
use crate::rt::sync::{OwnedSemaphorePermit, Semaphore};
use crate::rt::task::JoinSet;
#[cfg(feature = "script")]
use crate::rt::tenancy::{SpawnRefused, TenancyPolicy};
#[cfg(feature = "script")]
use crate::script::Tenant;
use lgwks_deps::tokio::task::Id;
use std::collections::{BTreeMap, VecDeque};
#[cfg(feature = "process")]
use std::process::ExitStatus;
use std::sync::Arc;
#[cfg(all(windows, feature = "process"))]
use std::sync::atomic::AtomicUsize;

/// Owns a bounded set of background tasks and stops them when it goes away.
///
/// See the [module docs](crate::rt::supervise) for the four mechanisms that keep a task from
/// leaking, the one that keeps a loop from running away, and the terminal
/// outcome every task reports.
pub struct Supervisor {
    /// Parents every task's token. Cancelling it stops the lot; a task holding
    /// a child token does not keep this supervisor alive, because the link runs
    /// child-to-parent only.
    pub(super) token: CancellationToken,
    /// Live tasks, tracked so `Drop` can abort them. Never detached.
    pub(super) set: JoinSet<TaskEnd>,
    /// The in-flight ceiling. Shared with the tasks, each of which holds one
    /// permit for its lifetime and releases it on completion or abort.
    pub(super) permits: Arc<Semaphore>,
    /// Process cleanup obligations transferred from terminal or aborted tasks.
    #[cfg(all(unix, feature = "process"))]
    pub(super) cleanup_owners: Arc<CleanupOwners>,
    /// Process tasks that have not yet settled their job object. Shutdown
    /// reads it to tell a job still settling from a body that ignores its
    /// token.
    #[cfg(all(windows, feature = "process"))]
    pub(super) live_jobs: Arc<AtomicUsize>,
    /// Maps the engine's task id to this module's, for the one path that cannot
    /// carry a [`TaskId`] in the value: a terminal `JoinError`, which is the
    /// error case and therefore has no value to carry it. Entries are removed
    /// as tasks are joined, so it is bounded by the in-flight ceiling.
    pub(super) identities: BTreeMap<Id, TaskId>,
    /// Terminal outcomes not yet drained by the caller. Capped at the in-flight
    /// ceiling; see [`Stats::reports_dropped`](crate::rt::supervise::Stats::reports_dropped).
    pub(super) reports: VecDeque<TaskOutcome>,
    /// The retention cap for [`Self::reports`], and the in-flight ceiling this
    /// supervisor was built with.
    pub(super) report_cap: usize,
    /// The in-flight ceiling as the caller declared it, retained so
    /// [`Supervisor::snapshot`](crate::rt::supervise::Supervisor::snapshot) can report the bound the caller asked for rather
    /// than re-deriving it from the clamped permit pool.
    pub(super) max_in_flight: usize,
    /// Next [`TaskId`] to hand out. Saturating, like the counters.
    pub(super) next_task: u64,
    /// Tasks started. Saturating.
    pub(super) spawned: u64,
    /// Tasks finished, however they ended. Saturating.
    pub(super) completed: u64,
    /// Tasks that ran to completion. Saturating.
    pub(super) succeeded: u64,
    /// Supervised processes that did not exit successfully. Saturating.
    pub(super) failed: u64,
    /// Tasks that observed their token and returned. Saturating.
    pub(super) cancelled: u64,
    /// Tasks dropped by the runtime before finishing. Saturating.
    pub(super) aborted: u64,
    /// Tasks whose body panicked. Saturating.
    pub(super) panicked: u64,
    /// Spawn attempts refused at the bound. Saturating.
    pub(super) refused: u64,
    /// Terminal outcomes refused by the retention cap. Saturating.
    pub(super) reports_dropped: u64,
    /// The clock that governs this supervisor's deadlines.
    ///
    /// Read by the budget loop and the process-deadline loop, and reported by
    /// [`Supervisor::snapshot`](crate::rt::supervise::Supervisor::snapshot), so a reader can ask *which* clock a deadline was
    /// measured on. A wall clock by default, because production must not have to
    /// drive anything; a virtual one makes every budget in this supervisor exact
    /// and instant.
    pub(super) clock: Clock,
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
    pub(super) tenancy: Option<Arc<tenancy_support::TenancyShell>>,
}

/// What a supervised body reported when it returned.
///
/// The supervisor's wrapper decides which of the two a body's normal return
/// means, because only the wrapper knows whether the body was built to observe
/// its token. A `JoinError` is the third and fourth case, and is not a `TaskEnd`
/// at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TaskEnd {
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
    /// Gated with the feature that can produce it: [`Supervisor::spawn_process`](crate::rt::supervise::Supervisor::spawn_process)
    /// is the only constructor, so a build without `process` has no value of
    /// this shape to describe. The public [`TaskOutcome::Failed`](crate::rt::supervise::TaskOutcome::Failed) it is absorbed
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

/// What ended one wait at the bound in [`Supervisor::claim`](crate::rt::supervise::Supervisor::claim).
#[derive(Debug)]
pub(super) enum Wait {
    /// A task ended, so its permit is back in the pool; its outcome is carried
    /// here so it is counted, not dropped.
    Joined(Result<(Id, TaskEnd), crate::rt::task::JoinError>),
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
pub(super) struct ProcessCleanup {
    /// The terminal receipt.
    pub(super) receipt: CleanupReceipt,
    /// What the cleanup read, signalled and could not account for.
    pub(super) containment: Containment,
}

#[cfg(feature = "process")]
impl ProcessCleanup {
    /// Split the pair into the two public shapes a report carries.
    pub(super) fn into_parts(self) -> (CleanupReceipt, Containment) {
        (self.receipt, self.containment)
    }
}

/// Whether a terminal outcome is subject to the retention cap.
///
/// [`Supervisor::reap`](crate::rt::supervise::Supervisor::reap) runs on the caller's schedule with the caller free to
/// spawn afterwards, so its reports are capped. [`Supervisor::shutdown`](crate::rt::supervise::Supervisor::shutdown) is
/// draining to completion, and the number of outcomes it can still produce is
/// bounded by the in-flight ceiling that is already in force, so capping there
/// could only lose evidence the caller is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Retention {
    /// Retain it only if the buffer has room; otherwise count it as dropped.
    Capped,
    /// Retain it: the caller is draining and the total is already bounded.
    Draining,
}

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
    /// A caller who wants to choose uses [`Supervisor::new`](crate::rt::supervise::Supervisor::new).
    fn default() -> Self {
        match std::thread::available_parallelism() {
            Ok(bound) => Self::new(bound.get()),
            Err(error) => {
                lgwks_std::trace::debug!(%error, "supervisor: no processor count; using the fallback ceiling");
                Self::new(FALLBACK_MAX_IN_FLIGHT)
            }
        }
    }
}

impl Supervisor {
    /// Create a supervisor that runs at most `max_in_flight` tasks at once.
    ///
    /// A `max_in_flight` of zero is treated as one, and one above
    /// [`Semaphore::MAX_PERMITS`] is clamped to it, so there is no argument
    /// that produces an unbounded supervisor.
    #[must_use]
    pub fn new(max_in_flight: usize) -> Self {
        Self::with_clock(max_in_flight, Clock::wall())
    }

    /// Create a supervisor whose deadlines are governed by `clock`.
    ///
    /// The same supervisor as [`Supervisor::new`](crate::rt::supervise::Supervisor::new) with one difference: the clock
    /// its budget checks and its process deadlines read. A wall clock is the
    /// default, so nothing in production has to drive anything; a
    /// caller-advanceable one makes every budget in this supervisor exact, so a
    /// test can exhaust a five-minute budget in one call with no real wait and
    /// observe the *same* refusal a real overrun produces.
    ///
    /// Clones share the clock rather than copying it, so one advance from a test
    /// reaches every task the supervisor started.
    #[must_use]
    pub fn with_clock(max_in_flight: usize, clock: Clock) -> Self {
        let bound = max_in_flight.clamp(1, Semaphore::MAX_PERMITS);
        Self::assembled(bound, Arc::new(Semaphore::new(bound)), clock)
    }

    /// Create a supervisor that runs at most `max_in_flight` tasks at once
    /// **and** isolates each tenant's share of them under `policy`.
    ///
    /// The additive 1.x form of [`Supervisor::new`](crate::rt::supervise::Supervisor::new): the same supervisor, the
    /// same report stream, the same terminal outcomes, plus the per-tenant
    /// ceiling, queue bound and weights a [`TenancyPolicy`] declares.
    ///
    /// # What changes for a caller that installs one
    ///
    /// - **The untenanted calls are one implicit tenant.**
    ///   [`Supervisor::spawn`](crate::rt::supervise::Supervisor::spawn), [`Supervisor::try_spawn`](crate::rt::supervise::Supervisor::try_spawn),
    ///   [`Supervisor::spawn_repeating`](crate::rt::supervise::Supervisor::spawn_repeating), `Supervisor::spawn_process` and
    ///   `Supervisor::run_process` are charged to one reserved tenant, so a
    ///   caller that predates tenancy shares that tenant's ceiling with every
    ///   other untenanted caller instead of reaching every tenant's permit. No
    ///   signature changes, and the one behavioural difference is a *visible*
    ///   refusal rather than an unbounded wait: a full implicit-tenant queue
    ///   refuses the call and counts it in [`Stats::refused`](crate::rt::supervise::Stats::refused), where an
    ///   untenanted supervisor without tenancy still waits.
    /// - **The tenant-scoped calls answer with a typed refusal.**
    ///   [`Supervisor::spawn_for`](crate::rt::supervise::Supervisor::spawn_for) and `Supervisor::run_process_for` name the
    ///   tenant that is at capacity and the bound it reached, so a caller can
    ///   tell one loud tenant from a stopped supervisor.
    ///
    /// A supervisor built without a policy takes the path it always took: the
    /// same semaphore, the same waits, the same refusals. That is what makes this
    /// additive rather than a behaviour change for existing callers.
    ///
    /// `a_tenant_scoped_spawn_runs_under_a_per_tenant_ceiling` (`tests/it/rt_front_door.rs`) runs it:
    /// 64 permits in total and 32 per tenant, so one tenant strands at most 32 of them and 32 stay
    /// reachable for everyone else however loud it is; the quiet tenant's task runs and the shutdown
    /// report accounts for exactly that one task, with no panic.
    #[cfg(feature = "script")]
    #[must_use]
    pub fn with_tenancy(max_in_flight: usize, policy: TenancyPolicy) -> Self {
        let bound = max_in_flight.clamp(1, Semaphore::MAX_PERMITS);
        let permits = Arc::new(Semaphore::new(bound));
        let mut supervisor = Self::assembled(bound, Arc::clone(&permits), Clock::wall());
        supervisor.tenancy = Some(Arc::new(tenancy_support::TenancyShell::new(
            permits, policy,
        )));
        supervisor
    }

    /// The one constructor, so a supervisor differs only by what its admission
    /// needs.
    ///
    /// A private constructor rather than two struct literals: a second spelling
    /// of the counters would be a second place for one of them to be forgotten,
    /// and every report a caller reads is built from these fields.
    pub(super) fn assembled(bound: usize, permits: Arc<Semaphore>, clock: Clock) -> Self {
        Self {
            token: CancellationToken::new(),
            set: JoinSet::new(),
            permits,
            #[cfg(all(unix, feature = "process"))]
            cleanup_owners: Arc::new(CleanupOwners::default()),
            #[cfg(all(windows, feature = "process"))]
            live_jobs: Arc::new(AtomicUsize::new(0)),
            identities: BTreeMap::new(),
            reports: VecDeque::new(),
            report_cap: bound,
            max_in_flight: bound,
            next_task: 0,
            spawned: 0,
            completed: 0,
            succeeded: 0,
            failed: 0,
            cancelled: 0,
            aborted: 0,
            panicked: 0,
            refused: 0,
            reports_dropped: 0,
            clock,
            #[cfg(feature = "script")]
            tenancy: None,
        }
    }

    /// The clock that governs this supervisor's deadlines.
    #[must_use]
    pub fn clock(&self) -> &Clock {
        &self.clock
    }

    /// A token that is cancelled when this supervisor is cancelled or dropped.
    ///
    /// This is the token a body should hold. It is a *child* of the
    /// supervisor's, so cancelling it (or a supervisor shutting down) stops
    /// the body, while the body stopping does not stop the supervisor.
    #[must_use]
    pub fn child_token(&self) -> CancellationToken {
        self.token.child_token()
    }

    /// The child token and the run bounds a spec declares, resolved once.
    ///
    /// Both process runners start from these values, and a second copy is a
    /// second place for the ceiling, the deadline or the token to drift.
    #[cfg(all(any(unix, windows), feature = "process"))]
    pub(super) fn prepare_process(&self, spec: &ProcessSpec) -> (CancellationToken, RunBounds) {
        (
            self.child_token(),
            RunBounds {
                deadline: spec.deadline_duration(),
                out_limit: spec.stdout_capture(),
                err_limit: spec.stderr_capture(),
            },
        )
    }

    /// Whether this supervisor has been cancelled or is shutting down.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Cancel every task this supervisor owns, without waiting for them.
    ///
    /// The asynchronous counterpart that also waits, and returns what it
    /// drained, is [`Supervisor::shutdown`](crate::rt::supervise::Supervisor::shutdown).
    pub fn cancel(&self) {
        self.token.cancel();
    }

    /// A snapshot of the counters.
    #[must_use]
    pub fn stats(&self) -> Stats {
        Stats {
            spawned: self.spawned,
            completed: self.completed,
            succeeded: self.succeeded,
            failed: self.failed,
            cancelled: self.cancelled,
            aborted: self.aborted,
            panicked: self.panicked,
            refused: self.refused,
            reports_dropped: self.reports_dropped,
        }
    }

    /// Number of retained Unix process-group cleanup obligations still charged
    /// to this supervisor's in-flight capacity.
    #[cfg(all(unix, feature = "process"))]
    #[must_use]
    pub fn pending_cleanup_count(&self) -> usize {
        self.cleanup_owners.pending_count()
    }

    /// A bounded, point-in-time view of what this supervisor owns right now.
    ///
    /// Reads the supervisor's own admission and reporting fields directly — the
    /// same fields [`Supervisor::stats`](crate::rt::supervise::Supervisor::stats) and the permit pool are built from — so
    /// what it reports and what the supervisor does cannot drift apart. It
    /// allocates a `Vec` proportional to the in-flight ceiling and retains
    /// nothing afterwards.
    ///
    /// # What it deliberately does not carry
    ///
    /// Terminal outcomes stay in the report stream ([`Supervisor::next_report`](crate::rt::supervise::Supervisor::next_report)),
    /// where the retention cap already governs them. A snapshot with its own copy
    /// of them would be a second ledger, and two ledgers disagree.
    ///
    /// `a_fresh_supervisor_snapshot_holds_every_permit` (`tests/it/rt_front_door.rs`) holds it: a
    /// supervisor of two reports a ceiling of two, two free permits and `NextAction::Admit`.
    #[must_use]
    pub fn snapshot(&self) -> SupervisorSnapshot {
        let live_limit = self.max_in_flight;
        let mut live: Vec<LiveTask> = Vec::with_capacity(self.identities.len().min(live_limit));
        let mut live_truncated: usize = 0;
        // Spawn order, from the supervisor's own identity map: the engine's join
        // order is arbitrary, and a listing that reordered itself between two
        // reads could not be asserted against.
        let mut ordered: Vec<TaskId> = self.identities.values().copied().collect();
        ordered.sort_unstable_by_key(|task| task.get());
        for (index, task) in ordered.into_iter().enumerate() {
            if index < live_limit {
                live.push(LiveTask {
                    task,
                    state: TaskState::Running { position: index },
                });
            } else {
                live_truncated = live_truncated.saturating_add(1);
            }
        }
        #[cfg(all(unix, feature = "process"))]
        let pending_cleanups = self.cleanup_owners.pending_count();
        SupervisorSnapshot {
            max_in_flight: self.max_in_flight,
            stats: self.stats(),
            live,
            live_truncated,
            cancelled: self.token.is_cancelled(),
            reports_pending: self.reports.len(),
            #[cfg(all(unix, feature = "process"))]
            pending_cleanups,
        }
    }

    /// How many tasks this supervisor is still tracking.
    ///
    /// The tracked count, not the accounting one:
    /// [`Stats::in_flight`](crate::rt::supervise::Stats::in_flight) reads
    /// `spawned - completed`, which keeps counting a task that ended and
    /// released its permit until the next reap. This reads the identity map
    /// `reap` drains, so it is the live tasks plus the ended-but-unreaped
    /// ones — it can briefly exceed the in-flight bound, exactly as the
    /// counters can, and
    /// [`Supervisor::snapshot`](crate::rt::supervise::Supervisor::snapshot) lists the first bound of
    /// it with the remainder in `live_truncated`. An owner reconciling its
    /// own ledger against this supervisor reads this and
    /// [`task_state`](Self::task_state), not the counters: a task the owner
    /// placed that this no longer tracks, with no report drained for it, is
    /// how a lost report shows itself (#703).
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.identities.len()
    }

    /// What `task` is doing right now, or `None` when this supervisor no
    /// longer tracks it.
    ///
    /// The checkable form of #703: a task the owner placed that is neither
    /// tracked here nor in a drained report was absorbed without attribution
    /// (counted in [`Stats::reports_dropped`](crate::rt::supervise::Stats::reports_dropped)) or was never
    /// placed. Terminal outcomes are not listed; they are in the report
    /// stream ([`Supervisor::next_report`](crate::rt::supervise::Supervisor::next_report)), so a task whose
    /// report is still queued already reads `None` here. The position is the
    /// spawn order among the tracked tasks, the same numbering
    /// [`Supervisor::snapshot`](crate::rt::supervise::Supervisor::snapshot) uses.
    #[must_use]
    pub fn task_state(&self, task: TaskId) -> Option<TaskState> {
        // One pass and no allocation: the spawn-order position is the count of
        // tracked identities issued before this one, so an owner reconciling
        // every task pays O(n) per question rather than a copy and a sort.
        let mut tracked = false;
        let mut position = 0_usize;
        for live in self.identities.values() {
            if *live == task {
                tracked = true;
            } else if live.get() < task.get() {
                position = position.saturating_add(1);
            }
        }
        tracked.then_some(TaskState::Running { position })
    }

    /// The oldest terminal outcome this supervisor has not yet handed over.
    ///
    /// `None` means every task that has ended has been reported and read. A
    /// caller that reads this in a loop holds no history at all, which is how
    /// the cap in [`Stats::reports_dropped`](crate::rt::supervise::Stats::reports_dropped) is kept from ever being reached.
    pub fn next_report(&mut self) -> Option<TaskOutcome> {
        #[cfg(all(unix, feature = "process"))]
        self.refresh_cleanup_owners();
        self.reports.pop_front()
    }

    /// Join every task that has already finished, returning how many were
    /// joined.
    ///
    /// Each entry point reaps before placing its task, which is what keeps the
    /// retained set proportional to the live bound rather than to the total
    /// number of spawns; the tenanted entry joins a bounded number per call
    /// (`REAP_PER_ADMISSION`), which keeps the same bound. It is public because a caller who has stopped spawning
    /// and wants the counters to settle should not have to spawn a task to make
    /// that happen.
    ///
    /// Every joined task produces exactly one [`TaskOutcome`](crate::rt::supervise::TaskOutcome), readable through
    /// [`Supervisor::next_report`](crate::rt::supervise::Supervisor::next_report).
    pub fn reap(&mut self) -> usize {
        self.reap_at_most(usize::MAX)
    }

    /// Join at most `limit` already-finished tasks, returning how many were
    /// joined.
    ///
    /// The bounded form a tenanted admission takes (`REAP_PER_ADMISSION`), so
    /// one call's bookkeeping is a constant rather than every task any tenant
    /// finished since the last call.
    pub(super) fn reap_at_most(&mut self, limit: usize) -> usize {
        let mut reaped: usize = 0;
        while reaped < limit {
            let Some(joined) = self.set.try_join_next_with_id() else {
                break;
            };
            self.absorb(joined, Retention::Capped);
            reaped = reaped.saturating_add(1);
        }
        #[cfg(all(unix, feature = "process"))]
        self.refresh_cleanup_owners();
        reaped
    }

    /// Wait until every task this supervisor has placed has ended, cancelling
    /// none of them, and return how many were joined.
    ///
    /// The awaiting counterpart of [`Supervisor::reap`](crate::rt::supervise::Supervisor::reap): `reap` joins what has
    /// already finished, so a caller that wanted to wait through it had to poll
    /// it on a timer, and the runtime's timer rounds a short sleep up to its
    /// one-millisecond tick — a fixed cost on every wait that the task set's own
    /// completion wakeup does not have (#269). Each task is joined the moment it
    /// ends and absorbed exactly as `reap` absorbs it, so every one produces one
    /// [`TaskOutcome`](crate::rt::supervise::TaskOutcome) under the same retention cap.
    ///
    /// Unlike [`Supervisor::shutdown`](crate::rt::supervise::Supervisor::shutdown) nothing is cancelled and the supervisor
    /// stays usable: this is the wait for "everything I started has finished",
    /// not for "stop". A body that never returns keeps this waiting; bound it
    /// with the runtime's `timeout`, or cancel, when that is possible. The wait
    /// is bounded by the in-flight ceiling, because nothing is spawned while it
    /// runs.
    pub async fn wait_idle(&mut self) -> usize {
        let mut joined: usize = 0;
        while let Some(next) = self.set.join_next_with_id().await {
            self.absorb(next, Retention::Capped);
            joined = joined.saturating_add(1);
        }
        #[cfg(all(unix, feature = "process"))]
        self.refresh_cleanup_owners();
        joined
    }

    /// Place `body` on this supervisor, waiting for a free slot if the bound is
    /// reached.
    ///
    /// `body` is called **before** anything is spawned, so it does not itself
    /// need to be `Send` or `'static`: only the future it returns does. That is
    /// what lets a call site capture borrowed state while the task it starts
    /// holds none.
    ///
    /// The permit is acquired before the spawn, so the wait is real
    /// backpressure: nothing is queued on this module's behalf.
    pub async fn spawn<F, Fut>(&mut self, body: F)
    where
        F: FnOnce(CancellationToken) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        // The fence lives in `claim`, which is why the constructor below is
        // safe to call: a cancelled supervisor never returns a permit, so the
        // body handed to it is never built. The refusal is counted there.
        let Some(lease) = self.claim().await else {
            return;
        };
        self.place_body(lease, body);
    }
    /// Place `body` on this supervisor, refusing if the bound is reached or
    /// the supervisor is cancelled.
    ///
    /// The non-blocking counterpart to [`Supervisor::spawn`](crate::rt::supervise::Supervisor::spawn). A refusal is a
    /// value, not a silent drop, and the two refusals name different worlds:
    /// [`TrySpawnRefusal::AtCapacity`] is a healthy supervisor that is full,
    /// while [`TrySpawnRefusal::Cancelled`] is a supervisor that has stopped
    /// admitting. Either way the body constructor has not run when the refusal
    /// is returned.
    ///
    /// # Errors
    ///
    /// [`TrySpawnRefusal::AtCapacity`] when every slot is taken;
    /// [`TrySpawnRefusal::Cancelled`] once the supervisor has been cancelled.
    pub fn try_spawn<F, Fut>(&mut self, body: F) -> Result<(), TrySpawnRefusal>
    where
        F: FnOnce(CancellationToken) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let Some(lease) = self.claim_now() else {
            // `claim_now` counted the refusal; which world refused is the one
            // fact the counter cannot carry, so it is read here for the type.
            if self.token.is_cancelled() {
                let refusal = Err(TrySpawnRefusal::Cancelled);
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "try_spawn: returning an error to the caller");
                return refusal;
            }
            let refusal = Err(TrySpawnRefusal::AtCapacity);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "try_spawn: returning an error to the caller");
            return refusal;
        };
        self.place_body(lease, body);
        Ok(())
    }

    /// Place `body` on this supervisor under `tenant`, waiting for a free slot if
    /// the bound is reached.
    ///
    /// The tenant-scoped counterpart of [`Supervisor::spawn`](crate::rt::supervise::Supervisor::spawn), and the only
    /// door that *charges* a tenant: the admission this call waits for counts
    /// against `tenant`'s own ceiling under the supervisor's [`TenancyPolicy`],
    /// and the slot it is given was offered by the deficit round robin because
    /// `tenant` had work waiting when a permit freed. That is the whole
    /// contract — one tenant cannot hold more than its share of the supervisor,
    /// so it cannot starve another.
    ///
    /// `body` is called **after** the admission is granted and before the task
    /// is placed, exactly as [`Supervisor::spawn`](crate::rt::supervise::Supervisor::spawn) calls it, so it need not be
    /// `Send` or `'static` itself; only the future it returns is.
    ///
    /// # Errors
    ///
    /// [`SpawnRefused::TenantAtCapacity`](crate::rt::tenancy::SpawnRefused::TenantAtCapacity) when `tenant` already holds its full
    /// queue of waiting admissions, naming the tenant and the bound;
    /// [`SpawnRefused::Cancelled`](crate::rt::tenancy::SpawnRefused::Cancelled) once the supervisor has stopped admitting.
    /// Other tenants are unaffected by either refusal.
    #[cfg(feature = "script")]
    pub async fn spawn_for<F, Fut>(&mut self, tenant: &Tenant, body: F) -> Result<(), SpawnRefused>
    where
        F: FnOnce(CancellationToken) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        // A supervisor with no policy admits this exactly as `spawn` does — the
        // implicit tenant is the whole story, and there is no per-tenant ceiling
        // to charge it to.
        let Some(shell) = self.tenancy.clone() else {
            self.spawn(body).await;
            return Ok(());
        };
        let lease = match self.claim_tenanted(&shell, tenant).await {
            Ok(lease) => lease,
            Err(refusal) => {
                self.refused = self.refused.saturating_add(1);
                let refused = Err(refusal);
                lgwks_std::trace::debug!(error = ?refused.as_ref().err(), "spawn_for: returning an error to the caller");
                return refused;
            }
        };
        self.place_body(lease, body);
        Ok(())
    }

    /// How many of `tenant`'s admissions this supervisor holds, and how many it
    /// has waiting.
    ///
    /// `(in flight, queued)`, read from the same per-tenant counters admission
    /// charges and the round serves (INV-BOT-31's rule: inspection reads the
    /// admission's own state, never a second ledger). `(0, 0)` for a tenant that
    /// has never arrived, for an untenanted supervisor, and for one whose entry
    /// the scheduler has already retired as idle.
    #[cfg(feature = "script")]
    #[must_use]
    pub fn tenant_capacity(&self, tenant: &Tenant) -> (usize, usize) {
        match self.tenancy.as_ref() {
            Some(shell) => shell.counts_of(tenant),
            None => (0, 0),
        }
    }

    /// Cumulative admission-side cost of this supervisor's tenancy round.
    ///
    /// `(admit_ops, admit_nanos, release_ops, release_nanos)`: how many
    /// lock-hold arrival decisions the round has taken and their cumulative
    /// wall time in nanoseconds, beside how many releases completing tasks
    /// took through the same lock and theirs. The arrival half is the
    /// arrival's wait slot plus the round and the pump under the scheduler
    /// lock (#351's admission side). The wait to *take* that lock is not
    /// charged: it is another decision holding the round, counted by that
    /// decision. A tenanted admission's reap, its task spawn, and the
    /// executor's own polls are outside the counters too, so the remainder of
    /// a measured admission wait is the scheduling share. `(0, 0, 0, 0)` for a
    /// supervisor built without a [`TenancyPolicy`], which takes no round
    /// decisions at all.
    ///
    /// `one_tenanted_admission_takes_at_least_one_arrival_decision` (`tests/it/rt_front_door.rs`)
    /// holds it: one `spawn_for` on a supervisor of four takes at least one arrival decision.
    #[cfg(feature = "script")]
    #[must_use]
    pub fn admission_cost(&self) -> (u64, u64, u64, u64) {
        match self.tenancy.as_ref() {
            Some(shell) => shell.decision_cost(),
            None => (0, 0, 0, 0),
        }
    }

    /// How many tenants currently hold at least one waiting admission.
    ///
    /// The load the round is carrying, for a report or an assertion about
    /// whether a fleet of tenants is actually backed up. Zero for an untenanted
    /// supervisor, which has no per-tenant queues at all.
    #[cfg(feature = "script")]
    #[must_use]
    pub fn tenants_waiting(&self) -> usize {
        match self.tenancy.as_ref() {
            Some(shell) => shell.tenants_waiting(),
            None => 0,
        }
    }

    /// Start a task that repeats under `budget` until the budget or the
    /// supervisor ends it.
    ///
    /// This is the shape a long-running bot loop should take. The bound is a
    /// required argument, so there is no call that reads as "loop forever".
    ///
    /// The loop's [`Outcome`](crate::rt::supervise::Outcome) is not discarded: a loop the supervisor cancelled
    /// is reported as [`TaskOutcome::Cancelled`](crate::rt::supervise::TaskOutcome::Cancelled) and one that spent its budget
    /// as [`TaskOutcome::Completed`](crate::rt::supervise::TaskOutcome::Completed), so the two are distinguishable from
    /// outside the task.
    pub async fn spawn_repeating<F, Fut>(&mut self, budget: Budget, body: F)
    where
        F: FnMut(u64) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let Some(permit) = self.claim().await else {
            return;
        };
        let token = self.child_token();
        let clock = self.clock.clone();
        self.place(permit, async move {
            match repeat_on(&clock, &token, budget, body).await {
                Outcome::Exhausted { .. } => TaskEnd::Completed,
                Outcome::Cancelled { .. } => TaskEnd::Cancelled,
            }
        });
    }
}

impl Drop for Supervisor {
    /// Cancel the token and abort the set.
    ///
    /// This is the guarantee that a supervisor which goes out of scope takes
    /// its tasks with it. It is deliberately synchronous: `Drop` cannot await,
    /// so tasks are aborted rather than joined. Use [`Supervisor::shutdown`](crate::rt::supervise::Supervisor::shutdown)
    /// when the caller needs to know they have finished before continuing.
    ///
    /// The drop also sweeps adopted zombies: on Linux the subreaper flag
    /// reparents orphaned descendants to this process, and without a reap
    /// they stay `kill(pid, 0)`-present past the death they report. The sweep
    /// reaps only this process's own children outside the live and parked
    /// sets, so it cannot take a pid another owner still tracks. Best effort
    /// and single-pass — `Drop` cannot await the deaths, so a zombie still
    /// dying here is collected by the next cleanup's confirming sweep rather
    /// than waited on.
    fn drop(&mut self) {
        self.token.cancel();
        self.set.abort_all();
        #[cfg(all(target_os = "linux", feature = "process"))]
        let _collected = sweep_adopted();
    }
}

/// Reports the supervisor's counters, its remaining capacity, and how much it
/// is still tracking.
///
/// Manual rather than derived: the supervisor owns a `JoinSet`, a permit pool
/// and a report buffer, and a derived rendering would descend into each of them
/// without answering the question a reader of a supervisor's `Debug` actually
/// has — what is still running, and what is left to run it. Every field printed
/// here is one of those answers, and each is read through the accessor or
/// length the type already exposes rather than through its interior.
impl core::fmt::Debug for Supervisor {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Supervisor")
            .field("stats", &self.stats())
            .field("available_permits", &self.permits.available_permits())
            .field("tracked", &self.set.len())
            .field("reports_queued", &self.reports.len())
            .field("report_cap", &self.report_cap)
            .field("cancelled", &self.token.is_cancelled())
            .finish()
    }
}
