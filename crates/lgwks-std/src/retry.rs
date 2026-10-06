//! `retry` owns zero-dependency retry budgets for networked callers.
//!
//! A [`RetryPolicy`](crate::retry::RetryPolicy) is a pure value: attempts,
//! exponential backoff, and a total deadline. It performs no I/O, spawns no
//! threads, and holds no clock: the caller sleeps (via
//! [`task`] synchronously or `lgwks_bot::rt::time`
//! asynchronously) and checks
//! [`deadline_exceeded`](crate::retry::RetryPolicy::deadline_exceeded)
//! against its own clock. That split keeps this module in `core` with zero
//! external dependencies and makes budgets explicit at every hop instead of
//! smuggled inside a client.
//!
//! [`task`]: crate::task
//!
//! Jitter is caller-supplied (`u64` entropy the caller already holds, e.g.
//! from `lgwks_std::random`) so `core` stays free of entropy sources:
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
//! INV-STD-RETRY: [`RetryPolicy`](crate::retry::RetryPolicy) never sleeps, never reads a clock,
//! never allocates, and computes the exact capped delay for every `u32` retry
//! index and every `Duration`. Its work is bounded independently of the retry
//! index. The one-word jitter mapping is deterministic and modulo-biased; it
//! does not claim uniform sampling across the full delay range.

use std::num::NonZeroU128;
use std::time::Duration;

/// How a caller spaces repeated attempts of one fallible operation.
///
/// All fields are plain data so policies can cross crate boundaries (JSON,
/// manifests, config files) without dragging an executor along.
///
/// # Sharing one policy across threads
///
/// A `RetryPolicy` is `Send + Sync` and holds no interior mutability, no clock
/// and no counter, so one policy serves any number of concurrent callers and
/// every one of them gets the same delay for the same `(attempt, entropy)`.
/// The public fields are settable, so "share it" means share it before
/// configuring it, or freeze the value in a binding the callers borrow; a
/// caller that mutates a policy another thread is reading has made a data race
/// in its own program, which is what the field mutability buys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct RetryPolicy {
    /// Maximum attempts including the first try. Zero is treated as one both
    /// at construction and use, including after direct public-field mutation.
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

    /// Cap any single backoff at `max_delay` (applied before jitter). A zero
    /// cap produces a zero delay.
    #[must_use]
    pub fn with_max_delay(mut self, max_delay: Duration) -> Self {
        self.max_delay = max_delay;
        self
    }

    /// Backoff after a failure and before its retry, with `attempt` as the
    /// zero-based retry index (`0` follows the first failure). The uncapped
    /// delay is `base_delay * 2^attempt`, capped at `max_delay`, then shortened
    /// by `backoff - (jitter_entropy % (backoff_nanoseconds + 1))`.
    ///
    /// The one `u64` entropy word maps by modulo, so it is biased unless the
    /// inclusive range divides `2^64`. If that range is wider than `2^64`,
    /// only its first `2^64` remainders are reachable. No entropy is sampled
    /// here; callers own that policy.
    #[must_use]
    pub fn delay(&self, attempt: u32, jitter_entropy: u64) -> Duration {
        let backoff_nanos = capped_backoff_nanos(
            self.base_delay.as_nanos(),
            self.max_delay.as_nanos(),
            attempt,
        );
        if backoff_nanos == 0 {
            return Duration::ZERO;
        }
        // The jitter window is inclusive of zero, so it is the backoff plus one.
        // `NonZeroU128` carries that: a window of zero would be a backoff of
        // zero, which returned above, and the saturating add cannot wrap to
        // zero, so the divisor a remainder is taken over exists by type rather
        // than by a checked call whose refusal arm would have to be answered
        // with a guess.
        let Some(jitter_window) = NonZeroU128::new(backoff_nanos.saturating_add(1)) else {
            return Duration::ZERO;
        };
        let jitter = match u128::from(jitter_entropy).checked_rem(jitter_window.get()) {
            Some(remainder) => backoff_nanos.saturating_sub(remainder),
            // A window that admits no remainder admits no entropy either, so
            // none is placed and the full backoff stands. The window above is
            // non-zero, which is what keeps this arm unreachable for every
            // policy a caller can configure.
            None => backoff_nanos,
        };
        duration_from_nanos(jitter)
    }

    /// Whether `elapsed` has consumed the total `deadline` budget.
    #[must_use]
    pub fn deadline_exceeded(&self, elapsed: Duration) -> bool {
        elapsed >= self.deadline
    }

    /// Whether an attempt is allowed after `failures` failed attempts; zero
    /// failures includes the initial attempt. The deadline gates that initial
    /// attempt too, and equality refuses it. This checks current eligibility,
    /// not whether the next delay fits: before sleeping, callers must bound
    /// `delay` by `deadline.saturating_sub(elapsed)`.
    #[must_use]
    pub fn should_retry(&self, failures: u32, elapsed: Duration) -> bool {
        failures < self.max_attempts.max(1) && !self.deadline_exceeded(elapsed)
    }
}

/// Returns the exact capped base-delay product in nanoseconds.
///
/// `Duration::as_nanos` fits in `u128`, including `Duration::MAX`. Comparing
/// against the cap shifted right proves whether the product reaches the cap
/// before shifting, so overflow and work proportional to `attempt` are avoided.
fn capped_backoff_nanos(base_nanos: u128, cap_nanos: u128, attempt: u32) -> u128 {
    let base_nanos = base_nanos.min(cap_nanos);
    if base_nanos == 0 || base_nanos == cap_nanos {
        return base_nanos;
    }
    if attempt >= u128::BITS {
        return cap_nanos;
    }
    let shift = attempt;
    if base_nanos > (cap_nanos >> shift) {
        cap_nanos
    } else {
        base_nanos << shift
    }
}

/// Nanoseconds in one second, the split point of [`duration_from_nanos`].
const NANOS_PER_SECOND: u128 = 1_000_000_000;

/// The largest subsecond nanosecond count `Duration` can carry.
const MAX_SUBSECOND_NANOS: u32 = 999_999_999;

/// Converts nanoseconds to a `Duration`, clamped to `Duration`'s own range.
///
/// `Duration` names a `u64` of whole seconds and a `u32` of subsecond
/// nanoseconds, so a nanosecond count splits into a whole-second part and a
/// remainder. Each part is converted through its own bound: a whole-second
/// count above `u64::MAX` clamps the whole duration to `Duration::MAX`, and a
/// remainder above the subsecond field's range clamps to the largest
/// subsecond value the field can carry. Neither clamp substitutes a plausible
/// number for a missing one — each states the nearest duration the type can
/// name — and every nanosecond count this module produces is at most
/// `max_delay`, which is already a `Duration`, so a policy-configured caller
/// never reaches either.
fn duration_from_nanos(nanos: u128) -> Duration {
    let whole_seconds = match nanos.checked_div(NANOS_PER_SECOND) {
        Some(whole_seconds) => whole_seconds,
        None => return Duration::MAX,
    };
    let seconds = match u64::try_from(whole_seconds) {
        Ok(seconds) => seconds,
        Err(_) => return Duration::MAX,
    };
    let remainder = match nanos.checked_rem(NANOS_PER_SECOND) {
        Some(remainder) => remainder,
        None => return Duration::MAX,
    };
    let subsecond_nanos = match u32::try_from(remainder) {
        Ok(subsecond_nanos) => subsecond_nanos,
        Err(_) => MAX_SUBSECOND_NANOS,
    };
    Duration::new(seconds, subsecond_nanos)
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
            let delay = policy.delay(2, entropy);
            assert!(
                delay <= Duration::from_millis(800),
                "jitter grew backoff: {delay:?}"
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
        assert_eq!(
            policy.delay(u32::MAX, 0),
            Duration::from_secs(30),
            "a huge retry index reaches the configured cap"
        );
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
        assert_eq!(
            policy.delay(4, 12345),
            Duration::ZERO,
            "zero base delay remains zero after any retry index"
        );
    }

    /// Computes the expected capped delay by bounded repeated doubling.
    fn reference_backoff_nanos(base: Duration, cap: Duration, attempt: u32) -> u128 {
        let cap_nanos = cap.as_nanos();
        let mut delay_nanos = base.as_nanos().min(cap_nanos);
        for _ in 0..attempt.min(u128::BITS) {
            delay_nanos = delay_nanos.saturating_mul(2).min(cap_nanos);
            if delay_nanos == cap_nanos {
                break;
            }
        }
        delay_nanos
    }

    #[test]
    fn public_delay_matches_reference_at_attempt_and_cap_boundaries() {
        let attempts = [0, 1, 30, 31, 32, 35, u32::MAX];
        let cases = [
            (Duration::ZERO, Duration::from_nanos(1)),
            (Duration::from_nanos(1), Duration::ZERO),
            (Duration::from_nanos(1), Duration::from_nanos(1)),
            (Duration::from_nanos(1), Duration::from_nanos(2)),
            (Duration::from_millis(100), Duration::from_millis(50)),
            (Duration::from_millis(100), Duration::from_millis(100)),
            (Duration::from_millis(100), Duration::from_millis(200)),
            (Duration::from_millis(100), Duration::from_secs(30)),
        ];
        for (base, cap) in cases {
            let policy = RetryPolicy::new(1, base, Duration::MAX).with_max_delay(cap);
            for attempt in attempts {
                let expected = duration_from_nanos(reference_backoff_nanos(base, cap, attempt));
                assert_eq!(
                    policy.delay(attempt, 0),
                    expected,
                    "attempt {attempt} must use exact capped doubling from {base:?} to {cap:?}"
                );
            }
        }
    }

    #[test]
    fn jitter_matches_wide_reference_across_duration_boundaries() {
        let durations = [
            Duration::from_nanos(1),
            Duration::from_nanos(u64::MAX),
            Duration::new(u64::MAX, 999_999_999),
            Duration::MAX,
        ];
        let entropies = [0, 1, u64::MAX];
        for duration in durations {
            for entropy in entropies {
                let policy = RetryPolicy::new(1, duration, Duration::MAX).with_max_delay(duration);
                let backoff_nanos = reference_backoff_nanos(duration, duration, 0);
                let expected = backoff_nanos - (u128::from(entropy) % (backoff_nanos + 1));
                assert_eq!(
                    policy.delay(0, entropy),
                    duration_from_nanos(expected),
                    "entropy {entropy} must follow the inclusive wide-range formula for {duration:?}"
                );
            }
        }
    }

    #[test]
    fn effective_attempt_floor_and_deadline_equality_are_consistent() {
        let mut policy = RetryPolicy::new(0, Duration::ZERO, Duration::from_nanos(1));
        assert_eq!(
            policy.max_attempts, 1,
            "construction normalizes zero attempts"
        );
        assert!(
            policy.should_retry(0, Duration::ZERO),
            "one initial attempt is allowed before the deadline"
        );
        assert!(
            !policy.should_retry(0, Duration::from_nanos(1)),
            "deadline equality refuses even the initial attempt"
        );
        policy.max_attempts = 0;
        assert!(
            policy.should_retry(0, Duration::ZERO),
            "direct mutation to zero retains the effective one-attempt floor"
        );
        assert!(
            !policy.should_retry(1, Duration::ZERO),
            "the effective one-attempt floor refuses work after one failure"
        );
        assert!(
            !RetryPolicy::new(1, Duration::ZERO, Duration::ZERO).should_retry(0, Duration::ZERO),
            "a zero deadline refuses at equality"
        );
    }

    #[test]
    fn delay_work_is_bounded_independently_of_the_attempt_value() {
        let policy = RetryPolicy::new(u32::MAX, Duration::from_nanos(1), Duration::MAX);
        // The shift-and-compare form performs a fixed number of machine
        // operations whatever `attempt` is. An implementation that looped
        // `attempt` times, or that walked the doubling sequence, would take
        // measurably longer at `u32::MAX` than at `0`.
        //
        // The oracle is a ratio, not an absolute: it holds on a loaded host as
        // long as the constant factor is nowhere near `2^32`.
        let repeats = 100_000_u64;
        let mut timings = [(0_u32, std::time::Duration::ZERO); 4];
        for (index, attempt) in [0_u32, 31, 1_000, u32::MAX].into_iter().enumerate() {
            let start = std::time::Instant::now();
            let mut observed = Duration::ZERO;
            for step in 0..repeats {
                // A changing base keeps the optimizer from hoisting the call
                // out of the loop; the value is consumed below.
                observed = policy.delay(attempt, step);
            }
            assert_eq!(
                observed,
                policy.delay(attempt, repeats.saturating_sub(1)),
                "the timed loop must finish on the last computed value"
            );
            timings[index] = (attempt, start.elapsed());
        }
        let baseline = timings[0].1.as_nanos();
        for (attempt, elapsed) in timings {
            assert!(
                elapsed.as_nanos() <= baseline.saturating_mul(4).saturating_add(1_000_000),
                "attempt {attempt} took {elapsed:?} against {baseline}ns at attempt 0; \
                 the work must not scale with the numeric attempt value"
            );
        }
    }

    #[test]
    fn the_ordinary_sequence_is_unchanged_for_an_existing_consumer() {
        // A caller that migrates nothing must see the same sequence it saw
        // before, from the same constructor, across the ordinary attempt range.
        let policy = RetryPolicy::new(6, Duration::from_millis(100), Duration::from_secs(60));
        let observed: Vec<Duration> = (0..6).map(|attempt| policy.delay(attempt, 0)).collect();
        let expected = [
            Duration::from_millis(100),
            Duration::from_millis(200),
            Duration::from_millis(400),
            Duration::from_millis(800),
            Duration::from_millis(1_600),
            Duration::from_millis(3_200),
        ];
        assert_eq!(
            observed, expected,
            "the ordinary doubling sequence must be unchanged"
        );
    }
}
