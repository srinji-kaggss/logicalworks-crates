//! Simulation family: every placed task reports exactly one terminal outcome.
//!
//! Issue #703: a supervisor reaped a child but never returned its
//! `TaskOutcome`, parking the owner until the task's deadline (a 20-minute CI
//! timeout on logical_ci main, run 39589452879). The loss is after the reap —
//! the end is produced but never reaches `next_report` — so the candidates are
//! a dropped attribution in `absorb` or a report the capped buffer refused.
//!
//! The seed chooses the ceiling, the task count, each body's shape (a body
//! that runs to completion or one that waits on the supervisor's token), a
//! mid-run cancellation point, and a seeded interleaving of `try_spawn`,
//! `reap` and partial drains. Two ceiling modes pin the two loss candidates
//! apart: `Roomy` builds the bound above the task count, so the report buffer
//! can never refuse and every placed task must reach exactly one drained
//! outcome with `reports_dropped == 0`; `Tight` builds it small, where drops
//! are legal but conserved — retained plus dropped equals spawned, and no
//! retained identity repeats. A missing attribution fails `Roomy` outright,
//! because the outcome is gone and the counter cannot name which task lost it.
//!
//! Determinism discipline: the runtime schedules bodies on its own threads,
//! so anything that depends on completion order — which tasks a mid-run drain
//! finds, which reports the cap refuses — varies run to run. The assertions
//! are structural relations that hold under every interleaving (joins are
//! consumptive, every absorb retains-or-counts exactly once), and only
//! timing-free facts reach the trace: the seed's own draws always, run totals
//! only in `Roomy` mode, where no refusal and no drop is possible.
//!
//! What the family deliberately does not cover: panicking bodies (the
//! workspace forbids `panic!`, so `Panicked` is unit-tested, not simulated).
//! Supervised processes share this exact `absorb` tail — `task_end` maps every
//! `ProcessEnd` onto the same `TaskEnd` the plain bodies produce — and the
//! issue's own shape (lanes that exit under a reap-and-drain loop) runs in the
//! process suites beside this one.
//!
//! Every band runs twice through `sim::assert_replays`, which compares the two
//! trace hashes, so each test is also its own replay receipt.

#![cfg(feature = "rt")]

use crate::band_family;
use crate::sim;

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;

use lgwks_bot::rt::supervise::{Supervisor, TaskId};
use lgwks_bot::rt::task::yield_now;

use sim::Band;

type TestResult = Result<(), Box<dyn Error>>;

/// How one seeded body ends.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// Yields, then returns with no regard for the token: `Completed`.
    Complete,
    /// Waits on the supervisor's token: `Cancelled` once the driver cancels.
    WaitForCancel,
}

/// One seeded run's shape. Every draw happens up front in [`plan`], so the
/// driver below reads fields rather than the seed stream mid-run.
struct Plan {
    /// The in-flight ceiling, or room for every task when `roomy`.
    bound: usize,
    /// True when the bound exceeds the task count, so the report buffer can
    /// never refuse and every drop is a defect.
    roomy: bool,
    /// Each task's ending shape.
    shapes: Vec<Shape>,
    /// For each task, how many times a `Complete` body yields before returning.
    yields: Vec<u32>,
    /// Cancel before spawning this task; equal to the task count means cancel
    /// only at the end. Always resolves `WaitForCancel` bodies.
    cancel_at: usize,
    /// Whether to reap after each spawn, as a long-lived caller does.
    reap: Vec<bool>,
    /// How many reports to drain after each spawn; zero means no drain.
    drain: Vec<usize>,
}

fn plan(rng: &mut sim::Rng) -> Result<Plan, Box<dyn Error>> {
    let roomy = rng.chance(500);
    let tasks = usize::try_from(rng.below(257))?;
    let bound = if roomy {
        tasks.saturating_add(1).max(1)
    } else {
        usize::try_from(rng.between(1, 17))?
    };
    let reap_per_mille = rng.below(1001);
    let drain_per_mille = rng.below(1001);
    let mut shapes = Vec::with_capacity(tasks);
    let mut yields = Vec::with_capacity(tasks);
    let mut reap = Vec::with_capacity(tasks);
    let mut drain = Vec::with_capacity(tasks);
    for _ in 0..tasks {
        if rng.below(10) < 8 {
            shapes.push(Shape::Complete);
        } else {
            shapes.push(Shape::WaitForCancel);
        }
        yields.push(rng.below(5));
        reap.push(rng.chance(reap_per_mille));
        if rng.chance(drain_per_mille) {
            drain.push(usize::try_from(rng.below(8))?.saturating_add(1));
        } else {
            drain.push(0);
        }
    }
    Ok(Plan {
        bound,
        roomy,
        shapes,
        yields,
        cancel_at: usize::try_from(rng.below(u32::try_from(tasks.saturating_add(1))?))?,
        reap,
        drain,
    })
}

/// Drain up to `max` reports, attributing each to its task.
fn drain(
    supervisor: &mut Supervisor,
    max: usize,
    reported: &mut BTreeMap<TaskId, u64>,
    placed: &mut BTreeSet<TaskId>,
) {
    let mut taken = 0usize;
    while taken < max {
        let Some(outcome) = supervisor.next_report() else {
            break;
        };
        placed.insert(outcome.task());
        reported
            .entry(outcome.task())
            .and_modify(|count| *count = count.saturating_add(1))
            .or_insert(1);
        taken = taken.saturating_add(1);
    }
}

/// Collect the identities the supervisor is tracking right now: an outcome
/// drained for an identity never seen live is exactly the #703 shape, and it
/// still counts as placed.
fn note_live(supervisor: &Supervisor, placed: &mut BTreeSet<TaskId>) {
    for live in supervisor.snapshot().live() {
        placed.insert(live.task);
    }
}

fn every_placed_task_reports_exactly_once(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let plan = plan(sim.rng())?;
        let tasks = u64::try_from(plan.shapes.len())?;
        let runtime = lgwks_bot::Runtime::new()?;
        let outcome = runtime.block_on(async {
            let mut supervisor = Supervisor::new(plan.bound);
            let mut placed: BTreeSet<TaskId> = BTreeSet::new();
            let mut reported: BTreeMap<TaskId, u64> = BTreeMap::new();
            for (index, shape) in plan.shapes.iter().copied().enumerate() {
                if index == plan.cancel_at {
                    supervisor.cancel();
                }
                let turns = plan.yields[index];
                match shape {
                    Shape::Complete => supervisor
                        .try_spawn(move |_token| async move {
                            for _ in 0..turns {
                                yield_now().await;
                            }
                        })
                        .ok(),
                    Shape::WaitForCancel => supervisor
                        .try_spawn(|token| async move {
                            token.cancelled().await;
                        })
                        .ok(),
                };
                // `try_spawn` never waits, so this loop cannot hang: a
                // refusal is a task never placed, and the live set says which.
                note_live(&supervisor, &mut placed);
                if plan.reap[index] {
                    supervisor.reap();
                    note_live(&supervisor, &mut placed);
                }
                drain(
                    &mut supervisor,
                    plan.drain[index],
                    &mut reported,
                    &mut placed,
                );
                // The accessors agree with the snapshot they read: one state,
                // read three ways, with no await between the reads. The
                // tracked set can exceed the bound by ended-but-unreaped
                // tasks, which the snapshot truncates and counts.
                let snapshot = supervisor.snapshot();
                assert_eq!(
                    supervisor.in_flight(),
                    snapshot
                        .live()
                        .len()
                        .saturating_add(snapshot.live_truncated()),
                    "in_flight counts what the snapshot lists plus what it truncated"
                );
                // Exactly the snapshot's state, position included: the
                // accessor counts in one pass what the snapshot sorts, so a
                // position off by one in either is a disagreement here.
                for live in snapshot.live() {
                    assert_eq!(
                        supervisor.task_state(live.task),
                        Some(live.state),
                        "a task the snapshot lists reads its own state and position: {}",
                        live.task.get()
                    );
                }
            }
            supervisor.cancel();
            supervisor.wait_idle().await;
            drain(&mut supervisor, usize::MAX, &mut reported, &mut placed);
            let stats = supervisor.stats();
            // Every placed task is untracked now, and none has a state.
            let live_now = supervisor.in_flight();
            let mut tracked = 0usize;
            for task in placed.iter() {
                if supervisor.task_state(*task).is_some() {
                    tracked = tracked.saturating_add(1);
                }
            }
            (stats, reported, placed, live_now, tracked)
        });
        let (stats, reported, placed, live_now, tracked) = outcome;
        let placed_count = u64::try_from(placed.len())?;
        // Joins are consumptive — a joined task leaves the set — so the join
        // count cannot exceed the placed count, and every absorb retains or
        // counts exactly one outcome. These relations hold under every
        // interleaving, which is what makes them simulation-safe.
        assert_eq!(
            stats.completed, stats.spawned,
            "every placed task was joined and absorbed exactly once"
        );
        for (task, times) in reported.iter() {
            assert_eq!(
                *times,
                1,
                "task {} reached {} terminal outcomes: exactly one",
                task.get(),
                times
            );
        }
        assert_eq!(
            u64::try_from(reported.len())?,
            stats.spawned.saturating_sub(stats.reports_dropped),
            "every retained outcome is attributed, and every drop is counted"
        );
        assert!(
            stats.spawned.saturating_sub(placed_count) <= stats.reports_dropped,
            "a placed task the run never observed must have had its report dropped"
        );
        if plan.roomy {
            // No refusal is possible (permits exceed tasks until the seeded
            // cancel, which refuses deterministically) and no drop is
            // possible (outcomes never exceed the bound), so every count here
            // is timing-free and every placed task drained exactly once.
            assert_eq!(
                stats.spawned,
                u64::try_from(plan.cancel_at)?,
                "only the seeded cancel refuses, and it refuses deterministically"
            );
            assert_eq!(
                stats.reports_dropped, 0,
                "a bound above the task count never refuses a report: a drop here is the #703 loss"
            );
            assert_eq!(
                u64::try_from(reported.len())?,
                placed_count,
                "every placed task drained exactly one outcome"
            );
            assert_eq!(
                placed_count, stats.spawned,
                "every placed task was observed live or drained"
            );
        }
        assert_eq!(live_now, 0, "nothing is tracked after the drain");
        assert_eq!(tracked, 0, "no placed task has a state after the drain");
        assert_eq!(
            stats.in_flight(),
            0,
            "the accounting agrees the supervisor is empty"
        );
        if plan.roomy {
            sim.record(&format!(
                "bound={} tasks={} roomy=true placed={} completed={} retained={} dropped={}",
                plan.bound,
                tasks,
                placed_count,
                stats.completed,
                reported.len(),
                stats.reports_dropped
            ));
        } else {
            sim.record(&format!(
                "bound={} tasks={} roomy=false cancel_at={}",
                plan.bound, tasks, plan.cancel_at
            ));
        }
        Ok(())
    })
}

band_family::band_family! {
    every_placed_task_reports_exactly_once_band_00 => every_placed_task_reports_exactly_once, 0;
    every_placed_task_reports_exactly_once_band_01 => every_placed_task_reports_exactly_once, 1;
    every_placed_task_reports_exactly_once_band_02 => every_placed_task_reports_exactly_once, 2;
    every_placed_task_reports_exactly_once_band_03 => every_placed_task_reports_exactly_once, 3;
    every_placed_task_reports_exactly_once_band_04 => every_placed_task_reports_exactly_once, 4;
    every_placed_task_reports_exactly_once_band_05 => every_placed_task_reports_exactly_once, 5;
    every_placed_task_reports_exactly_once_band_06 => every_placed_task_reports_exactly_once, 6;
    every_placed_task_reports_exactly_once_band_07 => every_placed_task_reports_exactly_once, 7;
    every_placed_task_reports_exactly_once_band_08 => every_placed_task_reports_exactly_once, 8;
    every_placed_task_reports_exactly_once_band_09 => every_placed_task_reports_exactly_once, 9;
    every_placed_task_reports_exactly_once_band_10 => every_placed_task_reports_exactly_once, 10;
    every_placed_task_reports_exactly_once_band_11 => every_placed_task_reports_exactly_once, 11;
    every_placed_task_reports_exactly_once_band_12 => every_placed_task_reports_exactly_once, 12;
    every_placed_task_reports_exactly_once_band_13 => every_placed_task_reports_exactly_once, 13;
    every_placed_task_reports_exactly_once_band_14 => every_placed_task_reports_exactly_once, 14;
    every_placed_task_reports_exactly_once_band_15 => every_placed_task_reports_exactly_once, 15;
}
