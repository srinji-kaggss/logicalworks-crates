//! The identity half of eval cases E06 and E07, as executing tests.
//!
//! `okf/evals.json` records both as `not_run`. Their oracles have two halves,
//! and only one of them can be checked at this layer today:
//!
//! - **E06, "Stale revision settlement."** Deliver duplicate old `Applied` and
//!   `NotApplied` evidence after a new revision occupies the old runtime slot.
//!   Oracle: the new action remains unknown, an identity mismatch is reported,
//!   and no extra effect occurs.
//! - **E07, "Stale attempt and contradictory receipt."** Deliver a
//!   previous-attempt result, an exact duplicate, and the same sequence with a
//!   contradictory result. Oracle: stale and contradictory input changes no
//!   state, the duplicate is idempotent, and full payload binding is checked.
//!
//! What these tests establish is that the identity predicates distinguish every
//! delivery the two cases name. What they cannot establish is the durable half:
//! both evals require "a real receiver or persistent-state oracle" and E11's
//! crash matrix, and no journal or environment exists in the crate yet. That
//! gap is not narrowed by anything here, and nothing below should be read as
//! claiming those cases pass.
//!
//! The last test is the design result worth keeping: a key cannot distinguish a
//! settled attempt from a contradicted one, because both are the same key with
//! different evidence. That is why the ledger keeps a per-generation settlement
//! record beside the key rather than deriving settlement from identity alone.

use core::num::NonZeroU64;

use lgwks_bot::effect::{
    ActionDigest, ActionId, AttemptId, EffectKey, EnvironmentEpoch, EnvironmentId, FlowRevision,
    RunId,
};

const RUN: &str = "0102030405060708090a0b0c0d0e0f10";
const ACTION: &str = "1112131415161718191a1b1c1d1e1f20";
const OTHER_ACTION: &str = "3132333435363738393a3b3c3d3e3f40";
const ENV: &str = "2122232425262728292a2b2c2d2e2f30";
const FLOW_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const DIGEST_HEX: &str = "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeffe0e1e2e3e4e5e6e7e8e9eaebecedeeef";
const OTHER_DIGEST_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e2f";

/// The crate refuses a panicking path anywhere, tests included, so these tests
/// return `Result` and propagate. A failure names the cause instead of a line
/// number inside a macro.
type TestResult = Result<(), Box<dyn std::error::Error>>;

fn counter(value: u64) -> Result<NonZeroU64, String> {
    NonZeroU64::new(value).ok_or_else(|| format!("{value} is not a valid counter"))
}

/// One delivery the caller makes about an attempt.
///
/// The ledger's own `EffectEvidence` is crate-private, and this target uses the
/// public API only. The distinction the enum carries is the one E07 names: the
/// same identity, asserted to have gone two opposite ways.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Claim {
    Applied,
    NotApplied,
}

/// A key for one attempt of one action, with the payload digest selected.
fn key(
    action: &str,
    attempt: u64,
    epoch: u64,
    digest_hex: &str,
) -> Result<EffectKey, Box<dyn std::error::Error>> {
    Ok(EffectKey::new(
        RunId::from_hex(RUN)?,
        ActionId::from_hex(action)?,
        AttemptId::new(counter(attempt)?),
        FlowRevision::from_tagged("blake3_256", FLOW_HEX)?,
        ActionDigest::from_tagged("blake3_256", digest_hex)?,
        EnvironmentId::from_hex(ENV)?,
        EnvironmentEpoch::new(counter(epoch)?),
    ))
}

fn ordinary(attempt: u64) -> Result<EffectKey, Box<dyn std::error::Error>> {
    key(ACTION, attempt, 1, DIGEST_HEX)
}

/// E06. A stale delivery carries an identity that is not the one in the slot.
///
/// The case is slot reuse: the old attempt was settled, and a *different*
/// action now occupies the same runtime slot. A settlement addressed by slot
/// alone matches; one addressed by identity does not.
#[test]
fn e06_a_reused_slot_is_not_the_same_identity() -> TestResult {
    let settled = ordinary(1)?;
    let now_in_the_slot = key(OTHER_ACTION, 1, 1, DIGEST_HEX)?;

    assert_ne!(
        settled, now_in_the_slot,
        "a different action in the same slot must not compare equal"
    );
    assert!(
        !settled.same_attempt_earlier_epoch(now_in_the_slot),
        "a different action is not the same attempt at a later epoch"
    );
    assert!(
        !now_in_the_slot.same_attempt_earlier_epoch(settled),
        "and the comparison is not symmetric in the wrong direction either"
    );
    Ok(())
}

/// E06. Only an epoch bump reads as the same attempt, and only forwards.
///
/// This is the one shape that *should* be recognised: the environment was
/// replaced under a command that was never handed over, so the command is
/// invalid rather than contradicted.
#[test]
fn e06_only_a_forward_epoch_bump_is_the_same_attempt() -> TestResult {
    let prepared = ordinary(1)?;
    let after_restart = key(ACTION, 1, 2, DIGEST_HEX)?;

    assert!(
        prepared.same_attempt_earlier_epoch(after_restart),
        "the same attempt at a later epoch is the slot-reuse case"
    );
    assert!(
        !after_restart.same_attempt_earlier_epoch(prepared),
        "a later epoch does not move backwards"
    );
    assert_ne!(
        prepared, after_restart,
        "and they are still not the same settlement"
    );
    Ok(())
}

/// E07. A previous attempt's result is distinguishable from this one's.
#[test]
fn e07_a_previous_attempt_is_a_different_identity() -> TestResult {
    let previous = ordinary(1)?;
    let current = ordinary(2)?;

    assert_ne!(previous, current);
    assert!(
        !previous.same_attempt_earlier_epoch(current),
        "a different attempt is not an epoch move, even at the same epoch"
    );
    Ok(())
}

/// E07. An exact duplicate is the same identity, which is what makes it safe to
/// repeat.
#[test]
fn e07_an_exact_duplicate_is_the_same_key() -> TestResult {
    let first = ordinary(2)?;
    let redelivered = ordinary(2)?;

    assert_eq!(first, redelivered);
    assert!(
        !first.same_attempt_earlier_epoch(redelivered),
        "a duplicate is not an epoch move, and must not be reported as one"
    );
    Ok(())
}

/// E07. Full payload binding: the same action and attempt with different input
/// is not the same settlement.
#[test]
fn e07_a_changed_payload_is_a_different_identity() -> TestResult {
    let original = ordinary(2)?;
    let rewritten = key(ACTION, 2, 1, OTHER_DIGEST_HEX)?;

    assert_ne!(original, rewritten);
    assert!(
        !original.same_attempt_earlier_epoch(rewritten),
        "a rewritten payload must not be excused as an environment replacement"
    );
    Ok(())
}

/// E07. The key alone cannot detect a contradiction, and that is the point.
///
/// `Applied` and `NotApplied` about one attempt are the *same* identity. So
/// identity cannot be the thing that decides whether a second delivery
/// contradicts the first: only the recorded evidence can. This is why the
/// ledger keeps a settlement record beside the entry rather than deriving
/// settlement from the key, and it is the reason this module does not attempt
/// to replace that record.
#[test]
fn e07_the_key_alone_cannot_detect_a_contradiction() -> TestResult {
    let attempt = ordinary(2)?;
    let claimed_applied = (attempt, Claim::Applied);
    let claimed_not_applied = (attempt, Claim::NotApplied);

    assert_eq!(
        claimed_applied.0, claimed_not_applied.0,
        "both claims are about one attempt, so the identities match"
    );
    assert_ne!(
        claimed_applied.1, claimed_not_applied.1,
        "and only the evidence distinguishes them"
    );
    Ok(())
}
