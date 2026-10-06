//! Seeded sweeps over the simulation substrate's own contracts.
//!
//! Every `sim_*` family rests on four things this file checks rather than
//! assumes: a run that recorded nothing is refused instead of replaying
//! vacuously, the generator replays exactly and stays inside the bounds it was
//! asked for, the weighted coin is calibrated, and the trace frames each record
//! so two different runs cannot hash alike by splitting the same bytes
//! differently. The scratch family checks the one directory guard every
//! durable family writes into: concurrent guards never share a path, and every
//! one is gone once dropped.
//!
//! Written out per band rather than through `band_family!`, because the gate
//! that counts simulation evidence reads `#[test]` attributes from the source.

use crate::sim;

use std::error::Error;

use sim::{Band, Rng, Trace};

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn an_empty_run_is_refused_by_the_sweep_band_00() -> TestResult {
    a_run_that_recorded_nothing_is_refused(Band::new(sim::band_of(0).first, sim::band_of(0).count))
}

#[test]
fn an_empty_run_is_refused_by_the_sweep_band_01() -> TestResult {
    a_run_that_recorded_nothing_is_refused(Band::new(sim::band_of(1).first, sim::band_of(1).count))
}

#[test]
fn an_empty_run_is_refused_by_the_sweep_band_02() -> TestResult {
    a_run_that_recorded_nothing_is_refused(Band::new(sim::band_of(2).first, sim::band_of(2).count))
}

#[test]
fn an_empty_run_is_refused_by_the_sweep_band_03() -> TestResult {
    a_run_that_recorded_nothing_is_refused(Band::new(sim::band_of(3).first, sim::band_of(3).count))
}

#[test]
fn the_generator_replays_and_stays_in_bounds_band_04() -> TestResult {
    the_generator_replays_and_stays_in_bounds(Band::new(
        sim::band_of(4).first,
        sim::band_of(4).count,
    ))
}

#[test]
fn the_generator_replays_and_stays_in_bounds_band_05() -> TestResult {
    the_generator_replays_and_stays_in_bounds(Band::new(
        sim::band_of(5).first,
        sim::band_of(5).count,
    ))
}

#[test]
fn the_generator_replays_and_stays_in_bounds_band_06() -> TestResult {
    the_generator_replays_and_stays_in_bounds(Band::new(
        sim::band_of(6).first,
        sim::band_of(6).count,
    ))
}

#[test]
fn the_generator_replays_and_stays_in_bounds_band_07() -> TestResult {
    the_generator_replays_and_stays_in_bounds(Band::new(
        sim::band_of(7).first,
        sim::band_of(7).count,
    ))
}

#[test]
fn the_weighted_coin_is_calibrated_band_08() -> TestResult {
    the_weighted_coin_is_calibrated(Band::new(sim::band_of(8).first, sim::band_of(8).count))
}

#[test]
fn the_weighted_coin_is_calibrated_band_09() -> TestResult {
    the_weighted_coin_is_calibrated(Band::new(sim::band_of(9).first, sim::band_of(9).count))
}

#[test]
fn the_weighted_coin_is_calibrated_band_10() -> TestResult {
    the_weighted_coin_is_calibrated(Band::new(sim::band_of(10).first, sim::band_of(10).count))
}

#[test]
fn the_weighted_coin_is_calibrated_band_11() -> TestResult {
    the_weighted_coin_is_calibrated(Band::new(sim::band_of(11).first, sim::band_of(11).count))
}

#[test]
fn the_trace_frames_every_record_band_12() -> TestResult {
    the_trace_frames_every_record(Band::new(sim::band_of(12).first, sim::band_of(12).count))
}

#[test]
fn the_trace_frames_every_record_band_13() -> TestResult {
    the_trace_frames_every_record(Band::new(sim::band_of(13).first, sim::band_of(13).count))
}

#[test]
fn the_trace_frames_every_record_band_14() -> TestResult {
    the_trace_frames_every_record(Band::new(sim::band_of(14).first, sim::band_of(14).count))
}

#[test]
fn the_trace_frames_every_record_band_15() -> TestResult {
    the_trace_frames_every_record(Band::new(sim::band_of(15).first, sim::band_of(15).count))
}

#[cfg(feature = "rt")]
#[test]
fn scratch_directories_never_collide_and_vanish_band_16() -> TestResult {
    scratch_directories_never_collide_and_vanish(Band::new(
        sim::band_of(16).first,
        sim::band_of(16).count,
    ))
}

#[cfg(feature = "rt")]
#[test]
fn scratch_directories_never_collide_and_vanish_band_17() -> TestResult {
    scratch_directories_never_collide_and_vanish(Band::new(
        sim::band_of(17).first,
        sim::band_of(17).count,
    ))
}

#[cfg(feature = "rt")]
#[test]
fn scratch_directories_never_collide_and_vanish_band_18() -> TestResult {
    scratch_directories_never_collide_and_vanish(Band::new(
        sim::band_of(18).first,
        sim::band_of(18).count,
    ))
}

#[cfg(feature = "rt")]
#[test]
fn scratch_directories_never_collide_and_vanish_band_19() -> TestResult {
    scratch_directories_never_collide_and_vanish(Band::new(
        sim::band_of(19).first,
        sim::band_of(19).count,
    ))
}

/// A sweep is refused exactly when some seed in it recorded nothing, and the
/// refusal names the first such seed.
///
/// Each seed draws how many facts it records, zero included, so a band holds
/// a mix; the sweep must agree with an independent scan of the same draws.
/// Every seed is also swept alone, and two controls run over the whole band —
/// one recording nothing, one recording every seed — so both verdicts are
/// exercised on every band rather than only when a draw happens to mix them.
fn a_run_that_recorded_nothing_is_refused(band: Band) -> TestResult {
    let facts_for = |seed: u64| Rng::new(seed ^ 0x00e0_0e0e).below(4);
    for seed in band.seeds() {
        let alone = sim::sweep(Band::new(seed, 1), |run| {
            for fact in 0..facts_for(run.seed) {
                run.trace.record_number("fact", fact);
            }
            Ok(())
        });
        assert_eq!(
            alone.is_err(),
            facts_for(seed) == 0,
            "seed {seed} swept alone: refused exactly when it recorded nothing"
        );
    }
    assert!(
        sim::sweep(band, |_| Ok(())).is_err(),
        "a band that recorded nothing was accepted"
    );
    let recorded = sim::sweep(band, |run| {
        run.trace.record_number("seed", run.seed);
        Ok(())
    })?;
    assert_eq!(
        recorded.len(),
        band.seeds().count(),
        "a band that recorded every seed returned every receipt"
    );
    let first_empty = band.seeds().find(|seed| facts_for(*seed) == 0);
    let verdict = sim::sweep(band, |run| {
        for fact in 0..facts_for(run.seed) {
            run.trace.record_number("fact", fact);
        }
        Ok(())
    });
    match (first_empty, verdict) {
        (None, Ok(hashes)) => {
            assert_eq!(
                hashes.len(),
                band.seeds().count(),
                "every seed that recorded something returned its receipt"
            );
        }
        (Some(seed), Err(refusal)) => {
            assert!(
                refusal
                    .to_string()
                    .contains(&format!("seed {seed} recorded nothing")),
                "the refusal names the first empty seed {seed}: {refusal}"
            );
        }
        (expected, got) => {
            let refusal: TestResult = Err(format!(
                "first empty seed {expected:?}, but the sweep answered {got:?}"
            )
            .into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "a_run_that_recorded_nothing_is_refused: the sweep disagreed with the scan");
            return refusal;
        }
    }
    Ok(())
}

/// The substrate's generator and `lgwks_std::seeded::Seeded` are one stream.
///
/// The substrate stays a `std`-only file because `lgwks_ast` includes it and
/// does not depend on `lgwks_std`; this is what keeps the two copies of the one
/// algorithm from drifting, so a seed recorded against any crate's simulations
/// draws the same words through the public API.
#[test]
fn the_substrate_and_lgwks_std_seeded_draw_one_stream() -> TestResult {
    for seed in (0..10_000_u64).chain([1 << 63, u64::MAX - 1, u64::MAX]) {
        let mut substrate = Rng::new(seed);
        let mut seeded = lgwks_std::seeded::Seeded::from_seed(seed);
        for step in 0..64_u32 {
            assert_eq!(
                substrate.next_u64(),
                seeded.next_u64(),
                "seed {seed} step {step}: the substrate left lgwks_std's stream"
            );
        }
    }
    Ok(())
}

/// Two generators on one seed draw the same sequence, and every bounded draw
/// lands where it was asked to.
fn the_generator_replays_and_stays_in_bounds(band: Band) -> TestResult {
    for seed in band.seeds() {
        let mut first = Rng::new(seed);
        let mut second = Rng::new(seed);
        for step in 0..512_u32 {
            let bound = step.saturating_add(1);
            let low = step;
            let high = step.saturating_mul(3);
            let below = first.below(bound);
            let between = first.between(low, high);
            assert_eq!(
                (below, between),
                (second.below(bound), second.between(low, high)),
                "seed {seed} step {step}: two generators on one seed diverged"
            );
            assert!(below < bound, "seed {seed}: below({bound}) gave {below}");
            assert!(
                (low..=high.max(low)).contains(&between),
                "seed {seed}: between({low}, {high}) gave {between}"
            );
        }
        assert_eq!(first.below(1), 0, "seed {seed}: below(1) has one value");
    }
    Ok(())
}

/// `chance(p)` fires about `p` times in a thousand, never at zero and always
/// at a thousand.
///
/// The window is wide enough that a calibrated coin cannot miss it on any seed
/// (over five standard deviations) and narrow enough that a coin reading a
/// biased or phase-locked bit would.
fn the_weighted_coin_is_calibrated(band: Band) -> TestResult {
    const DRAWS: u32 = 4_000;
    for seed in band.seeds() {
        let mut rng = Rng::new(seed);
        let per_mille = rng.between(100, 900);
        let mut fired = 0_u32;
        for _ in 0..DRAWS {
            fired = fired.saturating_add(u32::from(rng.chance(per_mille)));
        }
        let expected = per_mille.saturating_mul(4);
        assert!(
            fired.abs_diff(expected) <= 160,
            "seed {seed}: chance({per_mille}) fired {fired} of {DRAWS}, expected about {expected}"
        );
        assert!(!rng.chance(0), "seed {seed}: chance(0) fired");
        assert!(rng.chance(1_000), "seed {seed}: chance(1000) did not fire");
    }
    Ok(())
}

/// Splitting the same bytes into records differently hashes differently.
///
/// Without framing, `record("ab"); record("c")` and `record("a");
/// record("bc")` would be one receipt for two runs that recorded different
/// facts — a replay that passed while the run took another path.
fn the_trace_frames_every_record(band: Band) -> TestResult {
    const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyz";
    for seed in band.seeds() {
        let mut rng = Rng::new(seed);
        let length = rng.between(2, 48);
        let mut text = String::new();
        for _ in 0..length {
            let index = usize::try_from(rng.below(26))?;
            let letter = LETTERS.get(index).ok_or("a letter inside the alphabet")?;
            text.push(char::from(*letter));
        }
        let cut = usize::try_from(rng.between(1, length.saturating_sub(1)))?;
        let other = if cut > 1 {
            cut.saturating_sub(1)
        } else {
            cut.saturating_add(1)
        };
        let receipt_of = |at: usize| -> Result<u64, Box<dyn Error>> {
            let (head, tail) = text.split_at_checked(at).ok_or("a cut inside the text")?;
            let mut trace = Trace::new();
            trace.record(head);
            trace.record(tail);
            Ok(trace.hash())
        };
        assert_ne!(
            receipt_of(cut)?,
            receipt_of(other)?,
            "seed {seed}: {text:?} cut at {cut} and at {other} hashed alike"
        );
        assert_eq!(
            receipt_of(cut)?,
            receipt_of(cut)?,
            "seed {seed}: one cut, two receipts"
        );
    }
    Ok(())
}

/// Concurrent scratch guards never share a directory, and every one is gone
/// once its guard drops.
#[cfg(feature = "rt")]
fn scratch_directories_never_collide_and_vanish(band: Band) -> TestResult {
    use crate::scratch::Scratch;
    for seed in band.seeds() {
        let mut rng = Rng::new(seed);
        let count = usize::try_from(rng.between(2, 24))?;
        let tag = format!("sim-substrate-{seed}");
        let made: Vec<Result<Scratch, String>> = std::thread::scope(|scope| {
            // Every worker is spawned before any is joined, so the guards are
            // created concurrently rather than one after another.
            let mut workers = Vec::with_capacity(count);
            for _ in 0..count {
                workers.push(scope.spawn(|| Scratch::new(&tag).map_err(|error| error.to_string())));
            }
            workers
                .into_iter()
                .map(|worker| {
                    worker
                        .join()
                        .map_err(|panic| format!("a worker panicked: {panic:?}"))?
                })
                .collect()
        });
        let guards = made.into_iter().collect::<Result<Vec<_>, _>>()?;
        let mut paths: Vec<_> = guards
            .iter()
            .map(|guard| guard.path().to_path_buf())
            .collect();
        assert!(
            paths.iter().all(|path| path.is_dir()),
            "seed {seed}: every guard's directory exists while it is held"
        );
        paths.sort();
        paths.dedup();
        assert_eq!(
            paths.len(),
            count,
            "seed {seed}: {count} concurrent guards with one tag shared a directory"
        );
        drop(guards);
        assert!(
            paths.iter().all(|path| !path.exists()),
            "seed {seed}: a dropped guard left its directory behind"
        );
    }
    Ok(())
}
