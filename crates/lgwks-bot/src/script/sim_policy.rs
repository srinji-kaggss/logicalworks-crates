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
//! - the same seed replays the same grants and distinct seeds diverge.
//!
//! Sources: Finagle's `RetryBudget` (a reserve plus `percentCanRetry` of 20%),
//! arXiv:2608.25403 on retry storms under correlated failure, and
//! arXiv:2510.03551 on the metastable feedback loop the budget breaks.

use std::num::NonZeroUsize;

use super::{FAN_OUT_PER_CORE, MAX_IN_FLIGHT, MILLI, Policy, RETRY_RATIO_MILLI, RETRY_RESERVE};
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
            let granted = u64::try_from(grants.iter().filter(|granted| **granted).count())
                .map_err(|error| error.to_string())?;
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

/// The replay receipt of every grant drawn under `seed`.
fn grant_trace(seed: u64) -> Result<u64, String> {
    let mut stream = Stream::new(seed);
    let mut trace = RECEIPT_START;
    for _ in 0..RUNS {
        let (grants, firsts) = interleave(&mut stream, seed)?;
        let bits: Vec<u8> = grants.iter().map(|granted| u8::from(*granted)).collect();
        trace = fold(trace, &bits);
        trace = fold(trace, &firsts.to_le_bytes());
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
