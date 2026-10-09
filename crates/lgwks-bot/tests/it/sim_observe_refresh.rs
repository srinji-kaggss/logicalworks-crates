//! Simulation family: the observation layer under a seeded fault schedule.
//!
//! The non-simulation file `observe_refresh.rs` walks each declared failure once
//! and says exactly what it observed. This file is the other half: one seed
//! decides a whole *run* — which of the four declared causes each tenant's
//! sources hit, when, for how long, and whether the poll behind it also refuses
//! — and the run is swept across bands and replayed.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `forced_refresh_matches_the_schedule` | every armed cause reaches the report, naming that chain and that cause, and no unarmed chain does — a bijection rather than a count |
//! | `a_refresh_that_never_lands_stays_marked` | a chain whose every forced poll refuses is forced again on the next tick, reports the same cause, and does not drag its healthy sibling in |
//! | `tenants_never_cross` | two tenants over one process keep separate reports, separate marks and separate action logs |
//! | `event_identities_are_per_event` | a seeded stream of event ids over one fixed payload: each id fires on its first delivery and never again, including an older id redelivered |
//! | `the_same_seed_replays` | the same seed produces the same trace hash, twice |
//! | `a_wedged_source_is_reported_and_costs_its_neighbours_nothing` | a seeded mix of fast and never-resolving sources: every cancellation is reported once, by chain and by the source's own domain, and a chain that never stalled does byte-identical work to the same schedule run with nothing stalled |
//! | `a_saturated_wave_stalls_every_chain_and_still_lets_the_next_tenant_commit` | every chain in a fully-wedged wave is reported, while the tenant beside it reports none and keeps committing |
//! | `the_same_seed_replays_a_stalled_wave` | the stall families replay to the same trace hash, twice |
//!
//! # What the band families fix, and why
//!
//! The band families fix the chain count deliberately: their axis is *which*
//! chain declares what fault over which window, and a chain-count draw would
//! renumber every band for no added evidence. The 100 / 1,000 / 10,000 tiers
//! are here, but not in the bands — the families below the `band_family!`
//! declaration make the width itself a draw, building a bot from one source
//! type every chain shares so the builder can be extended in a loop. The bands
//! pin the fault dimension at a fixed width; the width families pin the width,
//! up to the 10,000-chain tier, and each records what it reached.
//!
//! # What is real and what is seeded
//!
//! The bot, the `bevy_ecs` schedule, the journal, the broker and the whole
//! ledger are the shipped ones, driven through the public `Observe`/`Execute`
//! traits. Seeded is only the *schedule*: which tenant's source answers on which
//! tick, with what value, and which declares a cache failure for how long. No
//! wall clock decides anything — the per-poll deadline is the substrate's own,
//! declared through the same builder a production caller uses — and the trace
//! records *which* chains were cancelled rather than when, so a run replays
//! exactly and the hash is a receipt rather than a measurement.
//!
//! The band sweep comes from `sim::band_of`, the shared seed space every other
//! `sim_*` family draws from, so a seed recorded against this file means the
//! same draw sequence as a seed recorded against `sim_task`.

use crate::sim;

// The band-declaration macro, defined once for the whole layer.
use crate::band_family;

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::future::Future;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use lgwks_bot::broker::Broker;
use lgwks_bot::effect::{EnvironmentId, EventId, FlowRevision, RunId};
use lgwks_bot::journal::MemoryJournal;
use lgwks_bot::spec::{BotBuilder, EffectEvidence, EffectIdentity, EffectScope};
use lgwks_bot::{
    Auth, Bot, BotError, Cap, DEFAULT_POLL_DEADLINE, DispatchCertainty, EffectLifetime, Execute,
    GrantSet, MAX_POLL_DEADLINE, Observe, RefreshReason,
};

use sim::Band;
use sim::Rng;

use crate::poll;

use poll::admit_poll;

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

// ── The schedule one seed decided ──────────────────────────────────────────

/// Every cause a source may declare, in the order the schedule draws from them.
///
/// A fixed list rather than an enum index so a failure names the cause in prose
/// and the trace records the same spelling `RefreshReason::as_str` uses. The
/// order is the draw order, so changing it would renumber every other family —
/// which is the reason the structural draws are cut first (see [`plan`]).
const CAUSES: [RefreshReason; 4] = [
    RefreshReason::Disconnected,
    RefreshReason::WatchOverflow,
    RefreshReason::StaleRemoteKey,
    RefreshReason::InvalidationFailed,
];

/// What one seed decided one tenant's sources would do.
#[derive(Clone, Copy, Debug)]
struct TenantPlan {
    /// Per chain, which cause its source declares, and for how many ticks.
    ///
    /// Sized to [`MAX_CHAINS`] because the bot is, and a longer table would be an
    /// index a scenario could reach for a chain that does not exist.
    faults: [FaultWindow; MAX_CHAINS],
    /// Whether the poll behind a declaration also refuses while it is armed.
    refusing: bool,
    /// Whether the tenant's action ever settles.
    holds: bool,
}

/// One chain's declared fault and its duration.
#[derive(Clone, Copy, Debug)]
struct FaultWindow {
    /// The cause the source declares, or `None` for a healthy chain.
    cause: Option<RefreshReason>,
    /// The tick it is armed on.
    from: u32,
    /// How many ticks it stays armed for.
    ticks: u32,
}

/// The whole run one seed decided.
#[derive(Clone, Copy, Debug)]
struct Plan {
    /// How many tenants share the process, at least two.
    tenants: u32,
    /// How many ticks the run drives.
    ticks: u32,
    /// Per tenant, its fault schedule.
    tenant: [TenantPlan; 2],
}

impl Plan {
    /// `tenant`'s schedule, or a healthy one past the end of the table.
    ///
    /// The table is fixed at two because the two-tenant isolation claim is what
    /// this family exists to prove, and a third tenant would add no evidence to
    /// it: the property is that two tenants do not cross, and a schedule that
    /// can hold three would prove the same thing about a different pair.
    /// The plan for the tenant at `index`, or `None` when the schedule declares
    /// no such tenant.
    ///
    /// `None` rather than a healthy plan: a row that names a tenant this schedule
    /// does not declare has no plan at all, and standing in a healthy one would
    /// measure a tenant nobody armed.
    fn tenant(&self, index: usize) -> Option<TenantPlan> {
        self.tenant.get(index).copied()
    }

    /// The schedule for tenant `index`, or a refusal: a plan draws one schedule
    /// per tenant it declares, so an index past them is a plan defect.
    fn schedule(&self, index: usize) -> Result<TenantPlan, Box<dyn Error>> {
        self.tenant(index)
            .ok_or_else(|| format!("a schedule that declares no tenant at index {index}").into())
    }
}

impl TenantPlan {
    /// A tenant whose sources are all healthy and whose action always lands.
    fn healthy() -> Self {
        Self {
            faults: [FaultWindow {
                cause: None,
                from: 0,
                ticks: 0,
            }; MAX_CHAINS],
            refusing: false,
            holds: false,
        }
    }
}

/// A plan drawn from `rng`.
///
/// The structural draws come first and in a fixed order — tenants, then ticks —
/// so that changing how a *cause* is chosen cannot renumber the tenants or the
/// ticks in another family. A schedule that shifted wholesale would make a
/// regression in the fault dimension unbisectable against one in the shape.
///
/// The chain count is not drawn: it is [`MAX_CHAINS`], and which of those chains
/// is armed is what varies.
fn plan(rng: &mut Rng) -> Plan {
    let tenants = rng.between(2, 2);
    let ticks = rng.between(3, 8);
    let mut tenant = [TenantPlan::healthy(); 2];
    for slot in &mut tenant {
        let mut faults = [FaultWindow {
            cause: None,
            from: 0,
            ticks: 0,
        }; MAX_CHAINS];
        for window in &mut faults {
            // Roughly half the chains are armed. A run in which every chain is
            // faulted proves nothing about the difference between a faulted and
            // an unfaulted chain, and the healthy ones are what make the
            // "nothing else is forced" half of the claim checkable.
            let armed = rng.chance(500);
            *window = FaultWindow {
                cause: match armed {
                    // The table is fixed at four, so the draw is in range by the
                    // generator's own bound; a host whose address space is
                    // narrower than the draw has no cause to arm, and arming the
                    // first one would test the wrong fault.
                    true => match usize::try_from(rng.below(4)) {
                        Ok(draw) => CAUSES.get(draw).copied(),
                        Err(_too_wide) => None,
                    },
                    false => None,
                },
                from: rng.between(1, ticks.max(2)),
                ticks: rng.between(1, 3),
            };
        }
        *slot = TenantPlan {
            faults,
            // Both switches are drawn often: a run where no source ever refuses
            // and no action ever holds exercises neither of the two properties
            // this layer is about.
            refusing: rng.chance(400),
            holds: rng.chance(400),
        };
    }
    Plan {
        tenants,
        ticks,
        tenant,
    }
}

// ── The rig: one source, one action, per tenant ────────────────────────────

/// The effect scope a tenant's bot runs under.
///
/// One scope per tenant because a scope is a run identity, an environment fence
/// and a journal, and `assemble` refuses a record naming an action this bot does
/// not declare — so two tenants cannot share one without the second build
/// refusing. That refusal is the reason tenants get their own identity, and it
/// is the first line of their isolation.
fn tenant_scope(tenant: u32) -> Result<EffectScope, Box<dyn Error>> {
    // The identity differs per tenant in the run's last byte and nowhere else,
    // so a cross-tenant effect would have to reuse another tenant's exact key to
    // slip past, and the environment is deliberately the *same* for both: the
    // fence that would catch it is the journal's, not the environment's.
    let run_hex = format!("0102030405060708090a0b0c0d0e0f{tenant:02x}");
    let environment = EnvironmentId::from_hex("2122232425262728292a2b2c2d2e2f30")?;
    let mut broker = Broker::new();
    broker.register(environment)?;
    Ok(EffectScope::new(
        EffectIdentity::new(
            RunId::from_hex(&run_hex)?,
            environment,
            FlowRevision::from_tagged(
                "blake3_256",
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            )?,
        ),
        broker,
        Box::new(MemoryJournal::new()),
    ))
}

/// One row of a tenant's tick report, in the form the trace and the assertions
/// both read.
struct ReportRow {
    /// Which tenant the report belongs to.
    tenant: u32,
    /// Which chain of that tenant the row is about.
    chain: usize,
    /// `forced` or `superseded`.
    kind: &'static str,
    /// The cause, or the superseded revision.
    detail: String,
}

impl ReportRow {
    /// The row as the trace spells it, which is also the sort order's key.
    fn spell(&self) -> String {
        format!(
            "{} {} {} {}",
            self.tenant, self.chain, self.kind, self.detail
        )
    }
}

/// One tenant's sources, their action, and what the run recorded.
///
/// The state a scenario reads. Every field is a counter or a log rather than a
/// flag, because "the report was non-empty" and "the report named chain 2 for
/// the right reason" are different assertions and a boolean cannot tell them
/// apart.
struct Tenant {
    /// The bot under test.
    bot: Bot,
    /// Per chain, the cells its source is built over.
    handles: Vec<ChainHandles>,
    /// Every value the action ran with, in order, with the chain that ran it.
    ran: Rc<RefCell<Vec<(usize, u32)>>>,
    /// The tenant number, for the bot's name and its journal identity.
    tenant: u32,
}

/// How many chains a tenant's bot declares.
///
/// A constant rather than a draw: see [`tenant`]. Three is enough for every
/// claim this file makes — a healthy chain, a faulted one and a sibling of each
/// — and a fourth would only widen the table the schedule indexes into.
const MAX_CHAINS: usize = 3;

impl Tenant {
    /// Run one tick and report what the observation phase saw into `out`.
    ///
    /// The tick's own `Result` is not the subject: a tenant whose action holds
    /// reports a typed hold, and a tenant whose source refuses reports a typed
    /// refusal, and both are ordinary outcomes of the schedule. What is read is
    /// [`Bot::tick_report`], which is published whether the tick returned `Ok`
    /// or `Err` — which is the property a failed tick has to keep.
    fn observe(&mut self, out: &mut Vec<ReportRow>) {
        let _outcome = self.bot.tick();
        let report = self.bot.tick_report();
        for forced in report.forced() {
            out.push(ReportRow {
                tenant: self.tenant,
                chain: forced.chain(),
                kind: "forced",
                detail: forced.reason().as_str().to_owned(),
            });
        }
        for passed in report.superseded() {
            out.push(ReportRow {
                tenant: self.tenant,
                chain: passed.chain(),
                kind: "superseded",
                detail: passed.revision().to_string(),
            });
        }
    }

    /// Arm every chain whose window covers `tick`, and disarm the rest.
    ///
    /// Armed from the schedule rather than by a random draw per tick, so the same
    /// seed produces the same fault *pattern* and not merely the same count of
    /// faults — which is what makes a trace hash comparable across two runs.
    fn arm(&self, tick: u32, plan: &TenantPlan) {
        for (index, window) in plan.faults.iter().enumerate() {
            let Some(held) = self.handles.get(index) else {
                continue;
            };
            let armed = window.cause.is_some_and(|_| {
                tick >= window.from && tick < window.from.saturating_add(window.ticks)
            });
            held.declared.set(if armed { window.cause } else { None });
            held.refusing.set(armed && plan.refusing);
        }
    }

    /// Move every chain's value to `tick`, so each tick commits a new one.
    fn advance(&self, tick: u32) {
        for held in &self.handles {
            held.value.set(tick);
        }
    }
}

/// A source whose value, declaration and refusal are the test's.
struct Armed {
    /// The value this poll reports.
    value: Rc<Cell<u32>>,
    /// What `cache_state` answers.
    declared: Rc<Cell<Option<RefreshReason>>>,
    /// Whether this poll refuses instead of reading.
    refusing: Rc<Cell<bool>>,
    /// How many times the body ran.
    polls: Rc<Cell<u32>>,
    /// The chain's index, for the domain identity a report names it by.
    chain: usize,
    /// The tenant this source belongs to, for the same reason.
    tenant: u32,
}

/// The refusal a seeded source answers with while its flag is set.
///
/// One construction for the sources that refuse on a seed, so two families
/// cannot name a different domain for the same refusal.
fn seeded_refusal(tenant: impl std::fmt::Display, chain: impl std::fmt::Display) -> BotError {
    BotError::DomainError {
        domain: format!("sim::t{tenant}-c{chain}"),
        certainty: DispatchCertainty::NotDelivered,
        cause: "the seeded source refused".to_owned(),
    }
}

impl Observe for Armed {
    type Output = u32;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        admit_poll(&call.0, Observe::required_caps(self), &self.polls)?;
        if self.refusing.get() {
            Err(seeded_refusal(self.tenant, self.chain))
        } else {
            Ok(self.value.get())
        }
    }

    fn cache_state(&self) -> Option<RefreshReason> {
        self.declared.get()
    }

    fn domain_id(&self) -> &str {
        // Stable per run and distinct per chain, which is what makes a report's
        // `domain()` readable: a caller triaging two forced refreshes has to be
        // able to tell which source each came from.
        domain_of(self.tenant, self.chain)
    }
}

/// The domain identity each tenant's chains report under.
///
/// A fixed table rather than a formatted string so the identity is `'static` and
/// the report carries the same pointer a reader can compare across two runs.
/// How many bits of a chain index name a domain: one row of [`DOMAINS`] is this
/// many entries wide, so the wrap is a mask rather than a remainder and the width
/// the mask names is the width the table declares.
const DOMAIN_ROW_BITS: u32 = 3;

/// The mask that wraps a chain index into one row of [`DOMAINS`].
const DOMAIN_ROW_MASK: usize = (1 << DOMAIN_ROW_BITS) - 1;

/// The domain a tenant/chain pair outside [`DOMAINS`] reports under.
///
/// Named rather than indexed past the table: a `domain()` is what a caller
/// triages by, and one that pointed into another tenant's row would blame the
/// wrong source — the one thing this family exists to rule out.
const UNMAPPED_DOMAIN: &str = "test::unmapped";

/// The domain a tenant/chain pair reports under.
fn domain_of(tenant: u32, chain: usize) -> &'static str {
    let row = match usize::try_from(tenant) {
        Ok(row) => row,
        Err(_too_wide) => return UNMAPPED_DOMAIN,
    };
    match DOMAINS
        .get(row)
        .and_then(|names| names.get(chain & DOMAIN_ROW_MASK))
    {
        Some(name) => name,
        None => UNMAPPED_DOMAIN,
    }
}

const DOMAINS: [[&str; 1 << DOMAIN_ROW_BITS]; 2] = [
    [
        "sim::t0c0",
        "sim::t0c1",
        "sim::t0c2",
        "sim::t0c3",
        "sim::t0c4",
        "sim::t0c5",
        "sim::t0c6",
        "sim::t0c7",
    ],
    [
        "sim::t1c0",
        "sim::t1c1",
        "sim::t1c2",
        "sim::t1c3",
        "sim::t1c4",
        "sim::t1c5",
        "sim::t1c6",
        "sim::t1c7",
    ],
];

/// An action that lands and records the chain it ran under, or holds.
struct Logs {
    /// Every value the action ran with, with the chain that ran it.
    ran: Rc<RefCell<Vec<(usize, u32)>>>,
    /// The chain this action serves.
    chain: usize,
    /// Whether the outcome is reported as unknown.
    holds: Rc<Cell<bool>>,
}

impl Execute for Logs {
    type Input = u32;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
        call.0.check(Execute::required_caps(self))?;
        self.ran.borrow_mut().push((self.chain, *call.1));
        if self.holds.get() {
            Err(BotError::EffectIndeterminate {
                domain: "sim::logs".to_owned(),
                cause: "the seeded hold".to_owned(),
            })
        } else {
            Ok(())
        }
    }

    fn domain_id(&self) -> &str {
        "sim::logs"
    }
}

/// An event source: the next event id the seed queued over one payload.
struct Feed {
    /// The events still to be delivered, in order.
    queue: Rc<RefCell<Vec<EventId<u32>>>>,
    /// How many times the body ran.
    polls: Rc<Cell<u32>>,
}

impl Observe for Feed {
    type Output = EventId<u32>;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<EventId<u32>, BotError> {
        admit_poll(&call.0, Observe::required_caps(self), &self.polls)?;
        self.queue.borrow_mut().pop().ok_or(BotError::DomainError {
            domain: "sim::feed".to_owned(),
            certainty: DispatchCertainty::NotDelivered,
            cause: "the seeded feed is drained".to_owned(),
        })
    }

    fn domain_id(&self) -> &str {
        "sim::feed"
    }
}

/// An action over an event, recording the ids that ran.
struct LogsEvents {
    /// Every event id this action ran with, in order.
    ran: Rc<RefCell<Vec<u64>>>,
}

impl Execute for LogsEvents {
    type Input = EventId<u32>;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &EventId<u32>)) -> Result<(), BotError> {
        call.0.check(Execute::required_caps(self))?;
        self.ran.borrow_mut().push(call.1.id());
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "sim::logs_events"
    }
}

/// Whether a chain's entry fires: the source reported a positive value.
fn answered(observed: &u32) -> bool {
    *observed > 0
}

/// Declare [`MAX_CHAINS`] chains on `builder`, one per source, each firing
/// [`logs`] into `ran` under its own index, and build the tenant's bot.
///
/// The three chains are written out rather than looped because each `observe`
/// returns a builder whose type names its source (see [`tenant`]); what this
/// shares is everything *but* the source, so [`tenant`] and [`paced_tenant`]
/// declare the same entries, actions and scope and differ only in what they
/// observe.
fn three_chains<S>(
    builder: BotBuilder,
    sources: [S; MAX_CHAINS],
    ran: &Rc<RefCell<Vec<(usize, u32)>>>,
    holds: &Rc<Cell<bool>>,
    tenant: u32,
) -> Result<Bot, Box<dyn Error>>
where
    S: Observe<Output = u32> + 'static,
{
    let [first, second, third] = sources;
    Ok(builder
        .observe(first)
        .on(answered, logs(ran, 0, holds))
        .observe(second)
        .on(answered, logs(ran, 1, holds))
        .observe(third)
        .on(answered, logs(ran, 2, holds))
        .with_effects(tenant_scope(tenant)?)
        .build(&GrantSet::empty())?)
}

/// Tenant `index` of `plan`, built from its own schedule, with the id it runs as.
fn tenant_of(plan: &Plan, index: usize) -> Result<(u32, Tenant), Box<dyn Error>> {
    let id = u32::try_from(index)?;
    let schedule = plan.schedule(index)?;
    let subject = tenant(id, &schedule)?;
    Ok((id, subject))
}

/// Build one tenant's bot over exactly [`MAX_CHAINS`] chains.
///
/// Written out rather than looped, because `Bot::observe` returns a builder of a
/// *different* type per source — `ObserveBuilder<S>` names `S` — so a loop's
/// accumulator changes type on every iteration and no fold can hold it either.
/// That is the erasure boundary the chain builder exists to make impossible to
/// cross at a call site, and a test wanting a runtime chain count would be
/// asking for the boundary to be crossable.
///
/// The chain count is therefore a constant rather than a draw. The *fault*
/// dimension still varies per seed — which chain is armed, with what cause, over
/// what window — and that is the dimension these families are about. A
/// chain-count draw would renumber every other family for no added evidence.
fn tenant(tenant: u32, schedule: &TenantPlan) -> Result<Tenant, Box<dyn Error>> {
    let ran = Rc::new(RefCell::new(Vec::new()));
    let holds = Rc::new(Cell::new(schedule.holds));

    let (first, second, third) = (
        ChainHandles::new(),
        ChainHandles::new(),
        ChainHandles::new(),
    );
    let armed = |held: &ChainHandles, chain| Armed {
        value: Rc::clone(&held.value),
        declared: Rc::clone(&held.declared),
        refusing: Rc::clone(&held.refusing),
        polls: Rc::new(Cell::new(0)),
        chain,
        tenant,
    };
    let bot = three_chains(
        Bot::builder(format!("sim-observe-t{tenant}")),
        [armed(&first, 0), armed(&second, 1), armed(&third, 2)],
        &ran,
        &holds,
        tenant,
    )?;

    Ok(Tenant {
        bot,
        handles: vec![first, second, third],
        ran,
        tenant,
    })
}

/// The three cells one chain's source is built over.
struct ChainHandles {
    /// The value this chain's source reports.
    value: Rc<Cell<u32>>,
    /// What that source declares.
    declared: Rc<Cell<Option<RefreshReason>>>,
    /// Whether that source refuses.
    refusing: Rc<Cell<bool>>,
    /// Whether that source answers or never resolves.
    ///
    /// Set to [`WEDGED_PACE`] to wedge a chain and to anything else to let it
    /// answer. Shared by the families that do not model a refusal at all, so a
    /// source that is *slow* is a different thing from one that *refused*: the
    /// first is cancelled by the substrate's deadline and the second reports its
    /// own error, and a fixture that conflated them would test neither.
    pace: Rc<Cell<u32>>,
    /// How many polls still return `Pending` before answering.
    ///
    /// A source that yields once is a third thing beside *slow* and *refused*:
    /// it made no progress on its first poll, so the wave had to start a
    /// deadline watcher for it, and yet it answered before the budget was spent
    /// — the case that separates "a wave with something pending" from "a wave
    /// whose every source answered on its first poll".
    pending: Rc<Cell<u32>>,
}

impl ChainHandles {
    /// A chain with a value of one, nothing declared, and a source that reads.
    fn new() -> Self {
        Self {
            value: Rc::new(Cell::new(1)),
            declared: Rc::new(Cell::new(None)),
            refusing: Rc::new(Cell::new(false)),
            pace: Rc::new(Cell::new(0)),
            pending: Rc::new(Cell::new(0)),
        }
    }
}

// ── Families ───────────────────────────────────────────────────────────────

/// Every armed cause reaches the report on the tick it was armed, and nothing
/// else does.
///
/// The property is a bijection between the schedule and the report: an armed
/// window produces a forced refresh naming that chain and that cause, and an
/// unarmed chain produces nothing. Either half alone would pass against a
/// report that listed every chain every tick, which is the defect the row
/// forbids from the other side — a bot that re-reads everything forever is
/// indistinguishable from a healthy one only if you never look at the report.
fn forced_refresh_matches_the_schedule(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let plan = plan(sim.rng());
        let count = usize::try_from(plan.tenants)?;
        let mut tenants = Vec::with_capacity(count);
        for index in 0..count {
            let (_, subject) = tenant_of(&plan, index)?;
            tenants.push(subject);
        }

        // Everything the run saw, as report rows.
        let mut rows: Vec<ReportRow> = Vec::new();
        for tick in 1..=plan.ticks {
            for (index, subject) in tenants.iter_mut().enumerate() {
                let schedule = plan.schedule(index)?;
                subject.arm(tick, &schedule);
                subject.advance(tick);
                subject.observe(&mut rows);
            }
        }

        // Every armed chain is reported at least once over the run, and every
        // reported forced row names a chain whose window covers the tick it was
        // reported on. Checked against the schedule rather than a count, because
        // a count is satisfied by the right number of wrong rows. Only the
        // `forced` rows are counted: a `superseded` row is a different outcome
        // and folding it in would make the two indistinguishable.
        let mut reported: Vec<String> = rows
            .iter()
            .filter(|row| row.kind == "forced")
            .map(ReportRow::spell)
            .collect();
        reported.sort();
        reported.dedup();
        for row in &rows {
            if row.kind != "forced" {
                continue;
            }
            let schedule = plan
                .tenant(usize::try_from(row.tenant)?)
                .ok_or("a report from a tenant the schedule does not declare")?;
            let window = schedule.faults.get(row.chain).copied().ok_or_else(|| {
                format!(
                    "tenant {} reported chain {} but declared {MAX_CHAINS} chains",
                    row.tenant, row.chain
                )
            })?;
            assert_eq!(
                window.cause.map(RefreshReason::as_str),
                Some(row.detail.as_str()),
                "tenant {} chain {} reported as {} but its window names {:?}",
                row.tenant,
                row.chain,
                row.detail,
                window.cause.map(RefreshReason::as_str)
            );
        }
        let armed_chains: usize = (0..usize::try_from(plan.tenants)?)
            .filter_map(|index| {
                // A tenant the schedule does not declare contributes no armed
                // chain, and the assertion above has already required every armed
                // chain to come from one it does declare.
                let schedule = plan.tenant(index)?;
                Some(
                    (0..MAX_CHAINS)
                        .filter_map(move |chain| {
                            schedule
                                .faults
                                .get(chain)?
                                .cause
                                .map(|cause| (index, chain, cause.as_str()))
                        })
                        .count(),
                )
            })
            .sum();
        let reported_chains = reported.len();
        assert_eq!(
            reported_chains, armed_chains,
            "every armed chain reached the report exactly once, and no unarmed one \
             did: the row is a bijection, not a count"
        );
        // The schedule's shape first, so a seed that armed nothing still replays
        // to a receipt of what it decided rather than to the empty trace.
        sim.record(&format!(
            "tenants={} ticks={} armed={armed_chains}",
            plan.tenants, plan.ticks
        ));
        for row in &rows {
            sim.record(&row.spell());
        }
        Ok(())
    })
}

/// A chain whose forced poll keeps refusing is forced again, on the next tick.
///
/// The property that separates "the mark is spent by the read" from "the mark is
/// spent by the *successful* read". A source that arms and then refuses every
/// poll has replaced nothing, so comparing against its old baseline would be
/// comparing against a value it has said it cannot trust — the quiet state T08
/// exists to rule out.
fn a_refresh_that_never_lands_stays_marked(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        // A schedule that refuses everything: no draw decides it, because the
        // property needs the refusal to be certain rather than likely. Chain 0
        // is armed from tick 2 onward; chains 1 and 2 are healthy throughout,
        // which is what makes "and not the siblings" checkable.
        let schedule = TenantPlan {
            faults: [
                FaultWindow {
                    cause: Some(RefreshReason::WatchOverflow),
                    from: 2,
                    ticks: 4,
                },
                FaultWindow {
                    cause: None,
                    from: 0,
                    ticks: 0,
                },
                FaultWindow {
                    cause: None,
                    from: 0,
                    ticks: 0,
                },
            ],
            refusing: true,
            holds: false,
        };
        let mut subject = tenant(0, &schedule)?;

        // One clean tick first, so there is a baseline to be unsound about.
        let _outcome = subject.bot.tick();

        let mut rows: Vec<ReportRow> = Vec::new();
        for tick in 2..=4_u32 {
            subject.arm(tick, &schedule);
            subject.advance(tick);
            subject.observe(&mut rows);

            let report = subject.bot.tick_report();
            let forced = report
                .forced()
                .iter()
                .find(|entry| entry.chain() == 0)
                .ok_or_else(|| {
                    format!(
                        "tick {tick}: chain 0 declared an unsound baseline whose every \
                         forced poll refused, and it was not forced again"
                    )
                })?;
            assert_eq!(
                forced.reason(),
                RefreshReason::WatchOverflow,
                "tick {tick}: the cause is still the one the source declared"
            );
            assert!(
                report.forced().iter().all(|entry| entry.chain() == 0),
                "tick {tick}: the healthy sibling is not dragged in by its sibling's \
                 fault — {:?}",
                report.forced()
            );
            sim.record(&format!(
                "tick {tick} forced chain {} {}",
                forced.chain(),
                forced.reason().as_str()
            ));
        }
        for row in &rows {
            sim.record(&row.spell());
        }
        Ok(())
    })
}

/// Two tenants over one process keep separate reports, separate marks and
/// separate action logs.
///
/// Isolation is not "the second tenant built": it is that a chain in one tenant
/// never appears in the other's report, that a fault armed on one does not
/// force the other, and that the two action logs contain only their own
/// chains' work. The two tenants share a `GrantSet`, an environment and a
/// process, and differ only in their run identity — which is the closest thing
/// to a cross-tenant effect that a test can construct without a shared journal.
fn tenants_never_cross(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let plan = plan(sim.rng());
        let mut left = tenant(
            0,
            &plan
                .tenant(0)
                .ok_or("a two-tenant schedule with no left tenant")?,
        )?;
        let mut right = tenant(
            1,
            &plan
                .tenant(1)
                .ok_or("a two-tenant schedule with no right tenant")?,
        )?;

        let mut rows: Vec<ReportRow> = Vec::new();
        for tick in 1..=plan.ticks {
            for (subject, index) in [(&mut left, 0_usize), (&mut right, 1_usize)] {
                let schedule = plan.schedule(index)?;
                subject.arm(tick, &schedule);
                subject.advance(tick);
                subject.observe(&mut rows);
            }
        }
        for row in &rows {
            sim.record(&row.spell());
        }

        // Each tenant's report names only its own chains, and only within the
        // chain count it declared. A bot-wide mark would show up here as one
        // tenant's chain index appearing in the other's report.
        for subject in [&left, &right] {
            let declared = MAX_CHAINS;
            let report = subject.bot.tick_report();
            for forced in report.forced() {
                assert!(
                    forced.chain() < declared,
                    "tenant {} reported chain {} but declared {declared} chains",
                    subject.tenant,
                    forced.chain()
                );
                assert!(
                    forced
                        .domain()
                        .starts_with(&format!("sim::t{}", subject.tenant)),
                    "tenant {} reported a source belonging to another tenant: {}",
                    subject.tenant,
                    forced.domain()
                );
            }
        }

        // The action logs are the other half: each contains only its own chains'
        // work. A shared journal or a shared ledger would show one tenant's
        // action running the other tenant's payload.
        for subject in [&left, &right] {
            let declared = MAX_CHAINS;
            for &(chain, _) in subject.ran.borrow().iter() {
                assert!(
                    chain < declared,
                    "tenant {} ran an action at chain {chain} but declared \
                     {declared} chains — the other tenant's work reached this one",
                    subject.tenant
                );
            }
            sim.record(&format!(
                "tenant-{} ran {}",
                subject.tenant,
                subject.ran.borrow().len()
            ));
        }
        Ok(())
    })
}

/// The same payload under distinct event ids fires each time; a redelivery of
/// any id already seen fires nothing.
///
/// The seeded half of T09's identity claim. The plan draws a stream of event
/// ids over one fixed payload, some of them repeats, and the expectation is
/// derived from the stream rather than asserted as a count: every id's *first*
/// occurrence executes, and no later occurrence of any id does.
fn event_identities_are_per_event(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        // One payload for the whole stream, so the only thing separating two
        // events is their id. A dedup keyed on the payload would fire exactly
        // one of them and the trace would show it.
        let payload = sim.rng().below(64);
        let delivered = sim.rng().between(4, 12);

        // The stream, drawn first so the queue order is fixed: the feed pops from
        // the back, so the queue is built in reverse to deliver in draw order.
        let mut stream = Vec::new();
        let mut distinct = 0_u32;
        for _ in 0..delivered {
            let reuse = !stream.is_empty() && sim.rng().chance(450);
            let id = if reuse {
                stream[usize::try_from(sim.rng().below(distinct))?]
            } else {
                distinct = distinct.saturating_add(1);
                distinct
            };
            stream.push(id);
        }
        let queue = Rc::new(RefCell::new(
            stream
                .iter()
                .rev()
                .copied()
                .map(|id| EventId::new(u64::from(id), payload))
                .collect::<Vec<_>>(),
        ));
        let ran = Rc::new(RefCell::new(Vec::new()));
        let mut bot = Bot::builder("sim-observe-events")
            .observe(Feed {
                queue: Rc::clone(&queue),
                polls: Rc::new(Cell::new(0)),
            })
            .on(
                |event: &EventId<u32>| event.id() > 0,
                LogsEvents {
                    ran: Rc::clone(&ran),
                },
            )
            .with_effects(tenant_scope(0)?)
            .build(&GrantSet::empty())?;

        for _ in 0..delivered {
            let _outcome = bot.tick();
        }

        // Every distinct id fires exactly once, and only on its first delivery. The
        // expectation is the *unique* ids in first-seen order, which is what
        // `Vec::dedup` does not give: it removes consecutive duplicates only, so
        // a stream like [1, 2, 1] survives it intact. A set would lose the order,
        // which is the half that says an id fires on its *first* delivery rather
        // than whenever it was last seen.
        let mut wanted: Vec<u64> = Vec::new();
        for id in &stream {
            let id = u64::from(*id);
            if !wanted.contains(&id) {
                wanted.push(id);
            }
        }
        let fired = ran.borrow().clone();
        assert_eq!(
            fired.len(),
            wanted.len(),
            "each distinct event id fired exactly once over an identical payload: \
             {fired:?} for a stream of {stream:?}"
        );
        let mut sorted_fired = fired.clone();
        sorted_fired.sort_unstable();
        let mut sorted_wanted = wanted.clone();
        sorted_wanted.sort_unstable();
        assert_eq!(
            sorted_fired, sorted_wanted,
            "the ids that fired are exactly the distinct ids of the stream — a \
             payload-keyed dedup would have fired one of them"
        );
        assert_eq!(
            wanted.len(),
            usize::try_from(distinct)?,
            "the drawn stream and the drawn distinct count agree, so the schedule \
             and the expectation are one fact rather than two"
        );
        assert!(
            distinct > 0,
            "the stream must contain at least one distinct event id, or the run \
             proves nothing"
        );
        sim.record(&format!(
            "events delivered={delivered} distinct={distinct} fired={}",
            fired.len()
        ));
        Ok(())
    })
}

/// The tier sweep lives in `observe_refresh.rs`, not here.
///
/// It has to: the saturation tiers need 100, 1,000 and 10,000 chains on one
/// bot, and `Bot::observe` returns a builder whose type follows its source — so a
/// bot of *n* chains needs *n* written-out `observe`/`on` pairs, which is a
/// compile-time constant and not something a test can express at runtime. The
/// tier sweep therefore pins the tiers it can build and says so, and this family
/// covers the fault dimension a fixed chain count is the right shape for: which
/// chain declares what, over what window, and whether the report says so.
///
/// This is stated rather than discovered: a family here that swept "tiers" by
/// varying the seed and not the chain count would produce a green run and a
/// coverage number nobody could reconcile with the other file's.
/// The same seed produces the same trace hash, twice over.
///
/// The body is the union of the families above, so the hash covers forced
/// refreshes, pass-overs, per-tenant reports and the action logs rather than one
/// scenario's shape. `assert_replays` sweeps the band twice and compares the two
/// hash vectors, so a nondeterministic run fails even when every assertion
/// passes.
fn the_same_seed_replays(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let plan = plan(sim.rng());
        let count = usize::try_from(plan.tenants)?;
        for index in 0..count {
            let (tenant_id, mut subject) = tenant_of(&plan, index)?;
            let schedule = plan.schedule(index)?;
            let mut rows: Vec<ReportRow> = Vec::new();
            for tick in 1..=plan.ticks {
                subject.arm(tick, &schedule);
                subject.advance(tick);
                subject.observe(&mut rows);
            }
            for row in &rows {
                sim.record(&row.spell());
            }
            // The action log goes into the trace in order, never a timing.
            for &(chain, value) in subject.ran.borrow().iter() {
                sim.record(&format!("ran {tenant_id} {chain} {value}"));
            }
            // So does the held work, which is the part of the run a quiet tick
            // would otherwise hide.
            for work in subject.bot.pending() {
                sim.record(&format!("held {tenant_id} {}", work.id().chain()));
            }
        }
        // Settling what is held is what turns the ledger from "held" into
        // "decided", and it is where a cross-tenant mistake would show up: a
        // settlement for another tenant's key is a typed refusal, not a quiet
        // success.
        for index in 0..count {
            let (tenant_id, mut subject) = tenant_of(&plan, index)?;
            subject.advance(0);
            let _outcome = subject.bot.tick();
            for work in subject.bot.pending() {
                let Some(key) = work.key() else {
                    continue;
                };
                match subject.bot.resolve_effect(&key, EffectEvidence::Applied) {
                    Ok(()) => sim.record(&format!("settled {tenant_id}")),
                    Err(error) => sim.record(&format!("settle-refused {tenant_id} {error:?}")),
                }
            }
        }
        Ok(())
    })
}

// ── T06 slow source: the per-poll deadline under a seeded mix ───────────────

/// How a seeded source behaves on a tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pace {
    /// Answers immediately, with the value the plan gave it.
    Fast,
    /// Never resolves: the poll parks forever and is cancelled at the deadline.
    Wedged,
}

/// The pace and value one seed gave one chain.
#[derive(Clone, Copy, Debug)]
struct PaceWindow {
    /// Whether the chain answers or never resolves on this tick.
    pace: Pace,
    /// The value it reports when it answers.
    value: u32,
}

/// One tenant's seeded mix of fast and wedged chains over the run.
///
/// Fast chains dominate by construction, and that is the point: a run in which
/// everything stalls proves that stalling is reported and nothing about what it
/// costs the chains beside it. The healthy ones are what make "and the fast ones
/// still committed" checkable.
#[derive(Clone, Copy, Debug)]
struct PacePlan {
    /// Per chain, what it does on each tick.
    windows: [[PaceWindow; MAX_CHAINS]; MAX_TICKS],
    /// How many ticks the run drives.
    ticks: u32,
}

impl PacePlan {
    /// Chain `chain`'s behaviour on `tick`, defaulting to fast so a table this
    /// short cannot leave a chain undefined and panic on it later.
    ///
    /// The row is `tick - 1`, because the table is filled one-based: row 0 is
    /// tick 1. Reading `tick` directly would look one tick into the future and
    /// hand back the fallback for the last tick of every run, so a schedule would
    /// silently arm every chain *fast* on the tick after its last — and the run
    /// would still pass every report assertion, because the arming and the
    /// expectation would have shifted together.
    /// The window this schedule declares for `chain` at `tick`, or `None` when
    /// the schedule declares no such cell.
    ///
    /// `None` rather than a fast window carrying the value `1`: a cell the
    /// schedule does not declare has no pace and no value, and a stand-in would
    /// put one into the trace as though the schedule had drawn it.
    fn at(&self, chain: usize, tick: u32) -> Option<PaceWindow> {
        let row = tick.saturating_sub(1);
        let row = self.windows.get(usize::try_from(row).ok()?)?;
        row.get(chain).copied()
    }

    /// Whether `chain` answers on every tick of the run.
    ///
    /// "Never stalled" rather than "fast right now", because the differential it
    /// gates compares a chain's *whole* log against a run with no wedging at all,
    /// and a chain that stalled on one tick and not on another cannot be compared
    /// that way. Deciding it here rather than at the call site is what keeps the
    /// filter and the comparison describing the same set.
    fn never_stalled(&self, chain: usize) -> bool {
        // A chain the schedule leaves undeclared at a tick has no pace, and a
        // chain that was never wedged is exactly that: absence is the answer.
        (1..=self.ticks).all(|tick| {
            self.at(chain, tick)
                .is_none_or(|window| window.pace == Pace::Fast)
        })
    }

    /// A plan whose *last* tick is `tick`, with `per_chain` naming each chain's
    /// pacing on it.
    ///
    /// Built per tick rather than drawn for the whole run, because the family that
    /// uses it arms one tick at a time: the question is how one *wave* spends its
    /// watchdogs, and a plan that decided a run's pacings up front would answer
    /// it only on whichever tick the draw happened to land on.
    fn one_tick(per_chain: &[u32], tick: u32) -> Result<PacePlan, Box<dyn std::error::Error>> {
        let mut windows = [[PaceWindow {
            pace: Pace::Fast,
            value: 1,
        }; MAX_CHAINS]; MAX_TICKS];
        let row = usize::try_from(tick.saturating_sub(1))?;
        for (index, window) in windows.iter_mut().enumerate() {
            for (chain, cell) in window.iter_mut().enumerate() {
                *cell = PaceWindow {
                    pace: if per_chain.get(chain).copied() == Some(WEDGED_PACE) {
                        Pace::Wedged
                    } else {
                        Pace::Fast
                    },
                    // A distinct value per cell, so a commit of one chain's
                    // value can never be mistaken for another's.
                    value: u32::try_from(index.saturating_mul(MAX_CHAINS).saturating_add(chain))?
                        .saturating_add(1),
                };
            }
        }
        Ok(PacePlan {
            windows,
            ticks: u32::try_from(row.saturating_add(1))?,
        })
    }
}

/// The longest run this family drives.
///
/// Fixed rather than drawn: a seed that drew the tick count would renumber every
/// other family for no added evidence, which is the reason the existing
/// `plan` in this file fixes its own chain count the same way.
const MAX_TICKS: usize = 6;

/// Draw one tenant's pace schedule from `rng`.
///
/// Roughly a third of the chain-ticks are wedged. The draw is per chain *per
/// tick* rather than per chain, because the property that matters is a source
/// that recovers — a chain wedged for the whole run would satisfy "every
/// cancellation is reported" without ever exercising "the next tick re-polls it".
fn pace_plan(rng: &mut Rng, ticks: u32) -> Result<PacePlan, Box<dyn std::error::Error>> {
    let drawn = usize::try_from(ticks)?.min(MAX_TICKS);
    let mut windows = [[PaceWindow {
        pace: Pace::Fast,
        value: 1,
    }; MAX_CHAINS]; MAX_TICKS];
    let span = u32::try_from(MAX_CHAINS)?;
    for (tick, row) in windows.iter_mut().enumerate().take(drawn) {
        // One-based, because the families drive ticks from 1. A zero-based table
        // read at tick 1 would describe the tick *before* the first one, so every
        // arming decision in the run would be off by one — and an off-by-one here
        // surfaces only as "the report does not match the schedule", which is the
        // one failure mode a simulation family cannot debug from its trace.
        let tick_index = u32::try_from(tick)?.saturating_add(1);
        for (chain, window) in row.iter_mut().enumerate() {
            // Distinct per chain and tick, so a commit that did not happen is
            // distinguishable from a commit of somebody else's value.
            *window = PaceWindow {
                pace: if rng.chance(350) {
                    Pace::Wedged
                } else {
                    Pace::Fast
                },
                value: tick_index
                    .saturating_mul(span)
                    .saturating_add(u32::try_from(chain)?)
                    .saturating_add(1),
            };
        }
    }
    Ok(PacePlan { windows, ticks })
}

/// A tenant whose sources' paces are the plan's, under a short poll deadline.
///
/// A wedged source costs exactly this budget, in real time, on every tick it is
/// wedged: the poll deadline is a real-time watchdog by design (see
/// `bounded_wave`), so a realistic 30-second default would turn a band into
/// minutes of waiting. What the family observes does not depend on the number —
/// every assertion is about *which* chain was reported and whether the others
/// still committed — and the deadline is declared rather than faked, so the same
/// machinery under test is the one `Bot::tick` runs in production.
///
/// One millisecond rather than more because nothing here races it: a source
/// that is not wedged answers on its first turn, and a first turn is always
/// polled however early the wave's reaper fires. A source that yields once is
/// read on the turn after its `Pending` whenever that turn lands, because an
/// expired wave turns every poll once more before cutting it off; only its
/// *first* turn can fall behind a deadline a wedged sibling armed, which is why
/// the replay family arms yields ahead of wedges only. At 40 ms the stall
/// families spent ~12 s a band waiting.
const SIM_DEADLINE: std::time::Duration = std::time::Duration::from_millis(1);

/// Build one tenant's bot over [`MAX_CHAINS`] paced sources.
///
/// The chain count is the same written-out constant as [`tenant`]'s, for the
/// same reason: `Bot::observe` returns a builder whose type follows its source,
/// so a runtime chain count cannot be expressed.
fn paced_tenant(tenant: u32, deadline: std::time::Duration) -> Result<Tenant, Box<dyn Error>> {
    let ran = Rc::new(RefCell::new(Vec::new()));
    let holds = Rc::new(Cell::new(false));
    let (first, second, third) = (
        ChainHandles::new(),
        ChainHandles::new(),
        ChainHandles::new(),
    );
    let paced = |held: &ChainHandles, chain| Paced {
        pace: Rc::clone(&held.pace),
        value: Rc::clone(&held.value),
        declared: Rc::clone(&held.declared),
        polls: Rc::new(Cell::new(0)),
        chain,
        tenant,
    };
    let bot = three_chains(
        Bot::builder(format!("sim-observe-stall-t{tenant}")).with_poll_deadline(deadline),
        [paced(&first, 0), paced(&second, 1), paced(&third, 2)],
        &ran,
        &holds,
        tenant,
    )?;

    Ok(Tenant {
        bot,
        handles: vec![first, second, third],
        ran,
        tenant,
    })
}

/// A source whose answer-or-not is the plan's, under a real per-poll deadline.
struct Paced {
    /// Whether this poll answers or never resolves.
    pace: Rc<Cell<u32>>,
    /// The value it reports when it answers.
    value: Rc<Cell<u32>>,
    /// What `cache_state` answers.
    declared: Rc<Cell<Option<RefreshReason>>>,
    /// How many times the body ran.
    polls: Rc<Cell<u32>>,
    /// The chain's index, for the domain identity a report names it by.
    chain: usize,
    /// The tenant this source belongs to, for the same reason.
    tenant: u32,
}

impl Observe for Paced {
    type Output = u32;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        admit_poll(&call.0, Observe::required_caps(self), &self.polls)?;
        if self.pace.get() == WEDGED_PACE {
            // Parks forever without waking. The deadline is the only thing that
            // ends this poll, which is the property the family is about.
            std::future::pending::<()>().await;
        }
        Ok(self.value.get())
    }

    fn cache_state(&self) -> Option<RefreshReason> {
        self.declared.get()
    }

    fn domain_id(&self) -> &str {
        domain_of(self.tenant, self.chain)
    }
}

/// The cell value that means "never resolves".
///
/// A named constant rather than a `bool` next to the value cell so the two
/// channels cannot be confused by a call site that sets the wrong one: the value
/// cell is a `u32` because the source's output is one, and a `bool` there would
/// compile and mean nothing.
const WEDGED_PACE: u32 = u32::MAX;

/// Arm every chain for `tick` and run it, returning what the tick reported.
fn paced_tick(
    subject: &mut Tenant,
    plan: &PacePlan,
    tick: u32,
) -> Result<Vec<ReportRow>, Box<dyn Error>> {
    for (index, held) in subject.handles.iter().enumerate() {
        let window = plan
            .at(index, tick)
            .ok_or("the schedule declares no window for this chain at this tick")?;
        held.pace.set(if window.pace == Pace::Wedged {
            WEDGED_PACE
        } else {
            0
        });
        held.value.set(window.value);
        held.declared.set(None);
    }
    let mut rows = Vec::new();
    subject.observe(&mut rows);
    Ok(rows)
}

/// Arm every chain for `tick` with the *same* values but with nothing wedged, and
/// run it.
///
/// The counterfactual half of the differential: the same schedule, the same
/// values, every source answering. Written beside [`paced_tick`] rather than as a
/// flag on it so the two runs cannot drift apart in how they arm a chain — the
/// only difference between them is the pace, which is the thing under test.
fn unpace_tick(
    subject: &mut Tenant,
    plan: &PacePlan,
    tick: u32,
) -> Result<Vec<ReportRow>, Box<dyn Error>> {
    for (index, held) in subject.handles.iter().enumerate() {
        held.pace.set(0);
        held.value.set(
            plan.at(index, tick)
                .ok_or("the schedule declares no window for this chain at this tick")?
                .value,
        );
        held.declared.set(None);
    }
    let mut rows = Vec::new();
    subject.observe(&mut rows);
    Ok(rows)
}

/// Every wedged source is reported stalled, and the independent chains beside it
/// do exactly what they would have done with no wedging at all.
///
/// # Why this is a differential, and not a count
///
/// The obvious assertion — "the chains that moved fired" — is wrong about this
/// substrate, and finding out why is the reason the family is shaped this way.
/// `fire_fold` selects its work through `Changed<Revision>`, which a change
/// filter consumes on the tick it is seen, so several chains moving on one tick
/// select one chain's generation and the others' work waits for the next tick.
/// That is pre-existing behaviour (INV-BOT-121, latest-state mode), not something
/// the deadline introduced, and a fixture that asserted "every mover fires" would
/// have been asserting a rule the substrate never had.
///
/// So the claim is stated as what T06 actually asks: **a slow source costs nothing
/// else.** The same seeded schedule is driven twice over, once with the wedging
/// and once with every source answering, and the fast chains' action logs must be
/// **identical**. That is stronger than a count — a substrate that fired
/// everything twice would fail it — and it is immune to the selection rule above,
/// because both runs are subject to it identically.
///
/// The differential also covers what the row is *not* about. A wedged chain's
/// baseline must stay exactly where it was, so the chain that recovers is still
/// compared against the value from before it stalled; a cancellation that spent
/// the baseline would make the recovered chain's log diverge from the un-wedged
/// run, which this catches.
///
/// The report is checked against the schedule per tick and per tenant, so a
/// cancellation is named where it happened rather than in aggregate: one
/// `stalled` row per wedged chain, naming that chain's own domain, and none for a
/// chain that answered. Two tenants share the process and each is checked against
/// its own schedule, so the "and not a neighbour's" half of the two-tenant case
/// lives here rather than in a separate family.
fn a_wedged_source_is_reported_and_costs_its_neighbours_nothing(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        // Both tenants draw from the same stream, so one seed fixes both schedules
        // and both runs.
        let left = pace_plan(sim.rng(), 4)?;
        let right = pace_plan(sim.rng(), 4)?;
        let schedules = [left, right];

        let mut wedged = [
            paced_tenant(0, SIM_DEADLINE)?,
            paced_tenant(1, SIM_DEADLINE)?,
        ];
        let mut answered = [
            paced_tenant(0, SIM_DEADLINE)?,
            paced_tenant(1, SIM_DEADLINE)?,
        ];

        for tick in 1..=left.ticks {
            for index in 0..wedged.len() {
                let plan = *schedules
                    .get(index)
                    .ok_or("a subject this run armed has no schedule")?;

                // The wedged run, as the schedule decided it.
                let rows = paced_tick(&mut wedged[index], &plan, tick)?;
                let report = wedged[index].bot.tick_report();

                // The same tick over the same schedule with nothing wedged. Every
                // source answers, so this run is the counterfactual the first is
                // compared against.
                let _answered = unpace_tick(&mut answered[index], &plan, tick)?;

                // (1) and (2): the report is exactly the schedule's wedged chains,
                // once each, named by the source's own domain.
                let reported: Vec<(usize, String)> = report
                    .stalled()
                    .iter()
                    .map(|row| (row.chain(), row.domain().to_owned()))
                    .collect();
                let wanted: Vec<(usize, String)> = (0..MAX_CHAINS)
                    .filter(|chain| {
                        plan.at(*chain, tick)
                            .is_some_and(|window| window.pace == Pace::Wedged)
                    })
                    .map(|chain| (chain, DOMAINS[index][chain % 8].to_owned()))
                    .collect();
                assert_eq!(
                    reported, wanted,
                    "tick {tick}, tenant {index}: every source the schedule wedged is \
                     reported stalled once, by chain and by its own domain, and no \
                     source that answered is"
                );

                // (3): a chain that has *never* stalled did exactly what the same schedule
                // would have had it do with no wedging anywhere — the whole log,
                // every tick, in order.
                //
                // The whole log rather than this tick's slice, because a cumulative
                // log cannot be filtered by the current tick without also dropping
                // everything a chain did on the ticks when it *was* wedged, and
                // dropping those is exactly what would hide a cancellation that
                // stopped a recovering chain. A chain that stalled even once is
                // excluded from this comparison and checked by the report above
                // instead, which is where its cancellation is named.
                let subject = &wedged[index];
                let peer = &answered[index];
                let (fast_log, answered_log): (Vec<_>, Vec<_>) = (0..MAX_CHAINS)
                    .filter(|chain| plan.never_stalled(*chain))
                    .map(|chain| {
                        (
                            subject
                                .ran
                                .borrow()
                                .iter()
                                .filter(|entry| entry.0 == chain)
                                .copied()
                                .collect::<Vec<(usize, u32)>>(),
                            peer.ran
                                .borrow()
                                .iter()
                                .filter(|entry| entry.0 == chain)
                                .copied()
                                .collect::<Vec<(usize, u32)>>(),
                        )
                    })
                    .unzip();
                assert_eq!(
                    fast_log, answered_log,
                    "tick {tick}, tenant {index}: a chain that never stalled did \
                     different work with a sibling source stalled than it would have \
                     with nothing stalled"
                );

                for row in &rows {
                    sim.record(&row.spell());
                }
                let logged: Vec<(usize, u32)> = subject.ran.borrow().iter().copied().collect();
                for (chain, value) in logged {
                    sim.record(&format!("ran {index} {chain} {value}"));
                }
            }
        }
        Ok(())
    })
}

/// The same property under saturation: many chains stall at once and the
/// independent ones still commit.
///
/// The saturation tiers are 100 / 1,000 / 10,000 for the *held-chain* half of T06,
/// which `observe_refresh.rs` writes out because a bot of *n* chains needs *n*
/// written-out `observe`/`on` pairs. This family pins the tiers it can build at
/// the fixed [`MAX_CHAINS`] the erasure boundary allows, and says so rather than
/// claiming a tier number it did not run.
///
/// What it does add over the family above is *simultaneity*: every chain in the
/// wave is wedged on the same tick, so there is no independent chain left to
/// attribute the surviving commits to except the tenant beside it. Two tenants,
/// one entirely stalled and one entirely fast, is the saturating shape at this
/// chain count — and the assertion that the stalled tenant's report names every
/// chain while the fast tenant's report names none is what would fail if a
/// cancellation still parked the tick.
fn a_saturated_wave_stalls_every_chain_and_still_lets_the_next_tenant_commit(
    band: Band,
) -> TestResult {
    sim::assert_replays(band, |sim| {
        // Tenant 0 is wedged on every chain for the whole run; tenant 1 is fast
        // throughout. The draw is the seed's only contribution beyond that, so a
        // family that stopped varying anything would still replay — and a
        // schedule this asymmetric is the one that can actually distinguish "the
        // cancellation is bounded" from "the whole tick is cancelled".
        let ceiling = u32::try_from(MAX_TICKS)?;
        let ticks = sim.rng().between(2, ceiling);
        let mut all_wedged = PacePlan {
            windows: [[PaceWindow {
                pace: Pace::Fast,
                value: 1,
            }; MAX_CHAINS]; MAX_TICKS],
            ticks,
        };
        let mut all_fast = PacePlan {
            windows: [[PaceWindow {
                pace: Pace::Fast,
                value: 1,
            }; MAX_CHAINS]; MAX_TICKS],
            ticks,
        };
        for tick in 0..MAX_TICKS {
            for chain in 0..MAX_CHAINS {
                all_wedged.windows[tick][chain] = PaceWindow {
                    pace: Pace::Wedged,
                    value: 9,
                };
                all_fast.windows[tick][chain] = PaceWindow {
                    pace: Pace::Fast,
                    value: u32::try_from(tick.saturating_add(1))?,
                };
            }
        }
        // The chain count this family reached, recorded rather than clamped.
        let chains = subject_count(&all_wedged);

        let mut stalled = paced_tenant(0, SIM_DEADLINE)?;
        let mut ready = paced_tenant(1, SIM_DEADLINE)?;

        for tick in 1..=ticks {
            let _rows = paced_tick(&mut stalled, &all_wedged, tick)?;
            let _ready = paced_tick(&mut ready, &all_fast, tick)?;

            let stalled_report = stalled.bot.tick_report();
            assert_eq!(
                stalled_report.stalled().len(),
                chains,
                "every one of the {chains} chains in the saturated wave is reported \
                 stalled — a dropped cancellation is a source nobody looks at again"
            );
            let ready_report = ready.bot.tick_report();
            assert!(
                ready_report.stalled().is_empty(),
                "the tenant beside the saturated wave reports no stall: its sources \
                 never stopped answering ({:?})",
                ready_report.stalled()
            );
            assert!(
                ready.ran.borrow().len() >= usize::try_from(tick)?,
                "and it still committed and acted while the other tenant's whole \
                 wave was cancelled"
            );
            sim.record(&format!(
                "tick {tick} saturated_stalled={} ready_ran={}",
                stalled_report.stalled().len(),
                ready.ran.borrow().len()
            ));
        }
        sim.record(&format!("saturation chains={chains}"));
        Ok(())
    })
}

/// How many chains the saturated wave covers, as the tier it actually reached.
fn subject_count(plan: &PacePlan) -> usize {
    // A plan with no rows has no wedged subject in it: zero is that count, not a
    // stand-in for a count nobody took.
    plan.windows.first().map_or(0, |row| {
        row.iter()
            .filter(|window| window.pace == Pace::Wedged)
            .count()
    })
}

/// A seeded scope spends one watchdog on a wave that has a poll left pending,
/// and none at all on a wave whose every source answers.
///
/// The property the whole wave-level watchdog exists for, stated as a number
/// rather than as an adjective. A bot of *n* chains used to start and join one OS
/// thread *per source poll, per tick* — including the ordinary tick, where every
/// source answered on its first poll and the thread had nothing to do — so a
/// thousand chains paid a thousand spawns a tick that had no slow source in it at
/// all. The ordinary tick is the common one, so its cost is the one that decides
/// whether a bot scales at all.
///
/// # What the number is read from
///
/// `TickReport::watchdogs`, which is operator data a caller reads beside
/// `stalled`. Not a `#[cfg(test)]` counter and not a field only a test can reach:
/// a thread count the product cannot report is a thread count the product has
/// promised nothing about, and a test reading one of its own would be measuring
/// its own instrumentation rather than the substrate.
///
/// # Why the seed is still there
///
/// The *count* is the easy half to assert and the easy half to fake: a family
/// that always wedged a source would pass against an implementation that spawns a
/// thread per poll, and a family that never wedged one would pass against an
/// implementation that never spawns any. The seed picks the scope, so both arms
/// and everything between them is driven, and the count is checked against the
/// scope the seed chose rather than against a fixed expectation.
///
/// Two tenants share the process and are read independently, so "one watchdog per
/// wave" is also "one watchdog per *tenant's* wave": a counter shared across
/// tenants would report one where two were started, and one tenant alone could
/// never see the difference.
///
/// The mixed scope costs one real deadline per wedged tick, because that is what
/// a wedged source *is* — the substrate genuinely has to wait for the wall clock.
/// That is the honest cost of the family, and it is why the scopes are per tick
/// rather than a mixture inside one tick.
fn a_wave_spends_one_watchdog_and_a_fast_wave_spends_none(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        // Cut before anything is built: the tick count is a draw like any other,
        // and a run that drew six ticks of wedged sources would spend six real
        // deadlines waiting for the wall clock rather than exercising the claim.
        let ceiling = u32::try_from(MAX_TICKS)?;
        let ticks = sim.rng().between(2, ceiling);

        // Which chains wedge is re-drawn per tick from the same stream, so a run
        // that wedged every chain on tick 1 and nothing after it is one this
        // family produces, and so is its exact mirror.
        let mut pacings: [Vec<u32>; 2] = [vec![0; MAX_CHAINS], vec![0; MAX_CHAINS]];
        let mut subjects = [
            paced_tenant(0, SIM_DEADLINE)?,
            paced_tenant(1, SIM_DEADLINE)?,
        ];

        for tick in 1..=ticks {
            for (index, subject) in subjects.iter_mut().enumerate() {
                let drawn = pacings
                    .get_mut(index)
                    .ok_or("the run has exactly two tenants")?;
                for pace in drawn.iter_mut() {
                    *pace = if sim.rng().chance(350) {
                        WEDGED_PACE
                    } else {
                        0
                    };
                }
                let plan = PacePlan::one_tick(drawn, tick)?;
                let _rows = paced_tick(subject, &plan, tick)?;

                let report = subject.bot.tick_report();
                let wedged = drawn.iter().filter(|pace| **pace == WEDGED_PACE).count();
                assert_eq!(
                    report.watchdogs(),
                    u32::from(wedged > 0),
                    "tick {tick}, tenant {index}: {wedged} of this tenant's chains \
                     never resolved, so the wave spent {} watchdog threads — one for \
                     the wave with a poll still pending, and none at all when every \
                     source answered",
                    report.watchdogs()
                );
                assert_eq!(
                    report.stalled().len(),
                    wedged,
                    "tick {tick}, tenant {index}: every wedged chain is reported \
                     stalled, and no chain that answered is"
                );
                sim.record(&format!(
                    "tick {tick} tenant {index} wedged={wedged} watchdogs={}",
                    report.watchdogs()
                ));
            }
        }
        Ok(())
    })
}

/// The stall families replay exactly, twice over.
///
/// `assert_replays` sweeps the band twice and compares the two hash vectors, so a
/// nondeterministic run fails even when every assertion above passes — and a
/// deadline under test is exactly where nondeterminism would hide, because how far
/// into a budget a cancellation lands depends on how loaded the host is. The
/// trace records the *set* of stalled chains rather than any timing, so the
/// receipt stays a statement about what the run decided and not about how fast
/// it decided it.
///
/// The body is the union of the two families above, so the hash covers forced
/// refreshes, pass-overs, per-tenant reports, stall reports and the action logs
/// rather than one scenario's shape.
fn the_same_seed_replays_a_stalled_wave(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let left = pace_plan(sim.rng(), 4)?;
        let right = pace_plan(sim.rng(), 4)?;
        let schedules = [left, right];
        let mut subjects = [
            paced_tenant(0, SIM_DEADLINE)?,
            paced_tenant(1, SIM_DEADLINE)?,
        ];

        for tick in 1..=left.ticks {
            for (index, subject) in subjects.iter_mut().enumerate() {
                let plan = *schedules
                    .get(index)
                    .ok_or("a subject this run armed has no schedule")?;
                let _rows = paced_tick(subject, &plan, tick)?;
                let report = subject.bot.tick_report();
                // The watchdog count beside the stall list: a run that replayed
                // its decisions but started a different number of threads is a
                // run whose *cost* was not deterministic, which is exactly what
                // this family exists to notice about a deadline.
                sim.record(&format!("watchdogs {index} {}", report.watchdogs()));
                for row in report.stalled() {
                    sim.record(&format!("stalled {index} {} {}", row.chain(), row.domain()));
                }
                for forced in subject.bot.tick_report().forced() {
                    sim.record(&format!(
                        "forced {index} {} {}",
                        forced.chain(),
                        forced.reason().as_str()
                    ));
                }
                for &(chain, value) in subject.ran.borrow().iter() {
                    sim.record(&format!("ran {index} {chain} {value}"));
                }
            }
        }
        Ok(())
    })
}

band_family::band_family! {
    forced_refresh_band_00 => forced_refresh_matches_the_schedule, 0;
    forced_refresh_band_01 => forced_refresh_matches_the_schedule, 1;
    forced_refresh_band_02 => forced_refresh_matches_the_schedule, 2;
    forced_refresh_band_03 => forced_refresh_matches_the_schedule, 3;
    forced_refresh_band_04 => forced_refresh_matches_the_schedule, 4;
    forced_refresh_band_05 => forced_refresh_matches_the_schedule, 5;
    a_refresh_that_never_lands_band_06 => a_refresh_that_never_lands_stays_marked, 6;
    a_refresh_that_never_lands_band_07 => a_refresh_that_never_lands_stays_marked, 7;
    a_refresh_that_never_lands_band_08 => a_refresh_that_never_lands_stays_marked, 8;
    a_refresh_that_never_lands_band_09 => a_refresh_that_never_lands_stays_marked, 9;
    tenants_never_cross_band_10 => tenants_never_cross, 10;
    tenants_never_cross_band_11 => tenants_never_cross, 11;
    tenants_never_cross_band_12 => tenants_never_cross, 12;
    tenants_never_cross_band_13 => tenants_never_cross, 13;
    event_identities_band_14 => event_identities_are_per_event, 14;
    event_identities_band_15 => event_identities_are_per_event, 15;
    event_identities_band_16 => event_identities_are_per_event, 16;
    event_identities_band_17 => event_identities_are_per_event, 17;
    same_seed_band_18 => the_same_seed_replays, 18;
    same_seed_band_19 => the_same_seed_replays, 19;
    same_seed_band_20 => the_same_seed_replays, 20;
    same_seed_band_21 => the_same_seed_replays, 21;
    same_seed_band_22 => the_same_seed_replays, 22;
    same_seed_band_23 => the_same_seed_replays, 23;
    same_seed_band_24 => the_same_seed_replays, 24;
    same_seed_band_25 => the_same_seed_replays, 25;
    wedged_sources_band_26 => a_wedged_source_is_reported_and_costs_its_neighbours_nothing, 26;
    wedged_sources_band_27 => a_wedged_source_is_reported_and_costs_its_neighbours_nothing, 27;
    wedged_sources_band_28 => a_wedged_source_is_reported_and_costs_its_neighbours_nothing, 28;
    wedged_sources_band_29 => a_wedged_source_is_reported_and_costs_its_neighbours_nothing, 29;
    saturation_band_30 => a_saturated_wave_stalls_every_chain_and_still_lets_the_next_tenant_commit, 30;
    saturation_band_31 => a_saturated_wave_stalls_every_chain_and_still_lets_the_next_tenant_commit, 31;
    stall_replay_band_32 => the_same_seed_replays_a_stalled_wave, 32;
    stall_replay_band_33 => the_same_seed_replays_a_stalled_wave, 33;
    watchdog_budget_band_34 => a_wave_spends_one_watchdog_and_a_fast_wave_spends_none, 34;
    watchdog_budget_band_35 => a_wave_spends_one_watchdog_and_a_fast_wave_spends_none, 35;
}

// ── INV-BOT-120..124: the deadline, watchdog and attribution families ───────
//
// The bands above sweep a *fixed* chain count. These families are the other
// axis: a seed that decides the wave *width* itself — how many chains share one
// deadline, whether any of them yields once or parks forever, and which chain
// of the wave a stall, a forced refresh or a pass-over is reported against. A
// chain count the band families fix is a claim about one shape; these are
// claims about the shape as a function of the width.
//
// They are declared as plain `#[test]` functions rather than through
// `band_family!` because each asserts a *different* property of the same
// machinery rather than the same property over more seeds, and the source
// counter reads a literal `#[test]` line where it deliberately does not read a
// macro invocation. The seeded bands the file already declares are untouched:
// widening them would buy no evidence and would move the seeds those families
// already record.

/// The width of one observation wave, as this file reads it.
///
/// A copy of the substrate's own fan-out rather than a re-derivation, exactly
/// as the corresponding non-simulation fixture does: it is the number that
/// decides how many chains share one deadline watchdog, and a family that
/// derived it would follow a change to the fan-out rather than noticing one.
const WAVE_WIDTH: usize = 32;

/// A source whose entire answer is the scenario's: it can answer, yield once,
/// park forever, refuse, and declare its own cached baseline unsound, all from
/// the cells one [`ChainHandles`] carries.
///
/// One source type rather than the two the bands use, because a builder's type
/// follows its source: a family that varies the chain count at runtime needs
/// every chain to be the same concrete type, and a source that can express
/// every shape these families need is what makes the count a draw instead of a
/// constant.
struct Scripted {
    /// The value this poll reports once it answers.
    value: Rc<Cell<u32>>,
    /// What `cache_state` answers.
    declared: Rc<Cell<Option<RefreshReason>>>,
    /// Whether this poll refuses instead of reading.
    refusing: Rc<Cell<bool>>,
    /// [`WEDGED_PACE`] parks the poll forever; anything else lets it answer.
    pace: Rc<Cell<u32>>,
    /// How many polls still return `Pending` before answering.
    pending: Rc<Cell<u32>>,
    /// The chain's index, for the domain identity a report names it by.
    chain: usize,
    /// The tenant this source belongs to, for the same reason.
    tenant: u32,
}

impl Scripted {
    /// The poll a source that is not refusing makes: wedged without resolving,
    /// pending for its paced polls, then ready with its value.
    async fn read(&self) -> Result<u32, BotError> {
        if self.pace.get() == WEDGED_PACE {
            // Parks without ever resolving, which is the shape the per-poll
            // deadline exists to bound. A yield is a *different* shape and is
            // handled below, so a fixture cannot confuse "slow" with "stopped".
            std::future::pending::<()>().await;
        }
        let pending = Rc::clone(&self.pending);
        let value = Rc::clone(&self.value);
        std::future::poll_fn(move |context: &mut Context<'_>| {
            if pending.get() > 0 {
                pending.set(pending.get().saturating_sub(1));
                // Wakes itself, so the wave re-polls without waiting out the
                // budget: the poll made no progress, but it did not stop.
                context.waker().wake_by_ref();
                Poll::Pending
            } else {
                Poll::Ready(Ok(value.get()))
            }
        })
        .await
    }
}

impl Observe for Scripted {
    type Output = u32;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        call.0.check(Observe::required_caps(self))?;
        if self.refusing.get() {
            Err(seeded_refusal(self.tenant, self.chain))
        } else {
            self.read().await
        }
    }

    fn cache_state(&self) -> Option<RefreshReason> {
        self.declared.get()
    }

    fn domain_id(&self) -> &str {
        domain_of(self.tenant, self.chain)
    }
}

/// Build one scripted source over `held`, as chain `chain` of `tenant`.
fn scripted(held: &ChainHandles, chain: usize, tenant: u32) -> Scripted {
    Scripted {
        value: Rc::clone(&held.value),
        declared: Rc::clone(&held.declared),
        refusing: Rc::clone(&held.refusing),
        pace: Rc::clone(&held.pace),
        pending: Rc::clone(&held.pending),
        chain,
        tenant,
    }
}

/// The action one chain's entry runs: it logs the value it ran with, and either
/// lands or reports its own outcome unknown.
fn logs(ran: &Rc<RefCell<Vec<(usize, u32)>>>, chain: usize, holds: &Rc<Cell<bool>>) -> Logs {
    Logs {
        ran: Rc::clone(ran),
        chain,
        holds: Rc::clone(holds),
    }
}

/// Arm one chain for one tick: its value, its pace, its declaration and how many
/// polls it yields for.
///
/// One helper rather than a field write per call site, because every family here
/// arms the same four cells and a site that forgot one would arm a chain the
/// previous tick left wedged.
fn arm_chain(
    held: &ChainHandles,
    value: u32,
    pace: u32,
    declared: Option<RefreshReason>,
    pending: u32,
) {
    held.value.set(value);
    held.pace.set(pace);
    held.declared.set(declared);
    held.pending.set(pending);
    held.refusing.set(false);
}

/// Build one tenant's bot over `width` scripted chains under `deadline`.
///
/// The chain count is a parameter rather than a constant because every chain is
/// the same concrete [`Scripted`] type, which is what lets the builder be
/// extended in a loop: the erasure boundary the band families run into only
/// binds when the source types differ.
fn scripted_bot(
    name: &str,
    tenant: u32,
    width: usize,
    deadline: Duration,
    holds: Rc<Cell<bool>>,
) -> Result<Tenant, Box<dyn Error>> {
    assert!(width >= 1, "a scripted bot needs at least one chain");
    let ran = Rc::new(RefCell::new(Vec::new()));
    let first = ChainHandles::new();
    let mut builder = Bot::builder(name)
        .with_poll_deadline(deadline)
        .observe(scripted(&first, 0, tenant))
        .on(|observed: &u32| *observed > 0, logs(&ran, 0, &holds));
    let mut handles = vec![first];
    for chain in 1..width {
        let held = ChainHandles::new();
        builder = builder
            .observe(scripted(&held, chain, tenant))
            .on(|observed: &u32| *observed > 0, logs(&ran, chain, &holds));
        handles.push(held);
    }
    let bot = builder
        .with_effects(tenant_scope(tenant)?)
        .build(&GrantSet::empty())?;
    Ok(Tenant {
        bot,
        handles,
        ran,
        tenant,
    })
}

/// Build one tenant's bot over `width` scripted chains and run one clean
/// baseline tick, so every chain has a committed value before it is armed.
///
/// Shared by the two families that need a pre-stall baseline, because "arm
/// every chain to a distinct value and tick once" is the same three-step setup
/// in both and a copy would drift the moment one of them changed what it
/// considered a baseline.
fn baseline_bot(
    name: &str,
    width: usize,
    deadline: Duration,
    holds: Rc<Cell<bool>>,
) -> Result<Tenant, Box<dyn Error>> {
    let mut subject = scripted_bot(name, 0, width, deadline, holds)?;
    for (index, held) in subject.handles.iter().enumerate() {
        let value = u32::try_from(index)?.saturating_add(1);
        arm_chain(held, value, 0, None, 0);
    }
    let _baseline = subject.bot.tick();
    Ok(subject)
}

/// A wave whose every source answers on its first poll starts no watchdog at all.
///
/// The lazy half of INV-BOT-124 as a function of the width: at 1 chain and at 32
/// the ordinary tick pays nothing, because a poll that answered never reaches
/// the arm that starts a watcher. The widths are drawn, so the property is
/// checked across the wave rather than at one point in it.
#[test]
fn a_fast_wave_spends_no_watchdog_across_seeded_widths() -> TestResult {
    const SEED: u64 = 0x1240_0001;
    let mut rng = Rng::new(SEED);
    let ceiling = u32::try_from(WAVE_WIDTH)?;
    for _ in 0..6_u32 {
        let width = usize::try_from(rng.between(1, ceiling))?;
        let holds = Rc::new(Cell::new(false));
        let mut subject = scripted_bot("sim-observe-fast-wave", 0, width, SIM_DEADLINE, holds)?;
        for (index, held) in subject.handles.iter().enumerate() {
            let value = u32::try_from(index)?.saturating_add(1);
            arm_chain(held, value, 0, None, 0);
        }
        let _outcome = subject.bot.tick();
        let report = subject.bot.tick_report();
        assert_eq!(
            report.watchdogs(),
            0,
            "seed {SEED} width {width}: an ordinary tick — every source answered \
             on its first poll — starts no deadline thread at all"
        );
        assert!(
            report.stalled().is_empty(),
            "seed {SEED} width {width}: and reports no stall, because nothing was \
             given up on: {:?}",
            report.stalled()
        );
    }
    Ok(())
}

/// A source that yields once before it answers spends exactly one watchdog for
/// its wave, and the waves beside it spend none.
///
/// INV-BOT-124's other half. The bot is wider than one wave, and the seed picks
/// which chain yields, so the assertion is that *one* wave — the one holding the
/// poll that went `Pending` — spends a watcher, and the resolved waves do not.
/// A source that yields once is the case a per-poll watcher would miss and a
/// never-started one would also miss; only a lazy, per-wave watcher gets it.
#[test]
fn a_pending_source_spends_one_watchdog_for_its_wave() -> TestResult {
    const SEED: u64 = 0x1240_0002;
    let mut rng = Rng::new(SEED);
    let wave = u32::try_from(WAVE_WIDTH)?;
    let width = usize::try_from(rng.between(wave.saturating_add(1), wave.saturating_mul(2)))?;
    let yields = usize::try_from(rng.below(u32::try_from(width)?))?;
    let holds = Rc::new(Cell::new(false));
    let mut subject = scripted_bot("sim-observe-pending", 0, width, SIM_DEADLINE, holds)?;
    for (index, held) in subject.handles.iter().enumerate() {
        let pending = u32::from(index == yields);
        let value = u32::try_from(index)?.saturating_add(1);
        arm_chain(held, value, 0, None, pending);
    }
    let _outcome = subject.bot.tick();
    let report = subject.bot.tick_report();
    assert_eq!(
        report.watchdogs(),
        1,
        "seed {SEED}: width {width} is {} waves and chain {yields} yielded once \
         before answering, so exactly its wave spends one watcher and the rest \
         spend none",
        width.div_ceil(WAVE_WIDTH)
    );
    assert!(
        report.stalled().is_empty(),
        "seed {SEED}: the poll that yielded once answered before the budget, so \
         nothing is reported stalled: {:?}",
        report.stalled()
    );
    Ok(())
}

/// The watchdog count summed over a run equals the number of ticks whose wave
/// was left pending.
///
/// The aggregate form of the wave rule, read from the reports rather than
/// inferred from the schedule: a tick where every source answered spends zero
/// and a tick that left one poll pending spends one, so the two sums are one
/// fact. A counter that drifted — a watcher started but not counted, or counted
/// but never started — would separate them.
#[test]
fn a_seeded_run_spends_one_watchdog_per_pending_tick() -> TestResult {
    const SEED: u64 = 0x1240_0003;
    let mut rng = Rng::new(SEED);
    let ticks = rng.between(3, 6);
    let holds = Rc::new(Cell::new(false));
    let mut subject = scripted_bot("sim-observe-pending-ticks", 0, 3, SIM_DEADLINE, holds)?;
    let mut watchdogs = 0_u32;
    let mut pending_ticks = 0_u32;
    for tick in 1..=ticks {
        let mut pending = false;
        for (index, held) in subject.handles.iter().enumerate() {
            let wedged = rng.chance(350);
            pending = pending || wedged;
            let pace = if wedged { WEDGED_PACE } else { 0 };
            let value = tick
                .saturating_mul(10)
                .saturating_add(u32::try_from(index)?);
            arm_chain(held, value, pace, None, 0);
        }
        let _outcome = subject.bot.tick();
        let report = subject.bot.tick_report();
        let expected = u32::from(pending);
        assert_eq!(
            report.watchdogs(),
            expected,
            "seed {SEED} tick {tick}: a tick that left a poll pending spends one \
             watcher and a tick where every source answered spends none"
        );
        watchdogs = watchdogs.saturating_add(report.watchdogs());
        pending_ticks = pending_ticks.saturating_add(expected);
    }
    assert_eq!(
        watchdogs, pending_ticks,
        "seed {SEED}: the watchdog count summed over {ticks} ticks equals the \
         number of ticks with a pending wave, both read from the reports"
    );
    Ok(())
}

/// A tick dropped mid-wave leaves the bot usable, and the next tick reports
/// normally.
///
/// The cancellation INV-BOT-123 documents is between phases; this is the other
/// drop the tick's own contract names. The future is polled once — far enough
/// to enter the wave and park on a wedged source — and then dropped. What the
/// family asserts is that the world it left behind is not poisoned: the next
/// tick re-polls every source, commits, and reports no stall of its own.
#[test]
fn a_cancelled_tick_leaves_the_bot_usable() -> TestResult {
    const SEED: u64 = 0x1240_0004;
    let mut rng = Rng::new(SEED);
    let holds = Rc::new(Cell::new(false));
    let mut subject = baseline_bot("sim-observe-cancelled-tick", 3, SIM_DEADLINE, holds)?;

    let wedged = usize::try_from(rng.below(3))?;
    for (index, held) in subject.handles.iter().enumerate() {
        let pace = if index == wedged { WEDGED_PACE } else { 0 };
        arm_chain(held, 7, pace, None, 0);
    }
    // `Bot::tick` parks the thread until the future resolves, so the only way
    // to drop a tick mid-flight is to drive `tick_async` by hand. `Waker::noop`
    // is the legal waker that never needs to wake: the future is dropped before
    // anything would.
    let mut context = Context::from_waker(Waker::noop());
    {
        let mut future = std::pin::pin!(subject.bot.tick_async());
        let first = Future::poll(future.as_mut(), &mut context);
        assert!(
            matches!(first, Poll::Pending),
            "seed {SEED}: the wedged source parks, so the first poll of the tick \
             returns Pending with the wave still in flight"
        );
    }

    for (index, held) in subject.handles.iter().enumerate() {
        let value = u32::try_from(index)?.saturating_add(20);
        arm_chain(held, value, 0, None, 0);
    }
    let _recovered = subject.bot.tick();
    let report = subject.bot.tick_report();
    assert_eq!(
        report.watchdogs(),
        0,
        "seed {SEED}: the tick after a cancelled one is ordinary — every source \
         answered on its first poll and no watcher was needed"
    );
    assert!(
        report.stalled().is_empty(),
        "seed {SEED}: nothing is reported stalled after the cancelled tick was \
         dropped: {:?}",
        report.stalled()
    );
    assert!(
        subject.ran.borrow().iter().any(|entry| entry.0 == wedged),
        "seed {SEED}: the chain that was wedged during the cancelled tick \
         committed on the next tick, so the bot is usable rather than poisoned"
    );
    Ok(())
}

/// A wedged source keeps its baseline and its forced-refresh mark standing, and
/// the next tick re-polls it.
///
/// INV-BOT-123's stall rule and INV-BOT-120's mark rule, together: a
/// cancellation commits nothing, so the value the source was to replace is
/// still there and the declaration that made it unsound is still standing. The
/// recovery tick re-enters the source and commits the value it reports, which is
/// the difference between a deferral and a retirement.
#[test]
fn a_stalled_chain_keeps_its_mark_and_is_re_polled_next_tick() -> TestResult {
    const SEED: u64 = 0x1230_0005;
    let mut rng = Rng::new(SEED);
    let cause = CAUSES[usize::try_from(rng.below(4))?];
    let holds = Rc::new(Cell::new(false));
    let mut subject = baseline_bot("sim-observe-stall-mark", 3, SIM_DEADLINE, holds)?;

    for (index, held) in subject.handles.iter().enumerate() {
        if index == 0 {
            arm_chain(held, 1, WEDGED_PACE, Some(cause), 0);
        } else {
            arm_chain(held, 10, 0, None, 0);
        }
    }
    let _stalled = subject.bot.tick();
    let after = subject.bot.tick_report();
    assert_eq!(
        after.stalled().len(),
        1,
        "seed {SEED}: chain 0's poll was cancelled: {:?}",
        after.stalled()
    );
    assert!(
        after
            .forced()
            .iter()
            .any(|row| row.chain() == 0 && row.reason() == cause),
        "seed {SEED}: the forced-refresh mark survives the stall, naming chain 0 \
         and the cause it declared: {:?}",
        after.forced()
    );

    arm_chain(&subject.handles[0], 42, 0, None, 0);
    let _recovered = subject.bot.tick();
    assert!(
        subject
            .ran
            .borrow()
            .iter()
            .any(|entry| entry.0 == 0 && entry.1 == 42),
        "seed {SEED}: the next tick re-polled the stalled chain and committed the \
         value it reported: {:?}",
        subject.ran.borrow()
    );
    let recovered = subject.bot.tick_report();
    assert!(
        recovered.stalled().is_empty(),
        "seed {SEED}: the tick that read the source reports no stall: {:?}",
        recovered.stalled()
    );
    Ok(())
}

/// A per-poll deadline is accepted at both boundaries and refused one step past
/// the ceiling, seeded around the edges.
///
/// The bound is only a bound if it cannot be set to something that is not one.
/// The smallest positive budget is a bounded wait and is accepted; the declared
/// ceiling is accepted; zero — which cancels every poll before its first poll —
/// and anything past the ceiling are typed refusals naming the number, because
/// a clamped deadline is indistinguishable from the one the caller asked for.
#[test]
fn a_poll_deadline_around_both_edges_is_accepted_or_refused_at_build() -> TestResult {
    const SEED: u64 = 0x1230_0006;
    let mut rng = Rng::new(SEED);
    let slack = u64::from(rng.between(1, 1_000));

    let smallest = Duration::from_nanos(slack);
    let accepted = Bot::builder("sim-deadline-smallest")
        .with_poll_deadline(smallest)
        .with_effects(tenant_scope(0)?)
        .build(&GrantSet::empty());
    assert!(
        accepted.is_ok(),
        "seed {SEED}: a positive budget is a bound, so it is accepted: {accepted:?}"
    );

    let at_ceiling = Bot::builder("sim-deadline-ceiling")
        .with_poll_deadline(MAX_POLL_DEADLINE)
        .with_effects(tenant_scope(0)?)
        .build(&GrantSet::empty());
    assert!(
        at_ceiling.is_ok(),
        "seed {SEED}: the declared ceiling is itself accepted: {at_ceiling:?}"
    );

    let default = Bot::builder("sim-deadline-default")
        .with_effects(tenant_scope(0)?)
        .build(&GrantSet::empty());
    assert!(
        default.is_ok(),
        "seed {SEED}: the declared default is inside both bounds: {default:?}"
    );
    assert!(
        DEFAULT_POLL_DEADLINE > Duration::ZERO && DEFAULT_POLL_DEADLINE <= MAX_POLL_DEADLINE,
        "seed {SEED}: the default budget is a bounded one"
    );

    let zero = Bot::builder("sim-deadline-zero")
        .with_poll_deadline(Duration::ZERO)
        .with_effects(tenant_scope(0)?)
        .build(&GrantSet::empty());
    assert!(
        matches!(zero, Err(BotError::PollDeadlineUnbounded { deadline }) if deadline.is_zero()),
        "seed {SEED}: zero bounds nothing and is refused, naming the number: {zero:?}"
    );

    let past = MAX_POLL_DEADLINE.saturating_add(Duration::from_nanos(slack));
    let over = Bot::builder("sim-deadline-over")
        .with_poll_deadline(past)
        .with_effects(tenant_scope(0)?)
        .build(&GrantSet::empty());
    match over {
        Err(BotError::PollDeadlineExceeded { deadline, ceiling }) => {
            assert_eq!(
                deadline, past,
                "seed {SEED}: the refusal names the budget that was asked for"
            );
            assert_eq!(
                ceiling, MAX_POLL_DEADLINE,
                "seed {SEED}: and the ceiling that would have been accepted, so the \
                 repair is a number rather than a guess"
            );
        }
        other => {
            return Err(
                format!("seed {SEED}: a budget past the ceiling was accepted: {other:?}").into(),
            );
        }
    }
    Ok(())
}

/// Among a wave of scripted sources, only the chain the seed wedged is reported
/// stalled — named by its own chain, its own `domain_id` and the applied budget.
///
/// The per-chain half of INV-BOT-123's report: the stall is not a wave-level
/// fact garbled across the chains in it. A report that named the wave, or the
/// first chain, or a neighbour would pass a "something stalled" assertion and
/// fail this one.
#[test]
fn only_the_seeded_wedged_chain_is_reported_stalled() -> TestResult {
    const SEED: u64 = 0x1230_0007;
    let mut rng = Rng::new(SEED);
    let ceiling = u32::try_from(WAVE_WIDTH)?;
    let width = usize::try_from(rng.between(2, ceiling))?;
    let wedged = usize::try_from(rng.below(u32::try_from(width)?))?;
    let holds = Rc::new(Cell::new(false));
    let mut subject = scripted_bot("sim-observe-one-stalled", 0, width, SIM_DEADLINE, holds)?;
    for (index, held) in subject.handles.iter().enumerate() {
        let pace = if index == wedged { WEDGED_PACE } else { 0 };
        let value = u32::try_from(index)?.saturating_add(1);
        arm_chain(held, value, pace, None, 0);
    }
    let _outcome = subject.bot.tick();
    let report = subject.bot.tick_report();
    assert_eq!(
        report.stalled().len(),
        1,
        "seed {SEED} width {width}: exactly the chain the seed wedged is \
         reported: {:?}",
        report.stalled()
    );
    let row = report
        .stalled()
        .first()
        .ok_or_else(|| format!("seed {SEED}: the wedged chain is named"))?;
    assert_eq!(
        row.chain(),
        wedged,
        "seed {SEED}: the reported chain is the wedged one"
    );
    assert_eq!(
        row.domain(),
        DOMAINS[0][wedged % 8],
        "seed {SEED}: named by its own domain_id, not a neighbour's"
    );
    assert_eq!(
        row.deadline(),
        SIM_DEADLINE,
        "seed {SEED}: and carrying the budget that was actually applied"
    );
    Ok(())
}

/// The siblings of a wedged source commit and act in the *same* tick, at seeded
/// widths.
///
/// INV-BOT-123's "the chains beside it commit and act in the same tick", stated
/// where a tick boundary can be seen: a cancellation is not a park, so the tick
/// reaches the walk with every other chain's observation committed. Exactly one
/// sibling moves, so the change-selection rule cannot hide it, and the wedged
/// chain's own action must not appear.
#[test]
fn siblings_of_a_wedged_source_act_in_the_same_tick() -> TestResult {
    const SEED: u64 = 0x1230_0008;
    let mut rng = Rng::new(SEED);
    let width = usize::try_from(rng.between(3, 12))?;
    let wedged = usize::try_from(rng.below(u32::try_from(width)?))?;
    let mover = wedged.saturating_add(1) % width;
    let holds = Rc::new(Cell::new(false));
    let mut subject = scripted_bot("sim-observe-siblings", 0, width, SIM_DEADLINE, holds)?;
    for (index, held) in subject.handles.iter().enumerate() {
        let value = u32::try_from(index)?.saturating_add(1);
        arm_chain(held, value, 0, None, 0);
    }
    let _baseline = subject.bot.tick();
    let before = subject.ran.borrow().len();

    for (index, held) in subject.handles.iter().enumerate() {
        let pace = if index == wedged { WEDGED_PACE } else { 0 };
        let value = if index == mover {
            999
        } else {
            u32::try_from(index)?.saturating_add(1)
        };
        arm_chain(held, value, pace, None, 0);
    }
    let _stalled = subject.bot.tick();
    let ran = subject.ran.borrow().clone();
    let added: Vec<(usize, u32)> = ran.iter().skip(before).copied().collect();
    assert!(
        ran.len() > before,
        "seed {SEED} width {width}: the sibling that moved acted in the same tick \
         the wedged source was cancelled: {ran:?}"
    );
    assert!(
        added.iter().any(|entry| entry.0 == mover && entry.1 == 999),
        "seed {SEED}: the moving sibling acted with the value it moved to: {added:?}"
    );
    assert!(
        !added.iter().any(|entry| entry.0 == wedged),
        "seed {SEED}: the wedged chain committed nothing and acted nothing, \
         because its poll was cancelled: {added:?}"
    );
    Ok(())
}

/// Each chain's own declared cause is reported against that chain in `forced`.
///
/// INV-BOT-120's per-chain mark: the report is a bijection with the
/// declarations, so a chain that declared nothing is not forced and a chain
/// that declared a cause names its own cause — not a sibling's, and not a
/// bot-wide guess about which transport is up.
#[test]
fn a_seeded_reason_per_chain_is_reported_against_its_own_chain() -> TestResult {
    const SEED: u64 = 0x1200_0009;
    let mut rng = Rng::new(SEED);
    let holds = Rc::new(Cell::new(false));
    let mut subject = scripted_bot("sim-observe-reasons", 0, 3, SIM_DEADLINE, holds)?;
    let mut expected: Vec<(usize, RefreshReason)> = Vec::new();
    for (index, held) in subject.handles.iter().enumerate() {
        let cause = if rng.chance(700) {
            let cause = CAUSES[usize::try_from(rng.below(4))?];
            expected.push((index, cause));
            Some(cause)
        } else {
            None
        };
        let value = u32::try_from(index)?.saturating_add(1);
        arm_chain(held, value, 0, cause, 0);
    }
    let _outcome = subject.bot.tick();
    let report = subject.bot.tick_report();
    let got: Vec<(usize, RefreshReason)> = report
        .forced()
        .iter()
        .map(|row| (row.chain(), row.reason()))
        .collect();
    assert_eq!(
        got, expected,
        "seed {SEED}: the forced report is a bijection with the declarations — \
         each chain names its own cause and no undeclared chain is forced"
    );
    for row in report.forced() {
        assert_eq!(
            row.domain(),
            DOMAINS[0][row.chain() % 8],
            "seed {SEED}: chain {} is named by its own domain",
            row.chain()
        );
    }
    Ok(())
}

/// A forced refresh whose poll keeps failing stays marked until a read that
/// commits spends it.
///
/// The two halves of INV-BOT-120's spend rule that a single-tick family cannot
/// see: while every forced poll fails, the mark outlives each tick and the
/// report still names the chain; once the source answers, the committing read
/// spends the mark, and the tick after that is quiet. A repair that cleared the
/// mark on the failed poll would return the bot to comparing against the
/// unsound baseline it just failed to replace.
#[test]
fn a_failed_forced_refresh_keeps_the_mark_until_a_committed_read_spends_it() -> TestResult {
    const SEED: u64 = 0x1200_0010;
    let mut rng = Rng::new(SEED);
    let cause = CAUSES[usize::try_from(rng.below(4))?];
    let failures = rng.between(2, 4);
    let holds = Rc::new(Cell::new(false));
    let mut subject = scripted_bot("sim-observe-failed-mark", 0, 2, SIM_DEADLINE, holds)?;
    arm_chain(&subject.handles[0], 1, 0, Some(cause), 0);
    subject.handles[0].refusing.set(true);
    arm_chain(&subject.handles[1], 2, 0, None, 0);

    for tick in 1..=failures {
        let _outcome = subject.bot.tick();
        let report = subject.bot.tick_report();
        assert!(
            report
                .forced()
                .iter()
                .any(|row| row.chain() == 0 && row.reason() == cause),
            "seed {SEED} failure {tick} of {failures}: chain 0's forced poll \
             refused, so its mark stands and the report still names it: {:?}",
            report.forced()
        );
        assert!(
            report.stalled().is_empty(),
            "seed {SEED} failure {tick}: a refused poll is a domain failure, not a \
             cancellation: {:?}",
            report.stalled()
        );
    }

    subject.handles[0].refusing.set(false);
    subject.handles[0].declared.set(None);
    subject.handles[0].value.set(50);
    let _spend = subject.bot.tick();
    assert!(
        subject
            .ran
            .borrow()
            .iter()
            .any(|entry| entry.0 == 0 && entry.1 == 50),
        "seed {SEED}: the committed read spent the mark by acting on the value it \
         read: {:?}",
        subject.ran.borrow()
    );

    let _quiet = subject.bot.tick();
    let after = subject.bot.tick_report();
    assert!(
        after.forced().iter().all(|row| row.chain() != 0),
        "seed {SEED}: the mark was spent by the read that committed, so the quiet \
         tick does not force chain 0 again: {:?}",
        after.forced()
    );
    Ok(())
}

/// A seeded sequence of value changes under a held action reports each replaced
/// unacted revision exactly once, and never as a fired effect.
///
/// INV-BOT-121 in the shape a counter cannot express: the generation that first
/// acted on the value stays held, so every later value is committed and passed
/// over. `superseded` must name each replaced revision once — the intermediates
/// between the admitted first value and the newest — and `fired` must count
/// none of them, because a pass-over is not work done.
#[test]
fn a_seeded_value_sequence_under_a_held_action_reports_each_replaced_revision_once() -> TestResult {
    const SEED: u64 = 0x1210_0011;
    let mut rng = Rng::new(SEED);
    let steps = rng.between(3, 6);
    let holds = Rc::new(Cell::new(true));
    let mut subject = scripted_bot(
        "sim-observe-superseded",
        0,
        1,
        SIM_DEADLINE,
        Rc::clone(&holds),
    )?;
    let mut values: Vec<u32> = Vec::new();
    for step in 0..steps {
        values.push(step.saturating_mul(7).saturating_add(10));
    }

    let mut superseded: Vec<u64> = Vec::new();
    let mut fired = 0_usize;
    for value in &values {
        arm_chain(&subject.handles[0], *value, 0, None, 0);
        let _outcome = subject.bot.tick();
        let report = subject.bot.tick_report();
        fired = fired.saturating_add(report.fired());
        assert!(
            !report.forced_any(),
            "seed {SEED}: a pass-over is not a forced refresh: {:?}",
            report.forced()
        );
        for row in report.superseded() {
            superseded.push(row.revision());
        }
    }

    let expected: Vec<u64> = (2..u64::from(steps)).collect();
    assert_eq!(
        superseded, expected,
        "seed {SEED}: each replaced unacted revision is reported exactly once, in \
         order — the intermediates between the first, admitted value and the \
         newest"
    );
    assert_eq!(
        subject.ran.borrow().clone(),
        vec![(0, values[0])],
        "seed {SEED}: the held generation ran only on the first value, so no \
         superseded value is counted as fired"
    );
    assert_eq!(
        fired, 0,
        "seed {SEED}: no effect was counted as fired — the only generation that \
         ran reported its outcome unknown, and a pass-over is never counted as \
         either"
    );
    Ok(())
}

/// A seeded mass of held chains never starves an independent chain, at every
/// tier the host can build.
///
/// INV-BOT-122's saturation claim on one bot: the independent chain is declared
/// first and its effect lands, while every held chain occupies a transition the
/// walk reaches and cannot drop. The independent chain still runs, every held
/// chain still reaches its own action once, and every one is still *reported*
/// as held — a dropped hold is a lost effect nobody would see. The tier reached
/// is recorded rather than clamped.
#[test]
fn a_seeded_mass_of_held_chains_never_starves_an_independent_chain() -> TestResult {
    const SEED: u64 = 0x1220_0012;
    let mut rng = Rng::new(SEED);
    let bias = rng.below(64);
    let mut reached: Vec<usize> = Vec::new();
    for requested in [100_usize, 1_000, 10_000] {
        let independent = Rc::new(RefCell::new(Vec::new()));
        let held_seen = Rc::new(RefCell::new(Vec::new()));
        let first = ChainHandles::new();
        arm_chain(&first, bias.saturating_add(1), 0, None, 0);
        let mut builder = Bot::builder("sim-observe-mass")
            .with_poll_deadline(SIM_DEADLINE)
            .observe(scripted(&first, 0, 0))
            .on(
                |observed: &u32| *observed > 0,
                Logs {
                    ran: Rc::clone(&independent),
                    chain: 0,
                    holds: Rc::new(Cell::new(false)),
                },
            );
        for chain in 1..=requested {
            let held = ChainHandles::new();
            arm_chain(&held, bias.saturating_add(2), 0, None, 0);
            builder = builder.observe(scripted(&held, chain, 0)).on(
                |observed: &u32| *observed > 0,
                Logs {
                    ran: Rc::clone(&held_seen),
                    chain,
                    holds: Rc::new(Cell::new(true)),
                },
            );
        }
        let mut bot = builder
            .with_effects(tenant_scope(0)?)
            .build(&GrantSet::empty())?;
        let _outcome = bot.tick();
        assert_eq!(
            independent.borrow().len(),
            1,
            "seed {SEED} tier {requested}: the independent chain's action ran while \
             {requested} held chains occupied the walk"
        );
        assert_eq!(
            held_seen.borrow().len(),
            requested,
            "seed {SEED} tier {requested}: every held chain reached its own action \
             once: a walk that stopped at the first hold would have run one"
        );
        assert_eq!(
            bot.pending().len(),
            requested,
            "seed {SEED} tier {requested}: and every one of them is still reported \
             as held rather than dropped to make room"
        );
        assert_eq!(
            bot.source_domains().len(),
            requested.saturating_add(1),
            "seed {SEED} tier {requested}: the bot declares one independent chain and \
             {requested} held ones"
        );
        reached.push(requested);
    }
    assert_eq!(
        reached,
        vec![100, 1_000, 10_000],
        "seed {SEED}: the tiers this run reached, recorded rather than clamped, so \
         a reader is never told a concurrency number nobody ran"
    );
    Ok(())
}

/// Two tenants interleaved over one host never cross stalled, forced or
/// superseded attribution.
///
/// Two bots of different widths, so a chain index that crossed is detectable: a
/// five-chain tenant's chain 4 is impossible in a three-chain one. Each tenant
/// forces one chain, wedges another and holds its actions, so all three report
/// kinds are live on every tick and the assertion is not vacuous.
#[test]
fn two_tenants_interleaved_ticks_never_cross_attribution() -> TestResult {
    const SEED: u64 = 0x1250_0013;
    let mut rng = Rng::new(SEED);
    let ticks = rng.between(3, 6);
    let holds_left = Rc::new(Cell::new(true));
    let holds_right = Rc::new(Cell::new(true));
    let mut left = scripted_bot("sim-observe-two-left", 0, 3, SIM_DEADLINE, holds_left)?;
    let mut right = scripted_bot("sim-observe-two-right", 1, 5, SIM_DEADLINE, holds_right)?;
    let mut superseded_seen = 0_usize;

    for tick in 1..=ticks {
        for subject in [&mut left, &mut right] {
            let cause = CAUSES[usize::try_from(rng.below(4))?];
            for (index, held) in subject.handles.iter().enumerate() {
                let pace = if index == 1 { WEDGED_PACE } else { 0 };
                let declared = if index == 0 { Some(cause) } else { None };
                let value = tick
                    .saturating_mul(10)
                    .saturating_add(u32::try_from(index)?);
                arm_chain(held, value, pace, declared, 0);
            }
            let _outcome = subject.bot.tick();
            let report = subject.bot.tick_report();
            let prefix = format!("sim::t{}", subject.tenant);
            let width = subject.handles.len();
            assert!(
                report.forced().iter().any(|row| row.chain() == 0),
                "seed {SEED} tick {tick}: tenant {} forced its declared chain",
                subject.tenant
            );
            assert!(
                report.stalled().iter().any(|row| row.chain() == 1),
                "seed {SEED} tick {tick}: tenant {} stalled its wedged chain",
                subject.tenant
            );
            for row in report.forced() {
                assert!(
                    row.domain().starts_with(&prefix),
                    "seed {SEED} tick {tick}: tenant {} reported a forced source \
                     belonging to another tenant: {}",
                    subject.tenant,
                    row.domain()
                );
                assert!(
                    row.chain() < width,
                    "seed {SEED} tick {tick}: tenant {} reported a forbidden chain {}",
                    subject.tenant,
                    row.chain()
                );
            }
            for row in report.stalled() {
                assert!(
                    row.domain().starts_with(&prefix),
                    "seed {SEED} tick {tick}: tenant {} reported a stalled source \
                     belonging to another tenant: {}",
                    subject.tenant,
                    row.domain()
                );
            }
            for row in report.superseded() {
                assert!(
                    row.chain() < width,
                    "seed {SEED} tick {tick}: tenant {} reported a superseded chain \
                     it does not declare: {}",
                    subject.tenant,
                    row.chain()
                );
                superseded_seen = superseded_seen.saturating_add(1);
            }
        }
    }
    assert!(
        superseded_seen > 0,
        "seed {SEED}: the run exercised supersession, so the cross-tenant \
         assertion about it is not vacuous"
    );
    Ok(())
}

/// The deadline and watchdog families replay exactly, twice over.
///
/// `assert_replays` sweeps the band twice and compares the two hash vectors, so
/// a run that decided a different stall set — or started a different number of
/// watchers — fails even though every assertion above passed. The trace records
/// the *set* of stalled chains and the watchdog count rather than any timing, so
/// the receipt is a statement about what the run decided and not about how fast
/// it decided it.
///
/// A source yields only when no wedged source precedes it in the wave. The wave
/// takes first turns in chain order, so a yielding source ahead of every wedged
/// one takes its first turn before any reaper exists, and its second turn is
/// read however late the host schedules it. Behind a wedged source its first
/// turn lands on whichever side of the deadline the host put it — the wall
/// deadline is real time by design (INV-BOT-123) — so a yield there would make
/// the trace record the scheduler rather than the run.
#[test]
fn the_same_seed_replays_a_deadline_wave() -> TestResult {
    sim::assert_replays(sim::band_of(40), |sim| {
        let holds = Rc::new(Cell::new(false));
        let mut subject = scripted_bot("sim-observe-deadline-replay", 0, 3, SIM_DEADLINE, holds)?;
        let ticks = sim.rng().between(2, 3);
        for tick in 1..=ticks {
            let mut behind_a_wedge = false;
            for (index, held) in subject.handles.iter().enumerate() {
                let wedged = sim.rng().chance(250);
                let drawn = sim.rng().between(0, 1);
                let yields = if behind_a_wedge { 0 } else { drawn };
                behind_a_wedge = behind_a_wedge || wedged;
                let pace = if wedged { WEDGED_PACE } else { 0 };
                let value = tick
                    .saturating_mul(10)
                    .saturating_add(u32::try_from(index)?);
                arm_chain(held, value, pace, None, yields);
            }
            let _outcome = subject.bot.tick();
            let report = subject.bot.tick_report();
            sim.record(&format!("tick {tick} watchdogs {}", report.watchdogs()));
            for row in report.stalled() {
                sim.record(&format!("stalled {} {}", row.chain(), row.domain()));
            }
        }
        for entry in subject.ran.borrow().iter() {
            sim.record(&format!("ran {} {}", entry.0, entry.1));
        }
        Ok(())
    })
}
