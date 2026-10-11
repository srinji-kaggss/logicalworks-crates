//! Abandonment is a barrier, a skip is not, and a transition keeps its payload.

use super::*;

/// T07, the subsequent change: a source moves to a new value while a sibling is
/// refusing, and the sibling then recovers. The moved source's update was never
/// committed, so it runs exactly once on recovery: not lost with the aborted
/// observation group, and not run again once it has committed.
#[test]
fn a_moved_source_runs_exactly_once_after_a_sibling_recovers() -> TestResult {
    let moving = Rc::new(Cell::new(17));
    let refusing = Rc::new(Cell::new(true));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut bot = Bot::builder("moved-source-sibling-recovery")
        .observe(Dial {
            value: Rc::clone(&moving),
        })
        .on(|_observed: &u32| true, Noted(Rc::clone(&seen)))
        .observe(PollProbe {
            domain: "test::poll_refuses_twice",
            value: 23,
            polls: Rc::new(Cell::new(0)),
            refuse_next: Rc::clone(&refusing),
        })
        .on(|_observed: &u32| true, Noted(Rc::clone(&seen)))
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())?;

    // The initial change: the sibling refuses, then recovers, and both fire.
    assert!(
        bot.tick().is_err(),
        "the first tick reports the sibling's refusal"
    );
    assert_eq!(
        bot.tick()?,
        2,
        "both sources fire once the sibling recovers"
    );
    assert_eq!(*seen.borrow(), vec![17, 23]);

    // The subsequent change: the source moves while the sibling refuses again.
    moving.set(18);
    refusing.set(true);
    let aborted = bot.tick();
    assert!(
        matches!(aborted, Err(BotError::DomainError { ref domain, .. }) if domain == "test::poll_refuses_twice"),
        "the second refusal is reported exactly, got {aborted:?}"
    );
    assert_eq!(
        *seen.borrow(),
        vec![17, 23],
        "an aborted observation admits no action, including for the moved source"
    );

    assert_eq!(
        bot.tick()?,
        1,
        "only the moved source fires on recovery; the sibling's value did not change"
    );
    assert_eq!(
        *seen.borrow(),
        vec![17, 23, 18],
        "the update that was held back runs with its own value"
    );
    assert_eq!(
        bot.tick()?,
        0,
        "a committed update is never run a second time"
    );
    assert_eq!(*seen.borrow(), vec![17, 23, 18]);
    Ok(())
}

/// A sibling's first refusal must not publish or cache away this source's
/// successful observation; the next public tick must poll it again and run
/// the action with the exact observed value.
#[test]
fn a_successful_public_poll_is_not_lost_when_a_sibling_refuses() -> TestResult {
    let successful_polls = Rc::new(Cell::new(0));
    let failing_polls = Rc::new(Cell::new(0));
    let refuse_next = Rc::new(Cell::new(true));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut bot = Bot::builder("poll-sibling-recovery")
        .observe(PollProbe {
            domain: "test::poll_success",
            value: 17,
            polls: Rc::clone(&successful_polls),
            refuse_next: Rc::new(Cell::new(false)),
        })
        .on(|_observed: &u32| true, Noted(Rc::clone(&seen)))
        .observe(PollProbe {
            domain: "test::poll_refuses_once",
            value: 23,
            polls: Rc::clone(&failing_polls),
            refuse_next: Rc::clone(&refuse_next),
        })
        .on(|_observed: &u32| true, Noted(Rc::clone(&seen)))
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())?;

    let first = bot.tick();
    assert!(
        matches!(first, Err(BotError::DomainError { ref domain, .. }) if domain == "test::poll_refuses_once"),
        "the first tick must report the sibling's exact refusal, got {first:?}"
    );
    assert!(
        seen.borrow().is_empty(),
        "an aborted observation admits no action"
    );

    assert_eq!(
        bot.tick()?,
        2,
        "both recovered observations reach their actions"
    );
    assert_eq!(
        successful_polls.get(),
        2,
        "the good sibling is polled again on recovery"
    );
    assert_eq!(
        failing_polls.get(),
        2,
        "the refusing sibling is retried on recovery"
    );
    assert_eq!(
        *seen.borrow(),
        vec![17, 23],
        "actions consume the actual successful values in source order"
    );
    Ok(())
}

/// An abandoned entry is never a quiet tick.
///
/// The short half of the counterexample, stated as the review states it: with
/// only the failed entry and nothing behind it, the second tick returned `Ok(0)`
/// while `pending()` still named the abandonment. A caller reading a clean `Ok`
/// as "this chain is handled" read something that was not true — the work had
/// not been handled, it had been given up on, and those call for different
/// responses from whoever owns the run.
///
/// Under a one-attempt policy ([`one_attempt`]) the first definite failure spends the
/// budget, so the entry is abandoned and the first tick reports the action's own
/// typed error rather than a summary of it. The source then holds still. What
/// the second tick owes the caller is the abandonment, named.
#[test]
fn an_abandoned_entry_is_never_a_quiet_tick() -> TestResult {
    let attempts = Rc::new(Cell::new(0));
    let mut lonely = Bot::builder("abandoned-alone")
        .with_retry_policy(one_attempt())
        .observe(Dial {
            value: Rc::new(Cell::new(1)),
        })
        .on(
            |_observed: &u32| true,
            Refuses {
                refusing: Rc::new(Cell::new(true)),
                attempts: Rc::clone(&attempts),
            },
        )
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())?;

    match lonely.tick() {
        Ok(fired) => return Err(format!("a definite failure was reported as {fired} fired").into()),
        Err(error) => assert!(
            matches!(error, BotError::DomainError { .. }),
            "expected the action's own error, got {error:?}"
        ),
    }
    assert_eq!(attempts.get(), 1, "the declared budget was one attempt");
    assert_eq!(
        held(&lonely).map(|(chain, entry, _, hold)| (chain, entry, hold)),
        Some((
            0,
            0,
            TransitionHold::Abandoned {
                reason: AbandonReason::AttemptsExhausted { attempts: 1 },
                cause: "test::refuses: refused".to_owned(),
            }
        )),
        "the abandonment is named after the tick that caused it"
    );

    // The source holds still, so there is nothing to attempt and — before the
    // repair — nothing to report either. The tick must not say the chain is
    // handled while `pending()` says otherwise.
    match lonely.tick() {
        Ok(fired) => {
            return Err(format!(
                "an abandoned entry was reported as {fired} fired while pending() still names \
                 it: {:?}",
                lonely.pending()
            )
            .into());
        }
        Err(error) => assert!(
            matches!(
                error,
                BotError::PendingTransition { ref work, outstanding: 1 }
                    if (work.id().chain(), work.id().entry()) == (0, 0)
            ),
            "expected the abandoned entry to be the reported work, got {error:?}"
        ),
    }
    assert_eq!(attempts.get(), 1, "an abandoned entry is not retried");
    assert!(
        !lonely.pending().is_empty(),
        "and it stays reported rather than being dropped"
    );
    Ok(())
}

/// T10, the skipped arm and its contrast. An entry whose condition was false is a
/// *decided* fact: it ran nothing, so it left nothing unsettled, and the entry
/// behind it runs. That is the other side of an abandoned or unknown predecessor,
/// which blocks its successors because something may have happened. A successor
/// is held back by an effect that is in doubt, never merely because a guard in
/// front of it did not hold.
#[test]
fn a_skipped_entry_does_not_block_its_successor() -> TestResult {
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut chained = Bot::builder("skipped-then-run")
        .observe(Dial {
            value: Rc::new(Cell::new(1)),
        })
        .on(
            |_observed: &u32| false,
            Noted(Rc::new(RefCell::new(Vec::new()))),
        )
        .on(|_observed: &u32| true, Noted(Rc::clone(&log)))
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())?;

    assert_eq!(
        chained.tick()?,
        1,
        "only the entry whose condition held fires"
    );
    assert_eq!(
        *log.borrow(),
        vec![1],
        "the successor of a skipped entry ran with the observed value"
    );
    assert!(
        chained.pending().is_empty(),
        "a skipped entry leaves nothing held: {:?}",
        chained.pending().len()
    );
    Ok(())
}

/// An abandoned entry is a barrier to its successors.
///
/// The long half of the counterexample, stated as the review states it: one
/// chain — reserve a draft, then send it — under a one-attempt policy ([`one_attempt`]).
/// The first action definitely fails, so the entry is abandoned and the first
/// tick reports the action's own typed error. The source then holds still.
///
/// Before the repair, the second tick walked past the abandoned entry as though
/// it had succeeded and ran the send: the chain executed a command whose
/// prerequisite had been given up on — the draft that was never reserved, then
/// the send of it. Declaration order is not success dependency, and treating an
/// abandonment as "behind us" is what confused the two.
///
/// The invariant is that an abandonment is a *barrier*, not a decision that the
/// work is behind us. Successors under the default sequential disposition are
/// not attempted while it stands, and they stay reported. Abandonment is not a
/// way to finish a chain, and not a way to finish it quietly: the caller either
/// supplies evidence that the effect did not happen — which revives the entry
/// and its successors with it — or the chain stays reported as unresolved.
#[test]
fn an_abandoned_entry_blocks_its_successors() -> TestResult {
    let attempts = Rc::new(Cell::new(0));
    let refusing = Rc::new(Cell::new(true));
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut chained = Bot::builder("abandoned-then-blocked")
        .with_retry_policy(one_attempt())
        .observe(Dial {
            value: Rc::new(Cell::new(1)),
        })
        .on(
            |_observed: &u32| true,
            Refuses {
                refusing: Rc::clone(&refusing),
                attempts: Rc::clone(&attempts),
            },
        )
        .on(|_observed: &u32| true, Noted(Rc::clone(&log)))
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())?;

    match chained.tick() {
        Ok(fired) => return Err(format!("a definite failure was reported as {fired} fired").into()),
        Err(error) => assert!(
            matches!(error, BotError::DomainError { .. }),
            "expected the action's own error, got {error:?}"
        ),
    }
    assert!(
        log.borrow().is_empty(),
        "the successor is not run in the tick that abandoned its prerequisite"
    );

    // The tick that used to run the successor. It must not: the send would be
    // for a draft that was never reserved.
    match chained.tick() {
        Ok(fired) => {
            return Err(format!(
                "a tick with an abandoned prerequisite reported {fired} fired and ran {:?}",
                log.borrow()
            )
            .into());
        }
        Err(error) => assert!(
            matches!(error, BotError::PendingTransition { .. }),
            "expected the unresolved chain to be reported, got {error:?}"
        ),
    }
    assert!(
        log.borrow().is_empty(),
        "the successor of an abandoned entry is not attempted while it stands abandoned: {:?}",
        log.borrow()
    );

    // Both are still reported — the abandonment and the entry it blocks — so
    // the caller can see which entry is the reason and which is waiting on it.
    let reported: Vec<(usize, usize)> = chained
        .pending()
        .iter()
        .map(|work| (work.id().chain(), work.id().entry()))
        .collect();
    assert_eq!(
        reported,
        vec![(0, 0), (0, 1)],
        "the abandonment and the entry it blocks are both listed: {:?}",
        chained.pending()
    );
    assert!(
        matches!(
            held(&chained).map(|(_, _, _, hold)| hold),
            Some(TransitionHold::Abandoned { .. })
        ),
        "and the first of them is the abandonment, which is the fact that explains the other"
    );

    // Evidence that the effect did not happen is what reopens the chain: the
    // entry is eligible again and its successor with it. This is the only way
    // past a barrier, which is what makes the barrier a decision the caller
    // makes rather than one the substrate makes for them. The action is allowed
    // through this time, so the chain is shown to complete rather than to stay
    // wedged.
    refusing.set(false);
    let blocked = chained
        .pending()
        .into_iter()
        .next()
        .ok_or("the abandoned entry is reported")?;
    chained.resolve_effect(&binding(&blocked)?, EffectEvidence::NotApplied)?;
    assert_eq!(
        chained.tick()?,
        2,
        "the revived entry and the successor it was blocking both run"
    );
    assert_eq!(
        attempts.get(),
        2,
        "the revived entry attempts again, because evidence is a new fact"
    );
    assert_eq!(
        *log.borrow(),
        vec![1],
        "and the successor runs, because its prerequisite is no longer abandoned"
    );
    assert!(
        chained.pending().is_empty(),
        "the chain is finished: {:?}",
        chained.pending()
    );
    Ok(())
}

/// An action that refuses a fixed number of attempts, then succeeds.
///
/// Recording the value it ran against is the whole point here: the defect is an
/// entry executed against a payload that did not come from the transition it
/// belongs to, and the only way to see that is to look at what it received.
/// Refusing rather than holding keeps the transition open across the movement
/// without the effect being indeterminate, which is the shape the review's
/// counterexample has.
struct Grudging {
    /// How many more attempts still refuse.
    refusals: Rc<Cell<usize>>,
    /// The value each attempt actually ran with, in the order it ran.
    seen: Rc<RefCell<Vec<u32>>>,
}

impl Execute for Grudging {
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
        let left = self.refusals.get();
        if left > 0 {
            self.refusals.set(left.saturating_sub(1));
            // `NotDelivered` and not `Refused`: this fixture is a *transient*
            // refusal that the chain is expected to retry while it has budget,
            // and `Refused` classifies as `RetryClass::Never`, which abandons
            // the entry on the first failure.
            let refusal = Err(undelivered("test::grudging", "refused"));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "execute_action: returning an error to the caller");
            return refusal;
        }
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::grudging"
    }
}

/// An open transition is bound to the payload it was opened under.
///
/// The counterexample this closes, stated as the review states it: the source
/// reports request A, the first entry succeeds against A and the second
/// definitely fails against A, so the transition stays open with the second
/// entry still owed an attempt. The next poll returns request B.
///
/// Before the repair the transition was retained — correctly, the work was
/// still outstanding — but the walk and the driver both read the *newest*
/// observed value, so the second entry's retry ran against B while the first
/// entry had only ever run against A. The chain acknowledged A and then
/// executed a B effect: two commands' effects in one transition, with nothing
/// recording that they came from different inputs.
///
/// The invariant is that an open transition is bound to the payload it was
/// opened (or resumed) under. Its entries are evaluated and executed only
/// against that payload, and a newer observation is admitted only when the
/// transition has nothing open. The movement is not lost by waiting: it is
/// admitted on the tick after the outstanding work resolves, and the whole
/// chain — including the entry that had already succeeded under A — runs
/// against it.
#[test]
fn an_open_transition_is_bound_to_the_payload_it_was_opened_under() -> TestResult {
    let value = Rc::new(Cell::new(1));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut bot = Bot::builder("bound-payload")
        .observe(Dial {
            value: Rc::clone(&value),
        })
        .on(|_observed: &u32| true, Noted(Rc::clone(&seen)))
        .on(
            |_observed: &u32| true,
            Grudging {
                refusals: Rc::new(Cell::new(1)),
                seen: Rc::clone(&seen),
            },
        )
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())?;

    // Request A: the first entry runs against it, the second refuses against it,
    // and the transition stays open with the second entry still owed an attempt.
    match bot.tick() {
        Ok(fired) => return Err(format!("a refusal was reported as {fired} fired").into()),
        Err(error) => assert!(
            matches!(error, BotError::DomainError { .. }),
            "expected the action's own error, got {error:?}"
        ),
    }
    assert_eq!(*seen.borrow(), vec![1, 1], "both entries ran against A");

    // Request B arrives while that work is outstanding.
    value.set(2);

    // The retry runs against A, because A is what the transition is bound to.
    // It succeeds, so the transition has nothing open afterwards.
    assert_eq!(bot.tick()?, 1, "the outstanding entry's retry runs");
    assert_eq!(
        *seen.borrow(),
        vec![1, 1, 1],
        "the retry of the second entry ran against A, not B: an open transition is bound to \
         the payload it was opened under"
    );

    // B was not dropped for being unadmitted: it is admitted now that nothing is
    // open, and the whole chain runs against it.
    assert_eq!(bot.tick()?, 2, "the movement to B runs the chain against B");
    assert_eq!(
        *seen.borrow(),
        vec![1, 1, 1, 2, 2],
        "the first entry runs against B too, which is the half the defect skipped: it had \
         acknowledged A, and a B effect must not be combined with that acknowledgment"
    );
    assert_eq!(bot.tick()?, 0, "and the chain is settled");
    Ok(())
}
