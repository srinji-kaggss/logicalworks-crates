//! Repaired #160 S1/S2/S4 contract, pinned to the probes that failed on the
//! unrepaired tree.
//!
//! Every test here is the exact inverse of a `baseline_*` probe that passed
//! against `main` at `4073e73f` and inverted with this repair. They drive the
//! public API of the shipped crate.

use lgwks_std::similarity::{
    BoundedJaccard, CheckedEvidence, CheckedSimilarity, Cosine, EditDistance, EditDistanceError,
    EvidenceError, Similarity, Weighted, WeightedError,
};
use std::error::Error;

/// Asserts a score's bit pattern equals `expected`'s exactly, with a reason.
///
/// `clippy::float_cmp` is forbidden estate-wide and cannot be lowered, so the
/// exact comparison is made on the bit pattern instead. Every value asserted
/// here is a contract constant (`0.0`, `0.5`, `1.0`, `-1.0`, and one derived
/// ratio), and the equality *is* the claim; `1.0 - 11.0/22.0` is also exact in
/// binary floating point, so the bit comparison is not stricter than a
/// tolerance it would need.
fn assert_exact(actual: f64, expected: f64, reason: &str) {
    assert!(
        actual.to_bits() == expected.to_bits(),
        "{reason}: expected exactly {expected}, got {actual}"
    );
}

/// Asserts a `Result` is `Err` with exactly `expected`, and says why.
///
/// `clippy::panic` and `clippy::unwrap_used` are forbidden in test code too, so
/// the check is a single `assert_eq!` against the expected error rather than an
/// unwrap followed by a comparison.
fn assert_error<T, E: std::fmt::Debug + PartialEq>(
    result: Result<T, E>,
    expected: E,
    reason: &str,
) {
    assert_eq!(result.map(|_| ()).err(), Some(expected), "{reason}");
}

#[test]
fn cosine_trait_impl_stays_inside_the_documented_unit_interval() -> Result<(), EvidenceError> {
    let cosine = Cosine::new();
    // Raw mathematical cosine keeps its [-1, 1] domain.
    assert_exact(
        cosine.try_score(&[1.0_f32], &[-1.0_f32])?,
        -1.0,
        "raw cosine keeps its mathematical domain",
    );
    // The shared trait contract reads it through the named mapping, so an
    // opposite pair is 0.0 rather than -1.0.
    assert_exact(
        Similarity::score(&cosine, &[1.0_f32], &[-1.0_f32]),
        0.0,
        "S1 repaired: the Similarity impl for Cosine no longer leaves [0,1]",
    );
    assert_exact(
        cosine.normalized_score(&[1.0_f32, 0.0], &[0.0, 1.0])?,
        0.5,
        "an orthogonal pair maps to the midpoint of the unit interval",
    );
    assert_exact(
        Similarity::score(&cosine, &[1.0_f32, 0.0], &[0.0, 1.0]),
        0.5,
        "the trait form uses the same named mapping",
    );
    assert_exact(
        Similarity::score(&cosine, &[3.0_f32, 4.0], &[6.0, 8.0]),
        1.0,
        "an identical direction still reads as identity",
    );
    Ok(())
}

#[test]
fn identical_over_limit_inputs_are_refused_rather_than_scored_zero() {
    let scorer = EditDistance::new(3);
    // The infallible trait form remains lossy for source compatibility.
    assert_exact(
        scorer.score("four", "four"),
        0.0,
        "the infallible adapter still maps a refusal to 0.0",
    );
    // The checked form is the one that distinguishes refusal from measurement.
    assert_eq!(
        scorer.try_score("four", "four"),
        Err(EditDistanceError::InputTooLong {
            maximum: 3,
            observed: 4
        }),
        "S1 repaired: identical inputs over budget are refused, not scored 0.0"
    );
}

#[test]
fn a_refused_component_is_not_accepted_at_threshold_zero() -> Result<(), Box<dyn Error>> {
    for threshold in [0.0, 0.5, 1.0] {
        let policy =
            CheckedEvidence::new(vec![Box::new(EditDistance::new(3))], vec![1.0], threshold)?;
        let verdict = policy.verdict("four", "four")?;
        assert!(
            !verdict.is_accepted(),
            "S2 repaired: refused evidence is not accepted at threshold {threshold}"
        );
        assert_eq!(
            verdict.score(),
            None,
            "S2 repaired: a refusal is reported as no score at threshold {threshold}"
        );
        let refusals = verdict.refusals();
        assert_eq!(
            refusals.len(),
            1,
            "S2 repaired: exactly one component refused at threshold {threshold}"
        );
        assert_eq!(
            refusals[0].reason().cloned(),
            Some(EvidenceError::InputTooLong {
                maximum: 3,
                observed: 4
            }),
            "the typed refusal reason reaches the verdict"
        );
    }
    Ok(())
}

#[test]
fn all_zero_weight_refuses_regardless_of_threshold() -> Result<(), Box<dyn Error>> {
    // Weight totals 0, 0.5, 1 and above 1 have exact outcomes.
    assert_error(
        CheckedEvidence::<str>::new(vec![Box::new(EditDistance::new(16))], vec![1.5], 0.0),
        WeightedError::WeightSumExceedsOne,
        "a weight total above 1.0 is refused at construction",
    );
    assert_error(
        CheckedEvidence::<str>::new(vec![Box::new(EditDistance::new(16))], vec![1.0], 1.5),
        WeightedError::InvalidThreshold,
        "a threshold above 1.0 is refused at construction",
    );
    assert_error(
        CheckedEvidence::<str>::new(vec![Box::new(EditDistance::new(16))], vec![-0.1], 0.0),
        WeightedError::InvalidWeight { index: 0 },
        "an invalid weight names its component index",
    );
    assert_error(
        CheckedEvidence::<str>::new(Vec::new(), Vec::new(), 0.0),
        WeightedError::Empty,
        "an empty component set is refused at construction",
    );
    assert_error(
        CheckedEvidence::<str>::new(
            vec![
                Box::new(EditDistance::new(16)),
                Box::new(EditDistance::new(16)),
            ],
            vec![1.0],
            0.0,
        ),
        WeightedError::WeightCountMismatch {
            scorers: 2,
            weights: 1,
        },
        "a parallel-list mismatch names both counts",
    );

    // Weight total exactly zero: an explicit insufficiency, never a score.
    let zero = CheckedEvidence::new(vec![Box::new(EditDistance::new(16))], vec![0.0], 0.0)?;
    assert_error(
        zero.verdict("a", "a"),
        EvidenceError::InsufficientEvidence {
            total_weight: 0.0_f64.to_bits(),
        },
        "S2 repaired: all-zero effective evidence refuses regardless of threshold",
    );

    // Weight total 0.5 with identical inputs: the evidence deficit is visible,
    // not disguised as the trait's identity score of 1.0.
    let half = CheckedEvidence::new(vec![Box::new(EditDistance::new(16))], vec![0.5], 0.4)?;
    let verdict = half.verdict("a", "a")?;
    assert_exact(
        verdict.score().unwrap_or(-1.0),
        0.5,
        "S1 repaired: a weighted evidence deficit is reported as 0.5, not identity",
    );
    assert!(
        verdict.is_accepted(),
        "a measured 0.5 clears a measured 0.4"
    );

    // Weight total 1.0 with identical inputs reaches identity.
    let full = CheckedEvidence::new(vec![Box::new(EditDistance::new(16))], vec![1.0], 1.0)?;
    let verdict = full.verdict("a", "a")?;
    assert_exact(
        verdict.score().unwrap_or(-1.0),
        1.0,
        "weight one restores identity",
    );
    assert!(verdict.is_accepted(), "identity clears threshold 1.0");

    // Threshold 0.0 with a measured zero accepts, because this is a measurement.
    let zero_threshold =
        CheckedEvidence::new(vec![Box::new(EditDistance::new(16))], vec![1.0], 0.0)?;
    let verdict = zero_threshold.verdict("abc", "xyz")?;
    assert_exact(
        verdict.score().unwrap_or(-1.0),
        0.0,
        "a genuine measured zero is reported as a score",
    );
    assert!(
        verdict.is_accepted(),
        "a measured zero satisfies a measured zero threshold"
    );
    Ok(())
}

#[test]
fn typed_refusals_carry_component_identity_through_composition() -> Result<(), Box<dyn Error>> {
    let policy = CheckedEvidence::new(vec![Box::new(Cosine::new())], vec![0.6], 0.0)?;

    for (left, right, expected) in [
        (
            &[1.0_f32, 0.0][..],
            &[1.0_f32, 0.0, 0.0][..],
            EvidenceError::DimensionMismatch { left: 2, right: 3 },
        ),
        (
            &[0.0_f32, 0.0][..],
            &[1.0_f32, 0.0][..],
            EvidenceError::ZeroMagnitude,
        ),
        (
            &[f32::NAN, 1.0][..],
            &[1.0_f32, 1.0][..],
            EvidenceError::NonFinite,
        ),
        (
            &[f32::INFINITY, 1.0][..],
            &[1.0_f32, 1.0][..],
            EvidenceError::NonFinite,
        ),
    ] {
        let verdict = policy.verdict(left, right)?;
        assert_eq!(
            verdict.score(),
            None,
            "a refused component produces no composed score: {expected:?}"
        );
        assert!(
            !verdict.is_accepted(),
            "a refused component is not accepted at threshold zero: {expected:?}"
        );
        let refusals = verdict.refusals();
        assert_eq!(
            refusals.len(),
            1,
            "exactly one component refused: {expected:?}"
        );
        assert_eq!(refusals[0].index(), 0, "the refusal names its component");
        assert_eq!(
            refusals[0].reason().cloned(),
            Some(expected),
            "the typed reason survives composition unchanged"
        );
    }

    // The refused weight is not redistributed onto a surviving neighbour, so
    // a structural neighbour cannot acquire a missing identity field's
    // authority: one component refusing withdraws the whole verdict even though
    // its neighbour measured a perfect structural match.
    let mixed = CheckedEvidence::new(
        vec![Box::new(Cosine::new()), Box::new(StructuralMatch)],
        vec![0.5, 0.5],
        0.0,
    )?;
    let verdict = mixed.verdict(&[f32::NAN, 1.0], &[1.0, 1.0])?;
    assert_eq!(
        verdict.score(),
        None,
        "one refused component withdraws the whole verdict; no renormalization"
    );
    let refusals = verdict.refusals();
    assert_eq!(refusals.len(), 1, "only the non-finite identity refused");
    assert_eq!(refusals[0].index(), 0, "the refusal names component 0");
    assert_exact(
        refusals[0].weight(),
        0.5,
        "the refused component still reports its own weight",
    );
    assert_exact(
        verdict.outcomes()[1].score().unwrap_or(-1.0),
        1.0,
        "the structural neighbour measured identity and is still reported",
    );
    assert!(
        !verdict.is_accepted(),
        "S2 repaired: a measured structural identity cannot carry a refused identity field"
    );
    Ok(())
}

/// A structural scorer that reports 1.0 for equal-length vectors and never
/// refuses, standing in for a geometry or shape component.
///
/// It exists so the composition test can show that a neighbour's perfect
/// measurement does not cover for a refused component, which is the property
/// that keeps one field from acquiring another's authority.
struct StructuralMatch;

impl CheckedSimilarity for StructuralMatch {
    type Value = [f32];

    fn try_score(&self, left: &Self::Value, right: &Self::Value) -> Result<f64, EvidenceError> {
        if left.len() != right.len() {
            return Err(EvidenceError::DimensionMismatch {
                left: left.len(),
                right: right.len(),
            });
        }
        Ok(1.0)
    }
}

#[test]
fn the_legacy_weighted_path_remains_available_and_documented_as_lossy() -> Result<(), Box<dyn Error>>
{
    // `Weighted` is still constructible and still returns a number, so a caller
    // that has not migrated keeps compiling. It is documented as the lossy
    // adapter, and the checked composition is what authority uses.
    let legacy = Weighted::<str>::new(vec![(1.0, Box::new(EditDistance::new(3)))], 0.0)?;
    assert_exact(
        legacy.score("four", "four"),
        0.0,
        "the legacy adapter still maps a refusal to 0.0, as documented",
    );
    assert!(
        legacy.is_accepted("four", "four"),
        "the legacy adapter still accepts at threshold zero, which is why it is not the authority path"
    );
    assert_exact(
        legacy.total_weight(),
        1.0,
        "the weight total is reported, not renormalized",
    );
    Ok(())
}

#[test]
fn bounded_jaccard_refuses_before_the_quadratic_scan() {
    let scorer = BoundedJaccard::<u32>::new(4);
    assert_eq!(
        scorer.maximum_length(),
        4,
        "the declared budget is readable"
    );
    assert_exact(
        CheckedSimilarity::try_score(&scorer, &[1, 2, 3, 4], &[4, 3, 2, 1]).unwrap_or(-1.0),
        1.0,
        "a within-budget set pair scores exactly",
    );
    assert_eq!(
        CheckedSimilarity::try_score(&scorer, &[1, 2, 3, 4, 5], &[1, 2, 3, 4]),
        Err(EvidenceError::CollectionTooLong {
            maximum: 4,
            observed: 5
        }),
        "S4 repaired: an over-budget set is refused with its limit"
    );
    assert_eq!(
        CheckedSimilarity::try_score(&scorer, &[], &[]),
        Ok(1.0),
        "empty inputs are handled before the budget"
    );
    assert_exact(
        Similarity::score(&scorer, &[1, 2, 3], &[3, 2, 1]),
        1.0,
        "set membership ignores order and duplicates",
    );
}

#[test]
fn budget_refusal_precedes_amplification_and_is_measurable() {
    // A large set is refused by length alone; the quadratic scan is never
    // entered. The assertion is on the growth ratio of a whole timed loop, not
    // of a per-call figure: a refusal costs about a nanosecond, so a per-call
    // reading floored to integer nanoseconds is 1 or 3 by timer noise alone
    // (CI saw "1ns -> 3ns" on an unchanged path). Each size takes the fastest of
    // `TRIALS` loops of `REPEATS` calls, so one preemption cannot be read as
    // growth, and every loop runs long enough for the timer to resolve it.
    const REPEATS: u64 = 200_000;
    const TRIALS: usize = 5;
    let scorer = BoundedJaccard::<u32>::new(16);
    let mut previous = 0_u128;
    for size in [2_000_usize, 4_000, 8_000] {
        let big: Vec<u32> = (0..size)
            .map(|value| u32::try_from(value % 4_000).unwrap_or(0))
            .collect();
        assert_eq!(
            CheckedSimilarity::try_score(&scorer, &big, &big),
            Err(EvidenceError::CollectionTooLong {
                maximum: 16,
                observed: size
            }),
            "an over-budget set of {size} is refused with its limit"
        );
        let mut fastest = u128::MAX;
        for _ in 0..TRIALS {
            let start = std::time::Instant::now();
            let mut refusals_seen = 0_u64;
            for _ in 0..REPEATS {
                // The verdict is consumed rather than discarded: `let _ =` is
                // forbidden by `clippy::let_underscore_must_use`. This measures
                // the refusal path, not the answer; the answer was asserted
                // above. `black_box` keeps the input opaque so the loop is not
                // folded into one call.
                let input = std::hint::black_box(big.as_slice());
                if let Err(reason) = CheckedSimilarity::try_score(&scorer, input, input) {
                    refusals_seen = refusals_seen.saturating_add(u64::from(matches!(
                        reason,
                        EvidenceError::CollectionTooLong { .. }
                    )));
                }
            }
            assert_eq!(
                refusals_seen, REPEATS,
                "every timed call refused with the budget error"
            );
            fastest = fastest.min(start.elapsed().as_nanos());
        }
        if previous > 0 {
            assert!(
                fastest < previous.saturating_mul(3),
                "S4: doubling the input must not quadruple the refusal work: \
                 {previous}ns -> {fastest}ns for {REPEATS} refusals"
            );
        }
        previous = fastest;
    }
}

#[test]
fn the_heuristic_path_score_is_never_an_exact_match_proof() {
    use lgwks_std::similarity::{PathSimilarity, is_exact_path_match};

    let scorer = PathSimilarity::new();
    assert_exact(
        scorer.score("root/child/1", "root/child/999"),
        1.0,
        "the documented lossy normalization collides",
    );
    assert!(
        !is_exact_path_match("root/child/1", "root/child/999"),
        "S4 repaired: the heuristic score is not reachable as an exact-match proof"
    );
    assert!(
        is_exact_path_match("root/child/1", "root/child/1"),
        "an identical path is an exact match"
    );
}

#[test]
fn the_edit_budget_charges_the_normalized_unit_not_the_raw_scalar_count() {
    let scorer = EditDistance::new(1);
    // `İ` lowercases to two scalars, so the raw scalar count is 1 while the
    // normalized count is 2. Only the normalized unit is the budget.
    assert_eq!(
        scorer.normalized_length("i"),
        1,
        "an ASCII scalar normalizes to itself"
    );
    assert_eq!(
        scorer.normalized_length("İ"),
        2,
        "S4 repaired: lower-case expansion is visible in the charged unit"
    );
    assert_eq!(
        scorer.try_score("İ", "i"),
        Err(EditDistanceError::InputTooLong {
            maximum: 1,
            observed: 2
        }),
        "S4 repaired: the expanded scalar is refused against the normalized budget"
    );
    // With a budget that fits the expansion, the pair is a real comparison:
    // `İ` normalizes to two scalars against `i`'s one, so one edit over a
    // maximum of two is exactly 0.5. The expansion is what changed the answer.
    let roomy = EditDistance::new(2);
    assert_exact(
        roomy.try_score("İ", "i").unwrap_or(-1.0),
        0.5,
        "S4 repaired: the expansion is charged, giving distance 1 over length 2",
    );
}

#[test]
fn unequal_length_inputs_use_the_shorter_dp_dimension_without_changing_the_score()
-> Result<(), EditDistanceError> {
    let scorer = EditDistance::new(64);
    let left = "abcdefghij";
    // Normalization collapses the space, so the right side is 21 scalars and
    // the appended suffix costs 11 edits: 1 - 11/21.
    let right = "abcdefghij-extra tail";
    let score = scorer.try_score(left, right)?;
    let reversed = scorer.try_score(right, left)?;
    assert_eq!(
        score.to_bits(),
        reversed.to_bits(),
        "edit similarity is symmetric across the row/column swap"
    );
    assert_exact(
        score,
        1.0 - (11.0 / 21.0),
        "distance 11 over the longer normalized length 21 gives the declared ratio",
    );
    assert_exact(
        scorer.try_score("", "")?,
        1.0,
        "empty inputs score identity",
    );
    assert_exact(
        scorer.try_score("", "x")?,
        0.0,
        "one empty side scores zero",
    );
    assert_exact(
        scorer.try_score("x", "")?,
        0.0,
        "one empty side scores zero",
    );
    Ok(())
}

#[test]
fn reordered_and_duplicated_set_members_score_exactly() {
    let scorer = lgwks_std::similarity::Jaccard::<u32>::new();
    let pair = |left: &[u32], right: &[u32]| scorer.score(left, right);
    assert_exact(pair(&[], &[]), 1.0, "two empty sets are identity");
    assert_exact(pair(&[], &[1]), 0.0, "one empty side is zero");
    assert_exact(pair(&[1, 2], &[1, 2]), 1.0, "equal sets are identity");
    assert_exact(pair(&[1, 2], &[2, 1]), 1.0, "set membership ignores order");
    assert_exact(
        pair(&[1, 1, 2], &[2, 1]),
        1.0,
        "duplicate entries collapse to one set member",
    );
    assert_exact(
        pair(&[1, 2], &[1, 3]),
        1.0 / 3.0,
        "one shared member out of a three-member union",
    );
}
