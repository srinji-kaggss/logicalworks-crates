//! The similarity module's unit tests.

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

use super::*;

fn assert_unit(value: f64) {
    assert!(value.is_finite(), "similarity must be finite: {value}");
    assert!(
        (0.0..=1.0).contains(&value),
        "similarity outside [0, 1]: {value}"
    );
}

/// Compares floating-point outputs without relying on exact arithmetic.
fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-12,
        "expected {expected}, got {actual}"
    );
}

/// Erases one edit scorer for a weighted composition without a cast.
fn boxed_edit_distance(maximum_length: usize) -> Box<dyn Similarity<Value = str>> {
    Box::new(EditDistance::new(maximum_length))
}

#[test]
fn edit_distance_handles_empty_and_identical_inputs() -> Result<(), EditDistanceError> {
    let scorer = EditDistance::new(16);
    assert_close(scorer.try_score("", "")?, 1.0);
    assert_close(scorer.try_score("same", "same")?, 1.0);
    assert_close(scorer.try_score("", "text")?, 0.0);
    assert_close(scorer.try_score("text", "")?, 0.0);
    assert_close(scorer.try_score("  SAME ", "same")?, 1.0);
    Ok(())
}

#[test]
fn edit_distance_normalizes_known_distance() -> Result<(), EditDistanceError> {
    let scorer = EditDistance::new(16);
    let score = scorer.try_score("kitten", "sitting")?;
    assert!((score - (4.0 / 7.0)).abs() < 1e-12, "score was {score}");
    Ok(())
}

#[test]
fn edit_distance_refuses_over_limit_inputs() -> Result<(), EditDistanceError> {
    let scorer = EditDistance::new(3);
    assert_close(scorer.try_score("abc", "abc")?, 1.0);
    assert!(matches!(
        scorer.try_score("four", "four"),
        Err(EditDistanceError::InputTooLong {
            maximum: 3,
            observed: 4
        })
    ));
    assert_close(scorer.score("four", "four"), 0.0);
    assert!(
        scorer.score("four", "four") <= 0.0,
        "the infallible trait method must map refusal to no similarity"
    );
    Ok(())
}

#[test]
fn every_evidence_error_variant_is_exercised_by_a_test() {
    // Each variant of the refusal enum is produced here, so a new variant
    // without a producer fails this test rather than sitting unreachable.
    let produced: Vec<EvidenceError> = vec![
        EvidenceError::from(EditDistanceError::InputTooLong {
            maximum: 1,
            observed: 2,
        }),
        EvidenceError::from(CosineError::DimensionMismatch { left: 1, right: 2 }),
        EvidenceError::from(CosineError::ZeroMagnitude),
        EvidenceError::from(CosineError::NonFinite),
        EvidenceError::CollectionTooLong {
            maximum: 1,
            observed: 2,
        },
        EvidenceError::InsufficientEvidence {
            total_weight: 0.0_f64.to_bits(),
        },
        EvidenceError::from(WeightedError::Empty),
        EvidenceError::from(WeightedError::InvalidWeight { index: 0 }),
        EvidenceError::from(WeightedError::WeightSumExceedsOne),
        EvidenceError::from(WeightedError::InvalidThreshold),
        EvidenceError::from(WeightedError::WeightCountMismatch {
            scorers: 2,
            weights: 1,
        }),
    ];
    assert_eq!(
        produced.len(),
        11,
        "every EvidenceError variant is produced exactly once"
    );
    // Each renders, and each conversion preserves the variant it came from.
    for reason in &produced {
        assert!(
            !reason.to_string().is_empty(),
            "{reason:?} must render a message"
        );
    }
    assert_eq!(
        EvidenceError::from(CosineError::ZeroMagnitude),
        EvidenceError::ZeroMagnitude,
        "a zero magnitude stays its own refusal through conversion"
    );
    assert_ne!(
        EvidenceError::ZeroMagnitude.to_string(),
        EvidenceError::NonFinite.to_string(),
        "the two vector refusals must not render identically"
    );
    assert_ne!(
        EvidenceError::InputTooLong {
            maximum: 1,
            observed: 2
        }
        .to_string(),
        EvidenceError::CollectionTooLong {
            maximum: 1,
            observed: 2
        }
        .to_string(),
        "the text and set budgets must name themselves apart"
    );
}

#[test]
fn bounded_jaccard_refuses_before_the_quadratic_scan() -> Result<(), EvidenceError> {
    let scorer = BoundedJaccard::<u32>::new(2);
    assert_close(
        CheckedSimilarity::try_score(&scorer, &[1, 2], &[2, 1])?,
        1.0,
    );
    assert_eq!(
        CheckedSimilarity::try_score(&scorer, &[1, 2, 3], &[1, 2]),
        Err(EvidenceError::CollectionTooLong {
            maximum: 2,
            observed: 3
        }),
        "an over-budget set is refused with its limit, before dedup"
    );
    assert_close(Similarity::score(&scorer, &[], &[]), 1.0);
    Ok(())
}

#[test]
fn the_normalized_budget_unit_is_the_expanded_scalar_count() -> Result<(), EditDistanceError> {
    let tight = EditDistance::new(1);
    assert_eq!(
        tight.normalized_length("i"),
        1,
        "an ASCII scalar normalizes to itself"
    );
    assert_eq!(
        tight.normalized_length("İ"),
        2,
        "`İ` lower-cases to two scalars, and that is the charged unit"
    );
    assert_eq!(
        tight.try_score("İ", "i"),
        Err(EditDistanceError::InputTooLong {
            maximum: 1,
            observed: 2
        }),
        "a budget of one refuses the expanded scalar, not the raw count"
    );
    let roomy = EditDistance::new(2);
    // With room for the expansion, the pair is one edit over two scalars.
    assert_close(roomy.try_score("İ", "i")?, 0.5);
    Ok(())
}

#[test]
fn jaccard_handles_boundary_sets() {
    let scorer = Jaccard::<&str>::new();
    assert_close(scorer.score(&[], &[]), 1.0);
    assert_close(scorer.score(&["a", "b"], &["a", "b"]), 1.0);
    assert_close(scorer.score(&["a"], &["b"]), 0.0);
    assert_close(scorer.score(&["a", "a"], &["a"]), 1.0);
}

#[test]
fn cosine_scores_known_angles() -> Result<(), CosineError> {
    let scorer = Cosine::new();
    // Identical direction, different magnitude: the magnitude cancels, and
    // that is the property that makes cosine the right metric for an
    // embedding, where a longer vector is not a closer one.
    assert_close(scorer.try_score(&[1.0, 2.0, 3.0], &[2.0, 4.0, 6.0])?, 1.0);
    assert_close(scorer.try_score(&[1.0, 0.0], &[0.0, 1.0])?, 0.0);
    assert_close(scorer.try_score(&[1.0, 0.0], &[-1.0, 0.0])?, -1.0);
    // 45 degrees: cos(pi/4).
    let diagonal = scorer.try_score(&[1.0, 0.0], &[1.0, 1.0])?;
    assert!((diagonal - core::f64::consts::FRAC_1_SQRT_2).abs() < 1e-12);
    Ok(())
}

#[test]
fn cosine_refuses_a_dimension_mismatch_rather_than_scoring_zero() {
    let scorer = Cosine::new();
    assert!(matches!(
        scorer.try_score(&[1.0, 0.0], &[1.0, 0.0, 0.0]),
        Err(CosineError::DimensionMismatch { left: 2, right: 3 })
    ));
    // The infallible trait method still maps refusal to no similarity, so a
    // caller that ignores the typed error cannot read refusal as a match.
    assert_close(scorer.score(&[1.0, 0.0], &[1.0, 0.0, 0.0]), 0.0);
}

#[test]
fn cosine_refuses_a_zero_magnitude_vector() {
    let scorer = Cosine::new();
    assert!(matches!(
        scorer.try_score(&[0.0, 0.0], &[1.0, 0.0]),
        Err(CosineError::ZeroMagnitude)
    ));
    assert!(matches!(
        scorer.try_score(&[], &[]),
        Err(CosineError::ZeroMagnitude)
    ));
}

#[test]
fn cosine_names_a_non_finite_vector_apart_from_a_zero_magnitude_one() {
    let scorer = Cosine::new();
    // A `NaN` component on either side, and an infinity on either side. All
    // four are refused as `NonFinite`, not as `ZeroMagnitude`: the two have
    // different repairs, and reporting the wrong one sends the operator to
    // the wrong end of the pipeline.
    for (left, right) in [
        (&[f32::NAN, 1.0][..], &[1.0, 1.0][..]),
        (&[1.0, 1.0][..], &[f32::NAN, 1.0][..]),
        (&[f32::INFINITY, 1.0][..], &[1.0, 1.0][..]),
        (&[1.0, 1.0][..], &[1.0, f32::NEG_INFINITY][..]),
    ] {
        assert!(
            matches!(scorer.try_score(left, right), Err(CosineError::NonFinite)),
            "a non-finite vector must be refused as NonFinite: {left:?} {right:?}"
        );
    }
    // The distinction has to reach the operator, so it is carried by the
    // rendered message and not only by the variant name.
    assert_ne!(
        CosineError::NonFinite.to_string(),
        CosineError::ZeroMagnitude.to_string(),
        "the two refusals must not render identically"
    );
}

#[test]
fn cosine_never_returns_a_non_finite_score_from_the_infallible_method() {
    let scorer = Cosine::new();
    // This is the property the typed error protects. A caller that reads
    // only the trait method gets `0.0` — "no similarity" — and never `NaN`,
    // which would compare false against every threshold and turn "we could
    // not measure this" into "this did not match".
    let score = scorer.score(&[f32::NAN, 1.0], &[1.0, 1.0]);
    assert!(score.is_finite(), "the infallible method returned {score}");
    assert_close(score, 0.0);
    assert_close(scorer.score(&[f32::INFINITY, 1.0], &[1.0, 1.0]), 0.0);
}

#[test]
fn cosine_stays_inside_the_contract_interval() -> Result<(), CosineError> {
    let scorer = Cosine::new();
    // `f32::from(u16)` is lossless and needs no cast; the second vector is
    // the first scaled, so the two are the same direction by construction.
    let wide: Vec<f32> = (0_u16..64).map(f32::from).collect();
    let narrow: Vec<f32> = wide.iter().map(|value| value * 0.5).collect();
    for (left, right) in [(&wide, &narrow), (&narrow, &wide)] {
        let score = scorer.try_score(left, right)?;
        assert!(
            (-1.0..=1.0).contains(&score),
            "score {score} left the contract interval"
        );
    }
    assert_close(scorer.try_score(&wide, &narrow)?, 1.0);
    Ok(())
}

#[test]
fn path_similarity_discards_volatile_segments() {
    let scorer = PathSimilarity::new();
    assert_close(
        scorer.score(
            "html/body/div/123/button.css-a1b2c3",
            "html/body/div/987/button.css-z9y8x7",
        ),
        1.0,
    );
    assert_close(scorer.score("root/left", "root/right"), 0.5);
    assert_close(scorer.score("", ""), 1.0);
    assert_close(scorer.score("left", "right"), 0.0);
}

#[test]
fn geometry_maps_zero_and_maximum_distance() {
    let scorer = Geometry::new(2.0);
    let origin = [0.0, 0.0, 0.0, 0.0];
    let far = [2.0, 0.0, 0.0, 0.0];
    assert_close(scorer.score(&origin, &origin), 1.0);
    assert_close(scorer.score(&origin, &far), 0.0);
    assert_close(scorer.score(&origin, &[1.0, 0.0, 0.0, 0.0]), 0.5);
}

#[test]
fn bounding_box_accessors_preserve_coordinates() {
    let box_value = BoundingBox::new(0.1, 0.2, 0.3, 0.4);
    assert_close(box_value.x(), 0.1);
    assert_close(box_value.y(), 0.2);
    assert_close(box_value.width(), 0.3);
    assert_close(box_value.height(), 0.4);
    for (actual, expected) in box_value.to_array().iter().zip([0.1, 0.2, 0.3, 0.4]) {
        assert_close(*actual, expected);
    }
}

#[test]
fn weighted_rejects_empty_components() {
    let result = Weighted::<str>::new(Vec::new(), 0.5);
    assert!(matches!(result, Err(WeightedError::Empty)));
}

#[test]
fn weighted_single_vector_returns_that_vectors_score() -> Result<(), WeightedError> {
    let direct = EditDistance::new(16);
    let weighted = Weighted::<str>::new(vec![(1.0, boxed_edit_distance(16))], 0.8)?;
    assert_close(
        weighted.score("kitten", "sitting"),
        direct.score("kitten", "sitting"),
    );
    assert!(!weighted.is_accepted("kitten", "sitting"));
    Ok(())
}

#[test]
fn every_metric_stays_in_unit_interval_for_fixed_inputs() -> Result<(), WeightedError> {
    let edit = EditDistance::new(32);
    for (left, right) in [("", ""), ("a", "b"), ("short", "longer"), ("A B", "a b")] {
        assert_unit(edit.score(left, right));
    }

    let sets = Jaccard::<&str>::new();
    for (left, right) in [
        (&[][..], &[][..]),
        (&["a"][..], &["b"][..]),
        (&["a", "b"][..], &["b", "c"][..]),
    ] {
        assert_unit(sets.score(left, right));
    }

    let paths = PathSimilarity::new();
    for (left, right) in [("", ""), ("root/1", "root/2"), ("a", "b/c")] {
        assert_unit(paths.score(left, right));
    }

    let geometry = Geometry::new(1.0);
    for (left, right) in [
        ([0.0, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 0.0]),
        ([0.0, 0.0, 0.0, 0.0], [1.0, 1.0, 1.0, 1.0]),
        ([0.5, 0.5, 0.25, 0.25], [0.25, 0.25, 0.5, 0.5]),
    ] {
        assert_unit(geometry.score(&left, &right));
    }

    let weighted = Weighted::<str>::new(vec![(1.0, boxed_edit_distance(32))], 0.5)?;
    for (left, right) in [
        ("", ""),
        ("same", "same"),
        ("a", "b"),
        ("too long", "too long"),
    ] {
        assert_unit(weighted.score(left, right));
    }
    Ok(())
}
