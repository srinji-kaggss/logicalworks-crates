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
//!   The supervisor owns completion and exposes it as [`Stats`].
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
//! supervisor.shutdown().await;
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

use std::fmt;
use std::future::Future;
use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lgwks_deps::tokio::sync::Semaphore;

use super::cancel::CancellationToken;
use super::task::{JoinSet, yield_now};

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

/// What a [`Supervisor`] has done so far.
///
/// Counters saturate rather than wrap, so they stay monotonic over any lifetime
/// a process can actually reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Stats {
    /// Tasks started, including any that have since finished.
    pub spawned: u64,
    /// Tasks that have finished, however they ended.
    pub completed: u64,
    /// Spawn attempts refused by [`Supervisor::try_spawn`] at the bound.
    pub refused: u64,
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
/// leaking and the one that keeps a loop from running away.
pub struct Supervisor {
    /// Parents every task's token. Cancelling it stops the lot; a task holding
    /// a child token does not keep this supervisor alive, because the link runs
    /// child-to-parent only.
    token: CancellationToken,
    /// Live tasks, tracked so `Drop` can abort them. Never detached.
    set: JoinSet<()>,
    /// The in-flight ceiling. Shared with the tasks, each of which holds one
    /// permit for its lifetime and releases it on completion or abort.
    permits: Arc<Semaphore>,
    /// Tasks started. Saturating.
    spawned: u64,
    /// Tasks finished. Saturating.
    completed: u64,
    /// Spawn attempts refused at the bound. Saturating.
    refused: u64,
}

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
            spawned: 0,
            completed: 0,
            refused: 0,
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
    /// The asynchronous counterpart that also waits is [`Supervisor::shutdown`].
    pub fn cancel(&self) {
        self.token.cancel();
    }

    /// A snapshot of the counters.
    #[must_use]
    pub fn stats(&self) -> Stats {
        Stats {
            spawned: self.spawned,
            completed: self.completed,
            refused: self.refused,
        }
    }

    /// Join every task that has already finished, returning how many were
    /// joined.
    ///
    /// Each entry point calls this before spawning, which is what keeps the
    /// retained set proportional to the live bound rather than to the total
    /// number of spawns. It is public because a caller who has stopped spawning
    /// and wants the counters to settle should not have to spawn a task to make
    /// that happen.
    pub fn reap(&mut self) -> usize {
        let mut reaped: usize = 0;
        while self.set.try_join_next().is_some() {
            self.completed = self.completed.saturating_add(1);
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
        self.reap();
        let Ok(permit) = Arc::clone(&self.permits).acquire_owned().await else {
            // Unreachable: this supervisor never closes its semaphore, and
            // `forbid(expect_used)` rules out the assert. Counting it as a
            // refusal keeps the failure observable instead of silently
            // swallowed, which a bare `return` would be.
            self.refused = self.refused.saturating_add(1);
            return;
        };
        let future = body(self.child_token());
        self.spawned = self.spawned.saturating_add(1);
        self.set.spawn(async move {
            let _permit = permit;
            future.await;
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
        self.reap();
        let Ok(permit) = Arc::clone(&self.permits).try_acquire_owned() else {
            self.refused = self.refused.saturating_add(1);
            return Err(AtCapacity);
        };
        let future = body(self.child_token());
        self.spawned = self.spawned.saturating_add(1);
        self.set.spawn(async move {
            let _permit = permit;
            future.await;
        });
        Ok(())
    }

    /// Start a task that repeats under `budget` until the budget or the
    /// supervisor ends it.
    ///
    /// This is the shape a long-running bot loop should take. The bound is a
    /// required argument, so there is no call that reads as "loop forever".
    pub async fn spawn_repeating<F, Fut>(&mut self, budget: Budget, body: F)
    where
        F: FnMut(u64) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.spawn(move |token| async move {
            // The outcome is the task's own report; it is not an error, and
            // there is nobody left to hand it to once the task is running, so
            // it is dropped deliberately rather than ignored by accident.
            let _outcome = repeat(&token, budget, body).await;
        })
        .await;
    }

    /// Cancel every task and wait for the set to drain.
    ///
    /// A task built on [`Supervisor::spawn_repeating`] or [`repeat`] observes
    /// the token and returns from its loop, and `repeat` yields the executor
    /// every few iterations, so a repeating loop no longer has to *suspend* to
    /// become cancellable. One that never observes its token at all is aborted,
    /// which takes effect the next time that task yields.
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
    pub async fn shutdown(mut self) {
        self.token.cancel();
        self.set.shutdown().await;
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
            || deadline.is_some_and(|deadline| Instant::now() >= deadline);
        if spent {
            return Outcome::Exhausted { iterations };
        }
        match token.run_until_cancelled(body(iterations)).await {
            Some(()) => iterations = iterations.saturating_add(1),
            None => return Outcome::Cancelled { iterations },
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

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{AtCapacity, Budget, Outcome, Supervisor, repeat};
    use crate::rt::cancel::CancellationToken;
    use crate::rt::runtime::block_on;
    use crate::rt::task::yield_now;
    use std::future::pending;
    use std::num::NonZeroU64;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

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
            supervisor.shutdown().await;
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
}
