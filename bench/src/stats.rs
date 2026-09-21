//! Statistics for the rig. No external dependency: the arithmetic here is small
//! enough to own, and owning it means the methodology is auditable in one file
//! rather than delegated to a benchmarking framework's defaults.
//!
//! # Why a paired design
//!
//! Comparing two independent sample sets on one machine measures the machine as
//! much as the code: thermal drift, frequency scaling, and background load all
//! move a multi-second run. Every comparison this rig reports is therefore
//! **paired** -- within one round, engine A and engine B are timed back to back
//! on the same input window, and the unit of analysis is the *ratio* `a_i / b_i`
//! for round `i`, not the two means independently. Drift that affects both legs
//! of a round cancels in the ratio.
//!
//! # Why the median and not the mean
//!
//! Benchmark timings are right-skewed: the tail is scheduler preemption and
//! nothing else. The mean is dragged by that tail and reports a number no
//! invocation ever achieved. The median is the typical round and is what the
//! confidence interval below is computed for.

/// A seeded xorshift64* generator.
///
/// Seeded and reproducible on purpose: a confidence interval that moves between
/// runs of the same data is a second source of noise in a document about noise.
/// A statistical RNG is exactly the case `INV-RANDOM-ONE-SOURCE` carves out --
/// this is not a security primitive, it decides which rounds a resample draws,
/// and the seed is printed alongside every interval it produces.
pub struct Rng(u64);

impl Rng {
    /// Seed the generator. A zero seed is remapped, since xorshift is absorbing
    /// at zero and would return zero forever.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed })
    }

    /// The next raw 64-bit draw.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A uniform draw in `[0, 1)`, from the top 53 bits (the mantissa width of
    /// an `f64`), so every representable value in range is reachable.
    pub fn next_unit(&mut self) -> f64 {
        let bits = self.next_u64() >> 11;
        (bits as f64) / ((1u64 << 53) as f64)
    }

    /// A uniform index in `[0, n)`, or `0` when `n` is zero.
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        (self.next_u64() % (n as u64)) as usize
    }
}

/// Sort in place, then return the value at quantile `q` by linear
/// interpolation between the two nearest order statistics.
///
/// Interpolation rather than nearest-rank: with a round count in the low tens,
/// nearest-rank makes p99 identical to the maximum, which reports the worst
/// single round as though it were a percentile.
///
/// `q` is clamped to `[0, 1]`. An empty slice yields `f64::NAN`.
#[must_use]
pub fn quantile(values: &mut [f64], q: f64) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let q = q.clamp(0.0, 1.0);
    let last = values.len() - 1;
    let position = q * (last as f64);
    let lower = position.floor();
    let upper = position.ceil();
    // Both indices are `<= last` because `q <= 1`, and `position >= 0`.
    let lo = values[lower as usize];
    let hi = values[upper as usize];
    let weight = position - lower;
    lo + (hi - lo) * weight
}

/// The median, `quantile(0.5)`.
pub fn median(values: &mut [f64]) -> f64 {
    quantile(values, 0.5)
}

/// A percentile bootstrap confidence interval for the median.
///
/// Resamples `sample` with replacement `iterations` times, takes the median of
/// each resample, and returns the `(lower, upper)` order statistics at
/// `(1 - level) / 2` and `1 - (1 - level) / 2`.
///
/// The bootstrap is the right instrument here because the sampling distribution
/// of a median has no closed form that holds for the sample sizes this rig
/// produces. It assumes only that the rounds are exchangeable, which the paired
/// design is what buys.
#[must_use]
pub fn bootstrap_median_ci(
    sample: &[f64],
    iterations: usize,
    level: f64,
    seed: u64,
) -> (f64, f64) {
    if sample.is_empty() {
        return (f64::NAN, f64::NAN);
    }
    if sample.len() == 1 {
        return (sample[0], sample[0]);
    }
    let mut rng = Rng::new(seed);
    let mut medians = Vec::with_capacity(iterations);
    let mut resample = vec![0.0f64; sample.len()];
    for _ in 0..iterations {
        for slot in resample.iter_mut() {
            *slot = sample[rng.below(sample.len())];
        }
        medians.push(median(&mut resample));
    }
    let tail = (1.0 - level) / 2.0;
    let lo = quantile(&mut medians.clone(), tail);
    let hi = quantile(&mut medians, 1.0 - tail);
    (lo, hi)
}

/// Report a ratio's interval as "does it exclude 1.0?".
///
/// A ratio interval that contains `1.0` is an interval that does not distinguish
/// the two engines. Saying so plainly is the point: the alternative is quoting a
/// ratio whose interval spans parity and letting a reader assume it does not.
#[must_use]
pub fn distinguishes_parity(lower: f64, upper: f64) -> bool {
    !(lower <= 1.0 && upper >= 1.0)
}
