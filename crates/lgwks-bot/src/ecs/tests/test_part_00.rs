//! ECS bot unit tests, part 00.

use super::*;

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

#[test]
fn a_change_tick_source_fires_once_per_movement_and_is_not_polled_otherwise() -> TestResult {
    let value = Rc::new(Cell::new(0u64));
    let rev = Rc::new(Cell::new(0u64));
    let polls = Rc::new(Cell::new(0usize));
    let counter = Rc::new(Cell::new(0usize));
    let mut bot = EcsBot::builder("revs")
        .observe(Rev {
            value: Rc::clone(&value),
            rev: Rc::clone(&rev),
            polls: Rc::clone(&polls),
            caps: Vec::new(),
        })
        .on(
            |value: &u64| value.is_multiple_of(2),
            CountU64(Rc::clone(&counter)),
        )
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())?;
    let mut fired = Vec::new();
    for step in 0..6u64 {
        // The value and the revision move together, every other tick: the
        // promise `Observe::revision` asks for, kept in both directions.
        // Halving a `u64` by a literal cannot overflow or divide by zero,
        // so the refusal is a statement about that rather than a path a
        // reader has to imagine.
        let held = step
            .checked_div(2)
            .ok_or("halving a u64 by two cannot fail")?;
        value.set(held);
        rev.set(held);
        fired.push(bot.tick()?);
    }
    // The value steps 0, 0, 1, 1, 2, 2 and the revision steps with it. Tick 2
    // moves to an **odd** value, so the condition runs and answers false —
    // which is the half the skip must not lose: a movement is evaluated, and
    // only a tick that did not move is skipped.
    assert_eq!(
        fired,
        vec![1, 0, 0, 0, 1, 0],
        "one fire per even movement and nothing while the revision holds: {fired:?}"
    );
    assert_eq!(
        polls.get(),
        3,
        "a skipped chain is not polled at all, so three movements are three polls"
    );
    assert_eq!(
        bot.pending(),
        Vec::new(),
        "a value that moves must be acted on, and a chain with nothing outstanding \
         must not be reported"
    );
    Ok(())
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

#[test]
fn a_condition_fires_only_on_the_tick_its_source_moves() -> TestResult {
    let counter = Rc::new(Cell::new(0));
    let mut bot = EcsBot::builder("scripted")
        .observe(Script::new(SCRIPT.to_vec()))
        .on(|value: &u16| *value >= 500, Count(Rc::clone(&counter)))
        .with_effects(test_effects()?)
        .build(&net_grants())?;

    let mut fired = Vec::new();
    let mut revisions = Vec::new();
    for _ in 0..5 {
        fired.push(bot.tick()?);
        revisions.push(
            bot.revisions()
                .first()
                .copied()
                .ok_or("a bot with one chain reports one revision")?,
        );
    }

    // `Revision` moves on the ticks the value actually moved: the first
    // poll, the step to 503, and the step back to 200.
    assert_eq!(
        revisions,
        vec![1, 1, 2, 2, 3],
        "Revision must track value movement, not system execution"
    );
    // 503 is held for two ticks; the condition is true on both, and the
    // effect runs once, on the transition.
    assert_eq!(fired, vec![0, 0, 1, 0, 0]);
    assert_eq!(counter.get(), 1);
    Ok(())
}

/// A chain whose source settles must settle.
///
/// This is the regression for a defect found by `bench/`'s fairness gate and
/// by nothing else: the bot fired 896,000 effects where the hand-rolled
/// baseline fired 17,920 for identical input, a 50x over-run that every
/// test in this crate passed through.
///
/// The cause was a lost baseline. A transition *takes* the observed value out
/// of its slot, and while the transition is retained that binding is where
/// the chain's newest value lives. When the transition finished and was
/// dropped instead, the value went with it, leaving the chain with nothing to
/// compare against. The next tick read "no baseline" as "the source moved",
/// re-opened the chain and fired the entry again — and again, every tick
/// after, forever.
///
/// So the assertion is not "it fired" but "it fired **once**", over a run
/// long enough that a per-tick re-fire is unmistakable. `Holds` is what makes
/// it decidable: a source that advances every poll cannot tell a chain that
/// settled from a chain that is still working.
#[test]
fn a_settled_chain_does_not_fire_again_on_every_later_tick() -> TestResult {
    let counter = Rc::new(Cell::new(0));
    let mut bot = counting_bot("held", &counter, test_effects()?)?;

    // The first tick is the movement: the chain opens and the effect runs.
    assert_eq!(bot.tick()?, 1, "the movement opens the chain");

    // Twenty more ticks in which nothing whatever happens. A lost baseline
    // shows up here as `1` rather than `0` on the second tick and every one
    // after it.
    for tick in 0_u32..20 {
        assert_eq!(
            bot.tick()?,
            0,
            "tick {}: the source held still, so the chain had nothing to run",
            tick.saturating_add(2)
        );
    }

    assert_eq!(
        counter.get(),
        1,
        "the effect is owed once per movement, not once per tick"
    );
    // The revision is the substrate's own record of movement, and it is the
    // other half of the same claim: one movement, one bump.
    assert_eq!(
        bot.revisions().first().copied(),
        Some(1),
        "Revision counts movements, so a settled chain's is still 1"
    );
    Ok(())
}

/// A detached digest cannot prove that an async poll returned the same
/// snapshot. The bot must continue to poll until an observer can bind its
/// revision to the returned value in one operation.
#[test]
fn a_source_with_a_detached_digest_is_polled_while_it_holds_still() -> TestResult {
    let polls = Rc::new(Cell::new(0));
    let counter = Rc::new(Cell::new(0));
    let mut bot = EcsBot::builder("lazy")
        .observe(Counted::new(200, Rc::clone(&polls)))
        .on(|value: &u16| *value >= 200, Count(Rc::clone(&counter)))
        .with_effects(test_effects()?)
        .build(&net_grants())?;

    assert_eq!(bot.tick()?, 1, "the first tick polls");
    assert_eq!(polls.get(), 1, "and polls exactly once");

    for tick in 0_u32..20 {
        assert_eq!(
            bot.tick()?,
            0,
            "tick {}: nothing moved",
            tick.saturating_add(2)
        );
    }
    assert_eq!(
        polls.get(),
        21,
        "a detached digest cannot suppress a later async poll"
    );
    assert_eq!(counter.get(), 1, "and must not fire the effect again");
    Ok(())
}

/// An A → B → A source race must not leave B held while stable A is skipped.
///
/// The first tick reads the legacy fingerprint at A, lets the observer
/// sample B, returns the live source to A, and only then admits B. The next
/// tick has to poll and deliver A. The retired cache used the earlier A
/// fingerprint for B, saw A as quiet on the next tick, and left B held.
#[test]
fn a_detached_digest_cannot_suppress_the_final_stable_aba_state() -> TestResult {
    let value = Rc::new(Cell::new(0));
    let phase = Rc::new(Cell::new(0));
    let polls = Rc::new(Cell::new(0));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut bot = EcsBot::builder("aba")
        .observe(AbaSource::new(
            Rc::clone(&value),
            Rc::clone(&phase),
            Rc::clone(&polls),
        ))
        .on(|_: &u16| true, Record(Rc::clone(&seen)))
        .with_effects(test_effects()?)
        .build(&net_grants())?;

    let mut tick = Box::pin(bot.tick_async());
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    if !matches!(tick.as_mut().poll(&mut cx), Poll::Pending) {
        return Err("the first poll must pause before sampling B".into());
    }
    value.set(1);
    if !matches!(tick.as_mut().poll(&mut cx), Poll::Pending) {
        return Err("the source must pause after sampling B".into());
    }
    value.set(0);
    match tick.as_mut().poll(&mut cx) {
        Poll::Ready(Ok(fired)) => assert_eq!(fired, 1, "the sampled B action fires"),
        Poll::Ready(Err(error)) => return Err(format!("the B tick failed: {error}").into()),
        Poll::Pending => return Err("the third poll must admit the sampled B value".into()),
    }
    drop(tick);
    assert_eq!(*seen.borrow(), vec![1], "the first tick delivers sampled B");

    assert_eq!(bot.tick()?, 1, "the next tick must observe stable A");
    assert_eq!(
        *seen.borrow(),
        vec![1, 0],
        "stable A is delivered instead of being suppressed by B's stale digest"
    );
    assert_eq!(polls.get(), 2, "both B and the final stable A were polled");
    Ok(())
}

/// Value comparison still avoids another effect when a source holds still.
#[test]
fn a_source_value_that_moves_is_polled_again_and_fires() -> TestResult {
    let polls = Rc::new(Cell::new(0));
    let counter = Rc::new(Cell::new(0));
    let source = Counted::new(200, Rc::clone(&polls));
    let value = Rc::clone(&source.value);
    let mut bot = EcsBot::builder("lazy-moving")
        .observe(source)
        .on(|value: &u16| *value >= 200, Count(Rc::clone(&counter)))
        .with_effects(test_effects()?)
        .build(&net_grants())?;

    assert_eq!(bot.tick()?, 1, "the first value fires");
    assert_eq!(bot.tick()?, 0, "and holds");
    assert_eq!(
        polls.get(),
        2,
        "the second tick polls and compares its value"
    );

    value.set(503);
    assert_eq!(bot.tick()?, 1, "the movement is seen, not skipped");
    assert_eq!(
        polls.get(),
        3,
        "the tick paid for a value to see the movement"
    );

    assert_eq!(bot.tick()?, 0, "and settles again");
    assert_eq!(polls.get(), 4, "the stable value is polled and compared");
    assert_eq!(counter.get(), 2, "two movements, two effects");
    Ok(())
}

/// A source without a digest keeps the same value-comparison behavior.
#[test]
fn a_source_without_a_digest_is_polled_every_tick() -> TestResult {
    let counter = Rc::new(Cell::new(0));
    let mut bot = counting_bot("unguarded", &counter, test_effects()?)?;

    assert_eq!(bot.tick()?, 1);
    for _ in 0..5 {
        assert_eq!(bot.tick()?, 0);
    }
    // Every tick polls; the effect still fires once because the value did
    // not move.
    assert_eq!(
        counter.get(),
        1,
        "the value decides the chain, not the digest"
    );
    Ok(())
}

#[test]
fn building_without_the_grant_is_refused() -> TestResult {
    let counter = Rc::new(Cell::new(0));
    match EcsBot::builder("ungranted")
        .observe(Script::new(SCRIPT.to_vec()))
        .on(|value: &u16| *value >= 500, Count(counter))
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())
    {
        Ok(_) => Err("a source requiring `bot.net` built without it".into()),
        Err(error) => {
            assert!(
                matches!(error, BotError::CapabilityDenied { .. }),
                "expected CapabilityDenied, got {error:?}"
            );
            Ok(())
        }
    }
}

/// Admission as one answer rather than a loop.
///
/// This bot is short of `bot.net` on its source and `bot.fs` on its action.
/// Admission that returned at the first unmet requirement told the caller
/// `bot.net`, and `bot.fs` only appeared on the next build — so a person
/// repairing a bot discovered its requirements one refusal at a time. The
/// gate already knows both; one refusal now carries both, and each says
/// which domain declared it.
#[test]
fn admission_names_every_unmet_requirement_and_the_domain_that_declared_it() -> TestResult {
    match EcsBot::builder("short")
        .observe(Holds::new(1))
        .on(
            |value: &u16| *value > 0,
            NeedsCaps {
                caps: vec![Cap::fs()],
                domain: "test::writes",
            },
        )
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())
    {
        Ok(_) => Err("a bot requiring two ungranted capabilities built".into()),
        Err(BotError::CapabilityDenied { deficit }) => {
            let named: Vec<(&str, Option<&str>)> = deficit
                .shortages()
                .map(|shortage| {
                    (
                        shortage.required().as_str(),
                        shortage.demand().map(Demand::domain),
                    )
                })
                .collect();
            assert_eq!(
                named,
                vec![
                    (Cap::NET, Some("test::holds")),
                    (Cap::FS, Some("test::writes")),
                ],
                "one refusal must name both requirements and both domains: {deficit}"
            );
            // And the repair is one call, not one per refusal.
            assert!(
                deficit
                    .to_grant_set()
                    .admit(&[Cap::net(), Cap::fs()])
                    .is_ok(),
                "the deficit must derive the whole repair: {deficit}"
            );
            Ok(())
        }
        Err(other) => Err(format!("expected a capability denial, got {other:?}").into()),
    }
}

#[test]
fn a_poll_error_is_returned_and_commits_nothing() -> TestResult {
    let counter = Rc::new(Cell::new(0));
    let mut bot = EcsBot::builder("exhausting")
        .observe(Exhausting {
            remaining: Rc::new(Cell::new(1)),
            caps: vec![Cap::net()],
        })
        .on(|value: &u16| *value >= 500, Count(Rc::clone(&counter)))
        .with_effects(test_effects()?)
        .build(&net_grants())?;

    assert_eq!(
        bot.tick()?,
        0,
        "the first poll answers 200 and fires nothing"
    );
    let after_success = bot.revisions();

    // The second poll fails. The tick reports it, and the observed value
    // from the first tick must survive: a failed tick leaves the world as
    // it was rather than half-updated.
    let error = refused(bot.tick(), |_| {
        String::from("a failing poll was reported as a successful tick")
    })?;
    assert!(
        matches!(error, BotError::DomainError { .. }),
        "expected the observer's own error, got {error:?}"
    );
    assert_eq!(
        bot.revisions(),
        after_success,
        "a failed tick must not commit a new Revision"
    );
    assert_eq!(
        counter.get(),
        0,
        "a failed poll fires nothing: the action must not have run"
    );
    Ok(())
}
