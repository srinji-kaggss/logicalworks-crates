//! Seeded sweeps of the append-latency verdict: the sweep half of
//! [`journal_scale::append_latency_tails_are_bounded`](crate::journal_scale).
//!
//! The verdict ([`append_verdict`]) decides whether a sweep of acknowledged
//! appends is pathological, against bare durable writes timed beside them. Its
//! source is main CI run 38929540150, where this path read p50 2.6 ms and p99
//! 453 ms with nothing in it changed because thirteen jobs of one machine began
//! at once, and Dean and Barroso, "The Tail at Scale" (Communications of the ACM
//! 56(2), 2013): a tail belongs to the host as often as to the code, so it is
//! read against the host.
//!
//! Each sweep builds the two sequences from a seed the way a host would deal
//! them, and holds the verdict to three claims: a stall the host deals both
//! sequences is never charged to the path, a path that costs a flush per rung is
//! refused whatever the host does, and a path that stalls where its host does not
//! is refused. [`check_deterministic`] runs every sweep twice and requires one
//! trace hash, so a verdict that depended on anything but its inputs fails here
//! even when each assertion happens to hold.

use crate::journal_scale::{STALL_RATIO, STALL_US, TYPICAL_RATIO, append_verdict, stalls};
use crate::sim::seed::{Rng, Trace};

use std::error::Error;

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

/// How many appends one sweep holds: what the journal test times.
const SAMPLES: usize = 1_024;

/// One percent of a sweep: the stalls its p99 can hold without being a stall.
const ONE_PERCENT: usize = SAMPLES.div_euclid(100);

/// One stall more than [`ONE_PERCENT`]: the fewest that make the p99 a stall.
const OVER_ONE_PERCENT: usize = ONE_PERCENT + 1;

/// How many seeds each sweep covers.
const SEEDS: u64 = 256;

/// The most stretches of the sweep a host stalls in one seed.
const MAX_WINDOWS: u32 = 4;

/// The longest stretch, in samples, a host stalls for.
const MAX_WINDOW_LEN: u32 = 60;

/// The slowest a stall runs past [`STALL_US`], in microseconds: run
/// 38929540150's slowest append took 1.4 s.
const STALL_SPREAD_US: u32 = 1_150_000;

/// The cost of every stall in the boundary cases: run 38929540150's p99.
const FAILED_RUN_STALL_US: u128 = 453_011;

/// The two sequences the journal test times in the same instants.
struct Sweep {
    /// Each acknowledged append's cost, microseconds.
    path: Vec<u128>,
    /// The bare durable write timed straight after it.
    bare: Vec<u128>,
}

/// What one pass over a band of seeds recorded.
struct Outcome {
    /// Every fact the pass observed, framed and hashable.
    trace: Trace,
    /// On how many seeds the claim under test was actually put to the verdict
    /// rather than satisfied vacuously.
    exercised: usize,
}

/// Run `sweep` twice and require one trace, and the same count of exercised seeds.
fn check_deterministic(
    sweep: impl Fn() -> Result<Outcome, Box<dyn Error>>,
) -> Result<usize, Box<dyn Error>> {
    let first = sweep()?;
    let second = sweep()?;
    assert_eq!(
        first.trace.hash(),
        second.trace.hash(),
        "the trace depends on the seeds alone, never on the pass"
    );
    assert_eq!(
        first.exercised, second.exercised,
        "and so does the coverage"
    );
    Ok(first.exercised)
}

/// The bare durable write's cost over a calm host: a base the seed picks, and
/// jitter. Microseconds, near what a RAM disk answers.
fn calm_bare(rng: &mut Rng) -> Vec<u128> {
    let base = rng.between(80, 400);
    (0..SAMPLES)
        .map(|_| u128::from(base.saturating_add(rng.below(120))))
        .collect()
}

/// `bare` scaled by `tenths` tenths, plus jitter: the path is that write behind
/// a hop, a fence and a frame.
fn scaled(bare: &[u128], tenths: u128, rng: &mut Rng) -> Vec<u128> {
    bare.iter()
        .map(|us| {
            let scaled = us.saturating_mul(tenths).div_euclid(10);
            scaled.saturating_add(u128::from(rng.below(50)))
        })
        .collect()
}

/// One stall of `cost` on sample `index`.
fn stall_at(sample: &mut [u128], index: u32, cost: u128) -> TestResult {
    let slot = sample
        .get_mut(usize::try_from(index)?)
        .ok_or("a stall landed outside the sweep")?;
    *slot = slot.saturating_add(cost);
    Ok(())
}

/// A stall's cost: [`STALL_US`] and up to [`STALL_SPREAD_US`] more.
fn stall_cost(rng: &mut Rng) -> u128 {
    STALL_US.saturating_add(u128::from(rng.below(STALL_SPREAD_US)))
}

/// A calm host's sweep: the path costs 1.1 to 2.5 times the bare write, a range
/// that covers the recorded runs (1.1 to 1.2 times at the median, up to 2.3 times
/// at the p99).
fn calm_sweep(rng: &mut Rng) -> Sweep {
    let bare = calm_bare(rng);
    let tenths = u128::from(rng.between(11, 25));
    let path = scaled(&bare, tenths, rng);
    Sweep { path, bare }
}

/// Stall up to [`MAX_WINDOWS`] stretches of the sweep as a device that blocks
/// everything on it for a while would: every append in a stretch stalls, and
/// each bare write beside it does with a probability the stretch draws between
/// one half and one, because the stall hits whichever write is in flight.
fn stall_host(rng: &mut Rng, sweep: &mut Sweep) -> TestResult {
    let span = u32::try_from(SAMPLES)?;
    for _ in 0..rng.between(0, MAX_WINDOWS) {
        let start = rng.below(span.saturating_sub(MAX_WINDOW_LEN));
        let len = rng.between(1, MAX_WINDOW_LEN);
        let share = rng.between(500, 1_000);
        for index in start..start.saturating_add(len) {
            let cost = stall_cost(rng);
            stall_at(&mut sweep.path, index, cost)?;
            if rng.below(1_000) < share {
                stall_at(&mut sweep.bare, index, cost)?;
            }
        }
    }
    Ok(())
}

/// Why `verdict` refused the sweep of `seed`, or an error if it did not.
fn refusal(seed: u64, verdict: Result<(), String>) -> Result<String, String> {
    match verdict {
        Err(reason) => Ok(reason),
        Ok(()) => Err(format!("seed {seed}: the sweep was not refused")),
    }
}

/// Every sample `us` but the first `stalled`, which are [`FAILED_RUN_STALL_US`].
fn flat(us: u128, stalled: usize) -> Vec<u128> {
    (0..SAMPLES)
        .map(|index| {
            if index < stalled {
                FAILED_RUN_STALL_US
            } else {
                us
            }
        })
        .collect()
}

/// A stall the host deals both sequences is never charged to the path, on any
/// seed, however many stretches it stalls and however long.
#[test]
fn host_stalls_are_never_charged_to_the_path_for_any_seed() -> TestResult {
    let exercised = check_deterministic(|| {
        let mut trace = Trace::new();
        let mut exercised: usize = 0;
        for seed in 0..SEEDS {
            let mut rng = Rng::new(seed ^ 0xa99e_4d5a_11e5_5eed);
            let mut sweep = calm_sweep(&mut rng);
            stall_host(&mut rng, &mut sweep)?;
            let (path_stalls, bare_stalls) = (stalls(&sweep.path), stalls(&sweep.bare));
            append_verdict(&sweep.path, &sweep.bare)
                .map_err(|reason| format!("seed {seed} was charged: {reason}"))?;
            trace.record_number("path-stalls", path_stalls);
            trace.record_number("bare-stalls", bare_stalls);
            // The old absolute bound would have failed this seed: its p99 is a stall.
            if path_stalls > ONE_PERCENT {
                exercised = exercised.saturating_add(1);
            }
        }
        Ok(Outcome { trace, exercised })
    })?;
    assert!(
        exercised > 0,
        "no seed stalled the path past its 1% line, so the sweep proved nothing"
    );
    Ok(())
}

/// A path that costs more than [`TYPICAL_RATIO`] times the bare write is
/// refused on every seed: a flush per rung of the four-rung ladder, or more.
#[test]
fn a_path_that_costs_a_flush_per_rung_is_refused_for_every_seed() -> TestResult {
    let refused = check_deterministic(|| {
        let mut trace = Trace::new();
        let mut exercised: usize = 0;
        for seed in 0..SEEDS {
            let mut rng = Rng::new(seed ^ 0xf1a5_4b3a_7c0d_e5ed);
            let bare = calm_bare(&mut rng);
            let times = TYPICAL_RATIO
                .saturating_add(1)
                .saturating_add(u128::from(rng.below(4)));
            let path: Vec<u128> = bare.iter().map(|us| us.saturating_mul(times)).collect();
            let reason = refusal(seed, append_verdict(&path, &bare))?;
            assert!(
                reason.contains("median append"),
                "seed {seed} was refused for the wrong reason: {reason}"
            );
            trace.record_number("times", times);
            exercised = exercised.saturating_add(1);
        }
        Ok(Outcome { trace, exercised })
    })?;
    assert_eq!(refused, usize::try_from(SEEDS)?, "every seed was refused");
    Ok(())
}

/// A path that stalls where its host does not is refused on every seed, from
/// one stall past the 1% line up.
#[test]
fn stalls_the_host_does_not_share_are_refused_for_every_seed() -> TestResult {
    let refused = check_deterministic(|| {
        let mut trace = Trace::new();
        let mut exercised: usize = 0;
        let span = u32::try_from(SAMPLES)?;
        let over = u32::try_from(OVER_ONE_PERCENT)?;
        for seed in 0..SEEDS {
            let mut rng = Rng::new(seed ^ 0x57a1_15e5_d0e5_c0de);
            let mut sweep = calm_sweep(&mut rng);
            let count = rng.between(over, over.saturating_add(70));
            let first = rng.below(span);
            // A stride of seven is coprime with 1,024, so the stalls are distinct.
            for step in 0..count {
                let index = first
                    .saturating_add(step.saturating_mul(7))
                    .checked_rem(span)
                    .ok_or("a sweep of no samples")?;
                let cost = stall_cost(&mut rng);
                stall_at(&mut sweep.path, index, cost)?;
            }
            let reason = refusal(seed, append_verdict(&sweep.path, &sweep.bare))?;
            assert!(
                reason.contains("not the host's"),
                "seed {seed} was refused for the wrong reason: {reason}"
            );
            trace.record_number("stalls", count);
            exercised = exercised.saturating_add(1);
        }
        Ok(Outcome { trace, exercised })
    })?;
    assert_eq!(refused, usize::try_from(SEEDS)?, "every seed was refused");
    Ok(())
}

/// Exactly 1% of the appends stalling leaves the p99 unstalled: the nearest-rank
/// p99 of 1,024 is the eleventh slowest, and ten stalls sit above it.
#[test]
fn ten_stalls_in_1024_appends_leave_the_p99_unstalled() {
    let path = flat(2_592, ONE_PERCENT);
    let bare = flat(2_592, 0);
    assert_eq!(stalls(&path), ONE_PERCENT);
    assert_eq!(append_verdict(&path, &bare), Ok(()));
}

/// Run 38929540150's shape: p99 at 453 ms means at least eleven stalls. Beside
/// a bare write that never stalled they are the path's.
#[test]
fn eleven_stalls_beside_a_calm_write_are_the_paths() -> TestResult {
    let path = flat(2_592, OVER_ONE_PERCENT);
    let bare = flat(2_592, 0);
    let reason = append_verdict(&path, &bare)
        .err()
        .ok_or("eleven stalls beside a calm write were passed")?;
    assert!(reason.contains("not the host's"), "{reason}");
    Ok(())
}

/// The same eleven stalls beside bare writes that stalled too, even once, are
/// the host's: the margin is [`STALL_RATIO`] times the bare count plus one.
#[test]
fn eleven_stalls_beside_a_stalling_write_are_the_hosts() {
    let path = flat(2_592, OVER_ONE_PERCENT);
    let bare = flat(2_592, 1);
    assert!(
        OVER_ONE_PERCENT < STALL_RATIO.saturating_mul(2),
        "one bare stall must outweigh eleven path stalls at this ratio"
    );
    assert_eq!(append_verdict(&path, &bare), Ok(()));
}

/// A bare write the clock reads as zero microseconds cannot make the typical
/// cost bound vacuous or divide by it: the bound floors at one.
#[test]
fn a_bare_write_that_reads_zero_still_bounds_the_path() -> TestResult {
    let bare = flat(0, 0);
    assert_eq!(append_verdict(&flat(TYPICAL_RATIO, 0), &bare), Ok(()));
    append_verdict(&flat(TYPICAL_RATIO.saturating_add(1), 0), &bare)
        .err()
        .ok_or("a path past the ratio was passed beside a zero-cost write")?;
    Ok(())
}

/// A sweep that sampled nothing is refused, never passed: no measurement is not
/// a healthy one.
#[test]
fn an_empty_sweep_is_refused_rather_than_passed() -> TestResult {
    let full = flat(2_592, 0);
    append_verdict(&[], &[])
        .err()
        .ok_or("two empty sweeps passed")?;
    append_verdict(&full, &[])
        .err()
        .ok_or("a missing control passed")?;
    append_verdict(&[], &full)
        .err()
        .ok_or("a missing path passed")?;
    Ok(())
}
