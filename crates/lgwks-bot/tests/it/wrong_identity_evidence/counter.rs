//! T12's counter clause: a checked counter never aliases an identity it issued, and a journal position carries a head so a wrapped sequence is detectable.

use super::*;

// ── The counter clause ──────────────────────────────────────────────────────

/// A checked counter's exhaustion never aliases an identity it already issued.
///
/// The row's second property, and the one with no bot in it. [`AttemptId`] and
/// [`EnvironmentEpoch`] are `NonZeroU64` newtypes over a monotonic sequence, and
/// the failure mode this rules out is specific: a wrapped counter restarts at
/// [`AttemptId::FIRST`], which is a value this type has already handed out, and
/// a settlement arriving under it would compare equal to one from the beginning
/// of the run — so a genuine settlement would be refused as a duplicate. Running
/// out is recoverable; silently reusing an identity is not.
#[test]
fn a_checked_counter_exhaustion_never_aliases_an_issued_identity() -> TestResult {
    let one = std::num::NonZeroU64::new(1).ok_or("one is non-zero")?;
    let last = std::num::NonZeroU64::new(u64::MAX).ok_or("u64::MAX is non-zero")?;

    let last_attempt = AttemptId::new(last);
    assert!(
        last_attempt.is_exhausted(),
        "the last attempt reports itself spent"
    );
    assert_eq!(
        last_attempt.checked_next(),
        None,
        "the last attempt has no next one: a wrapped counter would hand out \
         AttemptId::FIRST, which this sequence already issued"
    );
    assert_ne!(
        last_attempt.checked_next(),
        Some(AttemptId::new(one)),
        "exhaustion must not alias the first identity the sequence issued"
    );
    // And the value *before* the last still advances normally, which is what
    // makes exhaustion a boundary rather than a wall.
    let penultimate = AttemptId::new(std::num::NonZeroU64::new(u64::MAX - 1).ok_or("non-zero")?);
    assert_eq!(
        penultimate.checked_next(),
        Some(last_attempt),
        "the attempt before the last still advances to it"
    );
    assert!(!penultimate.is_exhausted(), "and is not itself exhausted");

    // The generation counter: the same refusal, the same reason, and a different
    // role name so a refusal message names the counter that ran out.
    let last_generation = EnvironmentEpoch::new(last);
    assert!(
        last_generation.is_exhausted(),
        "the last generation is spent"
    );
    assert_eq!(
        last_generation.checked_next(),
        None,
        "a wrapped generation would re-authorize commands the broker already \
         fenced, which is the one thing the counter exists to prevent"
    );
    assert_eq!(
        AttemptId::role(),
        "attempt",
        "and each counter names which sequence it is"
    );
    assert_eq!(
        EnvironmentEpoch::role(),
        "environment epoch",
        "so a refusal about exhaustion names the right one"
    );
    Ok(())
}

/// The journal's position counter is a pair, and a wrap would be detectable
/// only because of the head.
///
/// The part of the counter clause that reaches the journal. A position is
/// `(sequence, head)` precisely so that a wrapped sequence — which would make
/// two positions compare equal and defeat the guard the sequence exists to
/// provide — is detectable: the head changes with every append, so a position
/// that reappeared the same sequence could not also reappear the same head.
///
/// `u64::MAX` appends cannot be driven here, so the exhaustion refusal is
/// asserted as what it *is* — a named variant on the append path — rather than
/// by manufacturing the state. What is proved is the shape that makes the wrap
/// detectable, and that the refusal exists rather than a silent wrap.
#[test]
fn a_journal_position_carries_a_head_so_a_wrapped_sequence_is_detectable() -> TestResult {
    let mut journal = MemoryJournal::new();
    let genesis = journal.tail();
    assert_eq!(
        genesis,
        JournalPosition::genesis(),
        "an empty journal's tail is the genesis"
    );

    // Exhaustion is reachable and named: the variant exists on the append path
    // and says what was spent rather than reading as a storage failure.
    assert_eq!(
        JournalError::Exhausted.to_string(),
        "journal position exhausted",
        "the refusal names itself rather than reading as a storage failure"
    );

    let tail = journal.tail();
    let first = journal.compare_and_append(
        tail,
        &EffectEvent::IntentAdmitted {
            key: sim::rig::attempt_key(1)?,
        },
    )?;
    let after = journal.tail();
    assert_eq!(
        after.sequence(),
        genesis.sequence().saturating_add(1),
        "the first append moves the sequence by one, so it never reuses the \
         genesis value"
    );
    assert_ne!(
        after.head(),
        genesis.head(),
        "and the head moves with it, so a repeated sequence could not be \
         mistaken for the same position"
    );
    assert_eq!(
        first.position(),
        after,
        "the acknowledgment names the tail the journal then held"
    );
    Ok(())
}
