//! What a supervisor reports: its shutdown drain, its counters and its snapshot.

use super::TaskId;
#[cfg(feature = "process")]
use super::outcome::Containment;
use super::outcome::{CleanupReceipt, TaskOutcome};
#[cfg(all(unix, feature = "process"))]
use super::owners::{CleanupOwners, GroupObserver, NATIVE_GROUP_OBSERVER, settled_receipt};
#[cfg(all(unix, feature = "process"))]
use std::sync::Arc;

/// What a [`Supervisor::shutdown`](crate::rt::supervise::Supervisor::shutdown) drained, and the counters behind it.
///
/// Returned rather than discarded because shutdown is exactly the moment an
/// owner decides what to do about failure: a panic that is invisible at that
/// point is a panic nobody will act on.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ShutdownReport {
    /// One outcome per task that was live or unreported when shutdown began,
    /// in completion order.
    pub(super) outcomes: Vec<TaskOutcome>,
    /// The supervisor's counters at the moment it finished draining.
    pub(super) stats: Stats,
    /// Keeps transferred cleanup obligations and their leases alive.
    #[cfg(all(unix, feature = "process"))]
    pub(super) cleanup_owners: Arc<CleanupOwners>,
}

impl ShutdownReport {
    /// Every drained outcome, in completion order.
    #[must_use]
    pub fn outcomes(&self) -> &[TaskOutcome] {
        &self.outcomes
    }

    /// Consume the report only when no cleanup owner remains. On `Err`, the
    /// returned report still owns every pending cleanup lease and can be polled
    /// with `reap_pending_cleanups`.
    pub fn into_outcomes(self) -> Result<Vec<TaskOutcome>, Self> {
        #[cfg(all(unix, feature = "process"))]
        if self.pending_cleanup_count() > 0 {
            let refusal = Err(self);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "into_outcomes: returning an error to the caller");
            return refusal;
        }
        Ok(self.outcomes)
    }

    /// The supervisor's counters at the moment it finished draining.
    #[must_use]
    pub const fn stats(&self) -> Stats {
        self.stats
    }

    /// Every task that panicked, in completion order.
    pub fn panicked(&self) -> impl Iterator<Item = &TaskOutcome> {
        self.outcomes.iter().filter(|outcome| outcome.is_panic())
    }

    /// Whether every drained task returned normally.
    ///
    /// Cancellation counts as *not* clean, because a caller asking this
    /// question is asking whether the work finished, and a cancelled task did
    /// not. So does a supervised process that exited non-zero: it ended, and
    /// the caller's question is whether the work succeeded, not whether the
    /// task stopped. So, too, does a supervised process whose containment report
    /// is not complete (`Containment::is_complete`, behind the `process`
    /// feature) — including one that exited on its own,
    /// since nothing can say whether it left a `setsid` descendant running.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.outcomes.iter().all(|outcome| {
            outcome.is_success() && outcome.cleanup().is_none_or(CleanupReceipt::is_complete)
        }) && {
            #[cfg(feature = "process")]
            {
                self.pending_cleanup_count() == 0
                    && self
                        .outcomes
                        .iter()
                        .all(|outcome| outcome.containment().is_none_or(Containment::is_complete))
            }
            #[cfg(not(feature = "process"))]
            {
                true
            }
        }
    }

    /// Every supervised process that did not exit successfully, in completion
    /// order.
    ///
    /// The counterpart of [`ShutdownReport::panicked`] for
    /// `Supervisor::spawn_process` (behind the `process` feature): a report
    /// whose only readable failure is a panic would leave a command that exited
    /// non-zero to be found by hand-filtering [`ShutdownReport::outcomes`].
    pub fn failed(&self) -> impl Iterator<Item = &TaskOutcome> {
        self.outcomes
            .iter()
            .filter(|outcome| outcome.is_process_failure())
    }

    /// Number of cleanup obligations still charged to the process capacity.
    #[cfg(all(unix, feature = "process"))]
    #[must_use]
    pub fn pending_cleanup_count(&self) -> usize {
        self.cleanup_owners.pending_count()
    }

    /// Make one bounded observation pass over cleanup obligations retained by
    /// this report. Each returned outcome is a terminal absence receipt.
    #[cfg(all(unix, feature = "process"))]
    pub fn reap_pending_cleanups(&mut self) -> usize {
        self.reap_pending_cleanups_with(&NATIVE_GROUP_OBSERVER)
    }

    #[cfg(all(unix, feature = "process"))]
    /// Injected-observer seam for deterministic cleanup-owner tests.
    pub(super) fn reap_pending_cleanups_with(&mut self, observer: &dyn GroupObserver) -> usize {
        let settled = self.cleanup_owners.drive(observer, |_| false);
        let count = settled.len();
        self.outcomes
            .extend(
                settled
                    .into_iter()
                    .map(|(task, containment)| TaskOutcome::CleanupSettled {
                        task,
                        cleanup: settled_receipt(&containment),
                        containment,
                    }),
            );
        count
    }
}

/// What a [`Supervisor`](crate::rt::supervise::Supervisor) has done so far.
///
/// Counters saturate rather than wrap, so they stay monotonic over any lifetime
/// a process can actually reach. `completed` counts tasks that *ended*, however
/// they ended, and is split into the five ways that can happen: a caller that
/// only needs resource accounting reads it or [`Stats::in_flight`](crate::rt::supervise::Stats::in_flight), and a
/// caller that needs to know whether the work succeeded reads
/// [`Stats::succeeded`](crate::rt::supervise::Stats::succeeded) and cannot get the answer wrong by reading `completed`
/// instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Stats {
    /// Tasks started, including any that have since finished.
    pub spawned: u64,
    /// Tasks that have finished, however they ended.
    ///
    /// Equal to `succeeded + failed + cancelled + aborted + panicked`. It is
    /// the accounting total, not a success count.
    pub completed: u64,
    /// Tasks whose body ran to completion and returned.
    pub succeeded: u64,
    /// Supervised processes that ended without exiting successfully.
    ///
    /// Zero for a supervisor that never ran one. A process the supervisor
    /// killed because its token was cancelled counts as `cancelled` instead:
    /// this counter answers "did the command succeed", not "did the child
    /// stop".
    pub failed: u64,
    /// Tasks that observed their token and returned because of it.
    pub cancelled: u64,
    /// Tasks the runtime dropped before they finished.
    pub aborted: u64,
    /// Tasks whose body panicked.
    pub panicked: u64,
    /// Spawn attempts refused by [`Supervisor::try_spawn`](crate::rt::supervise::Supervisor::try_spawn) at the bound.
    pub refused: u64,
    /// Terminal outcomes not retained because the report buffer was full.
    ///
    /// The buffer is capped at the in-flight bound, so a caller that spawns
    /// without ever draining its reports loses detail — never memory — and this
    /// counter is how it learns that it did.
    pub reports_dropped: u64,
}

impl Stats {
    /// Tasks started and not yet reaped.
    ///
    /// This is accounting, not a live count: `completed` advances when a task
    /// is joined (by [`Supervisor::reap`](crate::rt::supervise::Supervisor::reap) or the reap every spawn performs), so
    /// a task that has ended and released its permit counts here until then,
    /// and the value can briefly exceed the in-flight bound. The bound itself
    /// is enforced by the permits, which a task holds for its whole run.
    ///
    /// Saturated subtraction: `completed` cannot exceed `spawned` because both
    /// advance on the same path, but a counter pair that silently wrapped would
    /// be worse than one that pins at zero.
    #[must_use]
    pub const fn in_flight(self) -> u64 {
        self.spawned.saturating_sub(self.completed)
    }
}

/// What one task a [`Supervisor`](crate::rt::supervise::Supervisor) owns is currently doing, as the owner reads
/// it.
///
/// A sum type rather than a flag so a state the owner learns to tell apart can be
/// added without changing what `Running` means. Today the owner distinguishes
/// one: a placed task holds a permit until it is reaped, whether or not its body
/// has returned, so capacity read from this listing is capacity the supervisor
/// will actually grant. Terminal outcomes are not listed here; they are in the
/// report stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TaskState {
    /// Placed and holding a permit, not yet reaped.
    Running {
        /// The task's position among the live tasks, in spawn order.
        ///
        /// Not a permit number: permits are interchangeable and carry no
        /// identity. Always below the in-flight ceiling.
        position: usize,
    },
}

impl TaskState {
    /// Whether the task is still doing work.
    #[must_use]
    pub const fn is_running(self) -> bool {
        matches!(self, Self::Running { .. })
    }
}

/// A bounded, point-in-time view of one [`Supervisor`](crate::rt::supervise::Supervisor).
///
/// This is **not** a second ledger. Every counter here is read from the same
/// fields the supervisor's own admission and reporting paths use, at the
/// instant of the call, and the snapshot owns nothing: no background sampler, no
/// retained history, no queue. That is the whole point — an inspection surface
/// that maintained its own state could disagree with the owner, and a
/// disagreement between the thing that runs work and the thing that reports on it
/// is worse than no inspection at all.
///
/// # Bounds
///
/// [`Self::live`] is capped at [`Self::live_limit`], and [`Self::live_truncated`]
/// says when it was. The live set is exactly the in-flight ceiling, which is
/// itself bounded, so the cap is a defence against a caller that raised the
/// ceiling rather than a routine truncation.
///
/// Terminal outcomes are **not** in this snapshot. They are already retained,
/// already bounded by the report cap, and already drainable through
/// [`Supervisor::next_report`](crate::rt::supervise::Supervisor::next_report); a snapshot that carried its own copy of them
/// would be the second source of truth this type refuses to be. A caller that
/// needs the authoritative terminal record reads the report stream.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SupervisorSnapshot {
    /// The declared in-flight ceiling, as the supervisor holds it.
    pub(super) max_in_flight: usize,
    /// The supervisor's own counters at this instant.
    pub(super) stats: Stats,
    /// Live tasks, at most `live_limit` of them, in [`TaskId`] order.
    pub(super) live: Vec<LiveTask>,
    /// How many live tasks the cap excluded.
    pub(super) live_truncated: usize,
    /// Whether the supervisor has stopped admitting.
    pub(super) cancelled: bool,
    /// Terminal outcomes waiting to be drained through
    /// [`Supervisor::next_report`](crate::rt::supervise::Supervisor::next_report).
    pub(super) reports_pending: usize,
    /// Cleanup obligations still charged to this supervisor's capacity.
    #[cfg(all(unix, feature = "process"))]
    pub(super) pending_cleanups: usize,
}

impl SupervisorSnapshot {
    /// The declared in-flight ceiling this supervisor was built with.
    #[must_use]
    pub const fn max_in_flight(&self) -> usize {
        self.max_in_flight
    }

    /// The supervisor's own counters at the instant of the snapshot.
    #[must_use]
    pub const fn stats(&self) -> Stats {
        self.stats
    }

    /// The live tasks, in spawn order, at most [`Self::live_limit`] of them.
    ///
    /// A borrow rather than the vector itself: the listing is a snapshot of the
    /// supervisor's state, and handing out the `Vec` would hand out the right to
    /// edit what a reader believes it observed. Nothing a caller does to the
    /// returned slice can change what the supervisor does next, and that is the
    /// whole claim of this type.
    #[must_use]
    pub fn live(&self) -> &[LiveTask] {
        &self.live
    }

    /// How many live tasks the cap excluded from [`Self::live`].
    ///
    /// Non-zero means the listing is a bounded prefix, and a caller drawing
    /// conclusions about capacity from it must account for what it did not see.
    #[must_use]
    pub const fn live_truncated(&self) -> usize {
        self.live_truncated
    }

    /// Whether the supervisor has stopped admitting.
    #[must_use]
    pub const fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    /// Terminal outcomes waiting to be drained through
    /// [`Supervisor::next_report`](crate::rt::supervise::Supervisor::next_report).
    ///
    /// The authoritative record is that stream; this is how many are queued, so a
    /// caller can size its drain before starting it.
    #[must_use]
    pub const fn reports_pending(&self) -> usize {
        self.reports_pending
    }

    /// Cleanup obligations still charged to this supervisor's capacity.
    #[cfg(all(unix, feature = "process"))]
    #[must_use]
    pub const fn pending_cleanups(&self) -> usize {
        self.pending_cleanups
    }

    /// The largest number of live tasks this snapshot can carry.
    ///
    /// Equal to the supervisor's in-flight ceiling, so a caller can size a
    /// buffer for [`Self::live`] once instead of guessing.
    #[must_use]
    pub const fn live_limit(&self) -> usize {
        self.max_in_flight
    }

    /// Whether the live listing came back empty and nothing was truncated.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.live.is_empty() && self.live_truncated == 0
    }

    /// Permits currently taken, from the live set.
    ///
    /// Equal to the number of `Running` entries, which is why it is derived here
    /// rather than tracked separately: a second counter would be a second thing
    /// that can disagree with the first.
    #[must_use]
    pub fn occupied(&self) -> usize {
        self.live
            .iter()
            .filter(|task| task.state.is_running())
            .count()
    }

    /// Permits this supervisor has free right now.
    #[must_use]
    pub fn free(&self) -> usize {
        self.max_in_flight.saturating_sub(self.occupied())
    }

    /// The next action the supervisor can take, as one value.
    ///
    /// The decision an owner actually has to make, rather than the four counters
    /// it is derived from. "Admission closed" is the one answer a caller that was
    /// about to spawn must not have to infer from three places.
    #[must_use]
    pub fn next_action(&self) -> NextAction {
        if self.cancelled {
            NextAction::Stopped
        } else if self.free() > 0 {
            NextAction::Admit
        } else {
            NextAction::WaitForCapacity
        }
    }
}

/// What a [`Supervisor`](crate::rt::supervise::Supervisor) will accept next, from one snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum NextAction {
    /// There is a free permit; a spawn would start.
    Admit,
    /// Every permit is taken; a spawn would block until one is released.
    WaitForCapacity,
    /// The supervisor is cancelled and admits nothing further.
    ///
    /// A spawn would be refused rather than queued, so a caller that retries here
    /// waits for something that will never happen.
    Stopped,
}

/// One live task inside a [`SupervisorSnapshot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct LiveTask {
    /// The supervisor's stable identity for this task, in spawn order.
    pub task: TaskId,
    /// What the task is currently doing.
    pub state: TaskState,
}
