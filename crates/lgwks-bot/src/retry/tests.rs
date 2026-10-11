//! RQ-009's rule, exercised one conjunct at a time.

use std::num::NonZeroU128;

use lgwks_std::hash::blake3;

use super::*;
use crate::effect::{
    ActionId, AttemptId, EffectIdentity, EnvironmentEpoch, EnvironmentId, FlowRevision, Id128,
    RunId,
};

/// A distinct identifier per role. The rule compares identities only for
/// equality, so each is a small number rather than a wire spelling: parsing the
/// wire form is `effect`'s subject, and a parse here could only fail a test for
/// a reason the rule has nothing to do with.
const fn id(offset: u128) -> Id128 {
    Id128::from_nonzero(NonZeroU128::MIN.saturating_add(offset))
}

const RUN: Id128 = id(0);
const ACTION: Id128 = id(1);
const OTHER_ACTION: Id128 = id(2);
const ENVIRONMENT: Id128 = id(3);

/// Tests that build a [`DedupScope`] propagate its refusal rather than unwrap.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The payload binding of the shared attempt.
fn payload() -> ActionDigest {
    ActionDigest::new(blake3(b"the shared attempt's payload"))
}

/// A payload binding that differs from [`payload`].
fn other_payload() -> ActionDigest {
    ActionDigest::new(blake3(b"a different payload"))
}

/// The first attempt on the shared action, dispatched under [`payload`].
fn dispatched() -> EffectKey {
    EffectIdentity::new(
        RunId::new(RUN),
        EnvironmentId::new(ENVIRONMENT),
        FlowRevision::new(blake3(b"the flow document")),
    )
    .key(
        ActionId::new(ACTION),
        AttemptId::FIRST,
        payload(),
        EnvironmentEpoch::FIRST,
    )
}

/// Three attempts, no wait, no deadline: the policy no conjunct below varies
/// unless its test says so.
fn policy() -> RetryPolicy {
    RetryPolicy::new(3, Duration::ZERO, Duration::MAX)
}

/// An unresolved attempt under a live authority and nothing else claimed:
/// zero attempts made, zero elapsed, and the shared payload.
///
/// The subject the contract tests below all start from. Built once because
/// they differ from each other in exactly one clause — the contract, its
/// window, or its late-arrival behaviour — and a fixture each of them wrote
/// out by hand is how two of them come to differ in a second one and stop
/// testing what their names say.
fn unresolved() -> RetryFacts {
    RetryFacts::new(
        policy(),
        0,
        RetryClass::RequiresEvidence,
        dispatched(),
        payload(),
    )
    .with_live_authority()
}

/// Facts that permit a retry, so each test changes exactly one thing.
fn permissive(class: RetryClass) -> RetryFacts {
    RetryFacts::new(policy(), 0, class, dispatched(), payload())
        .with_live_authority()
        .with_evidence(EffectEvidence::NotApplied)
}

/// A contract over `action` and `binding`, with the given window and
/// late-arrival behaviour.
fn contract_over(
    action: Id128,
    binding: ActionDigest,
    retention: Duration,
    late_arrival: LateArrival,
) -> Result<DeduplicationContract, Box<dyn std::error::Error>> {
    Ok(DeduplicationContract::new(
        ActionId::new(action),
        binding,
        DedupScope::new("tenant-7")?,
        retention,
        late_arrival,
    ))
}

/// A contract over the shared action and payload.
fn contract(
    retention: Duration,
    late_arrival: LateArrival,
) -> Result<DeduplicationContract, Box<dyn std::error::Error>> {
    contract_over(ACTION, payload(), retention, late_arrival)
}

#[test]
fn proven_non_application_is_a_plain_retry() {
    let decision = retry_admissible(&permissive(RetryClass::RequiresEvidence));
    assert_eq!(decision, RetryDecision::Safe);
    assert!(
        decision.permits_retry(),
        "a Safe decision permits the retry"
    );
}

#[test]
fn a_permanent_failure_short_circuits() {
    // Every other conjunct holds, and the class alone refuses.
    let decision = retry_admissible(&permissive(RetryClass::Never));
    assert_eq!(
        decision,
        RetryDecision::Refused(vec![RetryRefusal::PermanentFailure])
    );
    assert!(!decision.permits_retry(), "a refusal permits nothing");
}

/// The policy's deadline is a conjunct: a resend after it has passed is
/// refused with both durations named, and one inside it is not (#401).
#[test]
fn a_spent_deadline_refuses_and_names_the_durations() {
    let policy = RetryPolicy::new(3, Duration::ZERO, Duration::from_secs(30));
    let at = |elapsed| {
        let facts = RetryFacts {
            policy,
            ..permissive(RetryClass::RequiresEvidence)
        };
        retry_admissible(&facts.with_elapsed(elapsed))
    };
    assert_eq!(at(Duration::from_secs(29)), RetryDecision::Safe);
    assert_eq!(
        at(Duration::from_secs(30)),
        RetryDecision::Refused(vec![RetryRefusal::DeadlineExhausted {
            elapsed: Duration::from_secs(30),
            deadline: Duration::from_secs(30),
        }])
    );
}

#[test]
fn a_spent_budget_refuses_and_names_the_numbers() {
    let facts = RetryFacts {
        attempts_used: 3,
        ..permissive(RetryClass::RequiresEvidence)
    };
    assert_eq!(
        retry_admissible(&facts),
        RetryDecision::Refused(vec![RetryRefusal::BudgetExhausted {
            attempts_used: 3,
            max_attempts: 3,
        }])
    );
}

#[test]
fn one_attempt_policy_exhausts_after_the_first() {
    let facts = RetryFacts {
        policy: RetryPolicy::new(1, Duration::ZERO, Duration::MAX),
        attempts_used: 1,
        ..permissive(RetryClass::Safe)
    };
    assert!(
        !retry_admissible(&facts).permits_retry(),
        "a one-attempt policy has nothing left after the first"
    );
}

#[test]
fn absent_authority_refuses() {
    let mut facts = permissive(RetryClass::Safe);
    facts.authority_live = false;
    assert_eq!(
        retry_admissible(&facts),
        RetryDecision::Refused(vec![RetryRefusal::AuthorityNotLive])
    );
}

#[test]
fn authority_defaults_to_refusing() {
    // The builder is not called for authority, so nothing was established.
    let facts = RetryFacts::new(policy(), 0, RetryClass::Safe, dispatched(), payload())
        .with_evidence(EffectEvidence::NotApplied);
    assert_eq!(
        retry_admissible(&facts),
        RetryDecision::Refused(vec![RetryRefusal::AuthorityNotLive])
    );
}

#[test]
fn a_changed_intent_is_not_a_retry() {
    let facts = RetryFacts::new(policy(), 0, RetryClass::Safe, dispatched(), other_payload())
        .with_live_authority()
        .with_evidence(EffectEvidence::NotApplied);
    assert_eq!(
        retry_admissible(&facts),
        RetryDecision::Refused(vec![RetryRefusal::IntentChanged])
    );
}

#[test]
fn an_unknown_outcome_with_no_evidence_is_refused() {
    assert_eq!(
        retry_admissible(&unresolved()),
        RetryDecision::Refused(vec![RetryRefusal::NotProvenUndone])
    );
}

#[test]
fn a_gui_click_has_no_contract_to_reach_for() {
    // RQ-009: a GUI click has no generic deduplication contract, so an
    // unsettled click is never retried on the contract half of the
    // disjunct. Nothing here can express otherwise.
    let facts = unresolved().with_elapsed(Duration::from_secs(1));
    assert_eq!(
        retry_admissible(&facts),
        RetryDecision::Refused(vec![RetryRefusal::NotProvenUndone])
    );
}

#[test]
fn a_contract_inside_its_window_covers_a_resend() -> TestResult {
    let facts = unresolved()
        .with_contract(contract(Duration::from_secs(600), LateArrival::Applied)?)
        .with_elapsed(Duration::from_secs(30));
    assert_eq!(retry_admissible(&facts), RetryDecision::UnderContract);
    Ok(())
}

#[test]
fn a_contract_past_its_window_is_worthless_if_a_late_duplicate_applies() -> TestResult {
    let facts = unresolved()
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
    let facts = unresolved()
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
fn a_contract_past_its_window_still_covers_when_the_remote_refuses_late_duplicates() -> TestResult {
    let facts = unresolved()
        .with_contract(contract(Duration::from_secs(600), LateArrival::Refused)?)
        .with_elapsed(Duration::from_secs(86_400));
    assert_eq!(retry_admissible(&facts), RetryDecision::UnderContract);
    Ok(())
}

#[test]
fn a_contract_for_another_payload_does_not_cover() -> TestResult {
    let facts = unresolved().with_contract(contract_over(
        ACTION,
        other_payload(),
        Duration::from_secs(600),
        LateArrival::Refused,
    )?);
    assert_eq!(
        retry_admissible(&facts),
        RetryDecision::Refused(vec![RetryRefusal::NotProvenUndone])
    );
    Ok(())
}

#[test]
fn a_contract_for_another_action_does_not_cover() -> TestResult {
    let facts = unresolved().with_contract(contract_over(
        OTHER_ACTION,
        payload(),
        Duration::from_secs(600),
        LateArrival::Refused,
    )?);
    assert!(
        !retry_admissible(&facts).permits_retry(),
        "a contract for another action covers nothing about this one"
    );
    Ok(())
}

#[test]
fn evidence_is_the_stronger_half_of_the_disjunct() -> TestResult {
    // Both halves hold, and the decision reports the one that needs no
    // contract to justify.
    let facts = permissive(RetryClass::RequiresEvidence)
        .with_contract(contract(Duration::from_secs(600), LateArrival::Applied)?);
    assert_eq!(retry_admissible(&facts), RetryDecision::Safe);
    Ok(())
}

#[test]
fn applied_evidence_is_not_proof_of_non_application() {
    let facts = unresolved().with_evidence(EffectEvidence::Applied);
    assert_eq!(
        retry_admissible(&facts),
        RetryDecision::Refused(vec![RetryRefusal::NotProvenUndone])
    );
}

#[test]
fn every_failing_conjunct_is_reported() {
    let facts = RetryFacts::new(
        policy(),
        3,
        RetryClass::RequiresEvidence,
        dispatched(),
        other_payload(),
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
}

#[test]
fn a_scope_label_is_bounded_rather_than_truncated() -> TestResult {
    assert_eq!(DedupScope::new("tenant-7")?.as_str(), "tenant-7");
    let refused = DedupScope::new("x".repeat(MAX_SCOPE_BYTES + 1));
    assert!(
        matches!(refused, Err(ref error) if error.observed_len() == MAX_SCOPE_BYTES + 1),
        "an over-long scope is refused, naming its length: {refused:?}"
    );
    let longest = "x".repeat(MAX_SCOPE_BYTES);
    assert_eq!(DedupScope::new(longest)?.as_str().len(), MAX_SCOPE_BYTES);
    Ok(())
}

#[test]
fn a_permitted_decision_reports_no_refusals() {
    let decision = retry_admissible(&permissive(RetryClass::Safe));
    assert!(
        decision.refusals().is_empty(),
        "a permitted decision carries no refusals: {decision:?}"
    );
}

#[test]
fn the_refusal_renders_the_numbers_a_caller_needs() {
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
}
