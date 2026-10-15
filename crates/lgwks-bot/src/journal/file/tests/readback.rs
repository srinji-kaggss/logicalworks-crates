//! What a reopened or replayed journal reads back, and the outcomes it attests.

use super::*;

#[test]
fn a_batch_climbs_a_key_the_disk_already_knows() -> TestResult {
    let (path, _guard) = subject("batch-committed")?;
    let key = key()?;
    let mut journal = FileJournal::open(&path)?;
    // One rung committed by ordinary appends.
    journal.compare_and_append(journal.tail(), &EffectEvent::IntentAdmitted { key })?;

    // A batch that continues that same key's climb: the staged kinds must
    // follow the committed ladder, not be shadowed by it.
    let acks = journal.compare_and_append_all(&[
        EffectEvent::DispatchPrepared { key },
        EffectEvent::OutcomeObserved {
            key,
            evidence: EffectEvidence::Applied,
        },
    ])?;
    assert_eq!(acks.len(), 2, "both rungs acknowledged");
    assert_eq!(
        journal.recover().status(key),
        Some(AttemptStatus::Applied),
        "the batch continued the committed ladder"
    );
    Ok(())
}

#[test]
fn a_replayed_journal_is_the_journal_that_was_written() -> TestResult {
    let (path, _guard) = subject("replay")?;
    let key = key()?;
    {
        let mut journal = FileJournal::open(&path)?;
        journal.compare_and_append(journal.tail(), &EffectEvent::IntentAdmitted { key })?;
        journal.compare_and_append(journal.tail(), &EffectEvent::DispatchPrepared { key })?;
    }
    let reopened = FileJournal::open(&path)?;
    assert_eq!(reopened.committed()?.len(), 2);
    assert!(!reopened.torn_tail_repaired());
    assert_eq!(
        reopened.recover().status(key),
        Some(AttemptStatus::OutcomeUnknown)
    );
    assert_eq!(
        reopened.durability(),
        DurabilityPromise::ProcessCrash,
        "the file-backed promise is the one a kill can check"
    );
    Ok(())
}

#[test]
fn confirm_outcome_attests_only_its_own_committed_outcome() -> TestResult {
    let (path, _guard) = subject("confirm")?;
    let key = key()?;
    let mut journal = FileJournal::open(&path)?;
    journal.compare_and_append(journal.tail(), &EffectEvent::IntentAdmitted { key })?;
    journal.compare_and_append(journal.tail(), &EffectEvent::DispatchPrepared { key })?;
    let ack = journal.compare_and_append(
        journal.tail(),
        &EffectEvent::OutcomeObserved {
            key,
            evidence: EffectEvidence::Applied,
        },
    )?;

    // The honest receipt: the same position, the same fact, a grade this
    // journal can promise.
    journal.confirm_outcome(
        key,
        EffectEvidence::Applied,
        ack.position(),
        DurabilityPromise::ProcessCrash,
    )?;

    // The lying receipt: the right position, the wrong fact.
    let wrong_evidence = journal.confirm_outcome(
        key,
        EffectEvidence::NotApplied,
        ack.position(),
        DurabilityPromise::ProcessCrash,
    );
    assert!(
        matches!(wrong_evidence, Err(JournalError::ReceiptMismatch { .. })),
        "a receipt for an outcome this journal did not commit must be refused"
    );

    // The grade this journal cannot promise.
    let power_loss = journal.confirm_outcome(
        key,
        EffectEvidence::Applied,
        ack.position(),
        DurabilityPromise::PowerLoss,
    );
    assert!(
        matches!(
            power_loss,
            Err(JournalError::ReceiptUnavailable {
                required: DurabilityPromise::PowerLoss
            })
        ),
        "a file-backed journal must not claim power-loss durability"
    );
    Ok(())
}

#[test]
fn a_reopened_journal_answers_the_latest_outcome_by_key() -> TestResult {
    let (path, _guard) = subject("outcome-index")?;
    let key = key()?;
    let mut journal = FileJournal::open(&path)?;
    journal.compare_and_append(journal.tail(), &EffectEvent::IntentAdmitted { key })?;
    journal.compare_and_append(journal.tail(), &EffectEvent::DispatchPrepared { key })?;
    let outcome = EffectEvent::OutcomeObserved {
        key,
        evidence: EffectEvidence::Applied,
    };
    let ack = journal.compare_and_append(journal.tail(), &outcome)?;
    assert_eq!(
        journal.outcome_at(key)?,
        Some((ack.position(), EffectEvidence::Applied)),
        "the live index answers the committed outcome without scanning history"
    );
    drop(journal);
    let reopened = FileJournal::open(&path)?;
    assert_eq!(
        reopened.outcome_at(key)?,
        Some((ack.position(), EffectEvidence::Applied)),
        "the replay rebuilds the outcome index, so a restart keeps it"
    );
    assert_eq!(
        reopened
            .committed_entry(ack.position())?
            .map(|entry| *entry.event()),
        Some(outcome),
        "the acknowledged position still holds the exact outcome"
    );
    Ok(())
}
