//! Public API regression tests for `lgwks_std::similarity`.

use std::sync::Arc;

use lgwks_std::similarity::{
    BoundingBox, CheckedEvidence, Cosine, CosineError, EditDistance, Geometry, Similarity,
};

/// What every fallible test here returns.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Compiles and scores each input type documented by `Geometry`.
#[test]
fn geometry_accepts_arrays_and_documented_bounding_boxes() {
    let geometry = Geometry::new(2.0);
    let array = [0.1, 0.2, 0.3, 0.4];
    let bounding_box = BoundingBox::new(0.1, 0.2, 0.3, 0.4);

    assert!(
        (geometry.score(&array, &array) - 1.0).abs() < 1e-12,
        "array inputs must score identically"
    );
    assert!(
        (geometry.score(&bounding_box, &bounding_box) - 1.0).abs() < 1e-12,
        "BoundingBox inputs must compile and score identically"
    );
    assert!(
        (Similarity::score(&geometry, &array, &array) - 1.0).abs() < 1e-12,
        "trait dispatch must preserve the array scoring contract"
    );
}

/// Requirement #526 (a doc example that named the crate moved into a test): the
/// module header's opposite pair, dimension mismatch and named normalized mapping.
#[test]
fn cosine_keeps_the_raw_domain_and_the_shared_contract_apart() -> TestResult {
    let cosine = Cosine::new();
    let raw = cosine.try_score(&[1.0_f32], &[-1.0_f32])?;
    assert!(
        (raw + 1.0).abs() < 1e-12,
        "an opposite pair is -1.0 through the raw path, got {raw}"
    );
    let shared = Similarity::score(&cosine, &[1.0_f32], &[-1.0_f32]);
    assert!(
        shared.abs() < 1e-12,
        "an opposite pair is 0.0 through the shared contract, got {shared}"
    );
    assert!(
        matches!(
            cosine.try_score(&[1.0_f32, 0.0], &[1.0_f32, 0.0, 0.0]),
            Err(CosineError::DimensionMismatch { .. })
        ),
        "a dimension mismatch is a refusal, never a score"
    );
    let normalized = cosine.normalized_score(&[1.0_f32], &[-1.0_f32])?;
    assert!(
        normalized.abs() < 1e-12,
        "the named mapping reads an opposite pair as 0.0, got {normalized}"
    );
    Ok(())
}

/// Requirement #526: an over-budget input is a refusal rather than a zero, and one
/// policy behind an `Arc` serves every caller, as `CheckedEvidence` documents.
#[test]
fn an_over_budget_input_is_a_refusal_and_one_policy_is_shared() -> TestResult {
    /// Fails to compile unless the policy is `Sync`, the bound that makes it shareable.
    fn assert_sync<T: Sync>(_value: &T) {}

    let policy = CheckedEvidence::new(vec![Box::new(EditDistance::new(3))], vec![1.0], 0.0)?;
    assert_sync(&policy);
    let verdict = policy.verdict("four", "four")?;
    assert!(
        !verdict.is_accepted(),
        "an over-budget input is not accepted at any threshold"
    );
    assert!(
        verdict.score().is_none(),
        "an over-budget input has no score, not a score of zero"
    );
    let refusals = verdict.refusals();
    let first = refusals
        .first()
        .ok_or("the refused component is listed in the verdict")?;
    assert_eq!(first.index(), 0, "the refusal names component zero");

    let shared = Arc::new(policy);
    assert!(
        shared.verdict("cat", "cat")?.is_accepted(),
        "within the budget the same policy measures and accepts"
    );
    Ok(())
}
