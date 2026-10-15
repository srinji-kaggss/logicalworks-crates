use super::*;
use crate::effect::{
    ActionDigest, EffectIdentity, EnvironmentEpoch, EnvironmentId, FlowRevision, RunId,
};
use crate::journal::EffectEvidence;
use lgwks_std::hash::blake3;
use std::path::Path;

/// A test that needs a parsed value returns `Result` and propagates, because
/// the crate forbids a panicking path anywhere, tests included.
type TestResult = Result<(), Box<dyn std::error::Error>>;

const RUN: &str = "0102030405060708090a0b0c0d0e0f10";
const ACTION: &str = "1112131415161718191a1b1c1d1e1f20";
const ENV: &str = "2122232425262728292a2b2c2d2e2f30";
const FLOW_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const DIGEST_HEX: &str = "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeffe0e1e2e3e4e5e6e7e8e9eaebecedeeef";

/// A key for attempt `attempt` of the shared action.
fn key(attempt: &str) -> Result<EffectKey, Box<dyn std::error::Error>> {
    let run = RunId::from_hex(RUN)?;
    let environment = EnvironmentId::from_hex(ENV)?;
    let flow = FlowRevision::from_tagged("blake3_256", FLOW_HEX)?;
    let action = ActionId::from_hex(ACTION)?;
    let attempt = AttemptId::from_decimal(attempt)?;
    let digest = ActionDigest::from_tagged("blake3_256", DIGEST_HEX)?;
    let epoch = EnvironmentEpoch::from_decimal("1")?;
    Ok(EffectIdentity::new(run, environment, flow).key(action, attempt, digest, epoch))
}

/// The watermark of the shipped ceilings, which is what the headroom claim in
/// the module's docs is about.
#[test]
fn the_watermark_leaves_a_fifth_of_each_ceiling() {
    assert_eq!(watermark_of(100), 80);
    assert_eq!(watermark_of(100_000), 80_000);
    assert_eq!(watermark_of(64 * 1024 * 1024), 53_687_091);
    assert_eq!(watermark_of(0), 0);
    assert_eq!(watermark_of(u64::MAX), watermark_of(u64::MAX));
}

#[test]
fn a_journal_that_does_not_continue_is_never_due() -> TestResult {
    let mut watermark = ContinuationWatermark::inert(1, 2, 1, 2);
    assert!(!watermark.continues());
    assert!(
        !watermark.is_due(),
        "an inert journal is never due, at any size"
    );
    watermark = ContinuationWatermark::measured(80, 100, 80, 100, ContinuationPolicy::new(80, 80)?);
    assert!(
        watermark.is_due(),
        "eighty percent of either ceiling is due"
    );
    assert_eq!(watermark.event_headroom(), 20);
    assert_eq!(watermark.byte_headroom(), 20);
    Ok(())
}

#[test]
fn the_watermark_is_either_ceiling_not_both() -> TestResult {
    let policy = ContinuationPolicy::new(80, 80)?;
    let events = ContinuationWatermark::measured(100, 100, 0, 1_000_000, policy);
    assert!(
        events.is_due(),
        "a full event ceiling is due whatever the byte count says"
    );
    let bytes = ContinuationWatermark::measured(0, 1_000_000, 1_000_000, 1_000_000, policy);
    assert!(
        bytes.is_due(),
        "a full byte ceiling is due whatever the event count says"
    );
    Ok(())
}

/// The declared policy is the shipped fraction of the shipped ceilings, and a
/// policy at a ceiling is refused rather than clamped to one.
#[test]
fn the_declared_policy_leaves_the_shipped_headroom_and_a_late_one_is_refused() -> TestResult {
    let declared = ContinuationPolicy::declared();
    assert_eq!(declared, ContinuationPolicy::default());
    assert_eq!(declared.events_at(), 80_000);
    assert_eq!(declared.bytes_at(), 53_687_091);
    assert!(
        declared.events_at() < SHIPPED_EVENT_CEILING && declared.bytes_at() < SHIPPED_BYTE_CEILING,
        "the watermark is strictly inside both ceilings, so settlement always has room"
    );
    assert!(
        ContinuationPolicy::new(SHIPPED_EVENT_CEILING, 1).is_err(),
        "a trigger at the ceiling would ask for a continuation the journal cannot afford"
    );
    Ok(())
}

#[test]
fn a_checkpoint_round_trips_through_its_payload() -> TestResult {
    let key = key("7")?;
    let verification = Verification::new(
        crate::effect::Id128::from_hex(&"42".repeat(16))?,
        3,
        blake3(b"observed"),
        crate::journal::VerificationResult::NotSatisfied,
    );
    let checkpoint = Continuation::new(
        4,
        JournalPosition::genesis(),
        vec![SettledAttempt::new(
            key,
            EventKind::Verified,
            AttemptStatus::VerificationFailed,
            Some(verification),
        )],
        vec![UnresolvedAttempt::new(key, EventKind::DispatchPrepared)],
    )?;

    let payload = checkpoint.to_payload()?;
    assert!(
        Continuation::is_payload(&payload),
        "a checkpoint payload carries the magic a reader asks for first"
    );
    assert!(
        !Continuation::is_payload(&[0x00, 0x01, 0x02]),
        "an event payload does not"
    );

    let read =
        Continuation::from_payload(&payload).ok_or("a checkpoint payload reads as one")??;
    assert_eq!(read, checkpoint, "the archive is what a writer wrote");
    assert_eq!(read.generation(), 4);
    assert_eq!(read.settled().len(), 1);
    assert_eq!(read.unresolved().len(), 1);
    let carried = read.settled().first().ok_or("the fold is missing")?;
    assert_eq!(carried.status(), Some(AttemptStatus::VerificationFailed));
    assert_eq!(
        carried.verification().map(Verification::predicate_version),
        Some(3),
        "the verification digest's predicate version crosses the boundary"
    );
    assert_eq!(
        read.unresolved()
            .first()
            .map(UnresolvedAttempt::recovered_status),
        Some(Some(AttemptStatus::OutcomeUnknown)),
        "an unresolved attempt stays unknown on the far side"
    );
    Ok(())
}

#[test]
fn a_folded_action_refuses_an_older_attempt_and_admits_a_newer_one()
-> Result<(), Box<dyn std::error::Error>> {
    let latest = key("9")?;
    let folded = SettledAttempt::new(latest, EventKind::Verified, AttemptStatus::Applied, None);
    assert!(
        folded.already_walked(latest),
        "the latest attempt itself is already walked"
    );
    assert!(
        folded.already_walked(key("3")?),
        "an older attempt of the same action is already walked"
    );
    assert!(
        !folded.already_walked(key("10")?),
        "a newer attempt is new work, not a replay"
    );
    Ok(())
}

/// The largest checkpoint the two declared counts admit fits one frame.
///
/// Both counts are charged before the checkpoint is archived and the frame
/// ceiling after it, so a count the frame cannot hold is a bound no writer
/// reaches: the carry is refused by the frame, as a storage fault, instead of
/// by the count it names. Measured at the widest record each list can hold — a
/// settled attempt carrying a verification, at the largest generation.
#[test]
fn the_largest_checkpoint_the_counts_admit_fits_one_frame() -> TestResult {
    let one = key("1")?;
    let verification = Verification::new(
        crate::effect::Id128::from_hex(ACTION)?,
        u64::MAX,
        blake3(b"the widest verification a settled record carries"),
        crate::journal::VerificationResult::NotSatisfied,
    );
    let settled = (0..MAX_CHECKPOINT_SETTLED)
        .map(|_| {
            SettledAttempt::new(
                one,
                EventKind::Verified,
                AttemptStatus::VerificationFailed,
                Some(verification),
            )
        })
        .collect::<Vec<_>>();
    let unresolved = (0..MAX_CHECKPOINT_UNRESOLVED)
        .map(|_| UnresolvedAttempt::new(one, EventKind::DispatchPrepared))
        .collect::<Vec<_>>();
    let widest = Continuation::new(u64::MAX, JournalPosition::genesis(), settled, unresolved)?;
    let archived = widest.to_payload()?.len();
    assert!(
        archived <= super::super::file::MAX_FRAME_BYTES,
        "the widest checkpoint the counts admit archives to {archived} bytes, past \
             the {} byte frame it must be sealed in",
        super::super::file::MAX_FRAME_BYTES
    );
    Ok(())
}

#[test]
fn a_carry_past_its_declared_bound_is_refused_before_anything_is_written() -> TestResult {
    let one = key("1")?;
    let folded = (1..=MAX_CHECKPOINT_SETTLED + 1)
        .map(|_| SettledAttempt::new(one, EventKind::Verified, AttemptStatus::Applied, None))
        .collect::<Vec<_>>();
    let refused = Continuation::new(1, JournalPosition::genesis(), folded, Vec::new());
    assert!(
        matches!(
            refused,
            Err(JournalError::CapacityExceeded {
                resource: JournalLimitKind::CheckpointActions,
                ..
            })
        ),
        "an over-large carry is refused, not framed: {refused:?}"
    );

    let unresolved = (0..MAX_CHECKPOINT_UNRESOLVED + 1)
        .map(|_| UnresolvedAttempt::new(one, EventKind::DispatchPrepared))
        .collect::<Vec<_>>();
    let refused = Continuation::new(1, JournalPosition::genesis(), Vec::new(), unresolved);
    assert!(
        matches!(
            refused,
            Err(JournalError::CapacityExceeded {
                resource: JournalLimitKind::CheckpointUnresolved,
                ..
            })
        ),
        "an over-large unresolved carry is refused: {refused:?}"
    );

    Ok(())
}

#[test]
fn an_index_this_build_does_not_name_reads_as_absent_not_as_the_ladder_foot() {
    assert_eq!(rung_of(u8::MAX), None);
    assert_eq!(status_of(u8::MAX), None);
    assert_eq!(rung_of(0), Some(EventKind::IntentAdmitted));
    // Index 5 round-trips to the status it names; an absent reading is a
    // failure here, not a stand-in index to compare against.
    assert_eq!(
        status_of(5),
        Some(AttemptStatus::VerificationFailed),
        "index 5 names VerificationFailed"
    );
    assert_eq!(status_index(AttemptStatus::VerificationFailed), 5);
}

#[test]
fn a_successor_is_a_fixed_width_name_in_the_predecessors_own_directory() -> TestResult {
    let first = Path::new("/var/run/bot.jrnl");
    let second = successor_path(first);
    assert_eq!(second, Path::new("/var/run/bot.jrnl.cont/000001"));
    assert_eq!(generation_of(first), 1);
    assert_eq!(generation_of(&second), 2);
    let third = successor_path(&second);
    assert_eq!(third, Path::new("/var/run/bot.jrnl.cont/000002"));
    assert_eq!(generation_of(&third), 3);

    // The point of the fixed width: a chain a hundred long has names the same
    // length as a chain one long, which is what a run measured in weeks needs
    // and what an appended token per generation would lose at fifty.
    let mut here = first.to_path_buf();
    for _ in 0..500 {
        here = successor_path(&here);
    }
    assert_eq!(
        here.file_name().map(std::ffi::OsStr::len),
        Some(GENERATION_DIGITS),
        "a five-hundredth generation's name is still {} characters",
        GENERATION_DIGITS
    );
    assert_eq!(generation_of(&here), 501);

    // And a base journal whose own name reads as a generation is refused,
    // because its successor would be un-derivable.
    assert!(
        refuse_ambiguous_base(Path::new("/var/run/000123")).is_err(),
        "a six-digit base name would be read as a generation"
    );
    assert!(refuse_ambiguous_base(first).is_ok());
    assert!(refuse_ambiguous_base(&third).is_ok());
    assert!(
        is_generation_path(&second),
        "and the successor is this scheme's own, both halves of the name"
    );
    assert!(!is_generation_path(Path::new("/var/run/bot.jrnl.cont/abc")));
    Ok(())
}

/// Every `SealPause` names itself and the enum's sweep is the whole enum, so
/// a boundary added later cannot escape a crash sweep.
#[test]
fn every_boundary_names_itself() {
    let mut seen = std::collections::BTreeSet::new();
    for pause in SealPause::all() {
        assert!(
            seen.insert(pause.as_str()),
            "two boundaries share a spelling"
        );
        assert!(!pause.as_str().is_empty());
    }
    assert_eq!(seen.len(), SealPause::all().len());
}

/// The two carried statuses a successor must be able to report, so the enum
/// this module folds to has a witness for each.
#[test]
fn a_carried_rung_reads_back_as_the_status_a_recovery_fold_reports() -> TestResult {
    assert_eq!(
        UnresolvedAttempt::new(key("1")?, EventKind::IntentAdmitted).recovered_status(),
        Some(AttemptStatus::Prepared)
    );
    assert_eq!(
        UnresolvedAttempt::new(key("1")?, EventKind::DispatchPrepared).recovered_status(),
        Some(AttemptStatus::OutcomeUnknown)
    );
    assert_eq!(
        UnresolvedAttempt::new(key("1")?, EventKind::Verified).recovered_status(),
        None,
        "a resolved rung is not an unresolved attempt and reads as absent"
    );
    let evidence = EffectEvidence::Applied;
    assert_eq!(
        format!("{evidence:?}"),
        "Applied",
        "the evidence arm exists"
    );
    Ok(())
}
