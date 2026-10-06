//! Simulation family: waiting on a supervisor at its bound and at its end.
//!
//! Every scenario drives the real `Supervisor` on the real runtime. The seed
//! chooses the shape of the run — the bound, how many tasks, how long each body
//! runs before it returns, and whether the caller reaps between spawns — never a
//! clock reading, so the trace is built from counts that do not depend on how a
//! busy host scheduled the bodies.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `every_placed_task_is_joined_once` | `spawn` at the bound plus `wait_idle` at the end joins every task exactly once: completions, work and outcomes all conserve |
//!
//! Every band runs twice through `sim::assert_replays`, which compares the two
//! trace hashes, so each test is also its own replay receipt.
//!
//! `spawn` at a full bound waits on the task set and absorbs what it joins;
//! `wait_idle` joins the rest. A task joined by neither, or by both, shows up
//! here as a completion count that is not the placed count, or as an outcome
//! that is neither retained nor counted as dropped.

#![cfg(feature = "rt")]

use crate::sim;

// The band-declaration macro, defined once for the whole layer.
use crate::band_family;

use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lgwks_bot::rt::supervise::Supervisor;
use lgwks_bot::rt::task::yield_now;

use sim::Band;

type TestResult = Result<(), Box<dyn Error>>;

/// One seeded run's shape.
struct Plan {
    /// The in-flight ceiling.
    bound: usize,
    /// How many tasks the caller places.
    tasks: usize,
    /// For each task, how many times its body yields before it returns.
    yields: Vec<u32>,
    /// Whether the caller reaps after every spawn, as a long-lived caller does.
    reap_between: bool,
}

fn plan(rng: &mut sim::Rng) -> Result<Plan, Box<dyn Error>> {
    let bound = usize::try_from(rng.between(1, 17))?;
    let tasks = usize::try_from(rng.below(257))?;
    let mut yields = Vec::with_capacity(tasks);
    for _ in 0..tasks {
        yields.push(rng.below(5));
    }
    Ok(Plan {
        bound,
        tasks,
        yields,
        reap_between: rng.chance(400),
    })
}

fn every_placed_task_is_joined_once(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let plan = plan(sim.rng())?;
        let work = Arc::new(AtomicU64::new(0));
        let runtime = lgwks_bot::Runtime::new()?;
        let (stats, retained) = runtime.block_on(async {
            let mut supervisor = Supervisor::new(plan.bound);
            for &turns in &plan.yields {
                let work = Arc::clone(&work);
                supervisor
                    .spawn(move |_token| async move {
                        for _ in 0..turns {
                            yield_now().await;
                        }
                        work.fetch_add(1, Ordering::SeqCst);
                    })
                    .await;
                if plan.reap_between {
                    supervisor.reap();
                }
            }
            supervisor.wait_idle().await;
            let stats = supervisor.stats();
            let mut retained: u64 = 0;
            while supervisor.next_report().is_some() {
                retained = retained.saturating_add(1);
            }
            (stats, retained)
        });
        let placed = u64::try_from(plan.tasks)?;
        assert_eq!(
            stats.spawned, placed,
            "every spawn was placed: nothing refused a live supervisor"
        );
        assert_eq!(
            stats.refused, 0,
            "waiting at the bound is backpressure, not refusal"
        );
        assert_eq!(stats.in_flight(), 0, "wait_idle leaves nothing in flight");
        assert_eq!(
            stats.succeeded, placed,
            "every task was joined exactly once"
        );
        assert_eq!(
            work.load(Ordering::SeqCst),
            placed,
            "every body ran to its end: a join without the work is a lost task"
        );
        assert_eq!(
            retained.saturating_add(stats.reports_dropped),
            placed,
            "every outcome is either retained or counted as dropped"
        );
        sim.record(&format!(
            "bound={} tasks={} reap_between={} succeeded={} work={}",
            plan.bound,
            plan.tasks,
            plan.reap_between,
            stats.succeeded,
            work.load(Ordering::SeqCst)
        ));
        Ok(())
    })
}

band_family::band_family! {
    every_placed_task_is_joined_once_band_00 => every_placed_task_is_joined_once, 0;
    every_placed_task_is_joined_once_band_01 => every_placed_task_is_joined_once, 1;
    every_placed_task_is_joined_once_band_02 => every_placed_task_is_joined_once, 2;
    every_placed_task_is_joined_once_band_03 => every_placed_task_is_joined_once, 3;
    every_placed_task_is_joined_once_band_04 => every_placed_task_is_joined_once, 4;
    every_placed_task_is_joined_once_band_05 => every_placed_task_is_joined_once, 5;
    every_placed_task_is_joined_once_band_06 => every_placed_task_is_joined_once, 6;
    every_placed_task_is_joined_once_band_07 => every_placed_task_is_joined_once, 7;
    every_placed_task_is_joined_once_band_08 => every_placed_task_is_joined_once, 8;
    every_placed_task_is_joined_once_band_09 => every_placed_task_is_joined_once, 9;
    every_placed_task_is_joined_once_band_10 => every_placed_task_is_joined_once, 10;
    every_placed_task_is_joined_once_band_11 => every_placed_task_is_joined_once, 11;
    every_placed_task_is_joined_once_band_12 => every_placed_task_is_joined_once, 12;
    every_placed_task_is_joined_once_band_13 => every_placed_task_is_joined_once, 13;
    every_placed_task_is_joined_once_band_14 => every_placed_task_is_joined_once, 14;
    every_placed_task_is_joined_once_band_15 => every_placed_task_is_joined_once, 15;
    every_placed_task_is_joined_once_band_16 => every_placed_task_is_joined_once, 16;
    every_placed_task_is_joined_once_band_17 => every_placed_task_is_joined_once, 17;
    every_placed_task_is_joined_once_band_18 => every_placed_task_is_joined_once, 18;
    every_placed_task_is_joined_once_band_19 => every_placed_task_is_joined_once, 19;
    every_placed_task_is_joined_once_band_20 => every_placed_task_is_joined_once, 20;
    every_placed_task_is_joined_once_band_21 => every_placed_task_is_joined_once, 21;
    every_placed_task_is_joined_once_band_22 => every_placed_task_is_joined_once, 22;
    every_placed_task_is_joined_once_band_23 => every_placed_task_is_joined_once, 23;
    every_placed_task_is_joined_once_band_24 => every_placed_task_is_joined_once, 24;
    every_placed_task_is_joined_once_band_25 => every_placed_task_is_joined_once, 25;
    every_placed_task_is_joined_once_band_26 => every_placed_task_is_joined_once, 26;
    every_placed_task_is_joined_once_band_27 => every_placed_task_is_joined_once, 27;
    every_placed_task_is_joined_once_band_28 => every_placed_task_is_joined_once, 28;
    every_placed_task_is_joined_once_band_29 => every_placed_task_is_joined_once, 29;
    every_placed_task_is_joined_once_band_30 => every_placed_task_is_joined_once, 30;
    every_placed_task_is_joined_once_band_31 => every_placed_task_is_joined_once, 31;
}
