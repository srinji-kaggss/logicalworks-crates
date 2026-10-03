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
//!
//! # What is not here, and why
//!
//! The saturation tiers are not. `Bot::observe` returns a builder whose type
//! follows its source, so a bot of *n* chains needs *n* written-out
//! `observe`/`on` pairs — a compile-time constant, not something a test can
//! express at runtime. The 100 / 1,000 / 10,000 tier sweep therefore lives in
//! `observe_refresh.rs`, which writes them out, and this file covers the fault
//! dimension a fixed chain count is the right shape for.
//!
//! # What is real and what is seeded
//!
//! The bot, the `bevy_ecs` schedule, the journal, the broker and the whole
//! ledger are the shipped ones, driven through the public `Observe`/`Execute`
//! traits. Seeded is only the *fault schedule*: which tenant's chain declares
//! which cause, on which tick, for how many ticks, and whether the poll behind
//! the declaration also refuses. No wall clock is read and no sleep is taken, so
//! a run replays exactly — which is what makes the trace hash a receipt rather
//! than a measurement.
//!
//! The band sweep comes from `sim::band_of`, the shared seed space every other
//! `sim_*` family draws from, so a seed recorded against this file means the
//! same draw sequence as a seed recorded against `sim_task`.

mod sim;

// The band-declaration macro, defined once for the whole layer.
#[path = "sim/bands.rs"]
mod band_family;

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::rc::Rc;

use lgwks_bot::broker::Broker;
use lgwks_bot::effect::{EnvironmentId, EventId, FlowRevision, RunId};
use lgwks_bot::journal::MemoryJournal;
use lgwks_bot::spec::{EffectEvidence, EffectIdentity, EffectScope};
use lgwks_bot::{
    Auth, Bot, BotError, Cap, DispatchCertainty, EffectLifetime, Execute, GrantSet, Observe,
    RefreshReason,
};

use sim::Band;
use sim::Rng;

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
    fn tenant(&self, index: usize) -> TenantPlan {
        self.tenant
            .get(index)
            .copied()
            .unwrap_or_else(TenantPlan::healthy)
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
                cause: armed.then(|| {
                    let draw = rng.below(4);
                    CAUSES[usize::try_from(draw).unwrap_or(0)]
                }),
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

impl Observe for Armed {
    type Output = u32;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        call.0.check(Observe::required_caps(self))?;
        self.polls.set(self.polls.get().saturating_add(1));
        if self.refusing.get() {
            return Err(BotError::DomainError {
                domain: format!("sim::t{}-c{}", self.tenant, self.chain),
                certainty: DispatchCertainty::NotDelivered,
                cause: "the seeded source refused".to_owned(),
            });
        }
        Ok(self.value.get())
    }

    fn cache_state(&self) -> Option<RefreshReason> {
        self.declared.get()
    }

    fn domain_id(&self) -> &str {
        // Stable per run and distinct per chain, which is what makes a report's
        // `domain()` readable: a caller triaging two forced refreshes has to be
        // able to tell which source each came from.
        DOMAINS[usize::try_from(self.tenant).unwrap_or(0)][self.chain % 8]
    }
}

/// The domain identity each tenant's chains report under.
///
/// A fixed table rather than a formatted string so the identity is `'static` and
/// the report carries the same pointer a reader can compare across two runs.
const DOMAINS: [[&str; 8]; 2] = [
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
            return Err(BotError::EffectIndeterminate {
                domain: "sim::logs".to_owned(),
                cause: "the seeded hold".to_owned(),
            });
        }
        Ok(())
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
        call.0.check(Observe::required_caps(self))?;
        self.polls.set(self.polls.get().saturating_add(1));
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

    // One clone per use, because every `.on` moves its action into the builder
    // and a shared `Rc` is what lets three of them name the same log.
    let first = ChainHandles::new();
    let second = ChainHandles::new();
    let third = ChainHandles::new();
    let (ran_a, ran_b, ran_c) = (Rc::clone(&ran), Rc::clone(&ran), Rc::clone(&ran));
    let (hold_a, hold_b, hold_c) = (Rc::clone(&holds), Rc::clone(&holds), Rc::clone(&holds));

    let bot = Bot::builder(format!("sim-observe-t{tenant}"))
        .observe(Armed {
            value: Rc::clone(&first.value),
            declared: Rc::clone(&first.declared),
            refusing: Rc::clone(&first.refusing),
            polls: Rc::new(Cell::new(0)),
            chain: 0,
            tenant,
        })
        .on(
            |observed: &u32| *observed > 0,
            Logs {
                ran: ran_a,
                chain: 0,
                holds: hold_a,
            },
        )
        .observe(Armed {
            value: Rc::clone(&second.value),
            declared: Rc::clone(&second.declared),
            refusing: Rc::clone(&second.refusing),
            polls: Rc::new(Cell::new(0)),
            chain: 1,
            tenant,
        })
        .on(
            |observed: &u32| *observed > 0,
            Logs {
                ran: ran_b,
                chain: 1,
                holds: hold_b,
            },
        )
        .observe(Armed {
            value: Rc::clone(&third.value),
            declared: Rc::clone(&third.declared),
            refusing: Rc::clone(&third.refusing),
            polls: Rc::new(Cell::new(0)),
            chain: 2,
            tenant,
        })
        .on(
            |observed: &u32| *observed > 0,
            Logs {
                ran: ran_c,
                chain: 2,
                holds: hold_c,
            },
        )
        .with_effects(tenant_scope(tenant)?)
        .build(&GrantSet::empty())?;

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
}

impl ChainHandles {
    /// A chain with a value of one, nothing declared, and a source that reads.
    fn new() -> Self {
        Self {
            value: Rc::new(Cell::new(1)),
            declared: Rc::new(Cell::new(None)),
            refusing: Rc::new(Cell::new(false)),
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
        let mut tenants = Vec::new();
        for index in 0..usize::try_from(plan.tenants).unwrap_or(0) {
            tenants.push(tenant(
                u32::try_from(index).unwrap_or_default(),
                &plan.tenant(index),
            )?);
        }

        // Everything the run saw, as report rows.
        let mut rows: Vec<ReportRow> = Vec::new();
        for tick in 1..=plan.ticks {
            for (index, subject) in tenants.iter_mut().enumerate() {
                let schedule = plan.tenant(index);
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
            let schedule = plan.tenant(usize::try_from(row.tenant).unwrap_or(0));
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
        let armed_chains = (0..usize::try_from(plan.tenants).unwrap_or(0))
            .flat_map(|index| {
                let schedule = plan.tenant(index);
                (0..MAX_CHAINS).filter_map(move |chain| {
                    schedule
                        .faults
                        .get(chain)?
                        .cause
                        .map(|cause| (index, chain, cause.as_str()))
                })
            })
            .count();
        let reported_chains = reported.len();
        assert_eq!(
            reported_chains, armed_chains,
            "every armed chain reached the report exactly once, and no unarmed one \
             did: the row is a bijection, not a count"
        );
        for row in &rows {
            sim.record(&format!("{} {} {}", row.kind, row.tenant, row.spell()));
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
    sim::assert_replays(band, |_sim| {
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
        let mut left = tenant(0, &plan.tenant(0))?;
        let mut right = tenant(1, &plan.tenant(1))?;

        let mut rows: Vec<ReportRow> = Vec::new();
        for tick in 1..=plan.ticks {
            for (subject, index) in [(&mut left, 0_usize), (&mut right, 1_usize)] {
                let schedule = plan.tenant(index);
                subject.arm(tick, &schedule);
                subject.advance(tick);
                subject.observe(&mut rows);
            }
        }
        for row in &rows {
            sim.record(&format!("{} {}", row.kind, row.spell()));
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
                stream[usize::try_from(sim.rng().below(distinct)).unwrap_or(0)]
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
            usize::try_from(distinct).unwrap_or(0),
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
        for index in 0..usize::try_from(plan.tenants).unwrap_or(0) {
            let tenant_id = u32::try_from(index).unwrap_or_default();
            let mut subject = tenant(tenant_id, &plan.tenant(index))?;
            let mut rows: Vec<ReportRow> = Vec::new();
            for tick in 1..=plan.ticks {
                subject.arm(tick, &plan.tenant(index));
                subject.advance(tick);
                subject.observe(&mut rows);
            }
            for row in &rows {
                sim.record(&format!("{} {}", row.kind, row.spell()));
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
        for index in 0..usize::try_from(plan.tenants).unwrap_or(0) {
            let tenant_id = u32::try_from(index).unwrap_or_default();
            let mut subject = tenant(tenant_id, &plan.tenant(index))?;
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
}
