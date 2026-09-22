//! The journal half of eval cases E10 and E11.
//!
//! E10 is "lost reply after real external commit": a persistent service commits
//! one non-idempotent operation and the connection dies before the response, so
//! the controller must keep the attempt unknown and must not resend. E11 is
//! "crash at every durable boundary": committed identities recover, nothing is
//! blindly replayed, and a compare-and-append fence stops a competing
//! controller.
//!
//! What these tests exercise is the journal's half of both. The dispatch half —
//! a real receiver, a real restart, a real external resource oracle — is not
//! here, and neither case can be called passing on the strength of this file.
//! `okf/evals.json` keeps both at `not_run` for that reason.
//!
//! Everything below goes through the public API: no `crate::` path reaches
//! inside, so a change that breaks a consumer breaks these first.

use lgwks_bot::effect::{
    ActionDigest, ActionId, AttemptId, EffectKey, EnvironmentEpoch, EnvironmentId, FlowRevision,
    Id128, RunId,
};
use lgwks_bot::journal::{
    AttemptStatus, DurabilityPromise, EffectEvent, EffectEvidence, EffectJournal, JournalEntry,
    JournalError, MemoryJournal, RequiredDurability, Verification, VerificationResult,
};

const RUN: &str = "0102030405060708090a0b0c0d0e0f10";
const ACTION: &str = "1112131415161718191a1b1c1d1e1f20";
const ENV: &str = "2122232425262728292a2b2c2d2e2f30";
const FLOW_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const DIGEST_HEX: &str = "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeffe0e1e2e3e4e5e6e7e8e9eaebecedeeef";
const PREDICATE: &str = "4142434445464748494a4b4c4d4e4f50";

/// The suite refuses a panicking path, so every test propagates instead.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// A key for one attempt at the shared intent.
fn key(attempt: &str, epoch: &str) -> Result<EffectKey, Box<dyn std::error::Error>> {
    Ok(EffectKey::new(
        RunId::from_hex(RUN)?,
        ActionId::from_hex(ACTION)?,
        AttemptId::from_decimal(attempt)?,
        FlowRevision::from_tagged("blake3_256", FLOW_HEX)?,
        ActionDigest::from_tagged("blake3_256", DIGEST_HEX)?,
        EnvironmentId::from_hex(ENV)?,
        EnvironmentEpoch::from_decimal(epoch)?,
    ))
}

/// Append at the journal's own tail, which is what a correct caller does.
fn append(
    journal: &mut MemoryJournal,
    event: EffectEvent,
) -> Result<(), Box<dyn std::error::Error>> {
    let tail = journal.tail();
    journal.compare_and_append(tail, &event)?;
    Ok(())
}

/// Walk one attempt up to the point where the bytes are about to leave.
fn admit_and_prepare(
    journal: &mut MemoryJournal,
    key: EffectKey,
) -> Result<(), Box<dyn std::error::Error>> {
    append(
        journal,
        EffectEvent::IntentAdmitted {
            key,
            required: RequiredDurability::new(DurabilityPromise::Ephemeral),
        },
    )?;
    append(journal, EffectEvent::DispatchPrepared { key })?;
    Ok(())
}

/// What a restarted controller sees: the committed entries, replayed.
fn after_restart(journal: &MemoryJournal) -> Vec<JournalEntry> {
    journal.committed().to_vec()
}

#[test]
fn e10_a_lost_reply_leaves_the_attempt_unknown_rather_than_not_applied() -> TestResult {
    let key = key("1", "1")?;
    let mut journal = MemoryJournal::new();
    admit_and_prepare(&mut journal, key)?;

    // The crash: the controller is gone before any outcome could be recorded.
    // A recovered controller has the committed entries and nothing else.
    let committed = after_restart(&journal);
    let recovered = lgwks_bot::journal::recover(committed.iter().map(JournalEntry::event));

    assert_eq!(recovered.status(key), Some(AttemptStatus::OutcomeUnknown));
    assert_eq!(recovered.uncertain(), vec![key]);
    assert_ne!(
        recovered.status(key),
        Some(AttemptStatus::NotApplied),
        "nothing established that the bytes did not arrive"
    );
    Ok(())
}

#[test]
fn e10_evidence_settles_the_unknown_without_a_second_dispatch() -> TestResult {
    let key = key("1", "1")?;
    let mut journal = MemoryJournal::new();
    admit_and_prepare(&mut journal, key)?;

    let committed = after_restart(&journal);
    let recovered = lgwks_bot::journal::recover(committed.iter().map(JournalEntry::event));
    assert_eq!(recovered.uncertain(), vec![key]);

    // Settling it is an append, not a resend. The original attempt keeps its
    // identity and gains a fact.
    append(
        &mut journal,
        EffectEvent::OutcomeObserved {
            key,
            evidence: EffectEvidence::Applied,
        },
    )?;

    let recovered = journal.recover();
    assert_eq!(recovered.status(key), Some(AttemptStatus::Applied));
    assert!(recovered.uncertain().is_empty());
    Ok(())
}

#[test]
fn e10_an_ephemeral_journal_cannot_be_the_record_behind_a_handoff() -> TestResult {
    let journal = MemoryJournal::new();
    assert_eq!(journal.durability(), DurabilityPromise::Ephemeral);

    match journal.admit_external_handoff() {
        Err(JournalError::PromiseUnmet { required, offered }) => {
            assert_eq!(required, DurabilityPromise::ProcessCrash);
            assert_eq!(offered, DurabilityPromise::Ephemeral);
        }
        Err(other) => return Err(format!("expected a promise refusal, got {other}").into()),
        Ok(granted) => {
            return Err(format!(
                "an ephemeral journal must not admit an external handoff, got {granted}"
            )
            .into());
        }
    }
    Ok(())
}

#[test]
fn e11_a_competing_controller_cannot_dispatch_from_a_stale_tail() -> TestResult {
    // Named `first` and `second` rather than `key`, so neither binding shadows
    // the `key` constructor the second one still has to call.
    let first = key("1", "1")?;
    let second = key("2", "1")?;
    let mut journal = MemoryJournal::new();

    // Two controllers read the same tail before either writes.
    let controller_a = journal.tail();
    let controller_b = journal.tail();

    journal.compare_and_append(
        controller_a,
        &EffectEvent::IntentAdmitted {
            key: first,
            required: RequiredDurability::new(DurabilityPromise::Ephemeral),
        },
    )?;

    let refused = journal.compare_and_append(
        controller_b,
        &EffectEvent::IntentAdmitted {
            key: second,
            required: RequiredDurability::new(DurabilityPromise::Ephemeral),
        },
    );
    match refused {
        Err(JournalError::TailMismatch { expected, actual }) => {
            assert_eq!(expected, controller_b);
            assert_eq!(actual, journal.tail());
        }
        Err(other) => return Err(format!("expected a tail mismatch, got {other}").into()),
        Ok(ack) => {
            return Err(format!(
                "the second writer from a stale reading must be refused, got {ack:?}"
            )
            .into());
        }
    }

    assert_eq!(
        journal.committed().len(),
        1,
        "a refused append must not have changed the journal"
    );
    Ok(())
}

#[test]
fn e11_a_blind_resend_is_not_representable() -> TestResult {
    let key = key("1", "1")?;
    let mut journal = MemoryJournal::new();
    admit_and_prepare(&mut journal, key)?;

    // The controller restarts, sees an unknown outcome, and tries to dispatch
    // the same attempt again. There is no event that expresses it.
    let refused =
        journal.compare_and_append(journal.tail(), &EffectEvent::DispatchPrepared { key });
    assert!(
        matches!(
            refused,
            Err(JournalError::OutOfOrder {
                expected: Some(lgwks_bot::journal::EventKind::OutcomeObserved),
                ..
            })
        ),
        "a second dispatch of the same attempt must be unrepresentable"
    );
    assert_eq!(journal.committed().len(), 2);
    Ok(())
}

#[test]
fn e11_a_real_retry_is_a_new_attempt_and_still_expressible() -> TestResult {
    // The companion to the test above: refusing a blind resend must not make a
    // legitimate retry impossible. A retry is a new attempt, so it is a new key
    // with its own fresh ladder.
    let first = key("1", "1")?;
    let retry = key("2", "1")?;
    assert_ne!(first, retry);

    let mut journal = MemoryJournal::new();
    admit_and_prepare(&mut journal, first)?;
    append(
        &mut journal,
        EffectEvent::OutcomeObserved {
            key: first,
            evidence: EffectEvidence::NotApplied,
        },
    )?;
    admit_and_prepare(&mut journal, retry)?;

    let recovered = journal.recover();
    assert_eq!(recovered.status(first), Some(AttemptStatus::NotApplied));
    assert_eq!(recovered.status(retry), Some(AttemptStatus::OutcomeUnknown));
    assert_eq!(recovered.len(), 2);
    Ok(())
}

#[test]
fn e11_replay_recovers_every_committed_identity_after_a_restart() -> TestResult {
    let settled = key("1", "1")?;
    let in_flight = key("2", "1")?;

    let mut journal = MemoryJournal::new();
    admit_and_prepare(&mut journal, settled)?;
    append(
        &mut journal,
        EffectEvent::OutcomeObserved {
            key: settled,
            evidence: EffectEvidence::Applied,
        },
    )?;
    append(
        &mut journal,
        EffectEvent::Verified {
            key: settled,
            verification: Verification::new(
                Id128::from_hex(PREDICATE)?,
                1,
                lgwks_std::hash::blake3(b"postcondition observed"),
                VerificationResult::Satisfied,
            ),
        },
    )?;
    admit_and_prepare(&mut journal, in_flight)?;

    // The restart: nothing survives but the committed entries.
    let committed = after_restart(&journal);
    let recovered = lgwks_bot::journal::recover(committed.iter().map(JournalEntry::event));

    assert_eq!(recovered.len(), 2);
    assert_eq!(recovered.status(settled), Some(AttemptStatus::Verified));
    assert_eq!(
        recovered.status(in_flight),
        Some(AttemptStatus::OutcomeUnknown)
    );
    assert_eq!(
        recovered.uncertain(),
        vec![in_flight],
        "the settled attempt must not reappear as uncertain"
    );

    // The chain the acks committed to is still re-derivable.
    let rebuilt = MemoryJournal::new();
    assert!(rebuilt.committed().is_empty());
    assert_eq!(journal.verify()?, journal.tail());
    Ok(())
}
