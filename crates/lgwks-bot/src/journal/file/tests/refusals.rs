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

/// The tail search hashes only a candidate that decodes as a record this writer
/// frames. A real event payload is one; every shorter cut of it, and bytes no
/// writer produced, are not, so a noise tail costs a decode per length instead
/// of a blake3 over each (1.41 s to open before, `--example tail_cost`).
#[test]
fn only_a_payload_this_writer_frames_is_a_tail_candidate() -> TestResult {
    let (path, _guard) = subject("framed-record")?;
    let (bytes, third) = three_frame_file(&path)?;
    let start = third.saturating_add(LENGTH_BYTES);
    let end = start.saturating_add(usize::try_from(declared_at(&bytes, third))?);
    let payload = bytes.get(start..end).ok_or("the third frame's payload")?;
    assert!(
        scan::is_framed_record(payload),
        "an event this journal wrote is a candidate"
    );
    for cut in 1..payload.len() {
        let prefix = payload.get(..cut).ok_or("a cut inside the payload")?;
        assert!(
            !scan::is_framed_record(prefix),
            "a {cut}-byte cut of a {}-byte event is no record",
            payload.len()
        );
    }
    for fill in [0x00u8, 0xff, 0x5a] {
        let noise = vec![fill; payload.len()];
        assert!(
            !scan::is_framed_record(&noise),
            "{fill:#04x} noise of the event's length is no record"
        );
    }
    Ok(())
}

/// A carried seal whose length is inflated past the end of the file still
/// refuses under the filter: the event behind it decodes, so it is hashed, and
/// it chains from the seal's stored head, so the successor is refused rather
/// than trimmed back to an empty file.
#[test]
fn a_lengthened_carried_seal_is_refused_not_trimmed() -> TestResult {
    let (dir, _guard) = subject("lengthened-seal")?;
    let successor = sealed_successor(&dir)?;
    let bytes = std::fs::read(&successor)?;
    let seal = *frame_starts(&bytes, 0)?
        .first()
        .ok_or("a successor opens with its carried seal")?;
    let declared = declared_at(&bytes, seal);
    let past_end = u32::try_from(bytes.len().saturating_sub(seal))?;
    for lied_length in [past_end, past_end.saturating_add(64)] {
        let lied = with_prefix(&bytes, seal, lied_length);
        std::fs::write(&successor, &lied)?;
        require_refused_untouched(
            &successor,
            &lied,
            0,
            &format!("carried seal {declared} -> {lied_length}"),
        )?;
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
