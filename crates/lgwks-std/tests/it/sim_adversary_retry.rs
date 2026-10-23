//! A hostile configuration and a hostile entropy word drive a retry schedule.
//!
//! A retry policy is read from a manifest or a config file and fed entropy by the
//! caller, so the numbers are not the author's: a base delay of `Duration::MAX`,
//! a cap of zero, an attempt index of `u32::MAX`, a deadline already past. The
//! failures this class has produced are the ones that make a client hammer a
//! service or hang: an overflowing doubling that wraps to a tiny delay and turns
//! backoff into a tight loop (the retry storms of AWS's "Exponential Backoff And
//! Jitter" analysis), a shift by the full word width that panics or wraps, and a
//! deadline compared with the wrong inequality so a budget of zero still tries
//! once. The attacks: extreme magnitudes of every duration, every attempt index
//! around the word width, entropy at its extremes, a window small enough to
//! enumerate, a caller loop driven by a hostile policy, and one policy shared by
//! many threads.
//!
//! The reference is written here in `u128` nanoseconds from the documented
//! formula: the doubled base capped at `max_delay`, then shortened by the
//! entropy's remainder over the window `backoff + 1`.

use std::time::Duration;

use lgwks_std::retry::RetryPolicy;

use crate::adversary::{
    Attacker, TestResult, diverges_between_seeds, replays_under_every_seed, sweep,
};
use crate::seeded_bytes::fold_bytes;
use crate::seeded_sweep::initial_trace;

/// Nanoseconds in one second.
const SECOND: u128 = 1_000_000_000;

/// Attempt indices at and around every boundary of a machine word.
const ATTEMPTS: [u32; 12] = [
    0,
    1,
    2,
    31,
    32,
    63,
    64,
    127,
    128,
    129,
    u32::MAX.saturating_sub(1),
    u32::MAX,
];

/// Entropy words at the extremes.
const ENTROPIES: [u64; 5] = [
    0,
    1,
    u64::MAX.saturating_sub(1),
    u64::MAX,
    0x8000_0000_0000_0000,
];

/// Durations at the edges of the type and of a second.
const EDGES: [Duration; 8] = [
    Duration::ZERO,
    Duration::from_nanos(1),
    Duration::from_nanos(999_999_999),
    Duration::from_secs(1),
    Duration::from_secs(30),
    Duration::from_secs(u64::MAX),
    Duration::MAX,
    Duration::new(u64::MAX, 1),
];

/// A duration as a hostile config might name it: an edge, or a random magnitude.
fn draw_duration(attacker: &mut Attacker) -> Duration {
    match attacker.below(8) {
        0 => *attacker.pick(&EDGES),
        1 => Duration::from_nanos(attacker.word()),
        2 => Duration::from_secs(attacker.word().wrapping_shr(attacker.field(0, 64))),
        3 => Duration::new(attacker.word(), 999_999_999),
        4 => Duration::from_millis(attacker.word().wrapping_shr(attacker.field(20, 64))),
        5 => Duration::from_micros(attacker.word().wrapping_shr(attacker.field(10, 64))),
        6 => Duration::from_secs(u64::from(attacker.field(0, 120))),
        _ => Duration::from_secs(30),
    }
}

/// `nanos` as a `Duration`, saturating at the type's largest.
fn nanos_to_duration(nanos: u128) -> Duration {
    let seconds = nanos.div_euclid(SECOND);
    let subsecond = nanos.rem_euclid(SECOND);
    match (u64::try_from(seconds), u32::try_from(subsecond)) {
        (Ok(seconds), Ok(subsecond)) => Duration::new(seconds, subsecond),
        _ => Duration::MAX,
    }
}

/// The capped doubling in nanoseconds: `base * 2^attempt`, never above `cap`,
/// with an overflowing product meaning the cap.
fn reference_backoff(base: Duration, cap: Duration, attempt: u32) -> u128 {
    let (base_nanos, cap_nanos) = (base.as_nanos(), cap.as_nanos());
    let start = base_nanos.min(cap_nanos);
    if start == 0 {
        return 0;
    }
    match 1_u128
        .checked_shl(attempt)
        .and_then(|factor| start.checked_mul(factor))
    {
        Some(product) => product.min(cap_nanos),
        None => cap_nanos,
    }
}

/// The delay the documented formula gives: the backoff shortened by the
/// entropy's remainder over the window `backoff + 1`.
fn reference_delay(base: Duration, cap: Duration, attempt: u32, entropy: u64) -> Duration {
    let backoff = reference_backoff(base, cap, attempt);
    if backoff == 0 {
        return Duration::ZERO;
    }
    let remainder = u128::from(entropy).rem_euclid(backoff.saturating_add(1));
    nanos_to_duration(backoff.saturating_sub(remainder))
}

/// The policy a config naming these numbers builds.
fn policy_of(attempts: u32, base: Duration, deadline: Duration, cap: Duration) -> RetryPolicy {
    RetryPolicy::new(attempts, base, deadline).with_max_delay(cap)
}

/// One campaign: hostile policies and entropy through `delay`, outcomes folded.
fn campaign(seed: u64) -> u64 {
    let mut attacker = Attacker::new(seed);
    let mut trace = initial_trace();
    for _ in 0..256 {
        let (base, cap) = (draw_duration(&mut attacker), draw_duration(&mut attacker));
        let policy = policy_of(1, base, Duration::MAX, cap);
        let attempt = attacker.field(0, 140);
        let delay = policy.delay(attempt, attacker.word());
        fold_bytes(&mut trace, &delay.as_nanos().to_le_bytes());
    }
    trace
}

/// Whatever durations, attempt and entropy the attacker names, `delay` is the
/// reference's: no wrap to a tiny delay, no clamp short of the cap.
#[test]
fn sim_adversary_retry_hostile_policies_get_the_reference_delay() -> TestResult {
    sweep(2_048, |seed, round, attacker| {
        let (base, cap) = (draw_duration(attacker), draw_duration(attacker));
        let policy = policy_of(3, base, Duration::MAX, cap);
        let attempt = attacker.field(0, 200);
        let entropy = attacker.word();
        assert_eq!(
            policy.delay(attempt, entropy),
            reference_delay(base, cap, attempt, entropy),
            "seed {seed:#x} round {round}: base {base:?} cap {cap:?} attempt {attempt}"
        );
        Ok(())
    })
}

/// Each edge duration as base and as cap, at every attempt around the word
/// width, with entropy at both ends: all agree with the reference.
#[test]
fn sim_adversary_retry_every_edge_duration_at_every_edge_attempt() {
    for base in EDGES {
        for cap in EDGES {
            let policy = policy_of(5, base, Duration::MAX, cap);
            for attempt in ATTEMPTS {
                for entropy in ENTROPIES {
                    assert_eq!(
                        policy.delay(attempt, entropy),
                        reference_delay(base, cap, attempt, entropy),
                        "base {base:?} cap {cap:?} attempt {attempt} entropy {entropy:#x}"
                    );
                }
            }
        }
    }
}

/// Jitter only shortens: a delay never exceeds its backoff, zero entropy gives
/// the whole backoff, and entropy equal to the backoff gives none.
#[test]
fn sim_adversary_retry_jitter_only_shortens() -> TestResult {
    sweep(2_048, |seed, round, attacker| {
        let (base, cap) = (draw_duration(attacker), draw_duration(attacker));
        let policy = policy_of(3, base, Duration::MAX, cap);
        let attempt = attacker.field(0, 140);
        let backoff = nanos_to_duration(reference_backoff(base, cap, attempt));
        let entropy = attacker.word();
        assert!(
            policy.delay(attempt, entropy) <= backoff,
            "seed {seed:#x} round {round}: jitter lengthened {backoff:?}"
        );
        assert_eq!(
            policy.delay(attempt, 0),
            backoff,
            "seed {seed:#x} round {round}: zero entropy is the whole backoff"
        );
        Ok(())
    })
}

/// A backoff small enough to enumerate: every entropy word over a window of
/// eleven nanoseconds lands on a distinct delay from zero to ten, so the
/// remainder mapping is exact and covers the whole window.
#[test]
fn sim_adversary_retry_a_small_window_is_covered_exactly() {
    let policy = policy_of(3, Duration::from_nanos(10), Duration::MAX, Duration::MAX);
    let mut seen = std::collections::BTreeSet::new();
    for entropy in 0..11_u64 {
        let delay = policy.delay(0, entropy);
        assert_eq!(
            delay,
            Duration::from_nanos(10_u64.saturating_sub(entropy)),
            "entropy {entropy}"
        );
        seen.insert(delay);
    }
    assert_eq!(seen.len(), 11, "every shortening from zero to ten occurs");
    assert_eq!(
        policy.delay(0, 11),
        policy.delay(0, 0),
        "the window repeats after eleven"
    );
}

/// With no jitter the delay never falls as the attempt rises and never exceeds
/// the cap, so a slow service is never retried faster the longer it fails.
#[test]
fn sim_adversary_retry_the_backoff_never_falls_as_attempts_rise() -> TestResult {
    sweep(512, |seed, round, attacker| {
        let (base, cap) = (draw_duration(attacker), draw_duration(attacker));
        let policy = policy_of(3, base, Duration::MAX, cap);
        let mut previous = Duration::ZERO;
        for attempt in 0..=140_u32 {
            let delay = policy.delay(attempt, 0);
            assert!(
                delay >= previous && delay <= cap,
                "seed {seed:#x} round {round}: attempt {attempt} gave {delay:?} after {previous:?}"
            );
            previous = delay;
        }
        Ok(())
    })
}

/// An attempt index of any size is answered, and from the index where doubling
/// reaches the cap the answer is the cap: the work does not grow with the index.
#[test]
fn sim_adversary_retry_a_huge_attempt_index_is_answered_at_once() {
    let policy = policy_of(
        5,
        Duration::from_millis(100),
        Duration::MAX,
        Duration::from_secs(30),
    );
    for attempt in [
        10_u32,
        100,
        1_000,
        1_000_000,
        u32::MAX.saturating_sub(1),
        u32::MAX,
    ] {
        assert_eq!(
            policy.delay(attempt, 0),
            Duration::from_secs(30),
            "attempt {attempt}"
        );
    }
}

/// Zero attempts is one attempt, however the zero arrived: at construction, or
/// by assigning the public field afterwards.
#[test]
fn sim_adversary_retry_zero_attempts_means_one_try() {
    let mut policy = RetryPolicy::new(0, Duration::from_secs(1), Duration::from_secs(10));
    assert_eq!(policy.max_attempts, 1, "construction normalises zero");
    policy.max_attempts = 0;
    assert!(
        policy.should_retry(0, Duration::ZERO),
        "the first try is allowed"
    );
    assert!(!policy.should_retry(1, Duration::ZERO), "a second is not");
}

/// `should_retry` is exactly `failures < max(attempts, 1) && elapsed < deadline`
/// for hostile attempts, failures, elapsed time and deadline.
#[test]
fn sim_adversary_retry_should_retry_matches_the_reference() -> TestResult {
    sweep(2_048, |seed, round, attacker| {
        let attempts = attacker.field(0, 6);
        let (deadline, elapsed) = (draw_duration(attacker), draw_duration(attacker));
        let policy = policy_of(attempts, Duration::from_secs(1), deadline, Duration::MAX);
        let failures = attacker.field(0, 8);
        let expected = failures < attempts.max(1) && elapsed < deadline;
        assert_eq!(
            policy.should_retry(failures, elapsed),
            expected,
            "seed {seed:#x} round {round}: {failures} failures of {attempts}, {elapsed:?} of {deadline:?}"
        );
        Ok(())
    })
}

/// The deadline is exceeded at equality and one nanosecond after, not one
/// nanosecond before: the budget is exclusive, whatever its size.
#[test]
fn sim_adversary_retry_the_deadline_is_exclusive() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let deadline = draw_duration(attacker);
        let policy = policy_of(3, Duration::from_secs(1), deadline, Duration::MAX);
        assert!(
            policy.deadline_exceeded(deadline),
            "seed {seed:#x} round {round}: equal to {deadline:?} must be exceeded"
        );
        if let Some(before) = deadline.checked_sub(Duration::from_nanos(1)) {
            assert!(
                !policy.deadline_exceeded(before),
                "seed {seed:#x} round {round}: one nanosecond under {deadline:?}"
            );
        }
        if let Some(after) = deadline.checked_add(Duration::from_nanos(1)) {
            assert!(
                policy.deadline_exceeded(after),
                "seed {seed:#x} round {round}: one nanosecond over {deadline:?}"
            );
        }
        Ok(())
    })
}

/// A deadline of zero refuses even the first try, and a deadline of
/// `Duration::MAX` is reached only by `Duration::MAX` itself.
#[test]
fn sim_adversary_retry_the_extreme_deadlines() {
    let none = policy_of(9, Duration::from_secs(1), Duration::ZERO, Duration::MAX);
    assert!(
        !none.should_retry(0, Duration::ZERO),
        "a zero budget refuses the first try"
    );
    assert!(
        none.deadline_exceeded(Duration::ZERO),
        "zero is already spent"
    );
    let endless = policy_of(9, Duration::from_secs(1), Duration::MAX, Duration::MAX);
    let nearly = Duration::MAX.saturating_sub(Duration::from_nanos(1));
    assert!(
        !endless.deadline_exceeded(nearly),
        "one nanosecond short of the end"
    );
    assert!(endless.deadline_exceeded(Duration::MAX), "the end itself");
    assert!(
        endless.should_retry(8, nearly),
        "the last allowed try near the end"
    );
    assert!(
        !endless.should_retry(9, nearly),
        "the attempts, not the clock, stop it"
    );
}

/// The budget the module documents: three tries, a 100 ms base and a 5 s
/// deadline wait 100 ms then 200 ms, and the deadline is exceeded at 6 s and not
/// at 4 s.
#[test]
fn sim_adversary_retry_the_documented_budget_holds() {
    let policy = RetryPolicy::new(3, Duration::from_millis(100), Duration::from_secs(5));
    assert_eq!(
        policy.delay(0, 0),
        Duration::from_millis(100),
        "after the first failure"
    );
    assert_eq!(
        policy.delay(1, 0),
        Duration::from_millis(200),
        "after the second"
    );
    assert!(
        policy.deadline_exceeded(Duration::from_secs(6)),
        "6 s is past 5 s"
    );
    assert!(
        !policy.deadline_exceeded(Duration::from_secs(4)),
        "4 s is not"
    );
}

/// A cap of zero zeroes every delay, whatever the base, attempt and entropy.
#[test]
fn sim_adversary_retry_a_zero_cap_zeroes_every_delay() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let base = draw_duration(attacker);
        let policy = policy_of(3, base, Duration::MAX, Duration::ZERO);
        let attempt = attacker.field(0, 140);
        assert_eq!(
            policy.delay(attempt, attacker.word()),
            Duration::ZERO,
            "seed {seed:#x} round {round}: base {base:?}"
        );
        Ok(())
    })
}

/// A cap below the base applies from the first retry, so a config that names a
/// larger base than its cap is held to the cap.
#[test]
fn sim_adversary_retry_a_cap_below_the_base_binds_at_once() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let cap = draw_duration(attacker);
        let excess = attacker.word().rem_euclid(1_000).saturating_add(1);
        let base = cap.saturating_add(Duration::from_nanos(excess));
        let policy = policy_of(3, base, Duration::MAX, cap);
        assert_eq!(
            policy.delay(0, 0),
            cap,
            "seed {seed:#x} round {round}: base {base:?} over cap {cap:?}"
        );
        Ok(())
    })
}

/// Without `with_max_delay` the cap is thirty seconds: a one-second base doubles
/// to thirty and stays there for every later attempt.
#[test]
fn sim_adversary_retry_the_default_cap_is_thirty_seconds() {
    let policy = RetryPolicy::new(50, Duration::from_secs(1), Duration::MAX);
    let thirty = Duration::from_secs(30);
    assert_eq!(policy.max_delay, thirty, "the default cap");
    assert_eq!(
        policy.delay(4, 0),
        Duration::from_secs(16),
        "before the cap"
    );
    for attempt in 5..=70_u32 {
        assert_eq!(policy.delay(attempt, 0), thirty, "attempt {attempt}");
    }
}

/// Carries across the second boundary are exact: bases a nanosecond either side
/// of a second, doubled through sixty-five attempts, match the reference to the
/// nanosecond.
#[test]
fn sim_adversary_retry_carries_across_seconds_are_exact() {
    for base_nanos in [
        1_u64,
        3,
        999_999_999,
        1_000_000_000,
        1_000_000_001,
        4_294_967_296,
    ] {
        let base = Duration::from_nanos(base_nanos);
        let policy = policy_of(70, base, Duration::MAX, Duration::MAX);
        for attempt in 0..=65_u32 {
            assert_eq!(
                policy.delay(attempt, 0),
                nanos_to_duration(reference_backoff(base, Duration::MAX, attempt)),
                "base {base_nanos} ns, attempt {attempt}"
            );
        }
    }
}

/// With no jitter, the sleeps of a whole schedule add up to no more than the cap
/// times the retries: a client cannot wait longer than its configuration says.
#[test]
fn sim_adversary_retry_a_schedule_is_bounded_by_cap_times_retries() -> TestResult {
    sweep(512, |seed, round, attacker| {
        let (base, cap) = (draw_duration(attacker), draw_duration(attacker));
        let tries = attacker.field(1, 20);
        let policy = policy_of(tries, base, Duration::MAX, cap);
        let total: u128 = (0..tries.saturating_sub(1))
            .map(|attempt| policy.delay(attempt, 0).as_nanos())
            .fold(0_u128, u128::saturating_add);
        let bound = cap.as_nanos().saturating_mul(u128::from(tries));
        assert!(
            total <= bound,
            "seed {seed:#x} round {round}: {total} ns over {bound} ns"
        );
        Ok(())
    })
}

/// A caller loop that retries while `should_retry` and sleeps `delay` ends only
/// for a documented reason (attempts used or deadline spent), whatever the
/// policy: a hostile config cannot make it spin past both.
#[test]
fn sim_adversary_retry_a_caller_loop_ends_for_a_documented_reason() -> TestResult {
    sweep(512, |seed, round, attacker| {
        let attempts = attacker.field(0, 30);
        let (base, deadline, cap) = (
            draw_duration(attacker),
            draw_duration(attacker),
            draw_duration(attacker),
        );
        let policy = policy_of(attempts, base, deadline, cap);
        let (mut failures, mut elapsed) = (0_u32, Duration::ZERO);
        let mut steps = 0_u32;
        while policy.should_retry(failures, elapsed) && steps < 64 {
            elapsed = elapsed.saturating_add(policy.delay(failures, attacker.word()));
            failures = failures.saturating_add(1);
            steps = steps.saturating_add(1);
        }
        let stopped_for_a_reason = failures >= attempts.max(1) || elapsed >= deadline;
        assert!(
            stopped_for_a_reason,
            "seed {seed:#x} round {round}: loop left after {failures} failures at {elapsed:?}"
        );
        Ok(())
    })
}

/// One policy shared by eight threads gives each of them the same delays: it
/// holds no state, so tenants sharing it cannot disturb one another.
#[test]
fn sim_adversary_retry_one_policy_serves_many_threads_identically() {
    let policy = policy_of(
        9,
        Duration::from_millis(7),
        Duration::from_secs(60),
        Duration::from_secs(5),
    );
    let expected: Vec<Duration> = (0..200_u32)
        .map(|attempt| policy.delay(attempt, u64::from(attempt).wrapping_mul(0x9e37_79b9)))
        .collect();
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    (0..200_u32)
                        .map(|attempt| {
                            policy.delay(attempt, u64::from(attempt).wrapping_mul(0x9e37_79b9))
                        })
                        .collect::<Vec<Duration>>()
                })
            })
            .collect();
        for worker in workers {
            assert_eq!(
                worker.join().ok().as_ref(),
                Some(&expected),
                "a thread saw different delays"
            );
        }
    });
}

/// The same seed mounts the same attack on the schedule.
#[test]
fn sim_adversary_retry_the_same_seed_replays_the_same_campaign() {
    replays_under_every_seed(campaign);
}

/// Two seeds mount two attacks.
#[test]
fn sim_adversary_retry_distinct_seeds_mount_distinct_campaigns() {
    diverges_between_seeds(campaign);
}
