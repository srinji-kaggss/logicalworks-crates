//! The scenario table, and why the wide rows are sized the way they are (#640).
//!
//! Split out of `main.rs` because a table of measured configurations is data
//! and the measurement code is not: adding a source count reads as one more row,
//! not as an edit inside the timing loop.
//!
//! # Why the wide rows are sized the way they are
//!
//! Every fired effect appends three journal events (intent, dispatch, outcome),
//! and `MAX_JOURNAL_EVENTS` (100,000) refuses the next append rather than
//! dropping evidence. Each round builds its own bot, so a round's budget is
//! about 33,000 fired effects. A wide row therefore holds the **change-tick
//! fraction** at one in ten — the fraction `wide-256x10` already measured — by
//! shortening the round and its warm-up together, rather than by measuring a
//! different workload at each source count. A row whose timed window was more
//! change-heavy than another's would report a per-tick cost made partly of the
//! mix, and the slope over the family would be a slope through two workloads.
//!
//! The warm-up is one change tick where the journal can afford it: 256 and 1,024
//! sources warm, and the two widest rows warm nothing. At 4,096 and 16,384
//! sources a round admits one fire plus its own cold first tick, and a warm-up
//! tick would be a third — past the journal. Those rows therefore carry the cold
//! first tick inside the window, and their window is **two periods** so that
//! every round starts on a value of the same parity and does the same work: a
//! ten-tick window at 16,384 sources started on alternating parities, so half
//! the rounds fired and half did not, and the paired ratio's interval came out
//! [97, 1958] — a measurement of the parity, not of the tick.

/// One measured configuration.
#[derive(Clone, Copy)]
pub(super) struct Scenario {
    pub(super) name: &'static str,
    /// Why this scenario is in the set. Printed with the result so a reader does
    /// not have to infer the intent from the numbers.
    pub(super) intent: &'static str,
    pub(super) sources: usize,
    pub(super) period: u64,
    pub(super) entries: usize,
    /// Timed ticks in one round. Bounded by the bot's own journal ceiling: every
    /// fired effect writes three events, and `MAX_JOURNAL_EVENTS` refuses the
    /// next one rather than dropping evidence, so a round longer than this would
    /// be a rig that stops measuring rather than one that measures more.
    pub(super) ticks_per_round: u64,
    /// Untimed ticks on each round's own bot, before the timer starts. The first
    /// ticks of a fresh bot grow its buffers and prime its queries, and charging
    /// those to a round would report the warm-up as the tick's cost.
    pub(super) warmup_ticks: u64,
    pub(super) rounds: usize,
}

/// Every scenario the rig measures, in report order.
pub(super) const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "poll-only-64x100",
        intent: "cost decomposition floor: poll 64 sources and detect change, with no entries to walk",
        sources: 64,
        period: 100,
        entries: 0,
        ticks_per_round: 2_000,
        warmup_ticks: 200,
        rounds: 32,
    },
    Scenario {
        name: "steady-64x100",
        intent: "change detection is the bot's stated advantage: 64 sources, one change per 100 ticks",
        sources: 64,
        period: 100,
        entries: 1,
        ticks_per_round: 2_000,
        warmup_ticks: 200,
        rounds: 32,
    },
    Scenario {
        name: "churn-64x1",
        intent: "worst case for change detection: every source moves every tick",
        sources: 64,
        period: 1,
        entries: 1,
        ticks_per_round: 200,
        warmup_ticks: 50,
        rounds: 32,
    },
    Scenario {
        name: "fanout-1x64",
        intent: "chain-walk cost: one source, sixty-four (condition, action) entries",
        sources: 1,
        period: 1,
        entries: 64,
        ticks_per_round: 200,
        warmup_ticks: 50,
        rounds: 32,
    },
    Scenario {
        name: "wide-256x10",
        intent: "does per-source cost scale linearly with source count",
        sources: 256,
        period: 10,
        entries: 1,
        ticks_per_round: 250,
        warmup_ticks: 50,
        rounds: 32,
    },
    Scenario {
        name: "wide-1024x10",
        intent: "the same one-change-in-ten workload at four times the chain count",
        sources: 1_024,
        period: 10,
        entries: 1,
        ticks_per_round: 200,
        warmup_ticks: 20,
        rounds: 32,
    },
    Scenario {
        name: "wide-4096x10",
        intent: "the same workload at sixteen times wide-256, sized so one round fits the journal",
        sources: 4_096,
        period: 10,
        entries: 1,
        ticks_per_round: 20,
        warmup_ticks: 0,
        rounds: 64,
    },
    Scenario {
        name: "wide-16384x10",
        intent: "the same workload at sixty-four times wide-256, the widest round the journal admits",
        sources: 16_384,
        period: 10,
        entries: 1,
        ticks_per_round: 20,
        warmup_ticks: 0,
        rounds: 64,
    },
];

/// The scenario `name` names, or `None` where no row carries it.
pub(super) fn named(name: &str) -> Option<&'static Scenario> {
    SCENARIOS.iter().find(|scenario| scenario.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wide family varies the source count and nothing else, so the slope
    /// fitted over it is about the count and not about a mix of workloads.
    #[test]
    fn every_wide_row_changes_once_in_ten_ticks() {
        for scenario in SCENARIOS
            .iter()
            .filter(|scenario| scenario.name.starts_with("wide-"))
        {
            assert_eq!(
                scenario.period, 10,
                "{} carries a different period",
                scenario.name
            );
            assert_eq!(
                scenario.entries, 1,
                "{} carries a different entry count",
                scenario.name
            );
            assert_eq!(
                scenario.ticks_per_round % scenario.period,
                0,
                "{} has a timed window that is not a whole number of periods",
                scenario.name
            );
            assert_eq!(
                scenario.warmup_ticks % scenario.period,
                0,
                "{} warms for a fraction of a period",
                scenario.name
            );
        }
    }

    /// No wide round can exceed the journal. Every fired effect writes three
    /// events; one effect fires per source per **even** observed value, so at
    /// most half of the changes fire, plus the round's first tick. The bound is
    /// rounded up, because the round's first tick may sit anywhere inside a
    /// period: it is the worst window, not the average one.
    #[test]
    fn every_wide_round_fits_the_journal_ceiling() {
        for scenario in SCENARIOS
            .iter()
            .filter(|scenario| scenario.name.starts_with("wide-"))
        {
            let ticks = scenario
                .ticks_per_round
                .saturating_add(scenario.warmup_ticks);
            let changes = ticks.div_ceil(scenario.period);
            let fires = changes.div_ceil(2).saturating_add(1);
            let fired = fires.saturating_mul(scenario.sources as u64);
            let events = fired.saturating_mul(3);
            assert!(
                events < 100_000,
                "{} could fire {fired} effects, {events} journal events against a ceiling of 100,000",
                scenario.name
            );
        }
    }

    /// A row that warms nothing starts its window at a fixed offset from the
    /// previous round, so its window must be an even number of periods: an odd
    /// one alternates the starting value's parity and makes half the rounds fire.
    #[test]
    fn an_unwarmed_wide_round_starts_on_the_same_parity_every_time() {
        for scenario in SCENARIOS
            .iter()
            .filter(|scenario| scenario.name.starts_with("wide-") && scenario.warmup_ticks == 0)
        {
            let periods = scenario.ticks_per_round / scenario.period;
            assert_eq!(
                periods % 2,
                0,
                "{} runs {periods} periods per round, so its rounds alternate parity",
                scenario.name
            );
        }
    }

    /// A name resolves to its own row, and an unknown one resolves to nothing so
    /// the caller can refuse it instead of running a different scenario.
    #[test]
    fn a_name_resolves_only_to_the_row_that_carries_it() {
        assert_eq!(
            named("wide-4096x10").map(|scenario| scenario.sources),
            Some(4_096),
            "the 4096 row is the only row named wide-4096x10"
        );
        assert!(
            named("wide-8192x10").is_none(),
            "an absent name resolves to nothing"
        );
    }
}
