//! Baseline behavioural evidence for #160 S1/S2, using only API that exists on
//! the unrepaired tree. Each assertion is the defect the issue describes; the
//! repaired tree must invert it.

use lgwks_std::similarity::{Cosine, EditDistance, Similarity, Weighted};

#[test]
fn baseline_cosine_trait_impl_leaves_the_unit_interval() {
    let cosine = Cosine::new();
    let score = Similarity::score(&cosine, &[1.0_f32], &[-1.0_f32]);
    assert!(
        !(0.0..=1.0).contains(&score),
        "BASELINE DEFECT S1: the Similarity impl for Cosine returns {score} for an
         opposite pair, outside the [0,1] the trait documents. Expected on main."
    );
}

#[test]
fn baseline_a_refused_component_is_accepted_at_threshold_zero() {
    let weighted = Weighted::<str>::new(vec![(1.0, Box::new(EditDistance::new(3)))], 0.0);
    let Ok(weighted) = weighted else {
        return;
    };
    let accepted = weighted.is_accepted("four", "four");
    assert!(
        accepted,
        "BASELINE DEFECT S2: an over-limit input is refused by EditDistance::try_score,
         collapses to 0.0 through the trait, and satisfies threshold 0.0. Expected on main."
    );
}

#[test]
fn baseline_identical_over_limit_inputs_score_zero_not_one() {
    let scorer = EditDistance::new(3);
    let score = scorer.score("four", "four");
    assert!(
        score < 1.0,
        "BASELINE DEFECT S1: two identical inputs score {score} through the trait,
         because the refusal collapsed to 0.0 instead of the documented identity 1.0.
         Expected on main."
    );
}

#[test]
fn baseline_all_zero_weight_is_accepted_at_threshold_zero() {
    let weighted = Weighted::<str>::new(
        vec![(0.0, Box::new(EditDistance::new(16)))],
        0.0,
    );
    let Ok(weighted) = weighted else {
        return;
    };
    let accepted = weighted.is_accepted("a", "a");
    assert!(
        accepted,
        "BASELINE DEFECT S2: all-zero effective evidence is admitted and accepts.
         Expected on main."
    );
}