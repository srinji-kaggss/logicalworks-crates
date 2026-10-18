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
//! # Layout
//!
//! The rig below (the schedule, the tenants, the sources, the shared constants
//! and the scripted source) is this module's. The band families are in
//! `bands`, the stall families in `stalls`, the scripted-wave families in
//! `sim_scripted_waves` and `sim_scripted_reports`, and the rule that decides
//! which chain of a wave may yield in `sim_yield_rule`. The three modules that
//! hold `#[test]` functions carry the `sim_` prefix so a file name says what the
//! tests in it are. The band declarations stay at the foot of this file so every
//! band test keeps its name.
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
/// shares is everything *but* the source, so [`tenant`] and `paced_tenant`
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

/// The poll deadline every stall and scripted family builds its bot under.
///
/// A wedged source costs exactly this budget, in real time, on every tick it is
/// wedged: the poll deadline is a real-time watchdog by design (see
/// `bounded_wave`), so a realistic 30-second default would turn a band into
/// minutes of waiting. What the families observe does not depend on the number —
/// every assertion is about *which* chain was reported and whether the others
/// still committed — and the deadline is declared rather than faked, so the same
/// machinery under test is the one `Bot::tick` runs in production.
///
/// One millisecond rather than more because nothing here races it: a source
/// that is not wedged answers on its first turn, and a first turn is always
/// polled however early the wave's reaper fires. A source that yields once is
/// read on the turn after its `Pending` whenever that turn lands, because an
/// expired wave turns every poll once more before cutting it off; only a
/// yielder's *first* turn can fall behind a deadline an earlier `Pending` chain
/// armed, which is why the replay family arms a yield only ahead of every
/// other `Pending` chain (`yield_rule`). At 40 ms the stall families spent ~12 s
/// a band waiting.
const SIM_DEADLINE: std::time::Duration = std::time::Duration::from_millis(1);

/// The cell value that means "never resolves".
///
/// A named constant rather than a `bool` next to the value cell so the two
/// channels cannot be confused by a call site that sets the wrong one: the value
/// cell is a `u32` because the source's output is one, and a `bool` there would
/// compile and mean nothing.
const WEDGED_PACE: u32 = u32::MAX;

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

mod bands;
mod sim_scripted_reports;
mod sim_scripted_waves;
mod sim_yield_rule;
mod stalls;

band_family::band_family! {
    forced_refresh_band_00 => bands::forced_refresh_matches_the_schedule, 0;
    forced_refresh_band_01 => bands::forced_refresh_matches_the_schedule, 1;
    forced_refresh_band_02 => bands::forced_refresh_matches_the_schedule, 2;
    forced_refresh_band_03 => bands::forced_refresh_matches_the_schedule, 3;
    forced_refresh_band_04 => bands::forced_refresh_matches_the_schedule, 4;
    forced_refresh_band_05 => bands::forced_refresh_matches_the_schedule, 5;
    a_refresh_that_never_lands_band_06 => bands::a_refresh_that_never_lands_stays_marked, 6;
    a_refresh_that_never_lands_band_07 => bands::a_refresh_that_never_lands_stays_marked, 7;
    a_refresh_that_never_lands_band_08 => bands::a_refresh_that_never_lands_stays_marked, 8;
    a_refresh_that_never_lands_band_09 => bands::a_refresh_that_never_lands_stays_marked, 9;
    tenants_never_cross_band_10 => bands::tenants_never_cross, 10;
    tenants_never_cross_band_11 => bands::tenants_never_cross, 11;
    tenants_never_cross_band_12 => bands::tenants_never_cross, 12;
    tenants_never_cross_band_13 => bands::tenants_never_cross, 13;
    event_identities_band_14 => bands::event_identities_are_per_event, 14;
    event_identities_band_15 => bands::event_identities_are_per_event, 15;
    event_identities_band_16 => bands::event_identities_are_per_event, 16;
    event_identities_band_17 => bands::event_identities_are_per_event, 17;
    same_seed_band_18 => bands::the_same_seed_replays, 18;
    same_seed_band_19 => bands::the_same_seed_replays, 19;
    same_seed_band_20 => bands::the_same_seed_replays, 20;
    same_seed_band_21 => bands::the_same_seed_replays, 21;
    same_seed_band_22 => bands::the_same_seed_replays, 22;
    same_seed_band_23 => bands::the_same_seed_replays, 23;
    same_seed_band_24 => bands::the_same_seed_replays, 24;
    same_seed_band_25 => bands::the_same_seed_replays, 25;
    wedged_sources_band_26 => stalls::a_wedged_source_is_reported_and_costs_its_neighbours_nothing, 26;
    wedged_sources_band_27 => stalls::a_wedged_source_is_reported_and_costs_its_neighbours_nothing, 27;
    wedged_sources_band_28 => stalls::a_wedged_source_is_reported_and_costs_its_neighbours_nothing, 28;
    wedged_sources_band_29 => stalls::a_wedged_source_is_reported_and_costs_its_neighbours_nothing, 29;
    saturation_band_30 => stalls::a_saturated_wave_stalls_every_chain_and_still_lets_the_next_tenant_commit, 30;
    saturation_band_31 => stalls::a_saturated_wave_stalls_every_chain_and_still_lets_the_next_tenant_commit, 31;
    stall_replay_band_32 => stalls::the_same_seed_replays_a_stalled_wave, 32;
    stall_replay_band_33 => stalls::the_same_seed_replays_a_stalled_wave, 33;
    watchdog_budget_band_34 => stalls::a_wave_spends_one_watchdog_and_a_fast_wave_spends_none, 34;
    watchdog_budget_band_35 => stalls::a_wave_spends_one_watchdog_and_a_fast_wave_spends_none, 35;
}
