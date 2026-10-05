//! Seeded deterministic replay of the #164 retry arithmetic and policy contracts.
//!
//! One seed drives the configuration sweep, a failure prints the seed, and the
//! same seed must produce the same trace hash. Every probe drives the public
//! API of the shipped crate.

use lgwks_std::retry::RetryPolicy;
use std::time::Duration;

use crate::seeded_sweep;

use seeded_sweep::{
    SWEEP_SEEDS, assert_distinct_seeds_diverge, assert_same_seed_replays, fold, initial_trace,
    next_seed,
};

/// Folds a nanosecond count into the trace, saturating a narrowing cast so a
/// wide `Duration` cannot alias a small one.
fn fold_nanos(trace: &mut u64, nanos: u128) {
    fold(trace, u64::try_from(nanos).unwrap_or(u64::MAX));
}

/// Returns the exact capped backoff in nanoseconds by bounded repeated doubling.
///
/// This is the independent reference: it walks the sequence rather than
/// solving for it, so it shares no arithmetic with the shipped shift-and-
/// compare form. It stops at the cap, so the doubling count is bounded by the
/// cap's own magnitude and never by the attempt value.
fn reference_backoff_nanos(base_nanos: u128, cap_nanos: u128, attempt: u32) -> u128 {
    let mut value = base_nanos.min(cap_nanos);
    for _ in 0..attempt.min(u128::BITS) {
        value = value.saturating_mul(2).min(cap_nanos);
        if value == cap_nanos {
            break;
        }
    }
    value
}

/// Returns the exact delay a policy must produce, in nanoseconds.
///
/// The jitter is the documented inclusive-range remainder taken with
/// sufficiently wide arithmetic, so the whole `Duration` domain is covered
/// rather than a `u64` nanosecond truncation of it.
fn reference_delay_nanos(base_nanos: u128, cap_nanos: u128, attempt: u32, entropy: u64) -> u128 {
    let backoff = reference_backoff_nanos(base_nanos, cap_nanos, attempt);
    if backoff == 0 {
        return 0;
    }
    // `backoff` is `base.min(cap) << attempt` and is bounded by `cap`, so the
    // `+ 1` and the subtraction cannot overflow or underflow: the remainder is
    // below `backoff + 1` by construction.
    let inclusive_range = backoff.saturating_add(1);
    let jitter = u128::from(entropy)
        .checked_rem(inclusive_range)
        .unwrap_or(0);
    backoff.saturating_sub(jitter)
}

/// Runs the seeded sweep and returns its deterministic trace.
fn run_seeded_sweep(seed: u64) -> u64 {
    let mut state = seed;
    let mut trace = initial_trace();

    let bases = [
        Duration::ZERO,
        Duration::from_nanos(1),
        Duration::from_millis(1),
        Duration::from_millis(100),
        Duration::from_secs(20),
        Duration::MAX,
    ];
    let caps = [
        Duration::ZERO,
        Duration::from_nanos(1),
        Duration::from_millis(50),
        Duration::from_millis(100),
        Duration::from_millis(200),
        Duration::from_secs(30),
        Duration::from_secs(60),
        Duration::MAX,
    ];
    let attempts = [0_u32, 1, 30, 31, 32, 35, 1_000, u32::MAX];

    for _ in 0..400 {
        let base = bases[usize::try_from(next_seed(&mut state).rem_euclid(6)).unwrap_or(0)];
        let cap = caps[usize::try_from(next_seed(&mut state).rem_euclid(8)).unwrap_or(0)];
        let attempt = attempts[usize::try_from(next_seed(&mut state).rem_euclid(8)).unwrap_or(0)];
        let entropy = next_seed(&mut state);

        let policy = RetryPolicy::new(1, base, Duration::MAX).with_max_delay(cap);
        let observed = policy.delay(attempt, entropy);
        let expected = reference_delay_nanos(base.as_nanos(), cap.as_nanos(), attempt, entropy);
        fold_nanos(&mut trace, observed.as_nanos());
        assert_eq!(
            observed.as_nanos(),
            expected,
            "seed {seed}: attempt {attempt} with base {base:?} cap {cap:?} \
             entropy {entropy} must follow the exact capped formula"
        );

        // The capped delay never exceeds the cap, and never exceeds the whole
        // delay domain's representable maximum.
        assert!(
            observed <= cap,
            "seed {seed}: jitter lengthened the backoff beyond its cap: \
             {observed:?} > {cap:?}"
        );
        assert!(
            observed <= policy.delay(attempt, 0),
            "seed {seed}: jitter must only shorten the backoff"
        );

        // Eligibility is a separate decision from the delay, and a deadline at
        // equality refuses the initial attempt too.
        let with_deadline = RetryPolicy::new(1, base, cap);
        assert!(
            !with_deadline.should_retry(0, cap),
            "seed {seed}: deadline equality must refuse the initial attempt"
        );
        assert!(
            with_deadline.should_retry(0, Duration::ZERO) || cap == Duration::ZERO,
            "seed {seed}: a positive deadline must admit the initial attempt"
        );
    }
    trace
}

#[test]
fn the_same_seed_replays_to_the_same_trace() {
    for seed in SWEEP_SEEDS {
        assert_same_seed_replays(run_seeded_sweep, seed);
    }
}

#[test]
fn different_seeds_produce_different_traces() {
    assert_distinct_seeds_diverge(run_seeded_sweep, SWEEP_SEEDS[0], SWEEP_SEEDS[1]);
}

#[test]
fn exponential_scaling_reaches_the_exact_cap_past_the_old_shift_freeze() {
    // The specific values #164 R1 names. The shipped implementation froze at
    // attempt 31 because it clamped the shift; these are the values that
    // disambiguate.
    let policy = RetryPolicy::new(1, Duration::from_nanos(1), Duration::MAX);
    assert_eq!(
        policy.delay(30, 0),
        Duration::from_nanos(1_073_741_824),
        "attempt 30 doubles to 2^30 nanoseconds"
    );
    assert_eq!(
        policy.delay(31, 0),
        Duration::from_nanos(2_147_483_648),
        "attempt 31 doubles to 2^31 nanoseconds"
    );
    assert_eq!(
        policy.delay(32, 0),
        Duration::from_nanos(4_294_967_296),
        "attempt 32 must double again, not freeze at 2^31"
    );
    assert_eq!(
        policy.delay(35, 0),
        Duration::from_secs(30),
        "attempt 35 must reach the exact 30-second cap, not freeze at 2^31"
    );
    assert_eq!(
        policy.delay(u32::MAX, 0),
        Duration::from_secs(30),
        "the largest attempt index reaches the same exact cap"
    );
}

#[test]
fn the_inclusive_jitter_formula_holds_over_the_whole_duration_domain() {
    // `Duration::MAX` is wider than `u64` nanoseconds, so a truncation to
    // `u64` would lose the top of the range. The wide reference covers it.
    // The cap is raised to `Duration::MAX` explicitly, because `new` defaults
    // it to 30 seconds and would otherwise mask the domain under test.
    let policy = RetryPolicy::new(1, Duration::MAX, Duration::MAX).with_max_delay(Duration::MAX);
    for entropy in [0_u64, 1, u64::MAX] {
        let observed = policy.delay(0, entropy);
        let backoff =
            reference_backoff_nanos(Duration::MAX.as_nanos(), Duration::MAX.as_nanos(), 0);
        let inclusive_range = backoff.saturating_add(1);
        let jitter = u128::from(entropy)
            .checked_rem(inclusive_range)
            .unwrap_or(0);
        assert_eq!(
            observed.as_nanos(),
            backoff.saturating_sub(jitter),
            "entropy {entropy}: the inclusive-range remainder must hold over \
             the whole Duration domain"
        );
    }
    // A `Duration::MAX` backoff with maximum entropy shortens by exactly the
    // documented remainder rather than returning the whole backoff.
    let whole = policy.delay(0, u64::MAX);
    assert!(
        whole <= policy.delay(0, 0),
        "maximum entropy never lengthens the delay"
    );
    assert!(
        whole.as_nanos() < policy.delay(0, 0).as_nanos(),
        "maximum entropy must actually shorten a Duration::MAX backoff"
    );
}

#[test]
fn zero_base_cap_and_deadline_have_declared_exact_outcomes() {
    // Zero base stays zero at every attempt.
    let zero_base = RetryPolicy::new(3, Duration::ZERO, Duration::from_secs(1));
    for attempt in [0_u32, 1, 31, 32, u32::MAX] {
        assert_eq!(
            zero_base.delay(attempt, 7),
            Duration::ZERO,
            "a zero base stays zero at attempt {attempt}"
        );
    }
    // A zero cap produces a zero delay even with a positive base.
    let zero_cap = RetryPolicy::new(3, Duration::from_secs(1), Duration::from_secs(1))
        .with_max_delay(Duration::ZERO);
    for attempt in [0_u32, 31, u32::MAX] {
        assert_eq!(
            zero_cap.delay(attempt, 0),
            Duration::ZERO,
            "a zero cap produces a zero delay at attempt {attempt}"
        );
    }
    // A zero deadline refuses at equality, including the initial attempt.
    let zero_deadline = RetryPolicy::new(3, Duration::from_millis(1), Duration::ZERO);
    assert!(
        !zero_deadline.should_retry(0, Duration::ZERO),
        "a zero deadline refuses the initial attempt at equality"
    );
    assert!(
        !zero_deadline.should_retry(2, Duration::ZERO),
        "a zero deadline refuses every later attempt too"
    );
}

#[test]
fn the_effective_attempt_floor_survives_public_mutation() {
    let mut policy = RetryPolicy::new(0, Duration::from_millis(10), Duration::from_secs(1));
    assert_eq!(
        policy.max_attempts, 1,
        "construction normalizes zero attempts to one"
    );
    // Public mutation after construction cannot disagree with the floor.
    policy.max_attempts = 0;
    assert!(
        policy.should_retry(0, Duration::ZERO),
        "a mutated zero still permits the initial attempt"
    );
    assert!(
        !policy.should_retry(1, Duration::ZERO),
        "a mutated zero still refuses work after one failure"
    );
    // A policy constructed with one attempt refuses after one failure too.
    let single = RetryPolicy::new(1, Duration::from_millis(10), Duration::from_secs(1));
    assert!(single.should_retry(0, Duration::ZERO));
    assert!(
        !single.should_retry(1, Duration::ZERO),
        "one attempt means no retry after the first failure"
    );
}
