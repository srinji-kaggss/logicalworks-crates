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
//! not keep the supervisor alive. That is
//! [`CancellationToken`](crate::rt::cancel::CancellationToken)'s design, not
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
//! `repeat` is the only loop this module asks a caller to write, and it
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
//! `an_ongoing_repeat_still_ends_when_the_supervisor_shuts_down` (`tests/it/rt_front_door.rs`)
//! runs it: `Budget::Ongoing` is the unbounded budget, and it still ends because the supervisor
//! owns the token the loop is raced against and `shutdown` cancels it.
//!
//! # Limits
//!
//! [`Supervisor::spawn`] awaits a permit, so a caller that holds the last
//! permits inside a task this supervisor owns can deadlock with itself. The
//! bound is a backpressure contract, not a queue: a caller who wants to keep
//! producing past the ceiling should use [`Supervisor::try_spawn`] and decide
//! what the refusal means.

use std::fmt;
use std::num::NonZeroU64;
use std::time::Duration;

// Only the supervised process reports an `io::Error`; a build without the
// `process` feature has no such fallible call, so the import is gated with it.
#[cfg(feature = "process")]
use std::io;

/// Re-exported so a consumer reading a [`Containment`] report needs one import,
/// not two: the mechanism is named by the estate's process primitives and read
/// off this crate's receipt.
#[cfg(feature = "process")]
pub use lgwks_std::process::ContainmentMechanism;

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

/// A process started by
/// [`Supervisor::spawn_process_identified_gated`](crate::rt::supervise::Supervisor::spawn_process_identified_gated):
/// the task that owns it, the identity of the process leading its group, and
/// whether its payload was released.
///
/// The payload never ran before [`Self::release`]: a caller that records
/// [`Self::leader`] durably and then releases has no window in which a kill
/// leaves an unrecorded group. A caller that drops this without releasing
/// kills a group that never ran, through the supervisor's own group guard.
#[cfg(feature = "process")]
#[derive(Debug)]
pub struct GatedIdentifiedSpawn {
    /// The task's id, as [`Supervisor::spawn_process`](crate::rt::supervise::Supervisor::spawn_process) would have returned it.
    task: TaskId,
    /// The group leader's identity, read while the payload waited.
    leader: lgwks_std::process::ProcessIdentity,
    /// The leader's pid, signalled on release.
    pid: i32,
    /// Whether the payload was released to run.
    released: bool,
}

#[cfg(feature = "process")]
impl GatedIdentifiedSpawn {
    /// The id of the task that owns the process.
    #[must_use]
    pub const fn task(&self) -> TaskId {
        self.task
    }

    /// The identity of the process leading the child's group — the record a
    /// successor passes to
    /// [`reap_orphaned_group`](crate::rt::process::reap_orphaned_group),
    /// written down before [`Self::release`] lets the payload run.
    #[must_use]
    pub const fn leader(&self) -> &lgwks_std::process::ProcessIdentity {
        &self.leader
    }

    /// Whether the payload was released to run.
    #[must_use]
    pub const fn is_released(&self) -> bool {
        self.released
    }

    /// Release the payload to exec now, after the identity is recorded.
    ///
    /// The release waits until the child is observed stopped — a SIGCONT sent
    /// before the child stops itself would be lost, while one sent after the
    /// stop is observed resumes exactly the payload that waited. A leader
    /// that already exited, or whose pid the OS reissued to a stranger (told
    /// apart by the recorded start instant), needs no release: nothing of
    /// this spawn will run, so the release is already moot.
    ///
    /// Idempotent: releasing a released spawn is a no-op.
    ///
    /// # Errors
    ///
    /// The signal's own refusal when the child cannot be continued, or a
    /// timeout when the child never stops within its bound. Either leaves the
    /// spawn unreleased for a retry, and the supervisor's group guard still
    /// owns the child — a refused release never runs the payload unrecorded.
    #[cfg(all(unix, feature = "process"))]
    pub async fn release(&mut self) -> io::Result<()> {
        if self.released {
            return Ok(());
        }
        // The pid still names our leader: a reissued pid names a stranger the
        // release must not touch, and an exited leader needs no release.
        match lgwks_std::process::identify_process(self.pid)? {
            None => {
                self.released = true;
                return Ok(());
            }
            Some(current) if current.started() != self.leader.started() => {
                self.released = true;
                return Ok(());
            }
            Some(_) => {}
        }
        let Some(deadline) = std::time::Instant::now().checked_add(RELEASE_BUDGET) else {
            lgwks_std::trace::warn!(
                pid = self.pid,
                "GatedIdentifiedSpawn::release: the deadline overflowed the clock before the child stopped"
            );
            return Err(io::Error::other(
                "the release deadline overflowed the clock",
            ));
        };
        loop {
            if lgwks_std::process::child_has_exited_without_reaping(self.pid)? {
                self.released = true;
                return Ok(());
            }
            if lgwks_std::process::child_stopped_without_reaping(self.pid)? {
                match lgwks_std::process::continue_process(self.pid) {
                    Ok(()) => {
                        self.released = true;
                        return Ok(());
                    }
                    Err(error) if is_absence(&error) => {
                        self.released = true;
                        return Ok(());
                    }
                    Err(error) => {
                        lgwks_std::trace::warn!(
                            %error,
                            "GatedIdentifiedSpawn::release: the continue signal was refused"
                        );
                        return Err(error);
                    }
                }
            }
            if std::time::Instant::now() >= deadline {
                lgwks_std::trace::warn!(
                    pid = self.pid,
                    "GatedIdentifiedSpawn::release: a gated child never stopped behind its gate"
                );
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "a gated child that never stopped within {RELEASE_BUDGET:?} is not released"
                    ),
                ));
            }
            sleep(RELEASE_POLL).await;
        }
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

#[cfg(all(feature = "process", feature = "script"))]
use super::process::ProcessRunError;
#[cfg(all(unix, feature = "process"))]
use crate::rt::time::sleep;
// Per-tenant admission needs `script::Tenant` — the estate's one tenant
// identity, shared with step keys, `Host` and every run store — and
// `crate::task`, which needs it too, is gated the same way. The policy and the
// refusal are re-exported here because a caller reaches them through the
// supervisor that enforces them, and `rt::tenancy` is where they are defined.
#[cfg(feature = "script")]
pub use super::tenancy::{SpawnRefused, TenancyPolicy};

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
/// case and it is still terminated by cancellation, which `repeat` always
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

/// Why a `repeat` stopped.
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
/// adoptee that died on the bound's last pass lost its drain to the abort. A
/// Windows job settles inside its own two-second bound, so the same grace
/// covers it with room for a host under load.
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

/// Timed observations the post-reap confirmation adds while a signalled pid is
/// still running.
///
/// A SIGKILL is delivered at once and acted on at the process's next turn on the
/// scheduler, so on a loaded host a killed `setsid` descendant reads as running
/// for longer than [`CONTAINMENT_ROUNDS`] yields take: the rounds spend microseconds,
/// the death lands in milliseconds, and the receipt named a survivor that was
/// gone before the caller looked (`process_escape::a_session_escape_is_captured_and_stopped_by_pid`
/// at load 40, 2026-10-18). The passes wait out that window and no longer: 40 ×
/// [`start::REAP_POLL`] is 200 ms, spent only by a tree that still has a running pid,
/// so a clean cleanup costs nothing and a pid that does not die is still named.
#[cfg(all(unix, feature = "process"))]
const DEATH_CONFIRM_PASSES: usize = 40;

/// Group signals delivered on the drop-time path, while the unreaped leader still
/// pins the group id.
///
/// Bounded for the same reason the drain is: it covers the one window a capture
/// cannot — a member the leader forks after the capture and before it dies — and
/// the leader's death ends that window. It is a count of signals rather than a
/// sleep, so it costs nothing on the paths that stop early.
#[cfg(all(unix, feature = "process"))]
const GROUP_SIGNAL_ATTEMPTS: usize = 4;

/// How long [`GatedIdentifiedSpawn::release`] waits for the child to stop
/// itself before it refuses.
///
/// The stop lands on the child's first scheduling after the fork, so the wait
/// is milliseconds on any unloaded host; the bound is for the loaded one,
/// where the release still ends rather than parking the caller. Past the
/// bound the payload stays stopped under the supervisor's group guard, which
/// still owns it.
#[cfg(all(unix, feature = "process"))]
const RELEASE_BUDGET: Duration = Duration::from_secs(10);

/// How often a waiting [`GatedIdentifiedSpawn::release`] re-reads the child
/// while it starts.
///
/// A millisecond, like every other poll in this module: fine enough that the
/// release follows the stop with no added latency a caller could observe, and
/// coarse enough that the wait parks on the runtime clock instead of spinning.
#[cfg(all(unix, feature = "process"))]
const RELEASE_POLL: Duration = Duration::from_millis(1);

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

/// The async half of per-tenant admission: one lock over the pure round
/// scheduler, plus the parked waiters it hands permits to.
///
/// Beside [`Supervisor`] rather than inside [`rt::tenancy`](super::tenancy)
/// because it needs the supervisor's own permit pool and cancellation token,
/// and passing those across a module boundary would make them `pub(crate)` for a
/// type that already owns them privately. The scheduling decision itself lives
/// in [`rt::tenancy`](super::tenancy) and is untouched by any of this.
#[cfg(feature = "script")]
mod tenancy_support;

mod admit;
#[cfg(all(unix, feature = "process"))]
mod drive;
#[cfg(all(any(unix, windows), feature = "process"))]
mod ended;
#[cfg(all(unix, feature = "process"))]
mod group;
/// The Windows process path: a job object owns the child's tree (#263).
#[cfg(all(windows, feature = "process"))]
mod job;
/// Settling a job's tree to a receipt, held apart from the `kernel32` calls so
/// every host drives the same decision table against a scripted job.
#[cfg(all(feature = "process", any(windows, test)))]
mod job_cleanup;
mod lease;
mod outcome;
#[cfg(all(unix, feature = "process"))]
mod owners;
mod repeat;
mod report;
#[cfg(feature = "process")]
mod run;
#[cfg(all(unix, feature = "process"))]
mod start;
mod supervisor;
#[cfg(test)]
mod tests;

pub use self::outcome::{CleanupReceipt, TaskOutcome};
#[cfg(feature = "process")]
pub use self::outcome::{Containment, ResidualRisk};
pub use self::repeat::{repeat, repeat_on};
pub use self::report::{
    LiveTask, NextAction, ShutdownReport, Stats, SupervisorSnapshot, TaskState,
};
pub use self::supervisor::Supervisor;
