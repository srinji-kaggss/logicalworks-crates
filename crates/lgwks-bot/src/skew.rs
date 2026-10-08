//! Cross-host time: monotonic deadlines, wall timestamps, and the skew bound.
//!
//! One host's clock is not another host's. A deadline a lease carries is a
//! reading on the **granting** clock; a timestamp an operator reads is a
//! wall-clock fact. Mixing the two — judging a lease against a foreign reading
//! as though the clocks agreed, or persisting a wall instant as though it were
//! a deadline — is how a weeks-long bot honours a credential its issuer
//! cancelled, or drops one that is still good.
//!
//! This module names the three pieces and no more:
//!
//! - [`MonoDeadline`](crate::skew::MonoDeadline): a deadline on a monotonic clock, a duration since that
//!   clock's origin. No epoch, no host, nothing a second host can misread as
//!   its own time.
//! - [`WallStamp`](crate::skew::WallStamp): a wall-clock timestamp for records an operator reads after
//!   a restart. Never a deadline: nothing here compares one against a clock.
//! - [`SkewBound`](crate::skew::SkewBound): how far two honest clocks may disagree, and the two judges
//!   that apply it — [`lease_holds`](crate::skew::lease_holds) for time-bound authority and
//!   [`epoch_holds`](crate::skew::epoch_holds) for generation fences.
//!
//! The judges are pure predicates over readings, so a simulation drives them
//! with two virtual clocks and asserts the same answers a deployment gets.
//!
//! # Seams (issue #278, row 2)
//!
//! Leases are minted in [`crate::gate::GrantSet`] and sealed into
//! [`crate::cap::Auth`], which judges them on a foreign clock through
//! [`Auth::check_remote`](crate::cap::Auth::check_remote) — the production
//! caller of [`lease_holds`](crate::skew::lease_holds). Owner epochs are minted and compared in
//! [`crate::broker::Broker`], which this module does not touch: a generation
//! is exact and needs no skew allowance, and [`epoch_holds`](crate::skew::epoch_holds) states that fact
//! so a caller stops adding one. What crosses a host is the warrant's epoch
//! plus its lease; the epoch half is fenced exactly, the lease half within
//! the bound.

use std::fmt;
use std::time::{Duration, SystemTime};

/// How far two honest clocks may disagree before a cross-host judgement
/// refuses.
///
/// A bound, not a measurement: nobody measured the drift, and a value derived
/// from one observation of it would be that observation dressed as a promise.
/// The operator declares how much disagreement the deployment tolerates, and
/// every cross-host time judgement is taken against it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SkewBound {
    /// The largest disagreement the deployment tolerates.
    max: Duration,
}

impl SkewBound {
    /// No allowance: two readings must agree exactly.
    ///
    /// The default for one host, where the granting clock and the judging
    /// clock are the same counter and any disagreement is a defect rather
    /// than drift.
    pub const ZERO: Self = Self {
        max: Duration::ZERO,
    };

    /// Declare the largest disagreement two honest clocks may carry.
    #[must_use]
    pub const fn new(max: Duration) -> Self {
        Self { max }
    }

    /// The declared allowance.
    #[must_use]
    pub const fn get(self) -> Duration {
        self.max
    }
}

impl fmt::Display for SkewBound {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "max clock skew {:?}", self.max)
    }
}

/// A deadline on a monotonic clock: a duration since that clock's origin.
///
/// The cross-host form of "until when". It carries no epoch and no host
/// identity, so a record written on one host and judged on another is a
/// duration both can place on their own timeline, never an instant that means
/// nothing off the host that wrote it (INV-BOT-30).
///
/// Compare it only against a reading of the clock it was taken on, or against
/// a foreign reading through [`lease_holds`] with the deployment's
/// [`SkewBound`]. A direct comparison against a foreign clock is the defect
/// this type exists to prevent, and it is why the comparison goes through the
/// judge rather than an operator on this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MonoDeadline {
    /// The reading on the issuing clock at which the authority lapses.
    at: Duration,
}

impl MonoDeadline {
    /// Name the reading on the issuing clock at which the authority lapses.
    #[must_use]
    pub const fn at(at: Duration) -> Self {
        Self { at }
    }

    /// The reading this deadline names.
    #[must_use]
    pub const fn get(self) -> Duration {
        self.at
    }

    /// Whether `now` — a reading of the **issuing** clock — has reached this
    /// deadline.
    ///
    /// Same-clock only. A foreign reading passes through [`lease_holds`],
    /// which is the only comparison that knows about skew.
    #[must_use]
    pub fn is_due_on(self, now: Duration) -> bool {
        now >= self.at
    }
}

impl fmt::Display for MonoDeadline {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "monotonic deadline at {:?}", self.at)
    }
}

/// A wall-clock timestamp for records an operator reads, never a deadline.
///
/// The cross-host form of "when did this happen". Kept apart from
/// [`MonoDeadline`] by type because the two answer different questions — one
/// judges authority, the other tells an operator what changed after a restart
/// — and a value that serves as both is a deadline computed from a clock that
/// can jump and a history ordered by one that cannot be compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WallStamp {
    /// The wall-clock instant the event was recorded at.
    at: SystemTime,
}

impl WallStamp {
    /// Stamp an event with the current wall-clock time.
    ///
    /// The one constructor that reads a clock, and it reads the wall clock:
    /// a stamp is evidence for an operator, not an input to a deadline, so
    /// virtual time must never produce one.
    #[must_use]
    pub fn now() -> Self {
        Self {
            at: SystemTime::now(),
        }
    }

    /// Stamp an event with a known wall-clock instant, for tests and replays.
    #[must_use]
    pub const fn at(at: SystemTime) -> Self {
        Self { at }
    }

    /// The recorded instant.
    #[must_use]
    pub const fn get(self) -> SystemTime {
        self.at
    }
}

impl fmt::Display for WallStamp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "wall timestamp {at:?}", at = self.at)
    }
}

/// Whether a lease ending at `expires_at` still holds when the **observing**
/// host's clock reads `observer_now`, allowing two honest clocks to disagree
/// by up to `max_skew`.
///
/// The observer accepts while its own reading is before the expiry plus the
/// bound: a clock that runs ahead must not kill a credential that is still
/// good, and the boundary itself refuses, since the comparison is strict. A
/// clock that runs behind accepts a lease its issuer already considers
/// lapsed — the judge reads the observer's clock, and a lagging observer
/// cannot see the expiry. That accept is the liveness bias the bound prices,
/// stated rather than hidden: an expiry lands at most `max_skew` late on an
/// ahead clock, and a behind clock is blind to it. Past the bound on the
/// observer's own reading the lease is refused with the typed outcome the
/// caller already matches on
/// ([`crate::error::BotError::CredentialExpired`]).
///
/// Saturating: an expiry near the representable ceiling plus a bound is the
/// ceiling, not a wrap into a refusal of everything.
#[must_use]
pub fn lease_holds(expires_at: Duration, observer_now: Duration, max_skew: Duration) -> bool {
    observer_now < expires_at.saturating_add(max_skew)
}

/// Whether a warrant minted at `warrant_epoch` still fences against
/// `current_epoch`.
///
/// Exact equality, with no skew allowance by construction: a generation is a
/// counter, not a clock reading, and two hosts that agree on the counter agree
/// on the fence whatever their clocks say. Adding a bound here would turn a
/// stale worker's warrant into a live one for the length of the bound, which
/// is the opposite of fencing. Time-bound authority takes its allowance from
/// [`lease_holds`]; generation-bound authority takes none.
#[must_use]
pub const fn epoch_holds(warrant_epoch: u64, current_epoch: u64) -> bool {
    warrant_epoch == current_epoch
}

/// Whether time-bound and generation-bound authority hold together: the
/// warrant's epoch is the current one ([`epoch_holds`], exact) and its lease
/// holds on the observer's clock ([`lease_holds`], within the bound).
///
/// The conjunction is the whole cross-host authority check in one place, so a
/// caller cannot fence the epoch and forget the lease, or bound the lease and
/// forget the fence.
#[must_use]
pub fn warrant_holds(
    warrant_epoch: u64,
    current_epoch: u64,
    expires_at: Duration,
    observer_now: Duration,
    max_skew: Duration,
) -> bool {
    epoch_holds(warrant_epoch, current_epoch) && lease_holds(expires_at, observer_now, max_skew)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The crate's test idiom: `Result` and `?`, real values in every assert.
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn a_lease_judged_on_its_own_clock_needs_no_bound() -> TestResult {
        assert!(
            lease_holds(
                Duration::from_secs(10),
                Duration::from_secs(9),
                Duration::ZERO
            ),
            "a lease judged before its expiry on its own clock holds"
        );
        assert!(
            !lease_holds(
                Duration::from_secs(10),
                Duration::from_secs(10),
                Duration::ZERO
            ),
            "a lease judged at its expiry on its own clock has lapsed"
        );
        Ok(())
    }

    #[test]
    fn an_epoch_fence_is_exact_whatever_the_clocks_say() -> TestResult {
        assert!(epoch_holds(3, 3), "the current generation fences exactly");
        assert!(
            !epoch_holds(2, 3),
            "a superseded generation never fences, at any skew"
        );
        Ok(())
    }
}
