//! Settlements name the generation and the attempt they settle, and no other.

use super::*;

/// A settlement is accepted only for the binding it names.
///
/// The counterexample this closes, stated as the review states it: the entry at
/// `(0, 0)` is indeterminate under one binding, and the caller reads
/// `pending()`. Its report is delayed. The entry is settled, the transition
/// resolves, the source moves, and the same slot is held again — under a second
/// binding. The delayed report for the first binding then arrives.
///
/// Before the repair it was accepted, because `resolve_effect` named only
/// `(chain, entry)` and the ledger only ever holds one transition per chain, so
/// a slot is reused by every generation over it and a `WorkId` alone cannot tell
/// them apart. `Applied` acknowledged an effect at a generation nobody asked
/// about; `NotApplied`, worse, made an attempt the caller never saw eligible for
/// a retry. The action digest — the binding of the payload and the generation
/// the attempt ran under — is the part of the identity that survives the reuse,
/// and it is what [`EffectKey`] carries and what this binds.
///
/// Three more things are asserted here, because they are the same invariant read
/// from the other side: a settlement repeated for its own binding is a no-op
/// that succeeds, a settlement *contradicting* its own binding's is refused,
/// and neither refusal is `NoSuchWork` — that variant has to keep meaning "there
/// is no held effect here", or a caller reading it for a stale report is being
/// told the wrong thing about work that is still held.
#[test]
fn a_settlement_names_the_binding_it_settles_and_no_other() -> TestResult {
    let value = Rc::new(Cell::new(1));
    let uncertain = Rc::new(Cell::new(true));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut bot = doubtful_bot("generations", &value, &uncertain, &seen)?;

    // Revision 1, held: the attempt may be live, so nothing settles it on its
    // own and the tick says so rather than guessing.
    match bot.tick() {
        Ok(fired) => {
            return Err(format!("an indeterminate effect was reported as {fired} fired").into());
        }
        Err(error) => assert!(
            matches!(error, BotError::EffectIndeterminate { .. }),
            "expected the action's own error, got {error:?}"
        ),
    }
    let first = bot
        .pending()
        .first()
        .cloned()
        .ok_or("the held entry is reported")?;
    let first_binding = binding(&first)?;
    assert!(
        matches!(
            first.hold(),
            TransitionHold::OutcomeUnknown { attempts: 1, .. }
        ),
        "held as an unknown outcome, not as a decision: {:?}",
        first.hold()
    );

    // The caller's own settlement for the first binding, naming the first
    // binding. Accepted, and the generation it settles is now decided, so the
    // transition has nothing open and is dropped by the next tick.
    bot.resolve_effect(&first_binding, EffectEvidence::Applied)?;
    assert_eq!(bot.tick()?, 0, "an acknowledged effect is not replayed");
    assert!(
        bot.pending().is_empty(),
        "the acknowledged generation is resolved: {:?}",
        bot.pending()
    );

    // The source moves and the same slot is held again — under a second
    // binding. The `WorkId` is identical to the one above; only the binding
    // differs, which is the whole point.
    uncertain.set(true);
    value.set(2);
    match bot.tick() {
        Ok(fired) => {
            return Err(format!("an indeterminate effect was reported as {fired} fired").into());
        }
        Err(error) => assert!(
            matches!(error, BotError::EffectIndeterminate { .. }),
            "expected the action's own error, got {error:?}"
        ),
    }
    let second = bot
        .pending()
        .first()
        .cloned()
        .ok_or("the held entry is reported again")?;
    let second_binding = binding(&second)?;
    assert_eq!(second.id(), first.id(), "the slot is the same one");
    assert_ne!(
        second_binding, first_binding,
        "the binding it belongs to is not, which is the fact identity loses"
    );
    assert_eq!(
        *seen.borrow(),
        vec![1, 2],
        "each attempt ran against the value its own transition was opened under"
    );

    // The delayed report for the first binding arrives now. It is the same
    // report the caller read above, held across a tick and a movement — a move
    // rather than a clone, because that is what the caller has: the one report
    // it was given.
    //
    // The refusal is asserted as the exact variant and not merely as "not
    // `NoSuchWork`". `EvidenceSuperseded` naming both bindings is the invariant:
    // the caller has to be able to tell that its report was about a binding that
    // is gone, which binding it named, and which one stands there now — a
    // generic refusal would leave it guessing whether to retry the report or
    // re-read the work.
    let delayed = first_binding;
    // Both evidence kinds are refused by the same check, so they are refused by
    // the same closure: a helper written for `Applied` alone would let
    // `NotApplied` pass for a reason nobody checked.
    let refuse_as_superseded = |bot: &mut Bot, evidence: EffectEvidence| -> TestResult {
        match bot.resolve_effect(&delayed, evidence) {
            Ok(()) => Err(format!(
                "{evidence:?} for the first binding was accepted against the second"
            )
            .into()),
            Err(error) => {
                assert!(
                    matches!(
                        error,
                        BotError::EvidenceSuperseded { work, named, current }
                            if work == second.id()
                                && named == delayed.digest()
                                && current == second_binding.digest()
                    ),
                    "expected the named binding and the live one to be reported, got {error:?}"
                );
                assert_eq!(
                    held(bot).map(|(chain, entry, key, _)| (chain, entry, key)),
                    Some((0, 0, Some(second_binding))),
                    "the refusal left the held binding exactly as it was: {:?}",
                    bot.pending()
                );
                Ok(())
            }
        }
    };
    refuse_as_superseded(&mut bot, EffectEvidence::Applied)?;

    // `NotApplied` is the sharper half: accepting it would make an attempt the
    // caller was never asked about eligible to run again. It is refused by the
    // same variant — the reason is the binding, not the evidence.
    refuse_as_superseded(&mut bot, EffectEvidence::NotApplied)?;
    assert_eq!(
        held(&bot).map(|(chain, entry, key, _)| (chain, entry, key)),
        Some((0, 0, Some(second_binding))),
        "the entry is still held and still refused, not restarted: {:?}",
        bot.pending()
    );

    // The current binding, settled with the evidence that is actually about it:
    // accepted.
    bot.resolve_effect(&second_binding, EffectEvidence::Applied)?;

    // The same identity and the same evidence again is a duplicate report, not
    // a contradiction and not an error: a caller whose first delivery was
    // ambiguous has to be able to repeat it.
    bot.resolve_effect(&second_binding, EffectEvidence::Applied)?;

    // Evidence contradicting what this generation was settled with is refused,
    // and the settlement it contradicts stands. The refusal names both pieces of
    // evidence, because "your report was refused" without saying which way round
    // leaves the caller unable to tell whether it misread its own observation.
    match bot.resolve_effect(&second_binding, EffectEvidence::NotApplied) {
        Ok(()) => {
            return Err(
                "evidence contradicting the generation's own settlement was accepted".into(),
            );
        }
        Err(error) => assert!(
            matches!(
                error,
                BotError::EvidenceContradicted {
                    settled: EffectEvidence::Applied,
                    submitted: EffectEvidence::NotApplied,
                    ..
                }
            ),
            "expected the recorded and the refused evidence to be reported, got {error:?}"
        ),
    }
    assert!(
        bot.pending().is_empty(),
        "the acknowledged generation is still acknowledged: {:?}",
        bot.pending()
    );
    assert_eq!(
        bot.tick()?,
        0,
        "an acknowledged effect is never replayed, whatever was said about it afterwards"
    );
    assert_eq!(*seen.borrow(), vec![1, 2], "and no attempt was run for it");
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
