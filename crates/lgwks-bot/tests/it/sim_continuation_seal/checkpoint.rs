//! The checkpoint: both carry bounds, one record per action, and what every continuation admits, refuses and carries unchanged.

use super::*;

// ── The checkpoint ────────────────────────────────────────────────────────

/// A carry at the declared bound continues with every attempt still unknown, and
/// one more is refused before a byte moves.
fn carry_bound(sim: &mut sim::Sim) -> TestResult {
    let dir = sim.scratch("carry-bound")?;
    let actions = draw(sim, 1, 4);
    for carried in [
        MAX_CHECKPOINT_UNRESOLVED,
        MAX_CHECKPOINT_UNRESOLVED.saturating_add(1),
    ] {
        let mut journal = quiet(&dir.join(format!("carry-{carried}.jrnl")))?;
        let mut oracle = Oracle::default();
        let total = u64::try_from(carried)?;
        let share = total
            .checked_div(actions)
            .ok_or("the seed draws at least one action")?;
        let mut left = total;
        for action in 1..=actions {
            let count = if action == actions { left } else { share };
            write(&mut journal, &mut oracle, action, (1, count), Fate::Unknown)?;
            left = left.saturating_sub(count);
        }
        let before = length(journal.path())?;
        let answer = journal.continue_as_file();
        if carried <= MAX_CHECKPOINT_UNRESOLVED {
            let successor = answer?.ok_or("a carry at the bound continues")?;
            assert_eq!(
                successor.recover().uncertain().len(),
                carried,
                "every attempt at the bound is carried, still unknown"
            );
            assert_fold_agrees(&successor, &oracle, 1)?;
        } else {
            assert!(
                matches!(
                    answer,
                    Err(JournalError::CapacityExceeded {
                        resource: JournalLimitKind::CheckpointUnresolved,
                        ..
                    })
                ),
                "a carry past the bound is refused, never truncated"
            );
            assert_unwritten(&journal, before)?;
        }
    }
    sim.record("carry-bound-exact");
    sim.trace.record_number("carry-bound-actions", actions);
    Ok(())
}

/// A checkpoint folding the declared number of actions continues, and one more
/// action is refused before a byte moves.
fn action_bound(sim: &mut sim::Sim) -> TestResult {
    let dir = sim.scratch("action-bound")?;
    for folded in [
        MAX_CHECKPOINT_SETTLED,
        MAX_CHECKPOINT_SETTLED.saturating_add(1),
    ] {
        let mut journal = quiet(&dir.join(format!("actions-{folded}.jrnl")))?;
        let mut oracle = Oracle::default();
        let actions = u64::try_from(folded)?;
        let mut batch = Vec::new();
        for action in 1..=actions {
            let fate = settled(sim);
            batch.extend(ladder(fate, action, 1)?);
            oracle.dispatch(action, 1, fate);
            if batch.len() >= 64 || action == actions {
                journal.compare_and_append_all(&batch)?;
                batch.clear();
            }
        }
        let before = length(journal.path())?;
        let answer = journal.continue_as_file();
        if folded <= MAX_CHECKPOINT_SETTLED {
            let successor = answer?.ok_or("a fold at the bound continues")?;
            let checkpoint = successor
                .checkpoint()
                .ok_or("a successor is opened from a checkpoint")?;
            assert_eq!(checkpoint.settled().len(), folded, "one record per action");
            assert_fold_agrees(&successor, &oracle, 1)?;
        } else {
            assert!(
                matches!(
                    answer,
                    Err(JournalError::CapacityExceeded {
                        resource: JournalLimitKind::CheckpointActions,
                        ..
                    })
                ),
                "a fold past the bound is refused, never truncated"
            );
            assert_unwritten(&journal, before)?;
        }
    }
    sim.record("action-bound-exact");
    Ok(())
}

/// A successor, the oracle that wrote its history, and the latest attempt number
/// of every action it folded.
type Folded = (FileJournal, Oracle, Vec<(u64, u64)>);

/// A successor over a seeded history of settled attempts.
fn folded(sim: &mut sim::Sim, tag: &str) -> Result<Folded, Box<dyn Error>> {
    let dir = sim.scratch(tag)?;
    let mut journal = quiet(&dir.join("run.jrnl"))?;
    let mut oracle = Oracle::default();
    let mut latest = Vec::new();
    let actions = draw(sim, 1, 6);
    for action in 1..=actions {
        let attempts = draw(sim, 1, 6);
        write_with(&mut journal, &mut oracle, action, (1, attempts), || {
            settled(sim)
        })?;
        latest.push((action, attempts));
    }
    let successor = seal(&mut journal)?;
    Ok((successor, oracle, latest))
}

/// The checkpoint holds one record per action, naming the latest attempt and the
/// status the oracle recorded for it, whatever the history behind it.
fn one_record_per_action(sim: &mut sim::Sim) -> TestResult {
    let (successor, oracle, latest) = folded(sim, "per-action")?;
    let checkpoint = successor
        .checkpoint()
        .ok_or("a successor is opened from a checkpoint")?;
    assert_eq!(
        checkpoint.settled().len(),
        latest.len(),
        "one folded record per action"
    );
    assert!(
        checkpoint.unresolved().is_empty(),
        "a fully settled history carries nothing unresolved"
    );
    for &(action, last) in &latest {
        let held = checkpoint
            .settled_for(action_of(action)?)
            .ok_or("every action the predecessor walked is folded")?;
        let recorded = oracle
            .latest_of(action)
            .ok_or("the oracle holds every action it dispatched")?;
        assert_eq!(
            held.attempt().get(),
            last,
            "the fold keeps the latest attempt"
        );
        assert_eq!(
            held.status(),
            Some(status_of(recorded.fate)),
            "with the status the oracle recorded for it"
        );
    }
    sim.record("one-record-per-action");
    sim.trace.record_number("per-action-actions", latest.len());
    Ok(())
}

/// Every attempt a folded action already walked is refused after a continuation,
/// and the refusals move no byte.
fn older_refused(sim: &mut sim::Sim) -> TestResult {
    let (mut successor, oracle, latest) = folded(sim, "older")?;
    let before = length(successor.path())?;
    for &(action, last) in &latest {
        for number in 1..=last {
            let again = successor.compare_and_append(successor.tail(), &intent(action, number)?);
            assert!(
                again.is_err(),
                "attempt {number} of action {action} was admitted again after a continuation"
            );
        }
    }
    assert_eq!(
        length(successor.path())?,
        before,
        "every refusal left the successor's bytes alone"
    );
    assert_fold_agrees(&successor, &oracle, 1)?;
    sim.record("walked-attempts-refused");
    sim.trace.record_number("older-actions", latest.len());
    Ok(())
}

/// A newer attempt of every folded action is admitted in the successor and folds
/// to the status its ladder reached.
fn newer_admitted(sim: &mut sim::Sim) -> TestResult {
    let (mut successor, mut oracle, latest) = folded(sim, "newer")?;
    for &(action, last) in &latest {
        let next = last.saturating_add(draw(sim, 1, 3));
        let fate = drawn(sim);
        successor.compare_and_append_all(&ladder(fate, action, next)?)?;
        oracle.dispatch(action, next, fate);
        assert_eq!(
            successor.recover().status(key_of(action, next)?),
            Some(status_of(fate)),
            "attempt {next} of action {action} is admitted and folds to its fate"
        );
    }
    assert_fold_agrees(&successor, &oracle, 1)?;
    sim.record("newer-attempts-admitted");
    sim.trace.record_number("newer-actions", latest.len());
    Ok(())
}

/// A failed verification's digest crosses every continuation unchanged.
fn verification_crosses(sim: &mut sim::Sim) -> TestResult {
    let dir = sim.scratch("verdict")?;
    let mut journal = quiet(&dir.join("run.jrnl"))?;
    let mut oracle = Oracle::default();
    write(
        &mut journal,
        &mut oracle,
        1,
        (1, 1),
        Fate::VerificationFailed,
    )?;
    let expected = verdict(2, false)?;
    let hops = draw(sim, 1, 5);
    let mut next = 1_u64;
    for _ in 0..hops {
        let filler = draw(sim, 1, 4);
        write(&mut journal, &mut oracle, 2, (next, filler), Fate::Applied)?;
        next = next.saturating_add(filler);
        journal = seal(&mut journal)?;
        let held = journal
            .checkpoint()
            .ok_or("a successor is opened from a checkpoint")?
            .settled_for(action_of(1)?)
            .ok_or("the verified action is folded")?;
        assert_eq!(
            held.verification(),
            Some(expected),
            "the failed verification is carried byte for byte"
        );
        assert_eq!(
            journal.recover().status(key_of(1, 1)?),
            Some(AttemptStatus::VerificationFailed),
            "and the attempt it judged still folds as a failed verification"
        );
    }
    assert_fold_agrees(&journal, &oracle, 1)?;
    sim.record("verification-crosses-every-boundary");
    sim.trace.record_number("verdict-hops", hops);
    Ok(())
}

/// An attempt settled in a later generation stays settled through every
/// continuation after it, and is never re-admitted.
fn settled_later(sim: &mut sim::Sim) -> TestResult {
    let dir = sim.scratch("later")?;
    let mut journal = quiet(&dir.join("run.jrnl"))?;
    let mut oracle = Oracle::default();
    let stranded = draw(sim, 1, 10);
    write(&mut journal, &mut oracle, 1, (1, stranded), Fate::Unknown)?;
    let chosen = draw(sim, 1, u32::try_from(stranded)?);
    journal = seal(&mut journal)?;
    let key = key_of(1, chosen)?;
    journal.compare_and_append_all(&[
        EffectEvent::OutcomeObserved {
            key,
            evidence: EffectEvidence::Applied,
        },
        EffectEvent::Verified {
            key,
            verification: verdict(1, true)?,
        },
    ])?;
    oracle.settle(1, chosen);
    let hops = draw(sim, 1, 3);
    for _ in 0..hops {
        journal = seal(&mut journal)?;
        assert_eq!(
            journal.recover().status(key),
            Some(AttemptStatus::Verified),
            "an attempt settled in an earlier successor stays settled"
        );
        let again = journal.compare_and_append(journal.tail(), &intent(1, chosen)?);
        assert!(again.is_err(), "and is never admitted a second time");
        assert_eq!(
            journal.recover().uncertain().len(),
            oracle.unknown(),
            "the rest of the carry is still unknown, no more and no fewer"
        );
    }
    assert_fold_agrees(&journal, &oracle, 1)?;
    sim.record("settled-later-stays-settled");
    sim.trace.record_number("later-stranded", stranded);
    sim.trace.record_number("later-hops", hops);
    Ok(())
}

/// A not-applied attempt is carried as not applied, and the retry it invites is a
/// new attempt the successor admits.
fn not_applied_retry(sim: &mut sim::Sim) -> TestResult {
    let dir = sim.scratch("retry")?;
    let mut journal = quiet(&dir.join("run.jrnl"))?;
    let mut oracle = Oracle::default();
    let attempts = draw(sim, 1, 6);
    write(
        &mut journal,
        &mut oracle,
        1,
        (1, attempts),
        Fate::NotApplied,
    )?;
    let hops = draw(sim, 1, 3);
    for _ in 0..hops {
        journal = seal(&mut journal)?;
        assert_eq!(
            journal.recover().status(key_of(1, attempts)?),
            Some(AttemptStatus::NotApplied),
            "a not-applied attempt is carried as not applied"
        );
    }
    let retry = attempts.saturating_add(1);
    journal.compare_and_append_all(&ladder(Fate::Applied, 1, retry)?)?;
    oracle.dispatch(1, retry, Fate::Applied);
    assert_eq!(
        journal.recover().status(key_of(1, retry)?),
        Some(AttemptStatus::Verified),
        "the retry is a new attempt and lands"
    );
    let again = journal.compare_and_append(journal.tail(), &intent(1, attempts)?);
    assert!(again.is_err(), "while the attempt it retried stays refused");
    assert_fold_agrees(&journal, &oracle, 1)?;
    sim.record("not-applied-retried-as-new-attempt");
    sim.trace.record_number("retry-attempts", attempts);
    Ok(())
}

#[test]
fn the_carry_bound_admits_its_size_and_refuses_one_more_unwritten() -> TestResult {
    sim::assert_replays(band(11), carry_bound)
}

#[test]
fn the_action_bound_admits_its_size_and_refuses_one_more_unwritten() -> TestResult {
    sim::assert_replays(band(12), action_bound)
}

#[test]
fn a_checkpoint_holds_one_record_per_action_whatever_the_history() -> TestResult {
    sim::assert_replays(band(13), one_record_per_action)
}

#[test]
fn every_walked_attempt_is_refused_after_a_continuation() -> TestResult {
    sim::assert_replays(band(14), older_refused)
}

#[test]
fn a_newer_attempt_of_every_folded_action_is_admitted() -> TestResult {
    sim::assert_replays(band(15), newer_admitted)
}

#[test]
fn a_failed_verification_crosses_every_continuation_unchanged() -> TestResult {
    sim::assert_replays(band(16), verification_crosses)
}

#[test]
fn an_attempt_settled_in_a_successor_stays_settled_after_the_next() -> TestResult {
    sim::assert_replays(band(17), settled_later)
}

#[test]
fn a_not_applied_attempt_is_carried_and_its_retry_admitted() -> TestResult {
    sim::assert_replays(band(18), not_applied_retry)
}
