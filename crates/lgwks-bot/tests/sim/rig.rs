//! The dispatch rig: one bot, one broker, one shared-store journal adapter.
//!
//! This module is the single home for the scaffolding that drives a bot through
//! its durable dispatch path. It is a module rather than a test target — a
//! directory under `tests/` without a `main.rs` is compiled into whichever test
//! crates declare it, which is the only way two test binaries can share code.
//!
//! It exists because the deterministic simulation families and the acceptance
//! file both need the *same* bot, the *same* broker and the *same* adapter. A
//! copy would let a family go green against a bot the acceptance file no longer
//! exercises, which is the failure this module is here to make impossible.
//!
//! Everything is the shipped surface: the real [`Bot`], the real [`Broker`],
//! the real [`MemoryJournal`] behind a real [`EffectScope`], and the real
//! `Execute` and `Observe` traits. Nothing here is a mock. The one addition is
//! the shared handle on the store, so an effect can read the journal at the
//! instant it runs — the only vantage point from which a write-ahead claim
//! means anything.

#![allow(
    dead_code,
    reason = "each test binary that declares this module uses a different subset of it"
)]

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::rc::Rc;

use lgwks_bot::broker::Broker;
use lgwks_bot::effect::{
    ActionDigest, ActionId, AttemptId, EffectKey, EnvironmentEpoch, EnvironmentId, FlowRevision,
    RunId,
};
use lgwks_bot::journal::{
    DurabilityPromise, DurableAck, EffectEvent, EffectEvidence, EffectJournal, EventKind,
    JournalEntry, JournalError, JournalPosition, MemoryJournal,
};
use lgwks_bot::spec::{Bot, EffectIdentity, EffectScope};
use lgwks_bot::{Auth, BotError, Cap, EffectLifetime, Execute, GrantSet, Observe};

/// The value [`FixedSource`] yields on every poll.
///
/// A constant rather than a per-file literal, because a source that moves
/// would make every "the chain fired once" assertion in the suite mean
/// something different, and the rig exists to keep one meaning.
pub const VALUE: u32 = 1;

// ── The store a test can read from inside an effect ────────────────────────

/// The store-forwarding half of an `EffectJournal` adapter.
///
/// Every adapter here is a [`MemoryJournal`] behind a policy, and each forwards
/// the same read and identity methods to the shared store. This macro is that
/// half, written once: a second copy is how one adapter's forwarding drifts from
/// another's, and the ladder these tests exercise must be the shipped one, not a
/// reimplementation. The type must have a `store: Rc<RefCell<MemoryJournal>>`
/// field; it supplies its own `durability` and `compare_and_append`.
macro_rules! store_forwarding {
    ($ty:ty) => {
        fn tail(&self) -> JournalPosition {
            self.store.borrow().tail()
        }

        fn committed(&self) -> Result<Vec<EffectEvent>, JournalError> {
            EffectJournal::committed(&*self.store.borrow())
        }

        fn committed_entries(&self) -> Result<Vec<JournalEntry>, JournalError> {
            Ok(self.store.borrow().committed().to_vec())
        }
    };
}

/// A journal that shares its store with the test.
///
/// An [`EffectScope`] owns its journal by value, so the only way a test can
/// read the record at the instant an effect runs is for the journal to hand out
/// a handle to the same store. That is not a test-only trick: `EffectJournal`
/// is implementable outside the crate, and an adapter holding a store it did
/// not allocate is exactly what a host with a real durable backend writes.
///
/// Every method forwards to [`MemoryJournal`] rather than reimplementing the
/// ladder, so what these tests exercise is the shipped append logic and not a
/// second copy of it that could drift.
pub struct ProbeJournal {
    /// The store, shared with the test and with any action the test arms.
    pub store: Rc<RefCell<MemoryJournal>>,
}

impl EffectJournal for ProbeJournal {
    fn durability(&self) -> DurabilityPromise {
        self.store.borrow().durability()
    }

    store_forwarding!(ProbeJournal);

    fn compare_and_append(
        &mut self,
        expected_tail: JournalPosition,
        event: &EffectEvent,
    ) -> Result<DurableAck, JournalError> {
        self.store
            .borrow_mut()
            .compare_and_append(expected_tail, event)
    }
}

/// A store-sharing adapter whose next outcome append may lose its reply.
///
/// Graded [`DurabilityPromise::ProcessCrash`] so an external handoff is
/// admitted, the two one-shot faults cover both halves of the
/// [`JournalError::OutcomeUnknown`] contract: `ambiguous_once` commits the event
/// and then drops the acknowledgment, and `unknown_once` reports an unknown
/// outcome without writing anything. [`EffectJournal::confirm_outcome`] attests
/// only an outcome actually present in the store, which is the shape a
/// file-backed adapter has.
pub struct LostReplyJournal {
    /// The store, shared with the test and with any action the test arms.
    pub store: Rc<RefCell<MemoryJournal>>,
    /// One-shot: commit the next outcome append and lose its reply.
    pub ambiguous_once: Rc<Cell<bool>>,
    /// One-shot: the next outcome append reports an unknown outcome, unwritten.
    pub unknown_once: Rc<Cell<bool>>,
}

impl LostReplyJournal {
    /// A fresh adapter: an empty store and both faults disarmed.
    pub fn new() -> Self {
        Self {
            store: Rc::new(RefCell::new(MemoryJournal::new())),
            ambiguous_once: Rc::new(Cell::new(false)),
            unknown_once: Rc::new(Cell::new(false)),
        }
    }
}

impl Default for LostReplyJournal {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectJournal for LostReplyJournal {
    fn durability(&self) -> DurabilityPromise {
        DurabilityPromise::ProcessCrash
    }

    store_forwarding!(LostReplyJournal);

    fn compare_and_append(
        &mut self,
        expected_tail: JournalPosition,
        event: &EffectEvent,
    ) -> Result<DurableAck, JournalError> {
        let is_outcome = matches!(event, EffectEvent::OutcomeObserved { .. });
        // Half two: nothing is written, the reply is still reported lost, so
        // readback proves absence and the caller may safely re-append.
        if is_outcome && self.unknown_once.take() {
            let refusal = Err(JournalError::OutcomeUnknown {
                cause: std::io::Error::other("injected unknown outcome, nothing written"),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "compare_and_append: returning an error to the caller");
            return refusal;
        }
        let acknowledgment = self
            .store
            .borrow_mut()
            .compare_and_append(expected_tail, event)?;
        // Half one: the event is in the store, the reply is gone. `Storage`
        // would claim the journal is unchanged, which the committed event
        // contradicts, so the conforming report is `OutcomeUnknown`.
        if is_outcome && self.ambiguous_once.take() {
            let refusal = Err(JournalError::OutcomeUnknown {
                cause: std::io::Error::other("injected post-commit lost reply"),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "compare_and_append: returning an error to the caller");
            return refusal;
        }
        Ok(acknowledgment)
    }

    fn confirm_outcome(
        &mut self,
        key: EffectKey,
        evidence: EffectEvidence,
        position: JournalPosition,
        required: DurabilityPromise,
    ) -> Result<DurableAck, JournalError> {
        let held = self.store.borrow().committed().iter().any(|entry| {
            matches!(*entry.event(), EffectEvent::OutcomeObserved { key: held, evidence: held_evidence }
                    if held == key && held_evidence == evidence)
        });
        if !held {
            let refusal = Err(JournalError::ReceiptUnavailable { required });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "confirm_outcome: returning an error to the caller");
            return refusal;
        }
        Ok(DurableAck::new(position, self.durability()))
    }
}

/// Every event the store has committed, in order.
///
/// Read through the trait rather than through [`MemoryJournal`]'s own
/// `committed`, which returns the entries and shadows it. What these tests are
/// about is what an [`EffectJournal`] promises, so the trait is the way in: a
/// test that read the concrete type would keep passing if the trait's promise
/// changed.
pub fn recorded(store: &Rc<RefCell<MemoryJournal>>) -> Result<Vec<EffectEvent>, JournalError> {
    EffectJournal::committed(&*store.borrow())
}

// ── The file-journal identity ──────────────────────────────────────────────

/// The run, action, flow, digest and environment every file-backed family
/// writes under.
///
/// One definition for the whole simulation layer. Four families each spelled
/// these out, and a copy that drifted would make a trace hash from one family
/// mean a different key than the same hash from another, which is exactly the
/// kind of false green this layer exists to rule out.
pub const RUN: &str = "00000000000000000000000000000001";
pub const ACTION: &str = "00000000000000000000000000000002";
pub const FLOW_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
pub const DIGEST_HEX: &str = "1f1e1d1c1b1a191817161514131211100f0e0d0c0b0a09080706050403020100";
pub const ENV: &str = "2122232425262728292a2b2c2d2e2f30";

/// The one `EffectKey::new` in the simulation layer, parameterised by the
/// action and the fencing generation.
///
/// Both public constructors below are this function. A second copy that drifted
/// would make two families agree on an attempt number while writing different
/// facts under a different generation, which is precisely the false green the
/// trace hash exists to catch — and the takeover family in particular needs two
/// keys that differ in the generation and in nothing else.
fn key_as(action: ActionId, epoch: u64, n: u64) -> Result<EffectKey, Box<dyn Error>> {
    Ok(EffectKey::new(
        RunId::from_hex(RUN)?,
        action,
        AttemptId::from_decimal(&n.to_string())?,
        FlowRevision::from_tagged("blake3_256", FLOW_HEX)?,
        ActionDigest::from_tagged("blake3_256", DIGEST_HEX)?,
        EnvironmentId::from_hex(ENV)?,
        EnvironmentEpoch::from_decimal(&epoch.to_string())?,
    ))
}

/// A key for attempt `n` of [`RUN`], acting as `action`, at generation one.
///
/// Built from the constants above rather than from anything process-local, so a
/// key is reproducible across machines and a trace hash means the same thing
/// everywhere.
pub fn attempt_key_as(action: ActionId, n: u64) -> Result<EffectKey, Box<dyn Error>> {
    key_as(action, 1, n)
}

/// A key for attempt `n` of [`RUN`] under the shared [`ACTION`], at generation
/// one.
pub fn attempt_key(n: u64) -> Result<EffectKey, Box<dyn Error>> {
    attempt_key_as(ActionId::from_hex(ACTION)?, n)
}

/// A key for attempt `n` of [`RUN`] under the shared [`ACTION`], fenced at
/// `epoch`.
///
/// The generation is the one field a takeover moves, so a family that proves a
/// stale warrant is refused and the current one is accepted needs both keys
/// differing in that field and in nothing else — which is why the generation is
/// a parameter here rather than a constant.
pub fn attempt_key_at(epoch: u64, n: u64) -> Result<EffectKey, Box<dyn Error>> {
    key_as(ActionId::from_hex(ACTION)?, epoch, n)
}

/// The real ladder for one attempt: admitted, then prepared.
///
/// The ladder is the ordering the journal enforces, so a family that writes
/// these two rungs exercises the ordering invariant rather than appending
/// arbitrary bytes and hoping.
pub fn ladder(n: u64) -> Result<Vec<EffectEvent>, Box<dyn Error>> {
    let key = attempt_key(n)?;
    Ok(vec![
        EffectEvent::IntentAdmitted { key },
        EffectEvent::DispatchPrepared { key },
    ])
}

// ── The run ────────────────────────────────────────────────────────────────

/// The three run-level facts every bot here is built against.
///
/// One definition rather than one per call site, because the restart test's
/// subject is that two bots agree on them: a second copy that drifted would
/// make the second bot's recovered key foreign, and the test would then pass
/// for the reason it exists to exclude.
pub fn identity() -> Result<EffectIdentity, Box<dyn Error>> {
    Ok(EffectIdentity::new(
        RunId::from_hex("0102030405060708090a0b0c0d0e0f10")?,
        EnvironmentId::from_hex("2122232425262728292a2b2c2d2e2f30")?,
        FlowRevision::from_tagged(
            "blake3_256",
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
        )?,
    ))
}

/// The broker for a run acting on `identity`'s environment, at its first
/// generation.
///
/// Fresh per bot rather than carried across a restart, which is what a restart
/// is: the host registers the environment again and the broker hands out the
/// generation it hands the first caller. A recovered key therefore still
/// fences — its epoch is the one a fresh registration mints — which is the
/// property the hold in these tests depends on.
pub fn broker(environment: EnvironmentId) -> Result<Broker, Box<dyn Error>> {
    let mut broker = Broker::new();
    broker.register(environment)?;
    Ok(broker)
}

/// A scope over `store`, for a bot acting on `identity`.
pub fn scope(
    identity: EffectIdentity,
    store: Rc<RefCell<MemoryJournal>>,
) -> Result<EffectScope, Box<dyn Error>> {
    Ok(EffectScope::new(
        identity,
        broker(identity.environment())?,
        Box::new(ProbeJournal { store }),
    ))
}

// ── Sources, conditions and actions ───────────────────────────────────────

/// A source that yields the same value on every poll.
///
/// The first poll still moves the observed revision, from "never polled" to
/// one, so the one chain fires once and a second tick selects nothing.
pub struct FixedSource;

impl Observe for FixedSource {
    type Output = u32;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        call.0.check(Observe::required_caps(self))?;
        Ok(VALUE)
    }

    fn domain_id(&self) -> &str {
        "test::fixed_source"
    }
}

/// `true` for the one value [`FixedSource`] yields.
pub fn is_value(seen: &u32) -> bool {
    *seen == VALUE
}

/// An action that records what the journal held at the instant it ran.
///
/// This is the write-ahead claim measured from the effect's own vantage point:
/// the record must be there *before* this body executes, not merely before the
/// tick returns. The body reads the shared store rather than a copy, so what it
/// snapshots is the kernel's own journal at the moment the effect is live.
pub struct NoteWhatIsRecorded {
    /// The journal's store, read synchronously at the moment of the effect.
    pub store: Rc<RefCell<MemoryJournal>>,
    /// What the journal held, once this body has run.
    pub seen: Rc<RefCell<Option<Vec<EventKind>>>>,
    /// The key those events were about.
    pub key: Rc<RefCell<Option<EffectKey>>>,
}

impl Execute for NoteWhatIsRecorded {
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
        let journal = self.store.borrow();
        let events: Vec<EffectEvent> = journal.events().copied().collect();
        *self.seen.borrow_mut() = Some(events.iter().map(|event| event.kind()).collect());
        *self.key.borrow_mut() = events.first().map(|event| event.key());
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::note_what_is_recorded"
    }
}

/// An action that always reports the outcome as unknown, and counts how many
/// times it was entered.
///
/// The counter is the whole instrument: "held, not retried" and "exhausted its
/// budget" both leave a pending report behind, and only the number of times the
/// body ran distinguishes them.
pub struct NeverSettles {
    /// How many times the body has been entered.
    pub entered: Rc<Cell<u32>>,
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
        self.entered.set(self.entered.get().saturating_add(1));
        Err(BotError::EffectIndeterminate {
            domain: "test::never_settles".to_owned(),
            cause: "the acknowledgement never arrived".to_owned(),
        })
    }

    fn domain_id(&self) -> &str {
        "test::never_settles"
    }
}

/// An action that succeeds, and counts how many times it was entered.
pub struct AlwaysLands {
    /// How many times the body has been entered.
    pub entered: Rc<Cell<u32>>,
}

impl Execute for AlwaysLands {
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
        self.entered.set(self.entered.get().saturating_add(1));
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::always_lands"
    }
}
/// A run's journal store together with the counter its effect increments.
///
/// One type rather than a `store()` and a `counter()` pair per family, because
/// every scenario that drives a bot is "a fresh store, an armed action, a
/// counter to watch", and spelling that out once per family is once per family
/// for the fence wiring to drift.
pub struct Run {
    /// The shared store the bot writes through.
    pub store: Rc<RefCell<MemoryJournal>>,
    /// How many times the armed effect's body has run.
    pub entered: Rc<Cell<u32>>,
}

/// The name a [`Run`]'s bots are built under by default.
pub const RUN_NAME: &str = "sim-dispatch";

impl Default for Run {
    fn default() -> Self {
        Self::new()
    }
}

impl Run {
    /// A fresh run: an empty store and a zeroed counter.
    pub fn new() -> Self {
        Self {
            store: Rc::new(RefCell::new(MemoryJournal::new())),
            entered: Rc::new(Cell::new(0)),
        }
    }

    /// A bot over this run's store armed with `action`.
    ///
    /// One builder rather than one per family, because the durable-dispatch
    /// contract cares what the bot is armed with only insofar as the journal is
    /// concerned; a hand-written builder per family is a hand-written fence
    /// per family.
    pub fn bot_named<A>(&self, name: &str, action: A) -> Result<Bot, Box<dyn Error>>
    where
        A: Execute<Input = u32, Output = ()> + 'static,
    {
        Ok(Bot::builder(name)
            .observe(FixedSource)
            .on(is_value, action)
            .with_effects(scope(identity()?, Rc::clone(&self.store))?)
            .build(&GrantSet::empty())?)
    }

    /// A bot under [`RUN_NAME`] whose effect reports itself indeterminate.
    pub fn never(&self) -> Result<Bot, Box<dyn Error>> {
        self.bot_named(
            RUN_NAME,
            NeverSettles {
                entered: Rc::clone(&self.entered),
            },
        )
    }

    /// A bot under [`RUN_NAME`] whose effect lands.
    pub fn lands(&self) -> Result<Bot, Box<dyn Error>> {
        self.bot_named(
            RUN_NAME,
            AlwaysLands {
                entered: Rc::clone(&self.entered),
            },
        )
    }

    /// Every event the store holds, through the trait rather than the concrete
    /// type: what these families are about is what the journal promises, and a
    /// test reading `MemoryJournal` directly would keep passing if the promise
    /// changed.
    pub fn recorded(&self) -> Result<Vec<EffectEvent>, Box<dyn Error>> {
        Ok(EffectJournal::committed(&*self.store.borrow())?)
    }

    /// The kinds the store holds, in order.
    pub fn kinds(&self) -> Result<Vec<EventKind>, Box<dyn Error>> {
        Ok(self.recorded()?.iter().map(|event| event.kind()).collect())
    }

    /// How many recorded events are of `kind`.
    pub fn count(&self, kind: EventKind) -> Result<usize, Box<dyn Error>> {
        Ok(self.kinds()?.iter().filter(|seen| **seen == kind).count())
    }

    /// The key the first recorded event is about.
    pub fn first_key(&self) -> Result<EffectKey, Box<dyn Error>> {
        self.recorded()?
            .first()
            .map(|event| event.key())
            .ok_or_else(|| "the first attempt must have been recorded".into())
    }

    /// The distinct keys the store holds, in first-seen order.
    pub fn distinct_keys(&self) -> Result<Vec<EffectKey>, Box<dyn Error>> {
        let mut keys: Vec<EffectKey> = Vec::new();
        for event in self.recorded()? {
            if !keys.contains(&event.key()) {
                keys.push(event.key());
            }
        }
        Ok(keys)
    }

    /// How many times the effect's body has run.
    pub fn entered(&self) -> u32 {
        self.entered.get()
    }
}
