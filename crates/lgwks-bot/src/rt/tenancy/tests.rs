use super::{
    Arrival, DeficitRoundRobin, GrantOutcome, MAX_QUEUE_PER_TENANT, MAX_TENANT_WEIGHT,
    TenancyError, TenancyPolicy,
};
use crate::script::Tenant;
use std::error::Error;
use std::num::NonZeroU32;

/// A scheduler over numbered waiters and numbered permits; the pool is a
/// closure over a counter in each test.
type Core = DeficitRoundRobin<u32>;

/// The tenant named `name`, validated.
fn tenant(name: &str) -> Result<Tenant, Box<dyn Error>> {
    Ok(Tenant::new(name)?)
}

/// Every waiter is owned by somebody.
fn all_live(_waiter: &u32) -> bool {
    true
}

/// Whether an arrival parked rather than being admitted at once.
///
/// A predicate rather than a value comparison because the queued arm of
/// [`Arrival`] carries no permit, so there is nothing for the caller to
/// pattern it against — asking the question is the comparison.
fn parked<P>(arrival: Arrival<P>) -> bool {
    matches!(arrival, Arrival::Queued)
}

#[test]
fn a_fresh_core_admits_immediately_from_the_pool() -> Result<(), Box<dyn Error>> {
    let mut core: Core = DeficitRoundRobin::new(TenancyPolicy::new(2, 4));
    let t0 = tenant("t-0")?;
    assert_eq!(
        core.arrive(&t0, 100, || Some(7_u32)),
        Arrival::Immediate(7),
        "an empty ring and an under-ceiling tenant admit at once"
    );
    assert_eq!(
        core.in_flight_of(&t0),
        1,
        "the immediate admission is charged to the tenant"
    );
    let queued: Arrival<u32> = Arrival::Queued;
    assert_eq!(
        core.arrive(&t0, 101, || None),
        queued,
        "with a ring member present, the next arrival queues rather than jumping it"
    );
    Ok(())
}

/// The waiter is built only when an arrival parks (#375): an immediate
/// admission and both refusals never call `make_waiter`, and a queued
/// arrival calls it exactly once.
#[test]
fn a_waiter_is_built_only_for_an_arrival_that_parks() -> Result<(), Box<dyn Error>> {
    let mut core: Core = DeficitRoundRobin::new(TenancyPolicy::new(1, 1).with_queue_total(1));
    let t0 = tenant("t-0")?;
    let t1 = tenant("t-1")?;
    let built = std::cell::Cell::new(0_u32);
    let make = || {
        built.set(built.get().saturating_add(1));
        built.get()
    };
    assert_eq!(
        core.arrive_with(&t0, make, || Some(7_u32)),
        Arrival::Immediate(7),
        "a free pool admits at once"
    );
    assert_eq!(built.get(), 0, "an immediate admission built no waiter");
    assert!(
        parked(core.arrive_with(&t0, make, || None::<u32>)),
        "a spent pool parks the arrival"
    );
    assert_eq!(
        built.get(),
        1,
        "the parked arrival built exactly one waiter"
    );
    assert_eq!(
        core.arrive_with(&t0, make, || None::<u32>),
        Arrival::Refused { limit: 1 },
        "a full tenant queue refuses"
    );
    assert_eq!(
        core.arrive_with(&t1, make, || None::<u32>),
        Arrival::SupervisorFull { limit: 1 },
        "a full supervisor refuses"
    );
    assert_eq!(built.get(), 1, "neither refusal built a waiter");
    Ok(())
}

#[test]
fn a_refusal_names_the_queue_bound_and_retains_nothing() -> Result<(), Box<dyn Error>> {
    let mut core: Core = DeficitRoundRobin::new(TenancyPolicy::new(1, 2));
    let t0 = tenant("t-0")?;
    // Pool of one: the first arrival takes it, the rest queue behind it. Each
    // arrival is offered the same shared cell through its own closure, so the
    // one permit is spent exactly once across four calls.
    let cell = std::cell::Cell::new(1_u32);
    let take = || {
        let left = cell.get();
        if left == 0 {
            return None;
        }
        cell.set(left.saturating_sub(1));
        Some(left.saturating_sub(1))
    };
    assert_eq!(
        core.arrive(&t0, 1, take),
        Arrival::Immediate(0),
        "the pool's one permit is taken at once"
    );
    let queued: Arrival<u32> = Arrival::Queued;
    assert_eq!(core.arrive(&t0, 2, || None), queued, "the second waits");
    assert_eq!(core.arrive(&t0, 3, || None), queued, "the third waits");
    let spent = || None::<u32>;
    assert_eq!(
        core.arrive(&t0, 4, spent),
        Arrival::Refused { limit: 2 },
        "the fourth is past the queue bound and the refusal names it"
    );
    assert_eq!(
        core.queued_of(&t0),
        2,
        "only the two live waiters are counted"
    );
    Ok(())
}

#[test]
fn two_backlogged_tenants_alternate_permits() -> Result<(), Box<dyn Error>> {
    let mut core: Core = DeficitRoundRobin::new(TenancyPolicy::new(4, 8));
    let first = tenant("a")?;
    let second = tenant("b")?;
    // Three waiters each, so both tenants stay backlogged for the whole
    // sweep: alternation is a property of two *waiting* tenants, and a
    // tenant whose queue empties has nothing to be denied.
    let first_arrival = core.arrive(&first, 1, || None::<u32>);
    assert!(parked(first_arrival), "the first tenant queues");
    let second_arrival = core.arrive(&second, 2, || None::<u32>);
    assert!(parked(second_arrival), "the second tenant queues");
    for (tenant, waiter) in [(&first, 3_u32), (&second, 4_u32)] {
        let arrived = core.arrive(tenant, waiter, || None::<u32>);
        assert!(parked(arrived), "the backlog grows on both tenants");
    }
    let first_third = core.arrive(&first, 5, || None::<u32>);
    assert!(parked(first_third), "the first tenant queues a third");
    let second_third = core.arrive(&second, 6, || None::<u32>);
    assert!(parked(second_third), "the second tenant queues a third");
    // Four permits from a six-waiter backlog: enough grants to see the
    // pattern repeat without draining either queue.
    let mut served: Vec<u32> = Vec::new();
    for permit in 0..4_u32 {
        let mut live = all_live;
        let outcome = core.grant(permit, &mut live);
        let GrantOutcome::Granted(grant) = outcome else {
            return Err("a queued tenant must take the permit".into());
        };
        served.push(grant.waiter);
        core.note_release(&grant.tenant);
    }
    assert_eq!(
        served,
        vec![1, 2, 3, 4],
        "equal weights alternate strictly: first, second, first, second"
    );
    Ok(())
}

#[test]
fn a_double_weight_takes_two_permits_per_turn() -> Result<(), Box<dyn Error>> {
    let heavy = tenant("heavy")?;
    let light = tenant("light")?;
    let two: NonZeroU32 = 2_u32.try_into()?;
    let policy = TenancyPolicy::new(8, 16).with_weight(&heavy, two)?;
    let mut core: Core = DeficitRoundRobin::new(policy);
    let queued: Arrival<u32> = Arrival::Queued;
    // Both tenants stay backlogged for the whole sweep, which is what makes
    // the two-per-turn share measurable at all.
    assert_eq!(core.arrive(&heavy, 1, || None), queued, "heavy queues");
    assert_eq!(core.arrive(&light, 2, || None), queued, "light queues");
    assert_eq!(
        core.arrive(&heavy, 3, || None),
        queued,
        "heavy queues again"
    );
    assert_eq!(
        core.arrive(&light, 4, || None),
        queued,
        "light queues again"
    );
    // Six waiters each, a deep backlog, so the 2:1 share is observable over
    // many turns rather than in one lucky interleaving.
    for waiter in 5..11_u32 {
        let _heavy_arrival = core.arrive(&heavy, waiter, || None::<u32>);
    }
    for waiter in 11..17_u32 {
        let _light_arrival = core.arrive(&light, waiter, || None::<u32>);
    }
    let mut heavy_served: u32 = 0;
    let mut light_served: u32 = 0;
    for permit in 0..9_u32 {
        let mut live = all_live;
        let outcome = core.grant(permit, &mut live);
        let GrantOutcome::Granted(grant) = outcome else {
            return Err("a backlogged tenant must take the permit".into());
        };
        if grant.tenant == heavy {
            heavy_served = heavy_served.saturating_add(1);
        } else {
            light_served = light_served.saturating_add(1);
        }
        core.note_release(&grant.tenant);
    }
    // Nine permits against weights 2 and 1: the heavy tenant's share is its
    // weight over the total, so 6 heavy and 3 light is the fair outcome, and
    // any deviation means the round is not honouring the weights.
    assert_eq!(
        (heavy_served, light_served),
        (6, 3),
        "weight 2 takes two permits for every one weight 1 takes"
    );
    Ok(())
}

#[test]
fn a_tenant_at_its_ceiling_is_parked_until_it_releases() -> Result<(), Box<dyn Error>> {
    let mut core: Core = DeficitRoundRobin::new(TenancyPolicy::new(1, 8));
    let loud = tenant("loud")?;
    let quiet = tenant("quiet")?;
    assert_eq!(
        core.arrive(&loud, 1, || Some(10_u32)),
        Arrival::Immediate(10),
        "the loud tenant takes the pool's one permit"
    );
    let queued: Arrival<u32> = Arrival::Queued;
    assert_eq!(core.arrive(&loud, 2, || None), queued, "its next waits");
    assert_eq!(
        core.arrive(&quiet, 3, || None),
        queued,
        "the quiet tenant waits"
    );
    // A permit frees. The loud tenant is at its ceiling (one in flight of
    // one), so the quiet tenant is the one the round must serve.
    core.note_release(&loud);
    let mut live = all_live;
    let first = core.grant(20, &mut live);
    let GrantOutcome::Granted(grant) = first else {
        return Err("the quiet tenant is eligible and must be served".into());
    };
    assert_eq!(
        grant.waiter, 3,
        "the permit went to the tenant under its ceiling"
    );
    core.note_release(&grant.tenant);
    // Both tenants are under their ceiling again; the loud one is next on
    // the ring, so its waiter runs now.
    let second = core.grant(21, &mut live);
    let GrantOutcome::Granted(next) = second else {
        return Err("the loud tenant is eligible again and must be served".into());
    };
    assert_eq!(
        next.waiter, 2,
        "the loud tenant's waiter runs as soon as it has room"
    );
    Ok(())
}

#[test]
fn an_abandoned_waiter_is_skipped_and_stops_spending_the_bound() -> Result<(), Box<dyn Error>> {
    let mut core: Core = DeficitRoundRobin::new(TenancyPolicy::new(1, 1));
    let t0 = tenant("t-0")?;
    assert_eq!(
        core.arrive(&t0, 7, || Some(1_u32)),
        Arrival::Immediate(1),
        "one in flight"
    );
    let queued: Arrival<u32> = Arrival::Queued;
    assert_eq!(
        core.arrive(&t0, 8, || None),
        queued,
        "the queue bound has room"
    );
    let spent = || None::<u32>;
    assert_eq!(
        core.arrive(&t0, 9, spent),
        Arrival::Refused { limit: 1 },
        "the bound is reached while the waiter lives"
    );
    // Waiter 8 is the one whose owner walked away, and the liveness predicate says
    // so: the scheduler compacts on abandonment using the same knowledge the
    // grant skip uses, and a predicate that denied nothing would keep an
    // entry the count has already written off.
    let mut gone = |waiter: &u32| *waiter != 8;
    core.note_abandoned(&t0, &mut gone);
    assert_eq!(
        core.arrive(&t0, 10, || None),
        queued,
        "an abandoned waiter no longer spends the tenant's queue room"
    );
    core.note_release(&t0);
    let mut live = |waiter: &u32| *waiter != 8;
    let outcome = core.grant(2, &mut live);
    let GrantOutcome::Granted(grant) = outcome else {
        return Err("the live waiter must be served".into());
    };
    assert_eq!(
        grant.waiter, 10,
        "the abandoned head was skipped, not served"
    );
    assert_eq!(
        core.queued_of(&t0),
        0,
        "the skipped waiter is gone from the queue and the count with it"
    );
    Ok(())
}

#[test]
fn an_idle_tenant_costs_nothing_after_it_drains() -> Result<(), Box<dyn Error>> {
    let mut core: Core = DeficitRoundRobin::new(TenancyPolicy::new(2, 2));
    let t0 = tenant("t-0")?;
    assert_eq!(
        core.arrive(&t0, 1, || Some(1_u32)),
        Arrival::Immediate(1),
        "admitted"
    );
    core.note_release(&t0);
    assert_eq!(
        core.tenants_with_queues(),
        0,
        "a drained tenant holds no queue and no entry"
    );
    Ok(())
}

#[test]
fn an_idle_pool_answers_idle_rather_than_spinning() -> Result<(), Box<dyn Error>> {
    let mut core: Core = DeficitRoundRobin::new(TenancyPolicy::new(4, 8));
    let t0 = tenant("t-0")?;
    assert_eq!(
        core.arrive(&t0, 1, || Some(1)),
        Arrival::Immediate(1),
        "one in flight"
    );
    core.note_release(&t0);
    assert!(
        !core.has_eligible(),
        "with nothing waiting and the tenant drained, no tenant is eligible"
    );
    let mut live = all_live;
    let outcome = core.grant(9, &mut live);
    let GrantOutcome::Idle(permit) = outcome else {
        return Err("a permit nobody can take is handed back".into());
    };
    assert_eq!(
        permit, 9,
        "the idle permit is returned to the caller's pool"
    );
    Ok(())
}

#[test]
fn the_declared_bounds_are_clamped_to_their_ceilings() {
    let policy = TenancyPolicy::new(0, MAX_QUEUE_PER_TENANT.saturating_add(1));
    assert_eq!(
        policy.per_tenant_limit(),
        1,
        "a zero ceiling reads as one, as Supervisor::new reads its own bound"
    );
    assert_eq!(
        policy.queue_per_tenant(),
        MAX_QUEUE_PER_TENANT,
        "a queue bound past the ceiling is clamped to it"
    );
}

#[test]
fn a_weight_past_its_bound_is_refused_naming_both_numbers() -> Result<(), Box<dyn Error>> {
    let heavy = tenant("heavy")?;
    let past = MAX_TENANT_WEIGHT.saturating_add(1);
    let Some(too_heavy) = NonZeroU32::new(past) else {
        return Err("a weight one past its bound is never zero".into());
    };
    let outcome = TenancyPolicy::new(1, 1).with_weight(&heavy, too_heavy);
    assert_eq!(
        outcome.err(),
        Some(TenancyError::WeightTooHigh {
            weight: past,
            limit: MAX_TENANT_WEIGHT,
        }),
        "a weight above the ceiling is refused with the weight and the bound"
    );
    Ok(())
}
