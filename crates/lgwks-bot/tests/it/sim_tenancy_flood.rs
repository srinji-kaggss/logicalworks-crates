//! Simulation family: a fail-at-once flood against a neighbour, in virtual
//! time, through the shipped round scheduler (#375).
//!
//! INV-BOT-151's claim is "admission, not CPU scheduling": a tenant that floods
//! the supervisor must not cost its neighbour admission work or throughput. The
//! wall-clock tests that used to judge it read the host instead — on a CI
//! machine shared by thirteen lanes their paired ratios spread from 170‰ to
//! 34,985‰ in one run (main run 37970240208) — so they could neither hold the
//! claim nor refute it. This family checks the claim where it lives, in the
//! round's decisions, and checks it exactly:
//!
//! | Test | Property it pins |
//! |---|---|
//! | `a_flood_does_not_delay_the_neighbour_in_virtual_time` | with two ceilings that fit the pool, the neighbour's last task ends at the same virtual instant with and without the flood |
//! | `a_ceiling_past_half_the_pool_lets_a_flood_delay_the_neighbour` | the negative control: with ceilings that overlap, some seed's flood does delay it, so the equality above is a measurement |
//! | `round_work_stays_a_constant_multiple_of_its_events` | the round's own step count stays within four steps per event at every flood size, abandonment included |
//! | `the_same_seed_replays_the_same_flood` | one seed, one trace, twice |
//!
//! The executor is the supervisor's tenancy shell, step for step: an arrival
//! that queues pumps the round before its caller parks; a completion notes its
//! release and pumps with the permit it freed; a pump draws from the pool only
//! while some tenant is eligible; and an owner that walks away while queued is
//! reported to the round, which skips or compacts it. Each caller submits its
//! next task the moment its last one was admitted and waits only on its own
//! admission. The neighbour is one caller, as a `Supervisor` owner is; the
//! attacker is one to 256 callers, because the round is a public type whose
//! queues can hold many live waiters per tenant, and its work claim has to hold
//! there too — with one live waiter a queue never grows, and a compaction that
//! scanned it on every abandonment would cost nothing visible. Time moves only
//! to the next completion, so nothing here reads a clock.

#![cfg(feature = "script")]

use crate::sim;

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::error::Error;

use lgwks_bot::rt::tenancy::{Arrival, DeficitRoundRobin, GrantOutcome, TenancyPolicy};
use lgwks_bot::script::Tenant;

use sim::seed::Rng;

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

/// Fail the world with `what`, logged before it reaches the caller.
fn refused<T>(what: String) -> Result<T, Box<dyn Error>> {
    lgwks_std::trace::debug!(error = %what, "sim_tenancy_flood: returning an error to the caller");
    Err(what.into())
}

/// The neighbour's caller; every other caller submits for the attacker.
const NEIGHBOUR: usize = 0;

/// One tenant's caller: who it submits for, how much is left, and what it is
/// waiting on.
struct Caller {
    /// The tenant every submission is charged to.
    tenant: Tenant,
    /// Tasks still to submit.
    remaining: u32,
    /// The inclusive range a task's virtual duration is drawn from.
    duration: (u32, u32),
    /// Per mille of queued submissions whose owner walks away at once.
    abandon: u32,
    /// The waiter this caller is parked on, if its last arrival queued.
    parked: Option<u64>,
    /// This caller's own stream: durations and walk-aways. One stream per
    /// caller, so the neighbour draws the same workload whether or not the
    /// attacker draws anything beside it.
    rng: Rng,
}

/// One flood world: the policy, the callers and the scheduler's state.
struct World {
    /// The shipped scheduler. A waiter is its id.
    round: DeficitRoundRobin<u64>,
    /// Permits free in the pool.
    pool: u32,
    /// The virtual clock.
    now: u64,
    /// Running tasks, earliest end first: `(end, start order, caller)`.
    running: BinaryHeap<Reverse<(u64, u64, usize)>>,
    /// Waiter id to `(caller, duration)`, for the grant that starts it.
    waiting: BTreeMap<u64, (usize, u64)>,
    /// Waiters whose owner walked away.
    abandoned: BTreeSet<u64>,
    /// The next waiter id, and the next start order.
    next_id: u64,
    /// The neighbour's caller first, then the attacker's.
    callers: Vec<Caller>,
    /// When the neighbour's last task ended.
    neighbour_done: u64,
    /// Events the round was handed: arrivals, grants, releases, abandonments.
    events: u64,
}

/// How a world is drawn.
#[derive(Clone, Copy)]
struct Shape {
    /// The pool.
    pool: u32,
    /// The per-tenant ceiling, applied to both tenants.
    ceiling: u32,
    /// The neighbour's tasks.
    tasks: u32,
    /// Attacker submissions per neighbour task.
    flood: u32,
    /// The attacker's concurrent callers.
    submitters: u32,
    /// Per mille of the attacker's queued submissions abandoned at once.
    abandon: u32,
}

impl World {
    /// A world for `shape`, every draw taken from streams cut from `seed`.
    fn new(shape: Shape, seed: u64) -> Result<Self, Box<dyn Error>> {
        let mut neighbour_rng = Rng::new(seed);
        let neighbour_longest = neighbour_rng.between(1, 40);
        let attacker_longest = Rng::new(seed ^ ATTACKER_STREAM).between(0, 40);
        let queue = usize::try_from(shape.tasks.saturating_mul(shape.flood.max(1)))?;
        let ceiling = usize::try_from(shape.ceiling)?;
        let floods = shape.tasks.saturating_mul(shape.flood);
        let submitters = shape.submitters.max(1);
        let mut callers = vec![Caller {
            tenant: Tenant::new("neighbour")?,
            remaining: shape.tasks,
            duration: (1, neighbour_longest),
            abandon: 0,
            parked: None,
            rng: neighbour_rng,
        }];
        let attacker = Tenant::new("attacker")?;
        for index in 0..submitters {
            // The flood is split as evenly as it divides: the first
            // `floods % submitters` callers take one more.
            let share = floods
                .div_euclid(submitters)
                .saturating_add(u32::from(index < floods.rem_euclid(submitters)));
            callers.push(Caller {
                tenant: attacker.clone(),
                remaining: share,
                duration: (0, attacker_longest),
                abandon: shape.abandon,
                parked: None,
                rng: Rng::new(seed ^ ATTACKER_STREAM ^ u64::from(index).saturating_add(1)),
            });
        }
        Ok(Self {
            round: DeficitRoundRobin::new(
                TenancyPolicy::new(ceiling, queue).with_queue_total(queue.saturating_mul(2)),
            ),
            pool: shape.pool,
            now: 0,
            running: BinaryHeap::new(),
            waiting: BTreeMap::new(),
            abandoned: BTreeSet::new(),
            next_id: 0,
            callers,
            neighbour_done: 0,
            events: 0,
        })
    }

    /// Start a task for `caller` lasting `duration`.
    fn start(&mut self, caller: usize, duration: u64) {
        let order = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.running
            .push(Reverse((self.now.saturating_add(duration), order, caller)));
    }

    /// Hand permits to eligible tenants until none can take one: the shell's
    /// pump, with the pool as a counter.
    fn pump(&mut self, spare: bool) -> Result<(), Box<dyn Error>> {
        let mut spare = spare;
        loop {
            if !spare {
                if !self.round.has_eligible() || self.pool == 0 {
                    return Ok(());
                }
                self.pool = self.pool.saturating_sub(1);
            }
            spare = false;
            let abandoned = &self.abandoned;
            let mut is_live = |waiter: &u64| !abandoned.contains(waiter);
            match self.round.grant((), &mut is_live) {
                GrantOutcome::Idle(()) => {
                    self.pool = self.pool.saturating_add(1);
                    return Ok(());
                }
                GrantOutcome::Granted(grant) => {
                    self.events = self.events.saturating_add(1);
                    let (caller, duration) = self
                        .waiting
                        .remove(&grant.waiter)
                        .ok_or("a grant named a waiter nobody registered")?;
                    if let Some(parked) = self.callers.get_mut(caller)
                        && parked.parked == Some(grant.waiter)
                    {
                        parked.parked = None;
                    }
                    self.start(caller, duration);
                }
                _ => {
                    return refused(String::from(
                        "the round answered a grant it does not document",
                    ));
                }
            }
        }
    }

    /// The owner of queued waiter `id` walks away: the slot half first, then
    /// the round half, in the shell's order.
    fn abandon(&mut self, caller: usize, tenant: &Tenant, id: u64) {
        self.abandoned.insert(id);
        self.waiting.remove(&id);
        if let Some(who) = self.callers.get_mut(caller) {
            who.parked = None;
        }
        self.events = self.events.saturating_add(1);
        let abandoned = &self.abandoned;
        let mut is_live = |waiter: &u64| !abandoned.contains(waiter);
        self.round.note_abandoned(tenant, &mut is_live);
    }

    /// Submit `caller`'s next task.
    fn submit(&mut self, caller: usize) -> Result<(), Box<dyn Error>> {
        let (tenant, duration, walks_away) = {
            let who = self.callers.get_mut(caller).ok_or("no such caller")?;
            who.remaining = who.remaining.saturating_sub(1);
            let duration = u64::from(who.rng.between(who.duration.0, who.duration.1));
            let walks_away = who.rng.chance(who.abandon);
            (who.tenant.clone(), duration, walks_away)
        };
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.events = self.events.saturating_add(1);
        let pool = &mut self.pool;
        let take = || {
            if *pool == 0 {
                return None;
            }
            *pool = pool.saturating_sub(1);
            Some(())
        };
        match self.round.arrive(&tenant, id, take) {
            Arrival::Immediate(()) => self.start(caller, duration),
            Arrival::Queued => {
                self.waiting.insert(id, (caller, duration));
                if let Some(who) = self.callers.get_mut(caller) {
                    who.parked = Some(id);
                }
                self.pump(false)?;
                let still_parked = self
                    .callers
                    .get(caller)
                    .is_some_and(|who| who.parked == Some(id));
                if still_parked && walks_away {
                    self.abandon(caller, &tenant, id);
                }
            }
            other => {
                return refused(format!(
                    "a {tenant:?} arrival was refused ({other:?}) inside bounds sized for every task"
                ));
            }
        }
        Ok(())
    }

    /// End the earliest running task, moving the clock to its end. `false`
    /// when nothing is running.
    fn complete_next(&mut self) -> Result<bool, Box<dyn Error>> {
        let Some(Reverse((end, _, caller))) = self.running.pop() else {
            return Ok(false);
        };
        self.now = end;
        if caller == NEIGHBOUR {
            self.neighbour_done = end;
        }
        let tenant = self
            .callers
            .get(caller)
            .ok_or("no such caller")?
            .tenant
            .clone();
        self.events = self.events.saturating_add(1);
        self.round.note_release(&tenant);
        self.pump(true)?;
        Ok(true)
    }

    /// Run every caller to the end. Each submits whenever it is not parked;
    /// time moves only when all are parked or done.
    fn run(&mut self) -> Result<(), Box<dyn Error>> {
        loop {
            let mut submitted = true;
            while submitted {
                submitted = false;
                for caller in 0..self.callers.len() {
                    let ready = self
                        .callers
                        .get(caller)
                        .is_some_and(|who| who.parked.is_none() && who.remaining > 0);
                    if ready {
                        self.submit(caller)?;
                        submitted = true;
                    }
                }
            }
            if !self.complete_next()? {
                let stuck = self
                    .callers
                    .iter()
                    .any(|who| who.parked.is_some() || who.remaining > 0);
                if stuck {
                    return refused(String::from(
                        "a caller is still waiting with nothing running",
                    ));
                }
                return Ok(());
            }
        }
    }
}

/// What one seed's pair of worlds measured.
#[derive(Clone, Copy)]
struct Outcome {
    /// The neighbour's finish instant with no flood.
    alone: u64,
    /// The neighbour's finish instant under the flood.
    flooded: u64,
    /// The flooded round's step count.
    work: u64,
    /// The events the flooded round was handed.
    events: u64,
}

/// The neighbour's finish instant in a world drawn from `seed`, alone and
/// under the flood, plus the flooded world's round work and event count.
fn paired(seed: u64, shape: Shape) -> Result<Outcome, Box<dyn Error>> {
    let mut alone = World::new(Shape { flood: 0, ..shape }, seed)?;
    alone.run()?;
    let mut flooded = World::new(shape, seed)?;
    flooded.run()?;
    Ok(Outcome {
        alone: alone.neighbour_done,
        flooded: flooded.neighbour_done,
        work: flooded.round.work(),
        events: flooded.events,
    })
}

/// Which ceilings a sweep draws.
#[derive(Clone, Copy)]
enum Ceilings {
    /// Both ceilings fit the pool twice: the attacker can never hold a permit
    /// the neighbour's own ceiling would reach.
    Isolated,
    /// The ceilings overlap: the attacker can.
    Overlapping,
    /// Either, per seed.
    Either,
}

/// Cuts the attacker's streams from the seed apart from the neighbour's, so
/// the neighbour draws the same workload whatever the attacker draws.
const ATTACKER_STREAM: u64 = 0xa77a_c4e2_f100_d5ee;

/// The flood sizes every property is checked at.
const FLOODS: [u32; 4] = [1, 4, 32, 256];

/// Sweep the whole seed space twice, at every flood size, recording every
/// measurement in the trace, and hand each shape and outcome to `check`.
fn sweep_floods<F>(ceilings: Ceilings, mut check: F) -> TestResult
where
    F: FnMut(Shape, Outcome) -> Result<(), String>,
{
    for band in sim::bands(sim::SEED_SPACE, sim::BANDS) {
        sim::assert_replays(band, |run| {
            for flood in FLOODS {
                let isolated = match ceilings {
                    Ceilings::Isolated => true,
                    Ceilings::Overlapping => false,
                    Ceilings::Either => run.rng().chance(500),
                };
                let pool = run.rng().between(2, 64);
                let ceiling = if isolated {
                    run.rng().between(1, pool.div_euclid(2))
                } else {
                    run.rng()
                        .between(pool.div_euclid(2).saturating_add(1), pool)
                };
                let shape = Shape {
                    pool,
                    ceiling,
                    tasks: run.rng().between(1, 120),
                    flood,
                    submitters: run.rng().between(1, 256),
                    abandon: run.rng().below(400),
                };
                let seed = run.rng().next_u64();
                let outcome = paired(seed, shape)?;
                for (label, value) in [
                    ("alone", outcome.alone),
                    ("flooded", outcome.flooded),
                    ("work", outcome.work),
                    ("events", outcome.events),
                ] {
                    run.trace.record_number(label, value);
                }
                check(shape, outcome)?;
            }
            Ok(())
        })?;
    }
    Ok(())
}

/// With both ceilings inside half the pool, the neighbour never waits on a
/// permit the attacker holds, so its last task ends at the same virtual
/// instant whether or not the attacker floods — exactly, not within 10%.
#[test]
fn a_flood_does_not_delay_the_neighbour_in_virtual_time() -> TestResult {
    sweep_floods(Ceilings::Isolated, |shape, outcome| {
        if outcome.alone == outcome.flooded {
            return Ok(());
        }
        Err(format!(
            "pool {} ceiling {} tasks {} flood {}: the neighbour finished at {} under the \
             flood and {} alone",
            shape.pool, shape.ceiling, shape.tasks, shape.flood, outcome.flooded, outcome.alone
        ))
    })
}

/// The negative control: with ceilings past half the pool, the attacker can
/// hold permits the neighbour's own ceiling would have reached, and some
/// seed's flood delays it. Without this, the equality above could hold
/// because the world never let the flood touch the neighbour at all.
#[test]
fn a_ceiling_past_half_the_pool_lets_a_flood_delay_the_neighbour() -> TestResult {
    let mut delayed = 0_u32;
    sweep_floods(Ceilings::Overlapping, |_, outcome| {
        if outcome.flooded > outcome.alone {
            delayed = delayed.saturating_add(1);
        }
        Ok(())
    })?;
    assert!(
        delayed > 0,
        "no seed with overlapping ceilings let the flood delay the neighbour, so the \
         virtual-time equality is not a measurement"
    );
    Ok(())
}

/// The round's own step count stays within four steps per event at every
/// flood size — arrivals, grants, releases and abandonments each pay for the
/// loop passes and skips they cause — so the attacker's volume buys it no
/// extra round work per decision, whatever the queues hold.
#[test]
fn round_work_stays_a_constant_multiple_of_its_events() -> TestResult {
    /// Steps per event the accounting in `DeficitRoundRobin::work` allows.
    const STEPS_PER_EVENT: u64 = 4;
    sweep_floods(Ceilings::Either, |shape, outcome| {
        if outcome.work <= outcome.events.saturating_mul(STEPS_PER_EVENT) {
            return Ok(());
        }
        Err(format!(
            "flood {} submitters {}: the round took {} steps for {} events, past \
             {STEPS_PER_EVENT} per event",
            shape.flood, shape.submitters, outcome.work, outcome.events
        ))
    })
}

/// One seed, one trace, twice, across both ceiling regimes and every flood.
#[test]
fn the_same_seed_replays_the_same_flood() -> TestResult {
    sweep_floods(Ceilings::Either, |_, _| Ok(()))
}
