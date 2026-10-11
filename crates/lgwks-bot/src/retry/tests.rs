//! RQ-009's rule, exercised one conjunct at a time.

use super::*;
use crate::effect::{ActionId, AttemptId, EnvironmentEpoch, EnvironmentId, FlowRevision, RunId};

const RUN: &str = "0102030405060708090a0b0c0d0e0f10";
const ACTION: &str = "1112131415161718191a1b1c1d1e1f20";
const OTHER_ACTION: &str = "3132333435363738393a3b3c3d3e3f40";
const ENV: &str = "2122232425262728292a2b2c2d2e2f30";
const FLOW_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const DIGEST_HEX: &str = "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeffe0e1e2e3e4e5e6e7e8e9eaebecedeeef";
const OTHER_DIGEST_HEX: &str = "0102030405060708090a0b0c0d0e0f10f1f2f3f4f5f6f7f8f9fafbfcfdfeff00";

/// The crate refuses a panicking path anywhere, tests included, so a test
/// that needs a parsed value returns `Result` and propagates.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// A dispatched attempt on the shared action, carrying `digest_hex`.
fn dispatched(digest_hex: &str) -> Result<EffectKey, Box<dyn std::error::Error>> {
    let key_run = RunId::from_hex(RUN)?;
    let key_action = ActionId::from_hex(ACTION)?;
    let key_attempt = AttemptId::from_decimal("1")?;
    let key_flow_revision = FlowRevision::from_tagged("blake3_256", FLOW_HEX)?;
    let key_digest = ActionDigest::from_tagged("blake3_256", digest_hex)?;
    let key_environment = EnvironmentId::from_hex(ENV)?;
    let key_epoch = EnvironmentEpoch::from_decimal("1")?;
    Ok(
        crate::effect::EffectIdentity::new(key_run, key_environment, key_flow_revision).key(
            key_action,
            key_attempt,
            key_digest,
            key_epoch,
        ),
    )
}

/// The payload binding of the shared attempt.
fn payload() -> Result<ActionDigest, Box<dyn std::error::Error>> {
    Ok(ActionDigest::from_tagged("blake3_256", DIGEST_HEX)?)
}

/// A payload binding that differs from [`payload`].
fn other_payload() -> Result<ActionDigest, Box<dyn std::error::Error>> {
    Ok(ActionDigest::from_tagged("blake3_256", OTHER_DIGEST_HEX)?)
}

/// An unresolved attempt under a live authority and nothing else claimed:
/// zero attempts made, zero elapsed, and the shared payload.
///
/// The subject the contract tests below all start from. Built once because
/// they differ from each other in exactly one clause — the contract, its
/// window, or its late-arrival behaviour — and a fixture each of them wrote
/// out by hand is how two of them come to differ in a second one and stop
/// testing what their names say.
fn unresolved() -> Result<RetryFacts, Box<dyn std::error::Error>> {
    Ok(RetryFacts::new(
        RetryPolicy::new(3, Duration::ZERO, Duration::MAX),
        0,
        RetryClass::RequiresEvidence,
        dispatched(DIGEST_HEX)?,
        payload()?,
    )
    .with_live_authority())
}

/// Facts that permit a retry, so each test changes exactly one thing.
fn permissive(class: RetryClass) -> Result<RetryFacts, Box<dyn std::error::Error>> {
    Ok(RetryFacts::new(
        RetryPolicy::new(3, Duration::ZERO, Duration::MAX),
        0,
        class,
        dispatched(DIGEST_HEX)?,
        payload()?,
    )
    .with_live_authority()
    .with_evidence(EffectEvidence::NotApplied))
}

/// A contract over the shared action and payload, with the given window and
/// late-arrival behaviour.
fn contract(
    retention: Duration,
    late_arrival: LateArrival,
) -> Result<DeduplicationContract, Box<dyn std::error::Error>> {
    Ok(DeduplicationContract::new(
        ActionId::from_hex(ACTION)?,
        payload()?,
        DedupScope::new("tenant-7")?,
        retention,
        late_arrival,
    ))
}

#[test]
fn proven_non_application_is_a_plain_retry() -> TestResult {
    let decision = retry_admissible(&permissive(RetryClass::RequiresEvidence)?);
    assert_eq!(decision, RetryDecision::Safe);
    assert!(decision.permits_retry());
    Ok(())
}

#[test]
fn a_permanent_failure_short_circuits() -> TestResult {
    // Every other conjunct holds, and the class alone refuses.
    let facts = permissive(RetryClass::Never)?;
    let decision = retry_admissible(&facts);
    assert_eq!(
        decision,
        RetryDecision::Refused(vec![RetryRefusal::PermanentFailure])
    );
    assert!(!decision.permits_retry());
    Ok(())
}

/// The policy's deadline is a conjunct: a resend after it has passed is
/// refused with both durations named, and one inside it is not (#401).
#[test]
fn a_spent_deadline_refuses_and_names_the_durations() -> TestResult {
    let policy = RetryPolicy::new(3, Duration::ZERO, Duration::from_secs(30));
    let at = |elapsed| {
        permissive(RetryClass::RequiresEvidence).map(|facts| {
            retry_admissible(&RetryFacts { policy, ..facts }.with_elapsed(elapsed))
        })
    };
    assert_eq!(at(Duration::from_secs(29))?, RetryDecision::Safe);
    assert_eq!(
        at(Duration::from_secs(30))?,
        RetryDecision::Refused(vec![RetryRefusal::DeadlineExhausted {
            elapsed: Duration::from_secs(30),
            deadline: Duration::from_secs(30),
        }])
    );
    Ok(())
}

#[test]
fn a_spent_budget_refuses_and_names_the_numbers() -> TestResult {
    let facts = RetryFacts::new(
        RetryPolicy::new(3, Duration::ZERO, Duration::MAX),
        3,
        RetryClass::RequiresEvidence,
        dispatched(DIGEST_HEX)?,
        payload()?,
    )
    .with_live_authority()
    .with_evidence(EffectEvidence::NotApplied);

    assert_eq!(
        retry_admissible(&facts),
        RetryDecision::Refused(vec![RetryRefusal::BudgetExhausted {
            attempts_used: 3,
            max_attempts: 3,
        }])
    );
    Ok(())
}

#[test]
fn one_attempt_policy_exhausts_after_the_first() -> TestResult {
    let facts = RetryFacts::new(
        RetryPolicy::new(1, Duration::ZERO, Duration::MAX),
        1,
        RetryClass::Safe,
        dispatched(DIGEST_HEX)?,
        payload()?,
    )
    .with_live_authority()
    .with_evidence(EffectEvidence::NotApplied);
    assert!(!retry_admissible(&facts).permits_retry());
    Ok(())
}

#[test]
fn absent_authority_refuses() -> TestResult {
    let mut facts = permissive(RetryClass::Safe)?;
    facts.authority_live = false;
    assert_eq!(
        retry_admissible(&facts),
        RetryDecision::Refused(vec![RetryRefusal::AuthorityNotLive])
    );
    Ok(())
}

#[test]
fn authority_defaults_to_refusing() -> TestResult {
    // The builder is not called for authority, so nothing was established.
    let facts = RetryFacts::new(
        RetryPolicy::new(3, Duration::ZERO, Duration::MAX),
        0,
        RetryClass::Safe,
        dispatched(DIGEST_HEX)?,
        payload()?,
    )
    .with_evidence(EffectEvidence::NotApplied);
    assert_eq!(
        retry_admissible(&facts),
        RetryDecision::Refused(vec![RetryRefusal::AuthorityNotLive])
    );
    Ok(())
}

#[test]
fn a_changed_intent_is_not_a_retry() -> TestResult {
    let facts = RetryFacts::new(
        RetryPolicy::new(3, Duration::ZERO, Duration::MAX),
        0,
        RetryClass::Safe,
        dispatched(DIGEST_HEX)?,
        other_payload()?,
    )
    .with_live_authority()
    .with_evidence(EffectEvidence::NotApplied);

    assert_eq!(
        retry_admissible(&facts),
        RetryDecision::Refused(vec![RetryRefusal::IntentChanged])
    );
    Ok(())
}

#[test]
fn an_unknown_outcome_with_no_evidence_is_refused() -> TestResult {
    let facts = unresolved()?;
    assert_eq!(
        retry_admissible(&facts),
        RetryDecision::Refused(vec![RetryRefusal::NotProvenUndone])
    );
    Ok(())
}

#[test]
fn a_gui_click_has_no_contract_to_reach_for() -> TestResult {
    // RQ-009: a GUI click has no generic deduplication contract, so an
    // unsettled click is never retried on the contract half of the
    // disjunct. Nothing here can express otherwise.
    let facts = unresolved()?.with_elapsed(Duration::from_secs(1));

    assert_eq!(
        retry_admissible(&facts),
        RetryDecision::Refused(vec![RetryRefusal::NotProvenUndone])
    );
    Ok(())
}

#[test]
fn a_contract_inside_its_window_covers_a_resend() -> TestResult {
    let facts = unresolved()?
        .with_contract(contract(Duration::from_secs(600), LateArrival::Applied)?)
        .with_elapsed(Duration::from_secs(30));

    assert_eq!(retry_admissible(&facts), RetryDecision::UnderContract);
    Ok(())
}

#[test]
fn a_contract_past_its_window_is_worthless_if_a_late_duplicate_applies() -> TestResult {
    let facts = unresolved()?
        .with_contract(contract(Duration::from_secs(600), LateArrival::Applied)?)
        .with_elapsed(Duration::from_secs(601));

    assert_eq!(
        retry_admissible(&facts),
        RetryDecision::Refused(vec![RetryRefusal::NotProvenUndone])
    );
    Ok(())
}

#[test]
fn an_unspecified_late_arrival_is_not_a_refusal() -> TestResult {
    let facts = unresolved()?
        .with_contract(contract(
            Duration::from_secs(600),
            LateArrival::Unspecified,
        )?)
        .with_elapsed(Duration::from_secs(601));

    assert!(
        !retry_admissible(&facts).permits_retry(),
        "not knowing what the remote does is not a basis for sending"
    );
    Ok(())
}

#[test]
fn a_contract_past_its_window_still_covers_when_the_remote_refuses_late_duplicates() -> TestResult
{
    let facts = unresolved()?
        .with_contract(contract(Duration::from_secs(600), LateArrival::Refused)?)
        .with_elapsed(Duration::from_secs(86_400));

    assert_eq!(retry_admissible(&facts), RetryDecision::UnderContract);
    Ok(())
}

#[test]
fn a_contract_for_another_payload_does_not_cover() -> TestResult {
    let facts = unresolved()?.with_contract(DeduplicationContract::new(
        ActionId::from_hex(ACTION)?,
        other_payload()?,
        DedupScope::new("tenant-7")?,
        Duration::from_secs(600),
        LateArrival::Refused,
    ));

    assert_eq!(
        retry_admissible(&facts),
        RetryDecision::Refused(vec![RetryRefusal::NotProvenUndone])
    );
    Ok(())
}

#[test]
fn a_contract_for_another_action_does_not_cover() -> TestResult {
    let facts = unresolved()?.with_contract(DeduplicationContract::new(
        ActionId::from_hex(OTHER_ACTION)?,
        payload()?,
        DedupScope::new("tenant-7")?,
        Duration::from_secs(600),
        LateArrival::Refused,
    ));

    assert!(!retry_admissible(&facts).permits_retry());
    Ok(())
}

#[test]
fn evidence_is_the_stronger_half_of_the_disjunct() -> TestResult {
    // Both halves hold, and the decision reports the one that needs no
    // contract to justify.
    let facts = RetryFacts::new(
        RetryPolicy::new(3, Duration::ZERO, Duration::MAX),
        0,
        RetryClass::RequiresEvidence,
        dispatched(DIGEST_HEX)?,
        payload()?,
    )
    .with_live_authority()
    .with_evidence(EffectEvidence::NotApplied)
    .with_contract(contract(Duration::from_secs(600), LateArrival::Applied)?);

    assert_eq!(retry_admissible(&facts), RetryDecision::Safe);
    Ok(())
}

#[test]
fn applied_evidence_is_not_proof_of_non_application() -> TestResult {
    let facts = RetryFacts::new(
        RetryPolicy::new(3, Duration::ZERO, Duration::MAX),
        0,
        RetryClass::RequiresEvidence,
        dispatched(DIGEST_HEX)?,
        payload()?,
    )
    .with_live_authority()
    .with_evidence(EffectEvidence::Applied);

    assert_eq!(
        retry_admissible(&facts),
        RetryDecision::Refused(vec![RetryRefusal::NotProvenUndone])
    );
    Ok(())
}

#[test]
fn every_failing_conjunct_is_reported() -> TestResult {
    let facts = RetryFacts::new(
        RetryPolicy::new(3, Duration::ZERO, Duration::MAX),
        3,
        RetryClass::RequiresEvidence,
        dispatched(DIGEST_HEX)?,
        other_payload()?,
    );

    let decision = retry_admissible(&facts);
    assert_eq!(
        decision,
        RetryDecision::Refused(vec![
            RetryRefusal::BudgetExhausted {
                attempts_used: 3,
                max_attempts: 3,
            },
            RetryRefusal::AuthorityNotLive,
            RetryRefusal::IntentChanged,
            RetryRefusal::NotProvenUndone,
        ])
    );
    assert_eq!(decision.refusals().len(), 4);
    Ok(())
}

#[test]
fn a_scope_label_is_bounded_rather_than_truncated() -> TestResult {
    assert_eq!(DedupScope::new("tenant-7")?.as_str(), "tenant-7");
    let long = "x".repeat(MAX_SCOPE_BYTES + 1);
    let refused = DedupScope::new(long);
    assert!(
        matches!(refused, Err(ref error) if error.observed_len() == MAX_SCOPE_BYTES + 1),
        "an over-long scope is refused, naming its length: {refused:?}"
    );
    let longest = "x".repeat(MAX_SCOPE_BYTES);
    assert_eq!(DedupScope::new(longest)?.as_str().len(), MAX_SCOPE_BYTES);
    Ok(())
}

#[test]
fn a_permitted_decision_reports_no_refusals() -> TestResult {
    let decision = retry_admissible(&permissive(RetryClass::Safe)?);
    assert!(decision.refusals().is_empty());
    Ok(())
}

#[test]
fn the_refusal_renders_the_numbers_a_caller_needs() -> TestResult {
    let rendered = RetryDecision::Refused(vec![
        RetryRefusal::BudgetExhausted {
            attempts_used: 3,
            max_attempts: 3,
        },
        RetryRefusal::AuthorityNotLive,
    ])
    .to_string();
    assert!(rendered.contains("3 of 3 attempts used"), "{rendered}");
    assert!(rendered.contains("authority"), "{rendered}");
    Ok(())
}
