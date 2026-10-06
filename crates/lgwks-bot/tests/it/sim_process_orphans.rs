//! Simulation family: a supervised child whose owner lets go is killed and reaped.
//!
//! The supervisor starts each child through the standard library and owns it as
//! an `OwnedChild`. A child whose driver finishes is reaped by that driver; a
//! child whose driver is dropped mid-run — the supervisor dropped, or its task
//! aborted — is killed and parked, and the next supervised child that starts or
//! is reaped collects it. Without that, the test process would hold every such
//! child as a zombie for the rest of its life: killed, but never released.
//!
//! Every scenario drives the real `Supervisor::spawn_process` with real `sh`
//! children. The seed chooses the shape — how many children, whether each forks a
//! grandchild into its group, and whether the supervisor is dropped or shut down
//! — and the property is the same for every shape: once released, no process of
//! any child's group is left, **zombies included**. `group_is_gone` reads the
//! process table, where an unreaped zombie is still a row.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `released_children_are_reaped` | a dropped or shut-down supervisor leaves no process — running or zombie — in any child's group |
//!
//! Every band runs twice through `sim::assert_replays`. The trace records the
//! drawn shape and the outcome, never how many follow-up runs the reap took, so
//! a loaded host changes the wall time and not the hash.

#![cfg(all(
    unix,
    feature = "rt",
    feature = "time",
    feature = "sync",
    feature = "process"
))]

use crate::sim;

// The band-declaration macro, defined once for the whole layer.
use crate::band_family;

// The pid-file wait and the process-table probes are shared with the other
// process test targets, so there is one copy of each.
use crate::process_probe;

use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lgwks_bot::Runtime;
use lgwks_bot::rt::process::ProcessSpec;
use lgwks_bot::rt::supervise::Supervisor;

use process_probe::{describe_group, group_is_gone, wait_for_pid};

use sim::Band;

type TestResult = Result<(), Box<dyn Error>>;

/// How long a child may take to record its pid before the scenario refuses.
const PID_BUDGET: Duration = Duration::from_secs(5);

/// How many short supervised runs a scenario drives while it waits for the
/// released groups to disappear.
///
/// A parked child is collected by the next supervised child that starts or is
/// reaped, so each follow-up run is one more chance; a SIGKILLed process is torn
/// down by the kernel within milliseconds, so this ceiling is reached only by a
/// child the reaper never collects — which is the defect this family exists to
/// catch.
const FOLLOW_UPS: u32 = 50;

/// How the scenario lets go of its supervisor.
#[derive(Clone, Copy, Debug)]
enum Release {
    /// Drop it with its children running: the drivers are aborted, so every
    /// child is killed and parked rather than reaped by its driver.
    Drop,
    /// Shut it down: the drivers observe cancellation, stop each group and
    /// reap their own children.
    Shutdown,
}

/// One seeded scenario's shape.
struct Plan {
    /// How many children run at once.
    children: u32,
    /// Whether each child forks a grandchild into its own group.
    grandchild: bool,
    /// How the supervisor is released.
    release: Release,
}

fn plan(rng: &mut sim::Rng) -> Plan {
    Plan {
        children: rng.between(1, 5),
        grandchild: rng.chance(500),
        release: if rng.chance(500) {
            Release::Drop
        } else {
            Release::Shutdown
        },
    }
}

/// A child that records its pid and then runs far longer than the scenario.
fn long_child(pid_file: &Path, grandchild: bool) -> ProcessSpec {
    let script = if grandchild {
        // The grandchild is forked before the pid is written, so the release
        // lands after the fork, never while one is in progress.
        format!("sleep 30 & echo $$ > {}; wait", pid_file.display())
    } else {
        format!("echo $$ > {}; exec sleep 30", pid_file.display())
    };
    let mut spec = ProcessSpec::new("sh");
    spec.arg("-c").arg(script);
    spec
}

/// A child that exits at once: one more supervised start and reap.
fn short_child() -> ProcessSpec {
    let mut spec = ProcessSpec::new("sh");
    spec.arg("-c").arg("exit 0");
    spec
}

/// Start the plan's children, wait for each to record its pid, then release
/// the supervisor as the plan says. Returns the children's pids, which are also
/// their process-group ids.
async fn start_and_release(plan: &Plan, pid_files: &[PathBuf]) -> Result<Vec<i32>, Box<dyn Error>> {
    let mut supervisor = Supervisor::new(usize::try_from(plan.children)?);
    for pid_file in pid_files {
        supervisor
            .spawn_process(&long_child(pid_file, plan.grandchild))
            .await?;
    }
    let mut pids = Vec::with_capacity(pid_files.len());
    for pid_file in pid_files {
        pids.push(wait_for_pid(pid_file, PID_BUDGET).ok_or("a child never recorded its pid")?);
    }
    match plan.release {
        Release::Drop => drop(supervisor),
        Release::Shutdown => {
            let report = supervisor.shutdown().await;
            assert_eq!(
                report.outcomes().len(),
                pid_files.len(),
                "shutdown reports one outcome per child"
            );
        }
    }
    Ok(pids)
}

fn released_children_are_reaped(band: Band) -> TestResult {
    let runtime = Runtime::new()?;
    sim::assert_replays(band, |sim| {
        let plan = plan(sim.rng());
        let dir = sim.scratch("process-orphans")?;
        let pid_files: Vec<PathBuf> = (0..plan.children)
            .map(|index| dir.join(format!("child-{index}.pid")))
            .collect();
        let pids = runtime.block_on(start_and_release(&plan, &pid_files))?;

        let mut follow_up = Supervisor::new(1);
        let mut reaped = pids.iter().all(|pid| group_is_gone(*pid));
        for _ in 0..FOLLOW_UPS {
            if reaped {
                break;
            }
            runtime.block_on(follow_up.run_process(&short_child()))?;
            reaped = pids.iter().all(|pid| group_is_gone(*pid));
        }
        assert!(
            reaped,
            "seed {}: {:?} released {} children and {FOLLOW_UPS} later supervised runs left \
             their groups behind: {}",
            sim.seed,
            plan.release,
            plan.children,
            pids.iter()
                .map(|pid| describe_group(*pid))
                .collect::<Vec<_>>()
                .join(" || ")
        );
        sim.record(&format!(
            "children={} grandchild={} release={:?} reaped={reaped}",
            plan.children, plan.grandchild, plan.release
        ));
        Ok(())
    })
}

band_family::band_family! {
    released_children_are_reaped_band_00 => released_children_are_reaped, 0;
    released_children_are_reaped_band_01 => released_children_are_reaped, 1;
    released_children_are_reaped_band_02 => released_children_are_reaped, 2;
    released_children_are_reaped_band_03 => released_children_are_reaped, 3;
    released_children_are_reaped_band_04 => released_children_are_reaped, 4;
    released_children_are_reaped_band_05 => released_children_are_reaped, 5;
    released_children_are_reaped_band_06 => released_children_are_reaped, 6;
    released_children_are_reaped_band_07 => released_children_are_reaped, 7;
    released_children_are_reaped_band_08 => released_children_are_reaped, 8;
    released_children_are_reaped_band_09 => released_children_are_reaped, 9;
    released_children_are_reaped_band_10 => released_children_are_reaped, 10;
    released_children_are_reaped_band_11 => released_children_are_reaped, 11;
    released_children_are_reaped_band_12 => released_children_are_reaped, 12;
    released_children_are_reaped_band_13 => released_children_are_reaped, 13;
    released_children_are_reaped_band_14 => released_children_are_reaped, 14;
    released_children_are_reaped_band_15 => released_children_are_reaped, 15;
    released_children_are_reaped_band_16 => released_children_are_reaped, 16;
    released_children_are_reaped_band_17 => released_children_are_reaped, 17;
    released_children_are_reaped_band_18 => released_children_are_reaped, 18;
    released_children_are_reaped_band_19 => released_children_are_reaped, 19;
    released_children_are_reaped_band_20 => released_children_are_reaped, 20;
    released_children_are_reaped_band_21 => released_children_are_reaped, 21;
    released_children_are_reaped_band_22 => released_children_are_reaped, 22;
    released_children_are_reaped_band_23 => released_children_are_reaped, 23;
    released_children_are_reaped_band_24 => released_children_are_reaped, 24;
    released_children_are_reaped_band_25 => released_children_are_reaped, 25;
    released_children_are_reaped_band_26 => released_children_are_reaped, 26;
    released_children_are_reaped_band_27 => released_children_are_reaped, 27;
    released_children_are_reaped_band_28 => released_children_are_reaped, 28;
    released_children_are_reaped_band_29 => released_children_are_reaped, 29;
    released_children_are_reaped_band_30 => released_children_are_reaped, 30;
    released_children_are_reaped_band_31 => released_children_are_reaped, 31;
}
