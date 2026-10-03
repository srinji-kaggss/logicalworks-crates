//! Shared latency instrument for the example measurement harnesses.
//!
//! `resume_cost` and `poll_deadline_cost` both report p50/p95/p99 over a timed
//! sample, and the percentile arithmetic must be one definition: two copies
//! would be free to drift, and the whole point of a tail number is that the two
//! harnesses' numbers mean the same thing.
//!
//! Included by path, so it is a module of each example rather than a target of
//! its own: `#[path = "support/measure.rs"] mod measure;`. A file directly under
//! `examples/` would also be auto-discovered as an example and would need a
//! `main`; a file in a subdirectory is not.

/// A percentile of a sorted sample, by nearest rank.
///
/// Nearest rank rather than interpolation: the report then names a sample that
/// was actually observed, which is the property a reader of a latency claim
/// needs and an interpolated number does not have.
pub(crate) fn percentile(sorted: &[u128], per_mille: usize) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = sorted.len().saturating_mul(per_mille).div_ceil(1_000);
    sorted[rank.clamp(1, sorted.len()).saturating_sub(1)]
}

/// The percentiles of one measurement, in microseconds.
pub(crate) struct Summary {
    /// Median.
    p50: u128,
    /// 95th.
    p95: u128,
    /// 99th.
    p99: u128,
}

impl Summary {
    /// The percentiles of `samples`, sorted in place.
    pub(crate) fn of(samples: &mut [u128]) -> Self {
        samples.sort_unstable();
        Self {
            p50: percentile(samples, 500),
            p95: percentile(samples, 950),
            p99: percentile(samples, 990),
        }
    }

    /// The summary as one line.
    pub(crate) fn line(&self, label: &str) -> String {
        format!(
            "{label}: p50={}us p95={}us p99={}us",
            self.p50, self.p95, self.p99
        )
    }
}
