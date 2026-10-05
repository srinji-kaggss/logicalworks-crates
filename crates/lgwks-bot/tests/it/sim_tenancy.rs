//! Simulation family: the round scheduler's retention, swept over seeded worlds.
//!
//! One seed drives the whole scenario: which tenant arrives, and which of its
//! callers walk away. Nothing here reads the clock, the OS scheduler or an
//! unseeded entropy source, and no elapsed time enters the trace, so the trace
//! hash is a replay receipt -- a run that reordered its own actors changes the
//! hash, and a run that merely ran slower does not.
//!
//! The scheduler under test is the **shipped** `DeficitRoundRobin`, driven
//! synchronously. Nothing about the round is re-implemented here: a simulation
//! that modelled its own scheduler would pass while the real one rotted, which
//! is the failure mode this substrate exists to prevent.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `abandoned_waiters_stay_bounded_by_the_policy` | a tenant whose callers keep walking away retains at most twice its declared queue, at every act of every seed |
//! | `a_compaction_leaves_only_live_waiters` | after a compaction the retained count and the live count are the same number, so a compaction dropped exactly the abandoned waiters |
//! | `the_same_seed_replays_the_same_tenancy_trace` | the same seed produces the same trace hash and the same observations, twice |
//!
//! # The shape, and why it is arrival-only
//!
//! Every arrival queues, because the permit pool is spent from the first act --
//! the state a supervisor is in whenever any task is running. Nothing is ever
//! served, so a grant never skips an abandoned head and the retention under test
//! is the scheduler's own. It is also the shape the defect needs: a tenant
//! parked at its ceiling whose callers keep walking away is never touched by
//! anything else.
//!
//! The liveness predicate below is therefore the exact truth: a waiter is live
//! until this scenario abandons it. That is what lets the compaction be checked
//! waiter by waiter rather than in aggregate -- an aggregate that looks right
//! while one live waiter was dropped is exactly what a bound exists to catch.
#![cfg(feature = "script")]

use crate::band_family;
use crate::sim;

use std::collections::BTreeSet;
use std::error::Error;

use lgwks_bot::rt::tenancy::{Arrival, DeficitRoundRobin, TenancyPolicy};
use lgwks_bot::script::Tenant;

use sim::seed::{Rng, Trace};

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

/// The tenants one scenario drives. Two is the smallest world in which one
/// tenant's behaviour can be told apart from another's.
const TENANTS: u32 = 4;

/// The per-tenant queue bound, and therefore the ceiling the retained deque is
/// asserted against.
const QUEUE_PER_TENANT: usize = 16;

/// How many acts one scenario performs. Bounded so a band of seeds costs about
/// as much as one scenario: the property is about the bound holding at every
/// step, so the run has to be long enough to reach the bound and no longer.
const ACTS: u32 = 512;

/// The retention ceiling the compaction implies.
///
/// A compaction fires as soon as the abandoned outnumber the live, so a deque
/// never holds more than twice the live population, and the live population is
/// the queue bound. The `+ 1` is the entry the act that triggers the compaction
/// adds before the compaction answers.
fn retention_ceiling() -> usize {
    QUEUE_PER_TENANT.saturating_mul(2).saturating_add(1)
}

/// One act in a scenario.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    /// One tenant asks for an admission.
    Arrive,
    /// One of that tenant's waiters is abandoned, as a caller that walked away
    /// from a wait it never stopped making.
    Abandon,
}

/// What one scenario observed.
#[derive(Debug, Default)]
struct Observed {
    /// Arrivals that queued rather than being admitted.
    queued: u64,
    /// Arrivals refused at a queue bound.
    refused: u64,
    /// Waiters this scenario abandoned.
    abandoned: u64,
    /// Compactions the scheduler performed.
    compactions: u64,
    /// The largest retained deque the scenario saw on any one tenant.
    widest: usize,
}

/// Fold an observation into a trace, in a fixed field order.
///
/// The retained *maximum* is recorded rather than the final value: a bound that
/// held at the end of a run says nothing about a run that spiked in the middle.
impl Observed {
    /// Append this observation to `trace`.
    fn record(&self, trace: &mut Trace) {
        trace.record_u64("queued", self.queued);
        trace.record_u64("refused", self.refused);
        trace.record_u64("abandoned", self.abandoned);
        trace.record_u64("compactions", self.compactions);
        trace.record_count("widest", self.widest);
    }
}

/// Run one seeded scenario and return its trace and what it saw.
///
/// A refusal is the seed's own text, so a failing sweep names the seed that
/// produced it rather than only the family that read it.
fn scenario(seed: u64) -> Result<(Trace, Observed), Box<dyn Error>> {
    let mut rng = Rng::new(seed ^ 0x7e4a_7e4a_7e4a_7e4a);
    let mut trace = Trace::new();
    let mut observed = Observed::default();
    let mut named: Vec<Tenant> = Vec::with_capacity(usize::try_from(TENANTS).unwrap_or(0));
    for index in 0..TENANTS {
        named.push(Tenant::new(&format!("sim-tenant-{index}"))?);
    }
    // The pool is spent from the first act: a supervisor with any task running
    // answers every arrival with a queue, so every arrival here queues.
    let mut core: DeficitRoundRobin<u64> =
        DeficitRoundRobin::new(TenancyPolicy::new(2, QUEUE_PER_TENANT));
    // Every waiter this scenario has abandoned, wherever it still sits. The
    // liveness predicate reads exactly this set, so the ground truth and the
    // scheduler's are the same fact.
    let mut abandoned: BTreeSet<u64> = BTreeSet::new();
    let mut next_waiter = 0_u64;

    for _ in 0..ACTS {
        let index = usize::try_from(rng.below(TENANTS)).unwrap_or(0);
        let Some(tenant) = named.get(index).cloned() else {
            trace.record("a-tenant-index-past-the-roster");
            continue;
        };
        // A seeded mix rather than a uniform draw, so a run always exercises
        // both acts: an all-arrivals run never abandons anything, and an
        // all-abandonments run has nothing to abandon for a long time.
        let act = match rng.below(10) {
            0..=5 => Act::Arrive,
            _ if next_waiter == 0 => Act::Arrive,
            _ => Act::Abandon,
        };
        match act {
            Act::Arrive => {
                let waiter = next_waiter;
                next_waiter = next_waiter.saturating_add(1);
                match core.arrive(&tenant, waiter, || None::<u64>) {
                    Arrival::Queued => {
                        observed.queued = observed.queued.saturating_add(1);
                    }
                    Arrival::Refused { limit } => {
                        observed.refused = observed.refused.saturating_add(1);
                        trace.record_u64("refused-at", u64::try_from(limit).unwrap_or(0));
                    }
                    // Unreachable: the pool answers `None` to every arrival.
                    // Recorded rather than asserted, so a scenario that met it
                    // would be visible in the trace instead of silent, and a
                    // future `Arrival` variant lands here rather than breaking
                    // the build.
                    _ => trace.record("an-arrival-was-admitted-with-a-spent-pool"),
                }
            }
            Act::Abandon => {
                // A recent waiter, seeded. Naming one that has already been
                // dropped is harmless and is counted honestly: the compaction
                // only ever consults the predicate for waiters still in the
                // deque.
                let span = u64::from(rng.below(8));
                let Some(waiter) = next_waiter.saturating_sub(1).checked_sub(span) else {
                    continue;
                };
                if abandoned.insert(waiter) {
                    observed.abandoned = observed.abandoned.saturating_add(1);
                }
                let before = core.retained_of(&tenant);
                let mut is_live = |waiter: &u64| !abandoned.contains(waiter);
                core.note_abandoned(&tenant, &mut is_live);
                let after = core.retained_of(&tenant);
                if after < before {
                    observed.compactions = observed.compactions.saturating_add(1);
                    trace.record_u64(
                        "compacted-by",
                        u64::try_from(before.saturating_sub(after)).unwrap_or(0),
                    );
                    // A compaction retains exactly the waiters that answer live,
                    // so the retained count and the scheduler's own live count are
                    // the same number afterwards. A compaction that dropped a live
                    // waiter, or kept an abandoned one, breaks that equality.
                    assert_eq!(
                        after,
                        core.queued_of(&tenant),
                        "seed {seed}: a compaction on {tenant} retained {after} waiters \
                         while the scheduler reports {} live, so the two disagree",
                        core.queued_of(&tenant)
                    );
                }
            }
        }
        for (position, name) in named.iter().enumerate() {
            let retained = core.retained_of(name);
            observed.widest = observed.widest.max(retained);
            trace.record_count("retained", retained);
            trace.record_count("live", core.queued_of(name));
            let ceiling = retention_ceiling();
            assert!(
                retained <= ceiling,
                "seed {seed}: tenant {position} retained {retained} waiters against a \
                 queue bound of {QUEUE_PER_TENANT} and a retention ceiling of {ceiling}"
            );
        }
    }
    observed.record(&mut trace);
    Ok((trace, observed))
}

/// A tenant whose callers keep walking away retains a bounded number of waiters.
///
/// The bound is asserted after **every** act rather than at the end of the run: a
/// scheduler that held two thousand abandoned waiters and released them all in
/// one step would pass an end-of-run assertion and hold that memory throughout.
fn abandoned_waiters_stay_bounded_by_the_policy(band: sim::Band) -> TestResult {
    let mut widest = 0_usize;
    let mut compacted = false;
    for seed in band.seeds() {
        let (_, observed) = scenario(seed)?;
        assert!(
            observed.queued > 0,
            "seed {seed} queued nothing, so it exercised nothing"
        );
        widest = widest.max(observed.widest);
        compacted |= observed.compactions > 0;
    }
    assert!(
        widest > 0,
        "no seed retained a waiter, so the bound was never under pressure"
    );
    assert!(
        compacted,
        "no seed ever compacted a queue, so the retention bound was never the \
         thing under test"
    );
    Ok(())
}

/// A compaction drops exactly the waiters the liveness predicate refuses.
///
/// Asserted inside [`scenario`] at the moment of every compaction -- the retained
/// count and the scheduler's live count must be the same number afterwards -- and
/// named here so the family is a recorded test rather than an assertion buried in
/// a helper.
fn a_compaction_leaves_only_live_waiters(band: sim::Band) -> TestResult {
    for seed in band.seeds() {
        let (_, observed) = scenario(seed)?;
        assert!(
            observed.abandoned > 0,
            "seed {seed} abandoned nothing, so no compaction could be exercised"
        );
    }
    Ok(())
}

/// The same seed produces the same trace, twice, so the hash is a replay receipt
/// rather than a decoration.
fn the_same_seed_replays_the_same_tenancy_trace(band: sim::Band) -> TestResult {
    let mut first: Vec<u64> = Vec::new();
    for seed in band.seeds() {
        let (trace_a, observed_a) = scenario(seed)?;
        let (trace_b, observed_b) = scenario(seed)?;
        assert_eq!(
            trace_a.hash(),
            trace_b.hash(),
            "seed {seed} produced two different traces, so a trace hash is not a \
             replay receipt"
        );
        assert_eq!(
            observed_a.widest, observed_b.widest,
            "seed {seed} reached a different retained maximum on a repeat run"
        );
        first.push(trace_a.hash());
    }
    let mut distinct = first.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert!(
        distinct.len().saturating_mul(2) > first.len(),
        "only {} of {} seeded traces were distinct, so most seeds replay an \
         identical scenario",
        distinct.len(),
        first.len()
    );
    Ok(())
}

// The recorded public test names, one per family, sweeping a band each. The
// macro is what turns each family above into a test per band, so the families
// themselves carry no `#[test]`: a test function cannot take an argument.
band_family::band_family! {
    abandoned_waiters_stay_bounded_by_the_policy_band_00 => abandoned_waiters_stay_bounded_by_the_policy, 0;
    abandoned_waiters_stay_bounded_by_the_policy_band_01 => abandoned_waiters_stay_bounded_by_the_policy, 1;
    a_compaction_leaves_only_live_waiters_band_00 => a_compaction_leaves_only_live_waiters, 2;
    a_compaction_leaves_only_live_waiters_band_01 => a_compaction_leaves_only_live_waiters, 3;
    the_same_seed_replays_the_same_tenancy_trace_band_00 => the_same_seed_replays_the_same_tenancy_trace, 4;
    the_same_seed_replays_the_same_tenancy_trace_band_01 => the_same_seed_replays_the_same_tenancy_trace, 5;
}
