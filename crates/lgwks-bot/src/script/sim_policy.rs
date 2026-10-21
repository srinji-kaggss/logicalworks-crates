//! Seeded model check of the run-wide retry budget and the fan-out bound.
//!
//! One seed draws interleavings of first attempts and retries, single-threaded
//! and across threads, and every family compares the shipped [`Policy`] with
//! an integer reference written here from the module's documentation:
//!
//! - an unbounded `each` runs between one and [`MAX_IN_FLIGHT`] bodies, the
//!   machine's cores times the per-core allowance under that ceiling;
//! - a fresh run holds exactly the reserve, and five first attempts earn one
//!   more retry;
//! - every drawn interleaving grants exactly the retries the reference grants,
//!   and a refused retry spends nothing;
//! - a run never retries more than its reserve plus a fifth of its first
//!   attempts, however its attempts and retries interleave;
//! - threads racing for retries are granted the reserve exactly, never more;
//! - the same seed replays the same grants and distinct seeds diverge;
//! - a host's shared budget pools at most twice the reserve, not one reserve
//!   per run, however many first attempts burst into it;
//! - a host budget grants no more than the reserve plus a fifth of its
//!   traffic, however its attempts, retries and clock advances interleave;
//! - credit above the reserve expires after the time to live, the reserve
//!   survives it, and the window earns again from the reserve;
//! - runs racing on one host stay within the income bound;
//! - the same seed replays the same host grants and distinct seeds diverge.
//!
//! Sources: Finagle's `RetryBudget` (a reserve plus `percentCanRetry` of 20%),
//! gRPC's per-server retry throttle (one `token_count` per server, not per
//! call), arXiv:2608.25403 on retry storms under correlated failure, and
//! arXiv:2510.03551 on the metastable feedback loop the budget breaks.

use std::num::NonZeroUsize;
use std::time::Duration;

use crate::rt::clock::Clock;

use super::{
    FAN_OUT_PER_CORE, MAX_IN_FLIGHT, MILLI, Policy, RETRY_CEILING_MILLI, RETRY_RATIO_MILLI,
    RETRY_RESERVE, RETRY_RESERVE_MILLI, RETRY_TTL_NANOS,
};
use crate::sim_stream::{RECEIPT_START, Stream, fold};

/// The seeds every family replays.
const SEEDS: [u64; 4] = [
    0xB0D6_0000_0000_0001,
    0xB0D6_0000_0000_0002,
    0xB0D6_FFFF_0000_0003,
    0x5701_B0D6_0000_0004,
];

/// Operations each interleaving draws.
const STEPS: usize = 512;

/// Interleavings each family draws per seed.
const RUNS: usize = 16;

/// A family's result.
type Outcome = Result<(), String>;

/// The reference budget: a balance in thousandths of a retry.
struct Reference {
    /// Thousandths of a retry held.
    balance: u64,
}

impl Reference {
    /// A fresh run's budget: the reserve.
    const fn new() -> Self {
        Self {
            balance: RETRY_RESERVE * MILLI,
        }
    }

    /// A first attempt deposits a fifth of a retry.
    fn first_attempt(&mut self) {
        self.balance = self.balance.saturating_add(RETRY_RATIO_MILLI);
    }

    /// A retry withdraws one whole retry, or is refused and withdraws nothing.
    fn take_retry(&mut self) -> bool {
        match self.balance.checked_sub(MILLI) {
            Some(left) => {
                self.balance = left;
                true
            }
            None => false,
        }
    }
}

/// Retries a run with `firsts` first attempts may ever be granted.
fn ceiling(firsts: u64) -> Result<u64, String> {
    let earned = firsts
        .saturating_mul(RETRY_RATIO_MILLI)
        .checked_div(MILLI)
        .ok_or("a whole retry is a non-zero number of thousandths")?;
    Ok(RETRY_RESERVE.saturating_add(earned))
}

/// The retries an interleaving granted, counted.
fn granted_of(grants: &[bool]) -> Result<u64, String> {
    u64::try_from(grants.iter().filter(|granted| **granted).count())
        .map_err(|error| error.to_string())
}

/// One drawn interleaving, run against the shipped policy and the reference;
/// returns the grants and the first attempts made.
fn interleave(stream: &mut Stream, seed: u64) -> Result<(Vec<bool>, u64), String> {
    let policy = Policy::for_this_machine();
    let mut reference = Reference::new();
    let attempt_weight = stream.below(4)?;
    let mut grants = Vec::with_capacity(STEPS);
    let mut firsts = 0_u64;
    for step in 0..STEPS {
        if stream.below(4)? <= attempt_weight {
            policy.first_attempt();
            reference.first_attempt();
            firsts = firsts.saturating_add(1);
        } else {
            let granted = policy.take_retry();
            assert_eq!(
                granted,
                reference.take_retry(),
                "seed {seed:#x}: step {step}"
            );
            grants.push(granted);
        }
    }
    Ok((grants, firsts))
}

#[test]
/// An unbounded `each` runs the cores' allowance under the ceiling, never
/// zero and never past it.
fn sim_the_fan_out_is_the_machines_allowance_under_the_ceiling() -> Outcome {
    let cores = match std::thread::available_parallelism() {
        Ok(cores) => cores.get(),
        Err(_) => 1,
    };
    let expected = cores.saturating_mul(FAN_OUT_PER_CORE).min(MAX_IN_FLIGHT);
    for _ in SEEDS {
        let fan_out = Policy::for_this_machine().fan_out();
        assert_eq!(fan_out.get(), expected, "{cores} cores");
        assert!(fan_out >= NonZeroUsize::MIN && fan_out.get() <= MAX_IN_FLIGHT);
    }
    let placeholder = Policy::one_body_per_core_placeholder();
    assert_eq!(
        placeholder.fan_out(),
        NonZeroUsize::MIN,
        "no parallelism, one body"
    );
    Ok(())
}

#[test]
/// A fresh run holds exactly the reserve, and five first attempts earn one
/// more retry.
fn sim_a_fresh_run_holds_the_reserve_and_five_attempts_earn_one() -> Outcome {
    for seed in SEEDS {
        let mut stream = Stream::new(seed);
        for _ in 0..RUNS {
            let policy = Policy::for_this_machine();
            let granted = (0..RETRY_RESERVE).filter(|_| policy.take_retry()).count();
            assert_eq!(
                u64::try_from(granted).map_err(|error| error.to_string())?,
                RETRY_RESERVE
            );
            assert!(!policy.take_retry(), "seed {seed:#x}: the reserve is spent");
            let short = stream.below(5)?;
            for _ in 0..short {
                policy.first_attempt();
            }
            assert!(
                !policy.take_retry(),
                "seed {seed:#x}: {short} attempts earn no whole retry"
            );
            for _ in short..5 {
                policy.first_attempt();
            }
            assert!(
                policy.take_retry(),
                "seed {seed:#x}: five attempts earn one"
            );
            assert!(!policy.take_retry(), "seed {seed:#x}: and only one");
        }
    }
    Ok(())
}

#[test]
/// Every drawn interleaving grants exactly the retries the reference grants.
fn sim_every_interleaving_matches_the_reference() -> Outcome {
    for seed in SEEDS {
        let mut stream = Stream::new(seed);
        for _ in 0..RUNS {
            interleave(&mut stream, seed)?;
        }
    }
    Ok(())
}

#[test]
/// A run never retries more than its reserve plus a fifth of its first
/// attempts.
fn sim_a_run_never_retries_past_its_budget() -> Outcome {
    for seed in SEEDS {
        let mut stream = Stream::new(seed);
        for _ in 0..RUNS {
            let (grants, firsts) = interleave(&mut stream, seed)?;
            let granted = granted_of(&grants)?;
            assert!(
                granted <= ceiling(firsts)?,
                "seed {seed:#x}: {granted} retries from {firsts} first attempts"
            );
        }
    }
    Ok(())
}

#[test]
/// A refused retry spends nothing: the attempts that follow it earn their
/// retry as though it had not been asked for.
fn sim_a_refused_retry_spends_nothing() -> Outcome {
    for seed in SEEDS {
        let mut stream = Stream::new(seed);
        for _ in 0..RUNS {
            let policy = Policy::for_this_machine();
            while policy.take_retry() {}
            let refusals = stream.below(32)?.saturating_add(1);
            for _ in 0..refusals {
                assert!(
                    !policy.take_retry(),
                    "seed {seed:#x}: an empty budget refuses"
                );
            }
            for _ in 0..5 {
                policy.first_attempt();
            }
            assert!(
                policy.take_retry(),
                "seed {seed:#x}: {refusals} refusals spent something"
            );
        }
    }
    Ok(())
}

#[test]
/// A correlated failure across a thousand items spends the documented
/// budget, not a thousand items' worth of retries.
fn sim_a_correlated_failure_spends_the_budget_not_the_fleet() -> Outcome {
    for seed in SEEDS {
        let mut stream = Stream::new(seed);
        let items = stream.below(1_000)?.saturating_add(1_000);
        let tries_each = stream.below(4)?.saturating_add(2);
        let policy = Policy::for_this_machine();
        let mut granted = 0_u64;
        for _ in 0..items {
            policy.first_attempt();
            for _ in 1..tries_each {
                if policy.take_retry() {
                    granted = granted.saturating_add(1);
                } else {
                    break;
                }
            }
        }
        let firsts = u64::try_from(items).map_err(|error| error.to_string())?;
        let budget = ceiling(firsts)?;
        assert!(
            granted <= budget,
            "seed {seed:#x}: {granted} of {items} items"
        );
        assert!(
            granted >= budget.saturating_sub(RETRY_RESERVE),
            "seed {seed:#x}: the earned fifth was spent"
        );
    }
    Ok(())
}

/// The grants `threads` scoped threads running `body` at once make in total.
///
/// Scoped, so every thread is joined before this returns; a thread that
/// panicked counts as `usize::MAX`, which fails every bound a caller asserts.
fn race(threads: usize, body: impl Fn() -> usize + Sync) -> usize {
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads).map(|_| scope.spawn(&body)).collect();
        handles
            .into_iter()
            .map(|handle| match handle.join() {
                Ok(count) => count,
                Err(_) => usize::MAX,
            })
            .fold(0_usize, usize::saturating_add)
    })
}

#[test]
/// Threads racing for retries are granted the reserve exactly.
fn sim_racing_threads_are_granted_the_reserve_exactly() -> Outcome {
    for seed in SEEDS {
        let mut stream = Stream::new(seed);
        let threads = stream.below(7)?.saturating_add(2);
        let policy = Policy::for_this_machine();
        let granted = race(threads, || {
            (0..RETRY_RESERVE).filter(|_| policy.take_retry()).count()
        });
        let reserve = usize::try_from(RETRY_RESERVE).map_err(|error| error.to_string())?;
        assert_eq!(granted, reserve, "seed {seed:#x}: {threads} threads");
    }
    Ok(())
}

#[test]
/// Threads depositing and withdrawing together never exceed the budget.
fn sim_racing_deposits_and_retries_stay_within_budget() -> Outcome {
    for seed in SEEDS {
        let mut stream = Stream::new(seed);
        let threads = stream.below(5)?.saturating_add(2);
        let deposits = stream.below(200)?.saturating_add(50);
        let policy = Policy::for_this_machine();
        let granted = race(threads, || {
            (0..deposits)
                .filter(|_| {
                    policy.first_attempt();
                    policy.take_retry()
                })
                .count()
        });
        let firsts =
            u64::try_from(threads.saturating_mul(deposits)).map_err(|error| error.to_string())?;
        let granted = u64::try_from(granted).map_err(|error| error.to_string())?;
        let budget = ceiling(firsts)?;
        assert!(granted <= budget, "seed {seed:#x}: {granted} from {firsts}");
        assert!(!policy.take_retry() || granted < budget, "seed {seed:#x}");
    }
    Ok(())
}

/// Folds one interleaving's grants and first-attempt count into the trace.
fn fold_run(trace: u64, grants: &[bool], firsts: u64) -> u64 {
    let bits: Vec<u8> = grants.iter().map(|granted| u8::from(*granted)).collect();
    fold(fold(trace, &bits), &firsts.to_le_bytes())
}

/// The replay receipt of every grant drawn under `seed`.
fn grant_trace(seed: u64) -> Result<u64, String> {
    let mut stream = Stream::new(seed);
    let mut trace = RECEIPT_START;
    for _ in 0..RUNS {
        let (grants, firsts) = interleave(&mut stream, seed)?;
        trace = fold_run(trace, &grants, firsts);
    }
    Ok(trace)
}

#[test]
/// The replay oracle: one seed, one grant trace.
fn sim_the_same_seed_replays_the_same_grants() -> Outcome {
    for seed in SEEDS {
        assert_eq!(
            grant_trace(seed)?,
            grant_trace(seed)?,
            "seed {seed:#x} replays"
        );
    }
    Ok(())
}

#[test]
/// The divergence oracle: a sweep that ignored its seed would pass the replay.
fn sim_distinct_seeds_draw_different_grants() -> Outcome {
    assert_ne!(
        grant_trace(SEEDS[0])?,
        grant_trace(SEEDS[1])?,
        "two seeds drew alike"
    );
    Ok(())
}

/// The host budget's reference: a balance in thousandths of a retry, the
/// instant credit above the reserve was last earned, and the clock's now.
struct HostReference {
    /// Thousandths of a retry held by the host.
    balance: u64,
    /// The last instant a first attempt earned credit above the reserve.
    stamp: u64,
    /// The clock's now, in nanoseconds.
    now: u64,
}

impl HostReference {
    /// A fresh host budget: the reserve, never yet earned from.
    const fn new() -> Self {
        Self {
            balance: RETRY_RESERVE_MILLI,
            stamp: 0,
            now: 0,
        }
    }

    /// The balance a withdrawal sees: credit above the reserve past the time
    /// to live is gone, and what was already spent stays spent.
    fn held(&self) -> u64 {
        if self.now.saturating_sub(self.stamp) > RETRY_TTL_NANOS {
            self.balance.min(RETRY_RESERVE_MILLI)
        } else {
            self.balance
        }
    }

    /// A first attempt deposits a fifth of a retry, capped at twice the
    /// reserve, and refreshes the earning stamp.
    fn first_attempt(&mut self) {
        self.balance = self
            .held()
            .saturating_add(RETRY_RATIO_MILLI)
            .min(RETRY_CEILING_MILLI);
        self.stamp = self.stamp.max(self.now);
    }

    /// A retry withdraws one whole retry from what is held, or is refused.
    fn take_retry(&mut self) -> bool {
        match self.held().checked_sub(MILLI) {
            Some(left) => {
                self.balance = left;
                true
            }
            None => false,
        }
    }

    /// The clock advanced.
    fn advance(&mut self, nanos: u64) {
        self.now = self.now.saturating_add(nanos);
    }
}

/// A host budget on a fresh virtual clock, for families that advance it.
fn host_budget() -> (Clock, Policy) {
    let clock = Clock::virtual_at(Duration::ZERO);
    let policy = Policy::for_this_host(clock.clone());
    (clock, policy)
}

/// One drawn interleaving against a host's shared budget and its reference;
/// returns the grants and the first attempts made.
fn host_interleave(stream: &mut Stream, seed: u64) -> Result<(Vec<bool>, u64), String> {
    let (clock, policy) = host_budget();
    let mut reference = HostReference::new();
    let mut grants = Vec::with_capacity(STEPS);
    let mut firsts = 0_u64;
    for step in 0..STEPS {
        match stream.below(8)? {
            0..=3 => {
                firsts = firsts.saturating_add(1);
                reference.first_attempt();
                policy.first_attempt();
            }
            4..=6 => {
                let granted = policy.take_retry();
                assert_eq!(
                    granted,
                    reference.take_retry(),
                    "seed {seed:#x}: host step {step}"
                );
                grants.push(granted);
            }
            _ => {
                let millis =
                    u64::try_from(stream.below(1_000)?).map_err(|error| error.to_string())?;
                clock
                    .advance(Duration::from_millis(millis))
                    .map_err(|error| error.to_string())?;
                reference.advance(millis.saturating_mul(1_000_000));
            }
        }
    }
    Ok((grants, firsts))
}

/// The retries a host budget grants until it refuses, drained.
fn drain(policy: &Policy) -> u64 {
    let mut granted = 0_u64;
    while policy.take_retry() {
        granted = granted.saturating_add(1);
    }
    granted
}

#[test]
/// A host-shaped storm pools at twice the reserve, not one reserve per run.
fn sim_a_host_shaped_storm_pools_twice_the_reserve_not_the_fleet() -> Outcome {
    for seed in SEEDS {
        let mut stream = Stream::new(seed);
        let items = stream.below(1_000)?.saturating_add(2_000);
        let policy = Policy::for_this_host(Clock::virtual_at(Duration::ZERO));
        (0..items).for_each(|_| policy.first_attempt());
        let granted = drain(&policy);
        let expected = RETRY_CEILING_MILLI
            .checked_div(MILLI)
            .ok_or("a whole retry is a non-zero number of thousandths")?;
        assert_eq!(
            granted, expected,
            "seed {seed:#x}: {items} runs burst into one host budget"
        );
    }
    Ok(())
}

#[test]
/// A host budget grants no more than the reserve plus a fifth of its traffic.
fn sim_a_host_budget_grants_no_more_than_a_fifth_of_its_traffic() -> Outcome {
    for seed in SEEDS {
        let mut stream = Stream::new(seed);
        for _ in 0..RUNS {
            let (grants, firsts) = host_interleave(&mut stream, seed)?;
            let granted = granted_of(&grants)?;
            let budget = ceiling(firsts)?;
            assert!(
                granted <= budget,
                "seed {seed:#x}: {granted} host retries from {firsts} first attempts"
            );
        }
    }
    Ok(())
}

#[test]
/// Credit above the reserve expires after the time to live, and the reserve
/// survives the expiry.
fn sim_earned_credit_expires_after_the_ttl() -> Outcome {
    for seed in SEEDS {
        let mut stream = Stream::new(seed);
        let (clock, policy) = host_budget();
        let firsts = stream.below(16)?.saturating_add(25);
        for _ in 0..firsts {
            policy.first_attempt();
        }
        let spent = firsts
            .checked_div(5)
            .ok_or("five first attempts earn one whole retry")?;
        for _ in 0..spent {
            assert!(
                policy.take_retry(),
                "seed {seed:#x}: earned credit spends before the expiry"
            );
        }
        clock
            .advance(Duration::from_nanos(RETRY_TTL_NANOS + 1))
            .map_err(|error| error.to_string())?;
        let granted = drain(&policy);
        assert_eq!(
            granted, RETRY_RESERVE,
            "seed {seed:#x}: only the reserve survives the expiry"
        );
        assert!(!policy.take_retry(), "seed {seed:#x}: and nothing more");
    }
    Ok(())
}

#[test]
/// An expired window earns again from the reserve: five fresh first attempts
/// after the expiry deposit afresh.
fn sim_an_expired_window_earns_again_from_the_reserve() -> Outcome {
    for seed in SEEDS {
        let (clock, policy) = host_budget();
        let mut stream = Stream::new(seed);
        let firsts = stream.below(16)?.saturating_add(30);
        (0..firsts).for_each(|_| policy.first_attempt());
        clock
            .advance(Duration::from_nanos(RETRY_TTL_NANOS.saturating_add(1)))
            .map_err(|error| error.to_string())?;
        for _ in 0..5 {
            policy.first_attempt();
        }
        let granted = drain(&policy);
        assert_eq!(
            granted,
            RETRY_RESERVE + 1,
            "seed {seed:#x}: the window earned again from the reserve"
        );
    }
    Ok(())
}

#[test]
/// Runs racing on one host stay within the income bound.
fn sim_racing_runs_on_one_host_stay_within_the_income_bound() -> Outcome {
    for seed in SEEDS {
        let mut stream = Stream::new(seed);
        let pairs = stream.below(32)?.saturating_add(32);
        let threads = stream.below(7)?.saturating_add(2);
        let policy = Policy::for_this_host(Clock::virtual_at(Duration::ZERO));
        let granted = race(threads, || {
            (0..pairs)
                .filter(|_| {
                    policy.first_attempt();
                    policy.take_retry()
                })
                .count()
        });
        let firsts =
            u64::try_from(threads.saturating_mul(pairs)).map_err(|error| error.to_string())?;
        let budget = ceiling(firsts)?;
        let granted = u64::try_from(granted).map_err(|error| error.to_string())?;
        assert!(granted <= budget, "seed {seed:#x}: {granted} of {firsts}");
        assert!(granted >= RETRY_RESERVE, "seed {seed:#x}: the reserve held");
    }
    Ok(())
}

/// The replay receipt of every host grant drawn under `seed`.
fn host_grant_trace(seed: u64) -> Result<u64, String> {
    let mut trace = RECEIPT_START;
    let mut stream = Stream::new(seed);
    for _ in 0..RUNS {
        let (grants, firsts) = host_interleave(&mut stream, seed)?;
        trace = fold_run(trace, &grants, firsts);
    }
    Ok(trace)
}

#[test]
/// The host replay oracle: one seed, one grant trace.
fn sim_the_same_seed_replays_the_same_host_grants() -> Outcome {
    for seed in SEEDS {
        assert_eq!(
            host_grant_trace(seed)?,
            host_grant_trace(seed)?,
            "seed {seed:#x} host-replays"
        );
    }
    Ok(())
}

#[test]
/// The host divergence oracle: a sweep that ignored its seed would pass.
fn sim_distinct_seeds_draw_different_host_grants() -> Outcome {
    assert_ne!(
        host_grant_trace(SEEDS[0])?,
        host_grant_trace(SEEDS[1])?,
        "two host seeds drew alike"
    );
    Ok(())
}
