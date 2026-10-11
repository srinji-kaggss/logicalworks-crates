//! `ecs` tests: unrecorded attempts, condition holds, schedule refusal and the retry budget.

use super::*;

#[test]
fn an_attempt_whose_outcome_was_never_recorded_is_held_not_replayed() -> TestResult {
    // The state a tick leaves behind when an attempt was begun and no
    // outcome was recorded. The effect may be live, so the next tick holds
    // it — replaying it is the duplicate this state exists to prevent.
    //
    // The journal is kept honest while the ledger is put into the state:
    // the write-ahead (`IntentAdmitted`, `DispatchPrepared`) is committed
    // by a real dispatch and the outcome append is refused underneath it,
    // which is exactly the crash window this hold names. Fabricating
    // `Unrecorded` over a key whose outcome the journal already holds
    // would make the later `NotApplied` a contradiction rather than a
    // settlement, and the journal is the fact that wins (issue #106).
    let log = Rc::new(RefCell::new(Vec::new()));
    let journal = FaultJournal::new();
    let refuse_outcome = journal.refuse_outcome();
    refuse_outcome.set(true);
    let mut bot = EcsBot::builder("interrupted")
        .observe(Script::new(vec![200, 200, 200]))
        .on(|value: &u16| *value >= 200, Record(Rc::clone(&log)))
        .with_effects(test_effects_with(Box::new(journal))?)
        .build(&net_grants())?;

    let error = refused(bot.tick(), |fired| {
        format!("a refused outcome append reported {fired} fired")
    })?;
    assert!(
        matches!(error, BotError::EffectUnrecorded { .. }),
        "expected the outcome append's own failure, got {error:?}"
    );
    assert_eq!(*log.borrow(), vec![200], "the action ran once");
    refuse_outcome.set(false);
    {
        let revision = *bot.revisions().first().ok_or("the source has a revision")?;
        // The payload is part of the record: a transition is bound to the
        // observation it was opened under, and the value here is the one the
        // script has been answering with. `None` would be a transition
        // opened with nothing observed, which holds its entries rather than
        // evaluating them against a value nobody read — a real state, but
        // not this one.
        // The same admitted input the first tick bound: a fabricated
        // stamp would mint a key the journal never recorded, and the
        // later settlement would be about a different attempt entirely.
        let input = bot
            .world
            .non_send::<Ledger>()
            .transitions
            .first()
            .and_then(Option::as_ref)
            .map(|held| AdmittedInput {
                identity: held.input,
                event: held.event,
            })
            .ok_or("the first tick opened no transition to rebind")?;
        let mut transition = Transition::opened(input, revision, 1, Some(Erased::new(200_u16)));
        *transition
            .entries
            .first_mut()
            .ok_or("the chain declares no entries")? = EntryState::Unrecorded;
        // The attempt the unrecorded entry is on. `Unrecorded` is the state
        // a prepared dispatch leaves, so there is always an attempt to name
        // it by — and the record has to carry one, because the key a caller
        // settles against is derived from the attempt and an entry with no
        // attempt has no key for evidence to be about.
        *transition
            .attempts
            .first_mut()
            .ok_or("the chain declares no entries")? = AttemptRecord {
            begun: Some(AttemptId::FIRST),
            settled: None,
        };
        bot.world.non_send_mut::<Ledger>().put(0, Some(transition));
    }

    assert!(
        matches!(first_hold(&bot), Some(TransitionHold::Unrecorded)),
        "the interrupted attempt is reported as unrecorded: {:?}",
        first_hold(&bot)
    );
    let error = refused(bot.tick(), |fired| {
        format!("an unrecorded attempt fired {fired}")
    })?;
    let BotError::PendingTransition { work, .. } = error else {
        return Err(format!("expected PendingTransition, got {error:?}").into());
    };
    assert!(
        matches!(work.hold(), TransitionHold::Unrecorded),
        "the tick reports it as held, not as handled: {:?}",
        work.hold()
    );
    assert_eq!(
        *log.borrow(),
        vec![200],
        "the action was not run a second time: the first attempt may have taken effect"
    );

    // Evidence that it did not take effect is what unlocks the retry, and
    // it is evidence about the generation the caller was shown.
    let held = bot
        .pending()
        .into_iter()
        .next()
        .ok_or("the held entry is not reported")?;
    bot.resolve_effect(&held_key(&held)?, EffectEvidence::NotApplied)?;
    assert_eq!(bot.tick()?, 1, "and then it runs");
    assert_eq!(*log.borrow(), vec![200, 200], "exactly once more");
    Ok(())
}

#[test]
fn an_action_identity_is_framed_and_portable() -> TestResult {
    // Two different parses that an unframed concatenation collapses:
    // "ab"+"c" and "a"+"bc" are the same byte string with no lengths, and
    // the same ActionId for two different declarations (issue #101).
    let ab_c = derive_action_id("ab", 0, 0, "c");
    let a_bc = derive_action_id("a", 0, 0, "bc");
    assert_ne!(
        ab_c, a_bc,
        "length framing keeps \"ab\"+\"c\" apart from \"a\"+\"bc\""
    );
    // The index is u64 either way, so this is the 32-/64-bit vector in
    // one target's clothes: the same declaration always derives the same
    // identity, and a different position never does.
    assert_eq!(
        derive_action_id("bot", 2, 3, "dom"),
        derive_action_id("bot", 2, 3, "dom")
    );
    assert_ne!(
        derive_action_id("bot", 2, 3, "dom"),
        derive_action_id("bot", 2, 3, "dom2"),
        "a different domain is a different action"
    );
    assert_ne!(
        derive_action_id("bot", 2, 3, "dom"),
        derive_action_id("bot", 3, 2, "dom"),
        "chain and entry do not commute"
    );
    // A variable-length name cannot slide into the next field: the old
    // unframed hash gave these two the same digest.
    assert_ne!(
        derive_action_id("bot", 0, 0, "dom"),
        derive_action_id("bo", 0, 0, "tdom"),
        "length framing keeps \"bot\"+\"dom\" apart from \"bo\"+\"tdom\""
    );
    Ok(())
}

#[test]
fn an_unattempted_false_condition_still_skips_and_lets_its_successor_run() -> TestResult {
    // The control for issue #104's barrier: a false condition on an entry
    // that was never attempted is still a legitimate `Skip`, and the
    // successor still runs. The barrier exists for a *recovered unknown*,
    // not for every false condition — without this test the repair could
    // stop the chain at every skip and read as correct.
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut bot = EcsBot::builder("legitimate-skip")
        .observe(Script::new(vec![200]))
        .on(|value: &u16| *value < 200, Record(Rc::clone(&log)))
        .on(|value: &u16| *value >= 200, Record(Rc::clone(&log)))
        .with_effects(test_effects()?)
        .build(&net_grants())?;

    assert_eq!(bot.tick()?, 1, "the successor ran behind a skipped entry");
    assert_eq!(
        *log.borrow(),
        vec![200],
        "exactly one action, the successor's: {:?}",
        *log.borrow()
    );
    assert_eq!(bot.revisions(), vec![1], "the transition resolved");
    assert!(
        bot.pending().is_empty(),
        "a skipped entry owes nothing: {:?}",
        bot.pending()
    );
    Ok(())
}

#[test]
fn a_condition_that_cannot_be_evaluated_holds_the_chain_without_claiming_anything() -> TestResult {
    // A condition failure decides nothing about the effect. The entry has
    // not been attempted, nothing about it is claimed, and the entry behind
    // it is not run ahead of a question that has no answer — the failure
    // the old substrate turned into "this chain is done".
    //
    // The mismatched condition is built *behind* the builder now, because
    // the typed `on` refuses it: a `u16` source cannot carry a condition
    // that reads a `String`. That refusal is the front half of this fix, and
    // the back half is still needed, because erasure is not the only way to
    // reach a mis-paired chain — anything holding this world can insert an
    // entry. `typed_entry` is the erasure helper and keeps its free `T`, so
    // it can still express the defect; this test is the one place that does,
    // on purpose, to pin what happens when it is reached.
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut bot = EcsBot::builder("mismatched")
        .observe(Script::new(vec![200, 200]))
        .on(|value: &u16| *value >= 200, Record(Rc::clone(&log)))
        .with_effects(test_effects()?)
        .build(&net_grants())?;
    bot.world.non_send_mut::<Chains>().0[0].entries.insert(
        0,
        typed_entry::<_, _, String>(|value: &String| value.len() >= 3, Record(Rc::clone(&log))),
    );

    let error = refused(bot.tick(), |fired| {
        format!("an evaluate error was reported as {fired} fired")
    })?;
    assert!(
        matches!(error, BotError::EvaluateError { .. }),
        "expected the condition's own error, got {error:?}"
    );
    assert!(log.borrow().is_empty(), "nothing was attempted");
    assert_eq!(
        identities(&bot),
        vec![(0, 0), (0, 1)],
        "the entry whose condition failed is still outstanding, and so is the entry \
         behind it: {:?}",
        bot.pending()
    );
    assert_eq!(
        first_hold(&bot),
        Some(TransitionHold::NotStarted),
        "a condition that cannot be evaluated is not an attempt, so nothing is abandoned"
    );

    // And it stays that way rather than decaying into an abandonment: the
    // report is the truth, not a stage on the way to losing the work.
    let error = refused(bot.tick(), |fired| {
        format!("the second tick reported {fired} fired")
    })?;
    assert!(
        matches!(error, BotError::EvaluateError { .. }),
        "the condition is evaluated again, not silently given up on: {error:?}"
    );
    assert!(log.borrow().is_empty(), "and still nothing was attempted");
    Ok(())
}

#[test]
fn a_condition_failure_stops_the_walk_after_the_effects_it_cleared() -> TestResult {
    // The other half of "partial run", on the condition side, and a
    // characterisation rather than a new guarantee: this passes before and
    // after the decision phase was split out of the effect phase, and it is
    // here because that split is only sound if it does.
    //
    // Conditions are pure over the observed value, so the whole effect
    // program can be recorded before any of it runs. The equivalence to the
    // interleaved loop it replaced rests on exactly three things: the
    // entries before a failing condition still run, the entries after it
    // are not attempted, and the tick reports the condition's own error.
    let log = Rc::new(RefCell::new(Vec::new()));
    let after = Rc::new(Cell::new(0));
    let mut bot = EcsBot::builder("condition-failure")
        .observe(Script::new(vec![200, 200]))
        .on(|value: &u16| *value >= 200, Record(Rc::clone(&log)))
        .on(RefusesToEvaluate, Count(Rc::clone(&after)))
        .on(|value: &u16| *value >= 200, Count(Rc::clone(&after)))
        .with_effects(test_effects()?)
        .build(&net_grants())?;

    let error = refused(bot.tick(), |fired| {
        format!("a refused condition was reported as {fired} fired")
    })?;
    assert!(
        matches!(error, BotError::EvaluateError { .. }),
        "expected the condition's own error, got {error:?}"
    );
    assert_eq!(
        *log.borrow(),
        vec![200],
        "the effect the walk cleared before the failing condition ran"
    );
    assert_eq!(
        after.get(),
        0,
        "the entries after the failing condition were not attempted"
    );
    Ok(())
}

#[test]
fn an_ambiguous_schedule_is_refused_at_build() -> TestResult {
    let mut world = World::new();

    // Two exclusive systems conflict with everything, including each other,
    // so their relative order is indeterminate.
    let mut ambiguous = Schedule::default();
    ambiguous.set_build_settings(ScheduleBuildSettings {
        ambiguity_detection: LogLevel::Error,
        ..Default::default()
    });
    ambiguous.add_systems((observe_fold, fire_plan));

    match validate(&mut ambiguous, &mut world) {
        Ok(()) => return Err("an ambiguous schedule was accepted at build".into()),
        Err(error) => assert!(
            matches!(error, BotError::DomainError { .. }),
            "a schedule-build failure must surface as a typed BotError, got {error:?}"
        ),
    }

    // Control: the same two systems, ordered, must validate. Without this
    // the refusal above could be any failure at all.
    let mut ordered = Schedule::default();
    ordered.set_build_settings(ScheduleBuildSettings {
        ambiguity_detection: LogLevel::Error,
        ..Default::default()
    });
    ordered.add_systems((observe_fold, fire_plan).chain());
    validate(&mut ordered, &mut world)?;
    Ok(())
}

/// An action that declares `Input = u32` while counting its attempts.
///
/// Paired below with a `u16` source: the pairing is the fault, and the
/// count is how the test tells "one attempt" from "the whole budget".
struct CountsU32(Rc<Cell<usize>>);

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

#[test]
fn a_mispairing_behind_the_erasure_is_a_witness_miss() -> TestResult {
    // The pairing from `Chains` to `Observed` is by index, and a vector
    // position carries no type. This is that pairing gone wrong: the chain
    // declares a `u16` source, and the staged value is a `u32`.
    //
    // Written into the staging resource directly, because the shipped path
    // cannot produce this state: `poll_sources` boxes the output of the very
    // source it took the witness from, so a disagreement means the world
    // moved behind the schedule's back. That is precisely the state the
    // witness exists to refuse rather than to downcast, and it is why the
    // test has to construct it rather than trigger it.
    //
    // The assertions are the three things the witness is for. It fires
    // *before* the value is used, so nothing is committed and no condition
    // is asked. It names both types and the site. And it is a mismatch, not
    // a domain failure, so the disposition is terminal and no retry budget
    // is touched.
    let ran = Rc::new(Cell::new(0));
    let mut bot = EcsBot::builder("mispairing")
        .observe(Script::new(vec![200]))
        .on(
            |value: &u16| *value >= 200,
            Refuses::undelivered(Rc::clone(&ran)),
        )
        .with_effects(test_effects()?)
        .build(&net_grants())?;

    // `Ok(Some(_))`: a value that *moved*, which is the only shape that
    // carries a witness to check. A source answering `Ok(None)` has proven
    // its pairing by downcasting the baseline to its own output type, and
    // there is nothing left for the rendezvous to inspect.
    bot.world.non_send_mut::<Polled>().0 = vec![Ok(Some(Erased::new(300u32)))];
    bot.schedule.run(&mut bot.world);

    match bot.world.resource::<TickError>().0 {
        Some(BotError::TypeMismatch {
            site,
            chain,
            expected,
            observed,
        }) => {
            assert_eq!(site, "observe_fold rendezvous", "the site is greppable");
            assert_eq!(chain, Some(0), "the report names which chain disagreed");
            assert_eq!(expected, "u16", "the chain's claim");
            assert_eq!(observed, "u32", "what actually arrived");
        }
        ref other => {
            return Err(format!("expected a witness miss, got {other:?}").into());
        }
    }

    assert!(
        bot.world
            .non_send::<Observed>()
            .0
            .iter()
            .all(Option::is_none),
        "the fold commits nothing on a mismatch, so the previous tick's state survives"
    );
    assert_eq!(ran.get(), 0, "the action was never reached");
    Ok(())
}

#[test]
fn a_mismatched_pairing_costs_one_attempt_not_the_retry_budget() -> TestResult {
    // The chain is wired for `u16` while the action declares `u32`. That is
    // a wiring defect behind the erasure, not a domain failure: no number
    // of retries reaches a different answer, so it costs one attempt and
    // lands as terminal rather than spending the whole budget and being
    // reported as "we ran out of budget" — which names the budget as the
    // reason when the reason is that the two halves disagree.
    //
    // The report names both types, which is what makes it a diagnosis
    // rather than a report that something is wrong somewhere. It names no
    // domain, because no domain was reached.
    //
    // The attempt count is read from the ledger rather than counted by the
    // action, because the action must never be reached: the downcast is
    // checked before the proof is issued, so a mismatch costs no side
    // effect. `ran` is that second half.
    // Built behind the builder for the same reason as the condition
    // mismatch above: `on` no longer admits `CountsU32` on a `u16` source,
    // which is the fix, and the downcast arm is still what reports the
    // mis-pairing that reaches the erasure boundary anyway.
    let ran = Rc::new(Cell::new(0));
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut bot = EcsBot::builder("mismatched")
        .observe(Script::new(vec![200, 200, 200, 200]))
        .on(|value: &u16| *value >= 200, Record(Rc::clone(&log)))
        .with_effects(test_effects()?)
        .build(&net_grants())?;
    bot.world.non_send_mut::<Chains>().0[0].entries[0] =
        typed_entry::<_, _, u16>(|value: &u16| *value >= 200, CountsU32(Rc::clone(&ran)));

    let error = refused(bot.tick(), |fired| {
        format!("a wiring mismatch was reported as {fired} fired")
    })?;
    assert!(
        matches!(
            error,
            BotError::TypeMismatch {
                site: "spec::typed_entry",
                expected: "u32",
                observed: "u16",
                ..
            }
        ),
        "a wiring defect is a type mismatch naming both types, not a domain failure: {error:?}"
    );
    assert_eq!(
        ran.get(),
        0,
        "the downcast is checked before the action runs, so the mismatch was not a side effect"
    );
    assert!(
        matches!(
            first_hold(&bot),
            Some(TransitionHold::Abandoned {
                reason: AbandonReason::Terminal,
                ..
            })
        ),
        "one attempt, and the disposition is terminal rather than `AttemptsExhausted`: {:?}",
        first_hold(&bot)
    );

    // The second tick must not spend a second attempt on the same answer.
    // That is the observable difference the budget makes: `AttemptsExhausted`
    // is a fact about how many times the substrate was willing to ask, and
    // asking twice here buys nothing.
    let error = refused(bot.tick(), |fired| {
        format!("a retried wiring mismatch reported {fired} fired")
    })?;
    assert!(
        matches!(error, BotError::PendingTransition { .. }),
        "the abandoned entry is the barrier the next tick reports, got {error:?}"
    );
    Ok(())
}

#[test]
fn a_permanent_refusal_does_not_burn_the_retry_budget() -> TestResult {
    // A refusal that happens before dispatch and cannot be changed by
    // repeating it. Retrying spends attempts to learn the same answer, and
    // the report then blames the budget for a failure the budget was never
    // the reason for.
    let attempts = Rc::new(Cell::new(0));
    let mut bot = EcsBot::builder("permanent")
        .observe(Script::new(vec![200, 200, 200, 200]))
        .on(
            |value: &u16| *value >= 200,
            Refuses::permanently(Rc::clone(&attempts)),
        )
        .with_effects(test_effects()?)
        .build(&net_grants())?;

    let error = refused(bot.tick(), |fired| {
        format!("a permanent refusal was reported as {fired} fired")
    })?;
    assert!(
        matches!(error, BotError::DomainError { .. }),
        "expected the action's own error, got {error:?}"
    );
    assert_eq!(attempts.get(), 1, "a permanent refusal is attempted once");
    assert!(
        matches!(
            first_hold(&bot),
            Some(TransitionHold::Abandoned {
                reason: AbandonReason::Terminal,
                ..
            })
        ),
        "the disposition is terminal, not `AttemptsExhausted`: {:?}",
        first_hold(&bot)
    );
    Ok(())
}
