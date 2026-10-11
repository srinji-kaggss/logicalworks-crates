//! How a supervised task ended, and what its cleanup could account for.

use super::TaskId;
#[cfg(all(unix, feature = "process"))]
use super::owners::Capture;
#[cfg(feature = "process")]
use lgwks_std::process::ContainmentMechanism;
use std::process::ExitStatus;
/// How one supervised task ended, as its owner observed it.
///
/// This is the evidence an owner needs and used to be handed nothing of. A
/// return, a cooperative stop, an abort and a panic were one counter, so a task
/// that died before producing its result was indistinguishable — through every
/// public surface — from one that returned normally.
///
/// The states are a product, not a single flag: a caller's response to a
/// panic (surface the payload, page someone) is not its response to a
/// cancellation (expected, nothing to do), nor to an abort (a task ignored its
/// token, which is a bug in the body), nor to a supervised process that exited
/// non-zero (work that ran and did not do what it was asked).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TaskOutcome {
    /// The body ran to completion and returned normally.
    Completed {
        /// The task that ended.
        task: TaskId,
        /// Process-tree cleanup evidence, when this was a supervised process.
        cleanup: Option<CleanupReceipt>,
        /// What the cleanup read, signalled and could not account for.
        #[cfg(feature = "process")]
        containment: Option<Containment>,
    },
    /// The body returned while cancellation was in force.
    ///
    /// Two shapes report this. A body built on [`repeat`](crate::rt::supervise::repeat) whose loop stopped at
    /// the token says so itself — that is why [`Supervisor::spawn_repeating`](crate::rt::supervise::Supervisor::spawn_repeating)
    /// routes the loop's [`Outcome`](crate::rt::supervise::Outcome) here instead of dropping it where nobody
    /// can see it. A body placed with [`Supervisor::spawn`](crate::rt::supervise::Supervisor::spawn) has no such
    /// channel, so the supervisor reads its own token at the instant the body
    /// returned; that reading is a fact about the token, not a claim about the
    /// body's intent, and it is the conservative direction — a body that
    /// finished in the same instant cancellation landed is reported as
    /// cancelled rather than as a success it may not have been.
    Cancelled {
        /// The task that ended.
        task: TaskId,
        /// Process-tree cleanup evidence, when this was a supervised process.
        cleanup: Option<CleanupReceipt>,
        /// What the cleanup read, signalled and could not account for.
        #[cfg(feature = "process")]
        containment: Option<Containment>,
    },
    /// The runtime dropped the body before it finished.
    ///
    /// Distinct from [`Self::Cancelled`] on purpose. A cancelled body chose to
    /// stop; an aborted one was dropped at a suspension point because it did
    /// not, so the two need different repairs and a single count would hide
    /// the second.
    Aborted {
        /// The task that ended.
        task: TaskId,
    },
    /// The body panicked.
    ///
    /// The payload is preserved rather than logged and dropped: an owner that
    /// cannot see *why* the task died owns a supervisor that reports failure
    /// without a cause, which is a slightly louder version of reporting
    /// nothing.
    Panicked {
        /// The task that ended.
        task: TaskId,
        /// The panic payload, rendered.
        message: String,
    },
    /// A supervised process ended without exiting successfully — and the
    /// supervisor did not stop it.
    ///
    /// This is the outcome `Supervisor::spawn_process` reports — that method is
    /// behind the `process` feature — and it is the one a task body cannot
    /// produce: a body that returns *is* a success, so before
    /// this variant existed a command that exited non-zero and a command that
    /// exited zero were the same report. A process the supervisor killed
    /// because its token was cancelled is [`Self::Cancelled`], not this, so the
    /// two are distinguishable from outside — which is the question an owner of
    /// a subprocess actually asks.
    Failed {
        /// The task that ended.
        task: TaskId,
        /// The status the engine reported, or `None` when it could not report
        /// one (the wait itself failed).
        ///
        /// `None` is not a success and is not silence: it means the process
        /// ended and this supervisor cannot say how, which is reported rather
        /// than folded into [`Self::Cancelled`] — a supervisor that could not
        /// read the status does not know whether the process stopped on its
        /// own.
        ///
        /// On Unix a process killed by a signal reports
        /// [`ExitStatus::code`]`() == None` with a signal number, so a status
        /// that is present is not automatically a code.
        status: Option<ExitStatus>,
        /// Process-tree cleanup evidence, when this was a supervised process.
        cleanup: Option<CleanupReceipt>,
        /// What the cleanup read, signalled and could not account for.
        #[cfg(feature = "process")]
        containment: Option<Containment>,
    },
    /// A retained process-group cleanup owner later proved the group absent.
    #[cfg(all(unix, feature = "process"))]
    CleanupSettled {
        /// The process task whose cleanup obligation was settled.
        task: TaskId,
        /// Terminal cleanup evidence.
        cleanup: CleanupReceipt,
        /// What the retained cleanup read, signalled and could not account for.
        containment: Containment,
    },
}

/// Evidence about the owned process group after its leader ended or cancellation
/// requested termination.
///
/// Unix process groups are weaker than a kernel job object: a descendant that
/// deliberately creates a new session is outside the group itself. This cleanup
/// therefore does not stop at the group — it captures the leader's descendants
/// first and signals each of them by pid — but a capture is a snapshot, and the
/// receipt is where that limit is stated rather than implied.
///
/// # What `CleanupConfirmed` does and does not claim
///
/// **It claims: every process that was still in the supervised group when the
/// group was last observed is gone, and every descendant this cleanup captured
/// had stopped running by the time the bounded drain ended.** The group claim is
/// `killpg(group, 0)` and the descendant claim is
/// `Containment::is_complete` over the pids
/// `lgwks_std::process::capture_descendants` returned while the leader was
/// alive (both behind the `process` feature, which every receipt that carries a
/// capture was built with).
///
/// It does **not** claim that no process this supervisor ever started is still
/// running. Two cases survive that claim, and both are named on the receipt
/// rather than left to be inferred from a green test:
///
/// - A descendant forked *after* the last capture and left the group in the same
///   window. The capture is a snapshot of the parent relation, and a process
///   that was not in it cannot be signalled by pid.
/// - A descendant orphaned before the cleanup ran. It has been adopted by the
///   platform's init and no longer names the leader, so no walk from the leader
///   can reach it.
///
/// Closing the first on Linux is the tree's cgroup v2 scope, killed before the
/// rounds below; closing the second is the subreaper flag, which re-parents
/// the orphan to the supervisor so the post-reap pass can attribute, signal
/// and reap it by pid. A Windows job object would close both there, and is
/// still pending (#263).
///
/// `tests/it/process_escape.rs` exercises a real `setsid` escape and states
/// this boundary against a live process.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CleanupReceipt {
    /// The group no longer exists, and every captured descendant stopped running.
    CleanupConfirmed,
    /// SIGKILL was delivered while the unreaped leader pinned the group id,
    /// but the bounded drain did not prove removal.
    CleanupPending,
    /// The operating system refused the termination request.
    CleanupFailed,
    /// The group is accounted for, and these captured descendants were still
    /// running when the bounded drain ended.
    ///
    /// Its own arm rather than a `Pending`: "the group is gone but a `setsid`
    /// descendant of it is still running" and "the group itself is still there"
    /// are different facts with different repairs, and folding them into one
    /// pending state would report a stopped tree as an unstopped one. The pids
    /// are named so an operator can act on them, and
    /// `Containment::residual_risk` says which limit produced them.
    CleanupSurvivors {
        /// The captured pids that were still running when the drain ended,
        /// sorted. Never truncated: a survivor list a caller cannot read in full
        /// is a list it cannot act on.
        survivors: Vec<i32>,
    },
}

impl CleanupReceipt {
    /// Whether this receipt claims the supervised tree was stopped.
    ///
    /// `true` only for [`Self::CleanupConfirmed`], and `false` for the pending,
    /// failed and survivor arms alike — a caller asking "is it gone" must not
    /// read an unresolved cleanup as a clean one.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        matches!(self, Self::CleanupConfirmed)
    }

    /// The captured pids this receipt reports as still running, sorted.
    ///
    /// Empty for every other arm: a group that is present, a refused kill and a
    /// clean sweep all report no *named* survivors, and the difference between
    /// them is [`Self::is_complete`], not this list.
    #[must_use]
    pub fn survivors(&self) -> &[i32] {
        match *self {
            Self::CleanupSurvivors { ref survivors } => survivors.as_slice(),
            Self::CleanupConfirmed | Self::CleanupPending | Self::CleanupFailed => &[],
        }
    }
}

/// Why a cleanup could not account for every process it started.
///
/// Each arm is a limit of the mechanism that ran, reported so an operator reads
/// the receipt's scope instead of taking its silence for a claim.
#[cfg(feature = "process")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResidualRisk {
    /// No process table could be read, so nothing outside the group was captured
    /// and a member that forked and left the group is not accounted for.
    TableUnreadable,
    /// The capture stopped at
    /// [`lgwks_std::process::MAX_CAPTURED_DESCENDANTS`], so it held a prefix of
    /// the tree rather than the tree.
    CaptureTruncated,
    /// The leader had already exited when the cleanup began, so no capture could
    /// name its tree.
    ///
    /// A process's children are re-parented when it exits, not when it is
    /// reaped, so a walk from an exited leader reaches nothing whatever it left
    /// running. A descendant that stayed in the group is still stopped by the
    /// group signal; one that called `setsid` before the exit is out of reach.
    /// The cleanup therefore reads no table, and the report names that rather
    /// than presenting an empty capture as an empty tree.
    LeaderExited,
}

/// What one cleanup attempt read, signalled, and could not account for.
///
/// Carried by every supervised-process [`TaskOutcome`](crate::rt::supervise::TaskOutcome) and by every
/// [`ProcessRun`], because the question a caller
/// actually has after "stop the process" is *which mechanism ran* and *which pids
/// are still up* — and a single boolean cannot answer either.
///
/// Behind the `process` feature with the rest of the containment vocabulary: a
/// build without it starts no child process, so it has no tree to account for.
///
/// [`ProcessRun`]: crate::rt::process::ProcessRun
#[cfg(feature = "process")]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Containment {
    /// How the descendant tree was read.
    pub(super) mechanism: ContainmentMechanism,
    /// How many distinct descendants the capture held.
    pub(super) captured: usize,
    /// How many of them a signal was delivered to.
    pub(super) signalled: usize,
    /// The captured pids still running when the drain ended, sorted.
    pub(super) survivors: Vec<i32>,
    /// The limit of the mechanism that ran, when there is one.
    pub(super) residual: Option<ResidualRisk>,
}

#[cfg(feature = "process")]
impl Containment {
    /// How the descendant tree was read.
    ///
    /// [`ContainmentMechanism::ProcessGroupOnly`] means no process table was
    /// read at all, and every descendant claim this report could otherwise make
    /// is absent rather than satisfied.
    #[must_use]
    pub const fn mechanism(&self) -> ContainmentMechanism {
        self.mechanism
    }

    /// How many distinct descendants the capture held.
    #[must_use]
    pub const fn captured(&self) -> usize {
        self.captured
    }

    /// How many captured descendants a signal was delivered to.
    ///
    /// Below [`Self::captured`] when a pid was already gone when it was named:
    /// a signal to a process that has ended is refused with `ESRCH`, which is the
    /// answer that process has finished and not a failure.
    #[must_use]
    pub const fn signalled(&self) -> usize {
        self.signalled
    }

    /// The captured pids still running when the bounded drain ended, sorted.
    #[must_use]
    pub fn survivors(&self) -> &[i32] {
        &self.survivors
    }

    /// The limit of the mechanism that ran, or `None` when it ran whole.
    #[must_use]
    pub const fn residual_risk(&self) -> Option<ResidualRisk> {
        self.residual
    }

    /// Whether the capture was whole and every captured descendant stopped.
    ///
    /// `false` for a survivor, for a truncated capture, for a target that
    /// exposed no process table, and for a leader that had already exited on its
    /// own when the cleanup began ([`ResidualRisk::LeaderExited`]): all four are
    /// "this report does not account for the whole tree", which is the one
    /// question the flag answers. The last is the common case for a process that
    /// simply finished, and it is `false` because a `setsid` descendant it left
    /// running is invisible from an exited leader, not because one was seen.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.survivors.is_empty() && self.residual.is_none() && self.mechanism.read_a_table()
    }

    /// Fold one capture into this report.
    #[cfg(all(unix, feature = "process"))]
    pub(super) fn record_capture(&mut self, set: &Capture) {
        self.mechanism = set.mechanism.strongest(self.mechanism);
        if set.truncated {
            self.note(ResidualRisk::CaptureTruncated);
        }
        self.captured = self.captured.max(set.pids.len());
    }

    /// Record that a kernel-level owner stopped the tree.
    ///
    /// The same strongest-wins merge as [`Self::record_capture`]: a cgroup
    /// kill or a subreaper adoption that ran beside the capture rounds is what
    /// the receipt names, because "the kernel owned the membership" is a
    /// stronger claim than "a snapshot named these pids".
    #[cfg(all(target_os = "linux", feature = "process"))]
    pub(super) fn record_owner(&mut self, mechanism: ContainmentMechanism) {
        self.mechanism = self.mechanism.strongest(mechanism);
    }

    /// Record that a signal reached one captured pid.
    pub(super) fn record_signal(&mut self) {
        self.signalled = self.signalled.saturating_add(1);
    }

    /// Record which captured pids are still running.
    pub(super) fn record_survivors(&mut self, running: &[i32]) {
        self.survivors = running.to_vec();
    }

    /// Record the limit of the mechanism that ran.
    pub(super) fn note(&mut self, risk: ResidualRisk) {
        // First limit wins: a capture that never ran is the whole story, and a
        // later truncation of the group-only fallback adds nothing to it.
        if self.residual.is_none() {
            self.residual = Some(risk);
        }
    }
}

impl TaskOutcome {
    /// The task this outcome is about.
    #[must_use]
    pub const fn task(&self) -> TaskId {
        match *self {
            Self::Completed { task, .. }
            | Self::Cancelled { task, .. }
            | Self::Aborted { task }
            | Self::Failed { task, .. }
            | Self::Panicked { task, .. } => task,
            #[cfg(all(unix, feature = "process"))]
            Self::CleanupSettled { task, .. } => task,
        }
    }

    /// Whether the body ran to completion.
    ///
    /// `false` for every other state, including cancellation, so a caller that
    /// asks this question cannot accidentally read "the task stopped existing"
    /// as "the task did its work".
    #[must_use]
    pub const fn is_success(&self) -> bool {
        #[cfg(all(unix, feature = "process"))]
        {
            matches!(self, Self::Completed { .. } | Self::CleanupSettled { .. })
        }
        #[cfg(not(all(unix, feature = "process")))]
        {
            matches!(self, Self::Completed { .. })
        }
    }

    /// Whether the body panicked.
    #[must_use]
    pub const fn is_panic(&self) -> bool {
        matches!(self, Self::Panicked { .. })
    }

    /// Whether a supervised process ended without exiting successfully.
    ///
    /// `false` for every other state, so a caller can ask this of any outcome
    /// without matching on the variant first.
    #[must_use]
    pub const fn is_process_failure(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }

    /// The status of a supervised process that did not exit successfully.
    ///
    /// `None` for every other state, and for a process whose status the engine
    /// could not read, so a caller can carry the cause of a process failure out
    /// of a report without matching on the variant — the same shape as
    /// [`Self::panic_message`], and for the same reason: a match arm that
    /// forgot a variant must not silently read one failure as another.
    #[must_use]
    pub fn exit_status(&self) -> Option<ExitStatus> {
        // `*self` rather than `self`, and every state named rather than
        // covered by a wildcard, for the reasons `panic_message` gives: this
        // crate forbids `clippy::pattern_type_mismatch`, and a new variant must
        // force this question to be asked again.
        match *self {
            Self::Failed { status, .. } => status,
            Self::Completed { .. }
            | Self::Cancelled { .. }
            | Self::Aborted { .. }
            | Self::Panicked { .. } => None,
            #[cfg(all(unix, feature = "process"))]
            Self::CleanupSettled { .. } => None,
        }
    }

    /// Process-tree cleanup evidence, when this outcome belongs to a process.
    ///
    /// A borrow, where this returned the receipt by value: the receipt now
    /// carries the pids it could not prove gone, so a by-value return would copy
    /// that list per read and would make `Copy` unsound for a type whose whole
    /// purpose is to name processes.
    #[must_use]
    pub const fn cleanup(&self) -> Option<&CleanupReceipt> {
        match *self {
            Self::Completed { ref cleanup, .. }
            | Self::Cancelled { ref cleanup, .. }
            | Self::Failed { ref cleanup, .. } => cleanup.as_ref(),
            Self::Aborted { .. } | Self::Panicked { .. } => None,
            #[cfg(all(unix, feature = "process"))]
            Self::CleanupSettled { ref cleanup, .. } => Some(cleanup),
        }
    }

    /// What this outcome's cleanup read, signalled, and could not account for.
    ///
    /// `None` for every state that is not a supervised process, and for one whose
    /// cleanup never ran. The receipt says whether the tree was stopped;
    /// this says *how much was looked at* to say it.
    #[cfg(feature = "process")]
    #[must_use]
    pub const fn containment(&self) -> Option<&Containment> {
        match *self {
            Self::Completed {
                ref containment, ..
            }
            | Self::Cancelled {
                ref containment, ..
            }
            | Self::Failed {
                ref containment, ..
            } => containment.as_ref(),
            Self::Aborted { .. } | Self::Panicked { .. } => None,
            #[cfg(all(unix, feature = "process"))]
            Self::CleanupSettled {
                ref containment, ..
            } => Some(containment),
        }
    }

    /// The panic payload, if this outcome is a panic.
    ///
    /// `None` for every other state, so a caller can carry the cause of a
    /// failure out of a report without matching on the variant — and without
    /// the risk of a `Completed` being read as a panic by a match arm that
    /// forgot it.
    #[must_use]
    pub fn panic_message(&self) -> Option<&str> {
        // `*self` rather than `self`: this crate forbids
        // `clippy::pattern_type_mismatch`, which refuses a pattern of the
        // referent's type matched against a reference. The remaining states are
        // named rather than covered by a wildcard so a new variant cannot be
        // added without this question being asked again.
        match *self {
            Self::Panicked { ref message, .. } => Some(message.as_str()),
            Self::Completed { .. }
            | Self::Cancelled { .. }
            | Self::Aborted { .. }
            | Self::Failed { .. } => None,
            #[cfg(all(unix, feature = "process"))]
            Self::CleanupSettled { .. } => None,
        }
    }
}
