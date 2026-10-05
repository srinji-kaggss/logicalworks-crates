//! Per-stage timing of one tick, behind the default-off `profile` feature.
//!
//! # Why this exists and what it is not
//!
//! `bench/` publishes a ratio between this crate's tick and a hand-rolled loop
//! doing provably identical work, and the ratio used to be read as "the
//! schedule is slow" without anyone knowing which half of it was. This module is
//! the instrument that answers that: six named stages, measured on the tick path
//! itself, never on a reimplementation of it.
//!
//! It is a **measurement instrument, not a capability a bot uses**. Nothing in
//! the crate's own execution path reads it, and the whole module compiles out
//! when the feature is off: every charge site in `ecs.rs` is behind the same
//! `cfg`, so a build without `profile` contains no timestamp read and no
//! per-stage arithmetic at all. That is the difference between an instrument and
//! a permanent tax, and it is why this is a feature rather than a flag.
//!
//! # The accounting model, stated because it decides what the numbers mean
//!
//! [`charge`](Charge::drop) takes **one** timestamp and attributes the interval
//! since the previous charge to the stage whose guard just dropped. Two
//! consequences follow, and both are wanted:
//!
//! - **One clock read per stage per tick**, not one per guard entry and one on
//!   exit. A tick charged on thirty-six `Instant::now()` calls would cost more
//!   in the instrument than the stage it measures.
//! - **A guard nested inside another measures only what its parent has not
//!   already claimed.** The schedule step wraps the whole `Schedule::run`, and
//!   the stages inside it charge first, so the schedule's own figure is the
//!   *residual*: the dispatch of the two systems plus the bookkeeping inside
//!   `observe_fold` that no other stage claims. The stages therefore partition
//!   the tick rather than overlapping, which is what makes their shares
//!   summable.
//!
//! # What the instrument costs, and how a reader should use the numbers
//!
//! The instrument is not free: seven clock reads on a tick. `bench/` measures
//! that cost in the same process as the profile — the same workload timed
//! through `tick()` and through `tick_profiled()` — and prints the difference
//! beside the table, so the reader can subtract it rather than guess. The
//! **shares** are the claim the profile supports; the absolutes carry the
//! instrument, and the two are quoted separately for that reason.

use std::cell::Cell;
use std::time::{Duration, Instant};

/// One stage of a tick.
///
/// The six are the tick's own phases, named as the tick path names them. They
/// are a partition: every instant of a tick lands in exactly one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TickStage {
    /// The observation phase: every source polled, in bounded waves, under the
    /// per-poll deadline. It carries each source's own comparison of the value
    /// it read against the value the substrate already holds, because that is
    /// where this substrate's per-source change detection happens — inside the
    /// poll, while the output is still a concrete value.
    Poll,
    /// The admitted-input identity of every value that moves: the content
    /// digest a dispatch binds, derived where the output type is still a type
    /// parameter. Charged over the batch that derives them.
    Fingerprint,
    /// The substrate's own change detection outside the poll: which chains the
    /// `Changed<Revision>` query reports as moved, and whether the newest
    /// observation is admitted over the payload a retained transition holds.
    Compare,
    /// The ECS schedule step: dispatching `observe_fold` and `fire_plan`, and
    /// the commit bookkeeping inside the first that no other stage claims.
    /// A **residual**, because the stages nested inside it charge first.
    Schedule,
    /// The condition walk: every `Evaluate::check` this tick reached, over the
    /// entries whose transition the ledger says are eligible.
    Decide,
    /// The effects this tick selected, in the order it selected them: the
    /// ledger writes, the durable record, the broker's warrant, and the action
    /// future.
    Act,
}

impl TickStage {
    /// Every stage, in the order a tick passes through them.
    pub const ALL: [Self; 6] = [
        Self::Poll,
        Self::Fingerprint,
        Self::Compare,
        Self::Schedule,
        Self::Decide,
        Self::Act,
    ];

    /// A stable short name, for a report keyed by string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Poll => "poll",
            Self::Fingerprint => "fingerprint",
            Self::Compare => "compare",
            Self::Schedule => "schedule",
            Self::Decide => "decide",
            Self::Act => "act",
        }
    }
}

impl core::fmt::Display for TickStage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What one or more ticks spent in each stage.
///
/// One value per stage and a tick count, never a per-tick figure the caller has
/// to divide: a caller that divides by zero, or by a count it read at a
/// different moment, produces a number this type exists to make impossible.
///
/// Every field is private behind an accessor, for the reason [`TickReport`]
/// keeps its own private: a reader that could edit the record of what a tick
/// measured could make an instrumented run disagree with a timed one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TickProfile {
    /// What the observation phase spent.
    poll: Duration,
    /// What the admitted-input identities spent.
    fingerprint: Duration,
    /// What change detection outside the poll spent.
    compare: Duration,
    /// What the schedule step itself spent, once the nested stages are removed.
    schedule: Duration,
    /// What the condition walk spent.
    decide: Duration,
    /// What the effects spent.
    act: Duration,
    /// How many ticks contributed to the other six.
    ticks: u64,
}

impl TickProfile {
    /// A profile that has measured nothing.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            poll: Duration::ZERO,
            fingerprint: Duration::ZERO,
            compare: Duration::ZERO,
            schedule: Duration::ZERO,
            decide: Duration::ZERO,
            act: Duration::ZERO,
            ticks: 0,
        }
    }

    /// How many ticks contributed to this profile.
    ///
    /// Zero is the answer for a profile nothing was measured into, and every
    /// per-tick accessor below is defined for it rather than dividing by it.
    #[must_use]
    pub const fn ticks(&self) -> u64 {
        self.ticks
    }

    /// The total time one stage spent across every measured tick.
    #[must_use]
    pub const fn stage(&self, stage: TickStage) -> Duration {
        match stage {
            TickStage::Poll => self.poll,
            TickStage::Fingerprint => self.fingerprint,
            TickStage::Compare => self.compare,
            TickStage::Schedule => self.schedule,
            TickStage::Decide => self.decide,
            TickStage::Act => self.act,
        }
    }

    /// The mean time one stage spent on one tick.
    ///
    /// Zero for a profile with no ticks, because the mean of nothing is not a
    /// number and dividing by the count would say it was.
    #[must_use]
    pub fn per_tick(&self, stage: TickStage) -> Duration {
        if self.ticks == 0 {
            return Duration::ZERO;
        }
        // Saturating, not wrapping: a count past `u32::MAX` takes the maximum
        // rather than a small divisor, which would report a stage as costing
        // more per tick than the profile can hold.
        let ticks = u32::try_from(self.ticks).unwrap_or(u32::MAX);
        self.stage(stage)
            .checked_div(ticks)
            .unwrap_or(Duration::ZERO)
    }

    /// Every stage's total, which is the whole of the measured ticks.
    #[must_use]
    pub const fn total(&self) -> Duration {
        Duration::new(0, 0)
            .saturating_add(self.poll)
            .saturating_add(self.fingerprint)
            .saturating_add(self.compare)
            .saturating_add(self.schedule)
            .saturating_add(self.decide)
            .saturating_add(self.act)
    }

    /// One stage's share of the measured ticks, in `0.0..=1.0`.
    ///
    /// Zero when nothing was measured. The shares over every stage sum to one by
    /// construction, which is the property that makes a decomposition readable:
    /// the stages partition the tick rather than overlapping.
    #[must_use]
    pub fn share(&self, stage: TickStage) -> f64 {
        let total = self.total().as_secs_f64();
        if total <= 0.0 {
            return 0.0;
        }
        self.stage(stage).as_secs_f64() / total
    }

    /// Add another profile's figures into this one.
    ///
    /// So a caller can measure several windows and report them as one, which is
    /// what the rig does when a scenario's warm-up and its measured rounds are
    /// both profiled.
    pub fn absorb(&mut self, other: Self) {
        self.poll = self.poll.saturating_add(other.poll);
        self.fingerprint = self.fingerprint.saturating_add(other.fingerprint);
        self.compare = self.compare.saturating_add(other.compare);
        self.schedule = self.schedule.saturating_add(other.schedule);
        self.decide = self.decide.saturating_add(other.decide);
        self.act = self.act.saturating_add(other.act);
        self.ticks = self.ticks.saturating_add(other.ticks);
    }
}

thread_local! {
    /// Whether the tick currently being run is being measured.
    ///
    /// Const-initialised so reading it costs a load and never a lazy-init
    /// check: the instrument's own cost is the seven clock reads this module
    /// documents, and nothing else.
    static LIVE: Cell<bool> = const { Cell::new(false) };
    /// When the interval currently open began.
    static LAST: Cell<Option<Instant>> = const { Cell::new(None) };
    static POLL: Cell<Duration> = const { Cell::new(Duration::ZERO) };
    static FINGERPRINT: Cell<Duration> = const { Cell::new(Duration::ZERO) };
    static COMPARE: Cell<Duration> = const { Cell::new(Duration::ZERO) };
    static SCHEDULE: Cell<Duration> = const { Cell::new(Duration::ZERO) };
    static DECIDE: Cell<Duration> = const { Cell::new(Duration::ZERO) };
    static ACT: Cell<Duration> = const { Cell::new(Duration::ZERO) };
    static TICKS: Cell<u64> = const { Cell::new(0) };
}

/// Attribute the interval since the previous charge to `stage`, and open the next.
fn charge(stage: TickStage) {
    let now = Instant::now();
    // No open interval: the first charge of a thread's first measured tick, or a
    // mark reached after `end` closed the window. Nothing to attribute, and that
    // is a zero-length interval rather than a stage that went unmeasured — the
    // window simply had not begun.
    let since = LAST
        .get()
        .map_or(Duration::ZERO, |last| now.duration_since(last));
    LAST.set(Some(now));
    match stage {
        TickStage::Poll => POLL.set(POLL.get().saturating_add(since)),
        TickStage::Fingerprint => FINGERPRINT.set(FINGERPRINT.get().saturating_add(since)),
        TickStage::Compare => COMPARE.set(COMPARE.get().saturating_add(since)),
        TickStage::Schedule => SCHEDULE.set(SCHEDULE.get().saturating_add(since)),
        TickStage::Decide => DECIDE.set(DECIDE.get().saturating_add(since)),
        TickStage::Act => ACT.set(ACT.get().saturating_add(since)),
    }
}

/// Attributes the time its scope took to one stage.
///
/// Construction is free and the charge happens on drop, so a scope that returns
/// early, breaks, or unwinds is still measured: the guard is the only place the
/// tick's stages are recorded, and a stage measured only on the happy path is a
/// stage that reports the happy path's cost.
///
/// Guards nest only in one direction — the schedule step around the stages
/// inside it — and a nested guard charges only what its parent has not already
/// claimed. That is what makes [`TickStage::Schedule`] a residual rather than a
/// second count of the same instants.
pub(crate) struct Charge(TickStage);

impl Charge {
    /// Start attributing the current scope to `stage`.
    pub(crate) const fn new(stage: TickStage) -> Self {
        Self(stage)
    }
}

impl Drop for Charge {
    /// Close the interval into `stage`.
    ///
    /// Silent when the tick is not being measured, which is the whole of the
    /// feature's promise: a build without `profile` has no guard at all, and a
    /// build with it always has a window open.
    fn drop(&mut self) {
        if LIVE.get() {
            charge(self.0);
        }
    }
}

/// Arm the instrument for the thread this is called on.
///
/// Zeroes every accumulator, so a profile covers exactly the ticks run after
/// this and not a window a previous run left behind.
pub(crate) fn begin() {
    LAST.set(Some(Instant::now()));
    POLL.set(Duration::ZERO);
    FINGERPRINT.set(Duration::ZERO);
    COMPARE.set(Duration::ZERO);
    SCHEDULE.set(Duration::ZERO);
    DECIDE.set(Duration::ZERO);
    ACT.set(Duration::ZERO);
    TICKS.set(0);
    LIVE.set(true);
}

/// Close the window and read back what it measured.
pub(crate) fn end() -> TickProfile {
    // The tail of the tick after the last charge — the report write-back and the
    // pending scan — belongs to no stage, and is deliberately not folded into
    // one: the stages are a partition of what they measured, and claiming a
    // residual as part of the schedule would make the shares sum to more than
    // the tick.
    LAST.set(None);
    TICKS.set(TICKS.get().saturating_add(1));
    LIVE.set(false);
    TickProfile {
        poll: POLL.get(),
        fingerprint: FINGERPRINT.get(),
        compare: COMPARE.get(),
        schedule: SCHEDULE.get(),
        decide: DECIDE.get(),
        act: ACT.get(),
        ticks: TICKS.get(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_with_no_ticks_divides_to_zero_rather_than_panicking() {
        let empty = TickProfile::new();
        assert_eq!(empty.ticks(), 0, "a fresh profile has measured nothing");
        for stage in TickStage::ALL {
            assert_eq!(
                empty.per_tick(stage),
                Duration::ZERO,
                "the mean of no ticks is not a number: {}",
                stage
            );
            assert_eq!(
                empty.share(stage),
                0.0,
                "an unmeasured stage holds no share of nothing: {}",
                stage
            );
        }
    }

    #[test]
    fn the_stages_partition_the_measured_ticks() {
        let profile = TickProfile {
            poll: Duration::from_nanos(10),
            fingerprint: Duration::from_nanos(20),
            compare: Duration::from_nanos(30),
            schedule: Duration::from_nanos(40),
            decide: Duration::from_nanos(50),
            act: Duration::from_nanos(50),
            ticks: 2,
        };
        assert_eq!(
            profile.total(),
            Duration::from_nanos(200),
            "the six stages are the whole of a tick, so they add up"
        );
        assert_eq!(
            profile.total(),
            TickStage::ALL
                .iter()
                .map(|stage| profile.stage(*stage))
                .sum::<Duration>(),
            "every stage is reachable through the enum a caller matches on"
        );
        let shares: f64 = TickStage::ALL
            .iter()
            .map(|stage| profile.share(*stage))
            .sum();
        assert!(
            (shares - 1.0).abs() < 1e-12,
            "the shares must sum to one or the breakdown double-counts: {shares}"
        );
        assert_eq!(
            profile.per_tick(TickStage::Poll),
            Duration::from_nanos(5),
            "20 ns over two ticks is 5 ns per tick"
        );
    }

    #[test]
    fn an_absorbed_profile_adds_both_the_figures_and_the_count() {
        let mut first = TickProfile {
            poll: Duration::from_nanos(10),
            ticks: 1,
            ..TickProfile::new()
        };
        first.absorb(TickProfile {
            poll: Duration::from_nanos(30),
            ticks: 3,
            ..TickProfile::new()
        });
        assert_eq!(
            first.ticks(),
            4,
            "the count is what a per-tick figure divides"
        );
        assert_eq!(
            first.per_tick(TickStage::Poll),
            Duration::from_nanos(10),
            "40 ns over four ticks is 10 ns per tick, not 40"
        );
    }

    #[test]
    fn every_stage_has_its_own_stable_name() {
        let names: Vec<&str> = TickStage::ALL.iter().map(|stage| stage.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "poll",
                "fingerprint",
                "compare",
                "schedule",
                "decide",
                "act",
            ],
            "the report is keyed on these spellings, so they are part of the surface"
        );
        for stage in TickStage::ALL {
            assert_eq!(
                stage.to_string(),
                stage.as_str(),
                "Display and the stable name are one spelling, not two"
            );
        }
    }
}
