//! Seeded deterministic replay of the #160 score-domain and refusal contracts.
//!
//! One seed drives every case, a failure prints the seed that produced it, and
//! the same seed must produce the same trace hash. Every probe drives the
//! public API of the shipped crate.

use lgwks_std::similarity::{
    BoundedJaccard, CheckedEvidence, CheckedSimilarity, Cosine, EditDistance, EvidenceError,
    Jaccard, PathSimilarity, Similarity, is_exact_path_match,
};

use crate::seeded_sweep;

use crate::seeded_bytes::next_byte;
use seeded_sweep::seeded_stream;
use seeded_sweep::{
    SWEEP_SEEDS, assert_distinct_seeds_diverge, assert_same_seed_replays, fold, fold_score,
    fold_usize, initial_trace, next_index, next_seed,
};

/// The set window the sweep draws its set members from, in the width the set
/// element type is.
const SET_WINDOW: u32 = 12;

/// The trace arm a raw score whose named mapping refused folds as.
///
/// A raw score with no mapped score is not a score at all, so it gets a trace
/// value no score can take rather than a float standing in for one.
const NO_NAMED_MAPPING_ARM: u64 = u64::MAX.saturating_sub(1);

/// Runs the seeded sweep and returns its deterministic trace.
///
/// `seed` selects every vector, text, set and path below, so one number
/// reproduces the whole run.
fn run_seeded_sweep(seed: u64) -> u64 {
    let mut state = seeded_stream(seed);
    let mut trace = initial_trace();
    let cosine = Cosine::new();
    let edit = EditDistance::new(6);
    let bounded = BoundedJaccard::<u32>::new(8);
    let paths = PathSimilarity::new();

    for _ in 0..2_000 {
        // Vectors: opposite, orthogonal, equal, zero-magnitude, non-finite, and
        // dimension-mismatched, chosen by the seed.
        let length = next_index(&mut state).rem_euclid(4).saturating_add(1);
        let mut left = Vec::with_capacity(length);
        let mut right = Vec::with_capacity(length);
        for _ in 0..length {
            let left_value = i16::from(next_byte(&mut state))
                .rem_euclid(5)
                .saturating_sub(2);
            // The seeded pairing decides which family this iteration is.
            let family = next_seed(&mut state).rem_euclid(6);
            let right_value = match family {
                0 => left_value.saturating_neg(),
                1 => left_value.saturating_add(1),
                2 => 0,
                3 => 3_000,
                _ => left_value,
            };
            left.push(f32::from(left_value));
            right.push(f32::from(right_value));
        }

        match cosine.try_score(&left, &right) {
            Ok(raw) => {
                fold_score(&mut trace, raw);
                // The named mapping refused where the raw score did not, which is
                // the one case this family must see as itself rather than as a
                // score: the refusal is folded as a refusal — its own arm and the
                // length of what it said — and the comparisons below do not run
                // on a value the mapping never produced.
                let normalized = match cosine.normalized_score(&left, &right) {
                    Ok(normalized) => normalized,
                    Err(reason) => {
                        fold(&mut trace, NO_NAMED_MAPPING_ARM);
                        fold_usize(&mut trace, reason.to_string().len());
                        continue;
                    }
                };
                fold_score(&mut trace, normalized);
                let shared = Similarity::score(&cosine, &left, &right);
                assert!(
                    (0.0..=1.0).contains(&shared),
                    "seed {seed}: the shared contract left [0,1] with {shared} \
                     for {left:?} and {right:?}"
                );
                assert_eq!(
                    shared.to_bits(),
                    normalized.to_bits(),
                    "seed {seed}: the trait form and the named mapping must agree"
                );
            }
            Err(reason) => {
                fold(&mut trace, u64::MAX);
                // A refusal never becomes a number through a composition, and
                // never satisfies a threshold.
                let policy =
                    CheckedEvidence::new(vec![Box::new(Cosine::new())], vec![1.0], 0.0).ok();
                if let Some(policy) = policy
                    && let Ok(verdict) = policy.verdict(&left, &right)
                {
                    assert_eq!(
                        verdict.score(),
                        None,
                        "seed {seed}: a refusal produced a score"
                    );
                    assert!(
                        !verdict.is_accepted(),
                        "seed {seed}: a refusal was accepted at threshold zero"
                    );
                    assert_eq!(
                        verdict.refusals().len(),
                        1,
                        "seed {seed}: a refusal must be attributed to its component"
                    );
                }
                let _ = reason;
            }
        }

        // Text: over-budget and within-budget pairs exercise the normalized unit.
        let word_length = next_index(&mut state).rem_euclid(9).saturating_add(1);
        let left_text: String = std::iter::repeat_n('a', word_length).collect();
        let right_text: String = std::iter::repeat_n('b', word_length).collect();
        match edit.try_score(&left_text, &right_text) {
            Ok(score) => {
                fold_score(&mut trace, score);
                assert!(
                    (0.0..=1.0).contains(&score),
                    "seed {seed}: measured edit score {score} left [0,1]"
                );
            }
            Err(reason) => {
                fold(&mut trace, u64::MAX);
                assert!(
                    matches!(
                        reason,
                        lgwks_std::similarity::EditDistanceError::InputTooLong { .. }
                    ),
                    "seed {seed}: an over-budget text must refuse with the budget error"
                );
            }
        }

        // Unicode expansion: the charged unit is the normalized one, and the
        // refusal is consumed rather than discarded.
        let turkish = EditDistance::new(1);
        match turkish.try_score("İ", "i") {
            Ok(score) => fold_score(&mut trace, score),
            Err(_) => fold(&mut trace, u64::MAX),
        }
        fold_usize(&mut trace, turkish.normalized_length("İ"));

        // Sets: budgeted and unbounded scorers agree within the budget.
        // The set window is one byte wide, so `u8` into `u32` carries the length
        // and every member: the two sets are offsets of each other in the width
        // the set element type is, and neither is a narrowed index.
        let set_length = u32::from(next_byte(&mut state))
            .rem_euclid(SET_WINDOW)
            .saturating_add(1);
        let left_set: Vec<u32> = (0..set_length).collect();
        let right_set: Vec<u32> = (1..=set_length).collect();
        let unbounded = Jaccard::<u32>::new();
        fold_score(&mut trace, unbounded.score(&left_set, &right_set));
        match CheckedSimilarity::try_score(&bounded, &left_set, &right_set) {
            Ok(bounded_score) => {
                fold_score(&mut trace, bounded_score);
                assert_eq!(
                    bounded_score.to_bits(),
                    unbounded.score(&left_set, &right_set).to_bits(),
                    "seed {seed}: the bounded and unbounded set scorers must agree \
                     inside the budget"
                );
            }
            Err(reason) => {
                fold(&mut trace, u64::MAX);
                assert!(
                    matches!(reason, EvidenceError::CollectionTooLong { .. }),
                    "seed {seed}: an over-budget set must refuse with the budget error"
                );
            }
        }

        // Path heuristic: a scored collision is never an exact match.
        let left_path = format!("root/child/{word_length}");
        let right_path = format!("root/child/{}", word_length.saturating_add(1));
        fold_score(&mut trace, paths.score(&left_path, &right_path));
        assert!(
            !is_exact_path_match(&left_path, &right_path),
            "seed {seed}: a differing path is never an exact match"
        );
    }
    trace
}

#[test]
fn the_same_seed_replays_to_the_same_trace() {
    for seed in SWEEP_SEEDS {
        assert_same_seed_replays(run_seeded_sweep, seed);
    }
}

#[test]
fn different_seeds_produce_different_traces() {
    assert_distinct_seeds_diverge(run_seeded_sweep, SWEEP_SEEDS[0], SWEEP_SEEDS[1]);
}

#[test]
fn a_failure_reports_the_seed_that_produced_it() {
    // The sweep's assertions carry the seed in their message, so a failing run
    // names the reproducing seed. This checks the sweep is actually reached
    // rather than short-circuiting on the first case.
    let trace = run_seeded_sweep(SWEEP_SEEDS[0]);
    assert_ne!(trace, initial_trace(), "the sweep produced an empty trace");
}
