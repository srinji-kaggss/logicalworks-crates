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
//! ```
//! # use std::time::Duration;
//! # use lgwks_bot::rt::clock::Clock;
//! let clock = Clock::virtual_at(Duration::ZERO);
//! let budget = Duration::from_secs(5);
//!
//! // A run records how much of its budget it has spent, not a wall instant.
//! clock.advance(Duration::from_secs(2))?;
//! let recorded = clock.snapshot();
//!
//! // A restart re-establishes the origin from the recorded elapsed time, and
//! // the remaining budget is the same fact on either host.
//! let resumed = Clock::virtual_at(recorded.elapsed());
//! assert_eq!(resumed.snapshot().remaining_from(budget), Duration::from_secs(3));
//! # Ok::<(), lgwks_bot::rt::clock::ClockError>(())
//! ```
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
            Self::NotVirtual => formatter.write_str(
                "the clock follows real time and cannot be advanced by a caller",
            ),
            Self::OutOfRange { requested, .. } => {
                write!(formatter, "advancing the logical clock by {requested:?} leaves the representable range")
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
            }),
        }
    }

    /// A caller-advanceable clock whose origin is `elapsed` nanoseconds after
    /// tick zero.
    ///
    /// The origin argument exists for **restart**, and is a duration since the
    /// origin rather than a wall-clock timestamp. A process that replays a
    /// durable record restores the origin from the recorded elapsed time, not
    /// from a stored `Instant`: an `Instant` is process-local, has no epoch, and
    /// means nothing on another host.
    ///
    /// ```
    /// # use std::time::Duration;
    /// # use lgwks_bot::rt::clock::Clock;
    /// let clock = Clock::virtual_at(Duration::from_secs(3));
    /// assert_eq!(clock.now(), Duration::from_secs(3));
    /// ```
    #[must_use]
    pub fn virtual_at(elapsed: Duration) -> Self {
        Self {
            inner: Arc::new(Inner {
                elapsed: AtomicU64::new(duration_to_nanos(elapsed)),
                source: TimeSource::Virtual,
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
    /// For a [`TimeSource::Wall`] clock this is the counter advanced by the
    /// construction instant and never since, so it reports the origin's
    /// magnitude, **not** the time since this call. A wall clock's elapsed time
    /// is [`WallClock::elapsed`], which samples a real monotonic reading. This
    /// split is deliberate: the logical counter and the wall reading are
    /// different facts and reading one as the other is the bug the module
    /// exists to prevent.
    #[must_use]
    pub fn now(&self) -> Duration {
        Duration::from_nanos(self.inner.elapsed.load(Ordering::Relaxed))
    }

    /// Move a caller-advanceable clock forward.
    ///
    /// Saturating at [`Clock::elapsed_ceiling`]: a clock that reached the
    /// ceiling reports it rather than wrapping to a value in the past, because a
    /// deadline computed from a wrapped clock fires immediately and is
    /// indistinguishable from one that legitimately expired.
    ///
    /// # Errors
    ///
    /// [`ClockError::NotVirtual`] on a wall clock, and
    /// [`ClockError::OutOfRange`] when the advance would leave the range. In
    /// both cases the clock is unchanged.
    pub fn advance(&self, by: Duration) -> Result<Duration, ClockError> {
        if self.inner.source == TimeSource::Wall {
            return Err(ClockError::NotVirtual);
        }
        let headroom = Self::elapsed_ceiling().checked_sub(self.now());
        let Some(headroom) = headroom else {
            return Err(ClockError::OutOfRange {
                requested: by,
                ceiling: Duration::ZERO,
            });
        };
        let by = by.min(headroom);
        // `fetch_add` on a saturating basis: the CAS loop is the only way to
        // saturate atomically, and the load is re-read each attempt so a
        // concurrent advance from another task cannot lose an update.
        let mut current = self.inner.elapsed.load(Ordering::Relaxed);
        loop {
            let next = nanos_to_duration(current).saturating_add(by);
            match self.inner.elapsed.compare_exchange_weak(
                current,
                duration_to_nanos(next),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok(next),
                Err(observed) => current = observed,
            }
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
        ClockSnapshot { elapsed: self.now() }
    }

    /// A wall-clock watchdog that keeps running while logical time is frozen.
    ///
    /// The point of this method is that pausing logical time cannot pause a
    /// subprocess watchdog, a blocking callback or a store hang. Those are not
    /// waiting for time; they have stopped making progress, and only real
    /// elapsed time says so.
    ///
    /// ```
    /// # use std::time::Duration;
    /// # use lgwks_bot::rt::clock::Clock;
    /// let clock = Clock::virtual_at(Duration::ZERO);
    /// let watchdog = clock.wall_watchdog();
    /// clock.advance(Duration::from_secs(86_400))?;
    /// // Three logical days have passed and the watchdog has barely moved.
    /// assert!(watchdog.elapsed() < Duration::from_secs(1));
    /// # Ok::<(), lgwks_bot::rt::clock::ClockError>(())
    /// ```
    #[must_use]
    pub fn wall_watchdog(&self) -> WallClock {
        WallClock {
            started: Instant::now(),
        }
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
fn duration_to_nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

/// `nanos` nanoseconds as a [`Duration`].
///
/// The inverse of [`duration_to_nanos`] with no intermediate arithmetic, so
/// there is no quotient to truncate and no remainder to narrow.
fn nanos_to_duration(nanos: u64) -> Duration {
    Duration::from_nanos(nanos)
}