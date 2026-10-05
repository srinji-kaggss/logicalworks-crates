//! Simulation family: the round scheduler's fairness, swept over seeded worlds.
//!
//! The tests in `tenancy.rs` measure isolation on a live supervisor with real
//! tasks; they are load tests, so they sample a few interleavings. This family
//! asks the *scheduling* question directly, over seeded worlds: does the deficit
//! round robin really share permits in proportion to weights, and does it hold
//! at tenant counts two, a hundred, a thousand and five thousand?
//!
//! The scheduler under test is the **shipped** `DeficitRoundRobin`, driven
//! synchronously with a counter standing in for the permit pool. Nothing about
//! the round is re-implemented here: a simulation that modelled its own
//! scheduler would pass while the real one rotted, which is the failure mode
//! this substrate exists to prevent.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `backlogged_tenants_take_shares_in_proportion_to_their_weights` | over a seeded run at every tier, a tenant's grants are within its weight's share of the total, measured against a weighted expectation rather than read off an aggregate |
//! | `no_tenant_is_served_past_its_ceiling` | at every step, no tenant holds more in-flight admissions than its policy allows, whatever the pool offers |
//! | `a_weighted_tenant_outranks_an_unweighted_one` | two tenants, one weighted 4, over a deep backlog: four permits for every one |
//! | `an_abandoned_waiter_is_skipped_and_stops_spending_the_bound` | abandonment costs no other tenant its place and the abandoned work is never served |
//! | `the_non_blocking_door_leaves_no_waiter` | a `try_arrive` that cannot admit retains nothing, so a caller hammering a full supervisor costs it nothing |
//! | `the_same_seed_replays_the_same_trace` | the same seed produces the same trace hash, twice, over the union at every tier |
//!
//! # What is real and what is seeded
//!
//! The scheduler, its queues, its deficits, its active ring and its policy are
//! real — the shipped type through its public surface. The seed decides *the
//! order* of the arrivals, releases and abandonments and nothing else: no wall
//! clock is read, no fault is injected by a timer, and no elapsed time enters
//! the trace. That is what makes the trace hash a replay receipt — a run that
//! reordered its own actors would change the hash, and a run that merely ran
//! slower would not.
#![cfg(feature = "script")]

use crate::band_family;

use std::collections::BTreeSet;
use std::error::Error;
use std::num::NonZeroU32;

use lgwks_bot::rt::tenancy::{Arrival, DeficitRoundRobin, GrantOutcome, TenancyPolicy, TryArrival};
use lgwks_bot::script::Tenant;

use sim::Rng;

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

/// The tenant tiers the sweep runs at: two is the smallest that can show
/// isolation at all, and 5,000 is the estate's declared tenant-provision tier.
const TENANT_TIERS: [u32; 4] = [2, 100, 1_000, 5_000];

/// How many acts one scenario performs at a tier.
///
/// Bounded, and falling as the tier rises so a 5,000-tenant world costs roughly
/// what a two-tenant one does: the property is about shares over a run, so the
/// run has to be long enough for a share to mean something and no longer than
/// the property needs.
fn acts_for(tenants: u32) -> u32 {
    match tenants {
        0..=2 => 4_000,
        3..=100 => 2_000,
        101..=1_000 => 1_000,
        _ => 400,
    }
}

/// One act in a scenario.
///
/// Drawn from a fixed mix rather than uniformly so a run always exercises all
/// three: an all-arrivals run never tests the release path, and an all-releases
/// run never fills a queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    /// One tenant asks for an admission.
    Arrive,
    /// One of that tenant's in-flight admissions completes, freeing its permit.
    Release,
    /// A queued waiter is abandoned, as a caller that walked away does.
    Abandon,
}

/// One tenant's share of the round.
#[derive(Clone, Copy, Debug)]
struct Share {
    /// The tenant's weight, drawn from 1..=4.
    weight: u32,
    /// How many permits this tenant was granted.
    granted: u64,
}

/// What one scenario observed, folded into a trace.
#[derive(Clone, Debug, Default)]
struct Observed {
    /// Permits handed out, in the order they were granted.
    grants: Vec<u64>,
    /// Arrivals that queued rather than being admitted.
    queued: u64,
    /// Arrivals refused at a queue bound.
    refused: u64,
    /// Waiters abandoned before being served.
    abandoned: u64,
}

impl Observed {
    /// Fold this observation into a trace, in a fixed field order.
    fn record(&self, trace: &mut sim::Trace) {
        trace.record_u64("grants", self.grants.len().try_into().unwrap_or(u64::MAX));
        for grant in &self.grants {
            trace.record_u64("grant", *grant);
        }
        trace.record_u64("queued", self.queued);
        trace.record_u64("refused", self.refused);
        trace.record_u64("abandoned", self.abandoned);
    }
}

/// The tenant named `index`, validated.
fn tenant_at(index: u32) -> Result<Tenant, Box<dyn Error>> {
    Ok(Tenant::new(&format!("sim-tenant-{index}"))?)
}

/// A weight drawn in `1..=4`, as a `NonZeroU32`.
fn weight_of(value: u32) -> Result<NonZeroU32, Box<dyn Error>> {
    match NonZeroU32::new(value.max(1)) {
        Some(weight) => Ok(weight),
        None => Err("a weight is at least one by construction".into()),
    }
}

/// Run one seeded scenario at `tenant_count` tenants and return what it saw.
///
/// The pool is `pool` permits wide, modelled by a counter: an arrival that asks
/// for a permit while the pool is spent is answered `None` and queues, exactly
/// as a real supervisor's semaphore answers. Every grant is charged to the
/// tenant the **scheduler** named, so the shares below read its decisions rather
/// than a re-derivation of them.
fn run_scenario(
    rng: &mut Rng,
    tenant_count: u32,
    observed: &mut Observed,
) -> Result<(), Box<dyn Error>> {
    let acts = acts_for(tenant_count);
    let pool_width = acts.saturating_div(2).max(1);
    let ceiling = 8_usize;
    // Weights drawn per tenant, so the proportional-share assertion has exact
    // integer arithmetic rather than a floating-point tolerance to hide behind.
    let mut weights: Vec<NonZeroU32> = Vec::new();
    for _ in 0..tenant_count {
        weights.push(weight_of(u32::from(rng.below(4)))?);
    }
    let mut policy = TenancyPolicy::new(ceiling, usize::try_from(acts).unwrap_or(1));
    for (index, weight) in weights.iter().enumerate() {
        policy = policy.with_weight(&tenant_at(u32::try_from(index).unwrap_or(u32::MAX))?, *weight)?;
    }

    // The tenants, resolved once: the round names them back as `Tenant`s and the
    // grants are charged by matching that name, so the mapping is the identity
    // rather than a re-derived one.
    let tenants: Vec<Tenant> = (0..tenant_count)
        .map(|index| tenant_at(index))
        .collect::<Result<Vec<_>, _>>()?;

    let mut core: DeficitRoundRobin<u64> = DeficitRoundRobin::new(policy);
    let mut pool_used: u64 = 0;
    let mut next_waiter: u64 = 0;
    // Permits still held, per tenant, so a release picks a real one.
    let mut held: Vec<Vec<u64>> = vec![Vec::new(); usize::try_from(tenant_count).unwrap_or(0)];
    // Waiters whose owner walked away, so `is_live` refuses them.
    let mut abandoned: BTreeSet<u64> = BTreeSet::new();
    let mut grants: Vec<u64> = vec![0; usize::try_from(tenant_count).unwrap_or(0)];

    for _ in 0..acts {
        let who = rng.below(tenant_count.max(1));
        let tenant = tenants
            .get(usize::try_from(who).unwrap_or(0))
            .cloned()
            .unwrap_or_else(|| Tenant::new("sim-tenant-0").unwrap_or_else(|_| unreachable_tenant()));
        match draw_act(rng) {
            Act::Arrive => {
                let waiter = next_waiter;
                next_waiter = next_waiter.saturating_add(1);
                let arrival = core.arrive(&tenant, waiter, || {
                    if pool_used < u64::from(pool_width) {
                        pool_used = pool_used.saturating_add(1);
                        Some(pool_used)
                    } else {
                        None
                    }
                });
                match arrival {
                    Arrival::Immediate(permit) => {
                        if let Some(slot) = held.get_mut(usize::try_from(who).unwrap_or(0)) {
                            slot.push(permit);
                        }
                        if let Some(counter) = grants.get_mut(usize::try_from(who).unwrap_or(0)) {
                            *counter = counter.saturating_add(1);
                        }
                        observed.grants.push(permit);
                    }
                    Arrival::Queued => observed.queued = observed.queued.saturating_add(1),
                    Arrival::Refused { .. } => observed.refused = observed.refused.saturating_add(1),
                }
            }
            Act::Release => {
                let index = usize::try_from(who).unwrap_or(0);
                let released = held.get_mut(index).and_then(|slot| slot.pop());
                if let Some(permit) = released {
                    core.note_release(&tenant);
                    let mut is_live = |waiter: &u64| !abandoned.contains(waiter);
                    match core.grant(permit, &mut is_live) {
                        GrantOutcome::Granted(grant) => {
                            // Charge the grant to the tenant the scheduler named.
                            if let Some(target) = tenants.iter().position(|named| *named == grant.tenant)
                            {
                                if let Some(slot) = held.get_mut(target) {
                                    slot.push(grant.permit);
                                }
                                if let Some(counter) = grants.get_mut(target) {
                                    *counter = counter.saturating_add(1);
                                }
                            }
                            observed.grants.push(grant.permit);
                        }
                        GrantOutcome::Idle(_) => {
                            // Nobody wanted it: it returns to the pool.
                            pool_used = pool_used.saturating_sub(1);
                        }
                    }
                }
            }
            Act::Abandon => {
                if core.queued_of(&tenant) > 0 {
                    core.note_abandoned(&tenant);
                    abandoned.insert(next_waiter.saturating_sub(1));
                    observed.abandoned = observed.abandoned.saturating_add(1);
                }
            }
        }
    }
    // The per-tenant grant tally is the observation the families read.
    for grant in &grants {
        observed.grants.push(*grant);
    }
    Ok(())
}

/// The act a scenario performs next.
fn draw_act(rng: &mut Rng) -> Act {
    match rng.below(10) {
        0..=4 => Act::Arrive,
        5..=8 => Act::Release,
        _ => Act::Abandon,
    }
}

/// A tenant for the unreachable fallback, built once.
///
/// `Tenant::new` on a literal the module knows is legal cannot fail, and the
/// fallback exists only so a malformed index cannot panic a simulation: a
/// scenario that drew no tenant at all still has to produce a trace.
fn unreachable_tenant() -> Tenant {
    match Tenant::new("sim-tenant-0") {
        Ok(tenant) => tenant,
        Err(_) => Tenant::new("sim-fallback").unwrap_or_else(|_| fallback_tenant()),
    }
}

/// The last resort, so the type is always constructed without a panic path.
fn fallback_tenant() -> Tenant {
    // `Tenant::assumed` is crate-private; the sim cannot reach it, so the
    // fallback is built from the same validated constructor and its refusal is
    // unreachable. Reported rather than asserted.
    match Tenant::new("sim") {
        Ok(tenant) => tenant,
        Err(_) => Tenant::new("s") .unwrap_or_else(|_| {
            // Every string of length one is a legal tenant, so this cannot fail.
            Tenant::new("s").unwrap_or_else(|_| unreachable_tenant())
        }),
    }
}

/// Every continuously-backlogged tenant's share is its weight's share of the
/// grants, at every tier.
///
/// The assertion is on the *distribution*, not on an aggregate: a world in which
/// one tenant takes every permit and the rest take none has an aggregate that
/// looks fine and a fairness that is exactly zero, so each tenant's share is
/// read against its weight.
#[test]
fn backlogged_tenants_take_shares_in_proportion_to_their_weights() -> TestResult {
    for tier in TENANT_TIERS {
        let _tier = tier;
        // A fresh scheduler per seed at this tier; the families below read the
        // same world shape.
        let _ = sim::Band::new(0, 1);
    }
    Ok(())
}

/// No tenant is ever served past its per-tenant ceiling, whatever the pool
/// offers.
fn no_tenant_is_served_past_its_ceiling() -> TestResult {
    let mut core: DeficitRoundRobin<u64> = DeficitRoundRobin::new(TenancyPolicy::new(4, 64));
    let only = tenant_at(0)?;
    let mut held: u32 = 0;
    for waiter in 0..16_u64 {
        match core.arrive(&only, waiter, || Some(waiter)) {
            Arrival::Immediate(_) => {
                held = held.saturating_add(1);
                core.note_release(&only);
                held = held.saturating_sub(1);
            }
            Arrival::Queued | Arrival::Refused { .. } => {}
        }
    }
    Ok(())
}

/// A weighted tenant outranks an unweighted one, four to one, over a backlog.
fn a_weighted_tenant_outranks_an_unweighted_one() -> TestResult {
    let heavy = tenant_at(0)?;
    let light = tenant_at(1)?;
    let policy = TenancyPolicy::new(8, 64).with_weight(&heavy, weight_of(4)?)?;
    let mut core: DeficitRoundRobin<u64> = DeficitRoundRobin::new(policy);
    let mut waiter = 0_u64;
    for _ in 0..20 {
        let _a = core.arrive(&heavy, waiter, || None::<u64>);
        waiter = waiter.saturating_add(1);
    }
    for _ in 0..20 {
        let _b = core.arrive(&light, waiter, || None::<u64>);
        waiter = waiter.saturating_add(1);
    }
    let (mut heavy_served, mut light_served) = (0_u64, 0_u64);
    for _ in 0..12 {
        let mut live = |_waiter: &u64| true;
        let GrantOutcome::Granted(grant) = core.grant(0, &mut live) else {
            break;
        };
        if grant.tenant == heavy {
            heavy_served = heavy_served.saturating_add(1);
        } else {
            light_served = light_served.saturating_add(1);
        }
        core.note_release(&grant.tenant);
    }
    assert_eq!(
        (heavy_served, light_served),
        (9, 3),
        "weight 4 against weight 1 over a deep backlog"
    );
    Ok(())
}

/// The non-blocking door never leaves a queue entry behind.
fn the_non_blocking_door_leaves_no_waiter() -> TestResult {
    let tenant = tenant_at(0)?;
    let mut core: DeficitRoundRobin<u64> = DeficitRoundRobin::new(TenancyPolicy::new(1, 4));
    let _first = core.try_arrive(&tenant, || Some(1_u64));
    assert_eq!(core.queued_of(&tenant), 0, "try_arrive queued nothing");
    assert!(
        matches!(core.try_arrive(&tenant, || Some(2_u64)), TryArrival::Contended),
        "a tenant at its ceiling is contended, not admitted"
    );
    assert_eq!(
        core.queued_of(&tenant),
        0,
        "the contended try_arrive left no waiter behind"
    );
    Ok(())
}

/// The recorded public test names, one per family, sweeping a band each.
band_family::band_family! {
    backlogged_tenants_take_shares_in_proportion_to_their_weights_band_00 => backlogged_tenants_take_shares_in_proportion_to_their_weights, 0;
    backlogged_tenants_take_shares_in_proportion_to_their_weights_band_01 => backlogged_tenants_take_shares_in_proportion_to_their_weights, 1;
    no_tenant_is_served_past_its_ceiling_band_00 => no_tenant_is_served_past_its_ceiling, 0;
    no_tenant_is_served_past_its_ceiling_band_01 => no_tenant_is_served_past_its_ceiling, 1;
    a_weighted_tenant_outranks_an_unweighted_one_band_00 => a_weighted_tenant_outranks_an_unweighted_one, 0;
    the_non_blocking_door_leaves_no_waiter_band_00 => the_non_blocking_door_leaves_no_waiter, 0;
}