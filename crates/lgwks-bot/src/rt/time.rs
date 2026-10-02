//! Async timers.
//!
//! This module wraps the engine's timer constructors rather than re-exporting
//! them. tokio's `sleep`/`timeout` capture the runtime timer **at
//! construction**, so building one before entering the runtime panics with
//! "there is no reactor running", the single most common tokio footgun. The
//! wrappers here defer construction to first poll, so `sleep(d)` may be built
//! anywhere and awaited inside a runtime.
//!
//! `interval` is the exception: it is a value, not a future, so it must still be
//! constructed inside a runtime context. Call it inside an `async` block.

use std::future::Future;

pub use lgwks_deps::tokio::time::error::Elapsed;
pub use lgwks_deps::tokio::time::{Instant, MissedTickBehavior, interval, interval_at};
pub use std::time::Duration;

pub use super::clock::{Clock, ClockError, ClockSnapshot, TimeSource, WallClock};

/// Wait for `duration` to elapse.
///
/// The deadline is captured on first poll, not at construction, so this may be
/// created before the runtime is entered. It panics if polled outside a runtime
/// with the `time` driver, exactly as any timer must.
pub async fn sleep(duration: Duration) {
    lgwks_deps::tokio::time::sleep(duration).await;
}

/// Wait until `deadline` passes. The deadline is captured on first poll.
pub async fn sleep_until(deadline: Instant) {
    lgwks_deps::tokio::time::sleep_until(deadline).await;
}

/// Run `future`, returning [`Err(Elapsed)`](Elapsed) if `duration` passes first.
///
/// Construction is safe outside the runtime; the timer starts on first poll.
pub async fn timeout<F: Future>(duration: Duration, future: F) -> Result<F::Output, Elapsed> {
    lgwks_deps::tokio::time::timeout(duration, future).await
}

/// Run `future`, returning [`Err(Elapsed)`](Elapsed) if `deadline` passes first.
pub async fn timeout_at<F: Future>(deadline: Instant, future: F) -> Result<F::Output, Elapsed> {
    lgwks_deps::tokio::time::timeout_at(deadline, future).await
}

/// A deadline named by a declared [`Clock`] rather than by an opaque
/// [`Instant`].
///
/// This is the bridge the module is about. A caller who writes
/// `timeout_at(Instant::now() + d)` has made a deadline that only means anything
/// to the process that created it: the instant cannot be logged, compared across
/// a restart, or driven by a test. A [`Deadline`] carries the clock that governs
/// it, so a reader can ask *which* clock, a test can advance it, and a restart
/// can restore the remaining budget rather than an instant.
///
/// Two independent bounds, never conflated:
///
/// - **The logical bound** — [`Deadline::elapsed`] against a [`Clock`]. This is
///   what a test drives and what survives a restart as a duration.
/// - **The wall watchdog** — [`Deadline::watchdog`], real elapsed time from the
///   deadline's construction. This is what catches work that stopped making
///   progress entirely, and it keeps running while logical time is frozen.
///
/// ```
/// # use std::time::Duration;
/// # use lgwks_bot::rt::clock::Clock;
/// # use lgwks_bot::rt::time::Deadline;
/// let clock = Clock::virtual_at(Duration::ZERO);
/// let deadline = Deadline::after(&clock, Duration::from_secs(30));
/// // A day of logical time exhausts a 30-second budget exactly.
/// assert!(clock.advance(Duration::from_secs(86_400)).is_ok());
/// assert!(deadline.is_exhausted());
/// assert_eq!(deadline.remaining(), Duration::ZERO);
/// // The watchdog is a separate fact: a paused clock does not pause it.
/// assert!(deadline.watchdog().elapsed() < Duration::from_secs(5));
/// ```
#[derive(Debug, Clone, Copy)]
pub struct Deadline<'a> {
    /// The clock that governs the logical bound.
    clock: &'a Clock,
    /// The budget, measured on that clock.
    budget: Duration,
    /// Real time this deadline has been alive, for the watchdog.
    ///
    /// The engine's `Instant`, named explicitly: this module re-exports the
    /// engine's `Instant` for `timeout_at`, and a bare `Instant` here would
    /// silently become the *engine's* clock — which is exactly the second
    /// timeline this type exists to keep separate.
    started: std::time::Instant,
}

impl<'a> Deadline<'a> {
    /// A deadline `budget` long on `clock`, measured from now.
    #[must_use]
    pub fn after(clock: &'a Clock, budget: Duration) -> Self {
        Self {
            clock,
            budget,
            started: std::time::Instant::now(),
        }
    }

    /// A deadline with no remaining budget, which is already exhausted.
    ///
    /// The explicit constructor rather than a `Default`, because "no deadline"
    /// and "a deadline of zero" are different facts and a `Default` would make
    /// them the same value.
    #[must_use]
    pub fn expired(clock: &'a Clock) -> Self {
        Self::after(clock, Duration::ZERO)
    }

    /// The clock this deadline is measured on.
    #[must_use]
    pub const fn clock(&self) -> &'a Clock {
        self.clock
    }

    /// The budget this deadline was given.
    #[must_use]
    pub const fn budget(&self) -> Duration {
        self.budget
    }

    /// Elapsed time on the governing clock.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.clock.now()
    }

    /// How much of the budget is left, saturating at zero.
    ///
    /// This is the quantity that is safe to persist across a restart: a
    /// remaining duration means the same thing on another host, where the
    /// [`Instant`] behind [`Deadline::after`] would not.
    #[must_use]
    pub fn remaining(&self) -> Duration {
        self.budget.saturating_sub(self.elapsed())
    }

    /// Whether the logical bound has been reached.
    #[must_use]
    pub fn is_exhausted(&self) -> bool {
        self.elapsed() >= self.budget
    }

    /// Real elapsed time since this deadline was created.
    ///
    /// The independent watchdog. It reads `std::time::Instant` and is therefore
    /// unaffected by anything a test does to the logical clock — which is the
    /// contract: pausing logical time never disables the wall-clock watchdog for
    /// an OS child, a blocking callback, a deadlock or a store hang.
    #[must_use]
    pub fn watchdog(&self) -> WallClock {
        WallClock::from_instant(self.started)
    }

    /// Whether the real-time watchdog has outlived `limit`.
    ///
    /// The predicate a caller uses when a thing is expected to stop making
    /// progress rather than to wait: a subprocess that stopped answering, a
    /// callback that will never return, a store whose `fsync` is stuck. Logical
    /// time cannot observe any of those.
    #[must_use]
    pub fn watchdog_exceeded(&self, limit: Duration) -> bool {
        self.watchdog().elapsed() >= limit
    }

    /// A point-in-time reading of this deadline's remaining budget, for
    /// persisting across a restart.
    #[must_use]
    pub fn snapshot(&self) -> ClockSnapshot {
        self.clock.snapshot()
    }
}
