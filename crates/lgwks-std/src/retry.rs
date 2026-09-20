//! `retry` owns zero-dependency retry budgets for networked callers.
//!
//! A [`RetryPolicy`](crate::retry::RetryPolicy) is a pure value: attempts,
//! exponential backoff, and a total deadline. It performs no I/O, spawns no
//! threads, and holds no clock — the caller sleeps (via
//! [`task`](crate::task) synchronously or `lgwks_bot::rt::time`
//! asynchronously) and checks
//! [`deadline_exceeded`](crate::retry::RetryPolicy::deadline_exceeded)
//! against its own clock. That split keeps this module in `core` with zero
//! external dependencies and makes budgets explicit at every hop instead of
//! smuggled inside a client.
//!
//! Jitter is caller-supplied (`u64` entropy the caller already holds, e.g.
//! from [`random`](crate::random)) so `core` stays free of entropy sources:
//!
//! ```rust
//! use std::time::Duration;
//! use lgwks_std::retry::RetryPolicy;
//!
//! let policy = RetryPolicy::new(3, Duration::from_millis(100), Duration::from_secs(5));
//! assert_eq!(policy.delay(0, 0), Duration::from_millis(100));
//! assert_eq!(policy.delay(1, 0), Duration::from_millis(200));
//! // Full jitter only ever shortens the backoff, never lengthens it.
//! assert!(policy.delay(1, u64::MAX) <= Duration::from_millis(200));
//! assert!(!policy.deadline_exceeded(Duration::from_secs(4)));
//! assert!(policy.deadline_exceeded(Duration::from_secs(6)));
//! ```
//!
//! INV-RETRY-PURE: [`RetryPolicy`](crate::retry::RetryPolicy) never sleeps, never reads a clock, never
//! allocates. `delay` is O(1) with saturating arithmetic, so a hostile attempt
//! count cannot overflow into a panic or an unbounded sleep.

use std::time::Duration;

/// How a caller spaces repeated attempts of one fallible operation.
///
/// All fields are plain data so policies can cross crate boundaries (JSON,
/// manifests, config files) without dragging an executor along.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Maximum attempts including the first try. `0` is normalised to `1`:
    /// a policy always permits the initial attempt, never zero work.
    pub max_attempts: u32,
    /// Backoff after the first failure; doubled per subsequent attempt.
    pub base_delay: Duration,
    /// Upper bound on any single backoff, before jitter is applied.
    pub max_delay: Duration,
    /// Total budget across all attempts, measured by the caller.
    pub deadline: Duration,
}

impl RetryPolicy {
    /// A policy of `max_attempts` tries starting at `base_delay` backoff with
    /// a `deadline` total budget. The per-backoff cap defaults to 30 seconds;
    /// use [`with_max_delay`](crate::retry::RetryPolicy::with_max_delay) to narrow it.
    #[must_use]
    pub fn new(max_attempts: u32, base_delay: Duration, deadline: Duration) -> Self {
        Self {
            max_attempts: max_attempts.max(1),
            base_delay,
            max_delay: Duration::from_secs(30),
            deadline,
        }
    }

    /// Cap any single backoff at `max_delay` (applied before jitter).
    #[must_use]
    pub fn with_max_delay(mut self, max_delay: Duration) -> Self {
        self.max_delay = max_delay;
        self
    }

    /// Backoff before attempt `attempt` (0-based failures so far), shortened
    /// by full jitter derived from `jitter_entropy`: the returned delay is
    /// `backoff - (entropy % (backoff + 1))`, so jitter only ever shrinks the
    /// wait. Saturating shift arithmetic keeps hostile attempt counts O(1)
    /// and panic-free.
    #[must_use]
    pub fn delay(&self, attempt: u32, jitter_entropy: u64) -> Duration {
        let shift = attempt.min(31);
        let backoff = self
            .base_delay
            .checked_mul(1u32 << shift)
            .unwrap_or(self.max_delay)
            .min(self.max_delay);
        if backoff.is_zero() {
            return Duration::ZERO;
        }
        let nanos = backoff.as_nanos().min(u64::MAX as u128) as u64;
        let jitter = jitter_entropy % nanos.saturating_add(1);
        Duration::from_nanos(nanos.saturating_sub(jitter))
    }

    /// Whether `elapsed` has consumed the total `deadline` budget.
    #[must_use]
    pub fn deadline_exceeded(&self, elapsed: Duration) -> bool {
        elapsed >= self.deadline
    }

    /// Whether another attempt is allowed after `failures` failures.
    #[must_use]
    pub fn should_retry(&self, failures: u32, elapsed: Duration) -> bool {
        failures < self.max_attempts && !self.deadline_exceeded(elapsed)
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_caps() {
        let policy = RetryPolicy::new(5, Duration::from_millis(100), Duration::from_secs(60));
        assert_eq!(policy.delay(0, 0), Duration::from_millis(100));
        assert_eq!(policy.delay(1, 0), Duration::from_millis(200));
        assert_eq!(policy.delay(2, 0), Duration::from_millis(400));
        let capped = RetryPolicy::new(5, Duration::from_secs(20), Duration::from_secs(600))
            .with_max_delay(Duration::from_secs(25));
        assert_eq!(capped.delay(3, 0), Duration::from_secs(25));
    }

    #[test]
    fn jitter_only_shrinks() {
        let policy = RetryPolicy::new(4, Duration::from_millis(200), Duration::from_secs(60));
        for entropy in [0u64, 1, 7, 1_000, u64::MAX] {
            let d = policy.delay(2, entropy);
            assert!(
                d <= Duration::from_millis(800),
                "jitter grew backoff: {d:?}"
            );
        }
        assert_eq!(policy.delay(2, 0), Duration::from_millis(800));
    }

    #[test]
    fn zero_attempts_normalises_to_one_try() {
        let policy = RetryPolicy::new(0, Duration::from_millis(10), Duration::from_secs(1));
        assert_eq!(policy.max_attempts, 1);
        assert!(policy.should_retry(0, Duration::ZERO));
        assert!(!policy.should_retry(1, Duration::ZERO));
    }

    #[test]
    fn huge_attempt_counts_cannot_overflow_or_hang() {
        let policy = RetryPolicy::new(u32::MAX, Duration::from_millis(1), Duration::MAX);
        let d = policy.delay(u32::MAX, u64::MAX);
        assert!(d <= Duration::from_secs(30));
    }

    #[test]
    fn deadline_gates_retries() {
        let policy = RetryPolicy::new(10, Duration::from_millis(50), Duration::from_secs(5));
        assert!(policy.should_retry(9, Duration::from_secs(4)));
        assert!(!policy.should_retry(9, Duration::from_secs(5)));
        assert!(!policy.should_retry(10, Duration::ZERO));
    }

    #[test]
    fn zero_base_delay_stays_zero() {
        let policy = RetryPolicy::new(3, Duration::ZERO, Duration::from_secs(1));
        assert_eq!(policy.delay(4, 12345), Duration::ZERO);
    }
}
