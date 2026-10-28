//! Deterministic simulation of ADR 0006 R2: one seed draws a sequence of
//! estates, hosts and one-off `block_on` calls across tenants and tiers, and
//! however the doors interleave, the process builds at most one runtime.
//! Derived from deterministic simulation testing as FoundationDB practised it:
//! the seed is the whole schedule, and the same seed replays the same receipt.

use std::fmt;
use std::sync::atomic::Ordering;

use super::{BUILT, block_on, driver};
use crate::rt::estate::Estate;
use crate::sim_stream::{RECEIPT_START, Stream, fold};
use crate::task::task;

/// Seeds swept.
const SEEDS: [u64; 8] = [0, 1, 2, 3, 5, 8, 13, 21];
/// Doors opened per seed.
const STEPS: usize = 48;
/// Tenants a step may run for.
const TENANTS: [&str; 4] = ["acme", "globex", "initech", "umbrella"];

/// Why a simulated schedule failed.
#[derive(Debug)]
enum SimError {
    /// A door refused: the task, an estate, the host, the driver or the stream.
    Refused(String),
    /// A door answered something other than its input plus one.
    Answer {
        /// The schedule's seed.
        seed: u64,
        /// The door drawn.
        door: usize,
        /// The tenant it ran for.
        tenant: &'static str,
        /// What it answered.
        answer: Option<u64>,
    },
}

impl fmt::Display for SimError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Refused(ref why) => write!(formatter, "a door refused: {why}"),
            Self::Answer {
                seed,
                door,
                tenant,
                answer,
            } => write!(
                formatter,
                "seed {seed}: door {door} for {tenant} answered {answer:?}"
            ),
        }
    }
}

impl std::error::Error for SimError {}

/// A refusal from any door, kept as its own words.
fn refused(error: impl fmt::Display) -> SimError {
    SimError::Refused(error.to_string())
}

/// Runs every step of `seed`; returns the receipt of what each door answered.
fn simulate(seed: u64) -> Result<u64, SimError> {
    let probe = task("sim-one-driver", |_scope, input: u64| async move {
        Ok(input.saturating_add(1))
    })
    .map_err(refused)?;
    let mut stream = Stream::new(seed);
    let mut receipt = RECEIPT_START;
    for _ in 0..STEPS {
        let door = stream.below(5).map_err(refused)?;
        let tenant = stream.choose(&TENANTS).map_err(refused)?;
        let input = u64::try_from(stream.below(1_000).map_err(refused)?).map_err(refused)?;
        let answer = match door {
            0 => {
                let mut estate = Estate::supervised(tenant, 4).map_err(refused)?;
                let run = estate.run_blocking(&probe, input).map_err(refused)?;
                if stream.below(2).map_err(refused)? == 0 {
                    estate.shutdown_blocking().map_err(refused)?;
                }
                run.output().copied()
            }
            1 => {
                let estate = Estate::in_process(tenant).map_err(refused)?;
                let run = estate.run_blocking(&probe, input).map_err(refused)?;
                run.output().copied()
            }
            2 => {
                let estate = Estate::in_process(tenant).map_err(refused)?;
                let run = estate.host().block_on(&probe, input).map_err(refused)?;
                run.output().copied()
            }
            3 => Some(block_on(async move { input.saturating_add(1) })),
            _ => Some(
                driver()
                    .map_err(refused)?
                    .block_on(async move { input.saturating_add(1) }),
            ),
        };
        if answer != Some(input.saturating_add(1)) {
            let wrong = SimError::Answer {
                seed,
                door,
                tenant,
                answer,
            };
            lgwks_std::trace::debug!(error = %wrong, "simulate: a door answered wrongly");
            return Err(wrong);
        }
        receipt = fold(receipt, &[u8::try_from(door).map_err(refused)?]);
        receipt = fold(receipt, &input.to_le_bytes());
    }
    Ok(receipt)
}

#[test]
fn sim_every_door_in_any_order_shares_one_driver() -> Result<(), SimError> {
    let before = BUILT.load(Ordering::Relaxed);
    for seed in SEEDS {
        let receipt = simulate(seed)?;
        assert_eq!(simulate(seed)?, receipt, "seed {seed} replays");
    }
    assert_ne!(simulate(1)?, simulate(2)?, "another seed, another schedule");
    // At most one: the runner may share the process, and then a test before
    // this one built the driver and this sweep builds none.
    assert!(
        BUILT.load(Ordering::Relaxed).saturating_sub(before) <= 1,
        "{} schedules of estates, hosts and block_on built more than one runtime",
        SEEDS.len().saturating_mul(2).saturating_add(2)
    );
    Ok(())
}
