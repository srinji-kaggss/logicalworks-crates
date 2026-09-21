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
use std::future::Future;
use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lgwks_deps::tokio::sync::{OwnedSemaphorePermit, Semaphore};
use lgwks_deps::tokio::task::Id;

use super::cancel::CancellationToken;
use super::task::{JoinSet, yield_now};

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

/// Returned by [`Supervisor::try_spawn`] when the in-flight bound is reached.
///
/// The refusal is counted in [`Stats::refused`]. A caller that inspects neither
/// this value nor the counter has an unbounded producer and a bounded consumer,
/// which is a decision to drop work rather than a decision to grow memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct AtCapacity;

impl fmt::Display for AtCapacity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the supervisor is at its in-flight bound")
    }
}

impl std::error::Error for AtCapacity {}

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
/// The four states are a product, not a single flag: a caller's response to a
/// panic (surface the payload, page someone) is not its response to a
/// cancellation (expected, nothing to do) nor to an abort (a task ignored its
/// token, which is a bug in the body).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TaskOutcome {
    /// The body ran to completion and returned normally.
    Completed {
        /// The task that ended.
        task: TaskId,
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
}

impl TaskOutcome {
    /// The task this outcome is about.
    #[must_use]
    pub const fn task(&self) -> TaskId {
        match *self {
            Self::Completed { task }
            | Self::Cancelled { task }
            | Self::Aborted { task }
            | Self::Panicked { task, .. } => task,
        }
    }

    /// Whether the body ran to completion.
    ///
    /// `false` for every other state, including cancellation, so a caller that
    /// asks this question cannot accidentally read "the task stopped existing"
    /// as "the task did its work".
    #[must_use]
    pub const fn is_success(&self) -> bool {
        matches!(self, Self::Completed { .. })
    }

    /// Whether the body panicked.
    #[must_use]
    pub const fn is_panic(&self) -> bool {
        matches!(self, Self::Panicked { .. })
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
            Self::Completed { .. } | Self::Cancelled { .. } | Self::Aborted { .. } => None,
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
}

impl ShutdownReport {
    /// Every drained outcome, in completion order.
    #[must_use]
    pub fn outcomes(&self) -> &[TaskOutcome] {
        &self.outcomes
    }

    /// Consume the report and return its outcomes.
    #[must_use]
    pub fn into_outcomes(self) -> Vec<TaskOutcome> {
        self.outcomes
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
    /// not.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.outcomes.iter().all(TaskOutcome::is_success)
    }
}

/// What a [`Supervisor`] has done so far.
///
/// Counters saturate rather than wrap, so they stay monotonic over any lifetime
/// a process can actually reach. `completed` counts tasks that *ended*, however
/// they ended, and is split into the four ways that can happen: a caller that
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
    /// Equal to `succeeded + cancelled + aborted + panicked`. It is the
    /// accounting total, not a success count.
    pub completed: u64,
    /// Tasks whose body ran to completion and returned.
    pub succeeded: u64,
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
    /// Tasks started and not yet finished.
    ///
    /// Saturated subtraction: `completed` cannot exceed `spawned` because both
    /// advance on the same path, but a counter pair that silently wrapped would
    /// be worse than one that pins at zero.
    #[must_use]
    pub const fn in_flight(self) -> u64 {
        self.spawned.saturating_sub(self.completed)
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
    /// Maps the engine's task id to this module's, for the one path that cannot
    /// carry a [`TaskId`] in the value: a terminal `JoinError`, which is the
    /// error case and therefore has no value to carry it. Entries are removed
    /// as tasks are joined, so it is bounded by the in-flight ceiling.
    identities: BTreeMap<Id, TaskId>,
    /// Terminal outcomes not yet drained by the caller. Capped at the in-flight
    /// ceiling; see [`Stats::reports_dropped`].
    reports: VecDeque<TaskOutcome>,
    /// The retention cap for [`Self::reports`].
    report_cap: usize,
    /// Next [`TaskId`] to hand out. Saturating, like the counters.
    next_task: u64,
    /// Tasks started. Saturating.
    spawned: u64,
    /// Tasks finished, however they ended. Saturating.
    completed: u64,
    /// Tasks that ran to completion. Saturating.
    succeeded: u64,
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
}

/// What a supervised body reported when it returned.
///
/// The supervisor's wrapper decides which of the two a body's normal return
/// means, because only the wrapper knows whether the body was built to observe
/// its token. A `JoinError` is the third and fourth case, and is not a `TaskEnd`
/// at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskEnd {
    /// The body ran to the end of its own work.
    Completed,
    /// The body stopped because its token was cancelled.
    Cancelled,
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

/// Bounded number of yields [`Supervisor::shutdown`] gives cooperative bodies.
///
/// A supervisor cannot wait for a body that never observes its token, so the
/// wait has to be finite. This is finite by construction: a fixed count of
/// yields, after which everything still running is aborted. It exists so that a
/// body which *is* cooperative is reported as cancelled rather than aborted —
/// the abort is the fallback for the other case, not the first response.
const COOPERATIVE_DRAIN_YIELDS: u8 = 8;

impl Supervisor {
    /// Create a supervisor that runs at most `max_in_flight` tasks at once.
    ///
    /// A `max_in_flight` of zero is treated as one, and one above
    /// [`Semaphore::MAX_PERMITS`] is clamped to it, so there is no argument
    /// that produces an unbounded supervisor.
    #[must_use]
    pub fn new(max_in_flight: usize) -> Self {
        let bound = max_in_flight.clamp(1, Semaphore::MAX_PERMITS);
        Self {
            token: CancellationToken::new(),
            set: JoinSet::new(),
            permits: Arc::new(Semaphore::new(bound)),
            identities: BTreeMap::new(),
            reports: VecDeque::new(),
            report_cap: bound,
            next_task: 0,
            spawned: 0,
            completed: 0,
            succeeded: 0,
            cancelled: 0,
            aborted: 0,
            panicked: 0,
            refused: 0,
            reports_dropped: 0,
        }
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
            cancelled: self.cancelled,
            aborted: self.aborted,
            panicked: self.panicked,
            refused: self.refused,
            reports_dropped: self.reports_dropped,
        }
    }

    /// The oldest terminal outcome this supervisor has not yet handed over.
    ///
    /// `None` means every task that has ended has been reported and read. A
    /// caller that reads this in a loop holds no history at all, which is how
    /// the cap in [`Stats::reports_dropped`] is kept from ever being reached.
    pub fn next_report(&mut self) -> Option<TaskOutcome> {
        self.reports.pop_front()
    }

    /// Join every task that has already finished, returning how many were
    /// joined.
    ///
    /// Each entry point calls this before spawning, which is what keeps the
    /// retained set proportional to the live bound rather than to the total
    /// number of spawns. It is public because a caller who has stopped spawning
    /// and wants the counters to settle should not have to spawn a task to make
    /// that happen.
    ///
    /// Every joined task produces exactly one [`TaskOutcome`], readable through
    /// [`Supervisor::next_report`].
    pub fn reap(&mut self) -> usize {
        let mut reaped: usize = 0;
        while let Some(joined) = self.set.try_join_next_with_id() {
            self.absorb(joined, Retention::Capped);
            reaped = reaped.saturating_add(1);
        }
        reaped
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
        let Some(permit) = self.claim().await else {
            return;
        };
        let token = self.child_token();
        let future = body(token.clone());
        self.place(permit, async move {
            future.await;
            // The reading is taken at the instant the body returned, so it is
            // a fact about the supervisor's own state rather than a guess at
            // the body's intent: a body that returned while cancellation was
            // in force is reported cancelled, and one that returned with no
            // cancellation in force is reported completed.
            if token.is_cancelled() {
                TaskEnd::Cancelled
            } else {
                TaskEnd::Completed
            }
        });
    }

    /// Place `body` on this supervisor, refusing if the bound is reached.
    ///
    /// The non-blocking counterpart to [`Supervisor::spawn`]. A refusal is a
    /// value, not a silent drop.
    ///
    /// # Errors
    ///
    /// [`AtCapacity`] when every slot is taken.
    pub fn try_spawn<F, Fut>(&mut self, body: F) -> Result<(), AtCapacity>
    where
        F: FnOnce(CancellationToken) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let Some(permit) = self.claim_now() else {
            return Err(AtCapacity);
        };
        let token = self.child_token();
        let future = body(token.clone());
        self.place(permit, async move {
            future.await;
            if token.is_cancelled() {
                TaskEnd::Cancelled
            } else {
                TaskEnd::Completed
            }
        });
        Ok(())
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
        self.place(permit, async move {
            match repeat(&token, budget, body).await {
                Outcome::Exhausted { .. } => TaskEnd::Completed,
                Outcome::Cancelled { .. } => TaskEnd::Cancelled,
            }
        });
    }

    /// Cancel every task, wait for the set to drain, and return what it
    /// drained.
    ///
    /// Cancellation is cooperative first. A task built on
    /// [`Supervisor::spawn_repeating`] or [`repeat`] observes the token and
    /// returns, which is reported as [`TaskOutcome::Cancelled`]; a task that
    /// never observes its token is aborted after a bounded number of yields, so
    /// this always terminates: there is no argument or task shape that makes it
    /// hang. An aborted task is reported as [`TaskOutcome::Aborted`] rather than
    /// as a completed one, which is the difference this return value exists to
    /// make.
    pub async fn shutdown(mut self) -> ShutdownReport {
        self.token.cancel();
        // Give a cooperative body room to return on its own before the abort
        // lands, so `Cancelled` and `Aborted` mean what they say. Bounded by a
        // constant: this never waits on a body that ignores its token.
        for _ in 0..COOPERATIVE_DRAIN_YIELDS {
            yield_now().await;
        }
        // Everything still running is dropped here, which is what makes the
        // drain below terminate rather than wait for a body that will not stop.
        self.set.abort_all();
        while let Some(joined) = self.set.join_next_with_id().await {
            self.absorb(joined, Retention::Draining);
        }
        let stats = self.stats();
        let outcomes: Vec<TaskOutcome> = self.reports.drain(..).collect();
        ShutdownReport { outcomes, stats }
    }

    /// Take a permit, waiting for one if the bound is reached, and reap first.
    ///
    /// `None` is a refusal, already counted: the semaphore is never closed by
    /// this module, so the only reachable refusal is a saturated one.
    async fn claim(&mut self) -> Option<OwnedSemaphorePermit> {
        self.reap();
        match Arc::clone(&self.permits).acquire_owned().await {
            Ok(permit) => Some(permit),
            // Unreachable: this supervisor never closes its semaphore, and
            // `forbid(expect_used)` rules out the assert. Counting it as a
            // refusal keeps the failure observable instead of silently
            // swallowed, which a bare `return` would be.
            Err(_) => {
                self.refused = self.refused.saturating_add(1);
                None
            }
        }
    }

    /// The non-blocking counterpart of [`Supervisor::claim`].
    fn claim_now(&mut self) -> Option<OwnedSemaphorePermit> {
        self.reap();
        match Arc::clone(&self.permits).try_acquire_owned() {
            Ok(permit) => Some(permit),
            Err(_) => {
                self.refused = self.refused.saturating_add(1);
                None
            }
        }
    }

    /// Give the future a [`TaskId`], place it, and register the identity.
    ///
    /// The identity is registered before the task can run to completion,
    /// because the spawn and the insert happen on this thread with no await
    /// between them, so a terminal report always finds its entry.
    fn place<Fut>(&mut self, permit: OwnedSemaphorePermit, future: Fut)
    where
        Fut: Future<Output = TaskEnd> + Send + 'static,
    {
        let task = TaskId(self.next_task);
        self.next_task = self.next_task.saturating_add(1);
        self.spawned = self.spawned.saturating_add(1);
        let handle = self.set.spawn(async move {
            let _permit = permit;
            future.await
        });
        self.identities.insert(handle.id(), task);
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
                    .map(|task| TaskOutcome::Completed { task })
            }
            Ok((_, TaskEnd::Cancelled)) => {
                self.cancelled = self.cancelled.saturating_add(1);
                self.identities
                    .remove(&raw)
                    .map(|task| TaskOutcome::Cancelled { task })
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

impl Drop for Supervisor {
    /// Cancel the token and abort the set.
    ///
    /// This is the guarantee that a supervisor which goes out of scope takes
    /// its tasks with it. It is deliberately synchronous: `Drop` cannot await,
    /// so tasks are aborted rather than joined. Use [`Supervisor::shutdown`]
    /// when the caller needs to know they have finished before continuing.
    fn drop(&mut self) {
        self.token.cancel();
        self.set.abort_all();
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
pub async fn repeat<F, Fut>(token: &CancellationToken, budget: Budget, mut body: F) -> Outcome
where
    F: FnMut(u64) -> Fut,
    Fut: Future<Output = ()>,
{
    // Both bounds are computed once, outside the loop, and neither uses an
    // arithmetic operator: `checked_add` returns `None` instead of wrapping, and
    // a budget of `Duration::MAX` is a deadline that never arrives rather than
    // one that arrives immediately.
    let iteration_limit = match budget {
        Budget::Iterations(limit) => Some(limit.get()),
        Budget::For(_) | Budget::Ongoing => None,
    };
    let deadline = match budget {
        Budget::For(limit) => Instant::now().checked_add(limit),
        Budget::Iterations(_) | Budget::Ongoing => None,
    };

    let mut iterations: u64 = 0;
    loop {
        if token.is_cancelled() {
            return Outcome::Cancelled { iterations };
        }
        // Both bounds are "already spent" predicates rather than nested
        // conditionals, so the two ways a budget can run out read as one
        // decision and the loop has a single exit for each reason.
        let spent = iteration_limit.is_some_and(|limit| iterations >= limit)
            || deadline.is_some_and(|deadline| Instant::now() >= deadline);
        if spent {
            return Outcome::Exhausted { iterations };
        }
        match token.run_until_cancelled(body(iterations)).await {
            Some(()) => iterations = iterations.saturating_add(1),
            None => return Outcome::Cancelled { iterations },
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{
        AtCapacity, Budget, MAX_PANIC_MESSAGE_CHARS, Outcome, Supervisor, TaskId, TaskOutcome,
        repeat, truncate_panic_message,
    };
    use crate::rt::cancel::CancellationToken;
    use crate::rt::runtime::block_on;
    use crate::rt::task::yield_now;
    use std::future::pending;
    use std::num::NonZeroU64;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

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

    #[test]
    fn an_ongoing_budget_stops_when_the_token_is_cancelled() {
        let token = CancellationToken::new();
        let trigger = token.clone();
        let outcome = block_on(repeat(&token, Budget::Ongoing, move |_tick| {
            let trigger = trigger.clone();
            async move {
                trigger.cancel();
            }
        }));
        assert_eq!(
            outcome,
            Outcome::Cancelled { iterations: 1 },
            "a cancel during the first iteration must end the loop after it"
        );
    }

    #[test]
    fn cancellation_interrupts_a_body_that_is_still_awaiting() {
        let token = CancellationToken::new();
        let trigger = token.clone();
        let outcome = block_on(repeat(&token, Budget::Ongoing, move |_tick| {
            let trigger = trigger.clone();
            async move {
                trigger.cancel();
                // Never resolves. If cancellation were only observed between
                // iterations, the loop would have to await this to completion
                // and the test would hang rather than fail.
                pending::<()>().await;
            }
        }));
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
                Err(AtCapacity),
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
                Err(AtCapacity),
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
}
