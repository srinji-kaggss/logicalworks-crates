//! The ledger's repairs, continued: payload binding and attempt identity.

use super::*;
use super::ledger_tests::{Noted, binding};

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

/// Tick, expecting the held effect's own error rather than a decision.
///
/// A held effect is one the substrate cannot decide about on its own, so the
/// tick reports the action's error and leaves the entry standing.
fn tick_expecting_held(bot: &mut Bot) -> TestResult {
    match bot.tick() {
        Ok(fired) => Err(format!("an indeterminate effect was reported as {fired} fired").into()),
        Err(error) => {
            assert!(
                matches!(error, BotError::EffectIndeterminate { .. }),
                "expected the action's own error, got {error:?}"
            );
            Ok(())
        }
    }
}

/// A settlement is accepted only for the attempt it names.
///
/// The counterexample this closes: a chain retries *within* one generation, so
/// the revision that [`a_settlement_names_the_generation_it_settles_and_no_other`]
/// binds does not separate attempt 1 from attempt 2. The entry at `(0, 0)` is
/// held indeterminate at revision 1 and the caller reads `pending()`. It
/// establishes that the attempt did not happen, which is what makes the entry
/// eligible again, and the next tick begins attempt 2 at the same address under
/// the same generation. The caller's report is then delivered a second time —
/// which the ledger has to tolerate, because a caller that never saw its first
/// delivery acknowledged must be able to send it again.
///
/// Before the repair the repeat was accepted, because the entry was held and
/// held is what settlement looked for. `NotApplied` then overwrote attempt 2
/// with "definitely did not happen" on the strength of a statement about
/// attempt 1, and attempt 3 ran against an effect that may have been live.
///
/// The attempt ordinal is the part of the binding that survives a retry, and it
/// is what this binds.
#[test]
fn a_settlement_names_the_attempt_it_settles_and_no_other() -> TestResult {
    let value = Rc::new(Cell::new(1));
    let uncertain = Rc::new(Cell::new(true));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut bot = doubtful_bot("attempts", &value, &uncertain, &seen)?;

    // Attempt 1, first generation: held, because the effect may be live.
    tick_expecting_held(&mut bot)?;
    let held = bot.pending();
    let first = held.first().ok_or("the held entry is reported")?;
    let first_id = first.id();
    let first_binding = binding(first)?;
    assert_eq!(first_binding.attempt().get(), 1, "the attempt is the first");
    assert!(
        matches!(
            first.hold(),
            TransitionHold::OutcomeUnknown { attempts: 1, .. }
        ),
        "held as an unknown outcome: {:?}",
        first.hold()
    );

    // The caller establishes that attempt 1 did not happen. The entry is
    // eligible again, and the source has not moved: the retry is at the same
    // address, under the same generation.
    bot.resolve_effect(&first_binding, EffectEvidence::NotApplied)?;
    tick_expecting_held(&mut bot)?;
    let held_again = bot.pending();
    let second = held_again.first().ok_or("the retry is reported")?;
    let second_binding = binding(second)?;
    assert_eq!(
        second_binding.digest(),
        first_binding.digest(),
        "still the same generation"
    );
    assert_eq!(second.id(), first_id, "and the same address");
    assert_eq!(
        second_binding.attempt().get(),
        2,
        "but a second attempt, not the first again"
    );
    assert!(
        matches!(
            second.hold(),
            TransitionHold::OutcomeUnknown { attempts: 2, .. }
        ),
        "attempt 2 is what is held: {:?}",
        second.hold()
    );

    // The attempt-1 report arrives a second time. It is about work that is
    // over, and accepting it is what makes an attempt eligible to run again.
    match bot.resolve_effect(&first_binding, EffectEvidence::NotApplied) {
        Err(BotError::EvidenceStaleAttempt {
            reported,
            outstanding,
            ..
        }) => assert_eq!(
            (reported.get(), outstanding.get()),
            (1, 2),
            "the refusal names both attempts, so the caller knows what to report against"
        ),
        other => {
            return Err(format!(
                "a report about attempt 1 was answered with {other:?} while attempt 2 was held"
            )
            .into());
        }
    }
    let held_after = bot.pending();
    let after = held_after.first().ok_or("attempt 2 is still held")?;
    assert_eq!(
        after.hold(),
        second.hold(),
        "attempt 2 was not moved by a report about attempt 1"
    );

    // The report about the attempt that *is* outstanding is accepted, and a
    // repeat of that one is still idempotent: the refusal above must not take
    // away the tolerance it was carved out of.
    bot.resolve_effect(&second_binding, EffectEvidence::NotApplied)?;
    bot.resolve_effect(&second_binding, EffectEvidence::NotApplied)?;

    // And the chain is not wedged: attempt 3 runs and the transition settles.
    uncertain.set(false);
    assert_eq!(bot.tick()?, 1, "the attempt after the refused report runs");
    assert!(bot.pending().is_empty(), "and the transition settles");
    Ok(())
}
