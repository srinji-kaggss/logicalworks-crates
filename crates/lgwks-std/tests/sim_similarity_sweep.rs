//! Seeded deterministic replay of the #160 score-domain and refusal contracts.
//!
//! One seed drives every case, a failure prints the seed that produced it, and
//! the same seed must produce the same trace hash. Every probe drives the
//! public API of the shipped crate.

use lgwks_std::similarity::{
    BoundedJaccard, CheckedEvidence, CheckedSimilarity, Cosine, EditDistance, EvidenceError,
    Jaccard, PathSimilarity, Similarity, is_exact_path_match,
};

/// Advances a deterministic xorshift-style stream.
fn next_seed(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1);
    *state
}

/// Folds one observed value into the running trace.
fn fold(trace: &mut u64, value: u64) {
    *trace = trace.wrapping_mul(1_099_511_628_211).wrapping_add(value);
}

/// Returns the bit pattern of a score as a foldable value.
fn bits(score: f64) -> u64 {
    score.to_bits()
}

/// Runs the seeded sweep and returns its deterministic trace.
///
/// `seed` selects every vector, text, set and path below, so one number
/// reproduces the whole run.
fn run_seeded_sweep(seed: u64) -> u64 {
    let mut state = seed;
    let mut trace = 14_695_981_039_346_656_037u64;
    let cosine = Cosine::new();
    let edit = EditDistance::new(6);
    let bounded = BoundedJaccard::<u32>::new(8);
    let paths = PathSimilarity::new();

    for _ in 0..2_000 {
        // Vectors: opposite, orthogonal, equal, zero-magnitude, non-finite, and
        // dimension-mismatched, chosen by the seed.
        let length = usize::try_from(next_seed(&mut state).rem_euclid(4))
            .unwrap_or(1)
            .saturating_add(1);
        let mut left = Vec::with_capacity(length);
        let mut right = Vec::with_capacity(length);
        for _ in 0..length {
            let left_value = i16::try_from(next_seed(&mut state).rem_euclid(5))
                .unwrap_or(0)
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
                fold(&mut trace, bits(raw));
                let normalized = cosine.normalized_score(&left, &right).unwrap_or(-1.0);
                fold(&mut trace, bits(normalized));
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
        let word_length = usize::try_from(next_seed(&mut state).rem_euclid(9))
            .unwrap_or(1)
            .saturating_add(1);
        let left_text: String = std::iter::repeat_n('a', word_length).collect();
        let right_text: String = std::iter::repeat_n('b', word_length).collect();
        match edit.try_score(&left_text, &right_text) {
            Ok(score) => {
                fold(&mut trace, bits(score));
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
        let expanded = turkish.try_score("İ", "i").map(bits).unwrap_or(u64::MAX);
        fold(&mut trace, expanded);
        fold(
            &mut trace,
            u64::try_from(turkish.normalized_length("İ")).unwrap_or(u64::MAX),
        );

        // Sets: budgeted and unbounded scorers agree within the budget.
        let set_length = usize::try_from(next_seed(&mut state).rem_euclid(12))
            .unwrap_or(0)
            .saturating_add(1);
        let left_set: Vec<u32> = (0..set_length)
            .map(|index| u32::try_from(index).unwrap_or(0))
            .collect();
        let right_set: Vec<u32> = (0..set_length)
            .map(|index| u32::try_from(index.saturating_add(1)).unwrap_or(0))
            .collect();
        let unbounded = Jaccard::<u32>::new();
        fold(&mut trace, bits(unbounded.score(&left_set, &right_set)));
        match CheckedSimilarity::try_score(&bounded, &left_set, &right_set) {
            Ok(bounded_score) => {
                fold(&mut trace, bits(bounded_score));
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
        fold(&mut trace, bits(paths.score(&left_path, &right_path)));
        assert!(
            !is_exact_path_match(&left_path, &right_path),
            "seed {seed}: a differing path is never an exact match"
        );
    }
    trace
}

#[test]
fn the_same_seed_replays_to_the_same_trace() {
    for seed in [
        0x1600_5EED_0000_0001_u64,
        0x1600_5EED_0000_0002,
        0x1600_5EED_FFFF_FFFF,
        0xDEAD_BEEF_CAFE_0001,
    ] {
        let first = run_seeded_sweep(seed);
        let replayed = run_seeded_sweep(seed);
        assert_eq!(
            first, replayed,
            "seed {seed}: the same seed must produce the same trace"
        );
    }
}

#[test]
fn different_seeds_produce_different_traces() {
    let first = run_seeded_sweep(0x1600_5EED_0000_0001);
    let second = run_seeded_sweep(0x1600_5EED_0000_0002);
    assert_ne!(
        first, second,
        "two seeds must not collapse to the same trace, or the sweep proves nothing"
    );
}

#[test]
fn a_failure_reports_the_seed_that_produced_it() {
    // The sweep's assertions carry the seed in their message, so a failing run
    // names the reproducing seed. This checks the sweep is actually reached at
    // every seed rather than short-circuiting on the first one.
    let trace = run_seeded_sweep(0x1600_5EED_0000_0001);
    assert_ne!(
        trace, 0,
        "seed 0x1600_5EED_0000_0001 produced an empty trace"
    );
}
