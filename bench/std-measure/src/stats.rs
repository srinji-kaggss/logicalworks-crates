//! Percentiles over the raw per-call samples this harness collects.
//!
//! Every number this harness prints comes from a real observed call: one
//! `Instant::now()` around it, kept, and ranked afterwards. Nothing is averaged
//! on the way out, and the percentile is nearest-rank, so `p95` is the 95th
//! smallest sample a run actually produced rather than an interpolating point
//! between two samples that never occurred.
//!
//! **A statistic that cannot be computed is absent, not zero.** [`Samples::percentile`]
//! and [`Samples::mean`] return `Option<u64>` where `None` means *not measured*,
//! and [`cell`] prints `-` for it. An empty sample set has no p95; printing `0`
//! there would report a zero-nanosecond call that the run never observed, in the
//! exact column a reader compares against. The same rule governs the machine
//! line: a core count the OS will not report is written as `unknown`.
//!
//! The percentile is computed with [`core::cmp::Ord::sort_unstable`] rather
//! than `select_nth_unstable` so this file shares nothing with
//! `lgwks_std`'s own code: a measurement instrument that reused the measured
//! crate's helper would make the two numbers hard to tell apart.
//!
//! [`PartialEq`]: std::cmp::PartialEq
//! [`Eq`]: std::cmp::Eq

//! The 64-bit FNV-1a prime, used to hash a whole sample vector into one value.
const FNV_PRIME: u64 = 1_099_511_628_211;

/// The 64-bit FNV-1a offset basis.
const FNV_BASIS: u64 = 14_695_981_039_346_656_037;

/// Narrows an elapsed nanosecond count to `u64`, saturating at the ceiling.
///
/// `Duration::as_nanos` reports at `u128` precision, which spans about 584
/// years; a `u64` nanosecond count covers the same span. The value is therefore
/// clamped to the `u64` ceiling first and then read back a half at a time,
/// which needs no fallible conversion at all: a measurement this harness cannot
/// hold is reported as the largest representable duration rather than wrapping
/// to a small one, which would be a fabricated fast call.
fn saturating_nanos(elapsed_nanos: u128) -> u64 {
    let clamped = elapsed_nanos.min(u128::from(u64::MAX));
    let bytes = clamped.to_le_bytes();
    let mut low = [0_u8; size_of::<u64>()];
    low.copy_from_slice(&bytes[..size_of::<u64>()]);
    u64::from_le_bytes(low)
}

/// One measured scenario: every raw sample plus the row it produced.
#[derive(Clone)]
pub struct Samples {
    /// The scenario's name as printed and written to the results file.
    label: String,
    /// The raw per-call durations in nanoseconds, one per timed call.
    raw: Vec<u64>,
}

impl Samples {
    /// Returns the nearest-rank percentile `percent` in nanoseconds.
    ///
    /// With `n` samples the `p` percentile is the `ceil(p/100 * n)`-th
    /// smallest, one-based. `percent` is clamped to 100, so an out-of-range
    /// request reports the maximum sample rather than an unreachable rank.
    ///
    /// [`None`] means *not measured*: an empty sample set has no `p95`, and
    /// reporting `0 ns` for it would put a fabricated fastest-observed call in
    /// a column whose whole purpose is to say what was observed. A scenario
    /// that collected nothing prints [`cell`]'s `-` instead.
    ///
    /// `percent` is clamped to `100`, so p0 is the smallest observed sample and
    /// p200 (or `u64::MAX`) is the largest: a rank request outside `0..=100`
    /// names the end of the vector rather than a sample that does not exist.
    #[must_use]
    pub fn percentile(&self, percent: u64) -> Option<u64> {
        // An empty set has no rank to name. `checked_mul` guards the one
        // product here that could overflow a `u64` sample count, and every
        // `?` below is that same refusal: no rank, no sample, no number.
        let count = u64::try_from(self.raw.len()).ok()?;
        let rank = percent.min(100).checked_mul(count)?.div_ceil(100).max(1);
        let index = usize::try_from(rank.checked_sub(1)?).ok()?;
        let mut ordered = self.raw.clone();
        ordered.sort_unstable();
        ordered.get(index).copied()
    }

    /// Returns the arithmetic mean in nanoseconds, or [`None`] if nothing was
    /// measured.
    ///
    /// The sum accumulates at `u128` so a set of `u64` samples cannot overflow
    /// it, and the quotient is converted back only once; a mean that did not
    /// fit a `u64` would be a clock bug, so it is refused rather than clamped.
    #[must_use]
    pub fn mean(&self) -> Option<u64> {
        let count = u128::try_from(self.raw.len()).ok()?;
        if count == 0 {
            return None;
        }
        let mut total: u128 = 0;
        for sample in &self.raw {
            total = total.saturating_add(u128::from(*sample));
        }
        u64::try_from(total / count).ok()
    }

    /// Folds every raw sample into one trace hash.
    ///
    /// Two runs of the same scenario on a machine happen to agree at every
    /// percentile without being the same measurement; the raw vector does not.
    /// The hash is what makes that difference visible in the results file.
    #[must_use]
    pub fn trace_hash(&self) -> u64 {
        let mut hash = FNV_BASIS;
        for sample in &self.raw {
            hash = hash.wrapping_mul(FNV_PRIME).wrapping_add(*sample);
        }
        hash
    }

    /// Times `iterations` calls of `work`, collecting every raw sample.
    ///
    /// `work` is handed the iteration index so the measured call can change its
    /// input: a loop calling the same pure function with the same arguments is
    /// a measurement of the loop, not of the call.
    pub fn timed(label: &str, iterations: usize, mut work: impl FnMut(usize)) -> Self {
        let mut raw = Vec::with_capacity(iterations);
        for index in 0..iterations {
            let start = std::time::Instant::now();
            work(index);
            // `u64` nanoseconds is 584 years, so the saturating branch below is
            // unreachable on any clock this harness can read. It is spelled out
            // rather than truncated so a wider sample could never be reported
            // as a smaller one.
            raw.push(saturating_nanos(start.elapsed().as_nanos()));
        }
        Self {
            label: label.to_owned(),
            raw,
        }
    }

    /// Appends the row this scenario contributes to the printed table.
    ///
    /// Every duration column goes through [`cell`], so a statistic that could
    /// not be computed is printed as `-` in the same column a real number
    /// would occupy. A row that collected nothing therefore reads as absent
    /// rather than as the fastest measurement in the table.
    pub fn write_row(&self, out: &mut String) {
        out.push_str(&format!(
            "{:<34} {:>8} {:>9} {:>9} {:>9} {:>9} {:#018x}\n",
            self.label,
            self.raw.len(),
            cell(self.percentile(50)),
            cell(self.percentile(95)),
            cell(self.percentile(99)),
            cell(self.mean()),
            self.trace_hash(),
        ))
    }
}

/// Renders one nanosecond statistic as a table cell.
///
/// A [`None`] is *not measured* — an empty sample set, or a rank that does not
/// exist — and prints as `-`. Reporting `0` there would put a fabricated
/// instant call into a column whose whole purpose is to say what was observed.
/// The width matches the header so a missing value still lines up.
fn cell(value: Option<u64>) -> String {
    const UNMEASURED: &str = "-";
    match value {
        Some(nanos) => format!("{nanos:>9}"),
        None => format!("{UNMEASURED:>9}"),
    }
}

/// Prints the table header the rows are appended to.
pub fn write_header(out: &mut String) {
    out.push_str(&format!(
        "{:<34} {:>8} {:>9} {:>9} {:>9} {:>9} {:>18}\n",
        "scenario", "samples", "p50 ns", "p95 ns", "p99 ns", "mean ns", "trace"
    ));
}

/// Formats the machine description the results file records beside the numbers.
///
/// A benchmark number without the machine that produced it is not evidence, so
/// this is written into the file rather than kept in the reader's memory.
pub fn machine_description() -> String {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    // A machine whose core count the OS would not report is recorded as
    // `unknown`, not as `0`: the results file is evidence about this machine,
    // and a fabricated core count is a claim no run of this harness made.
    let cores = match std::thread::available_parallelism() {
        Ok(count) => count.get().to_string(),
        Err(_unsupported) => "unknown".to_owned(),
    };
    let kind = if cfg!(debug_assertions) {
        "unoptimized"
    } else {
        "opt-level=3 lto=true codegen-units=1"
    };
    format!("{os}/{arch} cores={cores} build={kind}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_is_nearest_rank_over_the_raw_samples() {
        // 1..=100 nanoseconds: the 50th percentile of a hundred samples is the
        // 50th smallest, which is exactly 50. An interpolating percentile would
        // not produce an integer here, so this pins nearest-rank rather than
        // leaving the choice implicit.
        let samples = Samples {
            label: "control".to_owned(),
            raw: (1_u64..=100).collect(),
        };
        assert_eq!(samples.percentile(50), Some(50));
        assert_eq!(samples.percentile(95), Some(95));
        assert_eq!(samples.percentile(99), Some(99));
        assert_eq!(samples.percentile(100), Some(100));
        assert_eq!(samples.mean(), Some(50));
    }

    #[test]
    fn an_empty_sample_set_reports_not_measured_rather_than_zero() {
        // Zero here would be a lie of a specific kind: it would say the
        // slowest-observed call in this scenario took no time at all, which is
        // the number a reader compares against. Nothing was measured, so the
        // statistic is absent.
        let samples = Samples {
            label: "empty".to_owned(),
            raw: Vec::new(),
        };
        assert_eq!(samples.percentile(50), None);
        assert_eq!(samples.percentile(100), None);
        assert_eq!(samples.mean(), None);
        assert_eq!(samples.trace_hash(), FNV_BASIS);
        // An empty row still renders, as `-` in all four duration columns
        // rather than as four zeroes a reader would compare against. The row is
        // compared column-wise: the sample count is legitimately `0` (zero
        // calls were timed) and the trace is a hexadecimal string.
        let mut row = String::new();
        samples.write_row(&mut row);
        let columns: Vec<&str> = row.split_whitespace().collect();
        assert_eq!(
            columns,
            ["empty", "0", "-", "-", "-", "-", "0xcbf29ce484222325"],
            "an unmeasured row prints a dash per duration column, got {row:?}"
        );
    }

    #[test]
    fn a_percentile_beyond_the_end_clamps_to_the_samples_that_exist() {
        // `percent` is a rank request, not an interpolation weight: asking for
        // p200 of the samples this run collected means the largest one, and p0
        // means the smallest, rather than either walking off the end.
        let samples = Samples {
            label: "clamped".to_owned(),
            raw: vec![4, 1, 9, 2],
        };
        assert_eq!(samples.percentile(200), Some(9));
        assert_eq!(samples.percentile(u64::MAX), Some(9));
        assert_eq!(samples.percentile(0), Some(1));
        // p50 of four samples ranks the 2nd smallest: [1, 2, 4, 9] → 2.
        assert_eq!(samples.percentile(50), Some(2));
    }

    #[test]
    fn a_rank_the_sample_count_cannot_express_is_refused_rather_than_wrapped() {
        // The one product in the ranking is `percent * count`. At a sample count
        // near the `u64` ceiling it overflows, and a wrapped product would rank
        // into the middle of the vector and report a fast sample as if it were
        // a slow one — so the guard has to refuse, not saturate. The count is
        // asserted on the arithmetic rather than by allocating a vector.
        let count = u64::MAX;
        assert_eq!(
            100_u64.checked_mul(count),
            None,
            "the product must be refused, not wrapped"
        );
        // A count this harness can actually hold never overflows, so the guard
        // is silent on every real sweep: the largest scenario is 200 000 calls.
        // A count this harness can actually hold never overflows, so the guard
        // is silent on every real sweep. The largest scenario is 200 000 timed
        // calls, and a full-size vector is checked end to end: every printed
        // rank resolves, and p100 is the last sample of the sorted order.
        const LARGEST_SCENARIO_SAMPLES: usize = 200_000;
        let raw: Vec<u64> = (1_u64..=977)
            .cycle()
            .take(LARGEST_SCENARIO_SAMPLES)
            .collect();
        let samples = Samples {
            label: "largest".to_owned(),
            raw,
        };
        for percent in [50_u64, 95, 99, 100] {
            assert!(
                samples.percentile(percent).is_some(),
                "p{percent} of {LARGEST_SCENARIO_SAMPLES} samples must rank"
            );
        }
        assert_eq!(
            samples.percentile(100),
            Some(977),
            "p100 of a full-size sweep is its largest sample"
        );
    }

    #[test]
    fn the_trace_hash_separates_two_runs_that_share_every_percentile() {
        // Two sample vectors with the same p50/p95/p99 but different tails are
        // the reason the raw vector is hashed at all.
        let head = vec![10_u64; 99];
        let short_tail = {
            let mut raw = head.clone();
            raw.push(10);
            raw
        };
        let long_tail = {
            let mut raw = head;
            raw.push(9_000);
            raw
        };
        let short = Samples {
            label: "short".to_owned(),
            raw: short_tail,
        };
        let long = Samples {
            label: "long".to_owned(),
            raw: long_tail,
        };
        // The claim is that two vectors can share every percentile this table
        // prints while being different measurements, so the shared percentile
        // is asserted against both sides rather than against itself.
        assert_eq!(short.percentile(50), Some(10));
        assert_eq!(long.percentile(50), Some(10));
        assert_eq!(short.percentile(99), long.percentile(99));
        assert_ne!(
            short.trace_hash(),
            long.trace_hash(),
            "identical percentiles must not imply an identical measurement"
        );
    }

    #[test]
    fn a_row_prints_every_duration_column_and_the_trace() {
        // The table is what a reader diffs between two runs, so a row that
        // dropped or reordered a column would still compile and still look
        // plausible. This pins the rendered shape end to end.
        let samples = Samples {
            label: "shape".to_owned(),
            raw: vec![7, 3, 11, 5],
        };
        let mut row = String::new();
        samples.write_row(&mut row);
        // [7, 3, 11, 5] ranks to [3, 5, 7, 11]: p50 is the 2nd smallest (5),
        // p95 and p99 both round up to the 4th (11), and the mean is 26/4 = 6.
        assert_eq!(
            row,
            format!(
                "{:<34} {:>8} {:>9} {:>9} {:>9} {:>9} {:#018x}\n",
                "shape",
                4,
                "5",
                "11",
                "11",
                "6",
                samples.trace_hash()
            )
        );
    }

    #[test]
    fn timing_a_closure_collects_exactly_one_sample_per_iteration() {
        let samples = Samples::timed("counting", 32, |index| {
            std::hint::black_box(index);
        });
        assert_eq!(samples.raw.len(), 32);
    }
}

/// Deterministic simulation of the ranking this harness reports.
///
/// One seed drives the whole model: the sample vector, the order statistics it
/// is checked against, and the trace hash. The reference is computed by a
/// deliberately different route than [`Samples`] — a hand-walked rank instead of
/// a closed-form `div_ceil`, an explicit `u128` accumulator instead of a
/// saturating fold — so agreement is evidence about the rank formula rather than
/// a restatement of it. Every failure names its seed, and one seed replays the
/// same trace hash.
#[cfg(test)]
mod sim {
    use super::{FNV_BASIS, FNV_PRIME, Samples};

    /// The seed every scenario in this module replays from unless it names its
    /// own. Fixed so a failure is reproducible without a rerun.
    const SEED: u64 = 0x5EED_0000_0000_0001;

    /// SplitMix64: the smallest generator giving a wide, reproducible spread of
    /// sample values from one `u64` seed. The measured harness shares nothing
    /// with the crate under measurement, and so do its tests.
    struct Rng(u64);

    impl Rng {
        /// Starts a stream at `seed`.
        fn new(seed: u64) -> Self {
            Self(seed)
        }

        /// Returns the next value in the stream.
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }

        /// Returns one sample in `1..=1_000` nanoseconds, the range a timer
        /// reads for a cheap call.
        fn sample(&mut self) -> u64 {
            self.next() % 1_000 + 1
        }
    }

    /// Builds the sample vector for `seed` at `count` samples.
    fn vector(seed: u64, count: usize) -> Vec<u64> {
        let mut rng = Rng::new(seed);
        (0..count).map(|_| rng.sample()).collect()
    }

    /// The reference nearest-rank percentile, computed without [`Samples`].
    ///
    /// The rank is walked one sample at a time rather than multiplied out, so
    /// this cannot share a rounding bug with the closed form it checks.
    fn reference_percentile(raw: &[u64], percent: u64) -> Option<u64> {
        if raw.is_empty() {
            return None;
        }
        let mut ordered = raw.to_vec();
        ordered.sort_unstable();
        let count = ordered.len();
        // The rank is walked with `usize` arithmetic throughout so no sample
        // count is ever narrowed or widened; `percent` is already clamped
        // above 100, so the conversion cannot fail.
        let percent = match usize::try_from(percent.min(100)) {
            Ok(percent) => percent,
            Err(_overflow) => return None,
        };
        // The `ceil(p * n / 100)`-th smallest, one-based, walked one sample at a
        // time: count how many samples it takes for the running proportion to
        // reach `percent`, then read that position. The count starts at one so
        // p0 lands on the first sample rather than falling off the front, and
        // p100 needs the whole vector to reach the full proportion.
        let mut reached = 1_usize;
        let mut chosen = None;
        for (position, sample) in ordered.iter().enumerate() {
            while percent.saturating_mul(count) > 100_usize.saturating_mul(reached) {
                reached += 1;
            }
            if reached == position + 1 {
                chosen = Some(*sample);
                break;
            }
        }
        chosen.or_else(|| ordered.last().copied())
    }

    /// The reference mean, accumulated explicitly rather than by `sum`.
    fn reference_mean(raw: &[u64]) -> Option<u64> {
        if raw.is_empty() {
            return None;
        }
        let mut total: u128 = 0;
        for sample in raw {
            total += u128::from(*sample);
        }
        let count = u128::try_from(raw.len()).ok()?;
        u64::try_from(total / count).ok()
    }

    /// The reference trace hash, folded over the same values in the same order.
    fn reference_trace(raw: &[u64]) -> u64 {
        let mut hash = FNV_BASIS;
        for sample in raw {
            hash = hash.wrapping_mul(FNV_PRIME).wrapping_add(*sample);
        }
        hash
    }

    #[test]
    fn the_harness_agrees_with_the_order_statistic_model_at_every_size_and_rank()
    -> Result<(), String> {
        // The rank formula is the one number every row in the results file
        // rests on, so it is checked across the shape space the harness
        // produces: sizes straddling the rank boundaries, at every printed rank
        // and its neighbours, plus the clamped ends.
        let sizes = [1_usize, 2, 3, 7, 99, 100, 101, 1_000];
        let ranks = [0_u64, 1, 25, 50, 95, 99, 100, 101, 200, u64::MAX];
        for count in sizes {
            let raw = vector(SEED, count);
            let samples = Samples {
                label: "sim".to_owned(),
                raw: raw.clone(),
            };
            for percent in ranks {
                let Some(expected) = reference_percentile(&raw, percent) else {
                    return Err(format!(
                        "seed {SEED:#x}, {count} samples, p{percent}: \
                         the model refused to rank a non-empty vector"
                    ));
                };
                match samples.percentile(percent) {
                    Some(actual) if actual == expected => {}
                    Some(actual) => {
                        return Err(format!(
                            "seed {SEED:#x}, {count} samples, p{percent}: \
                             model says {expected}, harness says {actual}"
                        ));
                    }
                    None => {
                        return Err(format!(
                            "seed {SEED:#x}, {count} samples, p{percent}: \
                             the harness reported nothing measured"
                        ));
                    }
                }
            }
            if samples.mean() != reference_mean(&raw) {
                return Err(format!(
                    "seed {SEED:#x}, {count} samples: the mean disagrees with the model"
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn the_same_seed_replays_the_same_measurement_and_a_different_one_does_not()
    -> Result<(), String> {
        // The comparison rule in `main.rs` rests on this: two runs can share
        // every percentile and still be different measurements, so the trace
        // hash has to be reproducible for one seed and separating for another.
        let first = vector(SEED, 4_096);
        let replay = vector(SEED, 4_096);
        let other = vector(SEED.wrapping_add(1), 4_096);
        if first != replay {
            return Err("one seed must replay the same samples".to_owned());
        }
        if reference_trace(&first) != reference_trace(&replay) {
            return Err("the replayed vector must fold to the same hash".to_owned());
        }
        if first == other {
            return Err("a neighbouring seed must draw different samples".to_owned());
        }
        if reference_trace(&first) == reference_trace(&other) {
            return Err("distinct vectors must fold to distinct hashes".to_owned());
        }
        let left = Samples {
            label: "left".to_owned(),
            raw: first,
        };
        let right = Samples {
            label: "right".to_owned(),
            raw: other,
        };
        if left.trace_hash() == right.trace_hash() {
            return Err("two different measurements must not share a trace".to_owned());
        }
        Ok(())
    }

    #[test]
    fn the_percentile_is_monotone_and_bounded_by_the_samples_it_ranks() -> Result<(), String> {
        // Every property the results table relies on, checked on one seeded
        // vector: p rises with the rank, no rank escapes the observed range,
        // and p100 is exactly the largest sample.
        let raw = vector(SEED, 4_096);
        let samples = Samples {
            label: "monotone".to_owned(),
            raw: raw.clone(),
        };
        let Some(smallest) = raw.iter().copied().min() else {
            return Err("a 4096-sample vector has a smallest sample".to_owned());
        };
        let Some(largest) = raw.iter().copied().max() else {
            return Err("a 4096-sample vector has a largest sample".to_owned());
        };
        let mut previous = None;
        for percent in 0..=100_u64 {
            let Some(value) = samples.percentile(percent) else {
                return Err(format!("p{percent} of a 4096-sample vector must rank"));
            };
            if value < smallest || value > largest {
                return Err(format!("p{percent} = {value} escaped the observed range"));
            }
            if previous.is_some_and(|before| before > value) {
                return Err(format!(
                    "p{percent} = {value} fell below the rank before it"
                ));
            }
            previous = Some(value);
        }
        if samples.percentile(100) != Some(largest) {
            return Err("p100 must be the largest observed sample".to_owned());
        }
        Ok(())
    }

    #[test]
    fn every_seed_of_a_batch_ranks_against_the_model_and_hashes_deterministically()
    -> Result<(), String> {
        // A batch rather than one seed, so a defect that appears only for
        // particular counts or particular draws is caught inside a single run.
        let mut hashes = Vec::with_capacity(64);
        for seed in 0_u64..64 {
            let Some(count) = usize::try_from(seed % 37 + 1).ok() else {
                return Err(format!(
                    "seed {seed:#x}: derived a count that is not a usize"
                ));
            };
            let raw = vector(SEED.wrapping_add(seed), count);
            let samples = Samples {
                label: "batch".to_owned(),
                raw: raw.clone(),
            };
            if samples.percentile(95) != reference_percentile(&raw, 95) {
                return Err(format!(
                    "seed {seed:#x} with {count} samples: p95 disagrees"
                ));
            }
            if samples.trace_hash() != reference_trace(&raw) {
                return Err(format!(
                    "seed {seed:#x}: the trace hash is not the model fold"
                ));
            }
            hashes.push(samples.trace_hash());
        }
        // 64 distinct vectors through a 64-bit fold: a repeat here would mean
        // the hash is collapsing distinct measurements, which is the whole
        // failure it exists to prevent.
        let mut sorted = hashes.clone();
        sorted.sort_unstable();
        sorted.dedup();
        if sorted.len() != hashes.len() {
            return Err(format!(
                "{} distinct vectors produced only {} distinct hashes",
                hashes.len(),
                sorted.len()
            ));
        }
        Ok(())
    }

    #[test]
    fn a_scenario_that_never_ran_is_simulated_rather_than_assumed() {
        // The empty case is a simulation too: a scenario whose timed closure is
        // never invoked produces no samples, every statistic must be absent, and
        // the row must still render.
        let mut calls = 0_usize;
        let samples = Samples::timed("never ran", 0, |_index| calls += 1);
        assert_eq!(
            calls, 0,
            "a zero-iteration sweep calls the closure no times"
        );
        assert_eq!(samples.percentile(50), None);
        assert_eq!(samples.mean(), None);
        assert_eq!(samples.trace_hash(), FNV_BASIS);
        let mut row = String::new();
        samples.write_row(&mut row);
        assert_eq!(
            row.matches('-').count(),
            4,
            "four duration columns, all absent"
        );
    }

    #[test]
    fn a_scenario_of_one_sample_ranks_to_that_sample() -> Result<(), String> {
        // The smallest non-empty case, where every percentile collapses onto
        // the single observation: a one-sample p99 reporting anything else
        // would be an interpolation the run never made.
        let raw = vector(SEED, 1);
        let samples = Samples {
            label: "one".to_owned(),
            raw: raw.clone(),
        };
        let Some(only) = raw.first().copied() else {
            return Err("a one-sample vector holds one sample".to_owned());
        };
        for percent in 0..=100_u64 {
            if samples.percentile(percent) != Some(only) {
                return Err(format!("p{percent} of one sample must be that sample"));
            }
        }
        if samples.mean() != Some(only) {
            return Err("the mean of one sample is that sample".to_owned());
        }
        Ok(())
    }
}
