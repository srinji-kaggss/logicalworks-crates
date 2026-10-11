//! The ECS bot's unit tests: shared fixtures here, cases in `test_part_*`.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::marker::PhantomData;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use super::bot::EcsBot;
use super::effects::Effects;
use super::entry::AttemptRecord;
use super::entry::EntryState;
use super::entry::RetryReading;
use super::entry::Transition;
use super::fold::fire_plan;
use super::fold::observe_fold;
use super::fold::validate;
use super::identity::derive_action_digest;
use super::identity::derive_action_id;
use super::identity::derive_input_stamp;
use super::identity::hash_parts;
use super::ledger::Ledger;
use super::resources::Chains;
use super::resources::Observed;
use super::resources::Policy;
use super::resources::Polled;
use super::resources::TickError;
use super::scope::EffectScope;
use super::watchdog::PollWatchdog;
use super::watchdog::WavePoll;
use super::work::AbandonReason;
use super::work::AdmittedInput;
use super::work::EffectEvidence;
use super::work::PendingWork;
use super::work::TransitionHold;
use super::work::WorkId;
use super::work::identify_output;
use crate::EventId;
use crate::broker::{Broker, DispatchError};
use crate::cap::{Auth, Cap, Demand};
use crate::clock::Clock;
use crate::effect::InputIdentity;
use crate::effect::{
    ActionDigest, ActionId, AttemptId, EffectIdentity, EffectKey, EnvironmentEpoch, FlowRevision,
};
use crate::effect::{EnvironmentId, RunId};
use crate::error::{BotError, DispatchCertainty, RetryClass};
use crate::gate::GrantSet;
use crate::journal::{DurabilityPromise, DurableAck, JournalPosition, MemoryJournal};
use crate::journal::{EffectEvent, EffectJournal, JournalError};
use crate::spec::{Erased, typed_entry};
use crate::verb::{EffectLifetime, Evaluate, Execute, Observe};
use lgwks_deps::bevy_ecs::{
    prelude::World,
    schedule::{IntoScheduleConfigs, LogLevel, Schedule, ScheduleBuildSettings},
};
use lgwks_std::hash::Hasher;
use lgwks_std::retry::RetryPolicy;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// A domain refusal from a test source, recorded before it is returned.
///
/// Every fixture that refuses says the same three things — which domain, that
/// nothing was delivered, and why — so they say it here once.
fn domain_refusal<T>(domain: &str, cause: &str) -> Result<T, BotError> {
    let refusal = Err(BotError::DomainError {
        domain: domain.into(),
        certainty: DispatchCertainty::NotDelivered,
        cause: cause.into(),
    });
    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "a test source refused its poll");
    refusal
}

/// A scripted source: yields the next value from `values` on each poll,
/// and refuses a poll past the end of its script rather than inventing one.
///
/// Deliberately `Rc`-based and therefore `!Send`, which is the property the
/// verbs actually have and the reason this whole module routes through
/// `NonSend` resources.
struct Script {
    values: Rc<RefCell<Vec<u16>>>,
    cursor: Rc<Cell<usize>>,
    caps: Vec<Cap>,
}

impl Script {
    fn new(values: Vec<u16>) -> Self {
        Self {
            values: Rc::new(RefCell::new(values)),
            cursor: Rc::new(Cell::new(0)),
            caps: vec![Cap::net()],
        }
    }
}

impl Observe for Script {
    type Output = u16;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u16, BotError> {
        call.0.check(&self.caps)?;
        let index = self.cursor.get();
        self.cursor.set(index.saturating_add(1));
        match self.values.borrow().get(index).copied() {
            Some(value) => Ok(value),
            None => domain_refusal("test::script", "polled past the end of its script"),
        }
    }

    fn domain_id(&self) -> &str {
        "test::script"
    }
}

/// A source that reports one value and then holds it for every later poll.
///
/// The counterpart to [`Script`], which advances on every call. A chain over
/// a scripted source moves on every tick, so it cannot tell "the chain ran
/// once" from "the chain runs every tick" — both look like work. This one
/// moves exactly once and then stands still, which is the shape that makes a
/// chain re-running settled work visible as a count rather than as a
/// coincidence.
struct Holds {
    value: u16,
    caps: Vec<Cap>,
}

impl Holds {
    fn new(value: u16) -> Self {
        Self {
            value,
            caps: vec![Cap::net()],
        }
    }
}

impl Observe for Holds {
    type Output = u16;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u16, BotError> {
        call.0.check(&self.caps)?;
        Ok(self.value)
    }

    fn domain_id(&self) -> &str {
        "test::holds"
    }
}

/// A source that counts how often it is asked for a value, and can answer
/// "has anything moved?" without being asked at all.
///
/// The counter is the point. `Holds` proves *what* a settled chain does;
/// this proves *how much it costs* — specifically that a quiet tick does
/// not call `poll`, which is the difference between the tick paying for a
/// value and the tick paying for a `u128`.
struct Counted {
    value: Rc<Cell<u16>>,
    polls: Rc<Cell<usize>>,
    caps: Vec<Cap>,
}

impl Counted {
    fn new(value: u16, polls: Rc<Cell<usize>>) -> Self {
        Self {
            value: Rc::new(Cell::new(value)),
            polls,
            caps: vec![Cap::net()],
        }
    }
}

impl Observe for Counted {
    type Output = u16;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u16, BotError> {
        call.0.check(&self.caps)?;
        self.polls.set(self.polls.get().saturating_add(1));
        Ok(self.value.get())
    }

    fn fingerprint(&self) -> Option<u128> {
        Some(u128::from(self.value.get()))
    }

    fn domain_id(&self) -> &str {
        "test::counted"
    }
}

/// A source that reports a change tick as well as a value.
struct Rev {
    value: Rc<Cell<u64>>,
    rev: Rc<Cell<u64>>,
    polls: Rc<Cell<usize>>,
    caps: Vec<Cap>,
}

impl Observe for Rev {
    type Output = u64;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u64, BotError> {
        call.0.check(&self.caps)?;
        self.polls.set(self.polls.get().saturating_add(1));
        Ok(self.value.get())
    }

    fn revision(&self) -> Option<u64> {
        Some(self.rev.get())
    }

    fn domain_id(&self) -> &str {
        "test::rev"
    }
}

/// An action over a `u64`, counting its own calls.
struct CountU64(Rc<Cell<usize>>);

impl Execute for CountU64 {
    type Input = u64;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &u64)) -> Result<(), BotError> {
        call.0.check(&[])?;
        self.0.set(self.0.get().saturating_add(1));
        Ok(())
    }

    fn domain_id(&self) -> &'static str {
        "test::count_u64"
    }
}

/// A source that fails once its script runs out.
struct Exhausting {
    remaining: Rc<Cell<usize>>,
    caps: Vec<Cap>,
}

impl Observe for Exhausting {
    type Output = u16;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u16, BotError> {
        call.0.check(&self.caps)?;
        let left = self.remaining.get();
        self.remaining.set(left.saturating_sub(1));
        if left == 0 {
            return domain_refusal("test::exhausting", "script exhausted");
        }
        Ok(200)
    }

    fn domain_id(&self) -> &str {
        "test::exhausting"
    }
}

/// A source with a digest that can fail on demand and counts its polls.
///
/// The fingerprint is pure and answers even while `fail` is set, which is
/// the contract `Observe::fingerprint` states and the shape that makes the
/// sibling-poll bug reachable: the digest moves (or never committed), the
/// poll runs and fails, and a cache that published the digest early would
/// skip the source on every later tick.
struct Switched {
    value: Rc<Cell<u16>>,
    fail: Rc<Cell<bool>>,
    polls: Rc<Cell<usize>>,
    caps: Vec<Cap>,
}

impl Switched {
    fn new(value: u16, fail: bool, polls: Rc<Cell<usize>>) -> Self {
        Self {
            value: Rc::new(Cell::new(value)),
            fail: Rc::new(Cell::new(fail)),
            polls,
            caps: vec![Cap::net()],
        }
    }
}

impl Observe for Switched {
    type Output = u16;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u16, BotError> {
        call.0.check(&self.caps)?;
        self.polls.set(self.polls.get().saturating_add(1));
        if self.fail.get() {
            return domain_refusal("test::switched", "switch is off");
        }
        Ok(self.value.get())
    }

    fn fingerprint(&self) -> Option<u128> {
        // No digest while the switch is off: a source that cannot produce a
        // value has none to offer a cheap key for, and `None` is the
        // contract's "must poll" answer. A stable digest across a failure
        // would let the cache skip the source and swallow the error.
        if self.fail.get() {
            None
        } else {
            Some(u128::from(self.value.get()))
        }
    }

    fn domain_id(&self) -> &str {
        "test::switched"
    }
}

/// A source that returns `Pending` once and then answers, with a digest.
///
/// Exists so a tick can be dropped mid-poll without hanging the test: the
/// first `poll` of the source's future parks, the caller drops the tick,
/// and the second tick finds the yield already spent and completes.
struct Yielding {
    value: u16,
    yielded: Rc<Cell<bool>>,
    polls: Rc<Cell<usize>>,
    caps: Vec<Cap>,
}

impl Yielding {
    fn new(value: u16, yielded: Rc<Cell<bool>>, polls: Rc<Cell<usize>>) -> Self {
        Self {
            value,
            yielded,
            polls,
            caps: vec![Cap::net()],
        }
    }
}

impl Observe for Yielding {
    type Output = u16;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u16, BotError> {
        call.0.check(&self.caps)?;
        self.polls.set(self.polls.get().saturating_add(1));
        let flag = Rc::clone(&self.yielded);
        std::future::poll_fn(move |cx| {
            if flag.get() {
                Poll::Ready(())
            } else {
                flag.set(true);
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        })
        .await;
        Ok(self.value)
    }

    fn fingerprint(&self) -> Option<u128> {
        Some(u128::from(self.value))
    }

    fn domain_id(&self) -> &str {
        "test::yielding"
    }
}

/// A source whose poll deliberately spans two externally controlled state
/// changes. Its digest remains an honest read of the live value; the test
/// uses the pauses to place those reads around the A → B → A schedule.
struct AbaSource {
    value: Rc<Cell<u16>>,
    phase: Rc<Cell<u8>>,
    polls: Rc<Cell<usize>>,
    caps: Vec<Cap>,
}

impl AbaSource {
    fn new(value: Rc<Cell<u16>>, phase: Rc<Cell<u8>>, polls: Rc<Cell<usize>>) -> Self {
        Self {
            value,
            phase,
            polls,
            caps: vec![Cap::net()],
        }
    }
}

impl Observe for AbaSource {
    type Output = u16;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u16, BotError> {
        call.0.check(&self.caps)?;
        self.polls.set(self.polls.get().saturating_add(1));
        let value = Rc::clone(&self.value);
        let phase = Rc::clone(&self.phase);
        let captured = Rc::new(Cell::new(0));
        let captured = std::future::poll_fn(move |cx| match phase.get() {
            0 => {
                phase.set(1);
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            1 => {
                captured.set(value.get());
                phase.set(2);
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            _ => {
                phase.set(0);
                Poll::Ready(captured.get())
            }
        })
        .await;
        Ok(captured)
    }

    fn fingerprint(&self) -> Option<u128> {
        Some(u128::from(self.value.get()))
    }

    fn domain_id(&self) -> &str {
        "test::aba"
    }
}

/// An action that counts how many times it ran.
struct Count(Rc<Cell<usize>>);

impl Execute for Count {
    type Input = u16;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &u16)) -> Result<(), BotError> {
        call.0.check(&[])?;
        self.0.set(self.0.get().saturating_add(1));
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::count"
    }
}

/// An action that records what it saw, so a test can assert on a
/// *persistent* effect rather than on a call count.
struct Record(Rc<RefCell<Vec<u16>>>);

impl Execute for Record {
    type Input = u16;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &u16)) -> Result<(), BotError> {
        call.0.check(&[])?;
        self.0.borrow_mut().push(*call.1);
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::record"
    }
}

/// An action that fails a fixed number of times and then records: the
/// retry half of a chain, with a persistent effect to assert on.
struct Flaky {
    remaining: Rc<Cell<usize>>,
    log: Rc<RefCell<Vec<u16>>>,
    kind: FlakyKind,
}

/// Which failure the action reports, because the two are not the same
/// question and the substrate must treat them differently.
#[derive(Clone, Copy)]
enum FlakyKind {
    /// The effect definitely did not happen.
    Refused,
    /// The effect may or may not have happened.
    Indeterminate,
}

impl Execute for Flaky {
    type Input = u16;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &u16)) -> Result<(), BotError> {
        call.0.check(&[])?;
        let left = self.remaining.get();
        if left == 0 {
            self.log.borrow_mut().push(*call.1);
            return Ok(());
        }
        self.remaining.set(left.saturating_sub(1));
        Err(match self.kind {
            FlakyKind::Refused => BotError::DomainError {
                domain: "test::flaky".into(),
                certainty: DispatchCertainty::NotDelivered,
                cause: "refused, and the effect did not happen".into(),
            },
            FlakyKind::Indeterminate => BotError::EffectIndeterminate {
                domain: "test::flaky".into(),
                cause: "the acknowledgment never arrived".into(),
            },
        })
    }

    fn domain_id(&self) -> &str {
        "test::flaky"
    }
}

/// The `(chain, entry)` identity of a pending report.
fn identity(work: &PendingWork) -> (usize, usize) {
    (work.id().chain(), work.id().entry())
}

/// The identities of every pending report, in order.
fn identities(bot: &EcsBot) -> Vec<(usize, usize)> {
    bot.pending().iter().map(identity).collect()
}

/// What is holding the first pending report, if anything is pending.
fn first_hold(bot: &EcsBot) -> Option<TransitionHold> {
    bot.pending().first().map(|work| work.hold().clone())
}

mod test_support;
pub(crate) use test_support::test_effects;
use test_support::{
    AltTick, CountsU32, FaultJournal, NeedsCaps, NoSettlementRoom, PrefixProbe,
    PrefixProbeNextSchema, PrefixProbeTail, Refuses, RefusesToEvaluate, SCRIPT,
    SwappedEntryJournal, TEST_ENV, TEST_FLOW, TEST_RUN, Unpolled, counting_bot, held_key,
    indeterminate_once_bot, net_grants, refused, test_effects_with,
};

mod test_part_00;
mod test_part_01;
mod test_part_02;
mod test_part_03;

// The cases INVARIANTS.md cites by path (INV-BOT-41, -42, -123) live here,
// at `ecs::tests::<name>`, so the `invariants` lane resolves them.

/// A yielding source turned once on a live wave, then again after the wave
/// expired, answers with its value; a source still pending on that turn is
/// the one reported stalled.
///
/// This is the interleaving a loaded host produces and an idle one does
/// not: the reaper fires between a source's `Pending` and the turn it woke
/// itself for. `join` waits on the real reaper, which only returns once the
/// wave has expired because both polls are still alive, so the interleaving
/// is forced rather than raced, and a wave that cut off the yielding source
/// unpolled fails here on every run.
#[test]
fn an_expired_wave_still_reads_a_source_ready_on_its_next_turn() -> TestResult {
    let deadline = Duration::from_millis(1);
    let watchdog = PollWatchdog::new(deadline, 2);
    let clock = Clock::wall();
    let armed = AtomicBool::new(false);
    let mut cx = Context::from_waker(Waker::noop());
    let yields = Rc::new(Cell::new(1_u32));
    let mut yielding = WavePoll {
        chain: 0,
        watchdog: &watchdog,
        slot: 0,
        installed: false,
        poll: Box::pin(std::future::poll_fn({
            let yields = Rc::clone(&yields);
            move |turn: &mut Context<'_>| {
                if yields.get() > 0 {
                    yields.set(yields.get().saturating_sub(1));
                    turn.waker().wake_by_ref();
                    Poll::Pending
                } else {
                    Poll::Ready(Ok(None))
                }
            }
        })),
    };
    let mut wedged = WavePoll {
        chain: 1,
        watchdog: &watchdog,
        slot: 1,
        installed: false,
        poll: Box::pin(std::future::pending()),
    };
    assert!(
        yielding
            .turn(&mut cx, &clock, &armed, deadline)
            .is_pending(),
        "a source that yields is pending on its first turn"
    );
    assert!(
        wedged.turn(&mut cx, &clock, &armed, deadline).is_pending(),
        "a wedged source is pending on its first turn"
    );
    assert!(
        armed.load(Ordering::Relaxed),
        "a wave with a pending poll starts its reaper"
    );
    watchdog.join();
    assert!(
        watchdog.expired(),
        "the reaper returned on the wave's expiry"
    );
    assert!(
        matches!(
            yielding.turn(&mut cx, &clock, &armed, deadline),
            Poll::Ready(Ok(None))
        ),
        "a source ready on the turn after expiry answers rather than stalls"
    );
    assert!(
        matches!(
            wedged.turn(&mut cx, &clock, &armed, deadline),
            Poll::Ready(Err(BotError::PollStalled { chain: 1, .. }))
        ),
        "a source still pending after its last turn is the one cut off"
    );
    assert_eq!(
        yields.get(),
        0,
        "the yielding source was turned exactly twice"
    );
    Ok(())
}

/// The applied-key index answers the same questions the vector did, at a
/// history size where scanning before every push was quadratic.
///
/// This drives the shipped path: [`Effects::note_applied`] is what the live
/// settle and the recovered seeding both call, and [`Effects::applied_in`]
/// is what the walk asks before it dispatches. Each key's digest is the one
/// `applied_in` recomputes for its coordinates, which is the discriminating
/// part — a wrong index answers false for a key it holds. The old
/// `Vec<EffectKey>` would need Θ(n²) comparisons to build this set; the
/// indexed form builds and queries it in one pass.
#[test]
fn applied_membership_answers_at_scale_where_a_scan_would_be_quadratic() -> TestResult {
    /// Distinct actions, one per obligation. Large enough that the removed
    /// scan-before-push is visibly quadratic, small enough for a suite.
    const COUNT: u64 = 50_000;

    /// A distinct 16-byte action id for `index`.
    fn action_for(index: u64) -> Result<ActionId, Box<dyn std::error::Error>> {
        Ok(ActionId::from_hex(&format!(
            "{:032x}",
            index.wrapping_add(1)
        ))?)
    }
    /// The coordinates `applied_in` would ask about for `index`.
    fn coordinates(index: u64) -> Result<(usize, usize), Box<dyn std::error::Error>> {
        let chain = index.checked_rem(16).ok_or("sixteen chains is not zero")?;
        let entry = index.checked_div(16).ok_or("sixteen chains is not zero")?;
        Ok((usize::try_from(chain)?, usize::try_from(entry)?))
    }

    let scope = test_effects()?;
    let mut effects = Effects::new(scope, JournalPosition::genesis());
    let identity = effects.identity();
    let flow = identity.flow();
    let run = identity.run();
    let environment = identity.environment();
    let epoch = EnvironmentEpoch::from_decimal("1")?;
    let attempt = AttemptId::from_decimal("1")?;
    let input = [0u8; 16];

    for index in 0..COUNT {
        let (chain, entry) = coordinates(index)?;
        let key = crate::effect::EffectIdentity::new(run, environment, flow).key(
            action_for(index)?,
            attempt,
            derive_action_digest(flow, chain, entry, &input),
            epoch,
        );
        effects.note_applied(key);
    }

    // A duplicate settlement must not grow either index.
    let (chain, entry) = coordinates(0)?;
    effects.note_applied(
        crate::effect::EffectIdentity::new(run, environment, flow).key(
            action_for(0)?,
            attempt,
            derive_action_digest(flow, chain, entry, &input),
            epoch,
        ),
    );
    assert_eq!(
        effects.applied_events.len(),
        usize::try_from(COUNT)?,
        "a duplicate settlement changed the event-identity index"
    );
    assert_eq!(
        effects.applied_latest.len(),
        usize::try_from(COUNT)?,
        "a duplicate settlement changed the latest-key index"
    );

    for index in 0..COUNT {
        let (chain, entry) = coordinates(index)?;
        let action = action_for(index)?;
        assert!(
            effects.applied_in(action, chain, entry, input, true),
            "the event identity for obligation {index} is absent from the index"
        );
        assert!(
            effects.applied_in(action, chain, entry, input, false),
            "the contract identity for obligation {index} is absent from the index"
        );
    }

    // An action the run never applied is not held, in either identity.
    let absent = action_for(COUNT)?;
    assert!(
        !effects.applied_in(absent, 0, 0, input, true)
            && !effects.applied_in(absent, 0, 0, input, false),
        "an unapplied obligation must not resolve as done"
    );
    Ok(())
}

/// An external handoff is refused before any rung when the settlement
/// record does not fit, so no attempt is ever left admitted and unable to
/// settle.
///
/// Acceptance for #122 item 2 / #156: the reservation happens before the
/// first write, so a journal that cannot hold the settlement refuses the
/// handoff and the store stays untouched — rather than admitting the intent
/// and the preparation and then being unable to persist the outcome.
#[test]
fn an_external_handoff_reserves_settlement_capacity_before_any_rung() -> TestResult {
    let appends = Rc::new(Cell::new(0));
    let journal = NoSettlementRoom {
        inner: MemoryJournal::new(),
        appends: Rc::clone(&appends),
    };
    let scope = test_effects_with(Box::new(journal))?;
    let mut effects = Effects::new(scope, JournalPosition::genesis());
    let key = crate::effect::EffectIdentity::new(
        RunId::from_hex(TEST_RUN)?,
        EnvironmentId::from_hex(TEST_ENV)?,
        FlowRevision::from_tagged("blake3_256", TEST_FLOW)?,
    )
    .key(
        ActionId::from_hex("1112131415161718191a1b1c1d1e1f20")?,
        AttemptId::from_decimal("1")?,
        ActionDigest::from_tagged(
            "blake3_256",
            "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeffe0e1e2e3e4e5e6e7e8e9eaebecedeeef",
        )?,
        EnvironmentEpoch::from_decimal("1")?,
    );

    match lgwks_std::task::block_on(effects.prepare(key, EffectLifetime::External)) {
        Err(DispatchError::Journal(JournalError::CapacityExceeded { .. })) => {}
        Err(other) => {
            return Err(format!("expected a capacity refusal, got {other:?}").into());
        }
        Ok(_) => {
            return Err("a handoff with no settlement room must be refused".into());
        }
    }
    assert_eq!(
        appends.get(),
        0,
        "the handoff must be refused before the first rung is written"
    );
    Ok(())
}
