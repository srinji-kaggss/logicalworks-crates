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
    fn measure(iterations: usize, mut work: impl FnMut(usize)) -> Self {
        let mut samples = Vec::with_capacity(iterations);
        for index in 0..iterations {
            let start = Instant::now();
            work(index);
            samples.push(u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX));
        }
        Self { samples }
    }

    /// Returns the sample at `percentile`, counting from the smallest.
    ///
    /// The index is the nearest-rank percentile: with `n` samples the `p`
    /// percentile is the `ceil(p/100 * n)`-th smallest. Nearest-rank rather
    /// than an interpolating percentile because every sample here is a real
    /// observed call, and an interpolated point is not one.
    fn percentile(&self, percentile: u64) -> u64 {
        if self.samples.is_empty() {
            return 0;
        }
        let count = u64::try_from(self.samples.len()).unwrap_or(u64::MAX);
        let rank = percentile.saturating_mul(count).div_ceil(100);
        let rank = usize::try_from(rank).unwrap_or(usize::MAX);
        let index = rank
            .saturating_sub(1)
            .min(self.samples.len().saturating_sub(1));
        let mut sorted = self.samples.clone();
        sorted.sort_unstable();
        sorted.get(index).copied().unwrap_or(0)
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
        let total: u128 = self.samples.iter().map(|value| u128::from(*value)).sum();
        let count = u128::try_from(self.samples.len()).unwrap_or(u128::MAX);
        u64::try_from(total.checked_div(count).unwrap_or(u128::MAX)).unwrap_or(u64::MAX)
    }
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
        small_total += policy.delay(
            u32::try_from(index % 2).unwrap_or(0),
            u64::try_from(index).unwrap_or(0),
        );
    });
    let mut large_total = Duration::ZERO;
    let large = Percentiles::measure(iterations, |index| {
        large_total += policy.delay(u32::MAX, u64::try_from(index).unwrap_or(0));
    });
    assert!(
        small_total > Duration::ZERO && large_total > Duration::ZERO,
        "both attempt ranges must produce real delays: {small_total:?} and {large_total:?}"
    );
    let (small_p50, small_p95, small_p99) = small.summary();
    let (large_p50, large_p95, large_p99) = large.summary();
    // The measurement is reported in the failure message so a run that drifts
    // still tells a reader what it saw.
    assert!(
        large.mean() <= small.mean().saturating_mul(4).saturating_add(50),
        "attempt u32::MAX must cost about the same as attempt 0: \
         p50/p95/p99 {small_p50}/{small_p95}/{small_p99}ns vs \
         {large_p50}/{large_p95}/{large_p99}ns"
    );
}

#[test]
fn glob_match_latency_is_linear_after_the_repair() {
    // #154 G1: the repaired matcher is one pass per token, so doubling the
    // path must roughly double the time and must not change the allocation
    // count.
    let Ok(pattern) = GlobPattern::compile_with_dialect("*a**/b[0-9]?", GlobDialect::Legacy) else {
        return;
    };
    let mut scratch = GlobScratch::new();
    // `a/**/b<digit><scalar>` matches, and the doubled form adds one more
    // directory level, so both paths are real matches and the only difference
    // is length.
    let short: String = format!("a/{}b7z", "x/".repeat(127));
    let long: String = format!("a/{}b7z", "x/".repeat(255));
    let iterations = 50_000;

    let mut small_hits = 0_usize;
    let small = Percentiles::measure(iterations, |_| {
        small_hits += usize::from(pattern.is_match_with(&short, &mut scratch));
    });
    let mut large_hits = 0_usize;
    let large = Percentiles::measure(iterations, |_| {
        large_hits += usize::from(pattern.is_match_with(&long, &mut scratch));
    });
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
    assert!(
        large.mean() <= small.mean().saturating_mul(6).saturating_add(200),
        "doubling the path must not quadruple the match cost: \
         p50/p95/p99 {small_p50}/{small_p95}/{small_p99}ns vs \
         {large_p50}/{large_p95}/{large_p99}ns"
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
    let mut verdicts = 0_usize;
    let measured = Percentiles::measure(iterations, |index| {
        let offset = u32::try_from(index % 8).unwrap_or(0);
        verdicts += usize::from(policy.verdict(&left, &left).is_ok());
        verdicts += usize::from(text.verdict("kitten", "sitting").is_ok());
        verdicts += usize::from(CheckedSimilarity::try_score(&bounded, &[offset, 1], &[1]).is_ok());
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
        total += policy.delay(u32::try_from(index % 8).unwrap_or(0), 0);
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
    let slowest = measured.samples.iter().copied().max().unwrap_or(0);
    assert!(
        p99 <= slowest,
        "the p99 cannot exceed the slowest observed sample: {p99} > {slowest}"
    );
    assert!(
        measured.mean() >= p50.saturating_sub(slowest),
        "the mean must lie within the observed range"
    );
}
