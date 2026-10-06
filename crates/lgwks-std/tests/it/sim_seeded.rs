//! Seeded sweeps over `lgwks_std::seeded`, the estate's one deterministic stream.
//!
//! Each family draws its own inputs from the seed under test and checks the
//! public API against a model written here, never against the API's own output:
//! a bounded draw is recomputed from the twin stream's raw words with plain
//! `u128` arithmetic, so a mis-split product, an off-by-one threshold or a
//! rejection that consumed the wrong number of words disagrees on some seed.

use std::error::Error;

use lgwks_std::seeded::{DrawError, Seeded};

/// What every test in this family returns.
type TestResult = Result<(), Box<dyn Error>>;

/// The seeds every sweep here covers, beyond the ones it draws itself.
const SEEDS: u64 = 10_000;

/// Bounds a careless reduction gets wrong: one, the powers of two and their
/// neighbours at both word halves, the two-thirds bound where a remainder is
/// most biased, the half-plus-one bound where a rejection is most likely, and
/// the top of the range.
const EDGE_BOUNDS: [u64; 12] = [
    1,
    2,
    3,
    (1 << 32) - 1,
    1 << 32,
    (1 << 32) + 1,
    (1 << 63) - 1,
    1 << 63,
    (1 << 63) + 1,
    0xaaaa_aaaa_aaaa_aaab,
    u64::MAX - 1,
    u64::MAX,
];

/// The model of `below`: Lemire's multiply-shift with the exact threshold,
/// computed from `words` with `u128` shifts and masks rather than byte splits.
fn modelled_below(words: &mut Seeded, bound: u64) -> Result<u64, Box<dyn Error>> {
    let threshold = bound
        .wrapping_neg()
        .checked_rem(bound)
        .ok_or("the model is only asked for non-zero bounds")?;
    loop {
        let product = u128::from(words.next_u64()).wrapping_mul(u128::from(bound));
        let low = u64::try_from(product & u128::from(u64::MAX))?;
        if low >= threshold {
            return Ok(u64::try_from(product >> 64)?);
        }
    }
}

/// A bound drawn from the seed: an edge bound, or a word of the stream masked
/// to a drawn width so small, middling and huge bounds all occur.
fn drawn_bound(picker: &mut Seeded) -> Result<u64, Box<dyn Error>> {
    if picker.below(2)? == 0 {
        let at = picker.index(EDGE_BOUNDS.len())?;
        return Ok(*EDGE_BOUNDS
            .get(at)
            .ok_or("an edge bound inside the table")?);
    }
    let width = picker.below(64)?;
    let mask = u64::MAX >> width;
    Ok((picker.next_u64() & mask).max(1))
}

#[test]
fn sim_every_bounded_draw_matches_the_model_and_its_range() -> TestResult {
    for seed in 0..SEEDS {
        let mut picker = Seeded::from_seed(seed ^ 0x5eed_0344_b0de_0000);
        let mut stream = Seeded::from_seed(seed);
        let mut model = Seeded::from_seed(seed);
        for step in 0..64 {
            let bound = drawn_bound(&mut picker)?;
            let drawn = stream.below(bound)?;
            let expected = modelled_below(&mut model, bound)?;
            assert_eq!(
                drawn, expected,
                "seed {seed} step {step}: below({bound}) left the model"
            );
            assert!(drawn < bound, "seed {seed}: below({bound}) gave {drawn}");
            assert_eq!(
                stream, model,
                "seed {seed} step {step}: below({bound}) consumed a different number of words"
            );
        }
    }
    Ok(())
}

#[test]
fn sim_an_index_lands_inside_every_collection_length() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let len = usize::try_from(stream.below(4_096)?)?.saturating_add(1);
        let at = stream.index(len)?;
        assert!(at < len, "seed {seed}: index({len}) gave {at}");
        let before = stream.clone();
        assert_eq!(stream.index(0), Err(DrawError::EmptyRange));
        assert_eq!(stream.below(0), Err(DrawError::EmptyRange));
        assert_eq!(
            stream, before,
            "seed {seed}: a refused draw moved the stream"
        );
    }
    Ok(())
}

#[test]
fn sim_small_bounds_are_uniform_on_every_seed_swept() -> TestResult {
    // 12,000 draws per bound: a chi-square statistic over at most eleven degrees
    // of freedom exceeds 45 with probability below 1e-5, and the sweep runs
    // 64 seeds by 11 bounds, so a fair generator fails it with probability
    // below one in a hundred and forty.
    for seed in 0..64_u64 {
        let mut stream = Seeded::from_seed(seed);
        for bound in 2..=12_u64 {
            let cells = usize::try_from(bound)?;
            let mut counts = vec![0_u64; cells];
            let draws = 12_000_u64;
            for _ in 0..draws {
                let at = usize::try_from(stream.below(bound)?)?;
                let cell = counts.get_mut(at).ok_or("a draw inside its bound")?;
                *cell = cell.saturating_add(1);
            }
            let expected = draws.checked_div(bound).ok_or("a non-zero bound")?;
            let chi_square_scaled: u64 = counts
                .iter()
                .map(|count| count.abs_diff(expected).saturating_pow(2))
                .sum();
            // chi² = Σ (o - e)² / e, compared as Σ (o - e)² < 45 · e.
            assert!(
                chi_square_scaled < expected.saturating_mul(45),
                "seed {seed}: below({bound}) counts {counts:?} are not uniform"
            );
        }
    }
    Ok(())
}

#[test]
fn sim_a_hundred_thousand_streams_on_a_thousand_threads_match_the_serial_run() -> TestResult {
    const THREADS: u64 = 1_000;
    const PER_THREAD: u64 = 100;
    fn fold(seed: u64) -> u64 {
        let mut stream = Seeded::from_seed(seed);
        (0..32).fold(seed, |acc, _| acc.rotate_left(5) ^ stream.next_u64())
    }
    let serial: Vec<u64> = (0..THREADS.saturating_mul(PER_THREAD)).map(fold).collect();
    let concurrent: Vec<Vec<u64>> = std::thread::scope(|scope| {
        // Every worker is spawned before any is joined, so the thousand streams
        // are drawn concurrently rather than one after another.
        let mut workers = Vec::with_capacity(usize::try_from(THREADS)?);
        for thread in 0..THREADS {
            workers.push(scope.spawn(move || {
                let first = thread.saturating_mul(PER_THREAD);
                (first..first.saturating_add(PER_THREAD))
                    .map(fold)
                    .collect::<Vec<u64>>()
            }));
        }
        workers
            .into_iter()
            .map(|worker| {
                worker
                    .join()
                    .map_err(|panic| format!("a worker panicked: {panic:?}").into())
            })
            .collect::<Result<_, Box<dyn Error>>>()
    })?;
    let concurrent: Vec<u64> = concurrent.into_iter().flatten().collect();
    assert_eq!(
        concurrent, serial,
        "a stream drawn on another thread is not the stream its seed names"
    );
    Ok(())
}
