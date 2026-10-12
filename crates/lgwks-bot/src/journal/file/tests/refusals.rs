//! Committed bytes the journal refuses rather than trims: a rotted prefix, an undecodable frame, a replayed second dispatch.

use super::*;

#[test]
fn a_rotted_length_prefix_is_refused_and_nothing_is_trimmed() -> TestResult {
    let (path, _guard) = subject("length-rot")?;
    {
        let mut journal = FileJournal::open(&path)?;
        for attempt in 1u64..=3 {
            let attempt_key_n = attempt_key(attempt)?;
            journal.compare_and_append(
                journal.tail(),
                &EffectEvent::IntentAdmitted { key: attempt_key_n },
            )?;
        }
    }
    let before = std::fs::metadata(&path)?.len();
    assert!(before > 3, "three frames are on the disk");

    // Rot in the second frame's length prefix: a complete prefix that no
    // torn write of this journal can produce, because a write only ever
    // leaves a prefix of the bytes it intended. A plausible writer cannot
    // have written this; only rot or a hand can have.
    let mut bytes = std::fs::read(&path)?;
    let first_len = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let len_bytes = u32::saturating_from(LENGTH_BYTES);
    let head_bytes = u32::saturating_from(HEAD_BYTES);
    let second = usize::saturating_from(
        first_len
            .saturating_add(len_bytes)
            .saturating_add(head_bytes),
    )
    .min(bytes.len());
    if second < bytes.len() {
        bytes[second] ^= 0x40;
    }
    std::fs::write(&path, &bytes)?;

    match FileJournal::open(&path) {
        Err(JournalError::Corrupt(corruption)) => {
            assert_eq!(corruption.at(), 1, "the rotted frame is named");
            assert!(
                matches!(corruption.kind(), CorruptionKind::Framed),
                "the refusal names the framing, not the contents"
            );
        }
        Err(other) => return Err(format!("expected a corruption refusal, got {other}").into()),
        Ok(_) => return Err("a rotted length prefix must not reopen as a journal".into()),
    }
    assert_eq!(
        std::fs::metadata(&path)?.len(),
        before,
        "refused bytes are never trimmed"
    );
    Ok(())
}

#[test]
fn an_undecodable_committed_frame_is_refused_not_trimmed() -> TestResult {
    let (path, _guard) = subject("undecodable")?;
    let key = key()?;
    {
        let mut journal = FileJournal::open(&path)?;
        journal.compare_and_append(journal.tail(), &EffectEvent::IntentAdmitted { key })?;
    }
    // Overwrite the frame's payload, keeping the length and the stored
    // head: well-framed bytes that no longer decode to the event whose
    // head they carry.
    let mut bytes = std::fs::read(&path)?;
    let raw_len = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let payload_len = usize::saturating_from(raw_len);
    let mid = LENGTH_BYTES.saturating_add(payload_len >> 1);
    bytes[mid] = bytes[mid].wrapping_add(0x55);
    std::fs::write(&path, &bytes)?;

    match FileJournal::open(&path) {
        Err(JournalError::Corrupt(corruption)) => {
            assert_eq!(corruption.at(), 0);
            assert!(matches!(
                corruption.kind(),
                CorruptionKind::Undecodable | CorruptionKind::Chain(_)
            ));
        }
        Err(other) => return Err(format!("expected a corruption refusal, got {other}").into()),
        Ok(_) => return Err("undecodable committed bytes must not reopen".into()),
    }
    Ok(())
}

#[test]
fn the_ladder_refuses_a_second_prepared_dispatch_from_a_replayed_view() -> TestResult {
    let (path, _guard) = subject("ladder")?;
    let key = key()?;
    let mut journal = FileJournal::open(&path)?;
    journal.compare_and_append(journal.tail(), &EffectEvent::IntentAdmitted { key })?;
    journal.compare_and_append(journal.tail(), &EffectEvent::DispatchPrepared { key })?;
    let refused =
        journal.compare_and_append(journal.tail(), &EffectEvent::DispatchPrepared { key });
    match refused {
        Err(JournalError::OutOfOrder {
            expected: Some(EventKind::OutcomeObserved),
            ..
        }) => {}
        Err(other) => {
            return Err(format!("expected an out-of-order refusal, got {other}").into());
        }
        Ok(_) => return Err("a second dispatch of one attempt must be unrepresentable".into()),
    }
    Ok(())
}
