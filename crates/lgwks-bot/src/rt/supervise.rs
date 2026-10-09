//! Supervision: background work that cannot leak and cannot run away.
//!
//! A caller writing a bot should be thinking about the bot. They should not be
//! thinking about whether a task outlives its owner, whether a fan-out has a
//! ceiling, whether a `JoinSet` is being drained, or whether a `loop` has an
//! exit. Two failure modes cause almost all of that thinking, and both are
//! designed out here rather than documented away.
//!
//! # Why a task cannot leak
//!
//! Four mechanisms, none of which the caller has to remember:
//!
//! - **Nothing is detached.** [`Supervisor::spawn`] returns no handle. A
//!   `JoinHandle` a caller might drop is the leak; there is no handle to drop.
//!   The supervisor owns completion and exposes it as [`Stats`] and as a
//!   bounded stream of [`TaskOutcome`] reports.
//! - **Nothing grows without a ceiling.** There is no unbounded constructor and
//!   no internal queue: [`Supervisor::new`] takes the in-flight bound, and
//!   [`Supervisor::spawn`] applies backpressure by awaiting a permit before it
//!   spawns. The pending work is the caller's own collection, not a buffer this
//!   module accumulates. [`Supervisor::try_spawn`] refuses instead of growing,
//!   and counts the refusal.
//! - **Completed tasks are reaped.** A `JoinSet` retains a finished task's slot
//!   until it is joined, so a set that is spawned into but never drained grows
//!   with total spawns rather than with live tasks. Every entry point reaps
//!   first, which keeps the retained set at the live bound rather than the
//!   lifetime total.
//! - **Drop stops everything.** [`Drop`] cancels the token and aborts the set,
//!   so a supervisor that goes out of scope takes its tasks with it. There is no
//!   `close()` to forget.
//!
//! No reference cycle is possible either: cancellation links a child **up** to
//! its parent and a parent holds nothing back, so a task holding its token does
//! not keep the supervisor alive. That is [`CancellationToken`]'s design, not
//! this module's, and it is the reason this module adds no `Weak` bookkeeping.
//!
//! # Why a caller can tell how a task ended
//!
//! Owning completion is only half of it. A `JoinSet` hands back a
//! `Result<(), JoinError>`, and an owner that joins the set and looks only at
//! `is_some()` throws that result away: a task that returned, a task that was
//! aborted, and a task that panicked before producing anything all become the
//! same increment. The caller then cannot distinguish work that finished from
//! work that died, which is the one thing they needed to know.
//!
//! So every task this module starts ends in exactly one [`TaskOutcome`], and
//! the outcome is attributable: it carries the [`TaskId`] the supervisor
//! assigned in spawn order, so "the second task I started panicked" is a
//! statement the API can make. The reports are held in a buffer capped at the
//! in-flight bound — a caller that never drains it loses detail, never memory,
//! and [`Stats::reports_dropped`] says how much. [`Supervisor::shutdown`]
//! returns the drained outcomes instead of discarding them.
//!
//! # Why a loop cannot run away
//!
//! [`repeat`] is the only loop this module asks a caller to write, and it
//! cannot be written without a [`Budget`]. Every iteration races the
//! cancellation token, so a cancel always wins even against a body that is
//! itself awaiting: the body is dropped, not waited on. [`Budget::Ongoing`] is
//! the unbounded case, and it is bounded in the way that matters: it is
//! cancellation-terminated, not free-running.
//!
//! Termination cannot rest on the body suspending. A body that resolves
//! immediately — the simplest repeating body there is — would otherwise make the
//! whole loop a single uninterruptible poll, and a cancellation ordered by
//! another task would not be delivered until the budget ran out. `repeat`
//! therefore performs a bounded number of iterations per poll and yields, which
//! is what keeps the cancellation and abort halves of the contract reachable.
//! The bound is documented on `repeat` itself; the interval is private, and no
//! caller has to know it.
//!
//! ```no_run
//! use lgwks_bot::rt::supervise::{Budget, Supervisor};
//!
//! # async fn run() {
//! let mut supervisor = Supervisor::new(4);
//! // `Ongoing` is the unbounded budget, and it still ends: the supervisor owns
//! // the token this loop is raced against, and `shutdown` cancels it.
//! supervisor
//!     .spawn_repeating(Budget::Ongoing, |tick| async move {
//!         let _tick = tick;
//!     })
//!     .await;
//! // The drained evidence, not just an empty set.
//! let report = supervisor.shutdown().await;
//! # }
//! ```
//!
//! # Limits
//!
//! [`Supervisor::spawn`] awaits a permit, so a caller that holds the last
//! permits inside a task this supervisor owns can deadlock with itself. The
//! bound is a backpressure contract, not a queue: a caller who wants to keep
//! producing past the ceiling should use [`Supervisor::try_spawn`] and decide
//! what the refusal means.

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
#[cfg(all(unix, feature = "process"))]
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
#[cfg(all(unix, feature = "process"))]
use super::process::{CapturedStream, ProcessRun, ProcessRunError};
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

/// What a [`Supervisor::shutdown`] drained, and the counters behind it.
///
/// Returned rather than discarded because shutdown is exactly the moment an
/// owner decides what to do about failure: a panic that is invisible at that
/// point is a panic nobody will act on.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ShutdownReport {
    /// One outcome per task that was live or unreported when shutdown began,
    /// in completion order.
    outcomes: Vec<TaskOutcome>,
    /// The supervisor's counters at the moment it finished draining.
    stats: Stats,
    /// Keeps transferred cleanup obligations and their leases alive.
    #[cfg(all(unix, feature = "process"))]
    cleanup_owners: Arc<CleanupOwners>,
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
    fn reap_pending_cleanups_with(&mut self, observer: &dyn GroupObserver) -> usize {
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

/// What a [`Supervisor`] has done so far.
///
/// Counters saturate rather than wrap, so they stay monotonic over any lifetime
/// a process can actually reach. `completed` counts tasks that *ended*, however
/// they ended, and is split into the five ways that can happen: a caller that
/// only needs resource accounting reads it or [`Stats::in_flight`], and a
/// caller that needs to know whether the work succeeded reads
/// [`Stats::succeeded`] and cannot get the answer wrong by reading `completed`
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
    /// Spawn attempts refused by [`Supervisor::try_spawn`] at the bound.
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
    /// is joined (by [`Supervisor::reap`] or the reap every spawn performs), so
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

/// One in-flight admission: the permit, and — when the supervisor was built
/// with a [`TenancyPolicy`] — the tenant the permit is charged to.
///
/// The permit and the tenant travel together because their lifetimes are one
/// fact: a task that ends returns its permit *to the round* rather than to the
/// pool, and the round needs to know which tenant just freed a slot. A
/// supervisor without a policy never charges a tenant, so its leases carry no
/// charge and their drop is exactly the bare permit's drop.
#[cfg(feature = "script")]
struct Lease {
    /// The pool permit, taken out of the pool when the admission was granted,
    /// and taken out of the lease only by its own drop.
    permit: Option<OwnedSemaphorePermit>,
    /// The tenant this admission is charged to, when the supervisor installed a
    /// policy. `None` for every untenanted supervisor.
    charge: Option<TenantCharge>,
}

/// A build without tenancy has no round, so its lease is the permit alone: held
/// for the task's life and returned to the pool by its own drop.
#[cfg(not(feature = "script"))]
struct Lease {
    /// The pool permit, held only so that dropping the lease releases it.
    _permit: OwnedSemaphorePermit,
}

/// The tenant half of a [`Lease`], handed to the round when the lease drops.
#[cfg(feature = "script")]
struct TenantCharge {
    /// The shell that admitted this lease.
    shell: Arc<tenancy_support::TenancyShell>,
    /// The tenant charged for the permit.
    tenant: Tenant,
}

#[cfg(feature = "script")]
impl Lease {
    /// A lease for a supervisor with no policy: dropping it returns the permit
    /// to the pool, exactly as the raw permit did.
    fn plain(permit: OwnedSemaphorePermit) -> Self {
        Self {
            permit: Some(permit),
            charge: None,
        }
    }

    /// A lease charged to `tenant`, so its drop returns the permit to the round
    /// rather than to the pool.
    fn tenanted(
        permit: OwnedSemaphorePermit,
        shell: Arc<tenancy_support::TenancyShell>,
        tenant: Tenant,
    ) -> Self {
        Self {
            permit: Some(permit),
            charge: Some(TenantCharge { shell, tenant }),
        }
    }
}

/// The plain constructor for a `sync`-only build, where there is no tenancy and
/// the lease is the permit and nothing else.
impl Lease {
    /// A lease for a supervisor with no policy.
    #[cfg(not(feature = "script"))]
    fn plain(permit: OwnedSemaphorePermit) -> Self {
        Self { _permit: permit }
    }
}

/// Only a build with tenancy has a round to hand a permit to. Without `script`
/// there is no `Drop` at all: the lease is its `permit` field, and that field's
/// own drop returns the permit to the pool.
#[cfg(feature = "script")]
impl Drop for Lease {
    /// Hand a tenanted lease's permit to the round, which decides whose waiter
    /// it serves next. An untenanted lease does nothing here: its `permit` field
    /// drops with it and the permit returns to the pool.
    fn drop(&mut self) {
        if let Some(charge) = self.charge.take()
            && let Some(permit) = self.permit.take()
        {
            charge.shell.release(&charge.tenant, permit);
        }
    }
}

/// The tenant a call that named none is charged to.
///
/// One reserved name, so the untenanted calls share a ceiling and a queue rather
/// than reaching every tenant's permit — which is what "untenanted calls behave
/// as one implicit tenant" has to mean for the accounting to be readable. The
/// leading underscore marks it as the crate's own rather than any caller's.
///
/// Only consulted when a [`TenancyPolicy`] is installed: without one there is no
/// per-tenant state at all and every call shares the single pool, exactly as it
/// always did.
#[cfg(feature = "script")]
const IMPLICIT_TENANT: &str = "_implicit";

/// The tenant an untenanted call is charged to.
#[cfg(feature = "script")]
fn implicit_tenant() -> Tenant {
    Tenant::assumed(IMPLICIT_TENANT)
}

/// The async half of per-tenant admission: one lock over the pure round
/// scheduler, plus the parked waiters it hands permits to.
///
/// Beside [`Supervisor`] rather than inside [`rt::tenancy`](super::tenancy)
/// because it needs the supervisor's own permit pool and cancellation token,
/// and passing those across a module boundary would make them `pub(crate)` for a
/// type that already owns them privately. The scheduling decision itself lives
/// in [`rt::tenancy`](super::tenancy) and is untouched by any of this.
#[cfg(feature = "script")]
mod tenancy_support {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::task::{Context, Poll, Waker};
    use std::time::Instant;

    use crate::rt::sync::{OwnedSemaphorePermit, Semaphore};

    use super::Lease;
    use crate::rt::tenancy::{Arrival, DeficitRoundRobin, Grant, GrantOutcome, TryArrival};
    use crate::script::Tenant;

    /// The round scheduler's lock, held for one decision.
    type RoundGuard<'shell> = std::sync::MutexGuard<'shell, DeficitRoundRobin<Arc<WaitSlot>>>;

    /// The permit pool and the round scheduler, with the two directions a
    /// permit travels: out of a completed task into the next eligible tenant's
    /// waiter.
    pub(super) struct TenancyShell {
        /// The supervisor's own pool. The tenancy layer never makes a permit; it
        /// decides which tenant an existing one belongs to.
        permits: Arc<Semaphore>,
        /// The pure round scheduler, and through it every parked waiter.
        round: std::sync::Mutex<DeficitRoundRobin<Arc<WaitSlot>>>,
        /// Cumulative wall time spent inside [`TenancyShell::admit`] and
        /// [`TenancyShell::try_admit`], in nanoseconds: the scheduler lock plus
        /// the round and the pump, which is #351's admission side of a
        /// neighbour's admission wait. Releases take the same lock from the
        /// completing task and are counted separately, so a submitter's own
        /// admission time reads without the completions landing beside it.
        admit_nanos: AtomicU64,
        /// How many arrival decisions `admit_nanos` covers.
        admit_ops: AtomicU64,
        /// Cumulative wall time spent inside [`TenancyShell::release`], in
        /// nanoseconds: the same lock and round, taken from the completing
        /// task rather than the submitter.
        release_nanos: AtomicU64,
        /// How many releases `release_nanos` covers.
        release_ops: AtomicU64,
    }

    /// One parked admission: where a waiter learns that a permit is coming.
    ///
    /// The state is behind its own mutex rather than the shell's, because the
    /// lock order is shell then slot: a permit is handed to a slot while the
    /// scheduler is held, and a waiter that goes away touches its slot alone
    /// and then, separately, the scheduler. Nested in the other order the two
    /// would deadlock.
    struct WaitSlot {
        /// The permit, the abandonment mark, the handover mark and the waker.
        state: std::sync::Mutex<SlotState>,
    }

    /// The fields behind a [`WaitSlot`].
    struct SlotState {
        /// The permit, once a grant has arrived.
        permit: Option<OwnedSemaphorePermit>,
        /// The owner went away before a permit arrived.
        abandoned: bool,
        /// A permit was handed over, so this slot is spent.
        handed: bool,
        /// The round took this waiter off its queue to deliver to it and found
        /// the owner gone. The waiter is no longer in any queue, so the owner's
        /// own report must not count it as an abandoned waiter there.
        withdrawn: bool,
        /// The task waiting for the handover.
        waker: Option<Waker>,
    }

    impl WaitSlot {
        /// A slot nobody is waiting on yet.
        fn fresh() -> Arc<Self> {
            Arc::new(Self {
                state: std::sync::Mutex::new(SlotState {
                    permit: None,
                    abandoned: false,
                    handed: false,
                    withdrawn: false,
                    waker: None,
                }),
            })
        }

        /// Take the lock. A poisoned lock still guards a consistent value: each
        /// critical section is a few plain writes that cannot be left half done,
        /// so a panic elsewhere is not a reason to strand a waiter.
        fn lock(&self) -> std::sync::MutexGuard<'_, SlotState> {
            crate::journal::owner::lock(&self.state)
        }

        /// Whether a grant may still be handed to this slot.
        ///
        /// The round scheduler asks this of every queue head it reaches and
        /// abandons the ones that answer no, so a waiter whose caller has gone
        /// costs one skip rather than a blocked pool.
        fn is_live(&self) -> bool {
            let state = self.lock();
            !state.abandoned && !state.handed
        }

        /// Hand `permit` to the waiter, or refuse it and return the permit.
        ///
        /// `Some(permit)` means the owner left between the round's liveness
        /// check and this handoff; the caller recycles the permit rather than
        /// dropping it, because a permit is capacity and capacity is what the
        /// next eligible tenant is waiting for. The slot is marked withdrawn in
        /// the same critical section, so the owner's report, which reaches the
        /// round only after the caller's pump releases it, knows the round
        /// already took this waiter off its queue.
        fn deliver(&self, permit: OwnedSemaphorePermit) -> Option<OwnedSemaphorePermit> {
            let mut state = self.lock();
            if state.abandoned {
                state.withdrawn = true;
                return Some(permit);
            }
            if state.handed {
                return Some(permit);
            }
            state.permit = Some(permit);
            state.handed = true;
            // The waker is taken rather than cloned and kept: it is woken once,
            // and a stale waker retained for a second wake would wake a task
            // that has already left.
            if let Some(waker) = state.waker.take() {
                waker.wake();
            }
            None
        }

        /// Take the permit, if one has arrived.
        fn take_permit(&self) -> Option<OwnedSemaphorePermit> {
            self.lock().permit.take()
        }

        /// The slot half of an owner's departure: collect a handed permit, or
        /// mark the slot abandoned so no grant can deliver to it.
        ///
        /// Taken under the slot lock alone. The round half,
        /// [`TenancyShell::report_abandoned`], runs after this lock is released,
        /// so the two critical sections nest only in the order the shell uses —
        /// and a pump that holds the round lock can find this slot abandoned
        /// between its liveness check and its delivery.
        fn leave(&self) -> Departure {
            let mut state = self.lock();
            if state.handed {
                return Departure::Handed(state.permit.take());
            }
            state.abandoned = true;
            Departure::Abandoned
        }

        /// Record the waiting task, keeping the one already registered.
        fn register(&self, waker: &Waker) {
            let mut state = self.lock();
            let stale = state
                .waker
                .as_ref()
                .is_none_or(|current| !current.will_wake(waker));
            if stale {
                state.waker = Some(waker.clone());
            }
        }
    }

    /// What an owner leaving its slot found there.
    enum Departure {
        /// A permit had been handed over; it is returned here unless the owner
        /// already collected it.
        Handed(Option<OwnedSemaphorePermit>),
        /// No permit had arrived. The slot is now abandoned, and the round has
        /// still to hear about it.
        Abandoned,
    }

    /// The future a parked admission awaits: the permit the round will hand it.
    ///
    /// The only way a queued waiter learns of its grant is being woken by
    /// [`WaitSlot::deliver`], which runs on whichever task released the permit —
    /// so a grant is delivered by the completion hook rather than polled for.
    /// The caller races this against its own cancellation token, which wakes on
    /// the token in turn; neither direction needs an interval.
    pub(super) struct WaitPermit {
        /// Where the grant is delivered.
        slot: Arc<WaitSlot>,
        /// The shell, for the release that a collected permit or an abandoned
        /// waiter owes.
        shell: Arc<TenancyShell>,
        /// The tenant this waiter belongs to.
        tenant: Tenant,
    }

    impl WaitPermit {
        /// A waiter for `tenant`, parked on `slot`.
        fn new(slot: Arc<WaitSlot>, shell: Arc<TenancyShell>, tenant: Tenant) -> Self {
            Self {
                slot,
                shell,
                tenant,
            }
        }
    }

    impl Future for WaitPermit {
        /// The permit, handed over by the round.
        type Output = OwnedSemaphorePermit;

        fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
            let this = self.get_mut();
            // Register before reading, never after: the round may hand the
            // permit over between the read and the registration, and a lost
            // wakeup here parks a waiter that has already been granted.
            this.slot.register(context.waker());
            match this.slot.take_permit() {
                Some(permit) => Poll::Ready(permit),
                None => Poll::Pending,
            }
        }
    }

    impl Drop for WaitPermit {
        /// Give back whatever this waiter was owed, or record that it is owed
        /// nothing.
        ///
        /// Two cases, and both owe the pool an exact answer. A waiter dropped
        /// *after* a permit was handed to it but before it was taken back
        /// returns the permit through the release path — the same path a
        /// completed task takes, because a grant nobody collects is capacity
        /// nobody else may reach. A waiter dropped with no permit is marked
        /// abandoned, which stops it counting against its tenant's queue bound
        /// and lets the next round skip it.
        ///
        /// The slot lock is released before the scheduler lock is taken, so the
        /// two critical sections nest only in the order the shell uses.
        fn drop(&mut self) {
            match self.slot.leave() {
                Departure::Handed(Some(permit)) => self.shell.release(&self.tenant, permit),
                Departure::Handed(None) => {}
                Departure::Abandoned => self.shell.report_abandoned(&self.slot, &self.tenant),
            }
        }
    }

    /// What the round decided for one arrival, before the caller's lease or
    /// waiter is built around it.
    enum Decided {
        /// Admitted with this permit.
        Admit(OwnedSemaphorePermit),
        /// Parked on this slot.
        Park(Arc<WaitSlot>),
        /// Refused; the admission already says why.
        Refuse(Admission),
    }

    /// What one admission attempt decided.
    pub(super) enum Admission {
        /// Admitted at once; the caller owns the lease.
        Admitted(Lease),
        /// Parked; the caller owns the waiter and must await it.
        Queued(WaitPermit),
        /// Not admitted: the tenant's bounded queue is at its bound, or nothing
        /// could take a permit right now. `limit` is the tenant's declared queue
        /// bound in both cases, so the caller can name it.
        Refused {
            /// The bound the tenant's queue reached.
            limit: usize,
        },
        /// Not admitted: the supervisor's own waiting bound is reached across
        /// every tenant, so the tenant may still have room and is refused
        /// anyway. The load is the supervisor's, not this tenant's.
        SupervisorQueueFull {
            /// The bound the supervisor's waiting total reached.
            limit: usize,
        },
    }

    impl TenancyShell {
        /// A shell over `permits`, enforcing `policy`.
        pub(super) fn new(permits: Arc<Semaphore>, policy: super::TenancyPolicy) -> Self {
            Self {
                permits,
                round: std::sync::Mutex::new(DeficitRoundRobin::new(policy)),
                admit_nanos: AtomicU64::new(0),
                admit_ops: AtomicU64::new(0),
                release_nanos: AtomicU64::new(0),
                release_ops: AtomicU64::new(0),
            }
        }

        /// Run one decision under the round lock, charging its own work to
        /// `ops` and `nanos` and not the wait to take the lock.
        ///
        /// `decide` runs with the lock held and is charged, through the unlock.
        /// The wait to take the lock is not: it is another decision holding the
        /// round — a completing task's release, charged to that release's own
        /// counter — and on an oversubscribed host it is mostly the time that
        /// holder spent descheduled. Charging it here made one host stall read
        /// as a 114x arrival cost (main run 37933518033), which is the
        /// scheduling share INV-BOT-151 does not claim to isolate.
        ///
        /// The two reads of the clock cost tens of nanoseconds and are
        /// symmetric across tenants, so a comparison of two arms' counters
        /// measures the decisions rather than the clock.
        fn timed<T>(
            &self,
            ops: &AtomicU64,
            nanos: &AtomicU64,
            decide: impl FnOnce(&mut RoundGuard<'_>) -> T,
        ) -> T {
            let mut round = self.lock();
            let held = Instant::now();
            let outcome = decide(&mut round);
            drop(round);
            let charged = held.elapsed();
            // A saturating read: the only input that does not fit is a decision
            // longer than 584 years, which pins the counter rather than wrapping
            // it, the same way the crate's counters saturate rather than wrap.
            let mut elapsed = u64::MAX;
            if let Ok(fits) = u64::try_from(charged.as_nanos()) {
                elapsed = fits;
            }
            ops.fetch_add(1, Ordering::Relaxed);
            nanos.fetch_add(elapsed, Ordering::Relaxed);
            outcome
        }

        /// Take the lock. A poisoned lock still guards a consistent value: every
        /// critical section is a handful of plain writes over queues and
        /// counters, so a panic elsewhere is not a reason to strand permits.
        fn lock(&self) -> RoundGuard<'_> {
            crate::journal::owner::lock(&self.round)
        }

        /// One permit from the pool, if the pool has one free.
        fn take_from_pool(&self) -> Option<OwnedSemaphorePermit> {
            Arc::clone(&self.permits).try_acquire_owned().ok()
        }

        /// A lease charged to `tenant`, so its drop returns the permit to the
        /// round rather than to the pool.
        fn lease(self: &Arc<Self>, permit: OwnedSemaphorePermit, tenant: &Tenant) -> Lease {
            Lease::tenanted(permit, Arc::clone(self), tenant.clone())
        }

        /// Admit one arrival for `tenant`, park it, or refuse it.
        ///
        /// Every step that could change the answer happens under the scheduler
        /// lock, so an arrival cannot observe a half-completed round. The
        /// receiver is `self: &Arc<Self>` because an admitted lease has to keep
        /// the shell alive for as long as it holds the permit.
        pub(super) fn admit(self: &Arc<Self>, tenant: &Tenant) -> Admission {
            let decided = self.timed(&self.admit_ops, &self.admit_nanos, |round| {
                // The slot is built only if the arrival parks: an arrival
                // admitted at once or refused allocates nothing (#375).
                let mut parked = None;
                let park = || {
                    let slot = WaitSlot::fresh();
                    parked = Some(Arc::clone(&slot));
                    slot
                };
                let outcome = round.arrive_with(tenant, park, || self.take_from_pool());
                match (outcome, parked) {
                    (Arrival::Immediate(permit), _) => Decided::Admit(permit),
                    (Arrival::Queued, Some(slot)) => {
                        // An arrival that could not take a permit may still be
                        // servable right now — the ring may have drained between its
                        // check and this one — so the round is pumped before the
                        // caller parks. A pump that grants to this very waiter wakes
                        // it, and the caller's poll collects the permit.
                        self.pump(round, None);
                        Decided::Park(slot)
                    }
                    // `arrive_with` queues only by calling `park`, so a queued
                    // arrival always has its slot. Answered rather than
                    // asserted: with no slot nothing was queued either, and the
                    // caller is told the queue is full instead of parking on a
                    // waiter no grant can reach.
                    (Arrival::Queued, None) => Decided::Refuse(Admission::Refused {
                        limit: round.policy().queue_per_tenant(),
                    }),
                    (Arrival::Refused { limit }, _) => {
                        Decided::Refuse(Admission::Refused { limit })
                    }
                    (Arrival::SupervisorFull { limit }, _) => {
                        Decided::Refuse(Admission::SupervisorQueueFull { limit })
                    }
                }
            });
            self.hand_over(decided, tenant)
        }

        /// Admit one arrival for `tenant` that refuses rather than waits.
        ///
        /// The non-blocking door: no tenant is added to any queue, so a caller
        /// that keeps hammering a full supervisor accumulates no waiters. The
        /// refusal path has to cost nothing as well as say so.
        pub(super) fn try_admit(self: &Arc<Self>, tenant: &Tenant) -> Admission {
            let decided = self.timed(&self.admit_ops, &self.admit_nanos, |round| {
                match round.try_arrive(tenant, || self.take_from_pool()) {
                    TryArrival::Immediate(permit) => Decided::Admit(permit),
                    TryArrival::Contended => Decided::Refuse(Admission::Refused {
                        limit: round.policy().queue_per_tenant(),
                    }),
                }
            });
            self.hand_over(decided, tenant)
        }

        /// Wrap a decision in what the caller owns, after the round lock is
        /// released.
        ///
        /// A lease and a waiter each clone the shell's and the tenant's `Arc`,
        /// and every lease a completing task drops releases them again on
        /// another core. Built under the lock, those counter updates were
        /// cache-line transfers inside the critical section every release and
        /// every arrival waits on, and a flood multiplied them (#375). Nothing
        /// in them reads the round, so they are built outside it.
        fn hand_over(self: &Arc<Self>, decided: Decided, tenant: &Tenant) -> Admission {
            match decided {
                Decided::Admit(permit) => Admission::Admitted(self.lease(permit, tenant)),
                Decided::Park(slot) => {
                    Admission::Queued(WaitPermit::new(slot, Arc::clone(self), tenant.clone()))
                }
                Decided::Refuse(refusal) => refusal,
            }
        }

        /// One of this supervisor's in-flight admissions ended, carrying its
        /// permit back into the round.
        ///
        /// The one place a permit is handed to another tenant: the completed
        /// task's own lease calls it, so a task that ends hands its capacity
        /// straight to the next eligible waiter rather than returning it to a
        /// pool for the next arrival to win.
        pub(super) fn release(&self, tenant: &Tenant, permit: OwnedSemaphorePermit) {
            self.timed(&self.release_ops, &self.release_nanos, |round| {
                round.note_release(tenant);
                self.pump(round, Some(permit));
            });
        }

        /// Hand permits to eligible tenants until none can take one.
        ///
        /// `spare` is a permit already off the pool — a completed task's — and
        /// the loop reaches for another only when the round says a queued tenant
        /// could take it. A contended pool is therefore drained exactly as far
        /// as the queued work justifies and no further.
        fn pump(&self, round: &mut RoundGuard<'_>, spare: Option<OwnedSemaphorePermit>) {
            let mut spare = spare;
            loop {
                let permit = match spare.take() {
                    Some(permit) => permit,
                    None if round.has_eligible() => match self.take_from_pool() {
                        Some(permit) => permit,
                        None => return,
                    },
                    None => return,
                };
                let mut is_live = |slot: &Arc<WaitSlot>| slot.is_live();
                match round.grant(permit, &mut is_live) {
                    GrantOutcome::Idle(permit) => {
                        // Nobody could take it. Dropping the permit returns it to
                        // the pool, which is what it was before this release, and
                        // the loop stops because an idle round says no tenant is
                        // waiting under a ceiling.
                        drop(permit);
                        return;
                    }
                    GrantOutcome::Granted(grant) => spare = Self::settle(round, grant),
                }
            }
        }

        /// Deliver one grant, or recycle its permit when the owner has left.
        ///
        /// Returns the permit when the delivery failed: the owner left between
        /// the round's liveness check and this handoff. The admission the grant
        /// charged is charged back, and the permit recycles into the same pump,
        /// so a grant that lost its waiter still reaches a live one.
        ///
        /// The round's abandoned count is not touched here. The owner has not
        /// reported yet — its report needs the round lock this pump holds — and
        /// when it does, it finds its slot withdrawn and reports nothing,
        /// because the waiter it would count is no longer in the queue.
        /// Correcting the count here instead would subtract before the owner
        /// added, and a count that saturates at zero loses the subtraction.
        fn settle(
            round: &mut RoundGuard<'_>,
            grant: Grant<Arc<WaitSlot>, OwnedSemaphorePermit>,
        ) -> Option<OwnedSemaphorePermit> {
            let returned = grant.waiter.deliver(grant.permit)?;
            round.note_release(&grant.tenant);
            Some(returned)
        }

        /// The round half of an owner's departure without a permit.
        ///
        /// The compaction inside `note_abandoned` needs to know which waiters are
        /// still owned, and the answer lives on each slot. Called after the slot
        /// lock is released, so the round lock is taken first and the slot locks
        /// inside it, in the order the pump uses.
        ///
        /// A withdrawn slot reports nothing: a pump already took its waiter off
        /// the queue and found it gone, and `note_abandoned` counts waiters that
        /// are still in a queue. The flag is read under the round lock, after
        /// any pump that could withdraw the slot has finished.
        fn report_abandoned(&self, slot: &WaitSlot, tenant: &Tenant) {
            let mut round = self.lock();
            if slot.lock().withdrawn {
                return;
            }
            let mut is_live = |waiter: &Arc<WaitSlot>| waiter.is_live();
            round.note_abandoned(tenant, &mut is_live);
        }

        /// How many admissions `tenant` holds, and how many it is waiting for.
        ///
        /// Read from the scheduler's own fields, so the report cannot disagree
        /// with the admission that produced it (INV-BOT-31's rule, applied to
        /// the per-tenant axis).
        pub(super) fn counts_of(&self, tenant: &Tenant) -> (usize, usize) {
            let round = self.lock();
            (round.in_flight_of(tenant), round.queued_of(tenant))
        }

        /// Cumulative admission-side decisions and their wall time: `(admit_ops,
        /// admit_nanos)` over [`TenancyShell::admit`] and
        /// [`TenancyShell::try_admit`], `(release_ops, release_nanos)` over
        /// [`TenancyShell::release`], all in nanoseconds.
        ///
        /// Read from the counters [`TenancyShell::timed`] charges, so the report
        /// cannot disagree with the decisions it measured (INV-BOT-31's rule,
        /// applied to the admission-cost axis).
        pub(super) fn decision_cost(&self) -> (u64, u64, u64, u64) {
            (
                self.admit_ops.load(Ordering::Relaxed),
                self.admit_nanos.load(Ordering::Relaxed),
                self.release_ops.load(Ordering::Relaxed),
                self.release_nanos.load(Ordering::Relaxed),
            )
        }

        /// How many of `tenant`'s waiting admissions this shell still holds, live or
        /// abandoned. Read from the round's own deque, so a test can see the
        /// retention rather than infer it from the live count.
        ///
        /// A test seam rather than production API: the report a caller needs is
        /// [`counts_of`](Self::counts_of), and the retained-but-abandoned count
        /// is a fact about callers that walked away rather than about capacity.
        #[cfg(test)]
        pub(super) fn retained_waiters(&self, tenant: &Tenant) -> usize {
            self.lock().retained_of(tenant)
        }

        /// How many tenants hold at least one waiter.
        pub(super) fn tenants_waiting(&self) -> usize {
            self.lock().tenants_with_queues()
        }
    }

    #[cfg(test)]
    mod tests {
        use std::error::Error;
        use std::sync::Arc;

        use crate::rt::sync::Semaphore;

        use super::{Departure, TenancyShell, WaitSlot};
        use crate::rt::tenancy::{Arrival, GrantOutcome, TenancyPolicy};
        use crate::script::Tenant;

        /// An owner that walks away between the round's choice and the delivery
        /// leaves its tenant's live count exact.
        ///
        /// The interleaving is the one the locks force, driven step by step
        /// through the shell's own functions: the pump holds the round lock from
        /// its grant to its delivery, so an owner leaving in that window runs
        /// its slot half first, the pump's delivery fails, and the owner's round
        /// half runs only once the pump lets go. The tenant still has three
        /// owners waiting behind the one that left, and its live count must say
        /// so. Three, because with fewer the miscount is large enough to trigger
        /// the round's compaction, which recounts the queue and hides it.
        #[test]
        fn a_late_abandonment_leaves_the_live_count_exact() -> Result<(), Box<dyn Error>> {
            let shell = TenancyShell::new(Arc::new(Semaphore::new(0)), TenancyPolicy::new(4, 4));
            let tenant = Tenant::new("late")?;
            let leaving = WaitSlot::fresh();
            let staying = [WaitSlot::fresh(), WaitSlot::fresh(), WaitSlot::fresh()];
            {
                let mut round = shell.lock();
                for slot in std::iter::once(&leaving).chain(&staying) {
                    let arrival = round.arrive(&tenant, Arc::clone(slot), || None::<()>);
                    assert!(
                        matches!(arrival, Arrival::Queued),
                        "a spent pool queues every arrival"
                    );
                }
            }
            let spare = Arc::new(Semaphore::new(1)).try_acquire_owned()?;
            let mut round = shell.lock();
            let mut is_live = |slot: &Arc<WaitSlot>| slot.is_live();
            let grant = match round.grant(spare, &mut is_live) {
                GrantOutcome::Granted(grant) => Some(grant),
                GrantOutcome::Idle(_) => None,
            }
            .ok_or("a queued waiter takes the permit")?;
            assert!(
                Arc::ptr_eq(&grant.waiter, &leaving),
                "the round chose the oldest waiter"
            );
            assert!(
                matches!(leaving.leave(), Departure::Abandoned),
                "the owner leaves before any permit reached it"
            );
            let recycled = TenancyShell::settle(&mut round, grant);
            drop(round);
            shell.report_abandoned(&leaving, &tenant);

            assert!(
                recycled.is_some(),
                "the failed delivery recycled its permit"
            );
            let (in_flight, queued) = shell.counts_of(&tenant);
            assert_eq!(
                in_flight, 0,
                "the failed delivery charged its admission back"
            );
            assert_eq!(
                queued,
                staying.len(),
                "three owners are still waiting, and the live count must say so"
            );
            assert_eq!(
                shell.retained_waiters(&tenant),
                staying.len(),
                "the round retains exactly the waiter still owned"
            );
            Ok(())
        }

        /// Every admit, non-blocking admit and release charges exactly one
        /// admission-side decision, so the counters a neighbour's split is read
        /// from count the decisions the round actually took.
        #[test]
        fn each_admission_path_charges_one_decision() -> Result<(), Box<dyn Error>> {
            use super::Admission;

            let shell = Arc::new(TenancyShell::new(
                Arc::new(Semaphore::new(1)),
                TenancyPolicy::new(1, 4),
            ));
            let tenant = Tenant::new("metered")?;
            assert_eq!(
                shell.decision_cost().0,
                0,
                "a fresh shell has taken no decisions"
            );
            let admitted = match shell.admit(&tenant) {
                Admission::Admitted(lease) => Some(lease),
                Admission::Queued(_)
                | Admission::Refused { .. }
                | Admission::SupervisorQueueFull { .. } => None,
            };
            let lease = admitted.ok_or("the pool's one permit admits at once")?;
            let (arrivals, _, releases, _) = shell.decision_cost();
            assert_eq!(arrivals, 1, "one arrival is one arrival decision");
            assert_eq!(releases, 0, "nothing has been released yet");
            drop(lease);
            let (arrivals, admit_nanos, releases, release_nanos) = shell.decision_cost();
            assert_eq!(arrivals, 1, "the release takes no arrival decision");
            assert_eq!(releases, 1, "the lease's release is the release decision");
            assert!(
                admit_nanos > 0 && release_nanos > 0,
                "both halves charged the time they held the lock"
            );
            Ok(())
        }
    }
}

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
#[cfg(all(unix, feature = "process"))]
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
    /// The same supervisor as [`Supervisor::new`] with one difference: the clock
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
    /// The additive 1.x form of [`Supervisor::new`]: the same supervisor, the
    /// same report stream, the same terminal outcomes, plus the per-tenant
    /// ceiling, queue bound and weights a [`TenancyPolicy`] declares.
    ///
    /// # What changes for a caller that installs one
    ///
    /// - **The untenanted calls are one implicit tenant.**
    ///   [`Supervisor::spawn`], [`Supervisor::try_spawn`],
    ///   [`Supervisor::spawn_repeating`], `Supervisor::spawn_process` and
    ///   `Supervisor::run_process` are charged to one reserved tenant, so a
    ///   caller that predates tenancy shares that tenant's ceiling with every
    ///   other untenanted caller instead of reaching every tenant's permit. No
    ///   signature changes, and the one behavioural difference is a *visible*
    ///   refusal rather than an unbounded wait: a full implicit-tenant queue
    ///   refuses the call and counts it in [`Stats::refused`], where an
    ///   untenanted supervisor without tenancy still waits.
    /// - **The tenant-scoped calls answer with a typed refusal.**
    ///   [`Supervisor::spawn_for`] and `Supervisor::run_process_for` name the
    ///   tenant that is at capacity and the bound it reached, so a caller can
    ///   tell one loud tenant from a stopped supervisor.
    ///
    /// A supervisor built without a policy takes the path it always took: the
    /// same semaphore, the same waits, the same refusals. That is what makes this
    /// additive rather than a behaviour change for existing callers.
    ///
    /// ```
    /// use lgwks_bot::rt::supervise::Supervisor;
    /// use lgwks_bot::rt::tenancy::TenancyPolicy;
    /// use lgwks_bot::script::Tenant;
    ///
    /// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// let quiet = Tenant::new("quiet")?;
    /// // 64 permits in total, 32 per tenant: one tenant can strand at most 32
    /// // of them, so 32 stay reachable for everyone else however loud it is.
    /// let mut supervisor = Supervisor::with_tenancy(64, TenancyPolicy::new(32, 1_024));
    /// supervisor
    ///     .spawn_for(&quiet, |_token| async { /* one unit of work */ })
    ///     .await?;
    /// let report = supervisor.shutdown().await;
    /// assert!(report.is_clean(), "the quiet tenant's task ran to completion");
    /// # Ok(())
    /// # }
    /// ```
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
    fn assembled(bound: usize, permits: Arc<Semaphore>, clock: Clock) -> Self {
        Self {
            token: CancellationToken::new(),
            set: JoinSet::new(),
            permits,
            #[cfg(all(unix, feature = "process"))]
            cleanup_owners: Arc::new(CleanupOwners::default()),
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
    #[cfg(all(unix, feature = "process"))]
    fn prepare_process(&self, spec: &ProcessSpec) -> (CancellationToken, RunBounds) {
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
    /// drained, is [`Supervisor::shutdown`].
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
    /// same fields [`Supervisor::stats`] and the permit pool are built from — so
    /// what it reports and what the supervisor does cannot drift apart. It
    /// allocates a `Vec` proportional to the in-flight ceiling and retains
    /// nothing afterwards.
    ///
    /// # What it deliberately does not carry
    ///
    /// Terminal outcomes stay in the report stream ([`Supervisor::next_report`]),
    /// where the retention cap already governs them. A snapshot with its own copy
    /// of them would be a second ledger, and two ledgers disagree.
    ///
    /// ```
    /// # use lgwks_bot::rt::supervise::Supervisor;
    /// # async fn example() {
    /// let mut supervisor = Supervisor::new(2);
    /// let snapshot = supervisor.snapshot();
    /// assert_eq!(snapshot.max_in_flight(), 2);
    /// assert_eq!(snapshot.free(), 2, "a fresh supervisor holds both permits");
    /// assert_eq!(snapshot.next_action(), supervisor::NextAction::Admit);
    /// # }
    /// # mod supervisor { pub use lgwks_bot::rt::supervise::NextAction; }
    /// ```
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

    /// The oldest terminal outcome this supervisor has not yet handed over.
    ///
    /// `None` means every task that has ended has been reported and read. A
    /// caller that reads this in a loop holds no history at all, which is how
    /// the cap in [`Stats::reports_dropped`] is kept from ever being reached.
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
    /// Every joined task produces exactly one [`TaskOutcome`], readable through
    /// [`Supervisor::next_report`].
    pub fn reap(&mut self) -> usize {
        self.reap_at_most(usize::MAX)
    }

    /// Join at most `limit` already-finished tasks, returning how many were
    /// joined.
    ///
    /// The bounded form a tenanted admission takes (`REAP_PER_ADMISSION`), so
    /// one call's bookkeeping is a constant rather than every task any tenant
    /// finished since the last call.
    fn reap_at_most(&mut self, limit: usize) -> usize {
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
    /// The awaiting counterpart of [`Supervisor::reap`]: `reap` joins what has
    /// already finished, so a caller that wanted to wait through it had to poll
    /// it on a timer, and the runtime's timer rounds a short sleep up to its
    /// one-millisecond tick — a fixed cost on every wait that the task set's own
    /// completion wakeup does not have (#269). Each task is joined the moment it
    /// ends and absorbed exactly as `reap` absorbs it, so every one produces one
    /// [`TaskOutcome`] under the same retention cap.
    ///
    /// Unlike [`Supervisor::shutdown`] nothing is cancelled and the supervisor
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
    /// The non-blocking counterpart to [`Supervisor::spawn`]. A refusal is a
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
    /// The tenant-scoped counterpart of [`Supervisor::spawn`], and the only
    /// door that *charges* a tenant: the admission this call waits for counts
    /// against `tenant`'s own ceiling under the supervisor's [`TenancyPolicy`],
    /// and the slot it is given was offered by the deficit round robin because
    /// `tenant` had work waiting when a permit freed. That is the whole
    /// contract — one tenant cannot hold more than its share of the supervisor,
    /// so it cannot starve another.
    ///
    /// `body` is called **after** the admission is granted and before the task
    /// is placed, exactly as [`Supervisor::spawn`] calls it, so it need not be
    /// `Send` or `'static` itself; only the future it returns is.
    ///
    /// # Errors
    ///
    /// [`SpawnRefused::TenantAtCapacity`] when `tenant` already holds its full
    /// queue of waiting admissions, naming the tenant and the bound;
    /// [`SpawnRefused::Cancelled`] once the supervisor has stopped admitting.
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
        self.tenancy
            .as_ref()
            .map_or((0, 0), |shell| shell.counts_of(tenant))
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
    /// ```
    /// use lgwks_bot::rt::supervise::Supervisor;
    /// use lgwks_bot::rt::tenancy::TenancyPolicy;
    /// use lgwks_bot::script::Tenant;
    ///
    /// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// let quiet = Tenant::new("quiet")?;
    /// let mut supervisor = Supervisor::with_tenancy(4, TenancyPolicy::new(2, 8));
    /// supervisor
    ///     .spawn_for(&quiet, |_token| async { /* one unit of work */ })
    ///     .await?;
    /// let (arrivals, _, _, _) = supervisor.admission_cost();
    /// assert!(arrivals >= 1, "one admission took at least one arrival decision");
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "script")]
    #[must_use]
    pub fn admission_cost(&self) -> (u64, u64, u64, u64) {
        self.tenancy
            .as_ref()
            .map_or((0, 0, 0, 0), |shell| shell.decision_cost())
    }

    /// How many tenants currently hold at least one waiting admission.
    ///
    /// The load the round is carrying, for a report or an assertion about
    /// whether a fleet of tenants is actually backed up. Zero for an untenanted
    /// supervisor, which has no per-tenant queues at all.
    #[cfg(feature = "script")]
    #[must_use]
    pub fn tenants_waiting(&self) -> usize {
        self.tenancy
            .as_ref()
            .map_or(0, |shell| shell.tenants_waiting())
    }

    /// Start a task that repeats under `budget` until the budget or the
    /// supervisor ends it.
    ///
    /// This is the shape a long-running bot loop should take. The bound is a
    /// required argument, so there is no call that reads as "loop forever".
    ///
    /// The loop's [`Outcome`] is not discarded: a loop the supervisor cancelled
    /// is reported as [`TaskOutcome::Cancelled`] and one that spent its budget
    /// as [`TaskOutcome::Completed`], so the two are distinguishable from
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

    /// Start `command` as a supervised child process.
    ///
    /// This is the only way this crate starts a process, and it behaves exactly
    /// like [`Supervisor::spawn`] where it matters — it awaits a free slot
    /// before starting anything, it returns no handle, and the process it
    /// starts is reported like any other task:
    ///
    /// - **Bounded.** The permit is acquired before the spawn, so a supervisor
    ///   already at its ceiling does not start the process until a slot frees.
    ///   Nothing is queued on this module's behalf.
    /// - **Killed on drop, as a whole group.** The supervisor's token reaches
    ///   the task, and the task kills the child's **process group** rather than
    ///   the child alone: a shell started here can spawn a pipeline, and
    ///   `Child::kill` would leave the grandchildren running with nobody
    ///   holding their handles. The same kill lands if the task is aborted,
    ///   because the guard that performs it lives in the task's own frame.
    /// - **Reported.** A process that exited zero is [`TaskOutcome::Completed`];
    ///   one that exited non-zero, was killed by a signal, or whose status
    ///   could not be read is [`TaskOutcome::Failed`], and carries its
    ///   [`ExitStatus`] when there is one; one this supervisor stopped because
    ///   it was cancelled is [`TaskOutcome::Cancelled`]. [`Stats::failed`]
    ///   counts the middle case, so a bot running a failing command is not
    ///   reported as a healthy one.
    ///
    /// # The handle this does not return
    ///
    /// The [`TaskId`] is not a handle. It cannot be joined, aborted, or waited
    /// on; it exists so a caller can match a terminal report to the command it
    /// started, which is the only thing a caller needs a running process to
    /// give back. A caller that wants the child's output redirects that stream
    /// into a sink it already owns — the supervisor owns the process, not the
    /// data.
    ///
    /// # Why the description is borrowed
    ///
    /// The description is data, not a running handle. Borrowing it lets the
    /// supervisor build the private engine command while the caller retains the
    /// auditable specification:
    ///
    /// ```no_run
    /// # use lgwks_bot::rt::process::ProcessSpec;
    /// # use lgwks_bot::rt::supervise::Supervisor;
    /// # async fn run() -> std::io::Result<()> {
    /// # let mut supervisor = Supervisor::default();
    /// let mut spec = ProcessSpec::new("sh");
    /// spec.arg("-c").arg("exit 0");
    /// supervisor.spawn_process(&spec).await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// The caller may retain and clone the description, but cannot start a child
    /// or obtain a child handle from it.
    ///
    /// # Errors
    ///
    /// An [`io::Error`] from the engine when the process could not be started
    /// at all: the program does not exist, is not executable, or the OS refused
    /// the fork. A failure to *start* is returned rather than turned into an
    /// outcome, because nothing was started and the caller is the one who knows
    /// what that means. The unreachable case — this module never closes its own
    /// semaphore — is returned too rather than asserted, since a
    /// `forbid`-level lint rules out the panic and a silent `return` would
    /// swallow the condition. A refused start does not consume a slot: the
    /// permit is released when this function returns.
    #[cfg(all(unix, feature = "process"))]
    pub async fn spawn_process(&mut self, spec: &ProcessSpec) -> io::Result<TaskId> {
        let (task, ()) = self.spawn_process_reading(spec, |_| Ok(())).await?;
        Ok(task)
    }

    /// Start `spec` exactly as [`Supervisor::spawn_process`] does, and also
    /// return the identity of the process that leads its group.
    ///
    /// The identity — the leader's pid and the instant the OS records it started
    /// — is what a *successor* needs when this supervisor is killed with SIGKILL
    /// and runs no cleanup: the child's group outlives it, and a later process
    /// that stored the identity hands it to
    /// [`reap_orphaned_group`](crate::rt::process::reap_orphaned_group), which
    /// stops the group only while the pid still names this leader. Store it
    /// durably (a database row) before relying on it; this method does not.
    ///
    /// The identity is read after the child starts and before the supervisor
    /// can reap it, so the pid cannot have been reissued yet. On Linux the read
    /// is two `/proc` files; elsewhere it runs `ps` once on the calling thread,
    /// which is why it is a separate door: [`Supervisor::spawn_process`] does
    /// not pay for it.
    ///
    /// # Errors
    ///
    /// Every error [`Supervisor::spawn_process`] returns, and the read's own
    /// error when the leader's identity could not be read. A spawn whose
    /// identity is unknown is refused rather than returned half-recorded: its
    /// group is killed by the same guard that kills an aborted task's group, so
    /// no unrecorded group is left running.
    #[cfg(all(unix, feature = "process"))]
    pub async fn spawn_process_identified(
        &mut self,
        spec: &ProcessSpec,
    ) -> io::Result<IdentifiedSpawn> {
        let (task, leader) = self.spawn_process_reading(spec, identify_leader).await?;
        Ok(IdentifiedSpawn { task, leader })
    }

    /// The one admission, start and placement both process spawns share.
    ///
    /// `read` sees the leader's pid while the child is started, guarded and not
    /// yet reaped, and what it returns travels back beside the task id. A
    /// refusal from it drops the guards built so far, which kill the group.
    #[cfg(all(unix, feature = "process"))]
    async fn spawn_process_reading<T>(
        &mut self,
        spec: &ProcessSpec,
        read: impl FnOnce(i32) -> io::Result<T>,
    ) -> io::Result<(TaskId, T)> {
        let Some(permit) = self.claim().await else {
            // The fence inside `claim` answered for capacity and for
            // cancellation alike; name which world refused, so a cancelled
            // supervisor's caller does not read this as a platform failure.
            if self.token.is_cancelled() {
                let refusal = Err(io::Error::other(SupervisorCancelled));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "spawn_process: returning an error to the caller");
                return refusal;
            }
            let refusal = Err(io::Error::other(
                "lgwks_bot: the supervisor's in-flight semaphore was closed",
            ));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "spawn_process: returning an error to the caller");
            return refusal;
        };
        // The recheck at the owned admission point, before the command is
        // built: a cancelled supervisor never forks, so the first instruction
        // of a refused command does not run. The permit drops here and the
        // refusal is counted.
        if self.token.is_cancelled() {
            self.refused = self.refused.saturating_add(1);
            let refusal = Err(io::Error::other(SupervisorCancelled));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "spawn_process: returning an error to the caller");
            return refusal;
        }
        let (token, bounds) = self.prepare_process(spec);
        let clock = self.clock.clone();
        let (child, group_id) = start(spec)?;
        // Construct the guard before the child is placed in the async task. If
        // the task is aborted before its first poll, this guard still owns the
        // cleanup fallback for a process that native spawning already started.
        let task = self.allocate_task_id();
        #[cfg(all(target_os = "linux", feature = "process"))]
        let scope = scope_tree(group_id);
        #[cfg(all(target_os = "linux", feature = "process"))]
        let group = ProcessGroup::of_scoped(
            group_id,
            task,
            permit,
            Arc::clone(&self.cleanup_owners),
            scope,
        );
        #[cfg(all(unix, not(target_os = "linux"), feature = "process"))]
        let group = ProcessGroup::of(group_id, task, permit, Arc::clone(&self.cleanup_owners));
        let live = LiveProcess::enter(&self.cleanup_owners);
        let read = read(group_id)?;
        let placed = self.place_owned(task, async move {
            let end = drive_process(&clock, child, &token, group, bounds).await;
            drop(live);
            task_end(end)
        });
        Ok((placed, read))
    }

    /// Run `spec` to completion and return its captured report.
    ///
    /// This is the result-bearing counterpart of [`Supervisor::spawn_process`],
    /// and it shares that method's ownership, deadline and cleanup machinery
    /// through the same private driver: the child is placed in its own process
    /// group, the deadline kills the **group** (INV-BOT-9), and the run reports
    /// the same [`CleanupReceipt`]. Where `spawn_process` hands back a
    /// [`TaskId`] and reports later, this awaits the child and returns what it
    /// observed.
    ///
    /// - **Bounded.** A slot is claimed before the fork, so a supervisor at its
    ///   ceiling waits for one rather than starting anyway. The permit is held
    ///   for the whole run and released when it ends.
    /// - **Captured.** A stream with [`StdioPolicy::Capture`](crate::rt::process::StdioPolicy::Capture)
    ///   is retained up to its ceiling and drained past it, and
    ///   [`ProcessRun`] reports the retained bytes, the exact total and whether
    ///   the stream was truncated.
    /// - **Truthful about a stop.** A deadline kill is reported through
    ///   [`ProcessRun::deadline_fired`], not as a normal exit, and its cleanup
    ///   receipt is the group's.
    /// - **Observable while it runs.** `on_line`, when given, is called once per
    ///   newline-terminated line of **stdout** as the child writes it, from the
    ///   same pipe read that retains the output. It is what makes a service's
    ///   readiness an observation of the child's output rather than a timer: the
    ///   caller's own readiness gate is closed on the line the child printed,
    ///   while the child is still running, and a child that never prints is
    ///   never released. The observer is called only for lines the capture read,
    ///   so a spec whose stdout is not [`StdioPolicy::Capture`](crate::rt::process::StdioPolicy::Capture)
    ///   observes nothing — the caller declares the capture, and this is the same
    ///   declaration. See [`Supervisor::run_process_observed`].
    ///
    /// # Errors
    ///
    /// [`ProcessRunError::Refused`] when the supervisor was cancelled before
    /// the fork, and [`ProcessRunError::NotStarted`] when the platform refused
    /// the program — both establish that nothing ran. A failure *after* the
    /// child started is [`ProcessRunError::AfterStart`], which establishes the
    /// opposite and is why the two are distinguishable.
    ///
    /// One method on every target: the Unix body is [`Self::run_process_observed`]
    /// and the non-Unix body is its typed pre-fork refusal, and choosing between
    /// them behind one signature is what keeps a caller from having to know which
    /// platform it compiled for before it can run a command.
    #[cfg(feature = "process")]
    pub async fn run_process(&mut self, spec: &ProcessSpec) -> Result<ProcessRun, ProcessRunError> {
        self.run_process_observed(spec, None).await
    }

    /// [`Supervisor::run_process`] with an observer on the child's stdout lines.
    ///
    /// The form a service's readiness takes. The observer is a `&dyn Fn` rather
    /// than a boxed closure because it lives only for the run's duration and
    /// never crosses a thread: it is called on the task driving this child, from
    /// inside the pipe read, between the child's write and this caller's next
    /// observation. A `None` observer is the same call with no observation, and
    /// is what [`Supervisor::run_process`] does.
    ///
    /// # Errors
    ///
    /// As [`Supervisor::run_process`]: the observer changes what the caller
    /// observes *while* the child runs, never what the run reports after it.
    #[cfg(all(unix, feature = "process"))]
    pub async fn run_process_observed(
        &mut self,
        spec: &ProcessSpec,
        on_line: Option<LineObserver<'_>>,
    ) -> Result<ProcessRun, ProcessRunError> {
        let Some(lease) = self.claim().await else {
            let refusal = Err(ProcessRunError::Refused);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "run_process_observed: returning an error to the caller");
            return refusal;
        };
        self.drive_run(lease, spec, on_line).await
    }

    /// Run `spec` to completion under `tenant`, charging the admission to that
    /// tenant, and return its captured report.
    ///
    /// The tenant-scoped counterpart of [`Supervisor::run_process`]: the same
    /// one process driver, the same ownership, deadline and cleanup, with the
    /// admission charged to `tenant` and a typed refusal when that tenant's
    /// bounded queue is full. The supervisor's global ceiling still bounds the
    /// total in-flight processes; `tenant`'s own share of them is what this pins.
    ///
    /// # Errors
    ///
    /// [`RunForError::Refused`] carrying [`SpawnRefused::TenantAtCapacity`] when
    /// `tenant`'s queue is at its bound, or [`SpawnRefused::Cancelled`] when the
    /// supervisor has stopped admitting — in both cases nothing ran. Otherwise
    /// [`RunForError::Run`] carrying exactly what [`Supervisor::run_process`]
    /// returns: [`ProcessRunError::NotStarted`] also establishes that nothing
    /// ran, and [`ProcessRunError::AfterStart`] the opposite.
    ///
    /// Two arms rather than one because "this tenant is at its bound" and "the
    /// command could not start" are different facts for the same caller: the
    /// first is the caller's own load to fix, the second is the platform's.
    #[cfg(all(unix, feature = "process", feature = "script"))]
    pub async fn run_process_for(
        &mut self,
        tenant: &Tenant,
        spec: &ProcessSpec,
    ) -> Result<ProcessRun, RunForError> {
        let Some(shell) = self.tenancy.clone() else {
            // No policy: this run is the implicit tenant's, admitted exactly as
            // `run_process` admits it.
            return self.run_process(spec).await.map_err(RunForError::Run);
        };
        self.reap();
        let lease = match self.claim_tenanted(&shell, tenant).await {
            Ok(lease) => lease,
            Err(refusal) => {
                self.refused = self.refused.saturating_add(1);
                let refused = Err(RunForError::Refused(refusal));
                lgwks_std::trace::debug!(error = ?refused.as_ref().err(), "run_process_for: returning an error to the caller");
                return refused;
            }
        };
        self.drive_run(lease, spec, None)
            .await
            .map_err(RunForError::Run)
    }

    /// The process driver every runner shares, from an admitted lease to a
    /// report.
    ///
    /// Split out of [`Supervisor::run_process_observed`] so the untenanted and
    /// the tenant-scoped runners are one body rather than two: a caller that
    /// named a tenant and a caller that did not fork the same child, cleaned up
    /// the same process group and reported the same receipt. The lease carries
    /// the permit — and, under a policy, the tenant it is charged to — through
    /// the whole run, so capacity comes back through the same door whether the
    /// call named a tenant or not.
    #[cfg(all(unix, feature = "process"))]
    async fn drive_run(
        &mut self,
        lease: Lease,
        spec: &ProcessSpec,
        on_line: Option<LineObserver<'_>>,
    ) -> Result<ProcessRun, ProcessRunError> {
        // The same owned admission point `spawn_process` rechecks: a cancelled
        // supervisor never forks, so the first instruction does not run.
        if self.token.is_cancelled() {
            self.refused = self.refused.saturating_add(1);
            let refusal = Err(ProcessRunError::Refused);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "drive_run: returning an error to the caller");
            return refusal;
        }
        let (token, bounds) = self.prepare_process(spec);
        let clock = self.clock.clone();
        let (child, group_id) = match start(spec) {
            Ok(started) => started,
            Err(source) => {
                let refusal = Err(ProcessRunError::NotStarted { source });
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "drive_run: returning an error to the caller");
                return refusal;
            }
        };
        let task = self.allocate_task_id();
        #[cfg(all(target_os = "linux", feature = "process"))]
        let scope = scope_tree(group_id);
        #[cfg(all(target_os = "linux", feature = "process"))]
        let group = ProcessGroup::of_scoped(
            group_id,
            task,
            lease,
            Arc::clone(&self.cleanup_owners),
            scope,
        );
        #[cfg(all(unix, not(target_os = "linux"), feature = "process"))]
        let group = ProcessGroup::of(group_id, task, lease, Arc::clone(&self.cleanup_owners));
        self.spawned = self.spawned.saturating_add(1);
        let end = drive_process_observed(&clock, child, &token, group, bounds, on_line).await;
        self.completed = self.completed.saturating_add(1);
        let deadline_fired = matches!(end.observation, ProcessObservation::Deadline);
        let settled = match end.observation {
            ProcessObservation::Exited => {
                if end.status.is_some_and(|status| status.success()) {
                    self.succeeded = self.succeeded.saturating_add(1);
                } else {
                    self.failed = self.failed.saturating_add(1);
                }
                true
            }
            ProcessObservation::Deadline | ProcessObservation::Unobservable => {
                self.failed = self.failed.saturating_add(1);
                true
            }
            // Unreachable in practice: the child token is a child of a
            // supervisor this call borrows exclusively, so nothing can cancel
            // it while the run is awaited. Reported rather than asserted, and
            // as permanently indeterminate rather than as a normal result.
            ProcessObservation::Cancelled => {
                self.cancelled = self.cancelled.saturating_add(1);
                false
            }
        };
        if !settled {
            let refusal = Err(ProcessRunError::AfterStart {
                source: io::Error::new(
                    io::ErrorKind::Interrupted,
                    "the supervisor was cancelled while the process ran",
                ),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "drive_run: returning an error to the caller");
            return refusal;
        }
        // A status is the command's *own* exit. A deadline kill reaps the group
        // and the engine then reports the signal, but that is the supervisor's
        // stop rather than the command's outcome, so it is not carried as a
        // status — the same reading `spawn_process` reports.
        let status = if matches!(end.observation, ProcessObservation::Exited) {
            end.status
        } else {
            None
        };
        Ok(ProcessRun {
            status,
            deadline_fired,
            stdout: end.stdout,
            stderr: end.stderr,
            cleanup: end.cleanup.receipt,
            containment: end.cleanup.containment,
        })
    }

    /// Process-group containment is unavailable on non-Unix targets.
    #[cfg(all(not(unix), feature = "process"))]
    pub async fn run_process_observed(
        &mut self,
        spec: &ProcessSpec,
        on_line: Option<LineObserver<'_>>,
    ) -> Result<ProcessRun, ProcessRunError> {
        let _ = (spec, on_line);
        Err(ProcessRunError::NotStarted {
            source: Self::no_process_group(),
        })
    }

    /// Process-group containment is unavailable on non-Unix targets, so no
    /// tenant's run ever starts and the admission is never charged.
    #[cfg(all(not(unix), feature = "process", feature = "script"))]
    pub async fn run_process_for(
        &mut self,
        tenant: &Tenant,
        spec: &ProcessSpec,
    ) -> Result<ProcessRun, RunForError> {
        let _ = (tenant, spec);
        let refusal = Err(RunForError::Run(ProcessRunError::NotStarted {
            source: Self::no_process_group(),
        }));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "run_process_for: returning an error to the caller");
        refusal
    }

    /// Process-group containment is unavailable on non-Unix targets.
    #[cfg(all(not(unix), feature = "process"))]
    pub async fn spawn_process(&mut self, spec: &ProcessSpec) -> io::Result<TaskId> {
        let _ = spec;
        Err(Self::no_process_group())
    }

    /// Process-group containment is unavailable on non-Unix targets.
    #[cfg(all(not(unix), feature = "process"))]
    pub async fn spawn_process_identified(
        &mut self,
        spec: &ProcessSpec,
    ) -> io::Result<IdentifiedSpawn> {
        let _ = spec;
        Err(Self::no_process_group())
    }

    /// The one refusal every non-Unix process door returns.
    ///
    /// Both doors say the same thing for the same reason — there is no process
    /// group to own, reap or clean up on this target — so the message is written
    /// once. A second copy is a second statement about the same boundary, and
    /// the two would drift.
    #[cfg(all(not(unix), feature = "process"))]
    fn no_process_group() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "lgwks_bot: supervised process-group cleanup is Unix-only",
        )
    }

    /// Cancel every task, wait for the set to drain, and return what it
    /// drained.
    ///
    /// A task built on [`Supervisor::spawn_repeating`] or [`repeat`] observes
    /// the token and returns from its loop, and `repeat` yields the executor
    /// every few iterations, so a repeating loop no longer has to *suspend* to
    /// become cancellable. One that never observes its token at all is aborted,
    /// which takes effect the next time that task yields. A cooperative return
    /// is reported as [`TaskOutcome::Cancelled`] and an abort as
    /// [`TaskOutcome::Aborted`], which is the difference this return value
    /// exists to make.
    ///
    /// # Limits
    ///
    /// Abort is cooperative. A future that never returns from `poll` — a
    /// blocking call, an unbounded loop with no await, a body that ignores its
    /// token and never suspends — is not preemptible, and this call waits for
    /// it. That is a property of cooperative scheduling, not of this
    /// supervisor: nothing in a thread-per-task executor can stop a poll in
    /// flight from the outside. Such work belongs behind an explicit budget or
    /// in another process. What this module guarantees is the other direction:
    /// every loop *it* drives is bounded per poll, so cancellation and abort do
    /// land.
    pub async fn shutdown(mut self) -> ShutdownReport {
        self.token.cancel();
        // Give a cooperative body room to return on its own before the abort
        // lands, so `Cancelled` and `Aborted` mean what they say. Bounded by
        // `COOPERATIVE_DRAIN_GRACE`: this never waits on a body that ignores its
        // token.
        //
        // The loop absorbs what has finished and then yields, rather than
        // yielding a fixed number of times and only then joining: a body that
        // returns early is settled immediately, so the grace is a ceiling on
        // the wait and not a charge for it.
        // The grace is bounded on the **wall** clock, deliberately, and not on
        // the supervisor's governing clock. This loop waits for real work to
        // reach a join boundary; a caller-advanceable clock would let one
        // advance end the grace before any body ran, and the abort would then
        // report every cooperative task as `Aborted` — a false terminal state
        // produced by the test's own clock. The wall watchdog is the one bound
        // that cannot be moved by whoever is driving time.
        let watchdog = self.clock.wall_watchdog();
        let deadline = watchdog.elapsed().saturating_add(COOPERATIVE_DRAIN_GRACE);
        // The fewest live process tasks seen so far, and when that count last
        // fell: a drain finishing is the progress the process grace runs from.
        let mut fewest_live = self.live_cleanups();
        let mut progressed_at = deadline;
        loop {
            while let Some(joined) = self.set.try_join_next_with_id() {
                self.absorb(joined, Retention::Draining);
            }
            // The set emptying is the real exit condition; the grace is the
            // ceiling on how long the loop may keep trying, extended only while a
            // process task is still draining its bounded cleanup and drains are
            // still finishing.
            let elapsed = watchdog.elapsed();
            let live = self.live_cleanups();
            if live < fewest_live {
                fewest_live = live;
                progressed_at = progressed_at.max(elapsed);
            }
            if self.set.is_empty()
                || (elapsed >= deadline
                    && !self.cleanup_holds_grace(elapsed.saturating_sub(progressed_at)))
            {
                break;
            }
            yield_now().await;
        }
        // Everything still running is dropped here, which is what makes the
        // drain below terminate rather than wait for a body that will not stop.
        self.set.abort_all();
        while let Some(joined) = self.set.join_next_with_id().await {
            self.absorb(joined, Retention::Draining);
        }
        #[cfg(all(unix, feature = "process"))]
        self.refresh_cleanup_owners();
        let stats = self.stats();
        let outcomes: Vec<TaskOutcome> = self.reports.drain(..).collect();
        ShutdownReport {
            outcomes,
            stats,
            #[cfg(all(unix, feature = "process"))]
            cleanup_owners: Arc::clone(&self.cleanup_owners),
        }
    }

    /// Whether shutdown keeps waiting, `quiet` after the later of its
    /// cooperative grace and the last drain that finished: only while a process
    /// task is live and [`PROCESS_CLEANUP_GRACE`] has not passed without one.
    #[cfg(all(unix, feature = "process"))]
    fn cleanup_holds_grace(&self, quiet: Duration) -> bool {
        quiet < PROCESS_CLEANUP_GRACE && self.live_cleanups() > 0
    }

    /// Supervised process tasks that have not yet finished their cleanup.
    #[cfg(all(unix, feature = "process"))]
    fn live_cleanups(&self) -> usize {
        self.cleanup_owners
            .live
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Without supervised processes no task drains a cleanup.
    #[cfg(not(all(unix, feature = "process")))]
    const fn live_cleanups(&self) -> usize {
        0
    }

    /// Without supervised processes nothing drains a cleanup, so the cooperative
    /// grace is the whole wait.
    #[cfg(not(all(unix, feature = "process")))]
    const fn cleanup_holds_grace(&self, _quiet: Duration) -> bool {
        false
    }

    /// Take a permit, waiting for one if the bound is reached, and reap first.
    ///
    /// `None` is a refusal, already counted. A cancelled supervisor refuses
    /// before the wait, during the wait, and at the moment a permit lands:
    /// cancellation is the terminal admission state, and a slot freeing after
    /// it is not an admission offer. The pool's own fair queue holds the
    /// waiter's place, so the acquire is pinned for the whole wait and raced
    /// against the token rather than re-created: a dropped acquire future
    /// forfeits its place in that queue, which is what lets a caller that
    /// arrived later take the permit a longer-waiting caller was queued for.
    /// `run_until_cancelled` polls the acquire first on every wake, so the race
    /// between "a slot freed" and "the supervisor was cancelled" is decided for
    /// cancellation on ties.
    async fn claim(&mut self) -> Option<Lease> {
        // The gate before the wait: a cancelled supervisor never parks on a
        // full pool, because there is nothing it would do with a slot.
        if self.token.is_cancelled() {
            return self.refuse();
        }
        // With a policy installed, the untenanted calls are one implicit tenant
        // and the round — not the pool — decides who a freed permit belongs to.
        #[cfg(feature = "script")]
        if let Some(shell) = self.tenancy.clone() {
            // The untenanted path keeps its silence: a refusal counts and returns,
            // which is exactly what it did before a tenant could be named. The
            // typed refusal belongs to the tenant-scoped doors.
            return match self.claim_tenanted(&shell, &implicit_tenant()).await {
                Ok(lease) => Some(lease),
                Err(_) => {
                    self.refused = self.refused.saturating_add(1);
                    None
                }
            };
        }
        self.reap();

        // The fast path, and it is the common one: an uncontended spawn takes a
        // permit that is already free, with no future built and no timer armed.
        //
        // The slow path, and for a bounded pipeline the steady state: the pool
        // is full and every spawn waits. A permit held by a task is released
        // when that task's future finishes, which is before the task set can
        // yield it, so a full pool frees exactly when a task can be joined. The
        // wait is therefore on the task set — the same wait a raw `JoinSet` +
        // `Semaphore` pipeline makes — and the outcome is absorbed as it arrives
        // rather than on a later reap. Waiting on the semaphore instead cost an
        // extra cross-thread wake per spawn, because the permit is released from
        // inside the finishing task, before the task set has anything to give
        // (#269).
        loop {
            if let Some(permit) = self.try_take() {
                return self.admit(permit);
            }
            match self.wait_at_bound().await {
                Wait::Joined(joined) => self.absorb(joined, Retention::Capped),
                Wait::Recheck => {
                    self.reap();
                }
                Wait::Permit(permit) => return self.admit(permit),
                Wait::Closed | Wait::Cancelled => return self.refuse(),
            }
        }
    }

    /// The tenanted counterpart of [`Supervisor::claim`]: the same wait, against
    /// the round instead of the pool, and a typed refusal instead of a silent
    /// `None` so [`Supervisor::spawn_for`] can hand the caller the tenant that
    /// was at capacity.
    ///
    /// A cancelled supervisor refuses before the wait, during the wait, and at
    /// the moment a permit lands, exactly as the untenanted path does: the token
    /// is read before the wait and again on the permit. There is **no** loop
    /// here, and that is the contract rather than an omission: one arrival
    /// produces exactly one waiter, and the round's queue is FIFO, so a second
    /// attempt would not make progress faster — it would forfeit the place the
    /// first attempt earned.
    ///
    /// Reaping is bookkeeping, not admission — a lease is released inside its
    /// task the moment the body ends, never at the reap — so it happens once per
    /// call, *after* the round has decided, and joins at most
    /// `REAP_PER_ADMISSION` tasks. A queued caller reaps while its grant is on
    /// the way, so the joins cost it nothing it was not already waiting
    /// through. Reaping everything first put every task any tenant had finished
    /// on the critical path of whichever tenant called next, which is how a
    /// tenant whose bodies end at once charged its own bookkeeping to its
    /// neighbour (#268's adversarial row). A grant delivered during the reap
    /// waits in the waiter's slot, which the first poll takes.
    #[cfg(feature = "script")]
    async fn claim_tenanted(
        &mut self,
        shell: &Arc<tenancy_support::TenancyShell>,
        tenant: &Tenant,
    ) -> Result<Lease, SpawnRefused> {
        if self.token.is_cancelled() {
            let refusal = Err(SpawnRefused::Cancelled);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "claim_tenanted: returning an error to the caller");
            return refusal;
        }
        match shell.admit(tenant) {
            tenancy_support::Admission::Admitted(lease) => {
                self.reap_at_most(REAP_PER_ADMISSION);
                // Cancellation wins the race for a permit that landed in the
                // same instant, and the lease's drop returns the capacity to the
                // round either way.
                if self.token.is_cancelled() {
                    Err(SpawnRefused::Cancelled)
                } else {
                    Ok(lease)
                }
            }
            tenancy_support::Admission::Refused { limit } => Err(SpawnRefused::TenantAtCapacity {
                tenant: tenant.clone(),
                limit,
            }),
            tenancy_support::Admission::SupervisorQueueFull { limit } => {
                Err(SpawnRefused::SupervisorQueueFull { limit })
            }
            tenancy_support::Admission::Queued(waiter) => {
                // Park on **this** waiter until it resolves or the token
                // cancels. The waiter is pinned for the whole wait and never
                // re-created, because the round's queue is FIFO: dropping it to
                // re-ask forfeits the place this caller earned and leaves an
                // abandoned entry behind for every interval it waited. A grant
                // does not need a poll interval to arrive — the completion hook
                // hands the permit to this waiter and wakes it directly — and
                // `run_until_cancelled` wakes on the token too, so neither
                // direction needs a timer.
                let mut waiter = std::pin::pin!(waiter);
                self.reap_at_most(REAP_PER_ADMISSION);
                let token = self.token.clone();
                match token.run_until_cancelled(waiter.as_mut()).await {
                    None => Err(SpawnRefused::Cancelled),
                    Some(permit) if !self.token.is_cancelled() => {
                        Ok(Lease::tenanted(permit, Arc::clone(shell), tenant.clone()))
                    }
                    Some(permit) => {
                        // Cancellation won the race against a grant that had
                        // already been charged to this tenant. The permit is
                        // handed back through the round, so the tenant is not
                        // left holding an admission nobody owns.
                        drop(Lease::tenanted(permit, Arc::clone(shell), tenant.clone()));
                        Err(SpawnRefused::Cancelled)
                    }
                }
            }
        }
    }

    /// Turn a permit into an admission, unless cancellation landed first.
    ///
    /// Cancellation is read again on the permit itself, so a slot that frees
    /// after cancellation is not an admission offer: the permit drops here and
    /// the refusal is counted. Every path that obtains a permit decides through
    /// this one rule.
    fn admit(&mut self, permit: OwnedSemaphorePermit) -> Option<Lease> {
        if self.token.is_cancelled() {
            drop(permit);
            return self.refuse();
        }
        Some(Lease::plain(permit))
    }

    /// Count one refused admission.
    const fn refuse(&mut self) -> Option<Lease> {
        self.refused = self.refused.saturating_add(1);
        None
    }

    /// Wait at the bound for the next thing that can change the admission
    /// decision: a task ending, cancellation, or a cleanup owner to re-drive.
    ///
    /// With tasks in flight the wait is on the task set, because ending is how a
    /// task gives its permit back. With none in flight every permit is held
    /// elsewhere — by a cleanup owner — and the wait is on the pool itself.
    /// The futures are pinned on this frame, not boxed, and polled in the order
    /// the decision needs: the slot first, then the token, then the recheck.
    async fn wait_at_bound(&mut self) -> Wait {
        let permits = &self.permits;
        let set = &mut self.set;
        let mut cancelled = pin!(self.token.cancelled());
        #[cfg(all(unix, feature = "process"))]
        let mut recheck = pin!(recheck(&self.cleanup_owners));
        #[cfg(not(all(unix, feature = "process")))]
        let mut recheck = pin!(std::future::pending::<()>());
        if set.is_empty() {
            let mut acquire = pin!(Arc::clone(permits).acquire_owned());
            return poll_fn(|context: &mut Context<'_>| {
                if let Poll::Ready(acquired) = acquire.as_mut().poll(context) {
                    return Poll::Ready(acquired.map_or(Wait::Closed, Wait::Permit));
                }
                if cancelled.as_mut().poll(context).is_ready() {
                    return Poll::Ready(Wait::Cancelled);
                }
                recheck.as_mut().poll(context).map(|()| Wait::Recheck)
            })
            .await;
        }
        let mut joined = pin!(set.join_next_with_id());
        poll_fn(|context: &mut Context<'_>| {
            if let Poll::Ready(next) = joined.as_mut().poll(context) {
                // `None` is an empty set, which was ruled out above; reading it
                // as a recheck re-enters the loop rather than inventing a state.
                return Poll::Ready(next.map_or(Wait::Recheck, Wait::Joined));
            }
            if cancelled.as_mut().poll(context).is_ready() {
                return Poll::Ready(Wait::Cancelled);
            }
            recheck.as_mut().poll(context).map(|()| Wait::Recheck)
        })
        .await
    }

    /// The non-blocking counterpart of [`Supervisor::claim`].
    ///
    /// Cancellation is checked before the pool and again on the permit, so a
    /// cancelled supervisor refuses even when the pool has room.
    fn claim_now(&mut self) -> Option<Lease> {
        if self.token.is_cancelled() {
            self.refused = self.refused.saturating_add(1);
            return None;
        }
        self.reap();
        #[cfg(feature = "script")]
        if let Some(shell) = self.tenancy.clone() {
            return match shell.try_admit(&implicit_tenant()) {
                tenancy_support::Admission::Admitted(lease) if !self.token.is_cancelled() => {
                    Some(lease)
                }
                tenancy_support::Admission::Admitted(_) => {
                    // The lease drops here, returning the permit it just took.
                    self.refused = self.refused.saturating_add(1);
                    None
                }
                tenancy_support::Admission::Refused { .. }
                | tenancy_support::Admission::SupervisorQueueFull { .. } => {
                    self.refused = self.refused.saturating_add(1);
                    None
                }
                tenancy_support::Admission::Queued(_) => {
                    // `try_admit` never queues; a queued answer would be a defect
                    // in the door, and it is reported as the capacity refusal it
                    // must never be mistaken for.
                    self.refused = self.refused.saturating_add(1);
                    None
                }
            };
        }
        if self.token.is_cancelled() {
            // Cancellation can land inside `reap`, which awaits. The permit, if one
            // landed below, would drop here: refusal wins the race.
            self.refused = self.refused.saturating_add(1);
            return None;
        }
        match self.try_take() {
            Some(permit) => Some(Lease::plain(permit)),
            None => {
                self.refused = self.refused.saturating_add(1);
                None
            }
        }
    }

    /// Place a caller's body under `lease`, reporting the two ways a body that
    /// returned can end.
    ///
    /// One wrapper for the untenanted, the refusing and the tenant-scoped spawn
    /// doors, because a third copy of the reading below is how one of them ends
    /// up reporting a cancelled body as a completed one.
    fn place_body<F, Fut>(&mut self, lease: Lease, body: F) -> TaskId
    where
        F: FnOnce(CancellationToken) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let token = self.child_token();
        let future = body(token.clone());
        self.place(lease, async move {
            future.await;
            // The reading is taken at the instant the body returned, so it is a
            // fact about the supervisor's own state rather than a guess at the
            // body's intent: a body that returned while cancellation was in
            // force is reported cancelled, and one that returned with no
            // cancellation in force is reported completed.
            if token.is_cancelled() {
                TaskEnd::Cancelled
            } else {
                TaskEnd::Completed
            }
        })
    }

    /// Take a permit if one is free, without counting anything.
    ///
    /// The non-counting half of [`Self::claim_now`], split out because
    /// [`Self::claim`]'s fast path must *not* count a full pool as a refusal: a
    /// supervisor at its bound is healthy and waiting, which is backpressure, and
    /// counting it as a refusal is the mistake this split exists to prevent.
    fn try_take(&mut self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.permits).try_acquire_owned().ok()
    }

    /// Give the future a [`TaskId`], place it, and register the identity.
    ///
    /// The identity is registered before the task can run to completion,
    /// because the spawn and the insert happen on this thread with no await
    /// between them, so a terminal report always finds its entry.
    ///
    /// The id is returned rather than dropped because a caller sometimes has to
    /// name the task it just started — [`Supervisor::spawn_process`] reports it
    /// so the process's terminal outcome can be matched to the spawn — and the
    /// id is not a handle: it cannot join, abort or observe the task.
    fn place<Fut>(&mut self, lease: Lease, future: Fut) -> TaskId
    where
        Fut: Future<Output = TaskEnd> + Send + 'static,
    {
        let task = self.allocate_task_id();
        self.place_owned(task, async move {
            let _lease = lease;
            future.await
        });
        task
    }

    /// Place a future whose native owner already holds the admission permit.
    fn place_owned<Fut>(&mut self, task: TaskId, future: Fut) -> TaskId
    where
        Fut: Future<Output = TaskEnd> + Send + 'static,
    {
        self.spawned = self.spawned.saturating_add(1);
        let handle = self.set.spawn(future);
        self.identities.insert(handle.id(), task);
        task
    }

    /// Allocate the stable id before constructing a resource-owning future.
    fn allocate_task_id(&mut self) -> TaskId {
        let task = TaskId(self.next_task);
        self.next_task = self.next_task.saturating_add(1);
        task
    }

    #[cfg(all(unix, feature = "process"))]
    /// Refresh retained owners and append terminal receipts when report capacity
    /// allows it.
    fn refresh_cleanup_owners(&mut self) {
        for (task, containment) in self.cleanup_owners.drive(&NATIVE_GROUP_OBSERVER, |task| {
            self.identities.values().any(|live| *live == task)
        }) {
            if self.reports.len() < self.report_cap {
                self.reports.push_back(TaskOutcome::CleanupSettled {
                    task,
                    cleanup: settled_receipt(&containment),
                    containment,
                });
            } else {
                self.reports_dropped = self.reports_dropped.saturating_add(1);
            }
        }
    }

    /// Turn one join result into a counted, retained terminal outcome.
    ///
    /// The `JoinError` is read, not discarded: `is_panic` and `is_cancelled`
    /// are the two ways a task ends without returning, and the panic payload is
    /// carried into the report rather than logged and dropped.
    fn absorb(
        &mut self,
        joined: Result<(Id, TaskEnd), super::task::JoinError>,
        retention: Retention,
    ) {
        // The engine's id is the only identity a `JoinError` carries, which is
        // why the map exists at all; the success path has it in the value too,
        // and the two agree. Read through `as_ref` so the match below can
        // consume `joined` by value, and through `map_or_else` rather than a
        // pattern on a reference, which this crate's
        // `clippy::pattern_type_mismatch` refuses.
        let raw = joined
            .as_ref()
            .map_or_else(|error| error.id(), |pair| pair.0);
        let outcome = match joined {
            Ok((_, TaskEnd::Completed)) => {
                self.succeeded = self.succeeded.saturating_add(1);
                self.identities
                    .remove(&raw)
                    .map(|task| TaskOutcome::Completed {
                        task,
                        cleanup: None,
                        #[cfg(feature = "process")]
                        containment: None,
                    })
            }
            #[cfg(feature = "process")]
            Ok((_, TaskEnd::CompletedWithCleanup { cleanup })) => {
                self.succeeded = self.succeeded.saturating_add(1);
                let (receipt, containment) = cleanup.into_parts();
                self.identities
                    .remove(&raw)
                    .map(|task| TaskOutcome::Completed {
                        task,
                        cleanup: Some(receipt),
                        #[cfg(feature = "process")]
                        containment: Some(containment),
                    })
            }
            Ok((_, TaskEnd::Cancelled)) => {
                self.cancelled = self.cancelled.saturating_add(1);
                self.identities
                    .remove(&raw)
                    .map(|task| TaskOutcome::Cancelled {
                        task,
                        cleanup: None,
                        #[cfg(feature = "process")]
                        containment: None,
                    })
            }
            #[cfg(feature = "process")]
            Ok((_, TaskEnd::CancelledWithCleanup { cleanup })) => {
                self.cancelled = self.cancelled.saturating_add(1);
                let (receipt, containment) = cleanup.into_parts();
                self.identities
                    .remove(&raw)
                    .map(|task| TaskOutcome::Cancelled {
                        task,
                        cleanup: Some(receipt),
                        #[cfg(feature = "process")]
                        containment: Some(containment),
                    })
            }
            // Gated with the variant and with the feature that produces it:
            // nothing else in this crate can report a non-zero process exit.
            #[cfg(feature = "process")]
            Ok((_, TaskEnd::Failed { status, cleanup })) => {
                let (receipt, containment) = cleanup.into_parts();
                // Counted as its own kind rather than folded into `succeeded`:
                // a process that exited 1 is work that did not do what it was
                // asked, and a counter that cannot see the difference reports a
                // failing bot as a healthy one. `completed` still counts it, so
                // the totals keep adding up.
                self.failed = self.failed.saturating_add(1);
                self.identities
                    .remove(&raw)
                    .map(|task| TaskOutcome::Failed {
                        task,
                        status,
                        cleanup: Some(receipt),
                        #[cfg(feature = "process")]
                        containment: Some(containment),
                    })
            }
            Err(error) if error.is_panic() => {
                self.panicked = self.panicked.saturating_add(1);
                let message = panic_message(error.into_panic());
                self.identities
                    .remove(&raw)
                    .map(|task| TaskOutcome::Panicked { task, message })
            }
            Err(_) => {
                self.aborted = self.aborted.saturating_add(1);
                self.identities
                    .remove(&raw)
                    .map(|task| TaskOutcome::Aborted { task })
            }
        };
        self.completed = self.completed.saturating_add(1);
        // A missing identity is unreachable — `place` inserts before the task
        // can finish — but it is handled rather than asserted. The counters
        // above are already exact, so what is lost here is attribution alone,
        // and counting that as a dropped report is the honest reading: the
        // outcome happened and was not retained.
        let Some(outcome) = outcome else {
            self.reports_dropped = self.reports_dropped.saturating_add(1);
            return;
        };
        match retention {
            Retention::Capped if self.reports.len() >= self.report_cap => {
                self.reports_dropped = self.reports_dropped.saturating_add(1);
            }
            Retention::Capped | Retention::Draining => self.reports.push_back(outcome),
        }
    }
}

/// What one task a [`Supervisor`] owns is currently doing, as the owner reads
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

/// A bounded, point-in-time view of one [`Supervisor`].
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
/// [`Supervisor::next_report`]; a snapshot that carried its own copy of them
/// would be the second source of truth this type refuses to be. A caller that
/// needs the authoritative terminal record reads the report stream.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SupervisorSnapshot {
    /// The declared in-flight ceiling, as the supervisor holds it.
    max_in_flight: usize,
    /// The supervisor's own counters at this instant.
    stats: Stats,
    /// Live tasks, at most `live_limit` of them, in [`TaskId`] order.
    live: Vec<LiveTask>,
    /// How many live tasks the cap excluded.
    live_truncated: usize,
    /// Whether the supervisor has stopped admitting.
    cancelled: bool,
    /// Terminal outcomes waiting to be drained through
    /// [`Supervisor::next_report`].
    reports_pending: usize,
    /// Cleanup obligations still charged to this supervisor's capacity.
    #[cfg(all(unix, feature = "process"))]
    pending_cleanups: usize,
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
    /// [`Supervisor::next_report`].
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

/// What a [`Supervisor`] will accept next, from one snapshot.
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

impl Drop for Supervisor {
    /// Cancel the token and abort the set.
    ///
    /// This is the guarantee that a supervisor which goes out of scope takes
    /// its tasks with it. It is deliberately synchronous: `Drop` cannot await,
    /// so tasks are aborted rather than joined. Use [`Supervisor::shutdown`]
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

/// Build the private engine command for `spec` and start the child.
///
/// The one constructor both [`Supervisor::spawn_process`] and
/// [`Supervisor::run_process`] call, so the group, the confinement, the
/// reaping and the configured streams cannot drift between them.
///
/// The engine's `Command` describes the child — program, arguments,
/// environment, streams and the new process group — and the standard library
/// starts it. `tokio::process::Command::spawn` is banned workspace-wide by
/// `clippy.toml`, with [`Supervisor::spawn_process`] as its replacement; this
/// is that replacement's own start, so it goes through the std command the
/// engine already built rather than through the banned call. What the engine's
/// child added beyond the std one — async pipes, a kill when the handle is
/// dropped, and reaping an orphan — [`OwnedChild`] provides.
///
/// A confined spec replaces the program with the platform's sandbox wrapper
/// (see [`ProcessSpec::command`](crate::rt::process::ProcessSpec::command)),
/// so the group below still leads the wrapper and through it the target.
#[cfg(all(unix, feature = "process"))]
fn start(spec: &ProcessSpec) -> io::Result<(OwnedChild, i32)> {
    let mut command = spec.command()?;
    spec.configure(&mut command)?;
    // The group kill reaches what the child spawned; `OwnedChild`'s drop-time
    // kill reaches the direct child on the paths where no group kill ran.
    command.process_group(0);
    // Under the start-sweep lock from the spawn until the track below: a new
    // leader is sweepable before it is tracked, so the lock keeps every
    // adoption pass from collecting a leader that exits fast.
    #[cfg(all(target_os = "linux", feature = "process"))]
    let _start_sweep = crate::journal::owner::lock(&START_SWEEP);
    let child = OwnedChild::own(command.into_std().spawn()?)?;
    // Read before the child is awaited: an unreaped child still has its id,
    // and a group leader's group id is its own pid, which `process_group(0)`
    // arranged above. A refusal here drops — and so kills — the child.
    let group = child.group()?;
    // The sweep's exclusion first: from here until the driver reaps or parks
    // the leader, its pid is somebody's answer rather than an adoptee.
    #[cfg(all(target_os = "linux", feature = "process"))]
    track_leader(group);
    // Released before the collection below: the pass takes the same lock, and
    // a mutex is not reentrant, so holding it across the call would deadlock
    // the start on its own sweep.
    #[cfg(all(target_os = "linux", feature = "process"))]
    drop(_start_sweep);
    // A start is the second moment adopted zombies are collected, beside the
    // confirmation pass: a supervisor that only ever starts children still
    // releases the adoptees its earlier trees stranded.
    #[cfg(all(target_os = "linux", feature = "process"))]
    let _collected = sweep_adopted();
    Ok((child, group))
}

/// Serialises a start's spawn-to-track against every adoption sweep.
///
/// [`track_leader`] runs after [`OwnedChild::own`], and owning takes locks and
/// builds pipe readers: under concurrency the new leader is visible in the
/// process table — and therefore sweepable — before it is tracked. A sweep
/// pass landing in that window collects a leader that exits fast, and its
/// driver's exit observation fails with `ECHILD`, which surfaces as an
/// unobservable run with no exit code. Both sides hold this lock — the start
/// from the spawn until the track, the sweep for the whole scan-and-reap pass —
/// so a pass never interleaves a start. Short holds only, never across an
/// await: the spawn and the nonblocking reaps are both synchronous.
#[cfg(all(target_os = "linux", feature = "process"))]
static START_SWEEP: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// How many tree scopes this process has named, disambiguating reused pids.
///
/// A scope name carries its leader's pid, and pids are reused: the counter
/// keeps two trees from sharing a scope name when the second leader inherits
/// the first's number.
#[cfg(all(target_os = "linux", feature = "process"))]
static SCOPE_IDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// The subreaper flag, enabled once per process before the first spawn.
///
/// Process-wide by kernel design: every supervisor in this process shares the
/// adoptees, which is why the cleanup intersects them with each tree's own
/// capture rather than trusting parenthood alone.
#[cfg(all(target_os = "linux", feature = "process"))]
static SUBREAPER_ONCE: std::sync::Once = std::sync::Once::new();

/// Pids of supervised leaders this process started and has not yet reaped.
///
/// The subreaper flag adopts every orphan into this process, and an adopted
/// zombie holds its pid — where `kill -0` still reports it present — until
/// somebody waits on it. The sweep below is that somebody for the adoptees,
/// and this set is what keeps it from waiting on a leader whose driver still
/// owns the exit: a pid leaves the set when its driver reaps or parks it, so
/// a collected pid is never a driver's answer.
#[cfg(all(target_os = "linux", feature = "process"))]
static LIVE_LEADERS: std::sync::Mutex<std::collections::BTreeSet<i32>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

/// Mark `pid` as a supervised leader whose exit its driver still owns.
///
/// Called with the pid [`start`] just returned, before any task can observe
/// the child: from here until the untrack, the sweep treats the pid as
/// somebody's answer rather than an adoptee.
#[cfg(all(target_os = "linux", feature = "process"))]
fn track_leader(pid: i32) {
    if pid > 0 {
        let _present = crate::journal::owner::lock(&LIVE_LEADERS).insert(pid);
    }
}

/// Release `pid`: its driver reaped or parked it, so the sweep must not wait
/// on it — and a later adoptee under a reused number starts unattributed.
#[cfg(all(target_os = "linux", feature = "process"))]
fn untrack_leader(pid: i32) {
    let _removed = crate::journal::owner::lock(&LIVE_LEADERS).remove(&pid);
}

/// Reap every zombie child no driver owns.
///
/// Adopted orphans land in this process whether or not any guard captured
/// them — a tree whose leader exited before any capture still orphans its
/// tree here — and a zombie holds its pid, where `kill -0` keeps reporting it
/// present, until it is waited on. This collects them: `waitpid` with `NOHANG`
/// only collects the already-dead, so live children are untouched, and leaders
/// and parked orphans are excluded by number, so no driver's exit is stolen.
/// Returns how many zombies were collected.
///
/// Best effort like every reap here: an unreadable child list collects
/// nothing, and a refused wait stops the pass rather than the cleanup.
///
/// **Residual:** a host-spawned child that is a zombie at a sweep moment is
/// collected too, and its owner's later `wait` reports it reaped rather than
/// collecting it itself. Live host children are never touched, and hosts that
/// wait promptly never notice; the edge is inherent to the process-wide
/// subreaper flag, whose adoptees would otherwise strand every tree's zombies
/// here.
/// Children of this process no driver owns: not live leaders, not parked orphans.
#[cfg(all(target_os = "linux", feature = "process"))]
fn unclaimed_children() -> Vec<i32> {
    let children = match lgwks_std::process::own_children() {
        Ok(children) => children,
        Err(error) => {
            lgwks_std::trace::debug!(%error, "sweep: the child list was unreadable; adopted zombies wait for the next sweep");
            return Vec::new();
        }
    };
    if children.is_empty() {
        return Vec::new();
    }
    let live = crate::journal::owner::lock(&LIVE_LEADERS).clone();
    let parked: std::collections::BTreeSet<i32> = crate::journal::owner::lock(&ORPHANS)
        .iter()
        .filter_map(|orphan| i32::try_from(orphan.id()).ok())
        .collect();
    children
        .into_iter()
        .filter(|pid| !live.contains(pid) && !parked.contains(pid))
        .collect()
}

/// Reap the unclaimed children of this process once, best effort.
///
/// One nonblocking pass over the children no driver owns — not live leaders,
/// not parked orphans — returning how many it collected. A refusal or an
/// unreadable child list waits for the next sweep rather than failing the
/// caller; the confirming sweep below is the one that waits out the deaths.
/// The whole pass holds the start-sweep lock, so it never interleaves a
/// start's spawn-to-track: without that a pass could collect a leader that
/// exits before its driver tracks it, stealing its driver's exit.
#[cfg(all(target_os = "linux", feature = "process"))]
fn sweep_adopted() -> usize {
    let _start_sweep = crate::journal::owner::lock(&START_SWEEP);
    let zombies = unclaimed_children();
    if zombies.is_empty() {
        return 0;
    }
    match lgwks_std::process::reap_descendants(&zombies) {
        Ok(reaped) => reaped.len(),
        Err(error) => {
            lgwks_std::trace::debug!(%error, "sweep: the adoption reap was refused; adopted zombies wait for the next sweep");
            0
        }
    }
}

/// Reap unclaimed children until none remain or the bound is spent.
///
/// The confirming half of [`sweep_adopted`]: a signalled process dies on the
/// kernel's schedule, and a single nonblocking reap attempted before the death
/// lands leaves a zombie no later pass collects. This waits for the deaths,
/// bounded to [`SWEEP_CONFIRM_PASSES`] × [`ADOPTION_REAP_INTERVAL`]: a death
/// lands in milliseconds, while a live adoptee
/// — a long-sleeping escapee the cleanup's own kill is still delivering —
/// never vanishes on its own, and waiting out the adoption bound here would
/// blow the shutdown grace a draining cleanup is owed (the drain test shuts
/// 32 trees at once against 50 ms). Past the bound whatever remains is the
/// observation rounds' to report, not this sweep's to wait away. Async like
/// the rest of the cleanup path, so the wait parks on the runtime clock
/// instead of blocking its thread; only ever waits on processes that are
/// already children of this process.
#[cfg(all(target_os = "linux", feature = "process"))]
async fn sweep_adopted_confirmed() -> usize {
    let mut collected = 0usize;
    for _ in 0..SWEEP_CONFIRM_PASSES {
        if unclaimed_children().is_empty() {
            break;
        }
        collected = collected.saturating_add(sweep_adopted());
        crate::rt::time::sleep(ADOPTION_REAP_INTERVAL).await;
    }
    collected
}

/// The wait between two reap passes over signalled adoptees.
#[cfg(all(target_os = "linux", feature = "process"))]
const ADOPTION_REAP_INTERVAL: Duration = Duration::from_millis(5);

/// Reap passes one drain's [`reap_adopted`] makes before it observes instead.
///
/// With [`ADOPTION_REAP_INTERVAL`] this is two seconds, the longest wait a
/// drain makes on its own bounds, so [`PROCESS_CLEANUP_GRACE`] is set past it.
#[cfg(all(target_os = "linux", feature = "process"))]
const ADOPTION_REAP_PASSES: u32 = 400;

/// Passes [`sweep_adopted_confirmed`] makes over the unclaimed children.
#[cfg(all(target_os = "linux", feature = "process"))]
const SWEEP_CONFIRM_PASSES: u32 = 10;

/// What [`reap_adopted`] left behind.
#[cfg(all(target_os = "linux", feature = "process"))]
struct AdoptionReap {
    /// Adopted pids still children of this process when the bound ran out.
    unreaped: Vec<i32>,
    /// Reap passes made, at most [`ADOPTION_REAP_PASSES`].
    passes: u32,
}

/// Reap signalled adoptees until none is still owed, or the bound is spent.
///
/// A pid is settled two ways: a pass here reaps it, or it is no longer a child
/// of this process at all. The second is the common case under shutdown, not
/// an edge: [`sweep_adopted`] collects every unclaimed child, and each draining
/// cleanup runs one sweep, so with thirty-two drains at once one drain's sweep
/// reaps another drain's adoptee. `waitpid` then answers `ECHILD`, which
/// `reap_descendants` passes over without naming, so the pid looks exactly like
/// one still dying. Read that way it held its drain for the whole bound, 400 ×
/// 5 ms, with no drain finishing meanwhile, and shutdown aborted a drain whose
/// tree was already gone (main run 37945731952, Linux container). So a pass
/// that leaves anything asks which of those pids are still this process's
/// children — a zombie still is until it is reaped — and keeps only those. An
/// unreadable child list keeps every pid owed, which costs the bound and never
/// claims a reap that did not happen.
#[cfg(all(target_os = "linux", feature = "process"))]
async fn reap_adopted(adopted: Vec<i32>) -> AdoptionReap {
    let mut unreaped = adopted;
    let mut passes = 0_u32;
    while passes < ADOPTION_REAP_PASSES {
        passes = passes.saturating_add(1);
        match lgwks_std::process::reap_descendants(&unreaped) {
            Ok(reaped) => unreaped.retain(|pid| reaped.binary_search(pid).is_err()),
            Err(error) => {
                lgwks_std::trace::debug!(%error, "confirm: the adoption reap was refused; retrying within the death bound");
            }
        }
        if !unreaped.is_empty() {
            match lgwks_std::process::adopted_descendants(&unreaped) {
                Ok(children) => unreaped.retain(|pid| children.contains(pid)),
                Err(error) => {
                    lgwks_std::trace::debug!(%error, "confirm: the child list was unreadable; every adoptee stays owed");
                }
            }
        }
        if unreaped.is_empty() {
            break;
        }
        crate::rt::time::sleep(ADOPTION_REAP_INTERVAL).await;
    }
    AdoptionReap { unreaped, passes }
}

/// Scope the just-spawned leader `pid` to its own cgroup, best effort.
///
/// `None` — an uncreatable mount, a refused attach — is the supervisor's cue
/// to run its capture rounds without the kernel owner, not a spawn failure:
/// containment degrades to the group and the capture, and the receipt names
/// the weaker mechanism rather than failing a start that succeeded. The
/// subreaper flag is enabled beside it, so an orphan the rounds never named
/// is still adopted by this process instead of init.
#[cfg(all(target_os = "linux", feature = "process"))]
fn scope_tree(pid: i32) -> Option<CgroupScope> {
    SUBREAPER_ONCE.call_once(|| {
        if let Err(error) = lgwks_std::process::enable_child_subreaper() {
            lgwks_std::trace::debug!(%error, "start: the subreaper flag was refused; orphans re-parent to init");
        }
    });
    if pid <= 0 {
        return None;
    }
    let id = SCOPE_IDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let scope = match CgroupScope::create(&format!("lgwks-{pid}-{id}")) {
        Ok(scope) => scope,
        Err(error) => {
            lgwks_std::trace::debug!(%error, pid, "start: no cgroup scope; the capture rounds own this tree");
            return None;
        }
    };
    if let Err(error) = scope.add(pid) {
        lgwks_std::trace::debug!(%error, pid, "start: the leader left before its scope attach; the capture rounds own this tree");
        return None;
    }
    Some(scope)
}

/// How often a child that has been signalled is checked for its exit while it
/// is being reaped. The same cadence [`observe_pid_without_reaping`] observes
/// a running child at, so reaping adds no finer timer than observing already
/// uses.
#[cfg(all(unix, feature = "process"))]
const REAP_POLL: Duration = Duration::from_millis(5);

/// Children killed when their owner was dropped but not yet exited.
///
/// A dropped handle cannot await its child, and a killed child that nobody
/// waits on stays a zombie and keeps its pid allocated. Each one is parked here
/// and reaped by the next [`OwnedChild`] that starts or drops — the same
/// contract as the engine's own orphan queue, which this replaces. Its length
/// is bounded by the children that were SIGKILLed and have not yet been torn
/// down by the kernel, which is momentary for every process not stuck in an
/// uninterruptible wait.
#[cfg(all(unix, feature = "process"))]
static ORPHANS: std::sync::Mutex<Vec<std::process::Child>> = std::sync::Mutex::new(Vec::new());

/// Reap every parked orphan that has exited, keeping the rest parked.
#[cfg(all(unix, feature = "process"))]
fn reap_orphans() {
    crate::journal::owner::lock(&ORPHANS)
        .retain_mut(|orphan| matches!(orphan.try_wait(), Ok(None)));
}

/// A started child this module owns: the std handle, its two captured pipes as
/// async readers, and whether it has been reaped.
///
/// Dropping an unreaped one kills the direct child and parks it in [`ORPHANS`],
/// so an aborted driver never leaves a running child or an unreaped zombie
/// behind it. The process-group guard covers the child's own descendants.
#[cfg(all(unix, feature = "process"))]
struct OwnedChild {
    /// The started child; `None` only once it has been reaped or parked.
    inner: Option<std::process::Child>,
    /// Its pid, which is also its process group's id.
    pid: u32,
    /// Its stdout, when the spec captured it.
    stdout: Option<ChildStdout>,
    /// Its stderr, when the spec captured it.
    stderr: Option<ChildStderr>,
}

#[cfg(all(unix, feature = "process"))]
impl OwnedChild {
    /// Own a just-started child, converting its captured pipes to async
    /// readers on this runtime's reactor.
    ///
    /// The child is owned before the conversion, so a conversion that fails
    /// drops an `OwnedChild` — killing and parking the child — rather than
    /// leaking a running process behind an error.
    fn own(mut inner: std::process::Child) -> io::Result<Self> {
        // A start is one of the two moments [`ORPHANS`] is collected, so a
        // supervisor that only ever starts children still releases the ones
        // its earlier drivers parked.
        reap_orphans();
        let stdout = inner.stdout.take();
        let stderr = inner.stderr.take();
        let mut child = Self {
            pid: inner.id(),
            inner: Some(inner),
            stdout: None,
            stderr: None,
        };
        child.stdout = stdout.map(ChildStdout::from_std).transpose()?;
        child.stderr = stderr.map(ChildStderr::from_std).transpose()?;
        Ok(child)
    }

    /// The child's pid as the `pid_t` its process group is signalled by.
    ///
    /// # Errors
    ///
    /// A pid that does not fit `pid_t` is not one this module can signal, so
    /// it is refused rather than mapped to an id that would name another group.
    fn group(&self) -> io::Result<i32> {
        i32::try_from(self.pid).map_err(io::Error::other)
    }

    /// Collect the child's exit status, releasing its pid.
    ///
    /// Not cancellable by the run's token on purpose: by the time this runs the
    /// child has exited or its group has been signalled, and a reap abandoned
    /// half way would leave the zombie this exists to collect. A dropped reap
    /// is still safe — the drop parks the child in [`ORPHANS`].
    async fn reap(&mut self) -> io::Result<std::process::ExitStatus> {
        loop {
            let Some(inner) = self.inner.as_mut() else {
                let refusal = Err(io::Error::other("the child was already reaped"));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), pid = self.pid, "owned child: reap after reap");
                return refusal;
            };
            if let Some(status) = inner.try_wait()? {
                self.inner = None;
                reap_orphans();
                #[cfg(all(target_os = "linux", feature = "process"))]
                if let Ok(pid) = i32::try_from(self.pid) {
                    untrack_leader(pid);
                }
                return Ok(status);
            }
            crate::rt::time::sleep(REAP_POLL).await;
        }
    }
}

#[cfg(all(unix, feature = "process"))]
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let Some(mut inner) = self.inner.take() else {
            return;
        };
        // A child that already exited is a zombie that still holds its pid, so
        // the kill cannot reach another process; the reap below is what
        // releases it either way.
        if let Err(error) = inner.kill() {
            lgwks_std::trace::debug!(%error, pid = self.pid, "owned child: direct kill on drop refused");
        }
        if !matches!(inner.try_wait(), Ok(Some(_))) {
            crate::journal::owner::lock(&ORPHANS).push(inner);
        }
        reap_orphans();
        // Parked or reaped: either way no driver owns this exit any more, so
        // the sweep must not mistake the number for an adoptee either.
        #[cfg(all(target_os = "linux", feature = "process"))]
        if let Ok(pid) = i32::try_from(self.pid) {
            untrack_leader(pid);
        }
    }
}

/// A child's process group, killed as a unit unless it is disarmed.
///
/// A `Child` is one process. A command a bot starts is usually a shell or a
/// wrapper that spawns its own children — a pipeline, a package manager, a
/// build — and `Child::kill` reaches only the first of them, leaving the rest
/// running with nobody holding their handles. Signalling the *group* is what
/// makes "stop the process" mean the whole tree.
///
/// The guard is armed when the group is created. It may signal only while the
/// leader is unreaped; after reaping, it uses signal-zero observation only.
/// The owner holds the process permit through the bounded absence observation.
#[cfg(all(unix, feature = "process"))]
struct ProcessGroup<'ops> {
    /// The group id, or `0` when the engine could not report the child's id.
    ///
    /// Zero is *refused* by `kill_process_group` rather than read as "this
    /// process's own group", so an unreadable id can never signal the
    /// supervisor's own process group.
    group: i32,
    /// Attribution for a later cleanup receipt.
    task: TaskId,
    /// The process admission lease, transferred to `owners` if cleanup remains.
    ///
    /// A [`Lease`] rather than a bare permit, so a cleanup obligation that
    /// outlives its task returns the capacity to the tenant it was charged to,
    /// not merely to a pool nobody is waiting on.
    permit: Option<Lease>,
    /// Supervisor-owned destination for unresolved cleanup.
    owners: Arc<CleanupOwners>,
    /// Whether a kill is still owed to the group.
    armed: bool,
    /// Whether the leader has been reaped and the numeric id released.
    leader_reaped: bool,
    /// Whether the leader was observed to have exited before the cleanup began,
    /// after which a walk from it can name nothing.
    leader_exited: bool,
    /// Whether the group was observed absent: by a signal refusal, or by the
    /// post-reap signal-zero probe.
    group_absent: bool,
    /// What the cleanup read, signalled, and could not account for.
    containment: Containment,
    /// Captured pids a signal has already been delivered to.
    ///
    /// The set is what keeps "signal every captured pid" from becoming "signal
    /// every captured pid once per round": a pid is signalled the first round it
    /// is captured in, and a pid that is still running afterwards is signalled
    /// again **only** because the observation proved it alive, which is the same
    /// rule the group follows for its own id.
    signalled_pids: std::collections::BTreeSet<i32>,
    /// The tree's cgroup v2 scope, when the mount allowed one.
    ///
    /// Killed first on every cleanup path, observed empty to earn the receipt,
    /// and removed when the guard drops. `None` where no child cgroup was
    /// creatable, where the capture rounds own the tree alone.
    #[cfg(all(target_os = "linux", feature = "process"))]
    cgroup: Option<CgroupScope>,
    /// The process-group signalling boundary, injectable for regression tests.
    signaller: &'ops dyn GroupSignaller,
    /// Non-mutating group existence boundary, injectable for regression tests.
    observer: &'ops dyn GroupObserver,
    /// The per-process containment boundary, injectable for regression tests.
    capture: &'ops dyn DescendantCapture,
}

/// One process-group signalling operation.
#[cfg(all(unix, feature = "process"))]
trait GroupSignaller: Sync {
    /// Send SIGKILL to `group`.
    fn signal(&self, group: i32) -> io::Result<()>;
}

/// One non-mutating process-group existence observation.
#[cfg(all(unix, feature = "process"))]
trait GroupObserver: Sync {
    /// Return whether `group` currently names a process group.
    fn exists(&self, group: i32) -> io::Result<bool>;
}

/// One capture's finding: which mechanism read it, and which pids it named.
///
/// The crate's own shape rather than
/// [`lgwks_std::process::DescendantSet`] itself, for two reasons: the estate type
/// is `#[non_exhaustive]` with private fields, so a test in this crate could not
/// build one and the seam could not be driven against a scripted tree at all; and
/// what the drain needs is three facts, not a process-table abstraction.
#[cfg(all(unix, feature = "process"))]
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Capture {
    /// How the pids were found.
    mechanism: ContainmentMechanism,
    /// The pids the reading named, sorted and deduplicated.
    pids: Vec<i32>,
    /// Whether a bound stopped the reading before the tree ended.
    truncated: bool,
}

#[cfg(all(unix, feature = "process"))]
impl Capture {
    /// The finding the audited `lgwks_std::process` reading produced.
    fn from_set(set: &lgwks_std::process::DescendantSet) -> Self {
        Self {
            mechanism: set.mechanism(),
            pids: set.pids().to_vec(),
            truncated: set.is_truncated(),
        }
    }

    /// Whether the reading named nothing.
    fn is_empty(&self) -> bool {
        self.pids.is_empty()
    }
}

/// The per-process containment boundary: name the tree, stop one of its members,
/// and observe which of them are still running.
///
/// Three operations rather than one, because they answer three different
/// questions and each has its own failure mode: a capture that read nothing is
/// not a capture that found nothing, a signal to a pid that has ended is `ESRCH`
/// rather than a failure, and an observation is the only one of the three that is
/// safe to repeat. Injectable so the drain's decision table can be exercised
/// against every combination of those failures without forking a process.
#[cfg(all(unix, feature = "process"))]
trait DescendantCapture: Sync {
    /// Every process descended from `root` right now.
    fn capture(&self, root: i32) -> io::Result<Capture>;
    /// Send SIGKILL to one captured pid.
    fn signal(&self, pid: i32) -> io::Result<()>;
    /// Which of `pids` are still running, as opposed to present-but-reaped.
    fn running(&self, pids: &[i32]) -> io::Result<Vec<i32>>;
}

/// A cleanup obligation retained after its original task ends.
#[cfg(all(unix, feature = "process"))]
struct PendingCleanup {
    /// Stable task attribution.
    task: TaskId,
    /// Process-group id, used only for non-mutating existence observation.
    group: i32,
    /// Capacity remains charged until terminal absence is observed. A
    /// [`Lease`], so a retained obligation is still charged to the tenant whose
    /// admission it holds.
    _lease: Lease,
    /// Absence was observed before its task's terminal report was joined.
    absence_observed: bool,
    /// What the retained cleanup read and signalled, for its terminal receipt.
    containment: Containment,
}

/// Bounded owners for groups whose leader task could not prove cleanup.
/// Each entry owns its process permit, so retained obligations stay charged to
/// the configured native-resource ceiling.
#[cfg(all(unix, feature = "process"))]
#[derive(Default)]
struct CleanupOwners {
    /// Owners retained under the in-flight semaphore ceiling.
    pending: Mutex<Vec<PendingCleanup>>,
    /// Signalled on every registration, so a spawn waiting at the bound learns
    /// that a permit has moved to an owner that only a reap will release.
    registered: Notify,
    /// Spawned process tasks that have not yet returned; see [`LiveProcess`].
    live: std::sync::atomic::AtomicUsize,
}

/// One spawned process task that has not yet returned.
///
/// Moved into the task's future, so it is dropped when the task returns *or* is
/// aborted, and [`Supervisor::shutdown`] can tell a process still draining its
/// cleanup from a body that ignores its token.
#[cfg(all(unix, feature = "process"))]
struct LiveProcess(Arc<CleanupOwners>);

#[cfg(all(unix, feature = "process"))]
impl LiveProcess {
    /// Count one more live process task against `owners`.
    fn enter(owners: &Arc<CleanupOwners>) -> Self {
        owners
            .live
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Self(Arc::clone(owners))
    }
}

#[cfg(all(unix, feature = "process"))]
impl Drop for LiveProcess {
    fn drop(&mut self) {
        self.0
            .live
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

#[cfg(all(unix, feature = "process"))]
impl CleanupOwners {
    /// Transfer a live task's lease and cleanup obligation to this registry.
    fn register(&self, task: TaskId, group: i32, lease: Lease, containment: Containment) {
        crate::journal::owner::lock(&self.pending).push(PendingCleanup {
            task,
            group,
            _lease: lease,
            absence_observed: false,
            containment,
        });
        self.registered.notify_one();
    }

    /// Count currently retained obligations.
    fn pending_count(&self) -> usize {
        crate::journal::owner::lock(&self.pending).len()
    }

    /// Probe each owner once; present or unobservable groups keep their lease.
    fn drive(
        &self,
        observer: &dyn GroupObserver,
        task_is_live: impl Fn(TaskId) -> bool,
    ) -> Vec<(TaskId, Containment)> {
        let mut pending = crate::journal::owner::lock(&self.pending);
        let mut retained = Vec::with_capacity(pending.len());
        let mut settled = Vec::new();
        for mut owner in pending.drain(..) {
            let absent =
                owner.absence_observed || matches!(observer.exists(owner.group), Ok(false));
            if absent {
                if task_is_live(owner.task) {
                    owner.absence_observed = true;
                    retained.push(owner);
                } else {
                    settled.push((owner.task, owner.containment));
                }
            } else {
                retained.push(owner);
            }
        }
        *pending = retained;
        settled
    }
}

/// Resolve when a spawn waiting at the bound must stop and reap: after
/// [`CLEANUP_RECHECK`] while a cleanup owner holds a permit, or the moment one is
/// registered while none was.
///
/// A registration is a permit that no task ending will release, so a waiter
/// that armed no timer must hear about it. `notify_one` stores the wakeup when
/// nobody is waiting yet, so a registration between the count and the first
/// poll of `notified` is not lost.
#[cfg(all(unix, feature = "process"))]
async fn recheck(owners: &CleanupOwners) {
    if owners.pending_count() > 0 {
        crate::rt::time::sleep(CLEANUP_RECHECK).await;
    } else {
        owners.registered.notified().await;
    }
}

#[cfg(all(unix, feature = "process"))]
impl fmt::Debug for CleanupOwners {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CleanupOwners")
            .field("pending", &self.pending_count())
            .finish()
    }
}

#[cfg(feature = "process")]
/// The production process-group syscall adapter.
#[cfg(all(unix, feature = "process"))]
struct NativeGroupSignaller;

#[cfg(all(unix, feature = "process"))]
impl GroupSignaller for NativeGroupSignaller {
    fn signal(&self, group: i32) -> io::Result<()> {
        lgwks_std::process::kill_process_group(group)
    }
}

#[cfg(all(unix, feature = "process"))]
/// The production signal-zero process-group observer.
struct NativeGroupObserver;

#[cfg(all(unix, feature = "process"))]
impl GroupObserver for NativeGroupObserver {
    fn exists(&self, group: i32) -> io::Result<bool> {
        lgwks_std::process::process_group_exists(group)
    }
}

#[cfg(feature = "process")]
/// One static production adapter, so task futures hold no borrowed runtime state.
#[cfg(all(unix, feature = "process"))]
static NATIVE_GROUP_SIGNALLER: NativeGroupSignaller = NativeGroupSignaller;

#[cfg(all(unix, feature = "process"))]
/// The static observer used by process-group cleanup.
static NATIVE_GROUP_OBSERVER: NativeGroupObserver = NativeGroupObserver;

#[cfg(all(unix, feature = "process"))]
/// The production per-process containment adapter.
struct NativeDescendantCapture;

#[cfg(all(unix, feature = "process"))]
impl DescendantCapture for NativeDescendantCapture {
    /// Read the tree through the audited `lgwks_std::process` primitives, which
    /// is where the rustix edge and the bounded walk live.
    fn capture(&self, root: i32) -> io::Result<Capture> {
        lgwks_std::process::capture_descendants(root).map(|set| Capture::from_set(&set))
    }

    fn signal(&self, pid: i32) -> io::Result<()> {
        lgwks_std::process::kill_process(pid)
    }

    fn running(&self, pids: &[i32]) -> io::Result<Vec<i32>> {
        Ok(lgwks_std::process::running_processes(pids)?
            .into_iter()
            .collect())
    }
}

#[cfg(all(unix, feature = "process"))]
/// One static production capture, so a process future holds no borrowed state.
static NATIVE_DESCENDANT_CAPTURE: NativeDescendantCapture = NativeDescendantCapture;

/// The terminal receipt a retained cleanup earns when its group is observed
/// absent.
///
/// Not always `CleanupConfirmed`: an obligation transferred to the registry still
/// carries the containment report its task ended with, and a cleanup that left a
/// survivor has not reclaimed the tree however absent the group became. Naming
/// the mechanism here is what lets a consumer read one settled receipt and know
/// both.
#[cfg(all(unix, feature = "process"))]
fn settled_receipt(containment: &Containment) -> CleanupReceipt {
    if containment.survivors().is_empty() {
        CleanupReceipt::CleanupConfirmed
    } else {
        CleanupReceipt::CleanupSurvivors {
            survivors: containment.survivors().to_vec(),
        }
    }
}

/// What happened while observing a supervised child without releasing its pid.
#[derive(Clone, Copy)]
#[cfg(all(unix, feature = "process"))]
enum ProcessObservation {
    /// The child exited; the status is retrieved after cleanup on Unix.
    Exited,
    /// The supervisor's cancellation token was set.
    Cancelled,
    /// The process deadline elapsed before an exit was observed.
    Deadline,
    /// The platform could not perform non-reaping exit observation.
    Unobservable,
}

/// Poll `pid`'s exit state without reaping it, keeping its pid allocated until
/// the process-group termination phase is complete.
///
/// Takes the numeric id rather than the engine's child, so the caller can keep
/// the child free to be waited on once the group has been signalled.
#[cfg(all(unix, feature = "process"))]
async fn observe_pid_without_reaping(
    clock: &Clock,
    pid: i32,
    deadline: Option<Duration>,
    token: &CancellationToken,
) -> ProcessObservation {
    if pid <= 0 {
        return ProcessObservation::Unobservable;
    }
    // The deadline is anchored on the governing clock's own timeline, so a
    // caller-advanceable clock exhausts it in one arithmetic step and a wall
    // clock exhausts it when real time says so — the same refusal either way.
    let started_at = clock.now();
    let deadline = deadline.map(|duration| started_at.saturating_add(duration));
    loop {
        if token.is_cancelled() {
            return ProcessObservation::Cancelled;
        }
        if deadline.is_some_and(|at| clock.now() >= at) {
            return ProcessObservation::Deadline;
        }
        match lgwks_std::process::child_has_exited_without_reaping(pid) {
            Ok(true) => return ProcessObservation::Exited,
            Ok(false) => {}
            Err(_) => return ProcessObservation::Unobservable,
        }
        if token
            .run_until_cancelled(crate::rt::time::sleep(Duration::from_millis(5)))
            .await
            .is_none()
        {
            return ProcessObservation::Cancelled;
        }
    }
}

/// Called once for each newline-terminated line of a child's stdout, as the
/// child writes it.
///
/// The shape a service's readiness gate takes: a long-lived child announces the
/// address or fact it is ready at on one line, and the caller closes its own
/// gate on that line rather than on a timer.
///
/// `Send + Sync` because a process driver is also run from
/// [`Supervisor::spawn_process`]'s spawned task, which must be `Send`; a
/// non-`Send` observer would force the unobserved driver to have a different
/// signature from the observed one. The bound is honest rather than a
/// convenience: the observer receives a line it must not retain, it is called
/// synchronously on the task driving one child, and a readiness gate it closes
/// is an `Arc`-backed fact that *is* shareable. A caller whose gate needs
/// thread-local state cannot be one, and should not be.
///
/// The bytes exclude the line terminator and one trailing carriage return, so
/// the same line is observed identically on a CRLF writer and an LF one, and
/// they borrow from the read buffer rather than being copied: the observation and
/// the retained capture are the same bytes, seen once.
#[cfg(feature = "process")]
pub type LineObserver<'a> = &'a (dyn Fn(&[u8]) + Send + Sync + 'a);

/// The terminal observation of one supervised child, before it is mapped to a
/// public report.
///
/// One driver produces this shape for both [`Supervisor::spawn_process`] and
/// [`Supervisor::run_process`], so the two cannot drift into cleaning up a
/// group differently: the mapping to a `TaskEnd` or a `ProcessRun` happens at
/// the edge.
#[cfg(all(unix, feature = "process"))]
struct ProcessEnd {
    /// What the observation saw.
    observation: ProcessObservation,
    /// The status the engine reported, when there is one.
    status: Option<ExitStatus>,
    /// The cleanup receipt and what it was based on.
    cleanup: ProcessCleanup,
    /// Captured stdout.
    stdout: CapturedStream,
    /// Captured stderr.
    stderr: CapturedStream,
}

/// Map a driven child's terminal observation to the supervisor's internal
/// task-end, so the public `TaskOutcome` is derived from one place.
#[cfg(all(unix, feature = "process"))]
fn task_end(end: ProcessEnd) -> TaskEnd {
    // The signal phase ran while the leader was unreaped; signals are over by the
    // time this is constructed. A `Pending` cleanup stays in this task through the
    // bounded post-reap, signal-zero probe.
    let ProcessEnd {
        observation,
        status,
        cleanup,
        ..
    } = end;
    match observation {
        ProcessObservation::Exited => match status {
            Some(ref observed) if observed.success() => TaskEnd::CompletedWithCleanup { cleanup },
            status => TaskEnd::Failed { status, cleanup },
        },
        ProcessObservation::Cancelled => TaskEnd::CancelledWithCleanup { cleanup },
        ProcessObservation::Deadline | ProcessObservation::Unobservable => TaskEnd::Failed {
            status: None,
            cleanup,
        },
    }
}

/// Drive one started child to a terminal observation, observing nothing.
///
/// This is the driver behind [`Supervisor::spawn_process`], whose body runs on
/// a spawned task that must be `Send` — and an observed driver's parameter is a
/// `dyn Fn`, which is not. The observation is therefore an argument of
/// [`drive_process_observed`] and *this* call supplies `None`, so there is one
/// body of process driving rather than two and the spawned path is a name for
/// the unobserved one rather than a second implementation of it.
#[cfg(all(unix, feature = "process"))]
async fn drive_process(
    clock: &Clock,
    child: OwnedChild,
    token: &CancellationToken,
    group: ProcessGroup<'static>,
    bounds: RunBounds,
) -> ProcessEnd {
    drive_process_observed(clock, child, token, group, bounds, None).await
}

/// The bounds one process run is held to, as its spec declares them.
///
/// One value rather than three parameters: [`Supervisor::prepare_process`]
/// resolves all three from the same spec, so a deadline can never travel with
/// another run's capture ceilings.
#[cfg(all(unix, feature = "process"))]
#[derive(Debug, Clone, Copy)]
struct RunBounds {
    /// How long the child may run before it is stopped.
    deadline: Option<Duration>,
    /// The most stdout bytes retained.
    out_limit: Option<NonZeroUsize>,
    /// The most stderr bytes retained.
    err_limit: Option<NonZeroUsize>,
}

/// Drive one started child to a terminal observation: drain its captured
/// streams, waiting for its exit (or the deadline, or cancellation), signal its
/// process group, and reap it.
///
/// This is the single body behind [`Supervisor::run_process`] and
/// [`Supervisor::run_process_observed`]. The three phases run concurrently as
/// [`join3`]: the two pipe reads must make progress while the child runs, or a
/// child that writes more than a pipe buffer would block forever waiting for a
/// reader that is waiting for it to exit.
///
/// `on_line` is forwarded to the **stdout** read only. It is not a second read
/// of the child's output: it is called from inside the read that retains that
/// output, on the bytes that read just observed, so an observation and a
/// capture can never disagree about what the child wrote.
///
/// The deadline and the two capture ceilings arrive as one [`RunBounds`],
/// resolved from a single spec by [`Supervisor::prepare_process`].
#[cfg(all(unix, feature = "process"))]
async fn drive_process_observed(
    clock: &Clock,
    mut child: OwnedChild,
    token: &CancellationToken,
    mut group: ProcessGroup<'static>,
    bounds: RunBounds,
    on_line: Option<LineObserver<'_>>,
) -> ProcessEnd {
    let RunBounds {
        deadline,
        out_limit,
        err_limit,
    } = bounds;
    let pid = child.group().ok();
    let capture_out = capture(child.stdout.take(), out_limit, on_line);
    let capture_err = capture(child.stderr.take(), err_limit, None);
    // Reading the pipes and waiting for the child are independent futures over
    // disjoint state; only the group is borrowed, and that borrow ends when
    // this future completes.
    let wait = async {
        let observation = match pid {
            Some(pid) => observe_pid_without_reaping(clock, pid, deadline, token).await,
            None => ProcessObservation::Unobservable,
        };
        if matches!(observation, ProcessObservation::Exited) {
            group.mark_exited();
        }
        let cleanup = group.cleanup().await;
        (observation, cleanup)
    };
    let (stdout, stderr, (observation, mut receipt)) = join3(capture_out, capture_err, wait).await;

    if matches!(receipt, CleanupReceipt::CleanupFailed)
        && !matches!(observation, ProcessObservation::Exited)
    {
        // Do not wait on a child whose group signal could not be delivered. The
        // owned child is dropped immediately after this return, and
        // `kill_on_drop` remains the direct-child fallback.
        let containment = group.containment().clone();
        drop(group);
        return ProcessEnd {
            observation,
            status: None,
            cleanup: ProcessCleanup {
                receipt,
                containment,
            },
            stdout,
            stderr,
        };
    }
    let (status, cleanup) = if matches!(receipt, CleanupReceipt::CleanupFailed) {
        // Keep the leader unreaped while the armed guard makes its final
        // synchronous kill attempt. The receipt reports the syscall failure;
        // dropping the child then relinquishes it to the platform's orphan
        // reaper rather than signalling a stale numeric group id.
        let containment = group.containment().clone();
        drop(group);
        let status = child.reap().await.ok();
        (
            status,
            ProcessCleanup {
                receipt,
                containment,
            },
        )
    } else {
        let status = child.reap().await.ok();
        group.mark_reaped();
        // Every unresolved arm goes through the post-reap pass, not only
        // `Pending`: a survivor list is exactly what a further observation can
        // clear, since the platform reaps what stopped running once the leader is
        // gone. No arm is promoted to `Confirmed` by anything but that pass.
        if !matches!(receipt, CleanupReceipt::CleanupConfirmed) {
            receipt = group.confirm_absence().await;
        }
        let containment = group.containment().clone();
        (
            status,
            ProcessCleanup {
                receipt,
                containment,
            },
        )
    };
    ProcessEnd {
        observation,
        status,
        cleanup,
        stdout,
        stderr,
    }
}

/// Read a captured pipe to EOF, retaining at most `limit` bytes and counting
/// every byte.
///
/// The pipe is drained even after the ceiling is reached, so a child that
/// writes far more than the ceiling cannot block on a full pipe. The retained
/// buffer is sized once to the ceiling, so the retained capacity is bounded by
/// the policy rather than by what the child wrote.
///
/// `on_line`, when given, is called once for each newline-terminated line of
/// **stdout** as it arrives, before the ceiling is consulted and whether or not
/// the bytes are retained. It is what lets a service's readiness come from what
/// the child printed rather than from a timer: the observer sees the same bytes
/// the capture retains, in the same order, at the moment they were read — so it
/// is an observation of the run rather than a second read of the child's
/// output. A trailing fragment with no newline is not delivered: a partial line
/// is not a line, and a caller waiting for one must keep waiting for the
/// newline rather than being woken by half a message.
#[cfg(all(unix, feature = "process"))]
async fn capture<R>(
    reader: Option<R>,
    limit: Option<NonZeroUsize>,
    on_line: Option<LineObserver<'_>>,
) -> CapturedStream
where
    R: AsyncRead + Unpin,
{
    let (Some(mut reader), Some(limit)) = (reader, limit) else {
        // No pipe (inherited or null) or no capture policy: nothing retained.
        return CapturedStream::default();
    };
    let cap = limit.get();
    let mut bytes: Vec<u8> = Vec::with_capacity(cap);
    let mut total: u64 = 0;
    // Whether any read carried more than the retained buffer had room for.
    let mut truncated = false;
    let mut buffer = [0_u8; 8192];
    // Bytes of the current line not yet terminated. Bounded by
    // `MAX_OBSERVED_LINE_BYTES`, so a child that writes an unterminated
    // megabyte cannot grow this without limit: the fragment is dropped once it
    // exceeds the bound and the count says how much was not observed.
    let mut fragment: Vec<u8> = Vec::new();
    let mut dropped_from_fragment: u64 = 0;
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => {
                // A `usize` wider than 64 bits saturates the counter, the same
                // contract `saturating_add` keeps for a long-lived stream.
                total = match u64::try_from(read) {
                    Ok(count) => total.saturating_add(count),
                    Err(_) => u64::MAX,
                };
                let Some(chunk) = buffer.get(..read) else {
                    continue;
                };
                if let Some(observe) = on_line {
                    for line in lines_of(chunk, &mut fragment, &mut dropped_from_fragment) {
                        observe(line);
                    }
                }
                let room = cap.saturating_sub(bytes.len());
                truncated |= read > room;
                if room > 0 {
                    let take = room.min(read);
                    if let Some(head) = buffer.get(..take) {
                        bytes.extend_from_slice(head);
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    CapturedStream::from_parts(bytes, total, truncated)
}

/// The longest unterminated line the stdout observer will hold.
///
/// A child printing a megabyte without a newline is a child that is not
/// speaking the line protocol an observer waits for, and retaining that
/// megabyte on its behalf would make the observer the unbounded buffer the
/// capture ceiling exists to prevent. 8 KiB is longer than any line a service
/// announces its readiness with, and longer than the pipe read buffer, so a line
/// that fits in one read is never split across the bound.
#[cfg(all(unix, feature = "process"))]
const MAX_OBSERVED_LINE_BYTES: usize = 8 * 1024;

/// Append `chunk` to `fragment`, yielding each newline-terminated line.
///
/// Split out of [`capture`] so the splitting rule is one definition rather than
/// one copy per captured stream, and so it can be exercised without a child.
///
/// `fragment` holds the unterminated tail of the last line. It is capped at
/// [`MAX_OBSERVED_LINE_BYTES`]: a longer tail is dropped rather than grown, and
/// `dropped` accumulates what was not observed so a caller can tell a short line
/// from a line this observer refused to assemble.
#[cfg(all(unix, feature = "process"))]
fn lines_of<'a>(
    chunk: &'a [u8],
    fragment: &'a mut Vec<u8>,
    dropped: &mut u64,
) -> impl Iterator<Item = &'a [u8]> {
    let mut complete = Vec::new();
    for (index, byte) in chunk.iter().enumerate() {
        if *byte != b'\n' {
            if fragment.len() < MAX_OBSERVED_LINE_BYTES {
                fragment.push(*byte);
            } else {
                *dropped = dropped.saturating_add(1);
            }
            continue;
        }
        let end = index.saturating_sub(fragment.len());
        if let Some(line) = chunk.get(end..index) {
            complete.push(strip_carriage_return(line));
        }
        fragment.clear();
    }
    complete.into_iter()
}

/// Drop one trailing carriage return, so a CRLF line and an LF line observe the
/// same bytes.
#[cfg(all(unix, feature = "process"))]
fn strip_carriage_return(line: &[u8]) -> &[u8] {
    match line.split_last() {
        Some((&b'\r', head)) => head,
        _ => line,
    }
}

/// Poll three futures to completion on the calling task, returning their
/// outputs together.
///
/// The engine's `join!` is behind the `macros` feature, which the `process`
/// feature does not require, so this is the small combinator that build needs.
/// Nothing is spawned and nothing is `Send`-bound: a child's two pipes must be
/// drained while it is observed, and the futures that do so borrow the caller's
/// child and group, which a spawned task could not.
#[cfg(all(unix, feature = "process"))]
async fn join3<A, B, C>(first: A, second: B, third: C) -> (A::Output, B::Output, C::Output)
where
    A: Future,
    B: Future,
    C: Future,
{
    let mut first = std::pin::pin!(first);
    let mut second = std::pin::pin!(second);
    let mut third = std::pin::pin!(third);
    let mut first_out: Option<A::Output> = None;
    let mut second_out: Option<B::Output> = None;
    let mut third_out: Option<C::Output> = None;
    std::future::poll_fn(|context| {
        if first_out.is_none()
            && let std::task::Poll::Ready(value) = first.as_mut().poll(context)
        {
            first_out = Some(value);
        }
        if second_out.is_none()
            && let std::task::Poll::Ready(value) = second.as_mut().poll(context)
        {
            second_out = Some(value);
        }
        if third_out.is_none()
            && let std::task::Poll::Ready(value) = third.as_mut().poll(context)
        {
            third_out = Some(value);
        }
        if first_out.is_some() && second_out.is_some() && third_out.is_some() {
            let first_value = first_out.take();
            let second_value = second_out.take();
            let third_value = third_out.take();
            return match (first_value, second_value, third_value) {
                (Some(first_value), Some(second_value), Some(third_value)) => {
                    std::task::Poll::Ready((first_value, second_value, third_value))
                }
                _ => std::task::Poll::Pending,
            };
        }
        std::task::Poll::Pending
    })
    .await
}

#[cfg(all(unix, feature = "process"))]
impl<'ops> ProcessGroup<'ops> {
    /// The guard over process group `group`, armed.
    ///
    /// `group` is the id [`start`] validated for the child it just started.
    fn of(
        group: i32,
        task: TaskId,
        lease: Lease,
        owners: Arc<CleanupOwners>,
    ) -> ProcessGroup<'static> {
        ProcessGroup {
            group,
            task,
            permit: Some(lease),
            owners,
            armed: true,
            leader_reaped: false,
            leader_exited: false,
            group_absent: false,
            containment: Containment::default(),
            signalled_pids: std::collections::BTreeSet::new(),
            #[cfg(all(target_os = "linux", feature = "process"))]
            cgroup: None,
            signaller: &NATIVE_GROUP_SIGNALLER,
            observer: &NATIVE_GROUP_OBSERVER,
            capture: &NATIVE_DESCENDANT_CAPTURE,
        }
    }

    /// The guard over process group `group`, armed, owning `scope`.
    ///
    /// The scoped form of [`Self::of`]: the same guard, plus the cgroup the
    /// spawn attached the leader to. Every cleanup path kills the scope first
    /// and observes it empty to earn the receipt.
    #[cfg(all(target_os = "linux", feature = "process"))]
    fn of_scoped(
        group: i32,
        task: TaskId,
        lease: Lease,
        owners: Arc<CleanupOwners>,
        scope: Option<CgroupScope>,
    ) -> ProcessGroup<'static> {
        let mut guard = Self::of(group, task, lease, owners);
        guard.cgroup = scope;
        guard
    }

    /// Capture the tree, signal the group and every captured pid, and observe
    /// until nothing captured is still running or the bounded drain is exhausted.
    ///
    /// Four phases per round, and the order is the whole design:
    ///
    /// 1. **Capture**, once per round and before any signal of that round. Before
    ///    the *first* signal the leader is still alive and its descendants still
    ///    name it as their parent; after the leader exits they are adopted
    ///    elsewhere and no walk from the leader reaches them. Within the drain,
    ///    re-capturing each round is what catches a fork that completed while an
    ///    earlier round's signal was in flight.
    /// 2. **Signal the group.** The floor, and the only thing that reaches a
    ///    member forked after the round's capture.
    /// 3. **Signal every captured pid.** A `setsid` descendant is in no group this
    ///    supervisor owns, so a pid signal is the only thing that reaches it.
    /// 4. **Observe** which captured pids are still *running**, as opposed to
    ///    present and unreaped.
    ///
    /// The rounds replace the 64-yield retry the group alone needed. Each round
    /// proves more than the last — it admits the forks the previous round's window
    /// let through, and re-signals anything the previous round's observation
    /// proved alive — so the bound is on *rounds of evidence* rather than on how
    /// long a scheduler happened to take. A delivered signal is still not proof of
    /// cleanup: only the observations settle it, and the group's own absence is
    /// settled after the reap by [`Self::confirm_absence`], because an unreaped
    /// zombie leader keeps its group present by construction.
    async fn cleanup(&mut self) -> CleanupReceipt {
        if self.leader_reaped {
            return CleanupReceipt::CleanupFailed;
        }
        if self.group <= 0 {
            return CleanupReceipt::CleanupFailed;
        }
        // The kernel owner first: one `cgroup.kill` write stops every member
        // the scope holds — including a `setsid` escapee — atomically, with no
        // fork race for them. What it could not hold (a descendant forked
        // before the attach) the capture rounds below still cover, and the
        // receipt names which owner ran.
        #[cfg(all(target_os = "linux", feature = "process"))]
        self.kill_scope();
        for round in 0..CONTAINMENT_ROUNDS {
            let tree = self.read_tree();
            if matches!(self.signal_group(), GroupSignal::Refused) {
                return CleanupReceipt::CleanupFailed;
            }
            if let Some(ref captured) = tree {
                self.signal_captured(captured);
                self.observe_captured(captured);
            }
            if self.no_survivors() {
                break;
            }
            if round.saturating_add(1) < CONTAINMENT_ROUNDS {
                yield_now().await;
            }
        }
        self.receipt()
    }

    /// Send SIGKILL to the scope's members through `cgroup.kill`.
    ///
    /// Best effort and first: a refused write leaves the scope untouched and
    /// the capture rounds own the tree, while a delivered one is what the
    /// receipt names. Never a failure — containment degrades, it does not
    /// refuse.
    #[cfg(all(target_os = "linux", feature = "process"))]
    fn kill_scope(&mut self) {
        let Some(scope) = self.cgroup.as_ref() else {
            return;
        };
        match scope.kill() {
            Ok(()) => self
                .containment
                .record_owner(ContainmentMechanism::CgroupKill),
            Err(error) => {
                lgwks_std::trace::debug!(%error, task = ?self.task, "cleanup: the scope kill was refused; the capture rounds own this tree");
            }
        }
    }

    /// Read the leader's descendants now, folding them into this report.
    ///
    /// `None` when no process table could be read at all, which is a **fact about
    /// the mechanism** rather than an empty tree: it records
    /// [`ResidualRisk::TableUnreadable`], leaves the group signal as the only
    /// thing this cleanup can rely on, and stops the per-process phases of the
    /// round rather than reporting "every descendant was".
    ///
    /// `None` without a read once the leader is known to have exited: its
    /// children were re-parented at the exit, so the walk could only come back
    /// empty, and on a host whose reading is a whole-table `ps` it would cost a
    /// process spawn per supervised process to learn nothing (#345, #347).
    fn read_tree(&mut self) -> Option<Capture> {
        if self.leader_exited {
            return None;
        }
        match self.capture.capture(self.group) {
            Ok(set) => {
                self.containment.record_capture(&set);
                Some(set)
            }
            Err(_) => {
                self.containment.note(ResidualRisk::TableUnreadable);
                None
            }
        }
    }

    /// Send SIGKILL to the group and report what the OS answered.
    fn signal_group(&mut self) -> GroupSignal {
        match self.signaller.signal(self.group) {
            Ok(()) => GroupSignal::Delivered,
            Err(error) if is_absence(&error) => {
                self.group_absent = true;
                GroupSignal::Absent
            }
            // `EPERM` after a delivered SIGKILL is not a termination refusal: on
            // macOS/BSD a process group whose leader is an unreaped zombie
            // (exactly the state the first signal produced) reports `EPERM` for a
            // further `killpg` while the group — the zombie leader included — is
            // still present. It is the same fact `exists` reports as `Ok(true)`:
            // the group is still there. Reporting `Failed` would claim the OS
            // refused a kill it already delivered, and would report a deadline
            // kill as a cleanup failure (INV-BOT-19). The absence is settled by
            // the post-reap signal-zero probe.
            Err(error) if is_present_but_unsignalable(&error) => GroupSignal::Present,
            Err(_) => GroupSignal::Refused,
        }
    }

    /// SIGKILL every captured pid this cleanup has not signalled, or that the last
    /// observation proved alive.
    ///
    /// The "or proved alive" is the T21 fence. A pid captured in an earlier round
    /// and already signalled is not signalled again on its own: its process may
    /// have died and its id been reissued, and signalling a number is signalling
    /// whoever holds it now. A pid the last observation reported **running** is
    /// that same process, still holding its id, so the repeat is safe — and is the
    /// only thing that reaches a descendant that ignored the first.
    fn signal_captured(&mut self, captured: &Capture) {
        let observed = self.containment.survivors.clone();
        for pid in &captured.pids {
            let fresh = !self.signalled_pids.contains(pid);
            if !fresh && !observed.contains(pid) {
                continue;
            }
            match self.capture.signal(*pid) {
                Ok(()) => {
                    self.signalled_pids.insert(*pid);
                    self.containment.record_signal();
                }
                // The process has ended: `ESRCH` is its answer rather than a
                // failure, and the pid is not signalled again.
                Err(error) if is_absence(&error) => {
                    self.signalled_pids.remove(pid);
                }
                // Anything else — including a refusal to signal somebody else's
                // process — leaves the pid unsignalled, and the round's own
                // observation then reports it: an unexplainable survivor belongs
                // in the receipt, not in a swallowed error.
                Err(_) => {}
            }
        }
    }

    /// Replace the survivor list with what the table says is still running.
    ///
    /// Signal-free, so it is safe to repeat and safe against a pid this supervisor
    /// no longer owns: the question is only ever asked about pids this cleanup
    /// captured. An unreadable observation claims nothing and reports every
    /// captured pid as running, the direction that understates the cleanup rather
    /// than overstating it.
    fn observe_captured(&mut self, captured: &Capture) {
        if captured.is_empty() {
            self.containment.record_survivors(&[]);
            return;
        }
        self.observe_running(&captured.pids);
    }

    /// Replace the survivor list with which of `pids` are still running.
    ///
    /// The one observation both phases make: [`Self::observe_captured`] asks it
    /// of a fresh capture, [`Self::confirm_absence`] of the pids an earlier
    /// observation proved running. An unreadable table records every pid asked
    /// about as running, the answer that understates the cleanup.
    fn observe_running(&mut self, pids: &[i32]) {
        match self.capture.running(pids) {
            Ok(running) => self.containment.record_survivors(&running),
            Err(_) => self.containment.record_survivors(pids),
        }
    }

    /// Whether no captured pid was still running at the last observation.
    fn no_survivors(&self) -> bool {
        self.containment.survivors.is_empty()
    }

    /// The receipt this cleanup has earned from what its rounds observed.
    ///
    /// One decision in one place, because the arms are three facts. A survivor
    /// outranks the group's own state — a `setsid` descendant is still a process
    /// this supervisor started, however absent its group became — and the group is
    /// reported absent only once something actually observed it absent.
    fn receipt(&mut self) -> CleanupReceipt {
        if !self.containment.survivors.is_empty() {
            return CleanupReceipt::CleanupSurvivors {
                survivors: self.containment.survivors.clone(),
            };
        }
        if self.group_absent {
            self.disarm();
            return CleanupReceipt::CleanupConfirmed;
        }
        CleanupReceipt::CleanupPending
    }

    /// Confirm absence after `child.wait()` has released the leader's pid.
    ///
    /// Both halves are **observation**: the probe sends no signal, so a reused
    /// identifier can never cause this supervisor to affect an unrelated group,
    /// and a still-present group consumes the same bounded budget and stays
    /// `Pending` rather than being promoted to proof of absence (INV-BOT-12,
    /// INV-BOT-13). The descendant half is observed for the same reason and
    /// because nothing more can be done about it here: the leader is reaped, so
    /// signalling any captured pid now could reach a reissued id.
    ///
    /// It observes the survivors the pinned phase captured and **does not capture
    /// again**. The leader's id is released by the reap, so a walk from it names
    /// whoever holds that number now — another process's children, attributed to
    /// this task — and the descendants it did have were re-parented away when it
    /// exited, so the walk could not find them either. It was also the dear half:
    /// a reaped root has no `/proc/<pid>/task`, so the Linux reading fell back to
    /// a whole-table `ps` spawn on every process a supervisor ran, which tripled
    /// `sim_review_path`'s 10,000-run tier on CI (62 s on main, 181 s with the
    /// re-capture). With no survivor the observation reads no table at all.
    ///
    /// The reap is what makes this pass able to succeed at all — an unreaped
    /// zombie leader keeps its group present, which is why the pinned phase could
    /// only ever report `Pending`.
    async fn confirm_absence(&mut self) -> CleanupReceipt {
        if !self.leader_reaped || self.group <= 0 {
            return CleanupReceipt::CleanupFailed;
        }
        // Adopted orphans first: a descendant whose parent died before the
        // cleanup was re-parented to this process by the subreaper flag, so it
        // is killed and reaped here rather than left for init. Only pids the
        // capture named while the leader lived are touched — an unreaped child
        // keeps its pid unreissued, so each one is provably still the process
        // this tree started.
        #[cfg(all(target_os = "linux", feature = "process"))]
        self.adopt_orphans().await;
        // Stranded adoptees of earlier trees — orphans adopted after their own
        // guard's pass — are collected here rather than left as rows `kill -0`
        // still reports present. Confirming, not single-pass: a stranded
        // adoptee still dying on the kernel's schedule when the sweep first
        // looks would otherwise survive into the observation below as a false
        // survivor, so the sweep waits out the deaths within the same bound
        // the adoption reap uses.
        #[cfg(all(target_os = "linux", feature = "process"))]
        let _collected = sweep_adopted_confirmed().await;
        // The scope's own confirmation before the rounds: an empty member
        // list is the kernel's evidence that nothing of the tree is left, and
        // a non-empty one joins the survivors the rounds observe, so an early
        // return below never claims a tree whose scope still holds members.
        #[cfg(all(target_os = "linux", feature = "process"))]
        self.confirm_scope();
        for round in 0..CONTAINMENT_ROUNDS {
            if matches!(self.observer.exists(self.group), Ok(false)) {
                self.group_absent = true;
            }
            if !self.no_survivors() {
                let survivors = self.containment.survivors.clone();
                self.observe_running(&survivors);
            }
            if self.group_absent && self.no_survivors() {
                return self.receipt();
            }
            if round.saturating_add(1) < CONTAINMENT_ROUNDS {
                yield_now().await;
            }
        }
        self.receipt()
    }

    /// Kill and reap the captured descendants this process adopted.
    ///
    /// One signal pass, bounded by the signalled set: orphans adopted after this
    /// call are not in it, and the receipt's rounds — not another pass — are
    /// what bound the work. A pid adopted here was captured while the leader
    /// lived and is an unreaped child now, so the signal cannot reach a
    /// stranger and the reap cannot steal a live leader's exit.
    ///
    /// The reap confirms rather than fires once: a signalled process dies on
    /// the kernel's schedule, not this task's, and under load the death lands
    /// after a single nonblocking reap attempted it — leaving a zombie no
    /// later pass collects while the receipt already claims the tree.
    /// [`reap_adopted`] waits for the deaths it caused, within
    /// [`ADOPTION_REAP_PASSES`], and this then observes only what is still
    /// genuinely alive; a pid that never dies is a survivor, not a missed reap.
    #[cfg(all(target_os = "linux", feature = "process"))]
    async fn adopt_orphans(&mut self) {
        let adopted = self.signal_adopted();
        if adopted.is_empty() {
            return;
        }
        let settled = reap_adopted(adopted).await;
        if !settled.unreaped.is_empty() {
            lgwks_std::trace::debug!(task = ?self.task, passes = settled.passes, unreaped = settled.unreaped.len(), "confirm: adoptees outlived the death bound; observing them");
            self.observe_running(&settled.unreaped);
        }
    }

    /// Signal the adopted orphans without observing them.
    ///
    /// The drop-time half of [`Self::adopt_orphans`]: no observation follows,
    /// because a dropped guard has no report to carry one, so every adopted
    /// pid is signalled once and reaped once, best effort. The same
    /// attribution rule applies — captured while the leader lived, an unreaped
    /// child now — so no stranger is reachable.
    #[cfg(all(target_os = "linux", feature = "process"))]
    fn kill_adopted(&mut self) {
        let adopted = self.signal_adopted();
        if adopted.is_empty() {
            return;
        }
        let _reaped = lgwks_std::process::reap_descendants(&adopted);
    }

    /// Signal the adopted descendants and name the owner that stopped them.
    ///
    /// The one signal these two paths share: which signalled pids this process
    /// adopted, each signalled once, and the subreaper adoption recorded as
    /// the mechanism. Returns the adopted pids for the caller to reap or
    /// observe; empty when nothing was adopted, so both callers stop there.
    #[cfg(all(target_os = "linux", feature = "process"))]
    fn signal_adopted(&mut self) -> Vec<i32> {
        let adopted = self.adopted_signalled();
        if adopted.is_empty() {
            return adopted;
        }
        self.containment
            .record_owner(ContainmentMechanism::SubreaperAdoption);
        for pid in &adopted {
            if self.capture.signal(*pid).is_ok() {
                self.containment.record_signal();
            }
        }
        adopted
    }

    /// Which signalled pids are currently children of this process.
    ///
    /// The one attribution query both adoption paths share: a pid that is
    /// both captured and adopted is provably still the same process, because
    /// the OS cannot reissue the pid of a child nobody reaped. Unreadable is
    /// empty rather than an error, because both callers degrade to the rounds
    /// rather than refusing.
    #[cfg(all(target_os = "linux", feature = "process"))]
    fn adopted_signalled(&self) -> Vec<i32> {
        if self.signalled_pids.is_empty() {
            return Vec::new();
        }
        let candidates: Vec<i32> = self.signalled_pids.iter().copied().collect();
        match lgwks_std::process::adopted_descendants(&candidates) {
            Ok(adopted) => adopted.into_iter().collect(),
            Err(error) => {
                lgwks_std::trace::debug!(%error, task = ?self.task, "cleanup: the adoption check was unreadable; the rounds own these pids");
                Vec::new()
            }
        }
    }

    /// Fold the scope's member list into the survivor report.
    ///
    /// Read before the absence rounds, so they observe what the kernel still
    /// holds: an empty scope confirms what the rounds are about to check, and
    /// a non-empty one is unioned with the survivors rather than replacing
    /// them, because the two readers — the kernel's membership and the table
    /// observation — answer different questions and neither absorbs the other.
    /// An unreadable scope leaves the rounds' answer standing.
    #[cfg(all(target_os = "linux", feature = "process"))]
    fn confirm_scope(&mut self) {
        let Some(scope) = self.cgroup.as_ref() else {
            return;
        };
        let members = match scope.members() {
            Ok(members) => members,
            Err(error) => {
                lgwks_std::trace::debug!(%error, task = ?self.task, "confirm: the scope member list was unreadable; the rounds own this tree");
                return;
            }
        };
        if members.is_empty() {
            return;
        }
        let mut survivors: std::collections::BTreeSet<i32> =
            self.containment.survivors.iter().copied().collect();
        survivors.extend(members);
        let survivors: Vec<i32> = survivors.into_iter().collect();
        self.containment.record_survivors(&survivors);
    }

    /// Signal the whole group and every captured pid as a drop-time fallback.
    ///
    /// A dropped future cannot await, so this is one synchronous pass rather than
    /// the drain above: capture once, name each captured pid once, then repeat the
    /// group signal a bounded number of times while the unreaped leader still pins
    /// the id. The repeats exist for the one thing a capture cannot reach — a
    /// member the leader forks after the capture and before it dies — and they are
    /// bounded rather than open because the leader's death is the event that ends
    /// the window.
    ///
    /// The capture is a blocking table read on this path: on a host whose reading
    /// is a `ps` invocation it costs one process spawn, which is a price worth
    /// paying on a terminal path that has just abandoned its task and a price the
    /// drain above would otherwise pay repeatedly. It is deliberately **not**
    /// routed through a blocking pool: a refused pool admission would silently
    /// degrade containment to the group alone at exactly the concurrency where
    /// containment matters most.
    fn kill(&mut self) {
        if self.leader_reaped || self.group <= 0 {
            return;
        }
        if let Some(ref captured) = self.read_tree() {
            for pid in &captured.pids {
                if self.capture.signal(*pid).is_ok() {
                    self.signalled_pids.insert(*pid);
                    self.containment.record_signal();
                }
            }
        }
        for _ in 0..GROUP_SIGNAL_ATTEMPTS {
            match self.signal_group() {
                // Absent and unsignalable are both "still there or already gone,
                // and neither is a reason to stop trying"; `Absent` ends it.
                GroupSignal::Absent => break,
                GroupSignal::Delivered | GroupSignal::Present => {
                    std::thread::yield_now();
                }
                GroupSignal::Refused => break,
            }
        }
        // The kernel owners, best effort and synchronous like the rest of this
        // path: the scope kill stops the members the capture above could not
        // name, and adopted orphans are signalled by pid. Neither allocates a
        // task or awaits, so neither can be refused by a runtime that is
        // already unwinding.
        #[cfg(all(target_os = "linux", feature = "process"))]
        self.kill_scope();
        #[cfg(all(target_os = "linux", feature = "process"))]
        self.kill_adopted();
        // Stranded adoptees, best effort and synchronous like the rest of this
        // path: a dropped guard cannot await, but it can wait on what is
        // already dead.
        #[cfg(all(target_os = "linux", feature = "process"))]
        let _collected = sweep_adopted();
    }

    /// What this cleanup read, signalled, and could not account for.
    ///
    /// Read by the driver to carry the report onto the outcome, and by [`Drop`] to
    /// hand it to the retained-cleanup registry: an obligation that outlives its
    /// task has to carry the same report its task ended with, or the settled
    /// receipt would claim a mechanism nobody ran.
    fn containment(&self) -> &Containment {
        &self.containment
    }

    /// Mark the group as already gone, so [`Drop`] does not signal it.
    fn disarm(&mut self) {
        self.armed = false;
    }

    /// Record that reaping released the group leader's numeric id.
    fn mark_reaped(&mut self) {
        self.leader_reaped = true;
    }

    /// Record that the leader exited on its own before the cleanup began.
    ///
    /// Every later capture is skipped and the report carries
    /// [`ResidualRisk::LeaderExited`], so a cleanup of a process that finished
    /// by itself claims the group it signalled and not a tree it never saw.
    fn mark_exited(&mut self) {
        self.leader_exited = true;
        self.containment.note(ResidualRisk::LeaderExited);
    }
}

#[cfg(all(unix, feature = "process"))]
impl Drop for ProcessGroup<'_> {
    /// Kill the group if a kill is still owed to it.
    ///
    /// This is the last line of the guarantee, and it is deliberately
    /// synchronous: a task aborted before it reached its own kill still takes
    /// its process group with it as its frame unwinds.
    fn drop(&mut self) {
        if self.armed && self.group > 0 {
            if !self.leader_reaped {
                self.kill();
            }
            if let Some(permit) = self.permit.take() {
                self.owners
                    .register(self.task, self.group, permit, self.containment.clone());
            }
        }
        // The cgroup scope, if any, drops with the guard: its members were
        // killed by the paths above, and the retained obligation the permit
        // transfers to probes the group, never the directory.
    }
}

/// Render a panic payload, keeping the head of it.
///
/// A payload is whatever the panicking code passed, so it is arbitrary text
/// rather than a message with a shape. The two `downcast`s cover what
/// `panic!` and `assert_eq!` actually produce; anything else is described
/// rather than dropped, because "this task panicked with something unrecognized"
/// is still better evidence than an empty string.
fn panic_message(payload: Box<dyn Any + Send + 'static>) -> String {
    if let Some(text) = payload.downcast_ref::<&'static str>() {
        return truncate_panic_message(text);
    }
    if let Some(text) = payload.downcast_ref::<String>() {
        return truncate_panic_message(text);
    }
    String::from("<panic payload was neither &str nor String>")
}

/// Cut `text` to [`MAX_PANIC_MESSAGE_CHARS`] characters, marking the cut.
///
/// The count is characters rather than bytes so the result is always valid
/// UTF-8, and the marker is a single character so a truncated message is never
/// mistaken for a complete one.
fn truncate_panic_message(text: &str) -> String {
    let mut kept: String = text.chars().take(MAX_PANIC_MESSAGE_CHARS).collect();
    if kept.len() < text.len() {
        kept.push('…');
    }
    kept
}

/// Run `body` repeatedly under `budget`, stopping when the budget runs out or
/// `token` is cancelled.
///
/// Every iteration is raced against `token` rather than merely checked before
/// it, so cancellation interrupts a body that is itself awaiting: the body's
/// future is dropped and the loop returns. That is the difference between a
/// loop that stops and a loop that stops *eventually*.
///
/// # Cancellation
///
/// A cancel is observed even when the budget is still unspent, and is reported
/// as [`Outcome::Cancelled`] rather than as exhaustion, so a caller can tell a
/// completed run from an interrupted one.
///
/// # Bounded poll
///
/// No single poll of this loop runs an unbounded number of iterations. After a
/// private interval of iterations the loop yields to the executor, so a
/// cancellable token is observably cancelled even when the body never suspends:
/// the task holding the token runs, the token is cancelled, and the loop exits
/// at its next iteration boundary. Without that, an immediately-ready body
/// holds the executor for the whole run and a cancellation ordered by another
/// task is not delivered until the budget is spent — on a current-thread
/// runtime, never.
///
/// This is the loop's own contract and it is the only part of the contract this
/// crate can enforce: a *body* that never returns from `poll` is noncooperative
/// user code, and no amount of yielding here can preempt it. See
/// [`Supervisor::shutdown`].
pub async fn repeat<F, Fut>(token: &CancellationToken, budget: Budget, body: F) -> Outcome
where
    F: FnMut(u64) -> Fut,
    Fut: Future<Output = ()>,
{
    repeat_on(&Clock::wall(), token, budget, body).await
}

/// [`repeat`] with the budget measured on a declared [`Clock`].
///
/// The same loop, and the same [`Outcome`], with one difference that matters:
/// `Budget::For` is compared against the clock's own timeline rather than
/// against a local monotonic instant. A wall clock is the default through
/// [`repeat`], so a caller that never asks for a clock observes the behaviour it
/// always did — real elapsed time — and a caller that supplies a
/// caller-advanceable one gets a budget that is exact and costs no real wait to
/// exhaust.
///
/// The iteration bound is unaffected: a count of iterations is a fact about the
/// body, not about time, and no clock governs it.
pub async fn repeat_on<F, Fut>(
    clock: &Clock,
    token: &CancellationToken,
    budget: Budget,
    mut body: F,
) -> Outcome
where
    F: FnMut(u64) -> Fut,
    Fut: Future<Output = ()>,
{
    // The iteration bound and the time bound are read once, outside the loop, and
    // neither uses an arithmetic operator: a saturating add cannot wrap, and a
    // budget of `Duration::MAX` is a deadline that never arrives rather than one
    // that arrives immediately.
    let iteration_limit = match budget {
        Budget::Iterations(limit) => Some(limit.get()),
        Budget::For(_) | Budget::Ongoing => None,
    };
    // The instant the budget is measured from, on the governing clock.
    let started_at = clock.now();
    let deadline = match budget {
        Budget::For(limit) => Some(started_at.saturating_add(limit)),
        Budget::Iterations(_) | Budget::Ongoing => None,
    };

    let mut iterations: u64 = 0;
    // Counts down to the next cooperative yield. A countdown rather than a
    // modulo of `iterations`: `clippy::modulo_arithmetic` and
    // `clippy::integer_division` are both forbidden workspace-wide, and a
    // countdown states the bound without either.
    let mut until_yield: u64 = YIELD_INTERVAL;
    loop {
        if token.is_cancelled() {
            return Outcome::Cancelled { iterations };
        }
        // Both bounds are "already spent" predicates rather than nested
        // conditionals, so the two ways a budget can run out read as one
        // decision and the loop has a single exit for each reason.
        let spent = iteration_limit.is_some_and(|limit| iterations >= limit)
            || deadline.is_some_and(|deadline| clock.now() >= deadline);
        if spent {
            return Outcome::Exhausted { iterations };
        }
        // The body is raced against the stop *and*, on a caller-advanceable
        // clock, against the bound.
        //
        // Without the second arm a suspended body would be checked against its
        // budget only after it returned — that is, never — and a body that waits
        // indefinitely would wait for a cancel the budget was supposed to have
        // delivered. A budget consulted only between iterations bounds nothing
        // that is suspended, which is most of what this loop exists to bound.
        //
        // The bound is polled on a short real interval rather than by a timer,
        // because a caller-advanceable clock's bound is not a real time at all:
        // no engine timer can be armed for it. The interval bounds how *late*
        // an advance is noticed, never whether it is — the loop re-reads the
        // clock, so an advance that lands at any point is observed.
        //
        // A wall clock takes the other arm, governed by its own timer and
        // unchanged from what this loop always did. Racing both for a wall clock
        // would double every wakeup for no difference in the reported outcome.
        if let Some(deadline) = deadline {
            if clock.source() != TimeSource::Wall {
                match wait_for_bound(clock, token, deadline, body(iterations)).await {
                    Bounded::Spent => return Outcome::Exhausted { iterations },
                    Bounded::Stopped => return Outcome::Cancelled { iterations },
                    Bounded::Finished => iterations = iterations.saturating_add(1),
                }
            } else {
                // The timer arm. Anchored on real elapsed time from now, which
                // is the wall clock's own reading and therefore the same value
                // the deadline above carries.
                // `timeout` rather than a `select!` against a sleep: the macro
                // is not part of every feature set this crate builds under, and
                // the two settle alike — a body that finishes at the instant
                // the budget runs out is counted, and the check at the top of
                // the loop then reports the budget spent.
                let remaining = deadline.saturating_sub(clock.now());
                match crate::rt::time::timeout(
                    remaining,
                    token.run_until_cancelled(body(iterations)),
                )
                .await
                {
                    Err(_elapsed) => return Outcome::Exhausted { iterations },
                    Ok(None) => return Outcome::Cancelled { iterations },
                    Ok(Some(())) => iterations = iterations.saturating_add(1),
                }
                continue;
            }
        } else {
            match token.run_until_cancelled(body(iterations)).await {
                Some(()) => iterations = iterations.saturating_add(1),
                None => return Outcome::Cancelled { iterations },
            }
        }
        // Hand the executor back periodically so a ready body cannot hold it
        // for the whole run. `yield_now` resolves on its second poll, so this
        // costs one extra poll per interval and never blocks: outside a runtime
        // the waker is woken immediately, inside one the task is placed behind
        // the tasks already ready to run, which is how the cancellation this
        // loop checks for at the top gets a chance to be performed.
        until_yield = until_yield.saturating_sub(1);
        if until_yield == 0 {
            until_yield = YIELD_INTERVAL;
            yield_now().await;
        }
    }
}

/// How one bounded wait on a caller-advanceable clock settled.
enum Bounded {
    /// The body returned.
    Finished,
    /// The logical clock reached `deadline`.
    Spent,
    /// The token was cancelled first.
    Stopped,
}

/// Run one body iteration under a logical deadline no engine timer can reach.
///
/// The polling interval is real time, and that is the point of the split between
/// the two clocks in this module: the *bound* is logical and exact, and the
/// *observation* of it is real and slightly late. A caller-advanceable clock
/// cannot be armed on any timer, so something has to wake up to notice — and
/// what wakes up reads the clock, so the bound itself is never approximated.
///
/// A one-millisecond poll is the observation granularity and not a budget: a
/// body that runs for an hour under a virtual clock is polled 3.6 million times,
/// which is the honest cost of asking a real executor to watch a counter nobody
/// real time is moving. A wall clock never reaches this function.
async fn wait_for_bound<Fut>(
    clock: &Clock,
    token: &CancellationToken,
    deadline: Duration,
    body: Fut,
) -> Bounded
where
    Fut: Future<Output = ()>,
{
    /// How often the logical clock is re-read while a body is suspended.
    const LOGICAL_POLL: Duration = Duration::from_millis(1);
    let mut body = std::pin::pin!(body);
    loop {
        if clock.now() >= deadline {
            return Bounded::Spent;
        }
        if crate::rt::time::timeout(LOGICAL_POLL, &mut body)
            .await
            .is_ok()
        {
            return Bounded::Finished;
        }
        // The stop is checked on every pass rather than only when the bound is
        // met, so a cancelled loop under a virtual clock stops in a
        // millisecond rather than when its budget happens to be reached — which
        // under a clock nobody is advancing is never.
        if token.is_cancelled() {
            return Bounded::Stopped;
        }
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

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    #[cfg(all(unix, feature = "process"))]
    use super::CleanupOwners;
    #[cfg(feature = "process")]
    use super::CleanupReceipt;
    #[cfg(all(not(unix), feature = "process"))]
    use super::ProcessSpec;
    #[cfg(feature = "script")]
    use super::SpawnRefused;
    #[cfg(all(unix, feature = "process"))]
    use super::SupervisorCancelled;
    use super::{
        Budget, Clock, Lease, MAX_PANIC_MESSAGE_CHARS, Outcome, Supervisor, TaskId, TaskOutcome,
        TrySpawnRefusal, repeat, truncate_panic_message,
    };
    #[cfg(all(unix, feature = "process"))]
    use super::{
        CONTAINMENT_ROUNDS, Containment, ContainmentMechanism, DescendantCapture, EPERM, ESRCH,
        GroupObserver, GroupSignaller, ProcessGroup, ResidualRisk,
    };
    use crate::rt::cancel::CancellationToken;
    use crate::rt::runtime::block_on;
    use crate::rt::task::yield_now;
    use std::future::Future;
    use std::future::pending;
    use std::num::NonZeroU64;
    use std::sync::Arc;
    #[cfg(any(feature = "process", feature = "script"))]
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::task::Poll;
    use std::time::Duration;

    #[cfg(all(not(unix), feature = "process"))]
    #[test]
    fn non_unix_process_group_spawn_is_refused_as_unsupported() {
        let mut supervisor = Supervisor::new(1);
        let result = block_on(supervisor.spawn_process(&ProcessSpec::new("unused")));
        assert_eq!(
            result.as_ref().err().map(std::io::Error::kind),
            Some(std::io::ErrorKind::Unsupported),
            "non-Unix callers receive an explicit safe refusal before starting a process"
        );
        assert_eq!(supervisor.stats().spawned, 0);
    }

    #[cfg(all(not(unix), feature = "process"))]
    #[test]
    fn non_unix_process_group_run_is_refused_as_unsupported() {
        let mut supervisor = Supervisor::new(1);
        let result = block_on(supervisor.run_process(&ProcessSpec::new("unused")));
        assert!(
            result.as_ref().err().is_some_and(|error| match *error {
                crate::rt::process::ProcessRunError::NotStarted { ref source } => {
                    source.kind() == std::io::ErrorKind::Unsupported
                }
                _ => false,
            }),
            "the result-bearing runner must refuse on non-Unix before the fork, got {result:?}"
        );
        assert_eq!(
            supervisor.stats().spawned,
            0,
            "a refused run must start nothing"
        );
    }

    /// Panic on the calling task, carrying `message` as the payload.
    ///
    /// `resume_unwind` and not `panic!`: the workspace forbids `panic` outright
    /// with no suppression path, and this is the documented form for a
    /// deliberate panic — it reports on the task that observes it and never
    /// aborts the process. A `panic-in-a-task` test that let the panic reach
    /// the test harness would prove nothing about the API that is supposed to
    /// report it, and would take the rest of the suite down with it.
    fn explode(message: &'static str) {
        std::panic::resume_unwind(Box::new(message));
    }

    /// A budget of `iterations`. Zero cannot be expressed, so it reads as the
    /// unbounded case rather than as a budget that never runs.
    fn budget_of(iterations: u64) -> Budget {
        match NonZeroU64::new(iterations) {
            Some(limit) => Budget::Iterations(limit),
            None => Budget::Ongoing,
        }
    }

    #[cfg(all(unix, feature = "process"))]
    struct RecordingGroupSignaller {
        calls: AtomicUsize,
    }

    #[cfg(all(unix, feature = "process"))]
    impl GroupSignaller for RecordingGroupSignaller {
        fn signal(&self, _group: i32) -> std::io::Result<()> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    /// A group whose leader is an unreaped zombie: macOS/BSD report `EPERM` for
    /// a further `killpg` while the group is still present.
    #[cfg(all(unix, feature = "process"))]
    struct EpermGroupSignaller;

    #[cfg(all(unix, feature = "process"))]
    impl GroupSignaller for EpermGroupSignaller {
        fn signal(&self, _group: i32) -> std::io::Result<()> {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "the group leader is a zombie",
            ))
        }
    }

    /// An error that is neither absence nor the zombie case: a genuine refusal.
    #[cfg(all(unix, feature = "process"))]
    struct UnexpectedGroupSignaller;

    #[cfg(all(unix, feature = "process"))]
    impl GroupSignaller for UnexpectedGroupSignaller {
        fn signal(&self, _group: i32) -> std::io::Result<()> {
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "injected unexpected failure",
            ))
        }
    }

    #[cfg(all(unix, feature = "process"))]
    struct SequenceGroupObserver {
        calls: AtomicUsize,
        present_before_absent: usize,
    }

    #[cfg(all(unix, feature = "process"))]
    impl GroupObserver for SequenceGroupObserver {
        fn exists(&self, _group: i32) -> std::io::Result<bool> {
            let call = self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(call < self.present_before_absent)
        }
    }

    /// A capture driven by the test rather than by the process table.
    ///
    /// Every combination the drain can meet — a tree nobody can read, a pid that
    /// refuses its signal, a pid that stays running whatever is sent, a truncated
    /// reading — is a value here rather than a process, which is what makes the
    /// decision table testable at all. The counters are how a test observes what
    /// the drain *did*, as opposed to what it reported.
    #[cfg(all(unix, feature = "process"))]
    #[derive(Default)]
    struct ScriptedCapture {
        /// The pids every reading names.
        tree: Vec<i32>,
        /// The pids a signal is refused for, as `ESRCH` would be for a process
        /// that has already ended.
        gone: Vec<i32>,
        /// The pids that keep running whatever is sent to them.
        immortal: Vec<i32>,
        /// Whether every reading fails, the way a target with no process table
        /// reports.
        unreadable: bool,
        /// Whether the reading is truncated at its bound.
        truncated: bool,
        /// How many readings the drain took.
        reads: AtomicUsize,
        /// How many signals the drain delivered.
        signals: AtomicUsize,
    }

    #[cfg(all(unix, feature = "process"))]
    impl ScriptedCapture {
        /// A reading that names `tree`.
        fn of(tree: &[i32]) -> Self {
            Self {
                tree: tree.to_vec(),
                ..Self::default()
            }
        }

        /// Readings, so a test can count them.
        fn reads(&self) -> usize {
            self.reads.load(Ordering::Relaxed)
        }

        /// Delivered signals, so a test can count them.
        fn signals(&self) -> usize {
            self.signals.load(Ordering::Relaxed)
        }
    }

    #[cfg(all(unix, feature = "process"))]
    impl DescendantCapture for ScriptedCapture {
        fn capture(&self, _root: i32) -> std::io::Result<super::Capture> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            if self.unreadable {
                let refusal = Err(std::io::Error::other("injected unreadable process table"));
                lgwks_std::trace::debug!(
                    error = ?refusal.as_ref().err(),
                    "scripted capture: returning an error to the caller"
                );
                return refusal;
            }
            Ok(super::Capture {
                mechanism: ContainmentMechanism::ProcessTableSnapshot,
                pids: self.tree.clone(),
                truncated: self.truncated,
            })
        }

        fn signal(&self, pid: i32) -> std::io::Result<()> {
            if self.gone.contains(&pid) {
                let refusal = Err(std::io::Error::from_raw_os_error(3));
                lgwks_std::trace::debug!(
                    pid,
                    error = ?refusal.as_ref().err(),
                    "scripted signal: returning an error to the caller"
                );
                return refusal;
            }
            self.signals.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn running(&self, pids: &[i32]) -> std::io::Result<Vec<i32>> {
            Ok(pids
                .iter()
                .copied()
                .filter(|pid| self.immortal.contains(pid))
                .collect())
        }
    }

    #[cfg(all(unix, feature = "process"))]
    /// A report from a whole reading that found no descendant.
    ///
    /// What a real drain carries when the process table was read and the tree was
    /// empty. `Containment::default()` is the *absence* of a report — a group-only
    /// cleanup — and a registry entry holding one is never clean, which is the
    /// point: an obligation that never captured anything has not shown it stopped
    /// the whole tree.
    fn whole_capture() -> Containment {
        Containment {
            mechanism: ContainmentMechanism::ProcessTableSnapshot,
            captured: 0,
            signalled: 0,
            survivors: Vec::new(),
            residual: None,
        }
    }

    #[cfg(all(unix, feature = "process"))]
    /// The task ids one drive settled, for the attribution assertions.
    ///
    /// The settled pairs carry the report each obligation ended with; these tests
    /// are about which task a receipt belongs to, and a shared reader keeps the
    /// pattern off both call sites.
    fn settled_tasks(settled: &[(TaskId, Containment)]) -> Vec<TaskId> {
        settled.iter().map(|pair| pair.0).collect()
    }

    #[cfg(all(unix, feature = "process"))]
    /// A group armed for one test, with every seam it drives.
    ///
    /// One constructor rather than seven literals: the drain grew three fields, and
    /// a literal per test is how half of them end up unset in exactly one arm.
    fn armed_group<'ops>(
        group: i32,
        task: TaskId,
        leader_reaped: bool,
        permit: Option<Lease>,
        owners: Arc<CleanupOwners>,
        seams: Seams<'ops>,
    ) -> ProcessGroup<'ops> {
        ProcessGroup {
            group,
            task,
            permit,
            owners,
            armed: true,
            leader_reaped,
            leader_exited: false,
            group_absent: false,
            containment: Containment::default(),
            signalled_pids: std::collections::BTreeSet::new(),
            #[cfg(all(target_os = "linux", feature = "process"))]
            cgroup: None,
            signaller: seams.signaller,
            observer: seams.observer,
            capture: seams.capture,
        }
    }

    #[cfg(all(unix, feature = "process"))]
    /// The three boundaries a cleanup test drives, as one value.
    ///
    /// They are one thing to a reader: what the drain is allowed to *see* and
    /// *touch*. Passing them as three arguments beside the group's own identity
    /// made every call site a list of eight values whose order nothing checked.
    #[derive(Copy, Clone)]
    struct Seams<'ops> {
        /// The group-signalling boundary.
        signaller: &'ops dyn GroupSignaller,
        /// The group-existence boundary.
        observer: &'ops dyn GroupObserver,
        /// The per-process containment boundary.
        capture: &'ops dyn DescendantCapture,
    }

    #[cfg(all(unix, feature = "process"))]
    /// The seams a group-only test drives: a signaller and observer of the test's
    /// own, and a reading that names nothing.
    fn group_seams<'ops>(
        signaller: &'ops dyn GroupSignaller,
        observer: &'ops dyn GroupObserver,
    ) -> Seams<'ops> {
        Seams {
            signaller,
            observer,
            capture: &NO_CAPTURE,
        }
    }

    #[cfg(all(unix, feature = "process"))]
    /// The seams a containment test drives, with its own reading.
    fn capture_seams<'ops>(
        signaller: &'ops dyn GroupSignaller,
        observer: &'ops dyn GroupObserver,
        capture: &'ops dyn DescendantCapture,
    ) -> Seams<'ops> {
        Seams {
            signaller,
            observer,
            capture,
        }
    }

    #[cfg(all(unix, feature = "process"))]
    /// The reading every group-only test uses: a table that named nothing.
    ///
    /// The reading every group-only test uses: a table that named nothing.
    ///
    /// A `static` rather than a `const`, because the seams hold a reference to it
    /// and a `const` would be a fresh temporary per use. Its counters are never
    /// read by a test that uses it: a group-only test is about the group.
    static NO_CAPTURE: ScriptedCapture = ScriptedCapture {
        tree: Vec::new(),
        gone: Vec::new(),
        immortal: Vec::new(),
        unreadable: false,
        truncated: false,
        reads: AtomicUsize::new(0),
        signals: AtomicUsize::new(0),
    };

    #[cfg(all(unix, feature = "process"))]
    struct FailingGroupObserver;

    #[cfg(all(unix, feature = "process"))]
    impl GroupObserver for FailingGroupObserver {
        fn exists(&self, _group: i32) -> std::io::Result<bool> {
            Err(std::io::Error::other("injected observation failure"))
        }
    }

    /// A guard over the fixed test group 42, with its own lease and owners.
    #[cfg(all(unix, feature = "process"))]
    fn test_group<'ops>(
        signaller: &'ops dyn GroupSignaller,
        observer: &'ops dyn GroupObserver,
        leader_reaped: bool,
    ) -> ProcessGroup<'ops> {
        armed_group(
            42,
            TaskId(0),
            leader_reaped,
            test_lease(),
            Arc::new(CleanupOwners::default()),
            group_seams(signaller, observer),
        )
    }

    /// A containment test's guard: test group 42 with an unreaped leader, its
    /// own lease and owners, and the reading the test scripts.
    #[cfg(all(unix, feature = "process"))]
    fn capture_group<'ops>(
        signaller: &'ops dyn GroupSignaller,
        observer: &'ops dyn GroupObserver,
        capture: &'ops dyn DescendantCapture,
    ) -> ProcessGroup<'ops> {
        armed_group(
            42,
            TaskId(0),
            false,
            test_lease(),
            Arc::new(CleanupOwners::default()),
            capture_seams(signaller, observer, capture),
        )
    }

    /// An observer that reports the group present `present_before_absent`
    /// times, then absent.
    #[cfg(all(unix, feature = "process"))]
    const fn sequence(present_before_absent: usize) -> SequenceGroupObserver {
        SequenceGroupObserver {
            calls: AtomicUsize::new(0),
            present_before_absent,
        }
    }

    /// A signaller that counts its calls and always succeeds.
    #[cfg(all(unix, feature = "process"))]
    const fn recording() -> RecordingGroupSignaller {
        RecordingGroupSignaller {
            calls: AtomicUsize::new(0),
        }
    }

    #[cfg(all(unix, feature = "process"))]
    fn test_lease() -> Option<Lease> {
        let permit = Arc::new(crate::rt::sync::Semaphore::new(1))
            .try_acquire_owned()
            .ok()?;
        Some(Lease::plain(permit))
    }

    #[cfg(all(unix, feature = "process"))]
    #[test]
    fn a_reaped_or_recycled_group_id_is_never_signalled() {
        let signaller = recording();
        let observer = sequence(0);
        let mut group = test_group(&signaller, &observer, true);
        assert_eq!(
            block_on(group.cleanup()),
            CleanupReceipt::CleanupFailed,
            "the stale identity is refused rather than signalled"
        );
        drop(group);
        assert_eq!(
            signaller.calls.load(Ordering::Relaxed),
            0,
            "cleanup and Drop must not signal after the owner has reaped the leader"
        );
    }

    #[cfg(all(unix, feature = "process"))]
    #[test]
    fn an_unsignalable_present_group_stays_pending_rather_than_failed() {
        // macOS/BSD report EPERM for a further killpg against a group whose
        // leader is an unreaped zombie. That is "the group is still there", not
        // "the OS refused the kill", and reading it as a failure would report a
        // deadline stop as a cleanup failure on those platforms.
        let signaller = EpermGroupSignaller;
        let observer = sequence(usize::MAX);
        let mut group = test_group(&signaller, &observer, false);
        assert_eq!(
            block_on(group.cleanup()),
            CleanupReceipt::CleanupPending,
            "an unsignalable but still-present group is pending, not a failed kill"
        );
        assert!(
            group.armed,
            "an unresolved group must stay armed for the drop-time fallback"
        );
        drop(group);
    }

    #[cfg(all(unix, feature = "process"))]
    #[test]
    fn an_unexpected_signal_error_is_a_failed_cleanup() {
        let signaller = UnexpectedGroupSignaller;
        let observer = sequence(0);
        let mut group = test_group(&signaller, &observer, false);
        assert_eq!(
            block_on(group.cleanup()),
            CleanupReceipt::CleanupFailed,
            "an unexpected errno remains a refused termination"
        );
        drop(group);
    }

    /// A group whose drain reports a named survivor, and the receipt that says so.
    #[cfg(all(unix, feature = "process"))]
    #[test]
    fn a_descendant_that_keeps_running_is_named_in_the_receipt() {
        let signaller = recording();
        let observer = sequence(usize::MAX);
        let capture = ScriptedCapture {
            tree: vec![70, 71],
            immortal: vec![71],
            ..ScriptedCapture::default()
        };
        let mut group = capture_group(&signaller, &observer, &capture);
        assert_eq!(
            block_on(group.cleanup()),
            CleanupReceipt::CleanupSurvivors {
                survivors: vec![71],
            },
            "a captured descendant that ignored every signal must be named, not absorbed \
             into a pending group"
        );
        let report = group.containment();
        assert_eq!(
            report.captured(),
            2,
            "the report must account for both captured pids"
        );
        assert_eq!(
            report.signalled(),
            1 + CONTAINMENT_ROUNDS,
            "one signal for the descendant that stopped and one per round for the one that \
             did not: a repeat is only ever sent while an observation proves the pid alive"
        );
        assert_eq!(
            report.survivors(),
            [71],
            "the survivor list is the running pid and nothing else"
        );
        assert_eq!(
            report.mechanism(),
            ContainmentMechanism::ProcessTableSnapshot,
            "the mechanism that read the table is the one reported"
        );
        assert_eq!(
            report.residual_risk(),
            None,
            "a whole reading with no survivor carries no residual risk"
        );
        assert!(
            !report.is_complete(),
            "a named survivor is by definition an incomplete containment"
        );
        // Nothing more can be done about it after the reap, so the pass observes
        // rather than signals and keeps the receipt it earned. It observes the
        // survivor by pid and never walks the tree again: a walk from a reaped
        // leader names whoever holds its id now.
        let pinned_reads = capture.reads();
        group.mark_reaped();
        assert_eq!(
            block_on(group.confirm_absence()),
            CleanupReceipt::CleanupSurvivors {
                survivors: vec![71],
            },
            "a survivor is never promoted to a clean cleanup by a later observation"
        );
        assert_eq!(
            capture.reads(),
            pinned_reads,
            "the post-reap pass observes the captured survivor and reads no tree"
        );
        assert_eq!(
            capture.signals(),
            1 + CONTAINMENT_ROUNDS,
            "the survivor is signalled once in the round that captured it and once per later \
             round, and only because an observation proves it alive; the descendant that \
             stopped is signalled exactly once"
        );
        assert!(
            group.armed,
            "an unresolved cleanup stays armed for its registry transfer"
        );
        drop(group);
    }

    /// A captured descendant that had already ended is not a survivor.
    #[cfg(all(unix, feature = "process"))]
    #[test]
    fn a_captured_pid_that_had_ended_is_not_reported_as_a_survivor() {
        let signaller = recording();
        let observer = sequence(0);
        let capture = ScriptedCapture {
            tree: vec![70],
            gone: vec![70],
            ..ScriptedCapture::default()
        };
        let mut group = capture_group(&signaller, &observer, &capture);
        assert_eq!(
            block_on(group.cleanup()),
            CleanupReceipt::CleanupPending,
            "the group is still pinned by its unreaped leader, so the pinned phase is pending"
        );
        let pinned_reads = capture.reads();
        group.mark_reaped();
        assert_eq!(
            block_on(group.confirm_absence()),
            CleanupReceipt::CleanupConfirmed,
            "an absent group and no running descendant is a complete cleanup"
        );
        assert_eq!(
            capture.reads(),
            pinned_reads,
            "a clean post-reap pass reads no tree: on Linux a walk from a reaped leader \
             was a whole-table `ps` spawn per process"
        );
        assert!(
            group.containment().is_complete(),
            "the report must name the mechanism it ran and carry no survivor"
        );
        drop(group);
    }

    /// A leader that exited on its own has had its children re-parented, so the
    /// cleanup reads no table, still signals the group, and names why its report
    /// is not the tree (#347).
    #[cfg(all(unix, feature = "process"))]
    #[test]
    fn a_leader_that_exited_on_its_own_reads_no_table_and_claims_no_tree() {
        let signaller = recording();
        let observer = sequence(0);
        let capture = ScriptedCapture {
            tree: vec![70],
            ..ScriptedCapture::default()
        };
        let mut group = capture_group(&signaller, &observer, &capture);
        group.mark_exited();
        assert_eq!(
            block_on(group.cleanup()),
            CleanupReceipt::CleanupPending,
            "the exited leader is unreaped, so its group is still pinned and pending"
        );
        assert_eq!(
            capture.reads(),
            0,
            "a walk from an exited leader can name nothing, so no table is read"
        );
        assert_eq!(
            capture.signals(),
            0,
            "nothing was named, so no pid is signalled"
        );
        assert!(
            signaller.calls.load(Ordering::Relaxed) >= 1,
            "the group is still signalled: a member that stayed in it is still reachable"
        );
        assert_eq!(
            group.containment().residual_risk(),
            Some(ResidualRisk::LeaderExited),
            "the report names the limit rather than presenting an empty capture as an empty tree"
        );
        assert_eq!(
            group.containment().mechanism(),
            ContainmentMechanism::ProcessGroupOnly,
            "no table was read"
        );
        group.mark_reaped();
        assert_eq!(
            block_on(group.confirm_absence()),
            CleanupReceipt::CleanupConfirmed,
            "the group itself is observed gone after the reap"
        );
        assert!(
            !group.containment().is_complete(),
            "a confirmed group is not a confirmed tree when the tree was never seen"
        );
        drop(group);
    }

    /// A pid is signalled once per drain, not once per observation of it.
    #[cfg(all(unix, feature = "process"))]
    #[test]
    fn a_captured_pid_is_signalled_once_and_not_once_per_observation() {
        let signaller = recording();
        let observer = sequence(usize::MAX);
        let capture = ScriptedCapture::of(&[70, 71, 72]);
        let mut group = capture_group(&signaller, &observer, &capture);
        assert_eq!(
            block_on(group.cleanup()),
            CleanupReceipt::CleanupPending,
            "nothing is running, so the pinned phase is pending on the group alone"
        );
        assert_eq!(
            capture.signals(),
            3,
            "three captured pids, three signals: a repeat would be a signal to an id whose \
             process has since ended"
        );
        assert_eq!(
            capture.reads(),
            1,
            "the drain stops on the first round once nothing captured is running"
        );
        drop(group);
    }

    /// No process table at all is a stated limit, not an empty tree.
    #[cfg(all(unix, feature = "process"))]
    #[test]
    fn an_unreadable_process_table_leaves_the_group_as_the_only_mechanism() {
        let signaller = recording();
        let observer = sequence(usize::MAX);
        let capture = ScriptedCapture {
            unreadable: true,
            ..ScriptedCapture::default()
        };
        let mut group = capture_group(&signaller, &observer, &capture);
        assert_eq!(
            block_on(group.cleanup()),
            CleanupReceipt::CleanupPending,
            "the group was signalled and nothing else could be read"
        );
        let report = group.containment();
        assert_eq!(
            report.mechanism(),
            ContainmentMechanism::ProcessGroupOnly,
            "no table was read, so the mechanism must say the group was all there was"
        );
        assert_eq!(
            report.residual_risk(),
            Some(ResidualRisk::TableUnreadable),
            "the limit is named rather than left to be inferred from an empty capture"
        );
        assert_eq!(
            report.captured(),
            0,
            "nothing was captured, so nothing may be reported as captured"
        );
        assert!(
            !report.is_complete(),
            "a group-only cleanup never claims the whole tree"
        );
        drop(group);
    }

    /// A truncated reading is a prefix, and the receipt says so.
    #[cfg(all(unix, feature = "process"))]
    #[test]
    fn a_truncated_capture_is_reported_as_a_prefix_and_not_as_the_tree() {
        let signaller = recording();
        let observer = sequence(usize::MAX);
        let capture = ScriptedCapture {
            tree: vec![70, 71],
            truncated: true,
            ..ScriptedCapture::default()
        };
        let mut group = capture_group(&signaller, &observer, &capture);
        assert_eq!(
            block_on(group.cleanup()),
            CleanupReceipt::CleanupPending,
            "the group is pinned by its unreaped leader"
        );
        assert_eq!(
            group.containment().residual_risk(),
            Some(ResidualRisk::CaptureTruncated),
            "a reading stopped at its bound must be reported as a prefix"
        );
        assert!(
            !group.containment().is_complete(),
            "a prefix cannot claim the whole tree"
        );
        drop(group);
    }

    #[cfg(all(unix, feature = "process"))]
    #[test]
    fn bounded_group_termination_keeps_cleanup_owned_until_absence_is_observed() {
        let signaller = recording();
        let observer = sequence(1);
        let mut group = test_group(&signaller, &observer, false);
        assert_eq!(
            block_on(group.cleanup()),
            CleanupReceipt::CleanupPending,
            "the bounded termination receipt must not claim observed absence"
        );
        let before_reap = signaller.calls.load(Ordering::Relaxed);
        assert_eq!(
            before_reap, 1,
            "the drain stops as soon as nothing captured is still running, so a group with no \
             descendants is signalled once and not once per configured round"
        );
        assert!(
            group.armed,
            "exhausting the signal budget is not proof that the group disappeared"
        );
        group.mark_reaped();
        assert_eq!(
            block_on(group.confirm_absence()),
            CleanupReceipt::CleanupConfirmed,
            "a signal-zero probe after reaping observes eventual group absence"
        );
        assert_eq!(observer.calls.load(Ordering::Relaxed), 2);
        drop(group);
        assert_eq!(
            signaller.calls.load(Ordering::Relaxed),
            before_reap,
            "no signal may follow reaping and release of the numeric id"
        );
    }

    #[cfg(all(unix, feature = "process"))]
    #[test]
    fn pending_cleanup_owner_keeps_its_permit_until_later_absence_receipt()
    -> Result<(), Box<dyn std::error::Error>> {
        use super::CleanupOwners;
        use crate::rt::sync::Semaphore;

        let semaphore = Arc::new(Semaphore::new(1));
        let Some(permit) = Arc::clone(&semaphore).try_acquire_owned().ok() else {
            return Err("the test semaphore started without its one permit".into());
        };
        let owners = Arc::new(CleanupOwners::default());
        let task = TaskId(9);
        let cleanup_observer = sequence(usize::MAX);
        let signaller = recording();
        let mut group = armed_group(
            42,
            task,
            false,
            Some(Lease::plain(permit)),
            Arc::clone(&owners),
            group_seams(&signaller, &cleanup_observer),
        );

        assert_eq!(
            block_on(group.cleanup()),
            CleanupReceipt::CleanupPending,
            "exhausting bounded signal attempts remains pending"
        );
        group.mark_reaped();
        assert_eq!(
            block_on(group.confirm_absence()),
            CleanupReceipt::CleanupPending,
            "a bounded post-reap pass cannot claim absence"
        );
        let later_observer = sequence(1);
        drop(group);
        assert_eq!(
            semaphore.available_permits(),
            0,
            "dropping a pending task-local guard must transfer, not release, its lease"
        );
        assert_eq!(owners.pending_count(), 1, "cleanup remains owned");
        assert!(
            owners.drive(&FailingGroupObserver, |_| false).is_empty(),
            "an observation error cannot release cleanup ownership"
        );
        assert_eq!(semaphore.available_permits(), 0);
        assert!(
            owners.drive(&later_observer, |_| false).is_empty(),
            "a still-present group cannot produce a terminal receipt"
        );
        assert_eq!(semaphore.available_permits(), 0);
        assert!(
            owners
                .drive(&later_observer, |candidate| candidate == task)
                .is_empty(),
            "absence before the task report must retain attribution and capacity"
        );
        assert_eq!(semaphore.available_permits(), 0);
        let settled = owners.drive(&later_observer, |_| false);
        // Attribution and capacity are this test's subject; what the report says
        // about the tree is the containment family's subject.
        assert_eq!(
            settled_tasks(&settled),
            vec![task],
            "later absence is attributed to its task"
        );
        assert_eq!(
            semaphore.available_permits(),
            1,
            "terminal proof releases capacity"
        );
        assert_eq!(owners.pending_count(), 0);
        assert_eq!(
            signaller.calls.load(Ordering::Relaxed),
            1,
            "one signal from the drain, and none after the reap: the id is released then, and \
             the drop-time fallback must not signal a number the OS may reissue"
        );
        Ok(())
    }

    #[cfg(all(unix, feature = "process"))]
    #[test]
    fn cleanup_failed_transfers_its_lease_until_absence_is_observed()
    -> Result<(), Box<dyn std::error::Error>> {
        use super::CleanupOwners;
        use crate::rt::sync::Semaphore;

        let semaphore = Arc::new(Semaphore::new(1));
        let Some(permit) = Arc::clone(&semaphore).try_acquire_owned().ok() else {
            return Err("the test semaphore started without its one permit".into());
        };
        let owners = Arc::new(CleanupOwners::default());
        let task = TaskId(10);
        let observer = sequence(0);
        let signaller = recording();
        let mut group = armed_group(
            43,
            task,
            true,
            Some(Lease::plain(permit)),
            Arc::clone(&owners),
            group_seams(&signaller, &observer),
        );

        assert_eq!(
            block_on(group.cleanup()),
            CleanupReceipt::CleanupFailed,
            "a guard whose leader was reaped cannot signal a possibly reused id"
        );
        drop(group);
        assert_eq!(owners.pending_count(), 1, "failed cleanup remains owned");
        assert_eq!(semaphore.available_permits(), 0);
        assert_eq!(
            settled_tasks(&owners.drive(&observer, |_| false)),
            vec![task],
            "an absent group settles the obligation it was transferred for"
        );
        assert_eq!(semaphore.available_permits(), 1);
        assert_eq!(signaller.calls.load(Ordering::Relaxed), 0);
        Ok(())
    }

    /// An adoptee another waiter reaped first is settled on the first pass,
    /// not waited on for the whole death bound.
    ///
    /// The other waiter is the test's own `wait`, standing in for a second
    /// drain's sweep: after it, `waitpid` on the pid answers `ECHILD`, which is
    /// what held a drain for 400 passes and had shutdown abort it.
    #[cfg(all(target_os = "linux", feature = "process"))]
    #[test]
    fn an_adoptee_another_waiter_reaped_is_settled_on_the_first_pass()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut child = std::process::Command::new("sleep").arg("30").spawn()?;
        let pid = i32::try_from(child.id())?;
        child.kill()?;
        let _status = child.wait()?;

        let settled = block_on(super::reap_adopted(vec![pid]));

        assert!(
            settled.unreaped.is_empty(),
            "a pid this process no longer holds is not owed a reap: {:?}",
            settled.unreaped
        );
        assert_eq!(
            settled.passes, 1,
            "it is settled by the pass that finds it gone, not by the bound"
        );
        Ok(())
    }

    /// The control: an adoptee still this process's child is reaped here, so
    /// settling a pid nobody holds did not stop the reap of one somebody does.
    #[cfg(all(target_os = "linux", feature = "process"))]
    #[test]
    fn a_killed_adoptee_still_held_is_reaped_by_the_drain() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut child = std::process::Command::new("sleep").arg("30").spawn()?;
        let pid = i32::try_from(child.id())?;
        child.kill()?;

        let settled = block_on(super::reap_adopted(vec![pid]));

        assert!(
            settled.unreaped.is_empty(),
            "a killed child dies inside the bound and is reaped: {:?}",
            settled.unreaped
        );
        assert!(
            lgwks_std::process::adopted_descendants(&[pid])?.is_empty(),
            "the reap collected the zombie, so the pid is no longer this process's child"
        );
        Ok(())
    }

    /// A quiet window outlasts the longest wait one drain makes on its own
    /// bounds, so a drain still inside a bound it was given is never aborted as
    /// stalled (INV-BOT-156).
    #[cfg(all(target_os = "linux", feature = "process"))]
    #[test]
    fn the_cleanup_grace_outlasts_a_drains_own_longest_wait() {
        let reap = super::ADOPTION_REAP_INTERVAL.saturating_mul(super::ADOPTION_REAP_PASSES);
        let sweep = super::ADOPTION_REAP_INTERVAL.saturating_mul(super::SWEEP_CONFIRM_PASSES);
        assert!(
            super::PROCESS_CLEANUP_GRACE > reap.saturating_add(sweep),
            "grace {:?} must outlast the adoption reap {reap:?} plus the sweep {sweep:?}",
            super::PROCESS_CLEANUP_GRACE
        );
    }

    #[cfg(all(unix, feature = "process"))]
    #[test]
    fn shutdown_report_keeps_pending_cleanup_and_emits_later_terminal_receipt()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::rt::sync::Semaphore;

        let semaphore = Arc::new(Semaphore::new(1));
        let Some(permit) = Arc::clone(&semaphore).try_acquire_owned().ok() else {
            return Err("the test semaphore started without its one permit".into());
        };
        let task = TaskId(11);
        let report = block_on(Supervisor::new(1).shutdown());
        report
            .cleanup_owners
            .register(task, 44, Lease::plain(permit), whole_capture());
        assert_eq!(report.pending_cleanup_count(), 1);
        assert!(
            !report.is_clean(),
            "an unresolved native resource is not clean"
        );
        let Err(mut report) = report.into_outcomes() else {
            return Err("into_outcomes consumed a report with pending cleanup".into());
        };
        assert_eq!(
            report.pending_cleanup_count(),
            1,
            "Err must retain ownership"
        );
        assert_eq!(semaphore.available_permits(), 0);

        assert_eq!(
            report.reap_pending_cleanups_with(&FailingGroupObserver),
            0,
            "an observer error is not a terminal receipt"
        );
        let observer = sequence(1);
        assert_eq!(report.reap_pending_cleanups_with(&observer), 0);
        assert_eq!(report.pending_cleanup_count(), 1);
        assert_eq!(report.reap_pending_cleanups_with(&observer), 1);
        assert_eq!(report.pending_cleanup_count(), 0);
        assert_eq!(semaphore.available_permits(), 1);
        assert!(report.is_clean());
        let outcomes = match report.into_outcomes() {
            Ok(outcomes) => outcomes,
            Err(_) => return Err("settled cleanup report remained unavailable".into()),
        };
        assert_eq!(
            outcomes,
            vec![TaskOutcome::CleanupSettled {
                task,
                cleanup: CleanupReceipt::CleanupConfirmed,
                containment: whole_capture(),
            }]
        );
        Ok(())
    }

    /// Drive the current task until nothing the supervisor started is still
    /// running.
    ///
    /// Returns `false` if the spin cap was reached, which would mean a task
    /// never completed. A test that asserts on the return value turns a hang
    /// into a failure instead of a timeout.
    ///
    /// It must be awaited on the runtime that spawned the tasks. [`block_on`]
    /// builds a current-thread runtime per call, so a task spawned in one call
    /// is aborted when that call's runtime is dropped and cannot be observed
    /// from the next, which is why every test here wraps its whole body in a
    /// single `block_on` rather than reaching for one per step.
    async fn settle(supervisor: &mut Supervisor) -> bool {
        let mut spins: u32 = 0;
        loop {
            supervisor.reap();
            if supervisor.stats().in_flight() == 0 {
                return true;
            }
            if spins >= 100_000 {
                return false;
            }
            spins = spins.saturating_add(1);
            yield_now().await;
        }
    }

    /// Drain every terminal outcome the supervisor is holding, oldest first.
    fn drain(supervisor: &mut Supervisor) -> Vec<TaskOutcome> {
        let mut outcomes = Vec::new();
        while let Some(outcome) = supervisor.next_report() {
            outcomes.push(outcome);
        }
        outcomes
    }

    /// The identities of every outcome satisfying `keep`, in completion order.
    ///
    /// Attribution is asserted through this rather than by matching on the
    /// outcome variants: completion order between two tasks that finish in the
    /// same scheduling pass is the engine's business, not this module's
    /// contract, so a test that pinned it would be pinning the wrong thing.
    fn tasks_where(outcomes: &[TaskOutcome], keep: impl Fn(&TaskOutcome) -> bool) -> Vec<TaskId> {
        outcomes
            .iter()
            .filter(|outcome| keep(outcome))
            .map(TaskOutcome::task)
            .collect()
    }

    /// Yield to the executor until `predicate` holds, or `limit` yields have
    /// happened.
    ///
    /// Returns whether the predicate held. A bounded spin rather than a timer:
    /// the executor under test here is the one a timer would need, so waiting
    /// for the thing being measured is the failure mode this helper exists to
    /// avoid.
    async fn yield_until(mut predicate: impl FnMut() -> bool, limit: u32) -> bool {
        let mut yields: u32 = 0;
        while !predicate() {
            if yields >= limit {
                return false;
            }
            yields = yields.saturating_add(1);
            yield_now().await;
        }
        true
    }

    /// Start a task that counts heartbeat increments until it is cancelled.
    ///
    /// It only advances when the executor schedules it, which is what makes it
    /// an observation of executor sharing rather than of wall-clock time.
    async fn spawn_heartbeat(supervisor: &mut Supervisor, beats: &Arc<AtomicU64>) {
        let beats = Arc::clone(beats);
        supervisor
            .spawn(move |token| async move {
                while !token.is_cancelled() {
                    beats.fetch_add(1, Ordering::SeqCst);
                    yield_now().await;
                }
            })
            .await;
    }

    #[test]
    fn a_large_finite_budget_still_yields_to_a_sibling_task() {
        // A body that resolves immediately never suspends the loop, so before
        // the loop yielded, one poll of it ran the entire budget and no other
        // task on the executor got to run at all. The budget is finite so the
        // old behaviour is a failed assertion here rather than a hung suite; a
        // current-thread runtime was the case that deadlocked outright, because
        // the task that would have cancelled could not even be scheduled.
        const BUDGET: u64 = 1_000_000;
        block_on(async {
            let beats = Arc::new(AtomicU64::new(0));
            let ran = Arc::new(AtomicU64::new(0));
            // The heartbeat count the body saw on its *first* iteration, and the
            // highest it saw on any later one. The heartbeat advancing between
            // two of the loop's own iterations is the observation that the
            // executor was shared: one uninterruptible poll of the loop sees a
            // single frozen value for its whole run, whatever that value is.
            let first_seen = Arc::new(AtomicU64::new(u64::MAX));
            let later_seen = Arc::new(AtomicU64::new(0));

            let mut supervisor = Supervisor::new(4);
            spawn_heartbeat(&mut supervisor, &beats).await;

            let body_ran = Arc::clone(&ran);
            let body_first = Arc::clone(&first_seen);
            let body_later = Arc::clone(&later_seen);
            let body_beats = Arc::clone(&beats);
            supervisor
                .spawn_repeating(budget_of(BUDGET), move |_tick| {
                    let body_ran = Arc::clone(&body_ran);
                    let body_first = Arc::clone(&body_first);
                    let body_later = Arc::clone(&body_later);
                    let body_beats = Arc::clone(&body_beats);
                    async move {
                        body_ran.fetch_add(1, Ordering::SeqCst);
                        let now = body_beats.load(Ordering::SeqCst);
                        if now < body_first.load(Ordering::SeqCst) {
                            body_first.store(now, Ordering::SeqCst);
                        }
                        if now > body_later.load(Ordering::SeqCst) {
                            body_later.store(now, Ordering::SeqCst);
                        }
                    }
                })
                .await;

            // Give the repeating worker its first poll before asserting on what
            // it observed: a worker cancelled before it starts proves nothing.
            assert!(
                yield_until(|| ran.load(Ordering::SeqCst) > 0, 1_000).await,
                "the repeating worker never took a single iteration"
            );
            assert!(
                yield_until(
                    || later_seen.load(Ordering::SeqCst) > first_seen.load(Ordering::SeqCst),
                    1_000
                )
                .await,
                "the repeating body ran {} of its {BUDGET} iterations seeing the heartbeat count \
                 frozen at {}, so one poll of it held the executor for the whole run",
                ran.load(Ordering::SeqCst),
                first_seen.load(Ordering::SeqCst)
            );

            supervisor.shutdown().await;
        });
    }

    #[test]
    fn a_cancel_from_a_sibling_task_stops_an_ongoing_ready_body() {
        // The case from the issue, in miniature: a loop whose body resolves
        // immediately, so it never suspends of its own accord, and a canceller
        // on a *different* task. The canceller can only run if the loop hands
        // the executor back, so one uninterruptible poll of the loop makes the
        // cancel unobservable — which on a current-thread runtime is not a
        // delay, it is a deadlock, because the canceller may never run at all.
        //
        // The body carries a brake so an unbounded loop still ends when the
        // cancel cannot land, turning the old behaviour into a failed assertion
        // rather than a hung suite. The assertions then distinguish the two:
        // the brake is three orders of magnitude above what a yielding loop
        // needs.
        const BRAKE: u64 = 1_000_000;
        block_on(async {
            let root = CancellationToken::new();
            let ran = Arc::new(AtomicU64::new(0));

            let mut supervisor = Supervisor::new(2);
            // The canceller holds the root, so its `cancel` reaches the loop's
            // child token. It waits for the loop's first iteration before
            // cancelling, so the cancel provably lands on a running loop rather
            // than on a task that never started.
            let canceller_root = root.clone();
            let canceller_ran = Arc::clone(&ran);
            supervisor
                .spawn(move |_task_token| async move {
                    while canceller_ran.load(Ordering::SeqCst) == 0 {
                        yield_now().await;
                    }
                    canceller_root.cancel();
                })
                .await;

            let loop_ran = Arc::clone(&ran);
            let loop_root = root.clone();
            let brake = root.clone();
            let outcome = repeat(&loop_root, Budget::Ongoing, move |_tick| {
                let brake = brake.clone();
                let loop_ran = Arc::clone(&loop_ran);
                async move {
                    let count = loop_ran.fetch_add(1, Ordering::SeqCst).saturating_add(1);
                    if count >= BRAKE {
                        brake.cancel();
                    }
                }
            })
            .await;

            assert!(
                outcome.was_cancelled(),
                "the loop ended as {outcome:?} rather than as cancelled"
            );
            let iterations = outcome.iterations();
            assert!(
                iterations < BRAKE,
                "the loop ran to its own {BRAKE}-iteration brake ({iterations} iterations) \
                 instead of observing the cancel from the sibling task"
            );

            // Drain the canceller, then check the body is not invoked again:
            // the cancel is a boundary, not a hint.
            assert!(
                settle(&mut supervisor).await,
                "the canceller task never finished"
            );
            let settled = ran.load(Ordering::SeqCst);
            for _ in 0..64 {
                yield_now().await;
            }
            assert_eq!(
                ran.load(Ordering::SeqCst),
                settled,
                "the body was invoked again after the loop observed the cancel"
            );
        });
    }

    #[test]
    fn shutdown_lands_on_an_ongoing_ready_body() {
        // The shutdown half of the same defect: `Supervisor::shutdown` cancels
        // and drains, and before the loop yielded it could not even be reached
        // until the repeating task returned on its own.
        //
        // The brake holds the loop's *own* token — the one `repeat` races — so
        // an unbounded loop whose cancel cannot land ends at the brake rather
        // than running forever, which is what keeps this a failed assertion
        // instead of a hung suite.
        const BRAKE: u64 = 1_000_000;
        block_on(async {
            let ran = Arc::new(AtomicU64::new(0));
            let mut supervisor = Supervisor::new(1);
            let body_ran = Arc::clone(&ran);
            supervisor
                .spawn(move |token| async move {
                    let brake = token.clone();
                    let _outcome = repeat(&token, Budget::Ongoing, move |_tick| {
                        let brake = brake.clone();
                        let body_ran = Arc::clone(&body_ran);
                        async move {
                            let count = body_ran.fetch_add(1, Ordering::SeqCst).saturating_add(1);
                            if count >= BRAKE {
                                brake.cancel();
                            }
                        }
                    })
                    .await;
                })
                .await;

            // Prove the loop is running before shutting down, so the shutdown
            // lands on a live body rather than on a task that never polled.
            assert!(
                yield_until(|| ran.load(Ordering::SeqCst) > 0, 1_000).await,
                "the repeating worker never took a single iteration"
            );
            supervisor.shutdown().await;

            let iterations = ran.load(Ordering::SeqCst);
            assert!(
                iterations < BRAKE,
                "the loop ran to its own {BRAKE}-iteration brake ({iterations} iterations) \
                 instead of being stopped by the shutdown"
            );
        });
    }

    #[test]
    fn an_already_cancelled_token_never_invokes_the_body() {
        let token = CancellationToken::new();
        token.cancel();
        let ran = AtomicU64::new(0);
        let body_ran = &ran;
        let outcome = block_on(repeat(&token, Budget::Ongoing, move |_tick| async move {
            body_ran.fetch_add(1, Ordering::SeqCst);
        }));
        assert_eq!(
            outcome,
            Outcome::Cancelled { iterations: 0 },
            "a token cancelled before the loop starts must stop it with no iterations"
        );
        assert_eq!(
            ran.load(Ordering::SeqCst),
            0,
            "the body must not be invoked at all past a cancellation boundary"
        );
    }

    #[test]
    fn a_budget_of_iterations_stops_at_the_limit() {
        let token = CancellationToken::new();
        let ticks = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&ticks);
        let outcome = block_on(repeat(&token, budget_of(3), move |_tick| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
            }
        }));
        assert_eq!(
            outcome,
            Outcome::Exhausted { iterations: 3 },
            "a three-iteration budget must run the body exactly three times"
        );
        assert_eq!(
            ticks.load(Ordering::SeqCst),
            3,
            "the body must have been entered once per iteration"
        );
    }

    /// Run an ongoing `repeat` whose body cancels its own token on entry and
    /// then runs `rest`.
    fn repeat_cancelling_itself<Rest, Fut>(rest: Rest) -> Outcome
    where
        Rest: Fn() -> Fut,
        Fut: Future<Output = ()>,
    {
        let token = CancellationToken::new();
        let trigger = token.clone();
        block_on(repeat(&token, Budget::Ongoing, move |_tick| {
            trigger.cancel();
            rest()
        }))
    }

    #[test]
    fn an_ongoing_budget_stops_when_the_token_is_cancelled() {
        let outcome = repeat_cancelling_itself(|| async {});
        assert_eq!(
            outcome,
            Outcome::Cancelled { iterations: 1 },
            "a cancel during the first iteration must end the loop after it"
        );
    }

    #[test]
    fn cancellation_interrupts_a_body_that_is_still_awaiting() {
        // The body never resolves. If cancellation were only observed between
        // iterations, the loop would have to await it to completion and the
        // test would hang rather than fail.
        let outcome = repeat_cancelling_itself(pending::<()>);
        assert!(
            outcome.was_cancelled(),
            "an in-flight body must be dropped at the cancel, got {outcome:?}"
        );
    }

    #[test]
    fn a_budget_of_duration_expires() {
        let token = CancellationToken::new();
        let outcome = block_on(repeat(
            &token,
            Budget::For(Duration::ZERO),
            move |_tick| async {},
        ));
        assert_eq!(
            outcome,
            Outcome::Exhausted { iterations: 0 },
            "a zero duration must expire before the first iteration"
        );
    }

    #[test]
    fn a_supervisor_reports_what_it_has_run() {
        block_on(async {
            let mut supervisor = Supervisor::new(2);
            supervisor.spawn(|_token| async {}).await;
            supervisor.spawn(|_token| async {}).await;
            assert!(settle(&mut supervisor).await, "both tasks must finish");
            let stats = supervisor.stats();
            assert_eq!(stats.spawned, 2, "both tasks must be counted as spawned");
            assert_eq!(
                stats.completed, 2,
                "both tasks must be counted as completed"
            );
            assert_eq!(
                stats.succeeded, 2,
                "both tasks returned, so both must be counted as succeeded"
            );
            assert_eq!(stats.in_flight(), 0, "nothing must remain in flight");
            assert_eq!(stats.refused, 0, "nothing must have been refused");
        });
    }

    #[test]
    fn a_cancelled_supervisor_refuses_admission_without_constructing_the_body() {
        block_on(async {
            // Cancellation is the terminal admission state: after it, no body
            // constructor runs and no slot is taken, on either spawn path. The
            // constructor is the observable, not the body: a body that writes a
            // marker or takes a lock would prove the old behavior, where the
            // constructor ran before anything looked at the token.
            let mut supervisor = Supervisor::new(2);
            supervisor.cancel();

            let built = Arc::new(AtomicU64::new(0));
            supervisor
                .spawn({
                    let built = Arc::clone(&built);
                    move |_token| {
                        built.fetch_add(1, Ordering::Relaxed);
                        async {}
                    }
                })
                .await;
            let stats = supervisor.stats();
            assert_eq!(
                built.load(Ordering::Relaxed),
                0,
                "a cancelled supervisor must never construct the body it refuses"
            );
            assert_eq!(stats.spawned, 0, "a refused spawn must not be placed");
            assert_eq!(
                stats.refused, 1,
                "the waiting-spawn refusal must be counted"
            );

            let built_now = Arc::new(AtomicU64::new(0));
            let refused = supervisor.try_spawn({
                let built_now = Arc::clone(&built_now);
                move |_token| {
                    built_now.fetch_add(1, Ordering::Relaxed);
                    async {}
                }
            });
            assert_eq!(
                refused,
                Err(TrySpawnRefusal::Cancelled),
                "the immediate path must name cancellation, not capacity"
            );
            assert_eq!(
                built_now.load(Ordering::Relaxed),
                0,
                "the immediate path must also refuse before the constructor"
            );
            assert_eq!(supervisor.stats().refused, 2, "both refusals counted");
        });
    }

    /// Fill `supervisor`'s only slot with a body that waits for the returned
    /// gate and then sets the returned done flag.
    async fn occupy(supervisor: &mut Supervisor) -> (Arc<AtomicBool>, Arc<AtomicBool>) {
        let gate = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        supervisor
            .spawn({
                let gate = Arc::clone(&gate);
                let done = Arc::clone(&done);
                move |_token| async move {
                    while !gate.load(Ordering::Relaxed) {
                        yield_now().await;
                    }
                    done.store(true, Ordering::Relaxed);
                }
            })
            .await;
        (gate, done)
    }

    #[test]
    fn cancellation_closes_admission_even_after_a_slot_frees() {
        block_on(async {
            // The order is the point: cancel first, then let the only slot
            // free. A supervisor that checks capacity but not cancellation
            // admits at the freeing of the slot — the constructor below runs
            // and the counter moves. The fence must refuse at the owned
            // admission point regardless of what the pool is doing.
            let mut supervisor = Supervisor::new(1);
            let (gate, done) = occupy(&mut supervisor).await;
            supervisor.cancel();
            gate.store(true, Ordering::Relaxed);
            // Let the occupier finish and its permit return to the pool: the
            // body's own flag says it ran to its end, and the yields give the
            // wrapper around it — which holds the permit — the polls it needs
            // to complete behind it. The supervisor's counters are not the
            // signal here: a cancelled supervisor stops reaping, but the
            // permit lives in the task wrapper, not in the counters.
            for _ in 0..256 {
                if done.load(Ordering::Relaxed) {
                    break;
                }
                yield_now().await;
            }
            assert!(
                done.load(Ordering::Relaxed),
                "the occupier must have ended before the admission attempt"
            );

            let built = Arc::new(AtomicU64::new(0));
            let refused = supervisor.try_spawn({
                let built = Arc::clone(&built);
                move |_token| {
                    built.fetch_add(1, Ordering::Relaxed);
                    async {}
                }
            });
            assert_eq!(
                refused,
                Err(TrySpawnRefusal::Cancelled),
                "a freed slot is not an admission offer after cancellation"
            );
            assert_eq!(
                built.load(Ordering::Relaxed),
                0,
                "no body may be constructed after cancellation, slot or not"
            );
            assert_eq!(supervisor.stats().spawned, 1, "only the occupier ran");
        });
    }

    #[test]
    fn a_cancelled_supervisor_with_a_full_pool_refuses_without_constructing() {
        block_on(async {
            // The pool is full *and* the supervisor is cancelled. The wait
            // must not park: cancellation is checked before the pool, so the
            // refusal returns while the occupier still holds the only slot,
            // and the constructor never runs. Without any cancellation
            // fence this parks forever and the test never finishes.
            let mut supervisor = Supervisor::new(1);
            let (gate, done) = occupy(&mut supervisor).await;
            supervisor.cancel();

            let built = Arc::new(AtomicU64::new(0));
            let refused_before = supervisor.stats().refused;
            supervisor
                .spawn({
                    let built = Arc::clone(&built);
                    move |_token| {
                        built.fetch_add(1, Ordering::Relaxed);
                        async {}
                    }
                })
                .await;
            assert_eq!(
                built.load(Ordering::Relaxed),
                0,
                "no body may be constructed for a cancelled supervisor, full pool or not"
            );
            assert_eq!(
                supervisor.stats().refused,
                refused_before + 1,
                "the waiting-spawn refusal must be counted"
            );
            assert_eq!(supervisor.stats().spawned, 1, "only the occupier ran");

            gate.store(true, Ordering::Relaxed);
            for _ in 0..256 {
                if done.load(Ordering::Relaxed) {
                    break;
                }
                yield_now().await;
            }
        });
    }

    /// Cancellation closes the native fork, not just the async placement:
    /// after it, `spawn_process` names cancellation and no child runs, so
    /// the first instruction of a refused command does not execute (issue
    /// #143 R11). Unix only, like the spawn path itself.
    #[cfg(all(unix, feature = "process"))]
    #[test]
    fn a_cancelled_supervisor_starts_no_process() {
        use super::ProcessSpec;

        block_on(async {
            // Nanos plus a monotone sequence: not a pid (reused by the OS).
            static MARK_SEQ: AtomicU64 = AtomicU64::new(0);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or_else(
                    |before| before.duration().as_nanos(),
                    |since| since.as_nanos(),
                );
            let seq = MARK_SEQ.fetch_add(1, Ordering::Relaxed);
            let marker = std::env::temp_dir().join(format!("lgwks-bot-cancel-{nanos}-{seq}.mark"));

            let mut supervisor = Supervisor::new(2);
            supervisor.cancel();
            let spawned_before = supervisor.stats().spawned;
            let refused_before = supervisor.stats().refused;

            let mut spec = ProcessSpec::new("sh");
            spec.arg("-c").arg(format!("touch {}", marker.display()));
            let refusal = supervisor.spawn_process(&spec).await;
            assert!(
                refusal.is_err(),
                "a cancelled supervisor must not start the process it was asked for"
            );
            assert!(
                refusal
                    .as_ref()
                    .err()
                    .and_then(std::io::Error::get_ref)
                    .and_then(|cause| cause.downcast_ref::<SupervisorCancelled>())
                    .is_some(),
                "the refusal must name cancellation, got {refusal:?}"
            );
            assert_eq!(
                supervisor.stats().spawned,
                spawned_before,
                "no process may be placed after cancellation"
            );
            assert_eq!(
                supervisor.stats().refused,
                refused_before + 1,
                "the process refusal must be counted"
            );
            assert!(
                !marker.exists(),
                "no child ran: the marker {} must be absent",
                marker.display()
            );
            // The absence above is the assertion; the removal only avoids
            // litter, and either outcome leaves the path gone.
            let removed = std::fs::remove_file(&marker);
            assert!(
                removed.is_ok() || !marker.exists(),
                "the marker path must be gone: {}",
                marker.display()
            );
        });
    }

    #[test]
    fn try_spawn_refuses_at_the_bound_instead_of_growing() {
        block_on(async {
            // The single slot is held by a body that never resolves, so the
            // refusal is attempted while the supervisor is genuinely full.
            let mut supervisor = Supervisor::new(1);
            let held = supervisor.try_spawn(|_token| async {
                pending::<()>().await;
            });
            assert!(held.is_ok(), "the first spawn must take the only slot");
            let refused = supervisor.try_spawn(|_token| async {});
            assert_eq!(
                refused,
                Err(TrySpawnRefusal::AtCapacity),
                "a second spawn past the bound must be refused, not queued"
            );
            let stats = supervisor.stats();
            assert_eq!(stats.refused, 1, "the refusal must be counted");
            assert_eq!(stats.spawned, 1, "a refused body must never be spawned");
            supervisor.cancel();
        });
    }

    #[test]
    fn the_retained_set_stays_at_the_bound_across_many_spawns() {
        block_on(async {
            // Far more spawns than the bound. A `JoinSet` retains a finished
            // task until it is joined, so without reaping on every entry point
            // the in-flight count would climb to the total instead of staying
            // at the bound.
            let mut supervisor = Supervisor::new(1);
            for _ in 0..64 {
                supervisor.spawn(|_token| async {}).await;
            }
            let stats = supervisor.stats();
            assert_eq!(stats.spawned, 64, "every spawn must be counted");
            assert!(
                stats.in_flight() <= 2,
                "the retained set must stay at the bound, not grow with total \
                 spawns; got {} in flight of {} spawned",
                stats.in_flight(),
                stats.spawned
            );
            assert!(
                settle(&mut supervisor).await,
                "the remaining tasks must finish"
            );
            assert_eq!(
                supervisor.stats().in_flight(),
                0,
                "settling must leave nothing in flight"
            );
        });
    }

    #[test]
    fn dropping_a_supervisor_cancels_the_tasks_it_owns() {
        block_on(async {
            let probe = {
                let mut supervisor = Supervisor::new(1);
                supervisor
                    .spawn(|_token| async {
                        pending::<()>().await;
                    })
                    .await;
                supervisor.child_token()
            };
            assert!(
                probe.is_cancelled(),
                "a task's token must be cancelled when the supervisor is dropped"
            );
        });
    }

    #[test]
    fn shutdown_drains_every_task() {
        block_on(async {
            let mut supervisor = Supervisor::new(4);
            supervisor
                .spawn(|token| async move {
                    let _outcome = repeat(&token, Budget::Ongoing, |_tick| async {}).await;
                })
                .await;
            supervisor
                .spawn(|token| async move {
                    let _outcome = repeat(&token, Budget::Ongoing, |_tick| async {}).await;
                })
                .await;
            // Would never return if `shutdown` waited on a cancelled task
            // instead of draining it.
            let report = supervisor.shutdown().await;
            assert_eq!(
                report.stats().spawned,
                2,
                "shutdown must report the counters it drained against"
            );
        });
    }

    #[test]
    fn a_repeating_task_stops_at_its_budget_under_supervision() {
        block_on(async {
            let ticks = Arc::new(AtomicU64::new(0));
            let counter = Arc::clone(&ticks);
            let mut supervisor = Supervisor::new(1);
            supervisor
                .spawn_repeating(budget_of(5), move |_tick| {
                    let counter = Arc::clone(&counter);
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                    }
                })
                .await;
            assert!(
                settle(&mut supervisor).await,
                "a repeating task must finish once its budget is spent"
            );
            assert_eq!(
                ticks.load(Ordering::SeqCst),
                5,
                "a supervised repeating task must stop at its iteration budget"
            );
            supervisor.shutdown().await;
        });
    }

    #[test]
    fn a_panicking_task_is_not_reported_as_a_success() {
        // The counterexample the defect was filed on: `async {}` and a body
        // that panics produced identical `Stats` and no report channel at all.
        // The panic is captured by the runtime into a `JoinError`, so it is
        // read here rather than propagated: a test that panics the suite
        // proves nothing about the API that is supposed to report the panic,
        // and takes the rest of the suite down with it.
        block_on(async {
            let mut supervisor = Supervisor::new(2);
            supervisor.spawn(|_token| async {}).await;
            supervisor
                .spawn(|_token| async {
                    explode("worker failed before producing its receipt");
                })
                .await;
            assert!(
                settle(&mut supervisor).await,
                "a panicking task still ends, so nothing is left in flight"
            );

            let stats = supervisor.stats();
            // The accounting counters are identical, and that is the point:
            // `completed` is a resource count and says nothing about success.
            assert_eq!(stats.spawned, 2, "both tasks were started");
            assert_eq!(stats.completed, 2, "both tasks ended");
            assert_eq!(
                stats.succeeded, 1,
                "only the task that returned is a success: {stats:?}"
            );
            assert_eq!(stats.panicked, 1, "one task panicked: {stats:?}");

            let outcomes = drain(&mut supervisor);
            assert_eq!(outcomes.len(), 2, "every task reports exactly once");
            assert_eq!(
                tasks_where(&outcomes, TaskOutcome::is_panic),
                vec![TaskId(1)],
                "the report names the task that panicked, in spawn order: {outcomes:?}"
            );
            assert_eq!(
                tasks_where(&outcomes, TaskOutcome::is_success),
                vec![TaskId(0)],
                "the task that returned is reported as the one that returned: {outcomes:?}"
            );
            let messages: Vec<&str> = outcomes
                .iter()
                .filter_map(TaskOutcome::panic_message)
                .collect();
            assert_eq!(
                messages,
                vec!["worker failed before producing its receipt"],
                "the panic payload is preserved rather than replaced by a marker"
            );
        });
    }

    #[test]
    fn a_panic_after_an_observable_effect_keeps_both_facts() {
        // A task can do its work and then die, and the supervisor has to be
        // able to say both things. Reporting only "one task ended" is what
        // made the effect untraceable: a caller reading the counter would
        // conclude the work was done.
        block_on(async {
            let effects = Arc::new(AtomicU64::new(0));
            let counter = Arc::clone(&effects);
            let mut supervisor = Supervisor::new(1);
            supervisor
                .spawn(move |_token| {
                    let counter = Arc::clone(&counter);
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        explode("died after the effect");
                    }
                })
                .await;
            assert!(settle(&mut supervisor).await, "the task must end");

            assert_eq!(
                effects.load(Ordering::SeqCst),
                1,
                "the effect happened, so the test's premise holds"
            );
            assert_eq!(
                supervisor.stats().succeeded,
                0,
                "an effect that happened is not a task that finished"
            );
            let outcomes = drain(&mut supervisor);
            assert_eq!(
                outcomes,
                vec![TaskOutcome::Panicked {
                    task: TaskId(0),
                    message: String::from("died after the effect"),
                }],
                "the report carries the panic, and does not claim success"
            );
        });
    }

    #[test]
    fn cooperative_cancellation_and_abort_are_told_apart() {
        block_on(async {
            let mut supervisor = Supervisor::new(2);
            // Cooperative: the body awaits its token, so the cancel makes it
            // return on its own and it is reported cancelled.
            supervisor
                .spawn(|token| async move {
                    token.cancelled().await;
                })
                .await;
            // Not cooperative: the body never looks at its token and never
            // finishes, so the only way it ends is the abort.
            supervisor
                .spawn(|_token| async {
                    loop {
                        yield_now().await;
                    }
                })
                .await;

            let report = supervisor.shutdown().await;
            let stats = report.stats();
            assert_eq!(stats.spawned, 2, "both tasks were started");
            assert_eq!(stats.completed, 2, "both tasks ended");
            assert_eq!(
                stats.cancelled, 1,
                "the cooperative body returned under cancellation: {stats:?}"
            );
            assert_eq!(
                stats.aborted, 1,
                "the body that ignored its token had to be dropped: {stats:?}"
            );
            assert_eq!(
                stats.succeeded, 0,
                "neither task ran to the end of its own work: {stats:?}"
            );
            assert!(
                !report.is_clean(),
                "a shutdown that cancelled or aborted anything is not a clean finish"
            );
            assert_eq!(
                tasks_where(report.outcomes(), TaskOutcome::is_success),
                Vec::<TaskId>::new(),
                "no failed task may appear as successful: {:?}",
                report.outcomes()
            );
            assert_eq!(
                report
                    .outcomes()
                    .iter()
                    .map(TaskOutcome::task)
                    .collect::<Vec<TaskId>>(),
                vec![TaskId(0), TaskId(1)],
                "both terminal outcomes are reported, each attributed to its task"
            );
        });
    }

    #[test]
    fn shutdown_reports_a_clean_finish_when_every_task_returned() {
        block_on(async {
            let mut supervisor = Supervisor::new(2);
            supervisor.spawn(|_token| async {}).await;
            supervisor.spawn(|_token| async {}).await;
            assert!(settle(&mut supervisor).await, "both tasks must finish");
            let report = supervisor.shutdown().await;
            assert_eq!(
                report.stats().succeeded,
                2,
                "both tasks returned before the cancel"
            );
            assert!(
                report.panicked().next().is_none(),
                "a clean finish has no panics to report"
            );
        });
    }

    #[test]
    fn an_undrained_report_buffer_is_capped_and_counted() {
        block_on(async {
            // The bound is one, so at most one report is retained. A caller
            // that spawns 32 tasks and never drains must not be able to make
            // this module's memory grow with the total number of spawns.
            let mut supervisor = Supervisor::new(1);
            for _ in 0..32 {
                supervisor.spawn(|_token| async {}).await;
            }
            assert!(settle(&mut supervisor).await, "every task must finish");
            let stats = supervisor.stats();
            assert_eq!(stats.spawned, 32, "every spawn is still counted");
            assert_eq!(stats.succeeded, 32, "every success is still counted");
            assert_eq!(
                stats
                    .succeeded
                    .saturating_add(stats.cancelled)
                    .saturating_add(stats.aborted)
                    .saturating_add(stats.panicked),
                stats.completed,
                "the four outcomes must account for every task that ended"
            );
            let retained = drain(&mut supervisor).len();
            assert_eq!(
                retained, 1,
                "the buffer retains at most the in-flight bound"
            );
            assert_eq!(
                stats.reports_dropped, 31,
                "and every report it did not retain is counted: {stats:?}"
            );
        });
    }

    #[test]
    fn a_capped_report_buffer_still_attributes_the_report_it_keeps() {
        block_on(async {
            let mut supervisor = Supervisor::new(1);
            // The first task takes the only slot, so the second is refused and
            // the first is the only task that can report.
            assert!(
                supervisor
                    .try_spawn(|_token| async {
                        pending::<()>().await;
                    })
                    .is_ok(),
                "the first spawn must take the only slot"
            );
            assert_eq!(
                supervisor.try_spawn(|_token| async {}),
                Err(TrySpawnRefusal::AtCapacity),
                "the premise: the bound is genuinely reached"
            );
            let report = supervisor.shutdown().await;
            assert_eq!(
                report.outcomes().len(),
                1,
                "one task ended, so one report is retained"
            );
            assert_eq!(
                report.outcomes().first().map(TaskOutcome::task),
                Some(TaskId(0)),
                "the retained report still names its task"
            );
            assert_eq!(
                report.stats().aborted,
                1,
                "the body never observed its token, so it was dropped"
            );
        });
    }

    #[test]
    fn shutdown_retains_every_report_even_past_the_cap() {
        block_on(async {
            // The cap is one, and the buffer is full when shutdown begins. A
            // drain that applied the cap would lose the second task's outcome
            // exactly when a caller is waiting for it, so the two retention
            // paths exist: `reap` is capped because the caller may spawn again,
            // and the drain is not because the total it can produce is already
            // bounded by the in-flight ceiling.
            let mut supervisor = Supervisor::new(1);
            supervisor.spawn(|_token| async {}).await;
            assert!(settle(&mut supervisor).await, "the first task must finish");
            assert_eq!(
                supervisor.stats().reports_dropped,
                0,
                "the premise: the one report so far fit in the buffer"
            );
            supervisor
                .spawn(|_token| async {
                    pending::<()>().await;
                })
                .await;

            let report = supervisor.shutdown().await;
            assert_eq!(
                report.outcomes().len(),
                2,
                "shutdown must hand over both outcomes, not just the ones that \
                 fit: {:?}",
                report.outcomes()
            );
            assert_eq!(
                report.stats().reports_dropped,
                0,
                "the drain path is not capped"
            );
            assert_eq!(
                tasks_where(report.outcomes(), TaskOutcome::is_success),
                vec![TaskId(0)],
                "the task that returned is reported as the one that returned"
            );
            assert_eq!(
                tasks_where(report.outcomes(), |outcome| !outcome.is_success()),
                vec![TaskId(1)],
                "and the one that did not is attributed to the other task"
            );
        });
    }

    // ── The drain's decision table, as a seeded simulation ───────────────────
    //
    // The arms above are the cases a reader thinks of. This family is the rest:
    // one seed draws a whole world — how many descendants a reading names, which
    // of them have already ended, which ignore every signal, whether the table
    // can be read at all, whether the reading is truncated, whether the leader had
    // already exited on its own, what the group signal answers and when the group
    // is observed absent — and a model written from
    // the documented contract says what the drain must then report. Every world
    // is driven through the real `ProcessGroup`, so the arms the simulation
    // covers are the arms production runs.

    #[cfg(all(unix, feature = "process"))]
    /// Reduce `value` into `0..bound` without a modulo operator.
    ///
    /// `clippy::modulo_arithmetic` is forbidden workspace-wide, and a bound drawn
    /// as a `%` is exactly the "bound I did not think about" this module forbids
    /// elsewhere. Multiply-shift keeps it in one place: the high half of a
    /// 64×64 product is below `bound`, so the narrowing cannot refuse, and it is
    /// still propagated rather than defaulted.
    fn reduce(value: u64, bound: NonZeroU64) -> Result<u64, std::num::TryFromIntError> {
        let wide = u128::from(value).wrapping_mul(u128::from(bound.get()));
        u64::try_from(wide >> 64)
    }

    #[cfg(all(unix, feature = "process"))]
    /// One seeded world's draws, keyed by field rather than drawn as a stream.
    ///
    /// A stream hides which choice a failure came from; a field name is the seed
    /// and the decision in one line, so a reported seed reproduces the world.
    struct Seed(u64);

    #[cfg(all(unix, feature = "process"))]
    impl Seed {
        /// The draw for `field` in `0..bound`.
        ///
        /// The seed and field are combined and passed through the splitmix64
        /// finalizer, so two fields of one seed are independent draws. A
        /// combination that was only multiplied made every field a fixed offset
        /// of every other, and some pairs of choices could never be drawn
        /// together: across 4,096 seeds no leader that exited on its own was
        /// ever paired with a refused group signal.
        fn draw(&self, field: u64, bound: NonZeroU64) -> Result<u64, std::num::TryFromIntError> {
            let mut mixed = self
                .0
                .wrapping_add(field.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            reduce(mixed ^ (mixed >> 31), bound)
        }

        /// A draw in `0..=max`.
        fn below(&self, field: u64, max: u64) -> Result<u64, std::num::TryFromIntError> {
            self.draw(field, NonZeroU64::MIN.saturating_add(max))
        }
    }

    /// How the group answers the one signal the drain may need.
    #[cfg(all(unix, feature = "process"))]
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum WorldGroup {
        /// Delivered, group still present.
        Delivered,
        /// Delivered and the group is already gone.
        Absent,
        /// Present but unsignalable, as an unreaped zombie leader is.
        Present,
        /// Refused.
        Refused,
    }

    /// One seeded world: the scripted capture, the scripted group, and the model.
    #[cfg(all(unix, feature = "process"))]
    struct World {
        /// The pids the reading names.
        tree: Vec<i32>,
        /// The pids that have already ended, so a signal answers `ESRCH`.
        gone: Vec<i32>,
        /// The pids that ignore every signal.
        immortal: Vec<i32>,
        /// Whether the process table cannot be read.
        unreadable: bool,
        /// Whether the reading stopped at its bound.
        truncated: bool,
        /// Whether the leader exited on its own before the cleanup began.
        exited: bool,
        /// What the group signal answers.
        group: WorldGroup,
        /// How many post-reap probes answer "present" before one answers "absent".
        present_probes: usize,
        /// The receipt the pinned phase must produce.
        expected: CleanupReceipt,
        /// The containment report the pinned phase must produce.
        containment: Containment,
    }

    #[cfg(all(unix, feature = "process"))]
    impl World {
        /// Draw the world `seed` names, with the model's answers beside it.
        fn draw(seed: u64) -> Result<Self, std::num::TryFromIntError> {
            let draws = Seed(seed);
            let length = draws.below(0, 3)?;
            let mut tree = Vec::with_capacity(usize::try_from(length)?);
            let mut gone = Vec::new();
            let mut immortal = Vec::new();
            for index in 0..length {
                let pid_field = index.saturating_add(1);
                let pid = i32::try_from(draws.below(pid_field, 60)?)?;
                if tree.contains(&pid) {
                    continue;
                }
                tree.push(pid);
                // Two fields per pid, so "already ended" and "ignores signals" are
                // disjoint by construction: a pid cannot be both, and a world
                // that could say otherwise would be testing an impossible state.
                let ends = draws.below(index.saturating_add(20), 1)? == 0;
                let ignores = draws.below(index.saturating_add(40), 1)? == 0;
                if ends {
                    gone.push(pid);
                } else if ignores {
                    immortal.push(pid);
                }
            }
            tree.sort_unstable();
            gone.sort_unstable();
            immortal.sort_unstable();
            let unreadable = draws.below(3, 3)? == 0;
            let truncated = !unreadable && draws.below(4, 3)? == 0;
            let group = match draws.below(5, 3)? {
                0 => WorldGroup::Absent,
                1 => WorldGroup::Present,
                2 => WorldGroup::Refused,
                _ => WorldGroup::Delivered,
            };
            let present_probes = usize::try_from(draws.below(6, 2)?)?;
            let exited = draws.below(7, 3)? == 0;

            // The model is the drain's documented contract, phase by phase.
            //
            // Phase 1, the reading: none when the leader had already exited (its
            // children were re-parented at the exit, so a walk names nothing) or
            // when the table cannot be read. Either way the tree is empty, and a
            // model that still counted the drawn pids would report a capture that
            // never happened.
            let read = !unreadable && !exited;
            let named: Vec<i32> = if read { tree.clone() } else { Vec::new() };
            // Phase 2, the group signal: a refusal ends the drain at once, after
            // the reading and before any pid is signalled or observed.
            let refused = group == WorldGroup::Refused;
            // Phases 3 and 4, signal and observe each named pid, run only on a
            // named tree under a group signal the OS accepted.
            let per_pid = read && !refused;
            let survivors: Vec<i32> = if per_pid {
                named
                    .iter()
                    .copied()
                    .filter(|pid| immortal.contains(pid))
                    .collect()
            } else {
                Vec::new()
            };
            // One round when nothing is left running, every round while something
            // is: that is the loop's own exit condition, not a choice. A pid is
            // signalled the first round it is named, unless it had already ended,
            // and again each later round only while it is observed running.
            let rounds = if survivors.is_empty() {
                1
            } else {
                CONTAINMENT_ROUNDS
            };
            let delivered = named.iter().filter(|pid| !gone.contains(pid)).count();
            let signalled = if per_pid {
                delivered.saturating_add(survivors.len().saturating_mul(rounds.saturating_sub(1)))
            } else {
                0
            };
            // The first limit noted wins, and the reading is where each is noted.
            let residual = if exited {
                Some(ResidualRisk::LeaderExited)
            } else if unreadable {
                Some(ResidualRisk::TableUnreadable)
            } else if truncated {
                Some(ResidualRisk::CaptureTruncated)
            } else {
                None
            };
            let expected = match group {
                WorldGroup::Refused => CleanupReceipt::CleanupFailed,
                _ if !survivors.is_empty() => CleanupReceipt::CleanupSurvivors {
                    survivors: survivors.clone(),
                },
                WorldGroup::Absent => CleanupReceipt::CleanupConfirmed,
                WorldGroup::Delivered | WorldGroup::Present => CleanupReceipt::CleanupPending,
            };
            Ok(Self {
                tree,
                gone,
                immortal,
                unreadable,
                truncated,
                exited,
                group,
                present_probes,
                expected,
                containment: Containment {
                    mechanism: if read {
                        ContainmentMechanism::ProcessTableSnapshot
                    } else {
                        ContainmentMechanism::ProcessGroupOnly
                    },
                    captured: named.len(),
                    signalled,
                    survivors,
                    residual,
                },
            })
        }

        /// The receipt the post-reap pass must reach, from the world alone.
        ///
        /// A survivor is observed again and never promoted. Otherwise the group
        /// is settled absent when the pinned phase already observed it gone, or
        /// when the post-reap probes reach their first "absent" inside the bounded
        /// rounds. It is read from the world rather than by probing the scripted
        /// observer again: a pass that returned at its first probe has not
        /// consumed the world's "present" answers, so a fresh probe would report a
        /// group the drain had correctly settled as still there.
        fn settled(&self) -> CleanupReceipt {
            if !self.containment.survivors.is_empty() {
                return CleanupReceipt::CleanupSurvivors {
                    survivors: self.containment.survivors.clone(),
                };
            }
            if self.group == WorldGroup::Absent || self.present_probes < CONTAINMENT_ROUNDS {
                CleanupReceipt::CleanupConfirmed
            } else {
                CleanupReceipt::CleanupPending
            }
        }

        /// The scripted capture this world drives.
        fn capture(&self) -> ScriptedCapture {
            ScriptedCapture {
                tree: self.tree.clone(),
                gone: self.gone.clone(),
                immortal: self.immortal.clone(),
                unreadable: self.unreadable,
                truncated: self.truncated,
                reads: AtomicUsize::new(0),
                signals: AtomicUsize::new(0),
            }
        }

        /// The scripted signaller this world drives.
        fn signaller(&self) -> WorldSignaller {
            WorldSignaller(self.group)
        }
    }

    /// A group signal that answers whatever the seeded world says.
    #[cfg(all(unix, feature = "process"))]
    struct WorldSignaller(WorldGroup);

    #[cfg(all(unix, feature = "process"))]
    impl GroupSignaller for WorldSignaller {
        fn signal(&self, _group: i32) -> std::io::Result<()> {
            match self.0 {
                WorldGroup::Absent => Err(std::io::Error::from_raw_os_error(ESRCH)),
                WorldGroup::Present => Err(std::io::Error::from_raw_os_error(EPERM)),
                WorldGroup::Refused => Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "injected refusal",
                )),
                WorldGroup::Delivered => Ok(()),
            }
        }
    }

    /// The receipt's arm as a number, for the coverage check.
    #[cfg(all(unix, feature = "process"))]
    fn receipt_arm(receipt: &CleanupReceipt) -> u64 {
        match *receipt {
            CleanupReceipt::CleanupConfirmed => 1,
            CleanupReceipt::CleanupPending => 2,
            CleanupReceipt::CleanupFailed => 3,
            CleanupReceipt::CleanupSurvivors { .. } => 4,
        }
    }

    /// The receipt's arm and survivor count as one number, for the trace.
    ///
    /// Both, because a family's replay oracle is only meaningful if two worlds
    /// that differ only in *how many* pids survived cannot collapse into one
    /// trace.
    #[cfg(all(unix, feature = "process"))]
    fn receipt_code(receipt: &CleanupReceipt) -> Result<u64, std::num::TryFromIntError> {
        Ok(match *receipt {
            CleanupReceipt::CleanupSurvivors { ref survivors } => {
                u64::try_from(survivors.len())?.saturating_add(4)
            }
            _ => receipt_arm(receipt),
        })
    }

    /// Drive one seeded world through the real drain and check it against the model.
    #[cfg(all(unix, feature = "process"))]
    fn sim_drain(seed: u64) -> Result<u64, Box<dyn std::error::Error>> {
        let world = World::draw(seed)?;
        let at = format!("seed {seed:#018x}");
        let capture = world.capture();
        let signaller = world.signaller();
        let observer = sequence(world.present_probes);
        let mut group = capture_group(&signaller, &observer, &capture);
        if world.exited {
            group.mark_exited();
        }
        let pinned = block_on(group.cleanup());
        assert_eq!(
            pinned, world.expected,
            "{at}: the pinned phase must report the model's receipt"
        );
        assert_eq!(
            *group.containment(),
            world.containment,
            "{at}: the containment report must carry the model's counts and limits"
        );
        assert_eq!(
            capture.signals(),
            world.containment.signalled(),
            "{at}: the drain must deliver exactly the signals the model counts"
        );
        if world.exited {
            assert_eq!(
                capture.reads(),
                0,
                "{at}: a leader that had exited has no tree to walk, so no table is read"
            );
            assert!(
                !group.containment().is_complete(),
                "{at}: a cleanup that never saw the tree must not claim it"
            );
        }
        // The post-reap pass decides the group, and a survivor is never promoted.
        group.mark_reaped();
        let settled = block_on(group.confirm_absence());
        let expected = world.settled();
        assert_eq!(
            settled, expected,
            "{at}: only an observed absence settles the group, and a survivor is never \
             promoted to a clean cleanup"
        );
        let captured = u64::try_from(group.containment().captured())?;
        let signalled = u64::try_from(group.containment().signalled())?;
        let pinned_code = receipt_code(&pinned)?;
        let settled_code = receipt_code(&settled)?;
        Ok(pinned_code
            .wrapping_add(captured)
            .wrapping_add(signalled)
            .wrapping_add(settled_code))
    }

    #[cfg(all(unix, feature = "process"))]
    /// The seeds this family replays, named so a failure names one.
    const DRAIN_SEEDS: [u64; 12] = [
        0x5EED_2630_0000_0001,
        0x5EED_2630_0000_0002,
        0x5EED_2630_0000_0003,
        0x5EED_2630_0000_0004,
        0x5EED_2630_0000_0005,
        0x5EED_2630_0000_0006,
        0x5EED_2630_0000_000E,
        0x5EED_2630_0000_0019,
        0x5EED_2630_0000_0025,
        0x5EED_2630_FFFF_FFFF,
        0xDEAD_BEEF_0263_0001,
        0xC0FF_EE00_2630_0001,
    ];

    #[cfg(all(unix, feature = "process"))]
    #[test]
    fn sim_a_seeded_drain_reaches_the_models_receipt() -> Result<(), Box<dyn std::error::Error>> {
        for seed in DRAIN_SEEDS {
            sim_drain(seed)?;
        }
        Ok(())
    }

    #[cfg(all(unix, feature = "process"))]
    #[test]
    fn sim_the_same_seed_replays_the_same_drain_trace() -> Result<(), Box<dyn std::error::Error>> {
        for seed in DRAIN_SEEDS {
            assert_eq!(
                sim_drain(seed)?,
                sim_drain(seed)?,
                "seed {seed:#018x}: the same seed must drive the same world and the same report"
            );
        }
        Ok(())
    }

    #[cfg(all(unix, feature = "process"))]
    #[test]
    fn sim_every_receipt_arm_is_reachable_in_the_family() -> Result<(), Box<dyn std::error::Error>>
    {
        let arms = DRAIN_SEEDS
            .iter()
            .map(|seed| World::draw(*seed).map(|world| receipt_arm(&world.expected)))
            .collect::<Result<Vec<u64>, _>>()?;
        for arm in [1_u64, 2, 3, 4] {
            assert!(
                arms.contains(&arm),
                "the family must reach receipt arm {arm}; it drew {arms:?} over {} seeds",
                DRAIN_SEEDS.len()
            );
        }
        Ok(())
    }

    /// How many consecutive seeds the exited-leader sweep draws.
    #[cfg(all(unix, feature = "process"))]
    const EXITED_SWEEP_SEEDS: u64 = 4_096;

    /// Every world in a wide seeded sweep reaches the model, and the sweep holds
    /// both leaders that exited on their own and leaders that did not, under
    /// every group answer, so the exited arm is exercised against each receipt
    /// rather than once.
    #[cfg(all(unix, feature = "process"))]
    #[test]
    fn sim_an_exited_leader_reads_no_table_and_claims_no_tree()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut exited_arms = std::collections::BTreeSet::new();
        let mut alive = 0_u64;
        for offset in 0..EXITED_SWEEP_SEEDS {
            let seed = 0x5EED_0347_0000_0000_u64.wrapping_add(offset);
            sim_drain(seed)?;
            let world = World::draw(seed)?;
            if world.exited {
                exited_arms.insert(receipt_arm(&world.expected));
            } else {
                alive = alive.saturating_add(1);
            }
        }
        assert_eq!(
            exited_arms,
            std::collections::BTreeSet::from([1_u64, 2, 3]),
            "an exited leader must be driven to the confirmed, pending and failed receipts; \
             it can never have a named survivor, because nothing was named"
        );
        assert!(
            alive > 0,
            "the sweep must still hold leaders that were alive at the cleanup"
        );
        Ok(())
    }

    #[cfg(all(unix, feature = "process"))]
    #[test]
    fn sim_distinct_seeds_drive_distinct_drains() -> Result<(), Box<dyn std::error::Error>> {
        let [first, second, ..] = DRAIN_SEEDS;
        assert_ne!(
            sim_drain(first)?,
            sim_drain(second)?,
            "seeds {first:#018x} and {second:#018x} must draw different worlds"
        );
        Ok(())
    }

    #[test]
    fn an_over_long_panic_message_is_truncated_not_dropped() {
        let long = "x".repeat(MAX_PANIC_MESSAGE_CHARS.saturating_add(1));
        let kept = truncate_panic_message(&long);
        assert_eq!(
            kept.chars().count(),
            MAX_PANIC_MESSAGE_CHARS.saturating_add(1),
            "a truncated message keeps the cap plus the single-character marker"
        );
        assert!(
            kept.ends_with('…'),
            "a truncated message says so, so it is never read as complete"
        );
        assert_eq!(
            truncate_panic_message("short"),
            "short",
            "a message within the cap is carried unchanged"
        );
    }

    /// A grant that lands in the same instant as a cancellation is handed back
    /// through the round, so the tenant it was charged to is not left holding an
    /// admission it does not have.
    ///
    /// The ordering this test creates is the whole point and it is created, not
    /// raced. On a current-thread runtime the task that ends is queued by
    /// `notify_waiters` **before** the parked admission is queued by `cancel`,
    /// because the helper body performs both in one poll with no await between
    /// them. The permit therefore reaches the parked waiter's slot *after* the
    /// token is cancelled, and the wait is woken with a permit in hand: the one
    /// state in which a bare permit would leak the tenant's in-flight charge.
    #[cfg(feature = "script")]
    #[test]
    fn a_cancelled_admission_hands_its_granted_permit_back_to_the_round()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::rt::sync::Notify;
        use crate::rt::tenancy::TenancyPolicy;
        use crate::script::Tenant;

        block_on(async {
            // Two permits and a ceiling of one per tenant: the occupier holds
            // one, the helper takes the other, and the pool still has to be able
            // to hand the occupier's permit to the waiter below.
            let mut supervisor = Supervisor::with_tenancy(2, TenancyPolicy::new(1, 8));
            let shell = Arc::clone(
                supervisor
                    .tenancy
                    .as_ref()
                    .ok_or("a policy must install a tenancy shell")?,
            );
            let tenant = Tenant::new("held")?;
            let helper = Tenant::new("helper")?;
            let gate = Arc::new(Notify::new());
            let parked = Arc::new(AtomicUsize::new(0));
            let root = supervisor.token.clone();

            // The occupier: `tenant` takes its one admission and parks.
            supervisor
                .spawn_for(&tenant, {
                    let gate = Arc::clone(&gate);
                    let parked = Arc::clone(&parked);
                    move |_token| async move {
                        parked.fetch_add(1, Ordering::SeqCst);
                        gate.notified().await;
                    }
                })
                .await
                .map_err(|refusal| format!("the occupier was refused: {refusal}"))?;
            assert!(
                yield_until(|| parked.load(Ordering::SeqCst) >= 1, 1_000).await,
                "the occupier never parked, so the premise of this test did not hold"
            );

            // The helper: waits until the claim below has actually parked in the
            // round, then ends the occupier and cancels, with no await between
            // the two so the ordering above is the one the executor follows.
            let helper_gate = Arc::clone(&gate);
            let helper_shell = Arc::clone(&shell);
            let helper_tenant = tenant.clone();
            let helper_root = root.clone();
            supervisor
                .spawn_for(&helper, move |_token| async move {
                    while helper_shell.counts_of(&helper_tenant).1 == 0 {
                        yield_now().await;
                    }
                    helper_gate.notify_waiters();
                    helper_root.cancel();
                })
                .await
                .map_err(|refusal| format!("the helper was refused: {refusal}"))?;

            let outcome = supervisor.claim_tenanted(&shell, &tenant).await;
            assert_eq!(
                outcome.err(),
                Some(SpawnRefused::Cancelled),
                "a supervisor cancelled while a grant was in flight refuses the admission"
            );
            assert!(
                settle(&mut supervisor).await,
                "the occupier and the helper must both finish"
            );
            let (in_flight, queued) = shell.counts_of(&tenant);
            assert_eq!(
                (in_flight, queued),
                (0, 0),
                "the permit the round had already charged to {tenant} must come back \
                 through the round; a bare permit returns to the pool and leaves the \
                 tenant charged for an admission it does not hold ({in_flight} in flight)"
            );
            assert_eq!(
                supervisor.permits.available_permits(),
                2,
                "both permits are back in the pool, so nothing leaked"
            );
            Ok::<(), Box<dyn std::error::Error>>(())
        })
    }

    /// A tenanted admission joins at most `REAP_PER_ADMISSION` finished
    /// tasks, so a tenant whose bodies end at once cannot charge its joins to
    /// the neighbour that admits next (#268's adversarial row), and the bounded
    /// reap still keeps the retained set at the in-flight ceiling.
    ///
    /// The flood is created, not raced: every attacker body has finished before
    /// the neighbour's admission, so an unbounded reap would join all of them
    /// inside that one call. The second half drives 10,000 admissions of bodies
    /// that end at once, alternating tenants, and reads the retained set after
    /// each: it never exceeds the ceiling, because every call that finds two or
    /// more finished tasks joins two and places one.
    #[cfg(feature = "script")]
    #[test]
    fn a_tenanted_admission_joins_a_bounded_backlog_and_keeps_the_set_at_the_ceiling()
    -> Result<(), Box<dyn std::error::Error>> {
        use super::REAP_PER_ADMISSION;
        use crate::rt::tenancy::TenancyPolicy;
        use crate::script::Tenant;

        const LIMIT: usize = 64;
        const FLOOD: usize = 32;
        const ADMISSIONS: usize = 10_000;
        // The bound is only a bound if it is smaller than the flood: an
        // unbounded reap joins all `FLOOD` below, so a constant at or past it
        // fails this test at compile time rather than passing vacuously.
        const _: () = assert!(
            REAP_PER_ADMISSION < FLOOD,
            "a per-admission reap must be smaller than the flood it bounds"
        );

        block_on(async {
            let mut supervisor = Supervisor::with_tenancy(LIMIT, TenancyPolicy::new(FLOOD, LIMIT));
            let attacker = Tenant::new("attacker")?;
            let neighbour = Tenant::new("neighbour")?;
            let ended = Arc::new(AtomicUsize::new(0));
            for _ in 0..FLOOD {
                let ended = Arc::clone(&ended);
                supervisor
                    .spawn_for(&attacker, move |_token| async move {
                        ended.fetch_add(1, Ordering::SeqCst);
                    })
                    .await
                    .map_err(|refusal| format!("a flood body was refused: {refusal}"))?;
            }
            assert!(
                yield_until(|| ended.load(Ordering::SeqCst) >= FLOOD, 10_000).await,
                "the flood bodies never ended, so the premise of this test did not hold"
            );
            // Let every ended body's task complete in the set, not just run.
            for _ in 0..FLOOD {
                yield_now().await;
            }
            let before = supervisor.stats().succeeded;
            supervisor
                .spawn_for(&neighbour, |_token| async {})
                .await
                .map_err(|refusal| format!("the neighbour was refused: {refusal}"))?;
            let joined = supervisor.stats().succeeded.saturating_sub(before);
            assert!(
                joined <= u64::try_from(REAP_PER_ADMISSION)?,
                "the neighbour's admission joined {joined} of the flood's finished tasks; \
                 at most {REAP_PER_ADMISSION} may land on its critical path"
            );

            let mut largest = 0_usize;
            for admission in 0..ADMISSIONS {
                let tenant = if admission % 2 == 0 {
                    &attacker
                } else {
                    &neighbour
                };
                supervisor
                    .spawn_for(tenant, |_token| async {})
                    .await
                    .map_err(|refusal| format!("admission {admission} was refused: {refusal}"))?;
                largest = largest.max(supervisor.set.len());
                yield_now().await;
            }
            assert!(
                largest <= LIMIT,
                "the retained set reached {largest} tasks against an in-flight ceiling of {LIMIT}"
            );
            let report = supervisor.shutdown().await;
            assert_eq!(
                report.stats.succeeded,
                u64::try_from(FLOOD + 1 + ADMISSIONS)?,
                "every body is joined and counted by shutdown"
            );
            Ok(())
        })
    }

    /// A parked admission keeps its place in its tenant's FIFO queue, and leaves
    /// nothing abandoned behind it, however long it waits.
    ///
    /// Three waiters on one tenant, because FIFO order is only observable with
    /// company: a queue of one is in order under every implementation. The
    /// claim below is driven by the real `claim_tenanted` and is parked with the
    /// poll interval the old loop used, so a waiter that was dropped and re-asked
    /// would rotate to the back of these three on every interval — which is the
    /// defect this pins.
    #[cfg(feature = "script")]
    #[test]
    fn a_parked_tenant_admission_keeps_its_place_in_the_queue()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::rt::sync::OwnedSemaphorePermit;
        use crate::rt::tenancy::TenancyPolicy;
        use crate::script::Tenant;
        use std::future::Future;
        use std::task::{Context, Poll, Waker};

        /// Slices of 50 ms. Twenty-four of them is 1.2 s, an order of magnitude
        /// past the 100 ms interval the old loop wrapped each wait in, so a
        /// waiter that was dropped and re-asked would rotate several times.
        const SLICES: u32 = 24;
        /// How long one slice of the park lasts.
        const SLICE: Duration = Duration::from_millis(50);

        block_on(async {
            // One permit, a ceiling of three and a queue of eight: the permit is
            // held back by the test, so every arrival parks rather than being
            // admitted, and the three waiters share one tenant's queue.
            let mut supervisor = Supervisor::with_tenancy(1, TenancyPolicy::new(3, 8));
            let shell = Arc::clone(
                supervisor
                    .tenancy
                    .as_ref()
                    .ok_or("a policy must install a tenancy shell")?,
            );
            let pool = Arc::clone(&supervisor.permits);
            let tenant = Tenant::new("queued")?;
            // The pool's one permit is held back by the test before the first
            // arrival, so every arrival below parks rather than being admitted.
            let mut held = Some(match Arc::clone(&pool).try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => return Err("the pool did not start with its one permit".into()),
            });

            // The claim under test registers first, so FIFO serves it first. It
            // borrows the supervisor for as long as it is parked, which is why
            // the pool is cloned out before the pin and the two company waiters
            // are registered through the shell rather than through the
            // supervisor.
            let mut claim = std::pin::pin!(supervisor.claim_tenanted(&shell, &tenant));
            let mut probe = Context::from_waker(Waker::noop());
            assert!(
                claim.as_mut().poll(&mut probe).is_pending(),
                "the claim must park on a spent pool rather than be admitted"
            );
            let mut second = queued_waiter(&shell, &tenant)?;
            let mut third = queued_waiter(&shell, &tenant)?;

            let mut slices = 0_u32;
            while slices < SLICES {
                assert!(
                    crate::rt::time::timeout(SLICE, claim.as_mut())
                        .await
                        .is_err(),
                    "the claim cannot resolve while the pool is spent"
                );
                assert_eq!(
                    shell.counts_of(&tenant).1,
                    3,
                    "all three waiters stay live while they are parked"
                );
                assert_eq!(
                    shell.retained_waiters(&tenant),
                    3,
                    "a waiter that keeps its place leaves no abandoned entry behind; \
                     the deque grew to {} entries over {slices} polls",
                    shell.retained_waiters(&tenant)
                );
                slices = slices.saturating_add(1);
            }

            // Serve them in turn. One permit is handed to the round, the waiter
            // the round names resolves, and the permit it was holding is handed
            // straight on to the next — so the order the three resolve in is the
            // queue's own order and nothing else.
            let mut order: Vec<u8> = Vec::new();
            // Hoisted out of the loop: a future that has returned must never be
            // polled again, and the claim is the one that resolves first.
            let mut claim_out: Option<Result<Lease, SpawnRefused>> = None;
            let mut claim_done = false;
            for _ in 0..3 {
                if let Some(permit) = held.take() {
                    shell.release(&tenant, permit);
                }
                let mut second_out: Option<OwnedSemaphorePermit> = None;
                let mut third_out: Option<OwnedSemaphorePermit> = None;
                crate::rt::time::timeout(
                    Duration::from_secs(5),
                    std::future::poll_fn(|context| {
                        if !claim_done
                            && claim_out.is_none()
                            && let Poll::Ready(value) = claim.as_mut().poll(context)
                        {
                            claim_done = true;
                            claim_out = Some(value);
                        }
                        if second_out.is_none()
                            && let Poll::Ready(permit) = second.as_mut().poll(context)
                        {
                            second_out = Some(permit);
                        }
                        if third_out.is_none()
                            && let Poll::Ready(permit) = third.as_mut().poll(context)
                        {
                            third_out = Some(permit);
                        }
                        if claim_done || second_out.is_some() || third_out.is_some() {
                            return Poll::Ready(());
                        }
                        Poll::Pending
                    }),
                )
                .await
                .map_err(|_| "no waiter resolved after a permit was released into the round")?;
                let index = if claim_out.is_some() {
                    match claim_out.take() {
                        Some(Ok(lease)) => {
                            // The granted lease hands its permit to the next
                            // waiter as it drops, which is the hand-off the next
                            // round needs.
                            drop(lease);
                            0
                        }
                        Some(Err(refusal)) => {
                            return Err(format!("the parked claim was refused: {refusal}").into());
                        }
                        None => return Err("the claim reported no outcome at all".into()),
                    }
                } else if let Some(permit) = second_out {
                    held = Some(permit);
                    1
                } else if let Some(permit) = third_out {
                    held = Some(permit);
                    2
                } else {
                    return Err("a resolution was reported with no waiter behind it".into());
                };
                order.push(index);
            }
            assert_eq!(
                order,
                vec![0, 1, 2],
                "one tenant's three waiters are served in arrival order: the claim \
                 that parked first, then the two behind it"
            );
            assert_eq!(
                shell.retained_waiters(&tenant),
                0,
                "every waiter left the queue once it was served"
            );
            Ok::<(), Box<dyn std::error::Error>>(())
        })
    }

    /// A waiting spawn keeps its place in the pool's own fair queue.
    ///
    /// Two supervisors share ONE pool of a single permit, built through the
    /// private `assembled` constructor, so the queue under test is the pool's
    /// and both waiters can be parked at once — a supervisor's `&mut self` is
    /// what stops two of its own waiters from existing, not the semaphore's
    /// fairness.
    ///
    /// The timing is the assertion, and it is arithmetic rather than hope. The
    /// first claim registers at t = 0; the second registers at t = 90, so under
    /// the old loop the first claim's acquire expires at t = 100 and it re-queues
    /// *behind* the second, leaving the pool's queue ordered second-then-first
    /// for the next ninety milliseconds. The release sits at t = 140, inside
    /// that ninety-millisecond window and two orders of magnitude clear of
    /// either end of it.
    #[test]
    fn a_waiting_spawn_keeps_its_place_in_the_pool_queue() -> Result<(), Box<dyn std::error::Error>>
    {
        /// When the second claim starts being polled, ninety milliseconds in.
        const REGISTER_AT: Duration = Duration::from_millis(90);
        /// When the pool's only permit goes back, half a period after the first
        /// claim's acquire would have expired.
        const RELEASE_AT: Duration = Duration::from_millis(150);
        /// A ceiling on a failure, not a budget the assertion must finish inside.
        const LIMIT: Duration = Duration::from_secs(2);

        block_on(async {
            let pool = Arc::new(crate::rt::sync::Semaphore::new(1));
            let mut first = Supervisor::assembled(1, Arc::clone(&pool), Clock::wall());
            let mut second = Supervisor::assembled(1, Arc::clone(&pool), Clock::wall());
            let mut held = match Arc::clone(&pool).try_acquire_owned() {
                Ok(permit) => Some(permit),
                Err(_) => return Err("the shared pool did not start with its one permit".into()),
            };

            let mut first_claim = std::pin::pin!(first.claim());
            let mut second_claim = std::pin::pin!(second.claim());
            let mut first_out: Option<Option<Lease>> = None;
            let mut second_out: Option<Option<Lease>> = None;

            // The driver is a real-clock spin rather than a chain of timed waits,
            // because the assertion is about two events a hundred milliseconds
            // apart: the first claim's acquire expiring, and the permit being
            // released. Timed slices drift by a millisecond or two each, and a
            // driver that drifts cannot place a release inside a window it is
            // trying to hit.
            let started = std::time::Instant::now();
            let mut second_started = false;
            let mut released = false;
            std::future::poll_fn(|context| {
                let now = started.elapsed();
                if !second_started && now >= REGISTER_AT {
                    second_started = true;
                }
                if first_out.is_none()
                    && let Poll::Ready(value) = first_claim.as_mut().poll(context)
                {
                    first_out = Some(value);
                }
                if second_started
                    && second_out.is_none()
                    && let Poll::Ready(value) = second_claim.as_mut().poll(context)
                {
                    second_out = Some(value);
                }
                if !released && now >= RELEASE_AT {
                    if let Some(permit) = held.take() {
                        // The permit goes back to the pool, and the pool hands it
                        // to the head of its own fair queue.
                        drop(permit);
                    }
                    released = true;
                }
                if first_out.is_some() || second_out.is_some() || now >= LIMIT {
                    return Poll::Ready(());
                }
                // Re-poll immediately: the driver is measuring elapsed real time
                // and has nothing to wait on.
                context.waker().wake_by_ref();
                Poll::Pending
            })
            .await;

            assert!(
                released,
                "the driver never reached the release moment within {LIMIT:?}"
            );
            assert!(
                first_out.is_some(),
                "the caller that arrived first never received the released permit, so \
                 its place in the pool's queue was forfeited; the caller that arrived \
                 second was served instead (second resolved: {})",
                second_out.is_some()
            );
            assert!(
                second_out.is_none(),
                "the permit went to the caller that arrived second: a waiter that keeps \
                 its place is served in arrival order"
            );
            Ok::<(), Box<dyn std::error::Error>>(())
        })
    }

    /// Park one extra waiter on `tenant`, or report which arm answered instead.
    ///
    /// A helper rather than three inline matches: the second and third waiter
    /// are identical constructions, and a copy per waiter is how two of them end
    /// up arming a different arm than the first.
    #[cfg(feature = "script")]
    fn queued_waiter(
        shell: &Arc<super::tenancy_support::TenancyShell>,
        tenant: &crate::script::Tenant,
    ) -> Result<std::pin::Pin<Box<super::tenancy_support::WaitPermit>>, Box<dyn std::error::Error>>
    {
        match shell.admit(tenant) {
            super::tenancy_support::Admission::Queued(waiter) => Ok(Box::pin(waiter)),
            super::tenancy_support::Admission::Admitted(_) => {
                Err("the pool was spent, so this arrival must have parked".into())
            }
            super::tenancy_support::Admission::Refused { limit } => {
                Err(format!("the queue bound of {limit} was already reached").into())
            }
            super::tenancy_support::Admission::SupervisorQueueFull { limit } => {
                Err(format!("the supervisor's waiting bound of {limit} was already reached").into())
            }
        }
    }

    #[test]
    fn wait_idle_joins_every_task_without_cancelling_and_leaves_the_supervisor_usable() {
        block_on(async {
            let ran = Arc::new(AtomicU64::new(0));
            let mut supervisor = Supervisor::new(3);
            for _ in 0..40 {
                let ran = Arc::clone(&ran);
                supervisor
                    .spawn(move |token| async move {
                        yield_now().await;
                        if !token.is_cancelled() {
                            ran.fetch_add(1, Ordering::SeqCst);
                        }
                    })
                    .await;
            }
            supervisor.wait_idle().await;
            let stats = supervisor.stats();
            assert_eq!(stats.in_flight(), 0, "nothing is left in flight");
            assert_eq!(stats.succeeded, 40, "every task completed, none cancelled");
            assert_eq!(stats.cancelled, 0, "waiting is not stopping");
            assert_eq!(ran.load(Ordering::SeqCst), 40, "every body ran to its end");
            assert!(
                !supervisor.is_cancelled(),
                "the supervisor was not cancelled"
            );

            // Still usable: the wait did not consume the supervisor.
            supervisor.spawn(|_token| async {}).await;
            assert_eq!(
                supervisor.wait_idle().await,
                1,
                "the next wait joins the next task"
            );
            assert_eq!(
                supervisor.wait_idle().await,
                0,
                "an idle supervisor joins nothing"
            );
        });
    }

    #[test]
    fn wait_idle_counts_reports_past_the_retention_cap_rather_than_losing_them() {
        block_on(async {
            let mut supervisor = Supervisor::new(2);
            for _ in 0..2 {
                supervisor.spawn(|_token| async {}).await;
            }
            // Fill the pool without reaping, so `wait_idle` has more outcomes
            // to absorb than the cap retains.
            supervisor.wait_idle().await;
            for _ in 0..2 {
                supervisor.spawn(|_token| async {}).await;
            }
            supervisor.wait_idle().await;
            let stats = supervisor.stats();
            let retained = drain(&mut supervisor).len();
            assert_eq!(
                u64::try_from(retained)
                    .ok()
                    .map(|kept| kept.saturating_add(stats.reports_dropped)),
                Some(4),
                "every outcome is either retained or counted as dropped"
            );
        });
    }

    #[cfg(all(unix, feature = "process"))]
    #[test]
    fn a_spawn_waiting_on_a_cleanup_owned_permit_is_admitted_once_the_owner_settles()
    -> Result<(), Box<dyn std::error::Error>> {
        block_on(async {
            let mut supervisor = Supervisor::new(1);
            let Some(permit) = supervisor.try_take() else {
                return Err("a fresh supervisor of bound one has its permit".into());
            };
            // A group id above any pid limit: the native probe reports it absent,
            // so the owner settles on the first reap that drives it — which only
            // the waiting spawn's recheck performs.
            let task = TaskId(u64::MAX);
            supervisor.cleanup_owners.register(
                task,
                0x7fff_fff0,
                Lease::plain(permit),
                whole_capture(),
            );
            let admitted = crate::rt::time::timeout(
                Duration::from_secs(5),
                supervisor.spawn(|_token| async {}),
            )
            .await;
            if admitted.is_err() {
                return Err("the spawn never left the bound: the recheck did not release the owner's permit".into());
            }
            supervisor.wait_idle().await;
            let settled = drain(&mut supervisor)
                .into_iter()
                .any(|outcome| matches!(outcome, TaskOutcome::CleanupSettled { task: settled, .. } if settled == task));
            if !settled {
                return Err("the owner's settlement was not reported".into());
            }
            assert_eq!(supervisor.stats().succeeded, 1, "the waiting body ran");
            Ok(())
        })
    }
}
