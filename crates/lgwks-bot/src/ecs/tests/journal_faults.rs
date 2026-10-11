//! `ecs` tests: the journal fault fixtures and the recording-failure tests.

use super::*;

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
    inner: Rc<RefCell<MemoryJournal>>,
    durable: bool,
    refuse_intent: Rc<Cell<bool>>,
    refuse_prepare: Rc<Cell<bool>>,
    refuse_outcome: Rc<Cell<bool>>,
    lose_reply_once: Rc<Cell<bool>>,
    unknown_once: Rc<Cell<bool>>,
    outcome_appends: Rc<Cell<usize>>,
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
fn injected<T>(error: JournalError) -> Result<T, JournalError> {
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

    fn confirm_outcome(
        &mut self,
        key: crate::effect::EffectKey,
        evidence: EffectEvidence,
        position: JournalPosition,
        required: DurabilityPromise,
    ) -> Result<DurableAck, JournalError> {
        let held = self.durable
            && EffectJournal::committed(&*self.inner.borrow())?.iter().any(
                |event| matches!(*event, EffectEvent::OutcomeObserved { key: held, evidence: held_evidence }
                    if held == key && held_evidence == evidence),
            );
        if !held {
            return injected(JournalError::ReceiptUnavailable { required });
        }
        Ok(DurableAck::new(position, self.durability()))
    }
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

#[test]
fn hash_parts_frames_distinct_variable_length_splits() -> TestResult {
    let left = hash_parts(&[b"ab", b"c"]);
    let right = hash_parts(&[b"a", b"bc"]);
    assert_ne!(left, right, "distinct field splits must not collide");
    assert_eq!(left, hash_parts(&[b"ab", b"c"]));
    Ok(())
}

#[test]
fn an_ambiguous_commit_then_error_is_idempotent() -> TestResult {
    let runs = Rc::new(Cell::new(0));
    let journal = FaultJournal::new();
    let lose_reply = journal.lose_reply_once();
    let store = journal.store();
    let mut bot = counting_bot(
        "ambiguous-commit",
        &runs,
        test_effects_with(Box::new(journal))?,
    )?;

    lose_reply.set(true);
    // The reply is lost, but the readback finds the record and the tick
    // settles in the same breath: an effect whose record is provably in
    // the journal is settled work, not an ambiguity to stall on.
    assert_eq!(bot.tick()?, 1);
    assert_eq!(runs.get(), 1);
    let committed = EffectJournal::committed(&*store.borrow())?;
    assert!(matches!(
        committed.last(),
        Some(EffectEvent::OutcomeObserved {
            evidence: EffectEvidence::Applied,
            ..
        })
    ));
    // A public repeat of the same evidence is the same fact, not a
    // refusal: the journal answers it from the committed record.
    let key = committed[0].key();
    bot.resolve_effect(&key, EffectEvidence::Applied)?;

    assert_eq!(bot.tick()?, 0);
    assert_eq!(runs.get(), 1, "retry must not dispatch the action again");
    assert!(bot.pending().is_empty());
    Ok(())
}

/// A journal whose positioned readback answers with a different event
/// than the one the controller acknowledged: the shape a concurrent
/// writer's tail swap has at the readback seam.
struct SwappedEntryJournal {
    inner: MemoryJournal,
    swap: EffectEvent,
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

/// A positioned readback that holds a different event is a mismatch,
/// never an acknowledgement: binding the event to the position stops an
/// adapter from laundering a concurrent writer's tail into this
/// controller's fold (R05 wrong-event control; #122 item 2).
#[test]
fn a_position_holding_a_different_event_is_a_mismatch_not_an_acknowledgement() -> TestResult {
    let key = crate::effect::EffectIdentity::new(
        RunId::from_hex(TEST_RUN)?,
        EnvironmentId::from_hex(TEST_ENV)?,
        crate::effect::FlowRevision::from_tagged("blake3_256", TEST_FLOW)?,
    )
    .key(
        crate::effect::ActionId::from_hex("1112131415161718191a1b1c1d1e1f20")?,
        crate::effect::AttemptId::from_decimal("1")?,
        crate::effect::ActionDigest::from_tagged(
            "blake3_256",
            "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeffe0e1e2e3e4e5e6e7e8e9eaebecedeeef",
        )?,
        crate::effect::EnvironmentEpoch::from_decimal("1")?,
    );
    let admitted = EffectEvent::IntentAdmitted { key };
    let mut backing = MemoryJournal::new();
    let ack = backing.compare_and_append(backing.tail(), &admitted)?;
    let swapped = EffectEvent::DispatchPrepared { key };
    let scope = test_effects_with(Box::new(SwappedEntryJournal {
        inner: backing,
        swap: swapped,
    }))?;
    let mut effects = Effects::new(scope, ack.position());
    match effects.accept_position(&admitted, ack.position()) {
        Err(JournalError::EntryMismatch {
            position,
            expected,
            actual,
        }) => {
            assert_eq!(position, ack.position());
            assert_eq!(*expected, admitted);
            assert_eq!(*actual, swapped);
        }
        other => {
            return Err(
                format!("a swapped positioned entry must be EntryMismatch, got {other:?}").into(),
            );
        }
    }
    Ok(())
}

#[test]
fn a_durable_retry_after_a_lost_reply_settles_instead_of_stalling() -> TestResult {
    let runs = Rc::new(Cell::new(0));
    let journal = FaultJournal::durable();
    let ambiguous_once = journal.lose_reply_once();
    let store = journal.store();
    let mut bot = counting_bot("lost-reply", &runs, test_effects_with(Box::new(journal))?)?;

    // Half one: the record landed, the reply did not. The readback finds
    // it, the journal attests its grade, and the tick settles in the same
    // breath — a receipt the journal can produce is not a reason to
    // stall, and never a reason to re-enter the action.
    ambiguous_once.set(true);
    assert_eq!(bot.tick()?, 1);
    assert_eq!(runs.get(), 1);
    let committed = EffectJournal::committed(&*store.borrow())?;
    assert!(
        matches!(
            committed.last(),
            Some(EffectEvent::OutcomeObserved {
                evidence: EffectEvidence::Applied,
                ..
            })
        ),
        "the store must hold the outcome whose reply was lost"
    );
    assert!(bot.pending().is_empty());

    // Half two: an unknown outcome where nothing landed. The readback
    // proves absence, the caller reports the occurrence, and the retry
    // re-appends — readback authorized it, the ladder stands behind it.
    // A separate bot: half one's `Applied` settlement retired that
    // generation, and a retired generation is owed no second append.
    let runs = Rc::new(Cell::new(0));
    let journal = FaultJournal::durable();
    let unknown_once = journal.unknown_once();
    let store = journal.store();
    let mut bot = counting_bot(
        "unknown-refused",
        &runs,
        test_effects_with(Box::new(journal))?,
    )?;

    unknown_once.set(true);
    match bot.tick() {
        Err(error @ BotError::EffectUnrecorded { .. }) => {
            assert_eq!(
                error.dispatch_certainty(),
                DispatchCertainty::Occurred,
                "the effect happened; only its record's reply was lost: {error:?}"
            );
        }
        other => return Err(format!("expected a lost reply, got {other:?}").into()),
    }
    assert_eq!(runs.get(), 1);
    let committed = EffectJournal::committed(&*store.borrow())?;
    assert!(
        !committed
            .iter()
            .any(|event| matches!(event, EffectEvent::OutcomeObserved { .. })),
        "an unknown outcome without a commit must leave the ladder free"
    );
    assert_eq!(
        bot.tick()?,
        1,
        "the record lands on the recovery tick and counts as fired"
    );
    assert_eq!(runs.get(), 1, "a lost reply must never re-run the action");
    assert!(bot.pending().is_empty());
    Ok(())
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

/// A post-applied append failure is a recording failure with a known
/// occurrence — never a pre-dispatch refusal, and never a re-dispatch.
///
/// Acceptance: the action's marker exists exactly once after recovery, the
/// returned certainty is `Occurred` (not `Refused`, not `NotDelivered`),
/// the entry is held as `RecordingFailed`, and the append retry does not
/// call the action again.
#[test]
fn a_post_applied_append_failure_is_a_recording_failure_not_a_refusal() -> TestResult {
    let runs = Rc::new(Cell::new(0));
    let journal = FaultJournal::new();
    let refuse_outcome = journal.refuse_outcome();
    let outcome_appends = journal.outcome_appends();
    let mut bot = counting_bot(
        "unrecorded-applied",
        &runs,
        test_effects_with(Box::new(journal))?,
    )?;

    refuse_outcome.set(true);
    let error = refused(bot.tick(), |fired| {
        format!("a refused outcome append was reported as {fired} fired")
    })?;
    assert!(
        matches!(error, BotError::EffectUnrecorded { .. }),
        "expected EffectUnrecorded, got {error:?}"
    );
    assert_eq!(
        error.dispatch_certainty(),
        DispatchCertainty::Occurred,
        "the effect happened: the certainty must not be Refused or NotDelivered"
    );
    assert_eq!(
        error.retry_class(),
        RetryClass::Never,
        "the action is never re-entered for a recording failure"
    );
    assert_eq!(runs.get(), 1, "the action ran exactly once");
    assert_eq!(
        outcome_appends.get(),
        1,
        "the outcome append was attempted once and refused"
    );
    assert!(
        matches!(
            first_hold(&bot),
            Some(TransitionHold::RecordingFailed { .. })
        ),
        "the entry is held for an append-only retry: {:?}",
        first_hold(&bot)
    );

    // Clear the fault. The recovery tick retries the *append* and must not
    // call the action again.
    refuse_outcome.set(false);
    assert_eq!(
        bot.tick()?,
        1,
        "the append lands and the recovered effect is counted as fired"
    );
    assert_eq!(
        runs.get(),
        1,
        "a retry of recording must not re-enter the action"
    );
    assert_eq!(
        outcome_appends.get(),
        2,
        "the outcome append was retried exactly once"
    );
    assert!(
        bot.pending().is_empty(),
        "the entry is resolved once its record lands: {:?}",
        bot.pending()
    );
    Ok(())
}

/// The same split after a definitely-not-applied effect: the occurrence is
/// `NotApplied`, the error is still `EffectUnrecorded`, and the action is
/// still not re-entered while the record is missing.
#[test]
fn a_post_failure_append_failure_keeps_the_not_applied_fact() -> TestResult {
    let runs = Rc::new(Cell::new(0));
    let journal = FaultJournal::new();
    let refuse_outcome = journal.refuse_outcome();
    let mut bot = EcsBot::builder("unrecorded-not-applied")
        .observe(Holds::new(200))
        .on(
            |value: &u16| *value >= 200,
            Refuses::undelivered(Rc::clone(&runs)),
        )
        .with_effects(test_effects_with(Box::new(journal))?)
        .build(&net_grants())?;

    refuse_outcome.set(true);
    let error = refused(bot.tick(), |fired| {
        format!("a refused outcome append was reported as {fired} fired")
    })?;
    assert!(
        matches!(
            error,
            BotError::EffectUnrecorded {
                evidence: EffectEvidence::NotApplied,
                ..
            }
        ),
        "expected EffectUnrecorded carrying NotApplied, got {error:?}"
    );
    assert_eq!(
        error.dispatch_certainty(),
        DispatchCertainty::NotDelivered,
        "the effect definitely did not happen"
    );
    assert_eq!(
        error.retry_class(),
        RetryClass::Never,
        "a recording failure never re-enters the action"
    );
    assert_eq!(runs.get(), 1, "the action ran exactly once");
    assert!(
        matches!(
            first_hold(&bot),
            Some(TransitionHold::RecordingFailed { .. })
        ),
        "the entry is held for an append-only retry: {:?}",
        first_hold(&bot)
    );

    refuse_outcome.set(false);
    // The append lands. The effect is known not-applied, so the entry
    // becomes eligible for a *fresh* attempt under the budget — that is a
    // response to "it did not happen", not a retry of the recording
    // failure. It is still open work, so the tick reports it rather than
    // claiming a clean finish.
    let error = refused(bot.tick(), |fired| {
        format!("an open entry was reported as {fired} fired")
    })?;
    assert!(
        matches!(error, BotError::PendingTransition { .. }),
        "expected the still-open entry, got {error:?}"
    );
    assert_eq!(
        runs.get(),
        1,
        "the append retry must not re-enter the action"
    );
    assert!(
        matches!(
            first_hold(&bot),
            Some(TransitionHold::Failed { attempts: 1, .. })
        ),
        "the entry is eligible again under the budget: {:?}",
        first_hold(&bot)
    );
    Ok(())
}

/// Append failures *before* the action are still pre-dispatch refusals.
///
/// The negative control for the two tests above: `Refused` is the right
/// answer when nothing left the process, and this is what keeps the new
/// variant from swallowing that case.
#[test]
fn an_append_failure_before_the_action_is_still_a_pre_dispatch_refusal() -> TestResult {
    let runs = Rc::new(Cell::new(0));

    // Before intent.
    let journal = FaultJournal::new();
    let refuse_intent = journal.refuse_intent();
    let mut bot = counting_bot(
        "refuse-intent",
        &runs,
        test_effects_with(Box::new(journal))?,
    )?;
    refuse_intent.set(true);
    let error = refused(bot.tick(), |fired| {
        format!("a refused intent append was reported as {fired} fired")
    })?;
    assert!(
        matches!(error, BotError::EffectRefused { .. }),
        "expected EffectRefused before intent, got {error:?}"
    );
    assert_eq!(error.dispatch_certainty(), DispatchCertainty::Refused);
    assert_eq!(runs.get(), 0, "the action never ran");

    // Before preparation.
    let journal = FaultJournal::new();
    let refuse_prepare = journal.refuse_prepare();
    let mut bot = counting_bot(
        "refuse-prepare",
        &runs,
        test_effects_with(Box::new(journal))?,
    )?;
    refuse_prepare.set(true);
    let error = refused(bot.tick(), |fired| {
        format!("a refused prepare append was reported as {fired} fired")
    })?;
    assert!(
        matches!(error, BotError::EffectRefused { .. }),
        "expected EffectRefused before preparation, got {error:?}"
    );
    assert_eq!(error.dispatch_certainty(), DispatchCertainty::Refused);
    assert_eq!(runs.get(), 0, "the action never ran");
    Ok(())
}
