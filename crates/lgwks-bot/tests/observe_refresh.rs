//! Acceptance for the observation layer's failure and supersession rows.
//!
//! Three rows of `docs/orchestration-acceptance.spec.md`, each driven through
//! the public surface only:
//!
//! | Row | What it asks | What this file observes |
//! |---|---|---|
//! | **T08** (LC-04) | Disconnect, watch overflow, stale remote key and invalidation failure force refresh rather than permanent quiet state | Each of the four reasons, armed on one source and left armed, forces the next tick to re-read it and appear in `TickReport::forced` with that reason. A forced read of an *unchanged* value commits nothing and fires nothing; a forced read of a *changed* one commits the newer value and then returns to quiet, so the repair is not "re-read forever". A refresh that *fails* leaves the mark standing. |
//! | **T09** (DX-07) | Identical payloads with different event ids both execute; a duplicate delivery does not. Latest-state mode reports supersession separately | Two `EventId`s over equal payloads are two admitted inputs and fire twice; the same id delivered again retires, as does a redelivery of an *older* id. An intermediate value overtaken before it was acted on is reported in `TickReport::superseded` with the revision it was committed under — neither a fired effect nor a retire. |
//! | **T06** (LC-03) | A saturated task A cannot starve independent B; one slow source does not block unrelated ready work | The saturation half, in two tests: a chain whose entries are all held open, and a mass of 100 / 1,000 / 10,000 such chains, never starves an independent chain declared beside it. The slow-source half is **not** proved, and the section that would have proved it says exactly why the architecture does not permit it. |
//!
//! # What is not proved here, stated before the tests rather than after
//!
//! T06's second clause — *one slow source does not block unrelated ready work* —
//! is not something this build can do. A tick's observation phase joins every
//! source in a wave through `lgwks_std::task::join_all_boxed`, which polls them
//! on the calling thread and returns only when the wave has resolved; a source
//! that never resolves therefore holds the tick, and no other chain's action can
//! run before the walk, which is after it. `MAX_IN_FLIGHT_POLLS` bounds the
//! fan-out, not the wait. The section on T06 sets this out in full.
//!
//! # Why the oracle is a controlled completion
//!
//! Every ordering here is established by the shape of a run the test can read —
//! a held entry, a value that moved, a log of what ran — and never by a clock.
//! T06's own text forbids a sleep as the oracle: a sleep says "the other thing
//! probably finished", and on a loaded machine it stops being true.
//!
//! # What is real
//!
//! Real `Bot`, real `bevy_ecs` schedule, real `MemoryJournal` behind a real
//! `EffectScope` with a real `Broker`. The sources and actions are the smallest
//! ones that can express the row, and every one of them reports through the
//! public `Observe` / `Execute` traits — nothing reaches past them into the
//! crate's internals.

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::rc::Rc;

use lgwks_bot::broker::Broker;
use lgwks_bot::effect::{EnvironmentId, EventId, FlowRevision, RunId};
use lgwks_bot::journal::MemoryJournal;
use lgwks_bot::spec::EffectScope;
use lgwks_bot::{
    Auth, Bot, BotError, Cap, DispatchCertainty, EffectLifetime, Execute, GrantSet, Observe,
    RefreshReason,
};

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

// ── The effect scope these tests run under ─────────────────────────────────

/// The effect scope these tests run under.
///
/// One definition for the whole file. A scope is a run identity, an environment
/// fence and a journal, and all three are single-use: `assemble` folds every
/// attempt the journal names and refuses a record naming an action this bot does
/// not declare. Sharing one across two bots would therefore be refused rather
/// than silently shared, so each test gets its own — which also keeps one test's
/// uncertainty out of another's recovery.
fn test_effects() -> Result<EffectScope, Box<dyn Error>> {
    let environment = EnvironmentId::from_hex("2122232425262728292a2b2c2d2e2f30")?;
    let mut broker = Broker::new();
    broker.register(environment)?;
    Ok(EffectScope::new(
        lgwks_bot::spec::EffectIdentity::new(
            RunId::from_hex("0102030405060708090a0b0c0d0e0f10")?,
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

// ── Sources ────────────────────────────────────────────────────────────────

/// A source whose value is the test's, and whose cache declaration and refusal
/// are both the test's.
///
/// The declaration is read through the public [`Observe::cache_state`] on the
/// tick after the poll that reached the failure, which is the ordering the trait
/// documents: the reason is a statement about state the poll already touched.
struct Declarable {
    /// The value this poll reports.
    value: Rc<Cell<u32>>,
    /// What `cache_state` answers, read fresh on every call.
    reason: Rc<Cell<Option<RefreshReason>>>,
    /// Whether the next poll refuses rather than reading.
    refusing: Rc<Cell<bool>>,
    /// How many times the body ran.
    polls: Rc<Cell<u32>>,
    /// The domain identity a report names this source by.
    domain: &'static str,
}

impl Observe for Declarable {
    type Output = u32;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        call.0.check(Observe::required_caps(self))?;
        self.polls.set(self.polls.get().saturating_add(1));
        if self.refusing.get() {
            return Err(BotError::DomainError {
                domain: self.domain.to_owned(),
                certainty: DispatchCertainty::NotDelivered,
                cause: "the source refused to read".to_owned(),
            });
        }
        Ok(self.value.get())
    }

    fn cache_state(&self) -> Option<RefreshReason> {
        self.reason.get()
    }

    fn domain_id(&self) -> &str {
        self.domain
    }
}

/// An event source: the next `EventId` the test queued, and the queue's
/// remaining length.
struct EventFeed {
    /// The events still to be delivered, in order.
    queue: Rc<RefCell<Vec<EventId<u32>>>>,
    /// How many times the body ran.
    polls: Rc<Cell<u32>>,
    /// The domain identity a report names this source by.
    domain: &'static str,
}

impl Observe for EventFeed {
    type Output = EventId<u32>;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<EventId<u32>, BotError> {
        call.0.check(Observe::required_caps(self))?;
        self.polls.set(self.polls.get().saturating_add(1));
        self.queue.borrow_mut().pop().ok_or(BotError::DomainError {
            domain: self.domain.to_owned(),
            certainty: DispatchCertainty::NotDelivered,
            cause: "the event feed is drained".to_owned(),
        })
    }

    fn domain_id(&self) -> &str {
        self.domain
    }
}

// ── Actions ────────────────────────────────────────────────────────────────

/// An action that lands, and records the value it ran with.
///
/// The log is the oracle for T06 and T09: it is the only place a run's order is
/// visible from outside, so every ordering assertion in this file reads it
/// rather than a counter.
struct Lands {
    /// Every value this action ran with, in the order it ran.
    seen: Rc<RefCell<Vec<u32>>>,
}

impl Execute for Lands {
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
        self.seen.borrow_mut().push(*call.1);
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::lands"
    }
}

/// An action that never settles: its attempt may still be live.
///
/// This is how a generation stays *open* long enough for a newer observation to
/// overtake it, which is the situation latest-state mode has to report. It
/// returns the action's own indeterminate error rather than a generic refusal,
/// because an indeterminate effect is the only one of the four certainties that
/// leaves the entry held instead of decided.
struct NeverSettles {
    /// Every value this action ran with, in the order it ran.
    seen: Rc<RefCell<Vec<u32>>>,
}

impl Execute for NeverSettles {
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
        self.seen.borrow_mut().push(*call.1);
        Err(BotError::EffectIndeterminate {
            domain: "test::never_settles".to_owned(),
            cause: "the acknowledgement never arrived".to_owned(),
        })
    }

    fn domain_id(&self) -> &str {
        "test::never_settles"
    }
}

// ── Shared assertions ──────────────────────────────────────────────────────

/// An action that lands over an event, recording the event id it ran with.
///
/// A separate type from [`Lands`] rather than a generic one: `Execute` is the
/// crate's public verb and a generic action would make every call site here
/// carry a type parameter for the difference between "a value moved" and "an
/// event arrived", which is the difference the whole of T09 is about.
struct LandsEvents {
    /// Every event id this action ran with, in the order it ran.
    seen: Rc<RefCell<Vec<u64>>>,
}

impl Execute for LandsEvents {
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
        self.seen.borrow_mut().push(call.1.id());
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::lands_events"
    }
}

/// Every value an action ran with, in order.
fn ran_with(seen: &Rc<RefCell<Vec<u32>>>) -> Vec<u32> {
    seen.borrow().clone()
}

/// The tick that fired, or a named failure.
///
/// Ticking returns `Err` for a held transition as well as for a fault, and this
/// file needs to say which it saw — "the tick failed" is not an observation, and
/// several rows are about a run that fires nothing precisely because something
/// was held.
fn tick(bot: &mut Bot) -> TestResult {
    match bot.tick() {
        Ok(fired) => {
            assert_eq!(
                fired,
                bot.tick_report().fired(),
                "the tick's count and its report are one fact read twice: {fired} then {}",
                bot.tick_report().fired()
            );
            Ok(())
        }
        Err(BotError::PendingTransition { .. }) => Ok(()),
        Err(error) => Err(format!("the tick failed with {error:?}").into()),
    }
}

/// Run one tick whose source is expected to refuse, and assert it said so.
///
/// Separate from [`tick`] because a refusing source and a healthy one want
/// opposite reports, and folding them into one helper would mean every caller
/// re-asserting the distinction it came here to read. The variant is matched
/// exactly rather than accepted as "any error": a refusal that arrived as a
/// `TypeMismatch` or a `PendingTransition` would be a different defect wearing
/// the same silence.
fn refused_tick(bot: &mut Bot) -> TestResult {
    match bot.tick() {
        Ok(fired) => Err(format!(
            "a source that refused read as a {fired}-effect quiet tick; the \
             refusal has to be reported"
        )
        .into()),
        Err(error) => {
            assert!(
                matches!(error, BotError::DomainError { .. }),
                "the refusal is the source's own error and keeps its type: {error:?}"
            );
            Ok(())
        }
    }
}

/// Run one tick whose action is expected to leave its entry held, and assert the
/// hold was reported.
///
/// The third distinct tick outcome in this file, beside a clean tick and a
/// refusing source. A hold is reported as an indeterminate effect on the tick
/// whose walk reaches the attempt, and as `PendingTransition` on a later tick
/// that finds the same attempt still held — both are the substrate refusing to
/// call an unknown outcome a decision, which is the property under test. Folding
/// either into [`tick`] would mean every caller re-deciding which of the three
/// it was looking at.
fn held_tick(bot: &mut Bot) -> TestResult {
    match bot.tick() {
        Ok(fired) => Err(format!(
            "an action whose outcome was unknown read as a {fired}-effect clean \
             tick; the hold has to be reported"
        )
        .into()),
        Err(error) => {
            assert!(
                matches!(
                    error,
                    BotError::EffectIndeterminate { .. } | BotError::PendingTransition { .. }
                ),
                "the hold keeps a typed variant — a retry classifier reads it, and \
                 neither arm of this match is a decision: {error:?}"
            );
            Ok(())
        }
    }
}

// ── T08: a declared failure forces a refresh, not a permanent quiet state ───

/// Each of T08's four causes, as the pair a test needs to arm one.
///
/// Written as a table rather than four hand-written tests because the row is a
/// claim about *all four*: a repair that handles disconnect and silently misses
/// the overflow passes any test that only checks disconnect.
const CAUSES: [(RefreshReason, &str); 4] = [
    (RefreshReason::Disconnected, "disconnected"),
    (RefreshReason::WatchOverflow, "watch-overflow"),
    (RefreshReason::StaleRemoteKey, "stale-remote-key"),
    (RefreshReason::InvalidationFailed, "invalidation-failed"),
];

/// The handles a test needs to arm one declarable source and read what it did.
///
/// A struct rather than a five-tuple: the tuple's shape is written three times
/// across this file, and a named field is what makes `polls` distinguishable
/// from `value` at a call site rather than by position.
struct DeclarableRig {
    /// The bot under test.
    bot: Bot,
    /// The value the source reports.
    value: Rc<Cell<u32>>,
    /// What `cache_state` answers.
    reason: Rc<Cell<Option<RefreshReason>>>,
    /// Whether the source refuses to read.
    refusing: Rc<Cell<bool>>,
    /// How many times the source was polled.
    polls: Rc<Cell<u32>>,
    /// Every value the action ran with, in order.
    seen: Rc<RefCell<Vec<u32>>>,
}

/// Build a one-chain bot over a declarable source.
fn declarable_bot(
    name: &'static str,
    domain: &'static str,
) -> Result<DeclarableRig, Box<dyn Error>> {
    let value = Rc::new(Cell::new(1));
    let reason = Rc::new(Cell::new(None));
    let refusing = Rc::new(Cell::new(false));
    let polls = Rc::new(Cell::new(0));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let source = Declarable {
        value: Rc::clone(&value),
        reason: Rc::clone(&reason),
        refusing: Rc::clone(&refusing),
        polls: Rc::clone(&polls),
        domain,
    };
    let bot = Bot::builder(name)
        .observe(source)
        .on(
            |observed: &u32| *observed > 0,
            Lands {
                seen: Rc::clone(&seen),
            },
        )
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())?;
    Ok(DeclarableRig {
        bot,
        value,
        reason,
        refusing,
        polls,
        seen,
    })
}

/// A source whose baseline the substrate is told is unsound is re-read on the
/// next tick, and the tick report names the cause.
///
/// The counterexample this closes: a source reports `Disconnected`, the
/// substrate keeps the value it last saw, and because a re-read would have
/// compared *equal* against that value the substrate never looks again. The bot
/// goes quiet and the only thing a caller can see is zero fired effects, which
/// is indistinguishable from a healthy source that held still.
#[test]
fn a_declared_failure_forces_a_refresh_rather_than_a_permanent_quiet_state() -> TestResult {
    for (cause, spelling) in CAUSES {
        let mut rig = declarable_bot("t08", "test::t08_source")?;

        // One clean tick: the source is read, the value commits, the entry runs,
        // and the chain settles. Nothing is armed, so nothing should be forced.
        tick(&mut rig.bot)?;
        let baseline_polls = rig.polls.get();
        assert!(
            baseline_polls >= 1,
            "{spelling}: the baseline tick must have polled the source at least once"
        );
        assert!(
            !rig.bot.tick_report().forced_any(),
            "{spelling}: an undeclared source is not forced — a bot that forces \
             everything would satisfy this row without reading anything"
        );
        let fired_before = ran_with(&rig.seen).len();
        assert!(
            fired_before >= 1,
            "{spelling}: the baseline tick must have fired its entry once"
        );

        // The value has not changed at all. The source says its baseline cannot
        // be trusted, and the substrate must re-read anyway: with the value
        // unchanged, a correct implementation sees "equal" here and fires
        // nothing, which is the right answer — and it only knows it is right
        // because it looked.
        rig.value.set(1);
        rig.reason.set(Some(cause));
        tick(&mut rig.bot)?;

        assert!(
            rig.polls.get() > baseline_polls,
            "{spelling}: a source that declared its cache unsound was not re-read; \
             polls stayed at {}",
            rig.polls.get()
        );
        let report = rig.bot.tick_report();
        let forced = report
            .forced()
            .first()
            .ok_or_else(|| format!("{spelling}: the tick report names no forced refresh"))?;
        assert_eq!(
            forced.reason(),
            cause,
            "{spelling}: the report must name the cause the source declared"
        );
        assert_eq!(
            forced.chain(),
            0,
            "{spelling}: the forced chain is the one whose source declared it"
        );
        assert_eq!(
            forced.domain(),
            "test::t08_source",
            "{spelling}: the report names the source by its own domain identifier"
        );
        assert_eq!(
            ran_with(&rig.seen).len(),
            fired_before,
            "{spelling}: a forced re-read of an unchanged value fires nothing — the \
             value really did not move"
        );
    }
    Ok(())
}

/// A forced refresh commits the value the source reported, not the one it
/// invalidated.
///
/// The half of T08 that is easy to get wrong in the other direction: a repair
/// that re-reads but then compares the fresh read against the baseline it
/// already distrusted would keep the stale value forever and re-observe on every
/// tick without ever converging. Here the value moves *while* the cache is
/// declared unsound, so the forced tick has to both report the forced refresh and
/// commit the newer value.
#[test]
fn a_forced_refresh_commits_the_newer_value_and_then_returns_to_quiet() -> TestResult {
    let mut rig = declarable_bot("t08-move", "test::t08_move")?;

    tick(&mut rig.bot)?;
    assert_eq!(
        ran_with(&rig.seen),
        vec![1],
        "the first generation runs on 1"
    );

    // The baseline is unsound *and* the value moved. The forced tick has to
    // commit 2, not 1: 1 is exactly the value the declaration says cannot be
    // trusted.
    rig.value.set(2);
    rig.reason.set(Some(RefreshReason::WatchOverflow));
    tick(&mut rig.bot)?;

    assert_eq!(
        ran_with(&rig.seen),
        vec![1, 2],
        "the forced refresh must commit the value the source reported, not the one \
         it declared unsound"
    );
    assert!(
        rig.bot.tick_report().forced_any(),
        "the tick that re-read a declared-unsound source reports itself as one"
    );
    let after_move = rig.polls.get();

    // The declaration is spent: the next tick has a sound baseline again, so it
    // is not forced, and a value that has not moved fires nothing. This is the
    // difference between "forced a refresh" and "stayed permanently invalid",
    // and it is the half a test that only checks the first tick would miss.
    rig.reason.set(None);
    tick(&mut rig.bot)?;
    assert!(
        !rig.bot.tick_report().forced_any(),
        "a source that declared nothing is not forced: the previous declaration was \
         spent by the read it forced"
    );
    assert_eq!(
        rig.polls.get(),
        after_move.saturating_add(1),
        "the quiet tick still polls — an unforced tick that skipped the poll would \
         pass this assertion for the wrong reason"
    );
    assert_eq!(
        ran_with(&rig.seen),
        vec![1, 2],
        "a quiet tick fires nothing, and says so rather than looking quiet because \
         it stopped looking"
    );
    Ok(())
}

/// A source whose poll fails keeps its baseline marked, so a *failed* refresh is
/// not mistaken for a completed one.
///
/// The failure mode this closes: the mark is spent by the forced poll, the poll
/// fails, and the next tick compares against the same unsound baseline it was
/// supposed to replace. The bot is quiet again for exactly the reason it was
/// quiet before, and the report says nothing.
#[test]
fn a_failed_refresh_keeps_the_baseline_marked() -> TestResult {
    let mut rig = declarable_bot("t08-fail", "test::t08_fail")?;
    tick(&mut rig.bot)?;
    let baseline = rig.polls.get();

    rig.reason.set(Some(RefreshReason::Disconnected));
    tick(&mut rig.bot)?;
    assert!(
        rig.polls.get() > baseline,
        "the armed source must be re-read"
    );
    assert_eq!(
        rig.bot
            .tick_report()
            .forced()
            .first()
            .map(|forced| forced.reason()),
        Some(RefreshReason::Disconnected),
        "the tick report names the cause"
    );

    // The source now refuses, *and* keeps declaring. The forced read never
    // landed, so the mark has to outlive the tick that made it.
    rig.refusing.set(true);
    refused_tick(&mut rig.bot)?;

    // Re-read again: the baseline is still unsound because the forced poll that
    // would have replaced it did not succeed. `declared_refresh` skips a chain
    // that is already marked, so the mark the refusal left standing is what
    // forces this one — not the reason the test happened to re-arm.
    rig.reason.set(None);
    let before_retry = rig.polls.get();
    refused_tick(&mut rig.bot)?;
    assert!(
        rig.polls.get() > before_retry,
        "a refresh that failed must leave the baseline marked, or the bot returns \
         to quiet against the value it just said it could not trust: polls stayed \
         at {}",
        rig.polls.get()
    );
    assert_eq!(
        rig.bot
            .tick_report()
            .forced()
            .first()
            .map(|forced| forced.reason()),
        Some(RefreshReason::Disconnected),
        "and the report still names it — a source that declared nothing on this \
         tick is still carrying the mark its failed refresh left"
    );
    Ok(())
}

/// Two tenants' sources over one bot: one chain's declared failure forces that
/// chain's refresh and leaves the other's quiet.
///
/// The two-tenant case, and the reason the mark is per chain rather than per
/// bot: a bot-wide flag would make one source's transport failure re-read every
/// other source forever, which is the same permanent-quiet defect wearing the
/// opposite face.
#[test]
fn two_tenants_sources_forced_refreshes_stay_attributed_to_their_own_chain() -> TestResult {
    let quiet_value = Rc::new(Cell::new(1));
    let quiet_polls = Rc::new(Cell::new(0));
    let quiet_seen = Rc::new(RefCell::new(Vec::new()));
    let broken_value = Rc::new(Cell::new(1));
    let broken_reason = Rc::new(Cell::new(None));
    let broken_polls = Rc::new(Cell::new(0));
    let broken_seen = Rc::new(RefCell::new(Vec::new()));

    let mut bot = Bot::builder("t08-tenants")
        .observe(Declarable {
            value: Rc::clone(&quiet_value),
            reason: Rc::new(Cell::new(None)),
            refusing: Rc::new(Cell::new(false)),
            polls: Rc::clone(&quiet_polls),
            domain: "test::tenant_quiet",
        })
        .on(
            |observed: &u32| *observed > 0,
            Lands {
                seen: Rc::clone(&quiet_seen),
            },
        )
        .observe(Declarable {
            value: Rc::clone(&broken_value),
            reason: Rc::clone(&broken_reason),
            refusing: Rc::new(Cell::new(false)),
            polls: Rc::clone(&broken_polls),
            domain: "test::tenant_broken",
        })
        .on(
            |observed: &u32| *observed > 0,
            Lands {
                seen: Rc::clone(&broken_seen),
            },
        )
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())?;

    tick(&mut bot)?;
    assert_eq!(ran_with(&quiet_seen), vec![1]);
    assert_eq!(ran_with(&broken_seen), vec![1]);

    let quiet_before = quiet_polls.get();
    broken_reason.set(Some(RefreshReason::StaleRemoteKey));
    tick(&mut bot)?;

    let report = bot.tick_report();
    assert_eq!(
        report.forced().len(),
        1,
        "exactly the chain that declared is forced, not the whole bot: {:?}",
        report.forced()
    );
    let forced = report.forced().first().ok_or("the broken chain is named")?;
    assert_eq!(
        forced.chain(),
        1,
        "the forced chain is the one whose source declared it"
    );
    assert_eq!(
        forced.domain(),
        "test::tenant_broken",
        "and it is named by its own domain, not the sibling's"
    );
    assert_eq!(forced.reason(), RefreshReason::StaleRemoteKey);
    assert!(
        broken_polls.get() > 1,
        "the declared-unsound source was re-read"
    );
    assert_eq!(
        quiet_polls.get(),
        quiet_before.saturating_add(1),
        "the sibling source is polled exactly once more, like any quiet tick — a \
         per-bot flag would re-read it forever"
    );
    assert_eq!(
        report.superseded(),
        &[],
        "a forced refresh is not a supersession: the value was equal, so nothing \
         was passed over"
    );
    Ok(())
}

// ── T09: event identity and latest-state supersession ──────────────────────

/// The handles a test needs to queue events and read what ran.
struct EventRig {
    /// The bot under test.
    bot: Bot,
    /// The events still to be delivered, in order.
    queue: Rc<RefCell<Vec<EventId<u32>>>>,
    /// Every event id the action ran with, in order.
    seen: Rc<RefCell<Vec<u64>>>,
}

/// Build a one-chain bot over an event feed.
fn event_bot(name: &'static str, domain: &'static str) -> Result<EventRig, Box<dyn Error>> {
    let queue = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let bot = Bot::builder(name)
        .observe(EventFeed {
            queue: Rc::clone(&queue),
            polls: Rc::new(Cell::new(0)),
            domain,
        })
        .on(
            |event: &EventId<u32>| event.id() > 0,
            LandsEvents {
                seen: Rc::clone(&seen),
            },
        )
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())?;
    Ok(EventRig { bot, queue, seen })
}

/// Two events over equal payloads are two events, and a redelivery of one is
/// one.
///
/// The row's own distinction, exercised in both directions in one run because
/// they are the same mechanism read twice: the admitted-input identity of an
/// `EventId` carries its event id, so two ids over equal payloads derive
/// different identities and both execute, while the same id delivered again
/// derives the same one and retires. A bot that keyed dedup on the payload would
/// fire the first and silently drop the second.
#[test]
fn identical_payloads_with_distinct_event_ids_both_execute_and_a_redelivery_does_not() -> TestResult
{
    let mut rig = event_bot("t09-events", "test::t09_feed")?;

    // Event 1 over payload 7, then event 2 over the *same* payload.
    rig.queue.borrow_mut().push(EventId::new(1, 7));
    tick(&mut rig.bot)?;
    assert_eq!(*rig.seen.borrow(), vec![1], "the first event executes");

    rig.queue.borrow_mut().push(EventId::new(2, 7));
    tick(&mut rig.bot)?;
    assert_eq!(
        *rig.seen.borrow(),
        vec![1, 2],
        "a second event id over an identical payload is a different event and must \
         execute; the payload alone cannot decide this"
    );

    // The same event, delivered again. The feed is drained, so the redelivery is
    // queued explicitly.
    rig.queue.borrow_mut().push(EventId::new(2, 7));
    tick(&mut rig.bot)?;
    assert_eq!(
        *rig.seen.borrow(),
        vec![1, 2],
        "a redelivery of the same event must not execute it a second time"
    );

    // And the first event comes round again, to show the dedup is over the whole
    // tagged history rather than over the last event only.
    rig.queue.borrow_mut().push(EventId::new(1, 7));
    tick(&mut rig.bot)?;
    assert_eq!(
        *rig.seen.borrow(),
        vec![1, 2],
        "a redelivery of an *older* event must not execute either — the fence is \
         over the events that have landed, not the most recent one"
    );
    Ok(())
}

/// An intermediate value overtaken before any entry acted on it is reported as
/// superseded, not as a fired effect and not as a retire.
///
/// The situation is a generation held open: the first value's action reports an
/// indeterminate outcome, so the entry is held and the transition keeps its
/// binding. A second value then arrives, and a third before the held one is
/// settled. The substrate admits only the newest once the walk releases the
/// chain, so the second value is passed over entirely — and the row asks for that
/// to be *visible*, because "fired twice" would claim work the bot never did and
/// "fired once" would hide a value the caller never saw.
#[test]
fn an_intermediate_value_is_reported_as_superseded_rather_than_fired_or_retired() -> TestResult {
    let value = Rc::new(Cell::new(1));
    let reason = Rc::new(Cell::new(None));
    let polls = Rc::new(Cell::new(0));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut bot = Bot::builder("t09-latest")
        .observe(Declarable {
            value: Rc::clone(&value),
            reason: Rc::clone(&reason),
            refusing: Rc::new(Cell::new(false)),
            polls: Rc::clone(&polls),
            domain: "test::t09_latest",
        })
        .on(
            |observed: &u32| *observed > 0,
            NeverSettles {
                seen: Rc::clone(&seen),
            },
        )
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())?;

    // Generation 1 opens and its action is held: the outcome is unknown, so the
    // entry stays outstanding and the transition keeps value 1 bound to it.
    held_tick(&mut bot)?;
    assert_eq!(
        ran_with(&seen),
        vec![1],
        "the first generation ran its action against value 1"
    );
    assert!(
        !bot.pending().is_empty(),
        "the entry is held: an indeterminate outcome is not a decision"
    );

    // Value 2 commits while generation 1 is still held. It cannot be admitted
    // over the binding, so it waits in the slot, unacted-on. The held generation
    // is still the one the walk reaches, so the tick reports the hold again
    // rather than a clean run.
    value.set(2);
    held_tick(&mut bot)?;
    assert_eq!(
        ran_with(&seen),
        vec![1],
        "the held generation keeps its own binding: value 2 must not reach an entry \
         that was acknowledged against value 1"
    );

    // Value 3 arrives before generation 1 releases. Now value 2 is passed over:
    // the substrate admits only the newest when the chain comes free, and nothing
    // ever acted on 2.
    value.set(3);
    held_tick(&mut bot)?;

    let report = bot.tick_report();
    assert_eq!(
        ran_with(&seen),
        vec![1],
        "nothing new ran: the held entry is still held"
    );
    assert!(
        report.superseded_any(),
        "an intermediate value overtaken before it was acted on must be reported; \
         report was {report:?}"
    );
    let superseded = report
        .superseded()
        .first()
        .ok_or("the pass-over is named")?;
    assert_eq!(
        superseded.chain(),
        0,
        "the pass-over names the chain whose value was overtaken"
    );
    assert_eq!(
        superseded.revision(),
        2,
        "and names the revision the *replaced* value was committed under — the one \
         that will never be acted on"
    );
    assert!(
        !report.forced_any(),
        "a pass-over is not a forced refresh: nothing about the source's caching \
         was in question"
    );
    assert_eq!(
        report.fired(),
        0,
        "a passed-over value is not work done, so it is not counted as a fired effect"
    );

    // Settle the held generation. Settling it admits the newest committed
    // observation — 3, which took the intermediate's place — and that opens the
    // next generation, so this is the tick the action runs for real. It runs
    // with `NeverSettles`, so it holds again; what matters here is the value it
    // ran with, which is the one that survived rather than the one that was
    // passed over.
    let held = bot
        .pending()
        .first()
        .cloned()
        .ok_or("the held entry is reported")?;
    let key = held
        .key()
        .ok_or("a held entry with an attempt carries its key")?;
    bot.resolve_effect(&key, lgwks_bot::spec::EffectEvidence::Applied)?;
    assert_eq!(
        bot.pending().len(),
        0,
        "the settlement decides the generation it names, and reports the chain as \
         free of held work"
    );

    held_tick(&mut bot)?;
    assert_eq!(
        ran_with(&seen),
        vec![1, 3],
        "settling generation 1 admits the newest committed value — 3 — and that is \
         the one that runs. Value 2 never ran, which is what the pass-over reported."
    );
    // Each pass-over is reported on the tick that caused it. This tick's pass-over
    // names value 3, because settling generation 1 admitted value 4 and value 3
    // was the last value nothing had acted on. One pass-over per overtaken value
    // is the property: a report that repeated the earlier one would be
    // indistinguishable from "several values were skipped on this tick".
    let overtaken = bot.tick_report().superseded().first().copied();
    assert_eq!(
        overtaken.map(|entry| (entry.chain(), entry.revision())),
        Some((0, 3)),
        "settling generation 1 admits value 4, so the value it replaces — value 3, \
         committed and never acted on — is the pass-over this tick reports"
    );
    assert_eq!(
        bot.tick_report().superseded().len(),
        1,
        "one pass-over per overtaken value, not a running total: an earlier \
         pass-over re-reported here would say two values were skipped on one tick"
    );
    Ok(())
}

// ── T06: a saturated chain does not starve an independent one ──────────────
//
// What this row can and cannot mean here, stated before the tests rather than
// discovered while writing them.
//
// The row asks two things. The first is **saturated task A cannot starve
// independent B** — a chain at its capacity ceiling must not stop an unrelated
// chain from completing. That is about the *walk*: a chain whose entries are all
// held open, and which therefore occupies the transition slot on every tick,
// must not prevent a different chain's entries from being reached and run. Both
// tests below are that claim, and both hold.
//
// The second is **one slow source does not block unrelated ready work** — and
// here the architecture does not permit it. A tick's observation phase joins
// every source in a wave with `lgwks_std::task::join_all_boxed`, which polls
// them on the calling thread and returns only when the whole wave has resolved.
// A source that never resolves therefore holds the tick, and no other chain's
// *action* can run before the observation phase completes, because the walk runs
// after it. `MAX_IN_FLIGHT_POLLS` bounds the fan-out; it does not bound the
// wait. Making a never-resolving source stop holding the tick would mean giving
// each wave a deadline, and a deadline is a policy this crate does not have:
// `rt::clock` exists and `Observe::poll` has no place to declare the budget a
// source should be given. Inventing one here would be a second admission
// surface on top of `GrantSet`, which is exactly what this crate does not keep.
//
// So the two halves are separated honestly. The saturation claims are proved;
// the slow-source claim is proved only in the form the architecture has — a
// source that is *slow* rather than *parked* delays the tick in proportion to
// its own latency and nothing more, which is what a per-source deadline would
// change and what this build does not claim.

/// The handles a test needs to read two chains.
struct PairRig {
    /// The bot under test.
    bot: Bot,
    /// The value the independent chain's source reports.
    ready_value: Rc<Cell<u32>>,
    /// How many times the saturated chain's source was polled.
    saturated_polls: Rc<Cell<u32>>,
    /// How many times the independent chain's source was polled.
    ready_polls: Rc<Cell<u32>>,
    /// Every value the saturated chain's action ran with.
    saturated_seen: Rc<RefCell<Vec<u32>>>,
    /// Every value the independent chain's action ran with.
    ready_seen: Rc<RefCell<Vec<u32>>>,
}

/// Build a two-chain bot: chain 0's action never settles, chain 1's always does.
///
/// Chain 0 is the saturated one and chain 1 the independent one, declared
/// through the same builder call chain — so there is one bot, one world and one
/// schedule step. The point of T06 is that these two live *on one substrate*,
/// not that they are two processes that happen to interleave.
///
/// `NeverSettles` is what holds chain 0 at capacity. Its entry goes to
/// `OutcomeUnknown` and stays there, so the chain has a transition it can never
/// drop: every tick walks it, finds it held, and stops there. That is the
/// saturation, and it is *bounded* — the transition is one state per entry of
/// the spec, so a held chain costs its declared slots and nothing more.
fn saturated_pair(name: &'static str) -> Result<PairRig, Box<dyn Error>> {
    let saturated_value = Rc::new(Cell::new(1));
    let saturated_polls = Rc::new(Cell::new(0));
    let saturated_seen = Rc::new(RefCell::new(Vec::new()));
    let ready_value = Rc::new(Cell::new(2));
    let ready_polls = Rc::new(Cell::new(0));
    let ready_seen = Rc::new(RefCell::new(Vec::new()));

    let bot = Bot::builder(name)
        .observe(Declarable {
            value: Rc::clone(&saturated_value),
            reason: Rc::new(Cell::new(None)),
            refusing: Rc::new(Cell::new(false)),
            polls: Rc::clone(&saturated_polls),
            domain: "test::saturated",
        })
        .on(
            |observed: &u32| *observed > 0,
            NeverSettles {
                seen: Rc::clone(&saturated_seen),
            },
        )
        .observe(Declarable {
            value: Rc::clone(&ready_value),
            reason: Rc::new(Cell::new(None)),
            refusing: Rc::new(Cell::new(false)),
            polls: Rc::clone(&ready_polls),
            domain: "test::independent",
        })
        .on(
            |observed: &u32| *observed > 0,
            Lands {
                seen: Rc::clone(&ready_seen),
            },
        )
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())?;
    Ok(PairRig {
        bot,
        ready_value,
        saturated_polls,
        ready_polls,
        saturated_seen,
        ready_seen,
    })
}

/// A chain held at capacity does not stop an independent chain from completing.
///
/// The oracle is the log of what ran, not a clock. Chain 0's action reports an
/// indeterminate outcome, so its entry is held for the rest of the run and the
/// walk stops there on every tick — that is the saturation. Chain 1's action
/// lands. If the held chain could starve the independent one, chain 1's log
/// would be empty and this fails on an exact value rather than on a timing.
///
/// The second half is the half a single tick would miss: the held chain is
/// re-walked and re-holds on *every* later tick, and the independent chain runs
/// again on each. Saturation that starves is saturation that persists, so a test
/// that only checks the first tick would pass against a repair that fixed one
/// tick's ordering and left the next one starved.
#[test]
fn a_chain_held_at_capacity_does_not_starve_an_independent_chain() -> TestResult {
    let mut rig = saturated_pair("t06-saturated")?;

    // Three ticks over a still-moving world: the first opens both generations
    // and the next two prove the hold persists without costing chain 1 anything.
    for tick_index in 1..=3_u32 {
        // Chain 0 re-holds, so its own action's error is the first failure in
        // declaration order on every tick the hold persists; chain 1 lands
        // regardless.
        held_tick(&mut rig.bot)?;
        assert_eq!(
            rig.saturated_polls.get(),
            tick_index,
            "the saturated chain's source is polled on every tick: a source that \
             stopped being read would make the saturation vacuous"
        );
        assert_eq!(
            rig.ready_polls.get(),
            tick_index,
            "the independent chain's source is polled on every tick too — a walk \
             that stopped at the held chain would never reach it"
        );
        assert_eq!(
            ran_with(&rig.saturated_seen),
            vec![1],
            "the saturated chain's action ran once across three ticks and its \
             outcome is held throughout: a second run would be a replay of an \
             effect that may already have happened"
        );
        assert_eq!(
            ran_with(&rig.ready_seen),
            vec![2],
            "the independent chain's value held still, so its entry is already \
             settled: a saturated chain cannot starve an independent one, and a \
             quiet one does not re-fire either"
        );
    }

    // The independent chain is the one that moves. It runs a new generation
    // while chain 0 is still held, which is the ordering claim rather than a
    // side effect of it: a walk that stopped at the held chain would leave this
    // log at one entry however ready chain 1 is.
    rig.ready_value.set(3);
    held_tick(&mut rig.bot)?;
    assert_eq!(
        ran_with(&rig.ready_seen),
        vec![2, 3],
        "the independent chain runs a new generation while the other chain is \
         still held at capacity"
    );
    assert_eq!(
        ran_with(&rig.saturated_seen),
        vec![1],
        "and the held chain still has not re-entered its action"
    );

    assert!(
        !rig.bot.pending().is_empty(),
        "the saturated chain's entry is still held after four ticks — this is the \
         capacity being occupied, not a tick that failed"
    );
    let held = rig.bot.pending();
    assert!(
        held.iter().all(|work| work.id().chain() == 0),
        "the only held work is chain 0's: a hold that spread to chain 1 would be \
         starvation wearing a different name — {:?}",
        held
    );
    Ok(())
}

/// A chain held at capacity does not starve an independent chain, at every
/// saturation tier the host can reach.
///
/// 100, 1,000 and 10,000 held chains declared on **one** bot alongside one
/// independent chain. Each held chain occupies a transition that the walk
/// reaches and cannot drop, so the walk's work grows with the mass while the
/// independent chain's share of it does not. The claim is that the independent
/// chain's action still runs, at every tier, and that every held chain is still
/// reported as held rather than silently dropped to make room for it.
///
/// The tier reached is recorded rather than clamped, so a host that reaches only
/// 100 does not read as though it reached 10,000.
#[test]
fn a_saturated_mass_does_not_starve_an_independent_chain_at_every_tier() -> TestResult {
    let mut reached: Vec<usize> = Vec::new();

    for requested in [100_usize, 1_000, 10_000] {
        let mass = Rc::new(RefCell::new(Vec::new()));
        let independent = Rc::new(RefCell::new(Vec::new()));

        // The independent chain is declared first, so it is chain 0 and its
        // effect is the first the walk reaches — the position a starved walk
        // would lose.
        let mut builder = Bot::builder("t06-tiers").observe(Declarable {
            value: Rc::new(Cell::new(1)),
            reason: Rc::new(Cell::new(None)),
            refusing: Rc::new(Cell::new(false)),
            polls: Rc::new(Cell::new(0)),
            domain: "test::independent",
        });
        builder = builder.on(
            |observed: &u32| *observed > 0,
            Lands {
                seen: Rc::clone(&independent),
            },
        );

        for _ in 0..requested {
            let seen = Rc::clone(&mass);
            builder = builder
                .observe(Declarable {
                    value: Rc::new(Cell::new(1)),
                    reason: Rc::new(Cell::new(None)),
                    refusing: Rc::new(Cell::new(false)),
                    polls: Rc::new(Cell::new(0)),
                    domain: "test::saturated",
                })
                .on(|observed: &u32| *observed > 0, NeverSettles { seen });
        }

        let mut bot = builder
            .with_effects(test_effects()?)
            .build(&GrantSet::empty())?;

        // The independent chain's own effect lands, so its entry never holds; the
        // saturated chains each hold and report their own indeterminate outcome,
        // and chain 0 is first in declaration order, so that is what the tick
        // reports. The chains behind it still ran.
        held_tick(&mut bot)?;

        assert_eq!(
            bot.source_domains().len(),
            requested.saturating_add(1),
            "the bot declares one independent chain and {requested} saturated ones"
        );
        assert_eq!(
            ran_with(&independent),
            vec![1],
            "at {requested} saturated chains the independent chain's action still ran"
        );
        assert_eq!(
            ran_with(&mass).len(),
            requested,
            "every saturated chain reached its own action once: a walk that stopped \
             at the first hold would have run one, not {requested}"
        );
        assert_eq!(
            bot.pending().len(),
            requested,
            "and every one of them is still reported as held rather than dropped to \
             make room — a dropped hold is a lost effect nobody would ever see"
        );
        reached.push(requested);
    }

    assert_eq!(
        reached,
        vec![100, 1_000, 10_000],
        "the tiers this run reached, recorded rather than clamped, so a reader is \
         never told a concurrency number nobody ran"
    );
    Ok(())
}
