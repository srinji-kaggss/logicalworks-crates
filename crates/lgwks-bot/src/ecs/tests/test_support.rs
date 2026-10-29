//! Shared fixtures for the ECS bot unit tests, second half.

use super::*;

/// An action that always refuses, counting the attempts it received, in
/// one of the two shapes a refusal takes.
pub(super) struct Refuses {
    pub(super) attempts: Rc<Cell<usize>>,
    pub(super) domain: &'static str,
    pub(super) certainty: DispatchCertainty,
    pub(super) cause: &'static str,
}

impl Refuses {
    /// Refuses after the attempt without delivering anything: a failure a
    /// retry may cure.
    pub(super) fn undelivered(attempts: Rc<Cell<usize>>) -> Self {
        Self {
            attempts,
            domain: "test::refuses",
            certainty: DispatchCertainty::NotDelivered,
            cause: "refused",
        }
    }

    /// Refuses permanently, before dispatch: the shape every "binding
    /// required" domain has.
    pub(super) fn permanently(attempts: Rc<Cell<usize>>) -> Self {
        Self {
            attempts,
            domain: "test::permanent",
            certainty: DispatchCertainty::Refused,
            cause: "no binding is installed for this domain",
        }
    }
}

impl Execute for Refuses {
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
        self.attempts.set(self.attempts.get().saturating_add(1));
        Err(BotError::DomainError {
            domain: self.domain.into(),
            certainty: self.certainty,
            cause: self.cause.into(),
        })
    }

    fn domain_id(&self) -> &str {
        self.domain
    }
}

/// A condition that fails structurally rather than evaluating to `false`.
///
/// Named apart from the refusing *action* above rather than sharing a name
/// with it: the two refuse in different halves of a chain, one is a condition
/// and one is an action, and a single name for both would make a call site
/// that reads `.on(Refuses, ..)` ambiguous about which half it is asserting on.
pub(super) struct RefusesToEvaluate;

impl Evaluate<u16> for RefusesToEvaluate {
    fn check(&self, observed: &u16) -> Result<bool, BotError> {
        Err(BotError::EvaluateError {
            cause: format!("this condition cannot decide about {observed}"),
        })
    }

    fn condition_id(&self) -> &str {
        "test::refuses"
    }
}

/// The action half of the same arrangement: `Count` requires nothing, so a
/// test built on it can only ever put a source's requirements in front of
/// the gate.
pub(super) struct NeedsCaps {
    pub(super) caps: Vec<Cap>,
    pub(super) domain: &'static str,
}

impl Execute for NeedsCaps {
    type Input = u16;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &u16)) -> Result<(), BotError> {
        call.0.check(&self.caps)?;
        Ok(())
    }

    fn domain_id(&self) -> &str {
        self.domain
    }
}

pub(super) fn net_grants() -> GrantSet {
    GrantSet::empty().grant(Cap::net())
}

/// The attempt a report is about, for the tests that settle one.
///
/// A `Result` rather than a bare key because an entry that has never been
/// attempted has no attempt identity, and a test that asked for one anyway
/// should say so rather than be handed a fabricated one.
pub(super) fn held_key(work: &PendingWork) -> Result<EffectKey, Box<dyn std::error::Error>> {
    work.key().ok_or_else(|| {
        format!(
            "chain {} entry {} has no attempt",
            work.id().chain(),
            work.id().entry()
        )
        .into()
    })
}

/// The run every test dispatches under.
pub(super) const TEST_RUN: &str = "0102030405060708090a0b0c0d0e0f10";

/// The environment every test dispatches against.
pub(super) const TEST_ENV: &str = "2122232425262728292a2b2c2d2e2f30";

/// The flow revision every test dispatches from.
pub(super) const TEST_FLOW: &str =
    "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

/// A scope the tests dispatch under: one run, one environment at its first
/// generation, and an in-memory journal.
///
/// Returns a `Result` rather than panicking, because the crate forbids a
/// panicking path anywhere — tests included — and a failure then names the
/// cause rather than a line number inside a macro.
///
/// The environment is registered because a broker that owns none mints no
/// authority, so an unregistered one would make every dispatch refuse with
/// `UnknownEnvironment` and no test would reach the path it is about.
pub(crate) fn test_effects() -> Result<EffectScope, Box<dyn std::error::Error>> {
    test_effects_with(Box::new(MemoryJournal::new()))
}

/// A memory journal with injectable append faults, counting what it saw.
///
/// One fixture for every journal fault these tests arm, wrapping
/// [`MemoryJournal`] so the real ladder and chain checks stay underneath the
/// injected fault:
///
/// - refusing an append by event kind — issue #102's acceptance: an append
///   failure *after* the action ran is not a pre-dispatch refusal, and a
///   retry of the recording does not re-enter the action;
/// - the two halves of the [`JournalError::OutcomeUnknown`] contract, each a
///   one-shot: commit-then-lost-reply (the record is in the store and the
///   caller settles by readback) and unknown-without-commit (nothing landed,
///   so the retry re-appends — safe precisely because the readback, not
///   hope, authorized it).
///
/// [`Self::durable`] grades it as surviving a process crash and gives it the
/// receipt operation a file-backed adapter has: it attests an outcome only
/// when that outcome is actually in the store. The ephemeral form keeps the
/// trait's refusal.
pub(super) struct FaultJournal {
    pub(super) inner: Rc<RefCell<MemoryJournal>>,
    pub(super) durable: bool,
    pub(super) refuse_intent: Rc<Cell<bool>>,
    pub(super) refuse_prepare: Rc<Cell<bool>>,
    pub(super) refuse_outcome: Rc<Cell<bool>>,
    pub(super) lose_reply_once: Rc<Cell<bool>>,
    pub(super) unknown_once: Rc<Cell<bool>>,
    pub(super) outcome_appends: Rc<Cell<usize>>,
}

impl FaultJournal {
    /// An ephemeral journal, graded as the memory journal it wraps.
    pub(super) fn new() -> Self {
        Self::graded(false)
    }

    /// A journal graded to survive a process crash, with a receipt operation.
    pub(super) fn durable() -> Self {
        Self::graded(true)
    }

    pub(super) fn graded(durable: bool) -> Self {
        Self {
            inner: Rc::new(RefCell::new(MemoryJournal::new())),
            durable,
            refuse_intent: Rc::new(Cell::new(false)),
            refuse_prepare: Rc::new(Cell::new(false)),
            refuse_outcome: Rc::new(Cell::new(false)),
            lose_reply_once: Rc::new(Cell::new(false)),
            unknown_once: Rc::new(Cell::new(false)),
            outcome_appends: Rc::new(Cell::new(0)),
        }
    }

    pub(super) fn refuse_outcome(&self) -> Rc<Cell<bool>> {
        Rc::clone(&self.refuse_outcome)
    }

    pub(super) fn refuse_intent(&self) -> Rc<Cell<bool>> {
        Rc::clone(&self.refuse_intent)
    }

    pub(super) fn refuse_prepare(&self) -> Rc<Cell<bool>> {
        Rc::clone(&self.refuse_prepare)
    }

    pub(super) fn outcome_appends(&self) -> Rc<Cell<usize>> {
        Rc::clone(&self.outcome_appends)
    }

    /// One-shot: commit the next outcome append and lose its reply.
    pub(super) fn lose_reply_once(&self) -> Rc<Cell<bool>> {
        Rc::clone(&self.lose_reply_once)
    }

    /// One-shot: the next outcome append reports an unknown outcome without
    /// committing anything.
    pub(super) fn unknown_once(&self) -> Rc<Cell<bool>> {
        Rc::clone(&self.unknown_once)
    }

    pub(super) fn store(&self) -> Rc<RefCell<MemoryJournal>> {
        Rc::clone(&self.inner)
    }
}

/// An injected journal failure, recorded before it is returned.
pub(super) fn injected<T>(error: JournalError) -> Result<T, JournalError> {
    let refusal = Err(error);
    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "a test journal injected a fault");
    refusal
}

impl EffectJournal for FaultJournal {
    fn durability(&self) -> DurabilityPromise {
        if self.durable {
            DurabilityPromise::ProcessCrash
        } else {
            self.inner.borrow().durability()
        }
    }

    fn tail(&self) -> JournalPosition {
        self.inner.borrow().tail()
    }

    fn committed(&self) -> Result<Vec<EffectEvent>, JournalError> {
        // Qualified: `MemoryJournal` also has an inherent `committed` that
        // returns its raw entries, and that one would win method resolution.
        EffectJournal::committed(&*self.inner.borrow())
    }

    fn committed_entries(&self) -> Result<Vec<crate::journal::JournalEntry>, JournalError> {
        Ok(self.inner.borrow().committed().to_vec())
    }

    fn compare_and_append(
        &mut self,
        expected_tail: JournalPosition,
        event: &EffectEvent,
    ) -> Result<DurableAck, JournalError> {
        let is_outcome = matches!(event, EffectEvent::OutcomeObserved { .. });
        let refuse = match *event {
            EffectEvent::IntentAdmitted { .. } => self.refuse_intent.get(),
            EffectEvent::DispatchPrepared { .. } => self.refuse_prepare.get(),
            EffectEvent::OutcomeObserved { .. } => {
                self.outcome_appends
                    .set(self.outcome_appends.get().saturating_add(1));
                self.refuse_outcome.get()
            }
            EffectEvent::Verified { .. } => false,
        };
        if refuse {
            return injected(JournalError::Storage(io::Error::other(
                "injected append refusal",
            )));
        }
        if is_outcome && self.unknown_once.take() {
            return injected(JournalError::OutcomeUnknown {
                cause: io::Error::other("injected unknown outcome, nothing written"),
            });
        }
        let ack = self
            .inner
            .borrow_mut()
            .compare_and_append(expected_tail, event)?;
        if is_outcome && self.lose_reply_once.take() {
            // Conforming ambiguity: the event is in the store and the reply
            // is lost. `Storage` would claim the journal is unchanged, which
            // the committed event contradicts.
            return injected(JournalError::OutcomeUnknown {
                cause: io::Error::other("injected post-commit lost reply"),
            });
        }
        Ok(ack)
    }

    crate::journal::confirm_outcome_by!(self, key, evidence, position, required, {
        let held = self.durable
            && EffectJournal::committed(&*self.inner.borrow())?.iter().any(
                |event| matches!(*event, EffectEvent::OutcomeObserved { key: held, evidence: held_evidence }
                    if held == key && held_evidence == evidence),
            );
        if !held {
            return injected(JournalError::ReceiptUnavailable { required });
        }
        Ok(DurableAck::new(position, self.durability()))
    });
}

/// The error a tick that has to refuse returned.
///
/// A tick that fires instead is a test failure, named by `reported` with the
/// count it fired, so every refusal test fails the same way and reads only
/// the assertions about the error it expected.
pub(super) fn refused(
    ticked: Result<usize, BotError>,
    reported: impl FnOnce(usize) -> String,
) -> Result<BotError, Box<dyn std::error::Error>> {
    match ticked {
        Ok(fired) => Err(reported(fired).into()),
        Err(error) => Ok(error),
    }
}

/// A bot whose one chain's first effect is indeterminate, logging every
/// effect that ran into `log`: the shape the evidence tests settle by hand.
pub(super) fn indeterminate_once_bot(
    name: &str,
    log: &Rc<RefCell<Vec<u16>>>,
) -> Result<EcsBot, Box<dyn std::error::Error>> {
    Ok(EcsBot::builder(name)
        .observe(Script::new(vec![200, 200, 200]))
        .on(
            |value: &u16| *value >= 200,
            Flaky {
                remaining: Rc::new(Cell::new(1)),
                log: Rc::clone(log),
                kind: FlakyKind::Indeterminate,
            },
        )
        .with_effects(test_effects()?)
        .build(&net_grants())?)
}

/// A bot over one settled source whose chain counts its runs into `runs`,
/// recording through `effects`.
///
/// The journal-fault tests differ in which fault they arm and when, not in
/// the bot they arm it against, so the bot is built here once.
pub(super) fn counting_bot(
    name: &str,
    runs: &Rc<Cell<usize>>,
    effects: EffectScope,
) -> Result<EcsBot, Box<dyn std::error::Error>> {
    Ok(EcsBot::builder(name)
        .observe(Holds::new(200))
        .on(|value: &u16| *value >= 200, Count(Rc::clone(runs)))
        .with_effects(effects)
        .build(&net_grants())?)
}

/// A journal whose positioned readback answers with a different event
/// than the one the controller acknowledged: the shape a concurrent
/// writer's tail swap has at the readback seam.
pub(super) struct SwappedEntryJournal {
    pub(super) inner: MemoryJournal,
    pub(super) swap: EffectEvent,
}

impl EffectJournal for SwappedEntryJournal {
    fn durability(&self) -> DurabilityPromise {
        self.inner.durability()
    }

    fn tail(&self) -> JournalPosition {
        self.inner.tail()
    }

    fn committed(&self) -> Result<Vec<EffectEvent>, JournalError> {
        EffectJournal::committed(&self.inner)
    }

    fn committed_entries(&self) -> Result<Vec<crate::journal::JournalEntry>, JournalError> {
        Ok(self.inner.committed().to_vec())
    }

    fn committed_entry(
        &self,
        position: JournalPosition,
    ) -> Result<Option<crate::journal::JournalEntry>, JournalError> {
        Ok(Some(crate::journal::JournalEntry::new(position, self.swap)))
    }

    fn compare_and_append(
        &mut self,
        expected_tail: JournalPosition,
        event: &EffectEvent,
    ) -> Result<DurableAck, JournalError> {
        self.inner.compare_and_append(expected_tail, event)
    }
}

/// Build an effect scope over a caller-supplied journal.
pub(super) fn test_effects_with(
    journal: Box<dyn EffectJournal>,
) -> Result<EffectScope, Box<dyn std::error::Error>> {
    let environment = EnvironmentId::from_hex(TEST_ENV)?;
    let mut broker = Broker::new();
    broker.register(environment)?;
    Ok(EffectScope::new(
        EffectIdentity::new(
            RunId::from_hex(TEST_RUN)?,
            environment,
            FlowRevision::from_tagged("blake3_256", TEST_FLOW)?,
        ),
        broker,
        journal,
    ))
}

/// The sequence every test uses: two ticks of 200, two of 503, one of 200.
/// It holds still for a tick at a time, which is what makes a change filter
/// falsifiable: a sequence that moved on every tick would make "changed"
/// and "ran" indistinguishable.
pub(super) const SCRIPT: [u16; 5] = [200, 200, 503, 503, 200];

/// An action that declares `Input = u32` while counting its attempts.
///
/// Paired below with a `u16` source: the pairing is the fault, and the
/// count is how the test tells "one attempt" from "the whole budget".
pub(super) struct CountsU32(pub(super) Rc<Cell<usize>>);

impl Execute for CountsU32 {
    type Input = u32;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
        call.0.check(&[])?;
        self.0.set(self.0.get().saturating_add(1));
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::counts_u32"
    }
}

// ── admitted-input identity framing ─────────────────────────────────────

/// Two output types whose `type_name`s share a prefix, the pair the
/// identity scheme must keep apart (issue #118).
///
/// `PrefixProbeTail`'s type name is `PrefixProbe`'s name plus `b"Tail"`,
/// and `PrefixProbe` writes that same suffix at the head of its identity
/// bytes, so an unframed `name || bytes` stream would collide byte for
/// byte. Under the declared-schema scheme they are distinct because their
/// authors declared distinct schema ids — the name games cannot reach the
/// digest any more.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PrefixProbe;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PrefixProbeTail;

/// The byte difference between the two type names above.
pub(super) const PREFIX_PROBE_SUFFIX: &[u8] = b"Tail";

impl InputIdentity for PrefixProbe {
    const SCHEMA_ID: &'static [u8] = b"lgwks.bot.schema.v1.test-prefix-probe";

    fn write_identity(&self, hasher: &mut Hasher) {
        hasher.update(PREFIX_PROBE_SUFFIX);
        hasher.update(b"payload");
    }
}

impl InputIdentity for PrefixProbeTail {
    const SCHEMA_ID: &'static [u8] = b"lgwks.bot.schema.v1.test-prefix-probe-tail";

    fn write_identity(&self, hasher: &mut Hasher) {
        hasher.update(b"payload");
    }
}

/// A second schema of a type whose identity bytes are byte-for-byte
/// identical to [`PrefixProbe`]'s — the control that proves the schema
/// id, not the bytes, separates the two.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PrefixProbeNextSchema;

impl InputIdentity for PrefixProbeNextSchema {
    const SCHEMA_ID: &'static [u8] = b"lgwks.bot.schema.v2.test-prefix-probe";

    fn write_identity(&self, hasher: &mut Hasher) {
        hasher.update(PREFIX_PROBE_SUFFIX);
        hasher.update(b"payload");
    }
}

/// The source type for the identity known-answer vectors.
///
/// [`identify_output`] reads only a source's output type, so these tests name
/// a source without building or ticking a bot over it. One generic type
/// stands for every output they derive an identity for, and a poll — which
/// none of them makes — is refused rather than answered.
pub(super) struct Unpolled<T>(pub(super) PhantomData<fn() -> T>);

impl<T> Observe for Unpolled<T> {
    type Output = T;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, _call: (Auth, ())) -> Result<T, BotError> {
        domain_refusal("test::unpolled", "identity fixtures are never polled")
    }

    fn domain_id(&self) -> &str {
        "test::unpolled"
    }
}

/// A payload whose identity bytes are identical to `u64`'s — the control
/// that proves the inner schema id, not the bytes, keeps two wrapper
/// types apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct AltTick(pub(super) u64);

impl InputIdentity for AltTick {
    const SCHEMA_ID: &'static [u8] = b"lgwks.bot.schema.v1.test-alt-tick";

    fn write_identity(&self, hasher: &mut Hasher) {
        hasher.update(&self.0.to_le_bytes());
    }
}

/// A durable journal that grades itself `ProcessCrash` but refuses to
/// reserve settlement capacity, counting the appends it was asked for.
///
/// The shipped [`FileJournal`] reserves room
/// against its own event ceiling; this stands in for the moment that
/// ceiling is reached, which a test cannot reach without a hundred thousand
/// real flushes.
pub(super) struct NoSettlementRoom {
    /// The record the appends would have gone to.
    pub(super) inner: MemoryJournal,
    /// How many appends were asked for, whether or not one happened.
    pub(super) appends: Rc<Cell<usize>>,
}

impl EffectJournal for NoSettlementRoom {
    fn durability(&self) -> DurabilityPromise {
        DurabilityPromise::ProcessCrash
    }

    fn tail(&self) -> JournalPosition {
        self.inner.tail()
    }

    fn committed(&self) -> Result<Vec<EffectEvent>, JournalError> {
        EffectJournal::committed(&self.inner)
    }

    fn compare_and_append(
        &mut self,
        expected_tail: JournalPosition,
        event: &EffectEvent,
    ) -> Result<DurableAck, JournalError> {
        self.appends.set(self.appends.get().saturating_add(1));
        self.inner.compare_and_append(expected_tail, event)
    }

    fn reserve_handoff_capacity(&self, rungs: u64) -> Result<(), JournalError> {
        Err(JournalError::CapacityExceeded {
            resource: crate::journal::JournalLimitKind::Events,
            limit: 0,
            requested: rungs,
        })
    }
}
