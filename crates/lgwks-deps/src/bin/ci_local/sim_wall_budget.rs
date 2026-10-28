//! Deterministic simulation of the wall budget: one seed draws a run's lanes,
//! their groups and their durations, and the run is judged on simulated time,
//! so a run can be placed exactly on the budget or a nanosecond past it.
//! Derived from deterministic simulation testing as FoundationDB practised it:
//! time and randomness come from one seed, and the same seed replays the same
//! trace.

use std::time::Duration;

use lgwks_std::seeded::{DrawError, Seeded};

use super::{WALL_BUDGET, over_budget};

/// Runs simulated per seed.
const RUNS: u64 = 4_000;
/// The most lanes a simulated run has.
const LANES_MAX: u64 = 48;
/// The most groups a simulated run has.
const GROUPS_MAX: u64 = 8;
/// The longest a simulated lane runs, in nanoseconds: two minutes.
const LANE_MAX_NANOS: u64 = 120_000_000_000;

/// One simulated run's wall time: groups run at once, lanes within a group in
/// order, so the wall is the slowest group's sum. Three runs in eight are put
/// on the budget, or a nanosecond either side of it.
fn simulated_wall(stream: &mut Seeded) -> Result<Duration, DrawError> {
    let groups = stream.below(GROUPS_MAX)?.saturating_add(1);
    let mut sums = [Duration::ZERO; 8];
    for _ in 0..stream.below(LANES_MAX)?.saturating_add(1) {
        let lane = Duration::from_nanos(stream.below(LANE_MAX_NANOS)?);
        let slot = stream.below(groups)?;
        for (index, sum) in (0_u64..).zip(sums.iter_mut()) {
            if index == slot {
                *sum = sum.saturating_add(lane);
            }
        }
    }
    let wall = sums.into_iter().fold(Duration::ZERO, Duration::max);
    Ok(match stream.below(8)? {
        0 => WALL_BUDGET,
        1 => WALL_BUDGET.saturating_add(Duration::from_nanos(1)),
        2 => WALL_BUDGET.saturating_sub(Duration::from_nanos(1)),
        _ => wall,
    })
}

/// Simulates every run of `seed`; returns the trace hash and how many runs were
/// refused, after checking each verdict against whole-nanosecond arithmetic.
fn simulate(seed: u64) -> Result<(u64, u64), DrawError> {
    let mut stream = Seeded::from_seed(seed);
    let mut trace: u64 = 0xCBF2_9CE4_8422_2325;
    let mut refused: u64 = 0;
    let limit = WALL_BUDGET.as_nanos();
    for _ in 0..RUNS {
        let wall = simulated_wall(&mut stream)?;
        let verdict = over_budget(wall);
        assert_eq!(verdict, wall.as_nanos() > limit, "seed {seed}: {wall:?}");
        refused = refused.saturating_add(u64::from(verdict));
        for byte in wall
            .as_nanos()
            .to_le_bytes()
            .into_iter()
            .chain([u8::from(verdict)])
        {
            trace = (trace ^ u64::from(byte)).wrapping_mul(0x0100_0000_01B3);
        }
    }
    Ok((trace, refused))
}

#[test]
fn sim_seeded_runs_refuse_any_nanosecond_past_the_budget() -> Result<(), DrawError> {
    for seed in 0..16 {
        let (trace, refused) = simulate(seed)?;
        assert_eq!(simulate(seed)?, (trace, refused), "seed {seed} replays");
        assert!(
            refused > 0 && refused < RUNS,
            "seed {seed} refused {refused} of {RUNS}"
        );
    }
    assert_ne!(
        simulate(1)?.0,
        simulate(2)?.0,
        "another seed, another trace"
    );
    Ok(())
}
