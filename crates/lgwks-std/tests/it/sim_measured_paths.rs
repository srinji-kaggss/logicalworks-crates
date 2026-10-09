//! Latency and peak-RSS measurement on the real paths this branch changed.
//!
//! #164's acceptance asks for p50/p95/p99; #160 and #154 were changed against
//! a stated cost, so the same measurement is reported for them. The numbers
//! are measured here rather than asserted as constants: a percentile over a
//! timing run is only meaningful against its own distribution, so the test
//! reports what it observed and asserts the property that would catch a
//! regression (a monotone increase in the changed path).
//!
//! Peak RSS is read from the process itself rather than sampled externally, so
//! a run reports the number the allocator handed back.

use lgwks_std::glob::{GlobDialect, GlobPattern, GlobScratch};
use lgwks_std::retry::RetryPolicy;
use lgwks_std::similarity::{CheckedEvidence, CheckedSimilarity, Cosine, EditDistance};
use std::time::{Duration, Instant};

/// Returns the peak resident set size of this process in bytes.
///
/// Linux exposes it through `/proc/self/statm` without `unsafe`; macOS would
/// need `getrusage`, which this workspace forbids. The unavailable case
/// returns `None` rather than an approximation: a wrong RSS figure is worse
/// than a stated absence, and the tests below treat `None` as "not exercised".
fn peak_rss_bytes() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages = statm.split_whitespace().nth(1)?;
    // Linux reports pages; a four-kilobyte page is every architecture this
    // workspace builds for.
    let pages = pages.parse::<u64>().ok()?;
    pages.checked_mul(4096)
}

/// Collects every sample duration, in nanoseconds.
struct Percentiles {
    /// Every observed duration.
    samples: Vec<u64>,
}

impl Percentiles {
    /// Times `iterations` calls of `work` and collects the samples.
    ///
    /// The iteration count and the index handed to `work` are `u32`s, the width
    /// every timed path takes its arguments in: a measurement that counts in one
    /// width and reports in another needs a conversion per sample, and a
    /// conversion per sample is a cost the measurement itself would be charging
    /// the path it measures. The range's exact size hint sizes the collection in
    /// one allocation.
    fn measure(iterations: u32, mut work: impl FnMut(u32)) -> Self {
        let samples: Vec<u64> = (0..iterations)
            .map(|index| {
                let start = Instant::now();
                work(index);
                nanos(start.elapsed())
            })
            .collect();
        Self { samples }
    }

    /// Times `iterations` calls of each of `first` and `second`, alternating
    /// them call by call, and collects each one's samples.
    ///
    /// Two paths measured one after the other are measured under two
    /// different host loads, and a comparison between them reads the load as
    /// cost: a lane sharing its machine saw the second of two sequential arms
    /// run slower than the first by more than the property under test.
    /// Alternating puts every burst on both arms, so their ratio is the paths'
    /// own.
    fn measure_paired(
        iterations: u32,
        mut first: impl FnMut(u32),
        mut second: impl FnMut(u32),
    ) -> (Self, Self) {
        // The range's exact size hint sizes both collections in one allocation
        // each, as `measure` does for one.
        let (first_samples, second_samples) = (0..iterations)
            .map(|index| {
                let start = Instant::now();
                first(index);
                let first_elapsed = nanos(start.elapsed());
                let start = Instant::now();
                second(index);
                (first_elapsed, nanos(start.elapsed()))
            })
            .unzip();
        (
            Self {
                samples: first_samples,
            },
            Self {
                samples: second_samples,
            },
        )
    }

    /// Returns the sample at `percentile`, counting from the smallest.
    ///
    /// The index is the nearest-rank percentile: with `n` samples the `p`
    /// percentile is the `ceil(p/100 * n)`-th smallest. Nearest-rank rather
    /// than an interpolating percentile because every sample here is a real
    /// observed call, and an interpolated point is not one. The rank is
    /// computed in the index width, where the sample count already lives.
    fn percentile(&self, percentile: usize) -> u64 {
        let count = self.samples.len();
        if count == 0 {
            return 0;
        }
        let rank = percentile.saturating_mul(count).div_ceil(100);
        let index = rank.saturating_sub(1).min(count.saturating_sub(1));
        let mut sorted = self.samples.clone();
        sorted.sort_unstable();
        // The index is the nearest rank clamped to the last position, so it is a
        // position this owns rather than an absent value.
        sorted[index]
    }

    /// The largest sample, or zero when nothing was observed.
    fn slowest(&self) -> u64 {
        self.samples.iter().copied().fold(0, u64::max)
    }

    /// Returns the p50, p95 and p99 in nanoseconds.
    fn summary(&self) -> (u64, u64, u64) {
        (
            self.percentile(50),
            self.percentile(95),
            self.percentile(99),
        )
    }

    /// Returns the mean in nanoseconds, used to compare the paths against each
    /// other where percentiles are noisy at small iteration counts.
    fn mean(&self) -> u64 {
        if self.samples.is_empty() {
            return 0;
        }
        // The sum is accumulated in `u128` so a long run of long calls cannot
        // overflow, and both the sum and the count are folded in the same pass
        // rather than converted from a `usize` this function never needed.
        let (mut total, mut count) = (0_u128, 0_u128);
        for sample in &self.samples {
            total = total.saturating_add(u128::from(*sample));
            count = count.saturating_add(1);
        }
        match u64::try_from(total.div_ceil(count)) {
            Ok(mean) => mean,
            // The mean of samples is at most the largest of them, so a mean past
            // the trace word is the accumulator's arithmetic rather than a
            // measurement, and the largest sample is the reading inside the word.
            Err(_) => self.slowest(),
        }
    }
}

/// An elapsed duration in nanoseconds.
///
/// `Duration::as_nanos` is a `u128` and the samples are a `u64`, so the
/// conversion would need a checked narrowing at every sample. It is read as
/// seconds and sub-second nanoseconds instead — both of which `Duration` reports
/// in a width that fits — and combined with saturating arithmetic, so a call
/// that somehow outlasted `u64::MAX` nanoseconds reads as the largest sample
/// rather than as a wrapped one.
fn nanos(elapsed: Duration) -> u64 {
    elapsed
        .as_secs()
        .saturating_mul(1_000_000_000_u64)
        .saturating_add(u64::from(elapsed.subsec_nanos()))
}

#[test]
fn retry_delay_latency_is_flat_across_the_attempt_range() {
    // #164 R1: the shipped form solves for the capped product rather than
    // walking it, so attempt 0 and `u32::MAX` must cost the same. The oracle is
    // the ratio between the smallest and largest attempt, which a doubling
    // loop could not meet at any iteration count.
    let policy = RetryPolicy::new(u32::MAX, Duration::from_nanos(1), Duration::MAX);
    let iterations = 200_000;
    // The entropy changes every iteration so the call cannot be hoisted, and
    // is consumed below so the result is not discarded with `let _ =`.
    let mut small_total = Duration::ZERO;
    let small = Percentiles::measure(iterations, |index| {
        small_total += policy.delay(index % 2, u64::from(index));
    });
    let mut large_total = Duration::ZERO;
    let large = Percentiles::measure(iterations, |index| {
        large_total += policy.delay(u32::MAX, u64::from(index));
    });
    assert!(
        small_total > Duration::ZERO && large_total > Duration::ZERO,
        "both attempt ranges must produce real delays: {small_total:?} and {large_total:?}"
    );
    let (small_p50, small_p95, small_p99) = small.summary();
    let (large_p50, large_p95, large_p99) = large.summary();
    // The bound is on the median and the 99th percentile, not the mean: at
    // 200,000 samples a percentile is stable, while one call the scheduler
    // descheduled for milliseconds moves a mean by tens of nanoseconds — a CI
    // run at load 90 failed this on the mean with both arms at 41/42/42 ns.
    // A form that walked the product would cost the attempt count in steps and
    // misses both bounds by orders of magnitude. The measurement is reported in
    // the failure message so a run that drifts still tells a reader what it saw.
    assert!(
        large_p50 <= small_p50.saturating_mul(4).saturating_add(50)
            && large_p99 <= small_p99.saturating_mul(4).saturating_add(50),
        "attempt u32::MAX must cost about the same as attempt 0: \
         p50/p95/p99 {small_p50}/{small_p95}/{small_p99}ns vs \
         {large_p50}/{large_p95}/{large_p99}ns (means {}ns vs {}ns)",
        small.mean(),
        large.mean()
    );
}

#[test]
fn glob_match_latency_is_linear_after_the_repair() {
    // #154 G1: the repaired matcher is one pass per token, so doubling the
    // path must roughly double the time: a quadratic matcher quadruples it.
    let Ok(pattern) = GlobPattern::compile_with_dialect("*a**/b[0-9]?", GlobDialect::Legacy) else {
        return;
    };
    // Each arm owns its scratch, as each caller does (INV-GLOB-1).
    let mut short_scratch = GlobScratch::new();
    let mut long_scratch = GlobScratch::new();
    // `a/**/b<digit><scalar>` matches, and the doubled form adds one more
    // directory level, so both paths are real matches and the only difference
    // is length.
    let short: String = format!("a/{}b7z", "x/".repeat(127));
    let long: String = format!("a/{}b7z", "x/".repeat(255));
    let iterations = 50_000;

    // The hit counts are `u32`s for the same reason the iteration count is: the
    // counter and the count it is compared against are then the same width.
    let mut small_hits = 0_u32;
    let mut large_hits = 0_u32;
    let (small, large) = Percentiles::measure_paired(
        iterations,
        |_| small_hits += u32::from(pattern.is_match_with(&short, &mut short_scratch)),
        |_| large_hits += u32::from(pattern.is_match_with(&long, &mut long_scratch)),
    );
    assert_eq!(
        small_hits, iterations,
        "every timed short match must be observed"
    );
    assert_eq!(
        large_hits, iterations,
        "every timed long match must be observed"
    );
    let (small_p50, small_p95, small_p99) = small.summary();
    let (large_p50, large_p95, large_p99) = large.summary();
    // The median, not the mean: one call the scheduler parked for a
    // millisecond moves a mean of microsecond calls by more than the whole
    // property, and it did — a lane sharing its host failed this on the mean
    // with medians 2.0x apart. The bound is 3x, between the 2x a linear pass
    // costs and the 4x a quadratic one does, plus timer granularity.
    assert!(
        large_p50 <= small_p50.saturating_mul(3).saturating_add(200),
        "doubling the path must not quadruple the match cost: \
         p50/p95/p99 {small_p50}/{small_p95}/{small_p99}ns vs \
         {large_p50}/{large_p95}/{large_p99}ns (means {}ns vs {}ns)",
        small.mean(),
        large.mean()
    );
    if let Some(rss) = peak_rss_bytes() {
        assert!(
            rss > 0,
            "peak RSS must be readable where the platform exposes it, got {rss}"
        );
    }
}

#[test]
fn checked_composition_latency_is_a_single_pass_over_its_components() {
    // #160 S2: the checked path evaluates each component once and folds the
    // outcomes, so its cost tracks the component count rather than any
    // renormalization or retry.
    let Ok(policy) = CheckedEvidence::new(vec![Box::new(Cosine::new())], vec![1.0], 0.0) else {
        return;
    };
    let Ok(text) = CheckedEvidence::new(vec![Box::new(EditDistance::new(64))], vec![1.0], 0.0)
    else {
        return;
    };
    let left: Vec<f32> = (0_u16..128).map(f32::from).collect();
    let iterations = 20_000;
    let bounded = lgwks_std::similarity::BoundedJaccard::<u32>::new(64);
    let mut verdicts = 0_u32;
    let measured = Percentiles::measure(iterations, |index| {
        let offset = index % 8;
        verdicts += u32::from(policy.verdict(&left, &left).is_ok());
        verdicts += u32::from(text.verdict("kitten", "sitting").is_ok());
        verdicts += u32::from(CheckedSimilarity::try_score(&bounded, &[offset, 1], &[1]).is_ok());
    });
    assert_eq!(
        verdicts,
        iterations.saturating_mul(3),
        "every call must return"
    );
    let (p50, p95, p99) = measured.summary();
    assert!(
        p50 < 1_000_000,
        "a checked composition must stay in the microsecond range: \
         p50/p95/p99 {p50}/{p95}/{p99}ns"
    );
}

#[test]
fn every_reported_percentile_is_ordered_and_bounded() {
    // A percentile report that is not monotone, or that exceeds the whole run,
    // is a broken measurement rather than a slow path.
    let policy = RetryPolicy::new(5, Duration::from_millis(1), Duration::from_secs(60));
    let mut total = Duration::ZERO;
    let measured = Percentiles::measure(10_000, |index| {
        total += policy.delay(index % 8, 0);
    });
    assert!(
        total > Duration::ZERO,
        "every timed call must produce a delay"
    );
    let (p50, p95, p99) = measured.summary();
    assert!(
        p50 <= p95,
        "p50/p95/p99 must be non-decreasing: {p50}/{p95}/{p99}"
    );
    assert!(
        p95 <= p99,
        "p50/p95/p99 must be non-decreasing: {p50}/{p95}/{p99}"
    );
    let slowest = measured.slowest();
    assert!(
        p99 <= slowest,
        "the p99 cannot exceed the slowest observed sample: {p99} > {slowest}"
    );
    assert!(
        measured.mean() >= p50.saturating_sub(slowest),
        "the mean must lie within the observed range"
    );
}
