use std::any::Any;
use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::future::{Future, poll_fn};
use std::num::{NonZeroU64, NonZeroUsize};
use std::pin::pin;
use std::process::ExitStatus;
use std::sync::Arc;
#[cfg(all(unix, feature = "process"))]
use std::sync::Mutex;
use std::task::{Context, Poll};
use std::time::Duration;

// Only the supervised process reports an `io::Error`; a build without the
// `process` feature has no such fallible call, so the import is gated with it.
#[cfg(feature = "process")]
use std::io;

#[cfg(all(unix, feature = "process"))]
use crate::rt::sync::Notify;
use crate::rt::sync::{OwnedSemaphorePermit, Semaphore};
use lgwks_deps::tokio::task::Id;

#[cfg(all(unix, feature = "process"))]
use lgwks_deps::tokio::process::{ChildStderr, ChildStdout};

/// Re-exported so a consumer reading a [`Containment`] report needs one import,
/// not two: the mechanism is named by the estate's process primitives and read
/// off this crate's receipt.
#[cfg(feature = "process")]
pub use lgwks_std::process::ContainmentMechanism;

/// The Linux kernel owners one cleanup may call on, beside the group and the
/// capture. Gated to the only target that has them; every use site is gated
/// the same way, so a build elsewhere never names them.
#[cfg(all(target_os = "linux", feature = "process"))]
use lgwks_std::process::CgroupScope;

/// A process started by [`Supervisor::spawn_process_identified`]: the task that
/// owns it and the identity of the process leading its group.
#[cfg(feature = "process")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdentifiedSpawn {
    /// The task's id, as [`Supervisor::spawn_process`] would have returned it.
    task: TaskId,
    /// The group leader's identity, read before the child could be reaped.
    leader: lgwks_std::process::ProcessIdentity,
}

#[cfg(feature = "process")]
impl IdentifiedSpawn {
    /// The id of the task that owns the process.
    #[must_use]
    pub const fn task(&self) -> TaskId {
        self.task
    }

    /// The identity of the process leading the child's group — the record a
    /// successor passes to
    /// [`reap_orphaned_group`](crate::rt::process::reap_orphaned_group).
    #[must_use]
    pub const fn leader(&self) -> &lgwks_std::process::ProcessIdentity {
        &self.leader
    }
}

/// The identity of the just-started leader `pid`, which the supervisor has not
/// reaped, so a process holding the pid is the leader itself.
#[cfg(all(any(unix, windows), feature = "process"))]
fn identify_leader(pid: i32) -> io::Result<lgwks_std::process::ProcessIdentity> {
    if let Some(leader) = lgwks_std::process::identify_process(pid)? {
        return Ok(leader);
    }
    let refusal = Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("lgwks_bot: the unreaped leader {pid} was absent from the process table"),
    ));
    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "identify_leader: returning an error to the caller");
    refusal
}

use super::cancel::CancellationToken;
use super::clock::{Clock, TimeSource};
#[cfg(all(unix, feature = "process"))]
use super::io::{AsyncRead, AsyncReadExt};
#[cfg(feature = "process")]
use super::process::ProcessSpec;
#[cfg(all(any(unix, windows), feature = "process"))]
use super::process::CapturedStream;
#[cfg(feature = "process")]
use super::process::{ProcessRun, ProcessRunError};
use super::task::{JoinSet, yield_now};

// Per-tenant admission needs `script::Tenant` — the estate's one tenant
// identity, shared with step keys, `Host` and every run store — and
// `crate::task`, which needs it too, is gated the same way. The policy and the
// refusal are re-exported here because a caller reaches them through the
// supervisor that enforces them, and `rt::tenancy` is where they are defined.
#[cfg(feature = "script")]
pub use super::tenancy::{SpawnRefused, TenancyPolicy};
#[cfg(feature = "script")]
use crate::script::Tenant;

/// Iterations one poll of [`repeat`] may complete before it hands the executor
/// back.
///
/// A body that resolves immediately — `async {}`, or any future that is ready on
/// its first poll — never suspends the loop, so without this the whole loop is
/// one uninterruptible poll: the executor never regains control, and a token
/// cancelled by another task is not observed until the budget is spent. On a
/// current-thread runtime that is not a stall but a deadlock, because the task
/// that would cancel cannot be scheduled at all.
///
/// The bound is on *work per poll*, not on the loop: cancellation is still
/// checked before every iteration, the body is still dropped mid-flight by
/// [`CancellationToken::run_until_cancelled`], and the iteration count is
/// unchanged. Yielding is what makes the abort and cancellation half of the
/// contract reachable, so the interval trades a scheduler round-trip per
/// `YIELD_INTERVAL` iterations for a bounded cancellation latency.
///
/// A body that awaits explicitly pays one extra scheduler poll per interval,
/// which is why this is not smaller; a ready body pays a round-trip it would
/// otherwise never make, which is why it is not larger.
const YIELD_INTERVAL: u64 = 32;

/// How long a repeated body may keep running.
///
/// There is deliberately no free-running variant. `Ongoing` is the unbounded
/// case and it is still terminated by cancellation, which [`repeat`] always
/// races against the body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Budget {
    /// Run the body at most this many times, then stop.
    Iterations(NonZeroU64),
    /// Run the body until this much wall time has passed, then stop.
    ///
    /// The deadline is checked between iterations, so a body that blocks for
    /// longer than the budget overruns it by one iteration. Cancellation is not
    /// subject to that slack: it interrupts the body itself.
    For(Duration),
    /// Run the body until the token is cancelled.
    ///
    /// The name is the contract. This is not "forever"; it is "for as long as
    /// the supervisor that owns it is alive", and it ends the moment that stops
    /// being true.
    Ongoing,
}

/// Why a [`repeat`] stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Outcome {
    /// The budget ran out. Every iteration the budget allowed completed.
    Exhausted {
        /// Iterations the body completed.
        iterations: u64,
    },
    /// The token was cancelled. The body's in-flight iteration was dropped.
    Cancelled {
        /// Iterations the body completed before the cancel landed.
        iterations: u64,
    },
}

impl Outcome {
    /// Iterations the body completed, whichever way the loop ended.
    #[must_use]
    pub const fn iterations(self) -> u64 {
        match self {
            Self::Exhausted { iterations } | Self::Cancelled { iterations } => iterations,
        }
    }

    /// Whether the loop ended because the token was cancelled.
    #[must_use]
    pub const fn was_cancelled(self) -> bool {
        matches!(self, Self::Cancelled { .. })
    }
}

/// Why [`Supervisor::try_spawn`] did not start the work it was handed.
///
/// The two refusals name different worlds: `AtCapacity` means the supervisor
/// is healthy and full, and the caller should decide between waiting and
/// dropping; `Cancelled` means the supervisor has stopped admitting, and the
/// work should go somewhere else entirely. A refusal is counted in
/// [`Stats::refused`] either way. A caller that inspects neither this value
/// nor the counter has an unbounded producer and a bounded consumer, which is
/// a decision to drop work rather than a decision to grow memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TrySpawnRefusal {
    /// Every slot is taken.
    AtCapacity,
    /// The supervisor has been cancelled and admits no new work.
    Cancelled,
}

impl fmt::Display for TrySpawnRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `*self` rather than matching through the reference: the workspace
        // refuses `clippy::pattern_type_mismatch`, which reads a `Self::`
        // pattern against `&Self` as one.
        match *self {
            Self::AtCapacity => formatter.write_str("the supervisor is at its in-flight bound"),
            Self::Cancelled => {
                formatter.write_str("the supervisor is cancelled and admits no new work")
            }
        }
    }
}

impl std::error::Error for TrySpawnRefusal {}

/// The error inside the [`std::io::Error`] that `Supervisor::spawn_process`
/// (behind the `process` feature) returns when the supervisor has been
/// cancelled and admits no process.
///
/// Recover it with `io::Error::downcast_ref::<SupervisorCancelled>()`; the
/// wrapper is an [`std::io::Error`] because the operation's other failure
/// mode — the platform refusing to start the command — already is one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct SupervisorCancelled;

impl fmt::Display for SupervisorCancelled {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the supervisor is cancelled and admits no new process")
    }
}

impl std::error::Error for SupervisorCancelled {}

/// Why a tenant-scoped process run ([`Supervisor::run_process_for`]) produced no
/// report.
///
/// Two arms because the caller has two different repairs: [`Self::Refused`] is
/// the tenant's own load to fix — the bounded queue was full, or the supervisor
/// had stopped admitting — and [`Self::Run`] is everything
/// [`ProcessRunError`] already says about the
/// process itself, carried unchanged rather than flattened. A single arm would
/// make "your tenant is loud" and "the platform could not start the program"
/// the same value, and they are not.
#[cfg(all(feature = "process", feature = "script"))]
#[derive(Debug)]
#[non_exhaustive]
pub enum RunForError {
    /// The run was refused before anything started.
    Refused(SpawnRefused),
    /// The run was admitted and the process itself failed, one way or another.
    Run(ProcessRunError),
}

#[cfg(all(feature = "process", feature = "script"))]
impl fmt::Display for RunForError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Refused(ref refusal) => write!(formatter, "{refusal}"),
            Self::Run(ref error) => write!(formatter, "{error}"),
        }
    }
}

#[cfg(all(feature = "process", feature = "script"))]
impl std::error::Error for RunForError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Refused(ref refusal) => Some(refusal),
            Self::Run(ref error) => Some(error),
        }
    }
}

/// Stable identity of one task a [`Supervisor`] started.
///
/// Assigned by the supervisor, in spawn order, at the moment the task is
/// placed. It is deliberately not the async engine's own task id: that one is
/// documented as neither sequential nor indicative of spawn order, and it would
/// put an engine type on this crate's public surface. What matters here is that
/// a terminal report is *attributable* — two tasks that ended the same way are
/// still two tasks, and "the third one I started panicked" must be a statement
/// the API can make.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub struct TaskId(u64);

impl TaskId {
    /// The identity as a number, for a report, a log line, or an assertion.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "task {}", self.0)
    }
}

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
    /// Two shapes report this. A body built on [`repeat`] whose loop stopped at
    /// the token says so itself — that is why [`Supervisor::spawn_repeating`]
    /// routes the loop's [`Outcome`] here instead of dropping it where nobody
    /// can see it. A body placed with [`Supervisor::spawn`] has no such
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
/// Carried by every supervised-process [`TaskOutcome`] and by every
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
    mechanism: ContainmentMechanism,
    /// How many distinct descendants the capture held.
    captured: usize,
    /// How many of them a signal was delivered to.
    signalled: usize,
    /// The captured pids still running when the drain ended, sorted.
    survivors: Vec<i32>,
    /// The limit of the mechanism that ran, when there is one.
    residual: Option<ResidualRisk>,
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
    fn record_capture(&mut self, set: &Capture) {
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
    fn record_owner(&mut self, mechanism: ContainmentMechanism) {
        self.mechanism = self.mechanism.strongest(mechanism);
    }

    /// Record that a signal reached one captured pid.
    fn record_signal(&mut self) {
        self.signalled = self.signalled.saturating_add(1);
    }

    /// Record which captured pids are still running.
    fn record_survivors(&mut self, running: &[i32]) {
        self.survivors = running.to_vec();
    }

    /// Record the limit of the mechanism that ran.
    fn note(&mut self, risk: ResidualRisk) {
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

