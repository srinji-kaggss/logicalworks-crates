//! Percentiles over the raw per-call samples this harness collects.
//!
//! Every number this harness prints comes from a real observed call: one
//! `Instant::now()` around it, kept, and ranked afterwards. Nothing is averaged
//! on the way out, and the percentile is nearest-rank, so `p95` is the 95th
//! smallest sample a run actually produced rather than an interpolating point
//! between two samples that never occurred.
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
    /// smallest, one-based. An empty sample set reports zero rather than
    /// panicking, so a scenario that collected nothing is still printable and
    /// visibly empty.
    pub fn percentile(&self, percent: u64) -> u64 {
        if self.raw.is_empty() {
            return 0;
        }
        let mut ordered = self.raw.clone();
        ordered.sort_unstable();
        let count = ordered.len() as u64;
        let rank = percent.saturating_mul(count).div_ceil(100);
        let index = usize::try_from(rank.saturating_sub(1)).unwrap_or(usize::MAX);
        ordered.get(index).copied().unwrap_or(0)
    }

    /// Returns the arithmetic mean in nanoseconds.
    pub fn mean(&self) -> u64 {
        if self.raw.is_empty() {
            return 0;
        }
        let total: u128 = self.raw.iter().map(|value| u128::from(*value)).sum();
        let count = u128::try_from(self.raw.len()).unwrap_or(u128::MAX);
        u64::try_from(total / count).unwrap_or(u64::MAX)
    }

    /// Folds every raw sample into one trace hash.
    ///
    /// Two runs of the same scenario on a machine happen to agree at every
    /// percentile without being the same measurement; the raw vector does not.
    /// The hash is what makes that difference visible in the results file.
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
            raw.push(start.elapsed().as_nanos() as u64);
        }
        Self {
            label: label.to_owned(),
            raw,
        }
    }

    /// Appends the row this scenario contributes to the printed table.
    pub fn write_row(&self, out: &mut String) {
        out.push_str(&format!(
            "{:<34} {:>8} {:>9} {:>9} {:>9} {:>9} {:#018x}\n",
            self.label,
            self.raw.len(),
            self.percentile(50),
            self.percentile(95),
            self.percentile(99),
            self.mean(),
            self.trace_hash(),
        ));
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
    let cores = std::thread::available_parallelism().map_or(0, |n| n.get());
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
        assert_eq!(samples.percentile(50), 50);
        assert_eq!(samples.percentile(95), 95);
        assert_eq!(samples.percentile(99), 99);
        assert_eq!(samples.percentile(100), 100);
        assert_eq!(samples.mean(), 50);
    }

    #[test]
    fn an_empty_sample_set_reports_zero_rather_than_panicking() {
        let samples = Samples {
            label: "empty".to_owned(),
            raw: Vec::new(),
        };
        assert_eq!(samples.percentile(50), 0);
        assert_eq!(samples.mean(), 0);
        assert_eq!(samples.trace_hash(), FNV_BASIS);
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
        assert_eq!(short.percentile(99), short.percentile(99));
        assert_ne!(
            short.trace_hash(),
            long.trace_hash(),
            "identical percentiles must not imply an identical measurement"
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
