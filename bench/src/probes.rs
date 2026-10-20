//! The three probes beside the timing table: capability-check scaling, the cost
//! of admitting a bot, and the determinism replay.
//!
//! They answer questions the paired ratio cannot: how `Auth::check` scales with
//! the capability count, what one `build` costs, and whether the schedule is a
//! function of its inputs. They share the table's engine builder, so a probe can
//! never measure a differently built bot than the timing rows.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use lgwks_bot::{Cap, GrantSet};

use crate::scenarios::Scenario;
use crate::{Clock, build_bot};

/// Time `Auth::check` as the number of required capabilities grows.
///
/// `Auth` is minted by cloning the required slice into a `Vec`, and `check`
/// then asks, for each required capability, whether that `Vec` contains it. That
/// is a linear scan inside a linear scan, so the expected shape is quadratic in
/// the capability count. Four shipped capabilities make that invisible; the
/// measurement is here to establish where it stops being invisible, because
/// `Cap::new` accepts any name by convention and a custom domain may require
/// many.
pub(super) fn cap_check_scaling() -> Result<Vec<(usize, f64)>, Box<dyn std::error::Error>> {
    let mut out = Vec::new();
    for count in [1usize, 2, 4, 8, 16, 32, 64, 128] {
        let mut grants = GrantSet::empty();
        let mut caps = Vec::with_capacity(count);
        for index in 0..count {
            let cap = Cap::new(format!("bench.cap.{index}"));
            grants = grants.grant(cap.clone());
            caps.push(cap);
        }
        let auth = grants.issue(&caps)?;

        // Warm the cache and the branch predictor before timing.
        for _ in 0..1_000 {
            auth.check(&caps)?;
        }

        const ITERATIONS: u32 = 200_000;
        let start = Instant::now();
        for _ in 0..ITERATIONS {
            auth.check(&caps)?;
        }
        let elapsed = start.elapsed().as_secs_f64();
        let per_call_ns = (elapsed / f64::from(ITERATIONS)) * 1e9;
        out.push((count, per_call_ns));
    }
    Ok(out)
}

/// Time `Bot::builder(..).build(&grants)` -- the admission path.
///
/// One admission of `sources` chains, measured once: it is the cost of building
/// the widget the timing rows then tick, and a row that reported ticks without
/// it would leave a reader unable to say what a run paid before its first tick.
pub(super) fn build_cost(sources: usize) -> Result<Duration, Box<dyn std::error::Error>> {
    let clock = Clock::new();
    let tally = Rc::new(Cell::new(0u64));
    let evals = Rc::new(Cell::new(0u64));
    let scenario = Scenario {
        name: "build",
        intent: "admission cost",
        sources,
        period: 100,
        entries: 1,
        ticks_per_round: 1,
        warmup_ticks: 1,
        rounds: 1,
    };
    let start = Instant::now();
    let _bot = build_bot(&scenario, &clock, &tally, &evals)?;
    Ok(start.elapsed())
}

/// Run the same scenario twice and compare the per-tick effect counts.
///
/// This is a *correctness* observation, not a timing one, and it is the only
/// claim in this rig that the timings cannot make: that the schedule is a
/// function of its inputs. The formal statement of the same property, and the
/// proof, are in `../proofs/`.
pub(super) fn determinism_probe(
    scenario: &Scenario,
    ticks: u64,
) -> Result<(bool, usize), Box<dyn std::error::Error>> {
    let run = || -> Result<Vec<u64>, Box<dyn std::error::Error>> {
        let clock = Clock::new();
        let tally = Rc::new(Cell::new(0u64));
        let evals = Rc::new(Cell::new(0u64));
        let mut bot = build_bot(scenario, &clock, &tally, &evals)?;
        let mut per_tick = Vec::with_capacity(ticks as usize);
        for tick in 0..ticks {
            clock.set(tick);
            per_tick.push(bot.tick()? as u64);
        }
        Ok(per_tick)
    };
    let first = run()?;
    let second = run()?;
    let fired: u64 = first.iter().sum();
    Ok((first == second, fired as usize))
}
