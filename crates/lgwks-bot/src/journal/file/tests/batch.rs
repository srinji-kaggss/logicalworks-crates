//! A batched ladder: four acknowledgments from one sync, and a stale handle refused even for an empty batch.

use super::*;

#[test]
fn a_batched_ladder_is_four_acknowledgments_from_one_sync() -> TestResult {
    let (path, _guard) = subject("batch")?;
    let key = key()?;
    let verdict = crate::journal::Verification::new(
        crate::effect::Id128::from_hex(&"42".repeat(16))?,
        1,
        lgwks_std::hash::blake3(b"postcondition observed"),
        crate::journal::VerificationResult::Satisfied,
    );
    let mut journal = FileJournal::open(&path)?;
    let ladder = [
        EffectEvent::IntentAdmitted { key },
        EffectEvent::DispatchPrepared { key },
        EffectEvent::OutcomeObserved {
            key,
            evidence: EffectEvidence::Applied,
        },
        EffectEvent::Verified {
            key,
            verification: verdict,
        },
    ];
    let acks = journal.compare_and_append_all(&ladder)?;
    assert_eq!(acks.len(), 4, "one acknowledgment per rung");
    assert!(
        acks.iter()
            .all(|ack| ack.promise() == DurabilityPromise::ProcessCrash),
        "every batched acknowledgment carries the same earned promise"
    );

    // The file carries exactly the batch, and a fresh controller reads it.
    drop(journal);
    let reopened = FileJournal::open(&path)?;
    assert_eq!(reopened.committed()?.len(), 4);
    assert_eq!(
        reopened.recover().status(key),
        Some(AttemptStatus::Verified),
        "the batch folded to the ladder's end state"
    );
    drop(reopened);

    // All-or-nothing: a batch whose second rung violates the ladder is
    // refused whole, and nothing of it reaches the file. The fence is
    // exclusive, so the stale handle is dropped before the fresh one
    // opens.
    let mut journal = FileJournal::open(&path)?;
    let before = journal.committed()?.len();
    let refused = journal.compare_and_append_all(&[
        EffectEvent::IntentAdmitted { key: key2()? },
        EffectEvent::IntentAdmitted { key: key2()? },
    ]);
    match refused {
        Err(JournalError::OutOfOrder { .. }) => {}
        Err(other) => {
            return Err(format!("expected an out-of-order refusal, got {other}").into());
        }
        Ok(acks) => {
            let _ = acks;
            return Err("a double admission in one batch must be refused whole".into());
        }
    }
    assert_eq!(
        journal.committed()?.len(),
        before,
        "a refused batch commits nothing"
    );
    drop(journal);
    let mut reopened = FileJournal::open(&path)?;
    assert_eq!(
        reopened.committed()?.len(),
        before,
        "and the file carries none of it"
    );

    // An empty batch is still behind the fence: a handle whose file
    // moved under it answers with the staleness refusal, not with an
    // empty success. `reopened` was opened before the foreign write, so
    // its view is the stale one.
    {
        use std::io::Write as _;
        let mut other = std::fs::OpenOptions::new().append(true).open(&path)?;
        other.write_all(&[0x00, 0x00, 0x00])?;
        other.sync_all()?;
    }
    let empty = reopened.compare_and_append_all(&[]);
    assert!(
        matches!(empty, Err(JournalError::Storage(_))),
        "a stale handle must not answer an empty batch with success"
    );
    Ok(())
}
