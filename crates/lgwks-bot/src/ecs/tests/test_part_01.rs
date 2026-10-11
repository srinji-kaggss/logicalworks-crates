//! ECS bot unit tests, part 01.

use super::*;

/// A failed sibling poll must not teach the fingerprint cache about a
/// value the fold refused to hold.
///
/// The counterexample, stated as issue #99 states it: A reports payload `1`
/// with digest `Some(1)` and B fails its first poll. The first tick fires
/// nothing under the all-or-nothing observation contract. Before the
/// repair A's digest was published anyway, so the second tick read A as
/// quiet and skipped it despite holding no committed payload — A's action
/// never ran unless A changed again.
#[test]
fn a_failed_sibling_does_not_publish_the_successful_sources_digest() -> TestResult {
    let left_polls = Rc::new(Cell::new(0));
    let right_polls = Rc::new(Cell::new(0));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let left = Switched::new(1, false, Rc::clone(&left_polls));
    let mid = Switched::new(9, true, Rc::clone(&right_polls));
    let mid_fail = Rc::clone(&mid.fail);
    let mut bot = EcsBot::builder("sibling")
        .observe(left)
        .on(|value: &u16| *value >= 1, Record(Rc::clone(&seen)))
        .observe(mid)
        .on(
            |value: &u16| *value >= 9,
            Record(Rc::new(RefCell::new(Vec::new()))),
        )
        .with_effects(test_effects()?)
        .build(&net_grants())?;

    let error = refused(bot.tick(), |fired| {
        format!("a failing sibling was reported as {fired} fired")
    })?;
    assert!(
        matches!(error, BotError::DomainError { .. }),
        "expected the sibling's own error, got {error:?}"
    );
    assert!(
        seen.borrow().is_empty(),
        "the failed fold must not commit A's payload"
    );
    assert_eq!(left_polls.get(), 1, "A was polled once on the first tick");
    assert_eq!(right_polls.get(), 1, "and so was B");

    mid_fail.set(false);
    assert_eq!(bot.tick()?, 2, "both chains recover and both actions fire");
    assert_eq!(
        *seen.borrow(),
        vec![1],
        "A's uncommitted payload is committed on recovery, with 1 as its input"
    );
    assert_eq!(
        left_polls.get(),
        2,
        "A is polled again: its digest must not have been published early"
    );
    assert_eq!(right_polls.get(), 2, "B recovers on a poll of its own");
    Ok(())
}

/// The second half of issue #99's public scenario: a *previously committed*
/// value changes while a sibling fails, and the recovery must keep the
/// transition to the new value rather than the older committed one.
#[test]
fn a_committed_source_that_moves_during_a_sibling_failure_is_not_lost() -> TestResult {
    let left_polls = Rc::new(Cell::new(0));
    let right_polls = Rc::new(Cell::new(0));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let left = Switched::new(0, false, Rc::clone(&left_polls));
    let left_value = Rc::clone(&left.value);
    let mid = Switched::new(9, false, Rc::clone(&right_polls));
    let mid_fail = Rc::clone(&mid.fail);
    let mut bot = EcsBot::builder("recovery")
        .observe(left)
        .on(|value: &u16| *value >= 1, Record(Rc::clone(&seen)))
        .observe(mid)
        .on(|_: &u16| true, Record(Rc::new(RefCell::new(Vec::new()))))
        .with_effects(test_effects()?)
        .build(&net_grants())?;

    assert_eq!(bot.tick()?, 1, "the first tick commits A=0 and B=9");
    assert_eq!(
        *seen.borrow(),
        Vec::<u16>::new(),
        "A=0 does not meet its condition"
    );
    assert_eq!(left_polls.get(), 1);

    left_value.set(1);
    mid_fail.set(true);
    let error = refused(bot.tick(), |fired| {
        format!("a failing sibling was reported as {fired} fired")
    })?;
    assert!(
        matches!(error, BotError::DomainError { .. }),
        "expected the sibling's own error, got {error:?}"
    );
    assert!(
        seen.borrow().is_empty(),
        "the failed fold must not commit A=1"
    );

    mid_fail.set(false);
    assert_eq!(
        bot.tick()?,
        1,
        "recovery commits A's move; B is unchanged and fires nothing"
    );
    assert_eq!(
        *seen.borrow(),
        vec![1],
        "the transition to 1 is retained, not the older committed 0"
    );
    assert_eq!(
        left_polls.get(),
        3,
        "A is polled on the failing tick and again on recovery"
    );
    Ok(())
}

/// Several siblings fail on one tick; every successful source must still
/// deliver its payload once the fold can commit.
#[test]
fn several_failed_siblings_do_not_suppress_the_sources_that_succeeded() -> TestResult {
    let polls_left = Rc::new(Cell::new(0));
    let polls_mid = Rc::new(Cell::new(0));
    let polls_right = Rc::new(Cell::new(0));
    let seen_left = Rc::new(RefCell::new(Vec::new()));
    let seen_right = Rc::new(RefCell::new(Vec::new()));
    let left = Switched::new(1, false, Rc::clone(&polls_left));
    let mid = Switched::new(2, true, Rc::clone(&polls_mid));
    let right = Switched::new(3, false, Rc::clone(&polls_right));
    // A second failing source, so "the first error wins" is not the only
    // thing the fold is being asked to survive.
    let far = Switched::new(4, true, Rc::new(Cell::new(0)));
    let mid_fail = Rc::clone(&mid.fail);
    let far_fail = Rc::clone(&far.fail);
    let mut bot = EcsBot::builder("multi-fail")
        .observe(left)
        .on(|value: &u16| *value >= 1, Record(Rc::clone(&seen_left)))
        .observe(mid)
        .on(|_: &u16| true, Record(Rc::new(RefCell::new(Vec::new()))))
        .observe(right)
        .on(|value: &u16| *value >= 3, Record(Rc::clone(&seen_right)))
        .observe(far)
        .on(|_: &u16| true, Record(Rc::new(RefCell::new(Vec::new()))))
        .with_effects(test_effects()?)
        .build(&net_grants())?;

    if let Ok(fired) = bot.tick() {
        return Err(format!("failed siblings were reported as {fired} fired").into());
    }
    assert!(
        seen_left.borrow().is_empty(),
        "nothing commits on a failed fold"
    );
    assert!(
        seen_right.borrow().is_empty(),
        "nothing commits on a failed fold"
    );
    assert_eq!(polls_left.get(), 1);
    assert_eq!(polls_right.get(), 1);

    mid_fail.set(false);
    far_fail.set(false);
    assert_eq!(bot.tick()?, 4, "every chain recovers and fires");
    assert_eq!(*seen_left.borrow(), vec![1], "A delivers its payload");
    assert_eq!(*seen_right.borrow(), vec![3], "C delivers its payload");
    assert_eq!(
        polls_left.get(),
        2,
        "A was re-polled after the aborted fold"
    );
    assert_eq!(
        polls_right.get(),
        2,
        "C was re-polled after the aborted fold"
    );
    Ok(())
}

/// A tick dropped mid-poll leaves the next tick able to observe normally.
#[test]
fn cancelling_a_poll_leaves_the_next_observation_usable() -> TestResult {
    let polls = Rc::new(Cell::new(0));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let yielded = Rc::new(Cell::new(false));
    let source = Yielding::new(200, Rc::clone(&yielded), Rc::clone(&polls));
    let mut bot = EcsBot::builder("cancel")
        .observe(source)
        .on(|value: &u16| *value >= 200, Record(Rc::clone(&seen)))
        .with_effects(test_effects()?)
        .build(&net_grants())?;

    // Drive the tick until the source parks, then drop it on the floor.
    {
        let mut tick = Box::pin(bot.tick_async());
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        match tick.as_mut().poll(&mut cx) {
            Poll::Ready(_) => {
                return Err("the yielding source resolved before it was cancelled".into());
            }
            Poll::Pending => {}
        }
        drop(tick);
    }
    assert!(seen.borrow().is_empty(), "a cancelled tick commits nothing");

    assert_eq!(bot.tick()?, 1, "the recovery tick completes and fires");
    assert_eq!(*seen.borrow(), vec![200], "with the payload as its input");
    assert_eq!(
        polls.get(),
        2,
        "the source is polled once per tick across the cancel"
    );

    assert_eq!(bot.tick()?, 0, "and the next tick compares the same value");
    assert_eq!(polls.get(), 3, "the next tick polls after a cancellation");
    Ok(())
}

/// A retained transition keeps its immutable input across a sibling failure
/// that admits a newer candidate and then aborts.
///
/// Acceptance case 4 of issue #99. The held entry is bound to the payload
/// it was opened under. A later tick moves the source and fails on a
/// sibling; the fold refuses that tick wholesale. After recovery the
/// binding must still be the older payload, and the newer value must not
/// have been folded into it — and must not have been lost either.
#[test]
fn a_held_transition_keeps_its_input_across_a_sibling_failure() -> TestResult {
    let logs = Rc::new(RefCell::new(Vec::new()));
    let mid_polls = Rc::new(Cell::new(0));
    let mid = Switched::new(9, false, Rc::clone(&mid_polls));
    let mid_fail = Rc::clone(&mid.fail);
    let left = Holds::new(200);
    let mut bot = EcsBot::builder("held-newer")
        .observe(left)
        .on(
            |value: &u16| *value >= 200,
            Flaky {
                remaining: Rc::new(Cell::new(1)),
                log: Rc::clone(&logs),
                kind: FlakyKind::Indeterminate,
            },
        )
        .observe(mid)
        .on(|_: &u16| true, Record(Rc::new(RefCell::new(Vec::new()))))
        .with_effects(test_effects()?)
        .build(&net_grants())?;

    let error = refused(bot.tick(), |fired| {
        format!("an indeterminate effect was reported as {fired} fired")
    })?;
    assert!(
        matches!(error, BotError::EffectIndeterminate { .. }),
        "expected the action's own error, got {error:?}"
    );
    assert_eq!(identities(&bot), vec![(0, 0)], "the held entry is named");
    assert!(
        matches!(
            first_hold(&bot),
            Some(TransitionHold::OutcomeUnknown { attempts: 1, .. })
        ),
        "the first tick holds the attempt: {:?}",
        first_hold(&bot)
    );
    assert!(logs.borrow().is_empty(), "nothing was recorded yet");

    // A sibling failure aborts the fold before any newer candidate can be
    // admitted. The held entry is untouched: same identity, same hold.
    mid_fail.set(true);
    let error = refused(bot.tick(), |fired| {
        format!("a failing sibling was reported as {fired} fired")
    })?;
    assert!(
        matches!(error, BotError::DomainError { .. }),
        "expected the sibling's own error, got {error:?}"
    );
    assert_eq!(
        identities(&bot),
        vec![(0, 0)],
        "the held entry survives an aborted fold"
    );
    assert!(
        matches!(
            first_hold(&bot),
            Some(TransitionHold::OutcomeUnknown { attempts: 1, .. })
        ),
        "and is still held on the same attempt: {:?}",
        first_hold(&bot)
    );

    // Recovery. The held entry is still not re-attempted without evidence,
    // and the sibling's own work is what the tick is able to report.
    mid_fail.set(false);
    match bot.tick() {
        Ok(_) => {}
        Err(error) => assert!(
            matches!(error, BotError::PendingTransition { .. }),
            "the held entry is still outstanding, got {error:?}"
        ),
    }
    assert!(
        logs.borrow().is_empty(),
        "a held effect is not attempted across a recovery without evidence"
    );

    // Evidence says it did not happen. The entry runs against the input it
    // was bound to — `Holds` never moved, so the immutable input is 200 and
    // there is no newer candidate that could have replaced it.
    let held = bot
        .pending()
        .into_iter()
        .find(|work| identity(work) == (0, 0))
        .ok_or("the held entry disappeared from the report")?;
    bot.resolve_effect(&held_key(&held)?, EffectEvidence::NotApplied)?;
    assert_eq!(bot.tick()?, 1, "the entry runs once evidence authorises it");
    assert_eq!(
        *logs.borrow(),
        vec![200],
        "the retained transition's immutable input is the one it was opened under"
    );
    Ok(())
}

#[test]
fn a_failure_mid_chain_leaves_the_untouched_entries_pending() -> TestResult {
    // The counterexample to "a tick is all-or-nothing", and the regression
    // for the repair. Three entries: the first records an effect, the
    // second is refused once, the third is never reached on the failing
    // tick.
    //
    // Before the repair this test asserted the *loss*: the revision was
    // committed before any action ran, so the unchanged second tick fired
    // nothing, the refused entry was never retried, and the third entry's
    // effect was absent forever with no record of it. That assertion
    // encoded the defect as correct and has been replaced.
    //
    // The script holds still at 200 so the second tick has an unchanged
    // source, which is what makes both halves falsifiable: a moving source
    // would fire again for reasons unrelated to the failure.
    let first = Rc::new(RefCell::new(Vec::new()));
    let second = Rc::new(RefCell::new(Vec::new()));
    let third = Rc::new(RefCell::new(Vec::new()));
    let mut bot = EcsBot::builder("partial")
        .observe(Script::new(vec![200, 200]))
        .on(|value: &u16| *value >= 200, Record(Rc::clone(&first)))
        .on(
            |value: &u16| *value >= 200,
            Flaky {
                remaining: Rc::new(Cell::new(1)),
                log: Rc::clone(&second),
                kind: FlakyKind::Refused,
            },
        )
        .on(|value: &u16| *value >= 200, Record(Rc::clone(&third)))
        .with_effects(test_effects()?)
        .build(&net_grants())?;

    let error = refused(bot.tick(), |fired| {
        format!("a refused action was reported as {fired} fired")
    })?;
    assert!(
        matches!(error, BotError::DomainError { .. }),
        "expected the action's own error, got {error:?}"
    );
    assert_eq!(
        *first.borrow(),
        vec![200],
        "the action before the failure ran and its effect is not rolled back"
    );
    assert!(
        second.borrow().is_empty(),
        "the refused action recorded nothing"
    );
    assert!(
        third.borrow().is_empty(),
        "the entry after the failure was not attempted on the failing tick"
    );
    assert_eq!(
        identities(&bot),
        vec![(0, 1), (0, 2)],
        "the refused entry and the entry behind it are both still outstanding"
    );
    assert!(
        matches!(
            first_hold(&bot),
            Some(TransitionHold::Failed {
                attempts: 1,
                budget: 3,
                ..
            })
        ),
        "the refused entry is held as a definite failure with the attempt counted: {:?}",
        first_hold(&bot)
    );

    // The source has not moved. The retry and the untouched third entry run
    // anyway, because eligible work is retained rather than re-derived from
    // the change filter.
    assert_eq!(
        bot.tick()?,
        2,
        "the refused entry is retried and the entry behind it runs, on a still source"
    );
    assert_eq!(
        *first.borrow(),
        vec![200],
        "the acknowledged first effect is not replayed to reach the third entry"
    );
    assert_eq!(
        *second.borrow(),
        vec![200],
        "the retried action ran exactly once, on the second tick"
    );
    assert_eq!(
        *third.borrow(),
        vec![200],
        "the effect the old code lost ran once the entry ahead of it settled"
    );
    assert!(
        bot.pending().is_empty(),
        "a clean tick means nothing is left holding the transition: {:?}",
        bot.pending()
    );
    Ok(())
}

#[test]
fn a_failure_before_the_first_effect_still_runs_it_once() -> TestResult {
    // The narrowest version of the defect: nothing has happened yet, the
    // first entry never stops failing, and the entry behind it must still
    // run — after the attempt budget is spent and the entry is abandoned,
    // which is reported rather than silent.
    let attempts = Rc::new(Cell::new(0));
    let log = Rc::new(RefCell::new(Vec::new()));
    // Seven polls: four at the threshold, then three below it, so the ticks
    // after the abandonment open no new entry and the ledger it leaves is
    // the one asserted on.
    let mut bot = EcsBot::builder("refusing")
        .observe(Script::new(vec![200, 200, 200, 200, 0, 0, 0]))
        .on(
            |value: &u16| *value >= 200,
            Refuses::undelivered(Rc::clone(&attempts)),
        )
        .on(|value: &u16| *value >= 200, Record(Rc::clone(&log)))
        .with_effects(test_effects()?)
        .build(&net_grants())?;

    for round in 1..=2 {
        let error = refused(bot.tick(), |fired| {
            format!("round {round} reported {fired} fired")
        })?;
        assert!(
            matches!(error, BotError::DomainError { .. }),
            "round {round}: expected the action's own error, got {error:?}"
        );
    }
    assert_eq!(attempts.get(), 2, "two ticks, two attempts");
    assert!(
        log.borrow().is_empty(),
        "the second entry is not run ahead of the entry holding it back"
    );
    assert_eq!(
        identities(&bot),
        vec![(0, 0), (0, 1)],
        "both entries are outstanding while the first keeps failing"
    );

    // Third attempt: the budget is spent and the entry is given up on
    // rather than retried forever. The tick reports the action's own typed
    // error, not a summary of it: a caller classifies a retry by matching
    // the variant, and rendering it into a string would throw that away.
    let error = refused(bot.tick(), |fired| {
        format!("a spent budget was reported as {fired} fired")
    })?;
    assert!(
        matches!(error, BotError::DomainError { .. }),
        "the abandonment does not replace the error that caused it: {error:?}"
    );
    assert_eq!(attempts.get(), 3, "exactly the declared budget was spent");

    // Giving up is reported, never silent: the entry is named with the
    // reason and the cause, and stays named.
    assert_eq!(
        first_hold(&bot),
        Some(TransitionHold::Abandoned {
            reason: AbandonReason::AttemptsExhausted { attempts: 3 },
            cause: "test::refuses: refused".into(),
        }),
        "the given-up-on entry is named after the tick that gave up on it"
    );

    // And the work it was holding back is not lost with it — but it is not
    // run either. The walk stops at the attempt that spent the budget, and
    // the abandonment it leaves is a barrier: the successor stays
    // `NotStarted` behind it and stays reported.
    //
    // These two assertions used to read `assert_eq!(bot.tick()?, 1, "the
    // unattempted work runs as soon as the entry ahead of it is decided")`
    // followed by `assert_eq!(*log.borrow(), vec![200])`, and later
    // `assert_eq!(bot.tick()?, 0, "the resolved transition fires nothing")`
    // while `pending()` was non-empty. Both are the F02 defect written down
    // as the intent — a successor executed past an abandoned prerequisite,
    // and a clean tick reported over work the ledger still names. They are
    // corrected rather than loosened: the tick is now expected to refuse
    // where it was expected to succeed.
    assert!(
        log.borrow().is_empty(),
        "the successor is not run ahead of the entry that was just decided"
    );
    let error = refused(bot.tick(), |fired| {
        format!(
            "a tick with an abandoned prerequisite reported {fired} fired and ran {:?}",
            log.borrow()
        )
    })?;
    assert!(
        matches!(error, BotError::PendingTransition { .. }),
        "expected the unresolved chain to be reported, got {error:?}"
    );
    assert!(
        log.borrow().is_empty(),
        "the unattempted work does not run past the entry that was given up on"
    );

    // Terminal, and the rest of the chain is blocked behind it, so the
    // transition stays reported: the abandonment first, because it is the
    // entry that explains the one behind it.
    let error = refused(bot.tick(), |fired| {
        format!(
            "an abandoned entry was reported as {fired} fired while pending() still \
                 names it: {:?}",
            bot.pending()
        )
    })?;
    assert!(
        matches!(error, BotError::PendingTransition { .. }),
        "expected the unresolved chain to be reported, got {error:?}"
    );
    assert!(
        identities(&bot) == vec![(0, 0), (0, 1)],
        "the abandoned entry and the entry it blocks are what is left to report: {:?}",
        bot.pending()
    );
    Ok(())
}

#[test]
fn one_attempt_is_a_policy_a_caller_can_declare() -> TestResult {
    // The budget is a policy, not a constant: with one attempt the same
    // chain abandons on the first refusal instead of the third.
    let attempts = Rc::new(Cell::new(0));
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut bot = EcsBot::builder("impatient")
        .with_retry_policy(RetryPolicy::ONE_ATTEMPT)
        .observe(Script::new(vec![200, 200, 200]))
        .on(
            |value: &u16| *value >= 200,
            Refuses::undelivered(Rc::clone(&attempts)),
        )
        .on(|value: &u16| *value >= 200, Record(Rc::clone(&log)))
        .with_effects(test_effects()?)
        .build(&net_grants())?;

    let error = refused(bot.tick(), |fired| {
        format!("a refusal was reported as {fired} fired")
    })?;
    assert!(
        matches!(error, BotError::DomainError { .. }),
        "expected the action's own error, got {error:?}"
    );
    assert_eq!(attempts.get(), 1, "the declared budget is one attempt");
    assert!(
        log.borrow().is_empty(),
        "the entry behind the abandoned one is not run in the tick that abandoned it"
    );

    // The next tick does not run what the abandoned entry was holding back,
    // and does not report a clean tick either. This assertion used to read
    // `assert_eq!(bot.tick()?, 1, "the next tick runs the work the abandoned
    // entry was holding back")`, which is the defect written down as the
    // intent: an abandonment is a prerequisite that is *not* satisfied, so
    // the send behind an unreserved draft must not run. It is corrected
    // rather than loosened — the tick is now expected to refuse, which is
    // strictly more than it was expected to do before.
    let error = refused(bot.tick(), |fired| {
        format!(
            "a tick with an abandoned prerequisite reported {fired} fired and ran {:?}",
            log.borrow()
        )
    })?;
    assert!(
        matches!(error, BotError::PendingTransition { .. }),
        "expected the unresolved chain to be reported, got {error:?}"
    );
    assert_eq!(attempts.get(), 1, "the abandoned entry is not retried");
    assert!(
        log.borrow().is_empty(),
        "the successor of an abandoned entry is not attempted while it stands abandoned"
    );
    assert_eq!(
        identities(&bot),
        vec![(0, 0), (0, 1)],
        "the abandonment and the entry it blocks are both still reported: {:?}",
        bot.pending()
    );

    // Evidence that the effect did not happen is the way past the barrier:
    // the entry becomes eligible again and its successor with it. The
    // action refuses once more, so this tick abandons it again — which is
    // the point of asserting it: evidence reopens the chain, it does not
    // promise the retry will succeed.
    let blocked = bot.pending().into_iter().next().ok_or("held")?;
    bot.resolve_effect(&held_key(&blocked)?, EffectEvidence::NotApplied)?;
    let error = refused(bot.tick(), |fired| {
        format!("a refusal was reported as {fired} fired")
    })?;
    assert!(
        matches!(error, BotError::DomainError { .. }),
        "expected the retry's own error, got {error:?}"
    );
    assert_eq!(attempts.get(), 2, "the revived entry attempted again");
    assert!(
        log.borrow().is_empty(),
        "and the successor is behind the barrier again, because the retry failed too"
    );
    Ok(())
}

#[test]
fn a_failure_does_not_stop_a_later_chain() -> TestResult {
    // The second half of the defect: the old loop broke out of *every*
    // chain on the first error, so an independent chain behind a broken one
    // never ran, and never would.
    let attempts = Rc::new(Cell::new(0));
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut bot = EcsBot::builder("two-chains")
        .observe(Script::new(vec![200, 200]))
        .on(
            |value: &u16| *value >= 200,
            Refuses::undelivered(Rc::clone(&attempts)),
        )
        .observe(Script::new(vec![200, 200]))
        .on(|value: &u16| *value >= 200, Record(Rc::clone(&log)))
        .with_effects(test_effects()?)
        .build(&net_grants())?;

    let error = refused(bot.tick(), |fired| {
        format!("a refused action was reported as {fired} fired")
    })?;
    assert!(
        matches!(error, BotError::DomainError { .. }),
        "expected the action's own error, got {error:?}"
    );
    assert_eq!(
        *log.borrow(),
        vec![200],
        "the chain after the failure ran: a failure is contained to its own chain"
    );
    assert_eq!(
        identities(&bot),
        vec![(0, 0)],
        "only the refusing chain has work outstanding: {:?}",
        bot.pending()
    );
    Ok(())
}

#[test]
fn an_indeterminate_effect_is_held_until_evidence_says_what_happened() -> TestResult {
    // The effect may have happened, so a retry is a possible duplicate. The
    // substrate refuses to guess: it holds the entry, reports it every
    // tick, and waits for evidence.
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut bot = indeterminate_once_bot("indeterminate", &log)?;

    let error = refused(bot.tick(), |fired| {
        format!("an indeterminate effect was reported as {fired} fired")
    })?;
    assert!(
        matches!(error, BotError::EffectIndeterminate { .. }),
        "expected the action's own error, got {error:?}"
    );
    assert!(
        log.borrow().is_empty(),
        "an indeterminate effect is not an effect: nothing was recorded"
    );

    // The source has not moved, and the entry is not re-attempted: a blind
    // retry is exactly the duplicate this state exists to prevent.
    let error = refused(bot.tick(), |fired| {
        format!("a held effect was reported as {fired} fired")
    })?;
    let BotError::PendingTransition { work, outstanding } = error else {
        return Err(format!("expected PendingTransition, got {error:?}").into());
    };
    assert_eq!(identity(&work), (0, 0), "the held entry is named");
    assert!(
        matches!(
            work.hold(),
            TransitionHold::OutcomeUnknown { attempts: 1, .. }
        ),
        "the hold carries the attempt that could not be settled: {:?}",
        work.hold()
    );
    assert_eq!(outstanding, 1, "one entry is still open");
    assert!(
        bot.pending()
            .iter()
            .all(|work| !matches!(work.hold(), TransitionHold::Abandoned { .. })),
        "nothing was given up on: {:?}",
        bot.pending()
    );
    assert!(
        log.borrow().is_empty(),
        "a held effect is not attempted again without evidence"
    );

    // Evidence that it did not happen: the entry becomes eligible again and
    // the attempt finally lands. The revision the report carries is passed
    // back with the identity, which is what makes this delivery sound: the
    // evidence is about the generation the caller was shown.
    let held = bot
        .pending()
        .into_iter()
        .next()
        .ok_or("the held entry disappeared from the report")?;
    bot.resolve_effect(&held_key(&held)?, EffectEvidence::NotApplied)?;
    assert_eq!(bot.tick()?, 1, "the entry runs once evidence authorises it");
    assert_eq!(*log.borrow(), vec![200], "the effect happened, once");
    assert!(
        bot.pending().is_empty(),
        "and nothing is left holding the transition: {:?}",
        bot.pending()
    );
    Ok(())
}

#[test]
fn evidence_that_the_effect_happened_records_it_without_replaying_it() -> TestResult {
    // The other half of the evidence question, and the one that saves the
    // duplicate: the caller knows the effect is live, so it is acknowledged
    // and never attempted again.
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut bot = indeterminate_once_bot("acknowledged", &log)?;

    let error = refused(bot.tick(), |fired| {
        format!("an indeterminate effect was reported as {fired} fired")
    })?;
    assert!(
        matches!(error, BotError::EffectIndeterminate { .. }),
        "expected the action's own error, got {error:?}"
    );

    let held = bot
        .pending()
        .into_iter()
        .next()
        .ok_or("the held entry is not reported")?;
    bot.resolve_effect(&held_key(&held)?, EffectEvidence::Applied)?;
    assert_eq!(
        bot.tick()?,
        0,
        "an acknowledged effect is never attempted again"
    );
    assert!(
        log.borrow().is_empty(),
        "the action that may already have run was not run a second time"
    );
    assert_eq!(bot.revisions(), vec![1], "and the transition is resolved");
    assert!(
        bot.pending().is_empty(),
        "nothing is left pending: {:?}",
        bot.pending()
    );

    // A repeat of the same evidence after the slot is gone is accepted
    // idempotently. It used to be refused as `NoSuchWork`, and that is how
    // the same acknowledgement stopped being a safe retry across a restart:
    // a reconstructed bot has no live slot to consult, and the journal's
    // record is the fact that answers (issue #106). Nothing moves a second
    // time — the durable record is already there.
    bot.resolve_effect(&held_key(&held)?, EffectEvidence::Applied)?;
    // The other evidence is still a contradiction, and it is refused
    // rather than silently replacing what was recorded. `NoSuchWork` is
    // reserved for a key the journal never recorded; this one was, and the
    // distinction is the whole reason the refusals are typed apart.
    match bot.resolve_effect(&held_key(&held)?, EffectEvidence::NotApplied) {
        Ok(()) => Err("contradicting evidence was accepted".into()),
        Err(error) => {
            assert!(
                matches!(
                    error,
                    BotError::EvidenceContradicted {
                        settled: EffectEvidence::Applied,
                        submitted: EffectEvidence::NotApplied,
                        ..
                    }
                ),
                "expected EvidenceContradicted, got {error:?}"
            );
            Ok(())
        }
    }
}
