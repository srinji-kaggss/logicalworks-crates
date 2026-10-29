//! One declared logical clock: the authority every deadline in this crate names.
//!
//! Three clocks exist in any async system and they are not the same clock. The
//! engine's timer wheel, `std::time::Instant`, and a test's notion of "now" all
//! advance independently, and a bot that mixes them produces deadlines that
//! disagree with each other by exactly the amount nobody measured. This module
//! makes one of them authoritative and makes the difference *nameable*.
//!
//! # The three clocks, and what each governs
//!
//! | Clock | Governs | Sourced from |
//! |---|---|---|
//! | [`Clock`] (logical) | every deadline this crate evaluates: retry, sleep, budget, readiness, continuation | a caller-advanceable counter |
//! | the engine timer | when a `sleep` future actually resolves | the engine's driver |
//! | [`WallClock`] | the watchdog on things a logical clock cannot stop | `std::time::Instant` |
//!
//! # Why the wall clock is never governed by the logical one
//!
//! A logical clock is the right authority for anything a test can model: a retry
//! that backs off, a sleep, a readiness wait, a continuation deadline. It is the
//! wrong authority for a subprocess that has stopped responding, a blocking
//! callback that will never return, or a store whose `fsync` is stuck — none of
//! those are waiting for time to pass, they have stopped making progress
//! entirely, and a paused logical clock would wait on them forever. That is why
//! [`WallClock`] exists, is always on, and is never reachable from a paused
//! logical clock: [`Clock::wall`] hands out a wall watchdog that keeps running
//! with logical time frozen.
//!
//! The rule this encodes, which the module docs make a contract: **pausing
//! logical time never disables the wall-clock watchdog.** A test that pauses
//! time to examine a three-day outage still cannot hang on a process that
//! stopped responding, because the watchdog it was given reads real elapsed
//! time, not the counter it paused.
//!
//! # Determinism, and what is *not* deterministic
//!
//! Advancing a [`Clock`] is deterministic in the order work becomes eligible,
//! which is what makes a seeded replay meaningful. It is not a claim about
//! anything outside this process:
//!
//! - Poll *order* across worker threads is not deterministic, and a clock does
//!   not make it so. What a clock determinizes is which deadline is *eligible*,
//!   not which task the OS scheduler runs first.
//! - Actual observed external order — the order a remote peer saw two requests
//!   in, the order bytes hit a socket — is never deterministic and is not
//!   claimed here.
//! - Clock skew between hosts is real and unmeasured. A logical clock has no
//!   cross-host notion at all.
//! - Real-time liveness is the [`WallClock`]'s domain and is the only thing that
//!   says a subprocess is still answering.
//!
//! # Duration and remaining-deadline semantics across a restart
//!
//! A [`Clock`] is a **process-local** counter and it is explicitly *not*
//! serializable. A `std::time::Instant` is an opaque monotonic reading with an
//! undefined epoch, and writing it to a durable store as though it were a
//! timestamp produces a value that means nothing on another host, or after the
//! host's clock source changes. What survives a restart is the **remaining
//! duration**, never the instant:
//!
//! The test `a_restart_restores_the_spent_duration_and_the_budget_that_remains`
//! (`tests/it/clock_restart.rs`) runs it: a run records how much of a five-second budget it has
//! spent as a snapshot of elapsed time, a restart rebuilds the origin with `Clock::virtual_at`
//! from that duration, and the remaining budget is the same three seconds on either host.
//!
//! [`ClockSnapshot::remaining_from`] gives the remaining duration of a budget at
//! the recorded point, and a restarted clock restored to that point has the same
//! remaining budget. Nothing on this type can be mistaken for a wall-clock
//! timestamp, because it carries no epoch and no host identity.
//!
//! # No-runtime construction
//!
//! A [`Clock`] is built, advanced and read entirely from `std`. It does not
//! require a runtime, and constructing one inside an existing runtime is
//! allowed. Nothing here starts a runtime, promotes local work to remote
//! execution, or nests one runtime per task.
//!
//! [`Clock`]: crate::clock::Clock
//! [`Clock::wall`]: crate::clock::Clock::wall
//! [`WallClock`]: crate::clock::WallClock
//! [`ClockSnapshot::remaining_from`]: crate::clock::ClockSnapshot::remaining_from

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Largest duration a logical instant can represent, saturating rather than
/// wrapping.
///
/// One [`Duration`] below the saturation point. A clock that reached it would
/// be reporting a deadline nobody could wait out in a human lifetime, so the
/// value is a documented ceiling rather than a silently wrong number.
const MAX_ELAPSED: Duration = Duration::from_nanos(u64::MAX);

/// Which time source a [`Clock`] reads, and therefore what "advance" means.
///
/// A sum type rather than a boolean, because the two sources have different
/// contracts — one can be driven by a test and the other cannot — and a
/// `bool` field would let a caller construct a combination the module cannot
/// honour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TimeSource {
    /// Time follows the real monotonic clock. `advance` is not available
    /// because there is nothing to advance past.
    Wall,
    /// Time only moves when a caller advances it, so a test controls every
    /// deadline exactly and a three-day outage costs three arithmetic
    /// operations.
    Virtual,
}

/// The one logical clock a caller declares, and the authority for every deadline
/// this crate evaluates.
///
/// Cheap to clone: every clone shares the same counter, so a body can hold one
/// and a test can drive it from outside. This is what makes it a *declared*
/// clock rather than a value copied around.
#[derive(Debug, Clone)]
pub struct Clock {
    /// Shared so [`Clock::advance`] from a test and [`Clock::now`] from a body
    /// observe the same counter, and so a clock handed into a task does not
    /// become a second, independent timeline.
    inner: Arc<Inner>,
}

/// The shared state behind every [`Clock`] clone.
#[derive(Debug)]
struct Inner {
    /// Nanoseconds since the origin.
    ///
    /// Relaxed is correct and is the whole reason this is a clock rather than a
    /// channel: every read and write is of one `u64` that a caller either drives
    /// with [`Clock::advance`] or observes with [`Clock::now`]. There is no
    /// multi-field invariant to publish, so there is nothing to order against.
    /// A reader that observes a stale value observes a value from a
    /// still-consistent instant of a monotonically increasing counter, which is
    /// what an elapsed-time reading is.
    elapsed: AtomicU64,
    /// Whether this clock can be advanced by a caller.
    source: TimeSource,
    /// Where a wall clock was placed on its timeline, read through
    /// [`Inner::origin`].
    ///
    /// `None` for a virtual clock, whose timeline is the counter above and
    /// nothing else. The `Option` rather than a bare `Instant` because
    /// `Instant::now()` at construction would be a real read a virtual-clock
    /// caller never asked for, and a clock that touches the wall clock when it
    /// is supposed not to is precisely the coupling this module forbids.
    placed: Option<Instant>,
    /// Futures waiting for a caller to advance this clock to their deadline.
    ///
    /// Only a virtual clock keeps any: a wall clock's waits are the engine's
    /// timer and never reach here. This is the machinery that lets a test own
    /// the settle schedule: the action registers, the test advances, and the
    /// advance wakes it — no real time passes and no poll loop runs.
    #[cfg(feature = "time")]
    sleepers: std::sync::Mutex<Vec<Sleeper>>,
    /// Futures waiting for the *first* sleeper to appear, so a test driver can
    /// park while the run is busy and move the clock only when work is waiting.
    #[cfg(feature = "time")]
    watchers: std::sync::Mutex<Vec<std::task::Waker>>,
    /// Hands each waiter an identity, so a completion that raced an advance can
    /// remove exactly its own entry and nothing else.
    #[cfg(feature = "time")]
    next_sleeper: AtomicU64,
}

/// One future waiting for a virtual clock to reach `deadline`.
#[cfg(feature = "time")]
#[derive(Debug)]
struct Sleeper {
    /// The waiting future's own identity, used to withdraw it on a raced
    /// completion.
    id: u64,
    /// The instant, on this clock, the waiter is waiting for.
    deadline: Duration,
    /// What to wake when the clock reaches `deadline`.
    waker: std::task::Waker,
}

/// The most waiters one clock retains before it wakes them all and starts over.
///
/// The registry is a cache of parked futures, not a queue of work: a waiter
/// that is dropped without completing leaves its entry until the clock is
/// advanced past it. Bounding the list means a flow that abandons sleeps
/// cannot grow it across a run, and waking everyone costs one re-registration
/// each rather than a lost wake.
#[cfg(feature = "time")]
const MAX_CLOCK_SLEEPERS: usize = 4_096;

impl Inner {
    /// The real monotonic instant real-time readings count from.
    ///
    /// A wall clock's construction instant. A virtual clock has none, so a
    /// watchdog asked of it starts now — the moment it was asked for — which is
    /// the only real origin a virtual clock can honestly offer.
    fn origin(&self) -> Instant {
        match self.placed {
            // A wall clock carries the instant it was placed at, so every
            // reading of that clock counts from one stable origin.
            Some(placed) => placed,
            // A virtual clock has no placement, and `Instant::now()` is not a
            // stand-in for one: it is the only real origin a clock that was
            // never placed has, which is why the watchdog it hands out starts
            // when it is asked for. The two arms are different facts about the
            // clock, not one value covering for another.
            None => Instant::now(),
        }
    }
}

/// Why a clock operation was refused.
///
/// One error type rather than a `bool`, so a caller that ignores the refusal has
/// a value whose name says what it lost, not a silent no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClockError {
    /// The operation requires a caller-advanceable clock and this clock follows
    /// real time. Nothing was advanced and the clock did not move.
    NotVirtual,
    /// The requested advance would leave the representable range. The clock is
    /// unchanged; a caller that needs a longer horizon needs a new origin.
    OutOfRange {
        /// The advance that was refused.
        requested: Duration,
        /// The largest advance that would have been accepted.
        ceiling: Duration,
    },
}

impl fmt::Display for ClockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::NotVirtual => formatter
                .write_str("the clock follows real time and cannot be advanced by a caller"),
            Self::OutOfRange { requested, .. } => {
                write!(
                    formatter,
                    "advancing the logical clock by {requested:?} leaves the representable range"
                )
            }
        }
    }
}

impl std::error::Error for ClockError {}

impl Clock {
    /// A clock that follows real monotonic time.
    ///
    /// The default for production use: nothing has to drive it, and a deadline
    /// it reports is a real elapsed duration rather than a test's opinion.
    #[must_use]
    pub fn wall() -> Self {
        Self {
            inner: Arc::new(Inner {
                elapsed: AtomicU64::new(0),
                source: TimeSource::Wall,
                placed: Some(Instant::now()),
                #[cfg(feature = "time")]
                sleepers: std::sync::Mutex::new(Vec::new()),
                #[cfg(feature = "time")]
                watchers: std::sync::Mutex::new(Vec::new()),
                #[cfg(feature = "time")]
                next_sleeper: AtomicU64::new(0),
            }),
        }
    }

    /// A wall clock re-established at `elapsed`, for a restart.
    ///
    /// The same fact as [`Clock::wall`] with a known starting point: the origin
    /// is the recorded elapsed time the previous process reached, and real time
    /// advances from there. This is the form a durable record replays, and it is
    /// why a restart restores a **duration** and never a stored `Instant` — an
    /// `Instant` is process-local and means nothing here.
    ///
    /// `a_wall_clock_restored_after_a_restart_keeps_the_time_already_spent`
    /// (`tests/it/clock_restart.rs`) runs the case: a wall clock restored four seconds in reads at
    /// least four, and at most six of a ten-second budget remain while real time keeps spending them.
    #[must_use]
    pub fn wall_at(elapsed: Duration) -> Self {
        Self {
            inner: Arc::new(Inner {
                elapsed: AtomicU64::new(duration_to_nanos(elapsed)),
                source: TimeSource::Wall,
                placed: Some(Instant::now()),
                #[cfg(feature = "time")]
                sleepers: std::sync::Mutex::new(Vec::new()),
                #[cfg(feature = "time")]
                watchers: std::sync::Mutex::new(Vec::new()),
                #[cfg(feature = "time")]
                next_sleeper: AtomicU64::new(0),
            }),
        }
    }

    /// A caller-advanceable clock whose origin is `elapsed` after tick zero.
    ///
    /// The origin argument exists for **restart**, and is a duration since the
    /// origin rather than a wall-clock timestamp. A process that replays a
    /// durable record restores the origin from the recorded elapsed time, not
    /// from a stored `Instant`: an `Instant` is process-local, has no epoch, and
    /// means nothing on another host.
    ///
    /// `a_virtual_clock_starts_where_it_was_placed` (`tests/it/clock_restart.rs`) holds it: a virtual
    /// clock placed three seconds in reads three seconds.
    #[must_use]
    pub fn virtual_at(elapsed: Duration) -> Self {
        Self {
            inner: Arc::new(Inner {
                elapsed: AtomicU64::new(duration_to_nanos(elapsed)),
                source: TimeSource::Virtual,
                placed: None,
                #[cfg(feature = "time")]
                sleepers: std::sync::Mutex::new(Vec::new()),
                #[cfg(feature = "time")]
                watchers: std::sync::Mutex::new(Vec::new()),
                #[cfg(feature = "time")]
                next_sleeper: AtomicU64::new(0),
            }),
        }
    }

    /// Which time source this clock reads.
    #[must_use]
    pub fn source(&self) -> TimeSource {
        self.inner.source
    }

    /// Elapsed time since this clock's origin.
    ///
    /// A [`TimeSource::Virtual`] clock reports the counter a caller drives, so a
    /// test owns every deadline exactly and a three-day outage costs three
    /// arithmetic operations.
    ///
    /// A [`TimeSource::Wall`] clock reports where it was placed plus the
    /// **real** time elapsed since, sampled from [`std::time::Instant`] on every
    /// call. A fresh wall clock is placed at zero; one restored with
    /// [`Clock::wall_at`] keeps the time its previous process had already spent,
    /// so a restart does not hand a run its whole budget back. See
    /// [`Clock::placed_at`] for the placement alone.
    #[must_use]
    pub fn now(&self) -> Duration {
        match self.inner.source {
            TimeSource::Wall => self
                .placed_at()
                .saturating_add(self.inner.origin().elapsed()),
            TimeSource::Virtual => Duration::from_nanos(self.inner.elapsed.load(Ordering::Relaxed)),
        }
    }

    /// Where this clock was placed on its timeline, for either source.
    ///
    /// The origin a clock was restored to. For a wall clock it is the recorded
    /// elapsed time the restart re-established, so `now` starts from it and
    /// advances with real time; for a virtual clock it is the current counter,
    /// which is what [`Clock::snapshot`] persists.
    #[must_use]
    pub fn placed_at(&self) -> Duration {
        Duration::from_nanos(self.inner.elapsed.load(Ordering::Relaxed))
    }

    /// Move a caller-advanceable clock forward.
    ///
    /// An advance that would carry the clock past [`Clock::elapsed_ceiling`]
    /// **saturates at the ceiling** rather than refusing: a deadline computed from
    /// a wrapped clock fires immediately and is indistinguishable from one that
    /// legitimately expired, so landing on the ceiling is the honest answer to
    /// "advance further". The return value is where the clock actually landed, so
    /// a caller that asked for more and got less can see it.
    ///
    /// A clock already sitting at the ceiling refuses, because there is nothing
    /// left to land on and reporting success would be a lie about movement that
    /// did not happen.
    ///
    /// # Errors
    ///
    /// [`ClockError::NotVirtual`] on a wall clock, and
    /// [`ClockError::OutOfRange`] when the clock is already at its ceiling. In
    /// both cases the clock is unchanged.
    pub fn advance(&self, by: Duration) -> Result<Duration, ClockError> {
        if self.inner.source == TimeSource::Wall {
            let refusal = Err(ClockError::NotVirtual);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "advance: returning an error to the caller");
            return refusal;
        }
        let now = self.placed_at();
        if now >= Self::elapsed_ceiling() {
            let refusal = Err(ClockError::OutOfRange {
                requested: by,
                ceiling: Duration::ZERO,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "advance: returning an error to the caller");
            return refusal;
        }
        // Clamped rather than refused: see the saturation contract above.
        let by = duration_to_nanos(by.min(Self::elapsed_ceiling().saturating_sub(now)));
        // `fetch_add` would wrap; a compare-exchange loop is the only way to
        // saturate atomically, and it re-reads on each attempt so a concurrent
        // advance from another task cannot lose an update.
        let mut current = duration_to_nanos(now);
        loop {
            let next = current.saturating_add(by);
            match self.inner.elapsed.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    let landed = nanos_to_duration(next);
                    #[cfg(feature = "time")]
                    self.wake_due(landed);
                    return Ok(landed);
                }
                Err(observed) => {
                    // Another task advanced past the ceiling between the check
                    // above and this attempt. Refuse rather than move: the
                    // clock is at a horizon the caller was told about.
                    if observed >= duration_to_nanos(Self::elapsed_ceiling()) {
                        let refusal = Err(ClockError::OutOfRange {
                            requested: nanos_to_duration(by),
                            ceiling: Duration::ZERO,
                        });
                        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "advance: returning an error to the caller");
                        return refusal;
                    }
                    current = observed;
                }
            }
        }
    }

    // `Clock::sleep` is implemented in `rt::time`: a wall wait is the driver's
    // timer, and the core names no driver (ADR 0006 decision 4).

    #[doc(hidden)]
    /// Whether any future is parked on this clock's virtual sleep.
    ///
    /// The signal a test driver advances on: a virtual clock moves because
    /// work is waiting, never because a driver guessed it might be. Hidden for
    /// the same reason the flow's expansion seam is — a consumer waits through
    /// the action it runs and never asks this.
    #[cfg(feature = "time")]
    #[must_use]
    pub fn has_waiters(&self) -> bool {
        !self.lock_sleepers().is_empty()
    }

    #[doc(hidden)]
    /// Wait until a future is parked on this clock's virtual sleep.
    ///
    /// Completes at once when one already is. The other half of
    /// [`Clock::has_waiters`]: a driver parks here while the run is busy, so it
    /// spends no CPU and polls no child process, and is woken exactly when the
    /// run reaches a wait the clock must settle.
    #[cfg(feature = "time")]
    pub async fn wait_for_waiter(&self) {
        std::future::poll_fn(|context| {
            if self.has_waiters() {
                return std::task::Poll::Ready(());
            }
            let waker = context.waker().clone();
            let overflow = {
                let mut watchers = self.lock_watchers();
                if watchers.len() >= MAX_CLOCK_SLEEPERS {
                    Some(std::mem::take(&mut *watchers))
                } else {
                    None
                }
            };
            if let Some(woken) = overflow {
                for watcher in woken {
                    watcher.wake();
                }
            }
            self.lock_watchers().push(waker);
            // Re-read after registering, for the same reason the sleeper does.
            if self.has_waiters() {
                std::task::Poll::Ready(())
            } else {
                std::task::Poll::Pending
            }
        })
        .await;
    }

    /// [`Clock::sleep`] on a caller-advanceable clock: the half of the wait the
    /// core owns, because it needs no driver.
    #[cfg(feature = "time")]
    pub(crate) async fn sleep_virtual(&self, duration: Duration) {
        let deadline = self.now().saturating_add(duration);
        std::future::poll_fn(|context| self.poll_virtual_sleep(deadline, context)).await;
    }

    /// One poll of a virtual sleep: ready once the counter has reached
    /// `deadline`, registered with the clock until it has.
    #[cfg(feature = "time")]
    fn poll_virtual_sleep(
        &self,
        deadline: Duration,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        if self.now() >= deadline {
            return std::task::Poll::Ready(());
        }
        let id = self.inner.next_sleeper.fetch_add(1, Ordering::Relaxed);
        let waker = context.waker().clone();
        // Bound the registry: wake everyone, who will re-register, rather than
        // letting an abandoned sleep grow the list without limit.
        let overflow = {
            let mut sleepers = self.lock_sleepers();
            if sleepers.len() >= MAX_CLOCK_SLEEPERS {
                Some(
                    sleepers
                        .drain(..)
                        .map(|sleeper: Sleeper| sleeper.waker)
                        .collect::<Vec<_>>(),
                )
            } else {
                None
            }
        };
        if let Some(woken) = overflow {
            for sleeper in woken {
                sleeper.wake();
            }
        }
        self.lock_sleepers().push(Sleeper {
            id,
            deadline,
            waker,
        });
        self.wake_watchers();
        // Re-read after registering: an advance between the first read and the
        // push would otherwise leave this waiter parked for a wake that already
        // happened.
        if self.now() >= deadline {
            self.lock_sleepers().retain(|sleeper| sleeper.id != id);
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    }

    /// The waiter list, recovered from a poisoned lock.
    ///
    /// A panic while the list was held leaves no half-written entry — every
    /// mutation is one `push`, one `retain` or one `swap_remove` — so the
    /// recovered list is a list of waiters, not a corrupt one.
    #[cfg(feature = "time")]
    fn lock_sleepers(&self) -> std::sync::MutexGuard<'_, Vec<Sleeper>> {
        match self.inner.sleepers.lock() {
            Ok(held) => held,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// The watcher list, recovered from a poisoned lock.
    #[cfg(feature = "time")]
    fn lock_watchers(&self) -> std::sync::MutexGuard<'_, Vec<std::task::Waker>> {
        match self.inner.watchers.lock() {
            Ok(held) => held,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Wake everyone waiting for a sleeper to appear.
    #[cfg(feature = "time")]
    fn wake_watchers(&self) {
        let watching: Vec<std::task::Waker> = self.lock_watchers().drain(..).collect();
        for watcher in watching {
            watcher.wake();
        }
    }

    /// Wake every waiter whose deadline `now` has reached.
    #[cfg(feature = "time")]
    fn wake_due(&self, now: Duration) {
        let due = {
            let mut sleepers = self.lock_sleepers();
            let mut due = Vec::new();
            let mut index = 0;
            while index < sleepers.len() {
                if sleepers[index].deadline <= now {
                    due.push(sleepers.swap_remove(index).waker);
                } else {
                    index = index.saturating_add(1);
                }
            }
            due
        };
        for sleeper in due {
            sleeper.wake();
        }
    }

    /// The largest elapsed time a logical instant can represent.
    ///
    /// Reported rather than discovered, because a caller that needs a horizon
    /// past this needs a new origin and should be told that by name.
    #[must_use]
    pub const fn elapsed_ceiling() -> Duration {
        MAX_ELAPSED
    }

    /// A point-in-time reading suitable for persisting across a restart.
    ///
    /// The reading is a *duration since the origin*, never an absolute instant,
    /// and it carries no host identity — so a record written from one host and
    /// read on another is a duration, and cannot be mistaken for a cross-host
    /// timestamp.
    #[must_use]
    pub fn snapshot(&self) -> ClockSnapshot {
        ClockSnapshot {
            elapsed: self.now(),
        }
    }

    /// The independent wall-clock watchdog, sampling the real monotonic origin.
    fn watchdog(&self) -> WallClock {
        WallClock {
            started: self.inner.origin(),
        }
    }

    /// A wall-clock watchdog that keeps running while logical time is frozen.
    ///
    /// The point of this method is that pausing logical time cannot pause a
    /// subprocess watchdog, a blocking callback or a store hang. Those are not
    /// waiting for time; they have stopped making progress, and only real
    /// elapsed time says so.
    ///
    /// `the_wall_watchdog_keeps_running_while_logical_time_moves_a_day`
    /// (`tests/it/clock_restart.rs`) holds it: a logical day passes in one `advance` call and the
    /// watchdog, which reads real time, has not moved with it.
    #[must_use]
    pub fn wall_watchdog(&self) -> WallClock {
        self.watchdog()
    }
}

/// A persistable reading of a [`Clock`]'s elapsed time.
///
/// Deliberately *not* a `std::time::Instant`. An `Instant` is a process-local
/// monotonic reading with an undefined epoch; persisting one as a timestamp
/// produces a value that means nothing on another host. This carries only a
/// duration, and [`Clock::virtual_at`] takes exactly this back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub struct ClockSnapshot {
    /// Nanoseconds since the clock's origin.
    elapsed: Duration,
}

impl ClockSnapshot {
    /// Elapsed time the snapshot records.
    #[must_use]
    pub const fn elapsed(&self) -> Duration {
        self.elapsed
    }

    /// How much of `budget` was left at the instant this snapshot was taken.
    ///
    /// The restart-safe form: a deadline persisted as a *remaining duration*
    /// means the same thing on another host, where a persisted instant does not.
    /// A budget already spent yields [`Duration::ZERO`] rather than wrapping.
    #[must_use]
    pub fn remaining_from(&self, budget: Duration) -> Duration {
        budget.saturating_sub(self.elapsed)
    }

    /// Whether `budget` had already been spent at this snapshot.
    #[must_use]
    pub fn is_exhausted(&self, budget: Duration) -> bool {
        self.elapsed >= budget
    }
}

/// The independent wall-clock watchdog.
///
/// Always real time, never the logical counter, and therefore never paused by a
/// test. Constructed through [`Clock::wall_watchdog`] so that every watchdog in a
/// program traces back to a declared clock, rather than being a free-floating
/// [`Instant`].
#[derive(Debug, Clone, Copy)]
pub struct WallClock {
    /// The monotonic instant this watchdog started from.
    started: Instant,
}

impl WallClock {
    /// A watchdog whose origin is `started`.
    ///
    /// Crate-visible rather than public because a caller with an [`Instant`] in
    /// hand and no logical clock in scope has not declared a clock, and this
    /// module's whole contract is that every deadline names one. The one public
    /// constructor is [`Clock::wall_watchdog`].
    ///
    /// Gated with its only caller, `rt::time::Deadline`, which exists only under
    /// the `time` feature. Every other way to obtain a watchdog — including the
    /// observation phase's per-poll deadline — goes through a declared [`Clock`],
    /// which is the point of the visibility restriction above.
    #[cfg(feature = "time")]
    pub(crate) fn from_instant(started: Instant) -> Self {
        Self { started }
    }

    /// Real elapsed time since this watchdog was created.
    ///
    /// Saturates at zero if the OS reports the start after the sample, which no
    /// monotonic source should do; the direction is chosen so a caller never
    /// computes a negative remaining budget.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        Instant::now().saturating_duration_since(self.started)
    }
}

/// Nanoseconds in `duration`, saturating at the representable ceiling.
///
/// `u64::MAX` nanoseconds is about 584 years, so the ceiling is never reached by
/// a real budget; it exists so that a caller who somehow asks for more gets the
/// documented ceiling rather than a wrapped instant.
///
/// Split rather than widen-then-narrow: `Duration` reports seconds and
/// sub-second nanoseconds through two infallible projections, so the only
/// arithmetic left is the scaling, and `saturating_mul`/`saturating_add` put
/// the ceiling in that arithmetic instead of in a conversion that could fail
/// and leave a caller holding a default.
///
/// Crate-visible because the script budget's shared window stamps its earned
/// credit with the same conversion, so a window stamp and a clock deadline
/// are measured by one rule.
pub(crate) fn duration_to_nanos(duration: Duration) -> u64 {
    const NANOS_PER_SEC: u64 = 1_000_000_000;
    duration
        .as_secs()
        .saturating_mul(NANOS_PER_SEC)
        .saturating_add(u64::from(duration.subsec_nanos()))
}

/// `nanos` nanoseconds as a [`Duration`].
///
/// The inverse of [`duration_to_nanos`] with no intermediate arithmetic, so
/// there is no quotient to truncate and no remainder to narrow.
fn nanos_to_duration(nanos: u64) -> Duration {
    Duration::from_nanos(nanos)
}

#[cfg(all(test, feature = "time"))]
mod tests {
    use super::Clock;
    use std::future::Future as _;
    use std::pin::pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll, Wake, Waker};
    use std::time::Duration;

    /// A waker that counts its wakes, so the test can prove the advance
    /// reached the parked future rather than only that its condition held.
    struct Countdown(AtomicUsize);

    impl Wake for Countdown {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// A virtual sleep is pending until an advance reaches its deadline, and
    /// then ready — with no runtime, no timer and no real time spent.
    #[test]
    fn a_virtual_sleep_waits_for_the_advance_that_reaches_its_deadline() {
        let clock = Clock::virtual_at(Duration::ZERO);
        let woken = Arc::new(Countdown(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&woken));
        let mut context = Context::from_waker(&waker);
        let mut sleep = pin!(clock.sleep(Duration::from_millis(10)));
        assert!(
            matches!(sleep.as_mut().poll(&mut context), Poll::Pending),
            "a virtual clock nobody advanced must not complete its sleep"
        );
        assert!(
            clock.advance(Duration::from_millis(5)).is_ok(),
            "half the deadline is inside the representable range"
        );
        assert!(
            matches!(sleep.as_mut().poll(&mut context), Poll::Pending),
            "half the deadline is not the deadline"
        );
        assert!(
            clock.advance(Duration::from_millis(5)).is_ok(),
            "the second half is inside the representable range"
        );
        assert!(
            woken.0.load(Ordering::Relaxed) >= 1,
            "the advance must wake the parked sleep, not only satisfy its condition"
        );
        assert!(
            matches!(sleep.as_mut().poll(&mut context), Poll::Ready(())),
            "an advance that reaches the deadline must complete the sleep"
        );
    }
}
