//! Simulation family: the shipped round scheduler, model-checked over seeded
//! worlds with an executor that follows the supervisor's own protocol.
//!
//! `sim_tenancy` pins retention and weighted shares on arrival-only shapes.
//! This file drives the whole life of an admission through the **shipped**
//! `DeficitRoundRobin`: arrivals that queue, refusing arrivals, completions
//! that hand their permit back through the round, owners that walk away while
//! they wait, and owners that walk away in the instant between the round
//! choosing them and the permit reaching them. A plain reference model, kept
//! beside the scheduler, says what every count must be after every act, and
//! each test below checks one property of the scheduler against it.
//!
//! The executor half is the supervisor's tenancy shell, step for step: an
//! arrival that queues pumps the round before its caller parks; a completion
//! notes its release and pumps with the permit it freed; a pump draws from the
//! pool only while some tenant is eligible; a grant whose owner left after the
//! liveness check is charged back and its permit recycles into the same pump,
//! with the round's abandoned count untouched; and that owner's own report,
//! which reaches the round only after the pump lets go of it, finds its slot
//! withdrawn and reports nothing, because its waiter is no longer in a queue.
//! The scheduler is not re-implemented here; only the pool is a counter.
//!
//! The late path is the one this file was written to check, and its first run
//! found it wrong: the shell used to correct the abandoned count inside the
//! pump, before the owner's report had added to it, and a count that saturates
//! at zero lost the correction. `a_late_abandonment_leaves_the_counts_exact`
//! failed at seed 9 and a tenant held five live waiters past a bound of four.
//!
//! One seed decides the policy, the tenants and every act. Nothing reads the
//! clock or an unseeded source, so each seed's trace hash is a replay receipt,
//! and every test sweeps the whole seed space twice and compares the hashes.
//!
//! | Test | Property it pins |
//! |---|---|
//! | `in_flight_never_passes_the_tenant_ceiling` | no tenant is ever charged more admissions than its ceiling |
//! | `live_waiters_never_pass_the_tenant_queue_bound` | no tenant ever holds more live waiters than its queue bound |
//! | `retained_waiters_never_pass_the_supervisor_bound` | the round never retains more waiters than the supervisor's bound |
//! | `permits_are_conserved_between_the_pool_and_the_round` | every permit is in the pool or charged to exactly one admission |
//! | `the_round_idles_only_when_no_live_waiter_has_room` | a permit is handed back only when no live waiter is under its ceiling |
//! | `each_tenant_is_served_in_arrival_order` | a tenant's live waiters are granted oldest first |
//! | `every_live_waiter_is_served_exactly_once` | after a drain, every waiter whose owner stayed was granted once |
//! | `an_abandoned_waiter_is_never_granted` | no grant ever names a waiter whose owner left |
//! | `a_contended_try_arrival_retains_nothing` | a refused non-blocking arrival changes no count |
//! | `an_immediate_admission_never_jumps_a_waiting_tenant` | nobody is admitted past the pool while anybody waits |
//! | `a_refusal_names_the_bound_it_reached` | each refusal arm fires only at its bound, and names it |
//! | `a_drained_round_retains_nothing` | once every admission ends, no waiter and no tenant state remains |
//! | `the_live_count_matches_the_owners_still_waiting` | `queued_of` is exactly the number of owners still waiting |
//! | `a_late_abandonment_leaves_the_counts_exact` | an owner leaving between choice and delivery costs no count |
//! | `an_empty_ring_means_the_round_is_idle` | `has_eligible` false is never contradicted by a grant |
//! | `declared_bounds_and_weights_read_back_as_enforced` | every seeded policy reads back what it enforces |
//! | `the_same_seed_replays_the_same_round` | the same seed produces the same trace twice |

#![cfg(feature = "script")]

use crate::sim;

use std::collections::{BTreeSet, VecDeque};
use std::error::Error;
use std::num::NonZeroU32;

use lgwks_bot::rt::tenancy::{
    Arrival, DeficitRoundRobin, Grant, GrantOutcome, MAX_QUEUE_PER_TENANT, MAX_TENANT_WEIGHT,
    MAX_TOTAL_QUEUE, TenancyError, TenancyPolicy, TryArrival,
};
use lgwks_bot::script::Tenant;

use sim::seed::Rng;

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

/// The one property a test asserts. The world is the same machinery for every
/// test; what differs is the shape that stresses the property and the single
/// check that may fail the run, so a failure names exactly one broken promise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Property {
    /// `in_flight_never_passes_the_tenant_ceiling`.
    Ceiling,
    /// `live_waiters_never_pass_the_tenant_queue_bound`.
    TenantQueue,
    /// `retained_waiters_never_pass_the_supervisor_bound`.
    SupervisorQueue,
    /// `permits_are_conserved_between_the_pool_and_the_round`.
    Conservation,
    /// `the_round_idles_only_when_no_live_waiter_has_room`.
    WorkConserving,
    /// `each_tenant_is_served_in_arrival_order`.
    Fifo,
    /// `every_live_waiter_is_served_exactly_once`.
    ExactlyOnce,
    /// `an_abandoned_waiter_is_never_granted`.
    NoDeadGrant,
    /// `a_contended_try_arrival_retains_nothing`.
    TryRetainsNothing,
    /// `an_immediate_admission_never_jumps_a_waiting_tenant`.
    NoJump,
    /// `a_refusal_names_the_bound_it_reached`.
    RefusalNamesBound,
    /// `a_drained_round_retains_nothing`.
    DrainedIsEmpty,
    /// `the_live_count_matches_the_owners_still_waiting` and
    /// `a_late_abandonment_leaves_the_counts_exact`.
    LiveCount,
    /// `an_empty_ring_means_the_round_is_idle`.
    EmptyRingIdles,
    /// `the_same_seed_replays_the_same_round`: every check, because a replay
    /// of a run that broke a promise is not a receipt worth keeping.
    Every,
}

/// How a world is drawn: the ranges its policy and acts come from.
///
/// Every range is inclusive, and every rate is per mille of acts (or of grants,
/// for the late abandonment).
#[derive(Clone, Copy, Debug)]
struct Shape {
    /// Tenants in the world.
    tenants: (u32, u32),
    /// The per-tenant ceiling.
    ceiling: (u32, u32),
    /// The per-tenant queue bound.
    queue: (u32, u32),
    /// The supervisor's waiting bound; `None` keeps the policy's default.
    total: Option<(u32, u32)>,
    /// Permits in the pool.
    pool: (u32, u32),
    /// Acts before the drain.
    acts: (u32, u32),
    /// Share of acts that are blocking arrivals.
    arrive: u32,
    /// Share of acts that are non-blocking arrivals.
    try_arrive: u32,
    /// Share of acts that are owners walking away while queued.
    abandon: u32,
    /// Chance, per grant, that its owner walks away before delivery.
    late: u32,
}

impl Shape {
    /// Every act in proportion, small bounds so every edge is reached often.
    const MIXED: Self = Self {
        tenants: (1, 6),
        ceiling: (1, 4),
        queue: (0, 6),
        total: Some((0, 24)),
        pool: (1, 10),
        acts: (100, 500),
        arrive: 450,
        try_arrive: 100,
        abandon: 120,
        late: 100,
    };

    /// A tight supervisor bound under generous tenant bounds, so the
    /// supervisor's bound is the one that binds.
    const SUPERVISOR_BOUND: Self = Self {
        queue: (4, 16),
        total: Some((0, 6)),
        ..Self::MIXED
    };

    /// Owners walk away between the round's choice and the delivery on half of
    /// all grants, with few ordinary abandonments to mask the count.
    const LATE: Self = Self {
        abandon: 30,
        late: 500,
        ..Self::MIXED
    };

    /// Non-blocking arrivals dominate, against a busy round.
    const TRY_HEAVY: Self = Self {
        arrive: 350,
        try_arrive: 350,
        ..Self::MIXED
    };
}

/// Run `property` over the whole seed space in `shape`, every seed twice.
fn sweep_all(shape: Shape, property: Property) -> TestResult {
    for band in sim::bands(sim::SEED_SPACE, sim::BANDS) {
        sim::assert_replays(band, |run| {
            let mut world = World::draw(run.rng(), shape)?;
            world.live(run, property)
        })?;
    }
    Ok(())
}

/// Fail the run with `what` unless `holds`, but only when `this` is the
/// property under test (or every property is).
fn ensure(
    property: Property,
    this: Property,
    holds: bool,
    what: impl FnOnce() -> String,
) -> TestResult {
    if holds || (property != this && property != Property::Every) {
        return Ok(());
    }
    let refusal: TestResult = Err(what().into());
    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "ensure: a property did not hold");
    refusal
}

/// A seeded index below `len`.
fn pick(rng: &mut Rng, len: usize) -> Result<usize, Box<dyn Error>> {
    let bound = u32::try_from(len)?;
    Ok(usize::try_from(rng.below(bound))?)
}

/// A seeded value in an inclusive range, as a `usize`.
fn draw(rng: &mut Rng, (low, high): (u32, u32)) -> Result<usize, Box<dyn Error>> {
    Ok(usize::try_from(rng.between(low, high))?)
}

/// One permit from the world's pool. A unit type of its own rather than `()`,
/// so a permit in flight is a value the reader can follow through the pump.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Permit;

/// One seeded world: the shipped scheduler, the pool it draws from, and the
/// reference model every count is checked against.
struct World {
    /// The scheduler under test, its waiters numbered by arrival.
    round: DeficitRoundRobin<u64>,
    /// The tenants, in the order the seed numbered them.
    tenants: Vec<Tenant>,
    /// The policy's ceiling, read back from the scheduler.
    ceiling: usize,
    /// The policy's tenant queue bound, read back.
    queue_bound: usize,
    /// The policy's supervisor bound, read back.
    total_bound: usize,
    /// Permits in the world, free or charged.
    pool_size: usize,
    /// Permits in the pool right now.
    pool: usize,
    /// One entry per charged admission: the index of its tenant.
    running: Vec<usize>,
    /// Per tenant, the waiters whose owners are still waiting, oldest first.
    waiting: Vec<VecDeque<u64>>,
    /// Every waiter whose owner walked away.
    dead: BTreeSet<u64>,
    /// Every waiter a grant reached.
    served: BTreeSet<u64>,
    /// The next waiter's number.
    next: u64,
    /// Acts before the drain.
    acts: usize,
    /// The world's shape.
    shape: Shape,
}

impl World {
    /// Draw a world's policy and tenants from `rng`.
    fn draw(rng: &mut Rng, shape: Shape) -> Result<Self, Box<dyn Error>> {
        let count = draw(rng, shape.tenants)?;
        let mut tenants = Vec::with_capacity(count);
        for index in 0..count {
            tenants.push(Tenant::new(&format!("tenant-{index}"))?);
        }
        let mut policy = TenancyPolicy::new(draw(rng, shape.ceiling)?, draw(rng, shape.queue)?);
        if let Some(total) = shape.total {
            policy = policy.with_queue_total(draw(rng, total)?);
        }
        for tenant in &tenants {
            if rng.chance(400) {
                let weight =
                    NonZeroU32::new(rng.between(1, 4)).ok_or("a weight of at least one")?;
                policy = policy.with_weight(tenant, weight)?;
            }
        }
        let pool_size = draw(rng, shape.pool)?;
        let round = DeficitRoundRobin::new(policy);
        Ok(Self {
            ceiling: round.policy().per_tenant_limit(),
            queue_bound: round.policy().queue_per_tenant(),
            total_bound: round.policy().queue_total(),
            round,
            waiting: vec![VecDeque::new(); count],
            tenants,
            pool_size,
            pool: pool_size,
            running: Vec::new(),
            dead: BTreeSet::new(),
            served: BTreeSet::new(),
            next: 0,
            acts: draw(rng, shape.acts)?,
            shape,
        })
    }

    /// The world's whole life: the seeded acts, then a drain in which every
    /// admission ends and nobody new arrives.
    fn live(&mut self, run: &mut sim::Sim, property: Property) -> TestResult {
        run.trace.record_number("tenants", self.tenants.len());
        run.trace.record_number("pool", self.pool_size);
        for _ in 0..self.acts {
            self.act(run, property)?;
        }
        while !self.running.is_empty() {
            self.complete(run, property)?;
            self.check_counts(property)?;
        }
        self.check_drained(run, property)
    }

    /// One seeded act, then every count checked against the model.
    fn act(&mut self, run: &mut sim::Sim, property: Property) -> TestResult {
        let roll = run.rng().below(1_000);
        let tenant = pick(run.rng(), self.tenants.len())?;
        let tries = self.shape.arrive.saturating_add(self.shape.try_arrive);
        let walks = tries.saturating_add(self.shape.abandon);
        let acted = if roll < self.shape.arrive {
            self.arrive(run, tenant, property)
        } else if roll < tries {
            self.try_arrive(run, tenant, property)
        } else if roll < walks {
            self.abandon(run, property)
        } else {
            self.complete(run, property)
        };
        acted?;
        self.check_counts(property)
    }

    /// Take one permit from the pool, as the shell's `take_from_pool` does.
    fn take(pool: &mut usize) -> Option<Permit> {
        let left = pool.checked_sub(1)?;
        *pool = left;
        Some(Permit)
    }

    /// A blocking arrival for `tenant`, pumping the round when it queues.
    fn arrive(&mut self, run: &mut sim::Sim, tenant: usize, property: Property) -> TestResult {
        let waiter = self.next;
        self.next = self.next.saturating_add(1);
        let anybody_waited = self.round.has_eligible();
        let live_before = self.live_of(tenant)?;
        let retained_before = self.round.retained_total();
        let name = self.name(tenant)?.clone();
        let pool = &mut self.pool;
        let outcome = self.round.arrive(&name, waiter, || Self::take(pool));
        match outcome {
            Arrival::Immediate(Permit) => self.admitted(run, tenant, anybody_waited, property),
            Arrival::Queued => {
                run.trace.record_number("queued", tenant);
                self.waiting_of(tenant)?.push_back(waiter);
                self.pump(run, None, property)
            }
            Arrival::Refused { limit } => {
                run.trace.record_number("refused", limit);
                let honest = limit == self.queue_bound && live_before >= limit;
                ensure(property, Property::RefusalNamesBound, honest, || {
                    format!(
                        "{name} was refused naming {limit} with {live_before} live waiters \
                         against a bound of {}",
                        self.queue_bound
                    )
                })
            }
            Arrival::SupervisorFull { limit } => {
                run.trace.record_number("supervisor-full", limit);
                let honest = limit == self.total_bound && retained_before >= limit;
                ensure(property, Property::RefusalNamesBound, honest, || {
                    format!(
                        "the supervisor refused naming {limit} while retaining \
                         {retained_before} against a bound of {}",
                        self.total_bound
                    )
                })
            }
            _ => self.unknown_arm("arrive"),
        }
    }

    /// An arrival through either door was admitted from the pool: nobody may
    /// have been waiting, and the admission is now running.
    fn admitted(
        &mut self,
        run: &mut sim::Sim,
        tenant: usize,
        anybody_waited: bool,
        property: Property,
    ) -> TestResult {
        run.trace.record_number("immediate", tenant);
        self.running.push(tenant);
        ensure(property, Property::NoJump, !anybody_waited, || {
            format!("tenant {tenant} was admitted from the pool while a tenant was waiting")
        })
    }

    /// A non-blocking arrival for `tenant`.
    fn try_arrive(&mut self, run: &mut sim::Sim, tenant: usize, property: Property) -> TestResult {
        let before = self.snapshot();
        let anybody_waited = self.round.has_eligible();
        let name = self.name(tenant)?.clone();
        let pool = &mut self.pool;
        let outcome = self.round.try_arrive(&name, || Self::take(pool));
        match outcome {
            TryArrival::Immediate(Permit) => self.admitted(run, tenant, anybody_waited, property),
            TryArrival::Contended => {
                run.trace.record_number("try-contended", tenant);
                let after = self.snapshot();
                ensure(
                    property,
                    Property::TryRetainsNothing,
                    before == after,
                    || {
                        format!(
                            "a contended try for {name} moved the round from {before:?} to {after:?}"
                        )
                    },
                )
            }
            _ => self.unknown_arm("try_arrive"),
        }
    }

    /// An owner still waiting walks away.
    fn abandon(&mut self, run: &mut sim::Sim, property: Property) -> TestResult {
        let holders: Vec<usize> = (0..self.waiting.len())
            .filter(|&index| {
                self.waiting
                    .get(index)
                    .is_some_and(|queue| !queue.is_empty())
            })
            .collect();
        if holders.is_empty() {
            return self.complete(run, property);
        }
        let tenant = *holders
            .get(pick(run.rng(), holders.len())?)
            .ok_or("a holder below the count")?;
        let queue = self.waiting_of(tenant)?;
        let position = pick(run.rng(), queue.len())?;
        let waiter = queue.remove(position).ok_or("a waiter below the length")?;
        self.dead.insert(waiter);
        run.trace.record_number("abandon", waiter);
        let name = self.name(tenant)?.clone();
        let dead = &self.dead;
        let mut is_live = |candidate: &u64| !dead.contains(candidate);
        self.round.note_abandoned(&name, &mut is_live);
        Ok(())
    }

    /// One charged admission ends: its release is noted and its permit pumped.
    fn complete(&mut self, run: &mut sim::Sim, property: Property) -> TestResult {
        if self.running.is_empty() {
            return Ok(());
        }
        let slot = pick(run.rng(), self.running.len())?;
        let tenant = self.running.swap_remove(slot);
        run.trace.record_number("complete", tenant);
        let name = self.name(tenant)?.clone();
        self.round.note_release(&name);
        self.pump(run, Some(Permit), property)
    }

    /// The shell's pump: hand permits to eligible tenants until none can take
    /// one.
    fn pump(
        &mut self,
        run: &mut sim::Sim,
        spare: Option<Permit>,
        property: Property,
    ) -> TestResult {
        let mut spare = spare;
        loop {
            let permit = match spare.take() {
                Some(permit) => permit,
                None if self.round.has_eligible() => match Self::take(&mut self.pool) {
                    Some(permit) => permit,
                    None => return Ok(()),
                },
                None => return Ok(()),
            };
            let eligible = self.round.has_eligible();
            let dead = &self.dead;
            let mut is_live = |candidate: &u64| !dead.contains(candidate);
            let outcome = self.round.grant(permit, &mut is_live);
            spare = match outcome {
                GrantOutcome::Idle(Permit) => return self.idled(run, property),
                GrantOutcome::Granted(grant) => self.granted(run, grant, eligible, property)?,
                _ => return self.unknown_arm("grant"),
            };
        }
    }

    /// The round handed its permit back: it returns to the pool, and no live
    /// waiter may have had room.
    fn idled(&mut self, run: &mut sim::Sim, property: Property) -> TestResult {
        run.trace.record("idle");
        self.pool = self.pool.saturating_add(1);
        self.check_idle(property)
    }

    /// The round chose a waiter. Returns the permit when the delivery fails.
    fn granted(
        &mut self,
        run: &mut sim::Sim,
        grant: Grant<u64, Permit>,
        eligible: bool,
        property: Property,
    ) -> Result<Option<Permit>, Box<dyn Error>> {
        ensure(property, Property::EmptyRingIdles, eligible, || {
            format!("{} was granted while the ring was empty", grant.tenant)
        })?;
        let tenant = self.index_of(&grant.tenant)?;
        run.trace.record_number("grant", grant.waiter);
        self.check_grant(tenant, grant.waiter, property)?;
        if !run.rng().chance(self.shape.late) {
            self.served.insert(grant.waiter);
            self.running.push(tenant);
            return Ok(None);
        }
        // The owner left after the liveness check: the delivery fails, the
        // admission is charged back and the permit recycles into this same
        // pump. The owner's own report finds its slot withdrawn and reports
        // nothing.
        run.trace.record_number("late", grant.waiter);
        self.dead.insert(grant.waiter);
        self.round.note_release(&grant.tenant);
        Ok(Some(grant.permit))
    }

    /// A grant reached `waiter` of `tenant`: it must be live, the oldest live
    /// waiter of its tenant, granted once, under the ceiling.
    fn check_grant(&mut self, tenant: usize, waiter: u64, property: Property) -> TestResult {
        ensure(
            property,
            Property::NoDeadGrant,
            !self.dead.contains(&waiter),
            || format!("waiter {waiter} was granted after its owner left"),
        )?;
        ensure(
            property,
            Property::ExactlyOnce,
            !self.served.contains(&waiter),
            || format!("waiter {waiter} was granted twice"),
        )?;
        let oldest = self.waiting_of(tenant)?.pop_front();
        ensure(property, Property::Fifo, oldest == Some(waiter), || {
            format!("tenant {tenant} was granted waiter {waiter} while {oldest:?} was older")
        })?;
        let charged = self.round.in_flight_of(self.name(tenant)?);
        ensure(property, Property::Ceiling, charged <= self.ceiling, || {
            format!(
                "tenant {tenant} holds {charged} admissions past a ceiling of {}",
                self.ceiling
            )
        })
    }

    /// The round handed a permit back: no live waiter may have room.
    fn check_idle(&self, property: Property) -> TestResult {
        for (index, queue) in self.waiting.iter().enumerate() {
            let charged = self.round.in_flight_of(self.name(index)?);
            ensure(
                property,
                Property::WorkConserving,
                queue.is_empty() || charged >= self.ceiling,
                || {
                    format!(
                        "the round idled while tenant {index} had {} live waiters and {charged} \
                         of {} admissions",
                        queue.len(),
                        self.ceiling
                    )
                },
            )?;
        }
        Ok(())
    }

    /// After every act: the bounds, the conservation and the exact counts.
    fn check_counts(&self, property: Property) -> TestResult {
        let charged_total: usize = self
            .tenants
            .iter()
            .map(|tenant| self.round.in_flight_of(tenant))
            .sum();
        ensure(
            property,
            Property::Conservation,
            self.pool.saturating_add(self.running.len()) == self.pool_size
                && charged_total == self.running.len(),
            || {
                format!(
                    "{} free and {} running of {} permits, the round charging {charged_total}",
                    self.pool,
                    self.running.len(),
                    self.pool_size
                )
            },
        )?;
        let retained = self.round.retained_total();
        ensure(
            property,
            Property::SupervisorQueue,
            retained <= self.total_bound,
            || {
                format!(
                    "the round retains {retained} past a bound of {}",
                    self.total_bound
                )
            },
        )?;
        for index in 0..self.tenants.len() {
            self.check_tenant(index, property)?;
        }
        Ok(())
    }

    /// Tenant `index`'s ceiling, queue bound and exact live count.
    fn check_tenant(&self, index: usize, property: Property) -> TestResult {
        let tenant = self.name(index)?;
        let charged = self.round.in_flight_of(tenant);
        ensure(property, Property::Ceiling, charged <= self.ceiling, || {
            format!(
                "{tenant} holds {charged} admissions past a ceiling of {}",
                self.ceiling
            )
        })?;
        let reported = self.round.queued_of(tenant);
        let waiting = self.live_of(index)?;
        ensure(
            property,
            Property::TenantQueue,
            waiting <= self.queue_bound,
            || {
                format!(
                    "{tenant} holds {waiting} live waiters past a bound of {}",
                    self.queue_bound
                )
            },
        )?;
        ensure(property, Property::LiveCount, reported == waiting, || {
            format!("{tenant} reports {reported} waiting while {waiting} owners still wait")
        })
    }

    /// After the drain: every owner who stayed was served, nothing is retained.
    fn check_drained(&self, run: &mut sim::Sim, property: Property) -> TestResult {
        run.trace.record_number("served", self.served.len());
        run.trace.record_number("dead", self.dead.len());
        let unserved: usize = self.waiting.iter().map(VecDeque::len).sum();
        ensure(property, Property::ExactlyOnce, unserved == 0, || {
            format!("{unserved} owners were still waiting after every admission ended")
        })?;
        let retained = self.round.retained_total();
        let queues = self.round.tenants_with_queues();
        ensure(
            property,
            Property::DrainedIsEmpty,
            retained == 0 && queues == 0 && self.pool == self.pool_size,
            || {
                format!(
                    "a drained round retains {retained} waiters across {queues} tenants with \
                     {} of {} permits free",
                    self.pool, self.pool_size
                )
            },
        )
    }

    /// The counts a refused non-blocking arrival must leave alone.
    fn snapshot(&self) -> (usize, usize, usize, Vec<usize>) {
        let charged = self
            .tenants
            .iter()
            .map(|tenant| self.round.in_flight_of(tenant))
            .collect();
        (
            self.round.retained_total(),
            self.round.tenants_with_queues(),
            self.pool,
            charged,
        )
    }

    /// Tenant `index`'s name.
    fn name(&self, index: usize) -> Result<&Tenant, Box<dyn Error>> {
        Ok(self.tenants.get(index).ok_or("a tenant below the count")?)
    }

    /// The index of `tenant`.
    fn index_of(&self, tenant: &Tenant) -> Result<usize, Box<dyn Error>> {
        Ok(self
            .tenants
            .iter()
            .position(|candidate| candidate == tenant)
            .ok_or("a granted tenant this world created")?)
    }

    /// How many of tenant `index`'s owners still wait.
    fn live_of(&self, index: usize) -> Result<usize, Box<dyn Error>> {
        Ok(self
            .waiting
            .get(index)
            .ok_or("a tenant below the count")?
            .len())
    }

    /// Tenant `index`'s waiting owners.
    fn waiting_of(&mut self, index: usize) -> Result<&mut VecDeque<u64>, Box<dyn Error>> {
        Ok(self
            .waiting
            .get_mut(index)
            .ok_or("a tenant below the count")?)
    }

    /// A non-exhaustive enum grew an arm this model does not know.
    fn unknown_arm(&self, call: &str) -> TestResult {
        let refusal: TestResult =
            Err(format!("{call} answered an arm this model does not know").into());
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "unknown_arm: returning an error to the caller");
        refusal
    }
}

/// No tenant is ever charged more admissions than its ceiling.
#[test]
fn in_flight_never_passes_the_tenant_ceiling() -> TestResult {
    sweep_all(Shape::MIXED, Property::Ceiling)
}

/// No tenant ever holds more live waiters than its queue bound.
#[test]
fn live_waiters_never_pass_the_tenant_queue_bound() -> TestResult {
    sweep_all(Shape::MIXED, Property::TenantQueue)
}

/// The round never retains more waiters than the supervisor's bound, abandoned
/// ones included, under a supervisor bound tighter than any tenant's.
#[test]
fn retained_waiters_never_pass_the_supervisor_bound() -> TestResult {
    sweep_all(Shape::SUPERVISOR_BOUND, Property::SupervisorQueue)
}

/// Every permit is in the pool or charged to exactly one running admission,
/// and the round's charges agree with the admissions that are running.
#[test]
fn permits_are_conserved_between_the_pool_and_the_round() -> TestResult {
    sweep_all(Shape::LATE, Property::Conservation)
}

/// A permit is handed back to the pool only when no live waiter is under its
/// tenant's ceiling: the round never idles with servable work.
#[test]
fn the_round_idles_only_when_no_live_waiter_has_room() -> TestResult {
    sweep_all(Shape::MIXED, Property::WorkConserving)
}

/// A tenant's live waiters are granted oldest first, abandoned ones skipped.
#[test]
fn each_tenant_is_served_in_arrival_order() -> TestResult {
    sweep_all(Shape::MIXED, Property::Fifo)
}

/// Once every admission has ended, every waiter whose owner stayed was granted
/// exactly once.
#[test]
fn every_live_waiter_is_served_exactly_once() -> TestResult {
    sweep_all(Shape::MIXED, Property::ExactlyOnce)
}

/// No grant ever names a waiter whose owner left before the round chose it.
#[test]
fn an_abandoned_waiter_is_never_granted() -> TestResult {
    sweep_all(Shape::MIXED, Property::NoDeadGrant)
}

/// A non-blocking arrival that is refused changes no count: no queue slot, no
/// tenant entry, no permit.
#[test]
fn a_contended_try_arrival_retains_nothing() -> TestResult {
    sweep_all(Shape::TRY_HEAVY, Property::TryRetainsNothing)
}

/// Nobody, through either door, is admitted from the pool while another
/// tenant is waiting.
#[test]
fn an_immediate_admission_never_jumps_a_waiting_tenant() -> TestResult {
    sweep_all(Shape::TRY_HEAVY, Property::NoJump)
}

/// Each refusal arm fires only when its bound is reached, and names the bound
/// the policy declared.
#[test]
fn a_refusal_names_the_bound_it_reached() -> TestResult {
    sweep_all(Shape::SUPERVISOR_BOUND, Property::RefusalNamesBound)?;
    sweep_all(Shape::MIXED, Property::RefusalNamesBound)
}

/// Once every admission has ended, the round retains no waiter and no tenant
/// with a queue, and every permit is back in the pool.
#[test]
fn a_drained_round_retains_nothing() -> TestResult {
    sweep_all(Shape::LATE, Property::DrainedIsEmpty)
}

/// `queued_of` is exactly the number of a tenant's owners still waiting, after
/// every act.
#[test]
fn the_live_count_matches_the_owners_still_waiting() -> TestResult {
    sweep_all(Shape::MIXED, Property::LiveCount)
}

/// An owner walking away between the round's choice and the delivery leaves
/// every tenant's live count exact, whether or not the tenant had other
/// abandoned waiters at the time.
#[test]
fn a_late_abandonment_leaves_the_counts_exact() -> TestResult {
    sweep_all(Shape::LATE, Property::LiveCount)
}

/// When `has_eligible` answers false, the grant it guards hands the permit
/// back rather than reaching a waiter.
#[test]
fn an_empty_ring_means_the_round_is_idle() -> TestResult {
    sweep_all(Shape::MIXED, Property::EmptyRingIdles)
}

/// The same seed produces the same trace twice, with every property checked.
#[test]
fn the_same_seed_replays_the_same_round() -> TestResult {
    sweep_all(Shape::LATE, Property::Every)?;
    sweep_all(Shape::MIXED, Property::Every)
}

/// Every seeded policy reads back the bounds and weights it enforces: a zero
/// ceiling reads as one, oversized bounds clamp to their ceilings, a weight
/// past its bound is refused naming both numbers, and an unnamed tenant
/// weighs one.
#[test]
fn declared_bounds_and_weights_read_back_as_enforced() -> TestResult {
    for band in sim::bands(sim::SEED_SPACE, sim::BANDS) {
        sim::assert_replays(band, |run| {
            let ceiling = usize::try_from(run.rng().below(64))?;
            let queue = usize::try_from(run.rng().below(u32::MAX))?;
            let total = usize::try_from(run.rng().below(u32::MAX))?;
            let policy = TenancyPolicy::new(ceiling, queue).with_queue_total(total);
            run.trace
                .record_number("ceiling", policy.per_tenant_limit());
            run.trace.record_number("queue", policy.queue_per_tenant());
            run.trace.record_number("total", policy.queue_total());
            let read_back = policy.per_tenant_limit() == ceiling.max(1)
                && policy.queue_per_tenant() == queue.min(MAX_QUEUE_PER_TENANT)
                && policy.queue_total() == total.min(MAX_TOTAL_QUEUE);
            ensure(Property::Every, Property::Every, read_back, || {
                format!("declared ({ceiling}, {queue}, {total}) did not read back as enforced")
            })?;
            let heavy = Tenant::new("heavy")?;
            let weight = run.rng().between(1, MAX_TENANT_WEIGHT.saturating_mul(2));
            let declared = NonZeroU32::new(weight).ok_or("a weight of at least one")?;
            let outcome = policy.with_weight(&heavy, declared);
            run.trace.record_number("weight", weight);
            let honest = match outcome {
                Ok(weighted) => {
                    weight <= MAX_TENANT_WEIGHT
                        && weighted.weight_of(&heavy) == declared
                        && weighted.weight_of(&Tenant::new("light")?) == NonZeroU32::MIN
                }
                Err(TenancyError::WeightTooHigh {
                    weight: named,
                    limit,
                }) => weight > MAX_TENANT_WEIGHT && named == weight && limit == MAX_TENANT_WEIGHT,
                Err(_) => false,
            };
            ensure(Property::Every, Property::Every, honest, || {
                format!("a weight of {weight} was not answered as the policy declares")
            })
        })?;
    }
    Ok(())
}
