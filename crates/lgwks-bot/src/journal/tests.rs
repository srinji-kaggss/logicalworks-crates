//! Unit tests for `journal`, moved verbatim out of journal.rs.

use super::*;
use crate::effect::{
    ActionDigest, ActionId, AttemptId, EnvironmentEpoch, EnvironmentId, FlowRevision, RunId,
};

const RUN: &str = "0102030405060708090a0b0c0d0e0f10";
const ACTION: &str = "1112131415161718191a1b1c1d1e1f20";
const OTHER_ACTION: &str = "3132333435363738393a3b3c3d3e3f40";
const ENV: &str = "2122232425262728292a2b2c2d2e2f30";
const FLOW_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const DIGEST_HEX: &str = "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeffe0e1e2e3e4e5e6e7e8e9eaebecedeeef";
const PREDICATE: &str = "4142434445464748494a4b4c4d4e4f50";

/// The crate refuses a panicking path anywhere, tests included, so a test
/// that needs a parsed value returns `Result` and propagates. A failure
/// then names the cause instead of a line number in a macro.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// A key for the shared run and action, at the given attempt and epoch.
fn key(attempt: &str, epoch: &str) -> Result<EffectKey, Box<dyn std::error::Error>> {
    build(ACTION, attempt, epoch)
}

/// A key for a different action, so identity comparisons have something to
/// differ from.
fn other_key(attempt: &str, epoch: &str) -> Result<EffectKey, Box<dyn std::error::Error>> {
    build(OTHER_ACTION, attempt, epoch)
}

/// Assemble a key from wire-form parts.
fn build(
    action: &str,
    attempt: &str,
    epoch: &str,
) -> Result<EffectKey, Box<dyn std::error::Error>> {
    let key_run = RunId::from_hex(RUN)?;
    let key_action = ActionId::from_hex(action)?;
    let key_attempt = AttemptId::from_decimal(attempt)?;
    let key_flow_revision = FlowRevision::from_tagged("blake3_256", FLOW_HEX)?;
    let key_digest = ActionDigest::from_tagged("blake3_256", DIGEST_HEX)?;
    let key_environment = EnvironmentId::from_hex(ENV)?;
    let key_epoch = EnvironmentEpoch::from_decimal(epoch)?;
    Ok(
        crate::effect::EffectIdentity::new(key_run, key_environment, key_flow_revision).key(
            key_action,
            key_attempt,
            key_digest,
            key_epoch,
        ),
    )
}

/// A verification record naming a real predicate and a real observation
/// digest.
fn satisfied() -> Result<Verification, Box<dyn std::error::Error>> {
    Ok(Verification::new(
        Id128::from_hex(PREDICATE)?,
        1,
        blake3(b"observations"),
        VerificationResult::Satisfied,
    ))
}

/// Append `event` to `journal` at its own tail, which is what a correct
/// caller does.
fn append(
    journal: &mut MemoryJournal,
    event: EffectEvent,
) -> Result<DurableAck, Box<dyn std::error::Error>> {
    let tail = journal.tail();
    Ok(journal.compare_and_append(tail, &event)?)
}

#[test]
fn memory_journal_refuses_history_beyond_its_declared_limit() -> TestResult {
    let mut journal = MemoryJournal::new();
    for attempt in 1_u64..=100_000 {
        let key = key(&attempt.to_string(), "1")?;
        journal.compare_and_append(journal.tail(), &EffectEvent::IntentAdmitted { key })?;
    }

    let extra_key = key("100001", "1")?;
    let extra = journal.compare_and_append(
        journal.tail(),
        &EffectEvent::IntentAdmitted { key: extra_key },
    );
    assert!(
        matches!(
            &extra,
            Err(JournalError::CapacityExceeded {
                resource: JournalLimitKind::Events,
                limit: 100_000,
                requested: 100_001,
            })
        ),
        "the refusal names both the hard limit and requested size: {extra:?}"
    );
    assert_eq!(
        journal.committed().len(),
        100_000,
        "history must remain complete at the declared limit; extra append was {extra:?}"
    );
    Ok(())
}

/// Walk one attempt to the point where the irreversible boundary is next.
fn admit_and_prepare(
    journal: &mut MemoryJournal,
    key: EffectKey,
) -> Result<(), Box<dyn std::error::Error>> {
    append(journal, EffectEvent::IntentAdmitted { key })?;
    append(journal, EffectEvent::DispatchPrepared { key })?;
    Ok(())
}

/// A journal holding one attempt walked past the boundary to an applied
/// outcome: the state every verification test starts from.
fn applied_journal(key: EffectKey) -> Result<MemoryJournal, Box<dyn std::error::Error>> {
    let mut journal = MemoryJournal::new();
    admit_and_prepare(&mut journal, key)?;
    append(
        &mut journal,
        EffectEvent::OutcomeObserved {
            key,
            evidence: EffectEvidence::Applied,
        },
    )?;
    Ok(journal)
}

#[test]
fn genesis_is_not_a_zero_head() -> TestResult {
    let genesis = JournalPosition::genesis();
    assert_eq!(genesis.sequence(), 0);
    assert_ne!(genesis.head(), blake3(&[]));
    assert_eq!(genesis, JournalPosition::genesis());
    Ok(())
}

#[test]
fn an_append_advances_the_sequence_and_moves_the_head() -> TestResult {
    let key = key("1", "1")?;
    let mut journal = MemoryJournal::new();
    let genesis = journal.tail();
    let ack = append(&mut journal, EffectEvent::IntentAdmitted { key })?;
    assert_eq!(ack.position().sequence(), 1);
    assert_ne!(ack.position().head(), genesis.head());
    assert_eq!(journal.tail(), ack.position());
    Ok(())
}

#[test]
fn the_acknowledgment_names_the_promise_that_was_claimed() -> TestResult {
    let key = key("1", "1")?;
    let mut journal = MemoryJournal::new();
    let ack = append(&mut journal, EffectEvent::IntentAdmitted { key })?;
    assert_eq!(ack.promise(), DurabilityPromise::Ephemeral);
    Ok(())
}

#[test]
fn positioned_readback_resolves_one_exact_sequence_and_head() -> TestResult {
    let key = key("1", "1")?;
    let mut journal = MemoryJournal::new();
    let ack = append(&mut journal, EffectEvent::IntentAdmitted { key })?;
    let entries = EffectJournal::committed_entries(&journal)?;
    let expected = entries.first().copied().ok_or("committed entry missing")?;

    assert_eq!(
        journal.committed_entry(ack.position())?,
        Some(expected),
        "the acknowledged position returns its exact committed entry"
    );
    let wrong_head = JournalPosition {
        sequence: ack.position().sequence(),
        head: blake3(b"not the committed head"),
    };
    assert_eq!(
        journal.committed_entry(wrong_head)?,
        None,
        "a matching sequence cannot authorize a different history head"
    );
    Ok(())
}

#[test]
fn two_events_in_another_order_are_a_different_head() -> TestResult {
    let first = key("1", "1")?;
    let second = other_key("1", "1")?;

    let mut forwards = MemoryJournal::new();
    append(&mut forwards, EffectEvent::IntentAdmitted { key: first })?;
    append(&mut forwards, EffectEvent::IntentAdmitted { key: second })?;

    let mut backwards = MemoryJournal::new();
    append(&mut backwards, EffectEvent::IntentAdmitted { key: second })?;
    append(&mut backwards, EffectEvent::IntentAdmitted { key: first })?;

    assert_eq!(forwards.tail().sequence(), backwards.tail().sequence());
    assert_ne!(forwards.tail().head(), backwards.tail().head());
    Ok(())
}

#[test]
fn a_stale_tail_is_refused_and_changes_nothing() -> TestResult {
    let key = key("1", "1")?;
    let mut journal = MemoryJournal::new();
    let genesis = journal.tail();
    let committed = append(&mut journal, EffectEvent::IntentAdmitted { key })?;

    let refused = journal.compare_and_append(genesis, &EffectEvent::DispatchPrepared { key });
    match refused {
        Err(JournalError::TailMismatch { expected, actual }) => {
            assert_eq!(expected, genesis);
            assert_eq!(actual, committed.position());
        }
        other => return Err(format!("expected a tail mismatch, got {other:?}").into()),
    }
    assert_eq!(journal.committed().len(), 1);
    assert_eq!(journal.tail(), committed.position());
    Ok(())
}

#[test]
fn a_second_controller_reading_the_same_tail_is_fenced() -> TestResult {
    let key = key("1", "1")?;
    let mut journal = MemoryJournal::new();
    let both_saw = journal.tail();

    let winner = journal.compare_and_append(both_saw, &EffectEvent::IntentAdmitted { key })?;

    let loser = journal.compare_and_append(both_saw, &EffectEvent::DispatchPrepared { key });
    assert!(
        matches!(loser, Err(JournalError::TailMismatch { .. })),
        "the second writer from a stale reading must be refused"
    );
    assert_eq!(journal.committed().len(), 1);
    assert_eq!(journal.tail(), winner.position());
    Ok(())
}

#[test]
fn a_dispatch_before_its_intent_is_refused() -> TestResult {
    let key = key("1", "1")?;
    let mut journal = MemoryJournal::new();
    let refused =
        journal.compare_and_append(journal.tail(), &EffectEvent::DispatchPrepared { key });
    match refused {
        Err(JournalError::OutOfOrder {
            key: refused_key,
            expected,
            attempted,
        }) => {
            assert_eq!(*refused_key, key);
            assert_eq!(expected, Some(EventKind::IntentAdmitted));
            assert_eq!(attempted, EventKind::DispatchPrepared);
        }
        other => return Err(format!("expected an ordering refusal, got {other:?}").into()),
    }
    assert!(journal.committed().is_empty());
    Ok(())
}

#[test]
fn the_ladder_is_walked_once_per_key() -> TestResult {
    let key = key("1", "1")?;
    let mut journal = MemoryJournal::new();
    append(&mut journal, EffectEvent::IntentAdmitted { key })?;
    let refused = journal.compare_and_append(journal.tail(), &EffectEvent::IntentAdmitted { key });
    assert!(
        matches!(
            refused,
            Err(JournalError::OutOfOrder {
                expected: Some(EventKind::DispatchPrepared),
                attempted: EventKind::IntentAdmitted,
                ..
            })
        ),
        "a second intent for one key is how a blind resend becomes representable"
    );
    assert_eq!(journal.committed().len(), 1);
    Ok(())
}

#[test]
fn a_verified_attempt_refuses_everything_but_a_later_verification() -> TestResult {
    let key = key("1", "1")?;
    let mut journal = applied_journal(key)?;
    append(
        &mut journal,
        EffectEvent::Verified {
            key,
            verification: satisfied()?,
        },
    )?;
    let refused = journal.compare_and_append(
        journal.tail(),
        &EffectEvent::OutcomeObserved {
            key,
            evidence: EffectEvidence::NotApplied,
        },
    );
    assert!(
        matches!(
            refused,
            Err(JournalError::OutOfOrder {
                expected: Some(EventKind::Verified),
                attempted: EventKind::OutcomeObserved,
                ..
            })
        ),
        "only a later verification follows a verification; the effect is not re-settled"
    );
    Ok(())
}

#[test]
fn an_ephemeral_journal_reports_itself_and_refuses_the_boundary() -> TestResult {
    let journal = MemoryJournal::new();
    assert_eq!(journal.durability(), DurabilityPromise::Ephemeral);
    match journal.admit_external_handoff() {
        Err(JournalError::PromiseUnmet { required, offered }) => {
            assert_eq!(required, DurabilityPromise::ProcessCrash);
            assert_eq!(offered, DurabilityPromise::Ephemeral);
        }
        other => {
            return Err(format!(
                "an in-memory journal must not host an external handoff, got {other:?}"
            )
            .into());
        }
    }
    Ok(())
}

#[test]
fn only_a_crash_surviving_promise_clears_the_boundary() {
    assert!(!DurabilityPromise::Ephemeral.survives_process_crash());
    assert!(DurabilityPromise::ProcessCrash.survives_process_crash());
    assert!(DurabilityPromise::PowerLoss.survives_process_crash());
    assert!(DurabilityPromise::PowerLoss > DurabilityPromise::ProcessCrash);
}

#[test]
fn a_prepared_dispatch_with_no_result_recovers_unknown() -> TestResult {
    let key = key("1", "1")?;
    let mut journal = MemoryJournal::new();
    admit_and_prepare(&mut journal, key)?;

    let recovered = journal.recover();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered.status(key), Some(AttemptStatus::OutcomeUnknown));
    assert_eq!(recovered.uncertain(), vec![key]);
    Ok(())
}

#[test]
fn an_observed_not_applied_settles_the_unknown() -> TestResult {
    let key = key("1", "1")?;
    let mut journal = MemoryJournal::new();
    admit_and_prepare(&mut journal, key)?;
    append(
        &mut journal,
        EffectEvent::OutcomeObserved {
            key,
            evidence: EffectEvidence::NotApplied,
        },
    )?;

    let recovered = journal.recover();
    assert_eq!(recovered.status(key), Some(AttemptStatus::NotApplied));
    assert!(
        recovered.uncertain().is_empty(),
        "evidence that the effect did not land is what ends the uncertainty"
    );
    Ok(())
}

#[test]
fn an_admitted_intent_alone_is_not_uncertain() -> TestResult {
    let key = key("1", "1")?;
    let mut journal = MemoryJournal::new();
    append(&mut journal, EffectEvent::IntentAdmitted { key })?;
    assert_eq!(journal.recover().status(key), Some(AttemptStatus::Prepared));
    assert!(journal.recover().uncertain().is_empty());
    Ok(())
}

#[test]
fn a_satisfied_predicate_is_the_only_verified_state() -> TestResult {
    let key = key("1", "1")?;
    let mut journal = applied_journal(key)?;
    append(
        &mut journal,
        EffectEvent::Verified {
            key,
            verification: satisfied()?,
        },
    )?;
    assert_eq!(journal.recover().status(key), Some(AttemptStatus::Verified));
    Ok(())
}

#[test]
fn a_predicate_that_did_not_hold_is_not_a_verification() -> TestResult {
    let key = key("1", "1")?;
    let mut journal = applied_journal(key)?;
    append(
        &mut journal,
        EffectEvent::Verified {
            key,
            verification: Verification::new(
                Id128::from_hex(PREDICATE)?,
                1,
                blake3(b"observations"),
                VerificationResult::NotSatisfied,
            ),
        },
    )?;

    let status = journal.recover().status(key);
    assert_eq!(status, Some(AttemptStatus::VerificationFailed));
    assert_ne!(status, Some(AttemptStatus::Applied));
    assert_ne!(status, Some(AttemptStatus::Verified));
    assert!(
        !status.is_some_and(AttemptStatus::is_uncertain),
        "the effect is known to have landed, so a failed predicate is not uncertainty"
    );
    Ok(())
}

fn verdict(
    version: u64,
    result: VerificationResult,
) -> Result<Verification, Box<dyn std::error::Error>> {
    Ok(Verification::new(
        Id128::from_hex(PREDICATE)?,
        version,
        blake3(format!("observations at version {version}").as_bytes()),
        result,
    ))
}

/// #260: a verdict is revisable. `Verified`, then the world moves and the
/// predicate no longer holds, then it holds again: each is recordable, the
/// recovered status follows the latest, and the verdict names the predicate
/// version it was decided at.
#[test]
fn a_verification_can_be_revised_in_both_directions() -> TestResult {
    let key = key("1", "1")?;
    let mut journal = applied_journal(key)?;
    for (version, result) in [
        (1, VerificationResult::Satisfied),
        (2, VerificationResult::NotSatisfied),
        (3, VerificationResult::Satisfied),
    ] {
        append(
            &mut journal,
            EffectEvent::Verified {
                key,
                verification: verdict(version, result)?,
            },
        )?;
    }

    let recovered = journal.recover();
    assert_eq!(recovered.status(key), Some(AttemptStatus::Verified));
    assert_eq!(
        recovered
            .verification(key)
            .map(|found| found.predicate_version()),
        Some(3),
        "the status is qualified by the version it was decided at"
    );
    let trail: Vec<_> = recovered
        .history(key)
        .iter()
        .map(|change| (change.from(), change.to(), change.position()))
        .collect();
    assert_eq!(
        trail,
        vec![
            (None, AttemptStatus::Prepared, 0),
            (
                Some(AttemptStatus::Prepared),
                AttemptStatus::OutcomeUnknown,
                1
            ),
            (
                Some(AttemptStatus::OutcomeUnknown),
                AttemptStatus::Applied,
                2
            ),
            (Some(AttemptStatus::Applied), AttemptStatus::Verified, 3),
            (
                Some(AttemptStatus::Verified),
                AttemptStatus::VerificationFailed,
                4
            ),
            (
                Some(AttemptStatus::VerificationFailed),
                AttemptStatus::Verified,
                5
            ),
        ],
        "what changed, what it superseded, and where in the journal"
    );
    Ok(())
}

#[test]
fn recovery_lists_attempts_in_admission_order() -> TestResult {
    let first = key("1", "1")?;
    let second = other_key("2", "1")?;
    let mut journal = MemoryJournal::new();
    admit_and_prepare(&mut journal, second)?;
    admit_and_prepare(&mut journal, first)?;

    let attempts = journal.recover().attempts().to_vec();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0].key(), second);
    assert_eq!(attempts[1].key(), first);
    Ok(())
}

#[test]
fn verify_accepts_an_untampered_chain() -> TestResult {
    let key = key("1", "1")?;
    let mut journal = MemoryJournal::new();
    admit_and_prepare(&mut journal, key)?;
    assert_eq!(journal.verify()?, journal.tail());
    Ok(())
}

#[test]
fn verify_rejects_a_rewritten_position() -> TestResult {
    let key = key("1", "1")?;
    let mut journal = MemoryJournal::new();
    admit_and_prepare(&mut journal, key)?;

    let mut tampered = journal.committed().to_vec();
    let last = tampered
        .pop()
        .ok_or("the journal should hold two entries after admit and prepare")?;
    tampered.push(JournalEntry::new(JournalPosition::genesis(), *last.event()));

    match verify_chain(&tampered) {
        Err(broken) => assert_eq!(broken.at(), 2),
        Ok(position) => {
            return Err(format!("a rewritten position must be detected, got {position}").into());
        }
    }
    Ok(())
}

#[test]
fn verify_rejects_a_reordered_chain() -> TestResult {
    let key = key("1", "1")?;
    let mut journal = MemoryJournal::new();
    admit_and_prepare(&mut journal, key)?;

    let mut reordered = journal.committed().to_vec();
    reordered.reverse();

    match verify_chain(&reordered) {
        Err(broken) => assert_eq!(broken.at(), 1),
        Ok(position) => {
            return Err(format!("a reordered chain must be detected, got {position}").into());
        }
    }
    Ok(())
}

#[test]
fn a_key_encodes_deterministically() -> TestResult {
    let key = key("1", "1")?;
    let bytes = key.to_bytes()?;
    assert_eq!(
        bytes.as_slice(),
        key.to_bytes()?.as_slice(),
        "one key encodes the same way twice"
    );
    assert!(
        !bytes.is_empty(),
        "and the encoding carries something: {bytes:?}"
    );
    Ok(())
}

#[test]
fn keys_differing_in_one_field_differ_in_bytes() -> TestResult {
    let base = key("1", "1")?;
    let later_attempt = key("2", "1")?;
    let later_epoch = key("1", "2")?;
    let other = other_key("1", "1")?;

    assert_ne!(
        base.to_bytes()?.as_slice(),
        later_attempt.to_bytes()?.as_slice()
    );
    assert_ne!(
        base.to_bytes()?.as_slice(),
        later_epoch.to_bytes()?.as_slice()
    );
    assert_ne!(base.to_bytes()?.as_slice(), other.to_bytes()?.as_slice());
    Ok(())
}

#[test]
fn an_event_encoding_carries_its_kind_and_its_key() -> TestResult {
    let key = key("1", "1")?;
    let admitted = EffectEvent::IntentAdmitted { key }.to_bytes()?;
    let prepared = EffectEvent::DispatchPrepared { key }.to_bytes()?;
    let applied = EffectEvent::OutcomeObserved {
        key,
        evidence: EffectEvidence::Applied,
    }
    .to_bytes()?;
    let not_applied = EffectEvent::OutcomeObserved {
        key,
        evidence: EffectEvidence::NotApplied,
    }
    .to_bytes()?;

    assert_ne!(admitted.as_slice(), prepared.as_slice());
    assert_ne!(applied.as_slice(), not_applied.as_slice());

    // What the encoding says about the event is read back out of it. A
    // field that failed to encode would not shorten the buffer, since the
    // archived form is one width for every variant of a type, so comparing
    // lengths would not have caught it. Decoding does.
    for (bytes, event) in [
        (admitted, EffectEvent::IntentAdmitted { key }),
        (prepared, EffectEvent::DispatchPrepared { key }),
        (
            applied,
            EffectEvent::OutcomeObserved {
                key,
                evidence: EffectEvidence::Applied,
            },
        ),
        (
            not_applied,
            EffectEvent::OutcomeObserved {
                key,
                evidence: EffectEvidence::NotApplied,
            },
        ),
    ] {
        assert_eq!(
            lgwks_std::wire::from_bytes::<EffectEvent, WireError>(&bytes)?,
            event
        );
    }
    Ok(())
}

#[test]
fn a_verification_encoding_names_its_predicate_and_its_version() -> TestResult {
    let key = key("1", "1")?;
    let first = EffectEvent::Verified {
        key,
        verification: satisfied()?,
    }
    .to_bytes()?;
    let next_version = EffectEvent::Verified {
        key,
        verification: Verification::new(
            Id128::from_hex(PREDICATE)?,
            2,
            blake3(b"observations"),
            VerificationResult::Satisfied,
        ),
    }
    .to_bytes()?;
    assert_ne!(first.as_slice(), next_version.as_slice());
    assert_eq!(
        lgwks_std::wire::from_bytes::<EffectEvent, WireError>(&first)?,
        EffectEvent::Verified {
            key,
            verification: satisfied()?,
        }
    );
    assert_eq!(
        lgwks_std::wire::from_bytes::<EffectEvent, WireError>(&next_version)?,
        EffectEvent::Verified {
            key,
            verification: Verification::new(
                Id128::from_hex(PREDICATE)?,
                2,
                blake3(b"observations"),
                VerificationResult::Satisfied,
            ),
        }
    );
    Ok(())
}
