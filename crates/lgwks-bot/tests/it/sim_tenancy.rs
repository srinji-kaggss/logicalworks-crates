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
//! | `backlogged_tenants_take_shares_in_proportion_to_their_weights` | at 2, 100, 1,000 and 5,000 continuously backlogged tenants, every tenant's share stays within one round of its weight's share of the grants, no backlogged tenant waits more than one round, and each tenant's share is recorded |
//! | `a_flood_round_completes_within_its_bound` | the live supervisor's flood round structure -- a tenant at its ceiling, a controlled completion, and a second tenant's submission -- completes inside a declared bound, on the shipped `Supervisor` |
//! | `the_supervisor_waiting_bound_is_global` | five thousand tenants share one waiting bound, the retained total never passes it, and the arrival that would pass it is refused by the arm that names the supervisor rather than a tenant |
//! | `the_waiting_bound_is_declared_and_clamped` | the default is the ceiling and a larger declaration is clamped to it |
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
use std::num::NonZeroU32;

use lgwks_bot::rt::supervise::{SpawnRefused, Supervisor};
use lgwks_bot::rt::tenancy::{
    Arrival, DeficitRoundRobin, GrantOutcome, MAX_TOTAL_QUEUE, TenancyPolicy,
};
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

/// The roster of one scenario, as the index arithmetic needs it.
///
/// A tenant count this module declares is a `u32` literal and a `usize` index on
/// every platform this crate builds for, so the narrowing is total. Written as
/// one named function because a narrowing repeated at thirteen call sites is a
/// narrowing thirteen call sites can get wrong differently.
fn roster(tenants: u32) -> Result<usize, Box<dyn Error>> {
    match usize::try_from(tenants) {
        Ok(as_index) => Ok(as_index),
        Err(refusal) => Err(format!("a roster of {tenants} is not addressable: {refusal}").into()),
    }
}

/// A count wide enough for a trace record.
///
/// The ceiling rather than a different number, because a trace that recorded a
/// silent zero for a count nobody could read would be a receipt for nothing.
fn wide(count: usize) -> u64 {
    match u64::try_from(count) {
        Ok(as_count) => as_count,
        Err(_) => u64::MAX,
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
    let roster = roster(TENANTS)?;
    let mut named: Vec<Tenant> = Vec::with_capacity(roster);
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
        let index = match usize::try_from(rng.below(TENANTS)) {
            Ok(as_index) => as_index,
            Err(refusal) => {
                return Err(format!("a drawn tenant index is not addressable: {refusal}").into());
            }
        };
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
                        trace.record_u64("refused-at", wide(limit));
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
                    trace.record_u64("compacted-by", wide(before.saturating_sub(after)));
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

/// What one waiting-bound sweep observed.
#[derive(Debug, Default)]
struct Sweep {
    /// Arrivals that queued rather than being admitted.
    queued: u64,
    /// Arrivals refused because their own tenant's queue was full.
    per_tenant_refused: u64,
    /// Arrivals refused because the supervisor's own bound was reached.
    supervisor_refused: u64,
    /// Waiters abandoned before being served.
    abandoned: u64,
    /// The largest retained total the sweep saw.
    widest: usize,
}

impl Sweep {
    /// Append this sweep to `trace`, in a fixed field order.
    fn record(&self, trace: &mut Trace) {
        trace.record_u64("bound-queued", self.queued);
        trace.record_u64("bound-per-tenant-refused", self.per_tenant_refused);
        trace.record_u64("bound-supervisor-refused", self.supervisor_refused);
        trace.record_u64("bound-abandoned", self.abandoned);
        trace.record_count("bound-widest", self.widest);
    }
}

/// Run one seeded sweep at `tenants` tenants against a waiting bound of
/// `queue_total`, and report what it saw.
///
/// One world shape rather than a second scenario type: the questions are the same
/// at every tenant count, and a family that only ran at the tier it was written
/// for would leave the other tiers untested.
fn sweep_bound(seed: u64, tenants: u32, queue_total: usize) -> Result<Sweep, Box<dyn Error>> {
    let mut rng = Rng::new(seed ^ 0x6a0a_6a0a_6a0a_6a0a);
    let mut trace = Trace::new();
    let mut observed = Sweep::default();
    let roster = roster(tenants)?;
    let mut named: Vec<Tenant> = Vec::with_capacity(roster);
    for index in 0..tenants {
        named.push(Tenant::new(&format!("bound-tenant-{index}"))?);
    }
    let mut core: DeficitRoundRobin<u64> = DeficitRoundRobin::new(
        TenancyPolicy::new(2, QUEUE_PER_TENANT).with_queue_total(queue_total),
    );
    let mut abandoned: BTreeSet<u64> = BTreeSet::new();
    let mut next_waiter = 0_u64;
    for _ in 0..ACTS {
        let index = match usize::try_from(rng.below(tenants.max(1))) {
            Ok(as_index) => as_index,
            Err(refusal) => {
                return Err(format!("a drawn tenant index is not addressable: {refusal}").into());
            }
        };
        let Some(tenant) = named.get(index).cloned() else {
            trace.record("a-tenant-index-past-the-roster");
            continue;
        };
        match rng.below(10) {
            0..=6 => {
                let waiter = next_waiter;
                next_waiter = next_waiter.saturating_add(1);
                match core.arrive(&tenant, waiter, || None::<u64>) {
                    Arrival::Queued => observed.queued = observed.queued.saturating_add(1),
                    Arrival::Refused { .. } => {
                        observed.per_tenant_refused = observed.per_tenant_refused.saturating_add(1);
                    }
                    Arrival::SupervisorFull { limit } => {
                        observed.supervisor_refused = observed.supervisor_refused.saturating_add(1);
                        trace.record_u64("supervisor-refused-at", wide(limit));
                    }
                    _ => trace.record("an-arrival-was-admitted-with-a-spent-pool"),
                }
            }
            _ => {
                let span = u64::from(rng.below(8));
                let Some(waiter) = next_waiter.saturating_sub(1).checked_sub(span) else {
                    continue;
                };
                if abandoned.insert(waiter) {
                    observed.abandoned = observed.abandoned.saturating_add(1);
                }
                let mut is_live = |waiter: &u64| !abandoned.contains(waiter);
                core.note_abandoned(&tenant, &mut is_live);
            }
        }
        let retained = core.retained_total();
        observed.widest = observed.widest.max(retained);
        trace.record_count("retained", retained);
        trace.record_count("tenants-with-queues", core.tenants_with_queues());
        assert!(
            retained <= queue_total,
            "seed {seed}: {tenants} tenants retained {retained} waiters against a \
             supervisor bound of {queue_total}"
        );
    }
    observed.record(&mut trace);
    Ok(observed)
}

/// The waiting bound is the supervisor's, not one tenant's: many tenants share
/// one bound, the retained total never passes it, and the arrival that would pass
/// it is refused by the arm that names the supervisor rather than a tenant.
///
/// Run at the estate's declared tenant-provision tier as well as below it,
/// because the tier is the point: a bound that only holds at four tenants is not
/// a bound on a fleet.
fn the_supervisor_waiting_bound_is_global(band: sim::Band) -> TestResult {
    /// The tiers the sweep runs at. 5,000 is the estate's declared
    /// tenant-provision tier.
    const TIERS: [u32; 2] = [4, 5_000];
    /// The bound each tier runs at. Small enough that a tier's whole sweep stays
    /// under it, so the refusal is reached inside the run rather than at a total
    /// the run never reaches.
    const BOUNDS: [usize; 2] = [64, 256];

    for (tier_index, tenants) in TIERS.iter().copied().enumerate() {
        let Some(bound) = BOUNDS.get(tier_index).copied() else {
            return Err("a tier had no bound beside it".into());
        };
        let mut widest = 0_usize;
        let mut refused = 0_u64;
        for seed in band.seeds() {
            let observed = sweep_bound(seed, tenants, bound)?;
            widest = widest.max(observed.widest);
            refused = refused.saturating_add(observed.supervisor_refused);
        }
        assert!(
            widest <= bound,
            "{tenants} tenants retained {widest} waiters against a bound of {bound}"
        );
        assert!(
            refused > 0,
            "no arrival was refused by the supervisor's own bound at {tenants} tenants, \
             so the bound was never reached and never refused anything"
        );
    }
    Ok(())
}

/// The declared default is the ceiling, and a larger declaration is clamped to
/// it, so no policy can author an unbounded supervisor.
#[test]
fn the_waiting_bound_is_declared_and_clamped() -> TestResult {
    let default = TenancyPolicy::new(1, 1);
    assert_eq!(
        default.queue_total(),
        MAX_TOTAL_QUEUE,
        "a policy that says nothing about the supervisor's queue gets the ceiling"
    );
    let clamped = TenancyPolicy::new(1, 1).with_queue_total(MAX_TOTAL_QUEUE.saturating_add(1));
    assert_eq!(
        clamped.queue_total(),
        MAX_TOTAL_QUEUE,
        "a declaration past the ceiling is clamped to it"
    );
    Ok(())
}

/// The tenant tiers the fairness sweep runs at.
///
/// Two is the smallest world in which a share can differ from another's, 100 and
/// 1,000 are the tiers a real fleet passes through, and 5,000 is the estate's
/// declared tenant-provision tier.
const FAIRNESS_TIERS: [u32; 4] = [2, 100, 1_000, 5_000];

/// The per-tenant ceiling the fairness sweep declares.
const FAIRNESS_CEILING: usize = 64;

/// A whole number of rounds, at least `minimum` of them.
///
/// A share is only readable at a round boundary: inside a round the algorithm is
/// *supposed* to have handed a weight-2 tenant two permits in a row, so a
/// mid-round reading is the algorithm working rather than failing. Rounding the
/// sweep up to a whole number of rounds is therefore what makes the share a
/// meaningful measurement instead of a snapshot of a turn in progress.
fn grants_for(tenants: u32, total_weight: u64, minimum: u64) -> u64 {
    let wanted = u64::from(tenants).saturating_mul(4);
    let rounds = wanted.div_ceil(total_weight).max(minimum);
    rounds.saturating_mul(total_weight)
}

/// One weighted round's outcome.
#[derive(Debug)]
struct Fairness {
    /// The weight every tenant drew.
    weights: Vec<u64>,
    /// The grants every tenant received, in tenant order.
    grants: Vec<u64>,
    /// The sum of every tenant's weight, which is one round of grants.
    total_weight: u64,
    /// The grants handed out, a whole number of rounds.
    handed_out: u64,
    /// The largest per-tenant deviation from its ideal share, in shares of one
    /// grant: the tightest the algorithm ever came to a proportional split.
    worst_deviation: u64,
    /// The longest a backlogged tenant waited between grants, measured in grants
    /// handed out.
    longest_wait: u64,
}

impl Fairness {
    /// Append this round to `trace`, in tenant order.
    fn record(&self, trace: &mut Trace, tenants: u32) {
        trace.record_u64("fair-tenants", u64::from(tenants));
        trace.record_u64("fair-total-weight", self.total_weight);
        trace.record_u64("fair-handed-out", self.handed_out);
        trace.record_u64("fair-worst-deviation", self.worst_deviation);
        trace.record_u64("fair-longest-wait", self.longest_wait);
        for (weight, grants) in self.weights.iter().zip(&self.grants) {
            trace.record_u64("fair-weight", *weight);
            trace.record_u64("fair-grants", *grants);
        }
    }
}

/// Waiters parked per tenant.
///
/// Deep enough that a tier's whole sweep cannot drain any of them -- a drained
/// tenant stops being a claimant for a share -- and shallow enough that the whole
/// roster's backlog fits under the supervisor's own waiting bound. The second
/// half is not a shortcut: the sweep has to live inside that bound rather than be
/// exempt from it, and at the largest tier a 24-deep roster reaches it.
fn depth_for(tenants: u32) -> Result<usize, Box<dyn Error>> {
    let roster = roster(tenants)?.max(1);
    // Floor, not a rounding up: the depth has to *fit* the supervisor's bound,
    // and a ceiling here would put the roster's own backlog past the very bound
    // it is living inside.
    match MAX_TOTAL_QUEUE.checked_div(roster) {
        Some(room) => Ok(room.min(24)),
        None => Err(format!(
            "a roster of {roster} has no place under a supervisor bound of {MAX_TOTAL_QUEUE}"
        )
        .into()),
    }
}

/// One weighted round at `tenants` tenants, with the fairness invariants checked
/// at every grant.
///
/// Every tenant is continuously backlogged -- each holds a deep queue of live
/// waiters from the first act -- because a share is only defined for a tenant
/// that had work waiting when every permit freed. A tenant whose queue empties
/// has nothing to be denied, and a tenant parked at its ceiling is not on the
/// ring at all, so the admission is released as it is granted and the ceiling is
/// above what the sweep can reach.
///
/// Two invariants, both O(1) and both checked after **every** grant:
///
/// 1. **No starvation.** A backlogged tenant is served within one round -- the
///    total weight of the world's weights -- of the grants handed out since it
///    was last served. That is the bound deficit round robin is defined by: a
///    complete round serves every backlogged tenant at least once.
/// 2. **Bounded deviation, at every round boundary.** A tenant's cumulative
///    grants stay within one round of its weight's ideal share. Checked for
///    every tenant at each boundary rather than for one tenant at every grant,
///    because inside a round the deviation is *supposed* to reach a weight's
///    worth of grants: that is what a weight means. Reading it at a boundary is
///    what makes it a fairness measurement.
fn fair_round(tenants: u32, seed: u64) -> Result<Fairness, Box<dyn Error>> {
    let mut rng = Rng::new(seed ^ 0xfa11_0fa1_1fa1_1fa1);
    let roster = roster(tenants)?;
    let mut weights: Vec<u64> = Vec::with_capacity(roster);
    let mut named: Vec<Tenant> = Vec::with_capacity(roster);
    let depth = depth_for(tenants)?;
    let backlog = roster.saturating_mul(depth).saturating_add(1);
    let mut policy = TenancyPolicy::new(FAIRNESS_CEILING, depth).with_queue_total(backlog);
    for index in 0..tenants {
        let weight = u64::from(rng.between(1, 4));
        let Ok(declared) = u32::try_from(weight) else {
            return Err("a weight drawn between one and four is not a u32".into());
        };
        let Some(nonzero) = NonZeroU32::new(declared) else {
            return Err("a weight drawn between one and four is never zero".into());
        };
        let tenant = Tenant::new(&format!("fair-{index}"))?;
        policy = policy.with_weight(&tenant, nonzero)?;
        named.push(tenant);
        weights.push(weight);
    }
    let total_weight = weights.iter().copied().sum();
    let mut core: DeficitRoundRobin<u64> = DeficitRoundRobin::new(policy);
    // Backlog every tenant deeply enough that a tier's whole sweep cannot drain
    // any of them.
    let mut next_waiter = 0_u64;
    for tenant in &named {
        for _ in 0..depth {
            let arrival = core.arrive(tenant, next_waiter, || None::<u64>);
            match arrival {
                Arrival::Queued => {}
                Arrival::Refused { limit } => {
                    return Err(format!(
                        "at tier {tenants} a backlogged tenant hit its own queue bound \
                         of {limit} on its {next_waiter}th arrival"
                    )
                    .into());
                }
                Arrival::Immediate(_) => {
                    return Err("a backlogged tenant was admitted against a spent pool".into());
                }
                Arrival::SupervisorFull { limit } => {
                    return Err(format!(
                        "at tier {tenants} a backlogged tenant hit the supervisor's own \
                         bound of {limit} on its {next_waiter}th arrival"
                    )
                    .into());
                }
                _ => return Err("an unknown arrival outcome".into()),
            }
            next_waiter = next_waiter.saturating_add(1);
        }
    }

    let mut grants: Vec<u64> = vec![0; named.len()];
    // The last grant count at which each tenant was served, so a wait is a
    // subtraction rather than a per-tenant counter advanced on every grant.
    let mut last_served: Vec<u64> = vec![0; named.len()];
    let mut handed_out = 0_u64;
    let mut worst_deviation = 0_u64;
    let mut longest_wait = 0_u64;
    let target = grants_for(tenants, total_weight, 2);
    while handed_out < target {
        let mut is_live = |_waiter: &u64| true;
        let outcome = core.grant(handed_out, &mut is_live);
        let GrantOutcome::Granted(grant) = outcome else {
            return Err(format!(
                "a continuously backlogged world of {tenants} tenants ran out of waiters \
                 after {handed_out} grants"
            )
            .into());
        };
        let Some(index) = named
            .iter()
            .position(|candidate| *candidate == grant.tenant)
        else {
            return Err("the round granted to a tenant this sweep never built".into());
        };
        // The admission the grant took is released immediately: the question is
        // the order, and a tenant held at its ceiling would drop off the ring and
        // turn a fairness question into a queue-length one.
        core.note_release(&grant.tenant);
        let Some(previous) = last_served.get(index).copied() else {
            return Err("the grant named a tenant this sweep never sized a tally for".into());
        };
        let waited = handed_out.saturating_sub(previous);
        longest_wait = longest_wait.max(waited);
        assert!(
            waited <= total_weight,
            "at tier {tenants} a backlogged tenant waited {waited} grants for one, \
             against a round of {total_weight}"
        );
        handed_out = handed_out.saturating_add(1);
        if let Some(slot) = grants.get_mut(index) {
            *slot = slot.saturating_add(1);
        }
        if let Some(slot) = last_served.get_mut(index) {
            *slot = handed_out;
        }
        // Invariant 2, at every round boundary and over every tenant. The last
        // boundary is the sweep's end, so the final share is read at one too.
        if handed_out.is_multiple_of(total_weight) || handed_out == target {
            for (position, count) in grants.iter().copied().enumerate() {
                let Some(weight) = weights.get(position).copied() else {
                    return Err(
                        "the round named a tenant this sweep never drew a weight for".into(),
                    );
                };
                let above = count
                    .saturating_mul(total_weight)
                    .saturating_sub(weight.saturating_mul(handed_out));
                let below = weight
                    .saturating_mul(handed_out)
                    .saturating_sub(count.saturating_mul(total_weight));
                let deviation = above.max(below);
                worst_deviation = worst_deviation.max(deviation);
                assert!(
                    deviation <= total_weight,
                    "at tier {tenants}, after {handed_out} grants, tenant {position} \
                     took {count} at weight {weight} of {total_weight}, which is \
                     {deviation} shares of one grant away from its ideal -- more than \
                     one round"
                );
            }
        }
    }
    assert!(
        grants.iter().all(|count| *count > 0),
        "at tier {tenants} a continuously backlogged tenant took no grant across \
         {target} grants"
    );
    Ok(Fairness {
        weights,
        grants,
        total_weight,
        handed_out,
        worst_deviation,
        longest_wait,
    })
}

/// Every continuously backlogged tenant's share stays inside one round of its
/// weight's share, at every tier, and each tenant's share is recorded.
///
/// The tiers are two, one hundred, one thousand and five thousand. Two is the
/// smallest world in which a share can differ from another's; five thousand is
/// the estate's declared tenant-provision tier, and a bound that only held at two
/// would not be a bound on a fleet.
fn backlogged_tenants_take_shares_in_proportion_to_their_weights(band: sim::Band) -> TestResult {
    let mut trace = Trace::new();
    for tenants in FAIRNESS_TIERS {
        let mut worst_deviation = 0_u64;
        let mut longest = 0_u64;
        let mut served_all = true;
        let mut rounds = 0_u64;
        let mut world_weight = 0_u64;
        for seed in band.seeds() {
            let round = fair_round(tenants, seed)?;
            worst_deviation = worst_deviation.max(round.worst_deviation);
            longest = longest.max(round.longest_wait);
            served_all &= round.grants.iter().all(|grants| *grants > 0);
            rounds = round.handed_out;
            world_weight = round.total_weight;
            if seed == band.first {
                round.record(&mut trace, tenants);
            }
        }
        assert!(
            served_all,
            "at tier {tenants} a continuously backlogged tenant took no grant at all"
        );
        assert!(
            longest > 0,
            "at tier {tenants} no tenant ever waited for a grant, so the starvation \
             invariant was never under pressure"
        );
        // No cross-seed bound on `longest` here: every seed draws its own
        // weights, so each has its own round length, and the longest gap across a
        // band is not measured against any one of them. The bound is enforced
        // where it means something -- inside `fair_round`, against the round
        // length of the world that produced the gap -- and the widest round this
        // band drew is recorded beside the longest gap so a reader has both.
        trace.record_u64("fair-tier-grants", rounds);
        trace.record_u64("fair-tier-widest-round", world_weight);
        trace.record_u64("fair-tier-worst-deviation", worst_deviation);
        trace.record_u64("fair-tier-longest-wait", longest);
    }
    Ok(())
}

/// A weighted round replays: the same seed produces the same shares at every
/// tier, so a fairness number above is a measurement of the algorithm rather than
/// of the draw that produced it.
fn a_weighted_round_replays(band: sim::Band) -> TestResult {
    for tenants in FAIRNESS_TIERS {
        for seed in band.seeds() {
            let first = fair_round(tenants, seed)?;
            let second = fair_round(tenants, seed)?;
            assert_eq!(
                first.grants, second.grants,
                "seed {seed} at tier {tenants} produced two different share vectors"
            );
            assert_eq!(
                (first.handed_out, first.total_weight),
                (second.handed_out, second.total_weight),
                "seed {seed} at tier {tenants} produced two different world shapes"
            );
        }
    }
    Ok(())
}

/// The pools and bounds the flood-round simulation runs at.
///
/// Four permits with a per-tenant ceiling of two is the smallest world in which
/// one tenant can be at its ceiling while another still has room, which is the
/// whole shape of the question. Small on purpose: the simulation exists to reach
/// a wedged state in milliseconds rather than in minutes, and a bound that takes
/// minutes to trip is a bound nobody will keep.
const FLOOD_POOL: usize = 4;
/// The per-tenant ceiling both arms run under.
const FLOOD_CEILING: usize = 2;
/// The per-tenant queue bound of the arm that refuses rather than parks.
const FLOOD_REFUSING_QUEUE: usize = 0;
/// The per-tenant queue bound of the arm that parks.
const FLOOD_PARKING_QUEUE: usize = 1;
/// How long one submission may wait before the run is a failure.
///
/// A bound, not a budget: the work under test completes in microseconds, so this
/// is how long a *failure* to complete is allowed to look before it is named. A
/// submission that would never resolve is a wedged admission, not a slow one.
const FLOOD_BOUND: std::time::Duration = std::time::Duration::from_millis(250);
/// How many rounds one seed drives.
const FLOOD_ROUNDS: u32 = 16;
/// How many times a body hands the executor back before it returns.
///
/// The bodies complete on their own, and that is the load-bearing property of
/// this simulation: **the work that returns a permit must not be released by the
/// same caller that is waiting for it.** A caller that owns both is a caller that
/// parks forever, because parked it opens nothing. See
/// [`flood_round_structure`].
const BODY_YIELDS: u32 = 4;

/// One arm of the sweep: how the tenants' queues are bounded, and so whether a
/// submission parks or is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arm {
    /// The per-tenant queue is bounded but non-empty, so a contended arrival
    /// **parks** and waits for a permit some other admitted task must return.
    Parks,
    /// The per-tenant queue is zero, so a contended arrival is **refused** and a
    /// refusal waits for nothing.
    Refuses,
}

impl Arm {
    /// The per-tenant queue bound this arm declares.
    fn queue(self) -> usize {
        match self {
            Self::Parks => FLOOD_PARKING_QUEUE,
            Self::Refuses => FLOOD_REFUSING_QUEUE,
        }
    }

    /// A name for the arm, for a failure that has to say which one wedged.
    fn label(self) -> &'static str {
        match self {
            Self::Parks => "parking",
            Self::Refuses => "refusing",
        }
    }
}

/// A body that returns its permit without waiting for anybody.
///
/// Bounded by a count rather than a clock, so the simulation's timing comes from
/// nothing but how fast the host is -- and a body that never returns would be a
/// hang in every harness that used one.
async fn self_completing_body() {
    for _ in 0..BODY_YIELDS {
        lgwks_bot::rt::task::yield_now().await;
    }
}

/// Drive the live supervisor through the flood round structure for one seed.
///
/// The structure is the one a noisy-neighbour harness needs: a tenant is driven
/// to its ceiling, and another tenant submits into the permits the first returns.
/// **Every** submission is awaited inside [`FLOOD_BOUND`], so a state in which one
/// would never resolve is a failure naming the arm, the round and the slot, not a
/// hung suite. That bound is the point of this family: the first version of this
/// structure wedged a run for twelve minutes with every worker parked, nothing
/// runnable and no timer pending, and reached the same state in 250 ms once the
/// bound existed.
///
/// **The rule the structure has to obey.** A contended submission parks, and a
/// parked submission can only be resolved by a permit some *other* admitted task
/// returns. So the work that returns permits must complete on its own. A harness
/// whose bodies park on a gate the submitting loop opens cannot do that: the loop
/// opens the gate only between submissions, so a submission that parks leaves
/// nothing to open it. The bodies here complete by themselves, and the live
/// flood test uses the refusing arm instead, where no submission parks at all.
///
/// The seed chooses which tenant takes each slot, so a band covers interleavings
/// rather than one hand-picked order.
fn flood_round_structure(seed: u64) -> Result<(), Box<dyn Error>> {
    for arm in [Arm::Refuses, Arm::Parks] {
        flood_round_structure_in(seed, arm)?;
    }
    Ok(())
}

/// [`flood_round_structure`] for one arm.
fn flood_round_structure_in(seed: u64, arm: Arm) -> Result<(), Box<dyn Error>> {
    let mut rng = Rng::new(seed ^ 0x7100_0d00_0000_0001);
    let loud = Tenant::new("flood-loud")?;
    let quiet = Tenant::new("flood-quiet")?;
    let runtime = lgwks_bot::Runtime::new()?;

    let driven = runtime.block_on(async {
        let mut supervisor =
            Supervisor::with_tenancy(FLOOD_POOL, TenancyPolicy::new(FLOOD_CEILING, arm.queue()));
        for round in 1..=FLOOD_ROUNDS {
            for slot in 0..FLOOD_CEILING {
                let quiet_slot = rng.below(10) < 3;
                let tenant = if quiet_slot { &quiet } else { &loud };
                let label = if quiet_slot { "quiet" } else { "loud" };
                let submitted = lgwks_bot::rt::time::timeout(
                    FLOOD_BOUND,
                    supervisor.spawn_for(tenant, |_t| async {
                        self_completing_body().await;
                    }),
                )
                .await;
                match submitted {
                    Ok(Ok(())) => {}
                    // A refusal is an ordinary answer under load and waits for
                    // nothing, so it is accepted rather than failed.
                    Ok(Err(SpawnRefused::TenantAtCapacity { .. }))
                    | Ok(Err(SpawnRefused::SupervisorQueueFull { .. })) => {}
                    Ok(Err(SpawnRefused::Cancelled)) => {
                        return Err(format!(
                            "seed {seed}: the {} arm refused a submission as cancelled at \
                             round {round} slot {slot}",
                            arm.label()
                        )
                        .into());
                    }
                    _ => {
                        let loud_state = supervisor.tenant_capacity(&loud);
                        let quiet_state = supervisor.tenant_capacity(&quiet);
                        let free = supervisor.snapshot().free();
                        return Err(format!(
                            "seed {seed}: the {} arm wedged -- round {round} slot {slot} on \
                             the {label} tenant never resolved within {FLOOD_BOUND:?}. A \
                             contended submission parked and no admitted task returned its \
                             permit to it. State at the wedge: {free} permit(s) free, loud \
                             {} in flight and {} queued, quiet {} in flight and {} queued.",
                            arm.label(),
                            loud_state.0,
                            loud_state.1,
                            quiet_state.0,
                            quiet_state.1,
                        )
                        .into());
                    }
                }
            }
        }
        let _report = supervisor.shutdown().await;
        Ok::<(), String>(())
    });
    driven.map_err(|why| -> Box<dyn Error> { why.into() })
}

/// Every flood round completes inside its bound, at every seed in the band.
///
/// The regression this pins is a wedged admission, not a slow one: a parked
/// submission that no completed task ever resolves. A bound turns that into a
/// named failure in a quarter of a second instead of a hung suite.
fn a_flood_round_completes_within_its_bound(band: sim::Band) -> TestResult {
    for seed in band.seeds() {
        flood_round_structure(seed)?;
    }
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
    backlogged_tenants_take_shares_in_proportion_to_their_weights_band_00 => backlogged_tenants_take_shares_in_proportion_to_their_weights, 4;
    backlogged_tenants_take_shares_in_proportion_to_their_weights_band_01 => backlogged_tenants_take_shares_in_proportion_to_their_weights, 5;
    a_weighted_round_replays_band_00 => a_weighted_round_replays, 6;
    a_flood_round_completes_within_its_bound_band_00 => a_flood_round_completes_within_its_bound, 7;
    the_supervisor_waiting_bound_is_global_band_00 => the_supervisor_waiting_bound_is_global, 8;
    the_same_seed_replays_the_same_tenancy_trace_band_00 => the_same_seed_replays_the_same_tenancy_trace, 13;
    the_same_seed_replays_the_same_tenancy_trace_band_01 => the_same_seed_replays_the_same_tenancy_trace, 12;
    a_weighted_round_replays_band_01 => a_weighted_round_replays, 14;
}
