//! Seeded simulations of the Windows job-object path, over real kernel jobs.
//!
//! Only the draw is simulated. A job object is the kernel's own membership list,
//! which no stand-in can fake, so every run starts real `cmd.exe` trees and stops
//! them through the supervisor's real Windows path. One seed picks the shape of
//! the run (how many background processes, how the tree ends, what else is
//! running beside it), and the same seed must make the same run and say the same
//! thing about it.
//!
//! The counts (background processes, trees) come from the seeded stream. The
//! two- and three-way choices (the exit code, how a supervisor lets go, how a
//! tree ends) are the seed's residue, because the first six words of the stream
//! drew one branch of the two-way choices for every seed, and a sweep that never
//! reaches a branch proves nothing about it.
//!
//! The trace of a run is a BLAKE3 digest over what the seed drew and what the
//! receipt claimed, never over a pid, a path, a time or a member count the host
//! can raise. Each family runs every seed twice and requires the two digests to
//! be equal, and requires the seeds to differ from one another, so a trace that
//! recorded nothing cannot pass. A failure names its family and its seed.
//!
//! - T19: a shell that exits by itself while its descendants run still has its
//!   job settled, and the receipt says so for every exit code.
//! - T20: a stop requested before the manager task has run, by shutdown or by
//!   dropping the supervisor, still leaves no descendant; so does one shutdown
//!   over several trees at once.
//! - T21: stopping one tree reaches neither another supervisor's tree nor a
//!   process the test started outside every job.
//!
//! The method is the one deterministic-simulation testing is named for: a seed
//! drives the choices and the run is replayed from it (FoundationDB, "Testing
//! Distributed Systems w/ Deterministic Simulation").

use std::collections::BTreeSet;
use std::error::Error;

use lgwks_std::hash::Hasher;
use lgwks_std::seeded::Seeded;

use lgwks_bot::rt::supervise::Supervisor;

use super::scratch::Scratch;
use super::tree;
use super::{TestResult, cmd, marker, outcome};

/// How many seeds each family sweeps; each is run twice.
const SEEDS: u64 = 6;

/// The most background pings one tree holds.
const MAX_HELD: usize = 3;

/// The most trees one supervisor is given at once.
const MAX_TREES: usize = 4;

/// Feed one observation into a trace: its tag, then its text, each framed so that
/// two splits of the same bytes hash apart.
fn observe(hasher: &mut Hasher, tag: &str, text: &str) {
    hasher.write_framed(tag.as_bytes());
    hasher.write_framed(text.as_bytes());
}

/// Sweep [`SEEDS`] seeds of `family`, each twice, and require the two traces of
/// every seed to be equal and the seeds to have drawn more than one trace.
fn sweep(family: &str, mut run: impl FnMut(u64) -> Result<String, Box<dyn Error>>) -> TestResult {
    let mut traces = BTreeSet::new();
    for seed in 0..SEEDS {
        let first = run(seed).map_err(|error| format!("{family}: seed {seed} failed: {error}"))?;
        let again = run(seed)
            .map_err(|error| format!("{family}: seed {seed} failed on its replay: {error}"))?;
        assert_eq!(
            first, again,
            "{family}: seed {seed} did not replay to the same trace"
        );
        traces.insert(first);
    }
    assert!(
        traces.len() > 1,
        "{family}: every seed drew the same run, so the sweep covered one case"
    );
    Ok(())
}

/// The number of background pings a seed draws, from one to [`MAX_HELD`].
fn draw_held(rng: &mut Seeded) -> Result<usize, Box<dyn Error>> {
    Ok(rng.index(MAX_HELD)?.saturating_add(1))
}

/// T19: a shell that exits by itself, with a drawn code, while its drawn number
/// of descendants run.
fn natural_exit(seed: u64) -> Result<String, Box<dyn Error>> {
    let mut rng = Seeded::from_seed(seed);
    let held = draw_held(&mut rng)?;
    let code: u8 = if seed.rem_euclid(2) == 0 { 0 } else { 7 };
    let dir = Scratch::new("sim-exit")?;
    let spec = cmd(
        &tree::script(held, &tree::exit_after_a_second(code)),
        dir.path(),
    );
    let (stopped, released) = lgwks_bot::block_on(async {
        let mut supervisor = Supervisor::default();
        supervisor.spawn_process(&spec).await?;
        let stopped = outcome(&mut supervisor).await?;
        let released = tree::all_released(dir.path(), held).await;
        Ok::<_, Box<dyn Error>>((stopped, released))
    })?;
    tree::exited_with(&stopped, code)?;
    let receipt = tree::settled(&stopped, held)?;
    tree::require(
        released,
        "a descendant still held its file after the job settled",
    )?;
    let mut hasher = Hasher::new();
    observe(&mut hasher, "shape", &format!("held={held} exit={code}"));
    observe(&mut hasher, "receipt", &receipt);
    Ok(hasher.finalize().to_hex())
}

#[test]
fn sim_a_shell_that_exits_alone_leaves_no_descendant_t19() -> TestResult {
    sweep("natural exit", natural_exit)
}

/// How the supervisor lets go of a tree whose manager task has not yet run.
#[derive(Clone, Copy)]
enum Release {
    /// Shut the supervisor down and read what it drained.
    Shutdown,
    /// Drop the supervisor with its tree running.
    Drop,
}

/// T20: a stop requested after the tree is up and before the manager task that
/// owns it has run.
fn before_the_manager_runs(seed: u64) -> Result<String, Box<dyn Error>> {
    let mut rng = Seeded::from_seed(seed);
    let held = draw_held(&mut rng)?;
    let release = if seed.rem_euclid(2) == 0 {
        Release::Shutdown
    } else {
        Release::Drop
    };
    let dir = Scratch::new("sim-pre-poll")?;
    let spec = cmd(&tree::script(held, tree::STAY), dir.path());
    let (receipt, released) = lgwks_bot::block_on(async {
        let mut supervisor = Supervisor::default();
        supervisor.spawn_process(&spec).await?;
        tree::up_without_yielding(dir.path())?;
        tree::require(
            tree::all_held(dir.path(), held),
            "the background pings hold their files before the stop",
        )?;
        let receipt = match release {
            Release::Shutdown => tree::stopped_tree(&supervisor.shutdown().await, held)?,
            Release::Drop => {
                drop(supervisor);
                String::from("dropped")
            }
        };
        let released = tree::all_released(dir.path(), held).await;
        Ok::<_, Box<dyn Error>>((receipt, released))
    })?;
    tree::require(released, "a descendant still held its file after the stop")?;
    let mut hasher = Hasher::new();
    let way = match release {
        Release::Shutdown => "shutdown",
        Release::Drop => "drop",
    };
    observe(&mut hasher, "shape", &format!("held={held} release={way}"));
    observe(&mut hasher, "receipt", &receipt);
    Ok(hasher.finalize().to_hex())
}

#[test]
fn sim_a_stop_before_the_manager_task_runs_leaves_no_descendant_t20() -> TestResult {
    sweep("before the manager runs", before_the_manager_runs)
}

/// T20, wide: one supervisor holding a drawn number of trees, shut down once.
fn many_trees(seed: u64) -> Result<String, Box<dyn Error>> {
    let mut rng = Seeded::from_seed(seed);
    let trees = rng.index(MAX_TREES)?.saturating_add(1);
    let held = (0..trees)
        .map(|_| draw_held(&mut rng))
        .collect::<Result<Vec<_>, _>>()?;
    let dirs = (0..trees)
        .map(|_| Scratch::new("sim-many"))
        .collect::<Result<Vec<_>, _>>()?;
    let specs = dirs
        .iter()
        .zip(&held)
        .map(|(dir, count)| cmd(&tree::script(*count, tree::STAY), dir.path()))
        .collect::<Vec<_>>();
    let (receipts, released) = lgwks_bot::block_on(async {
        let mut supervisor = Supervisor::default();
        for spec in &specs {
            supervisor.spawn_process(spec).await?;
        }
        for dir in &dirs {
            tree::up_without_yielding(dir.path())?;
        }
        let report = supervisor.shutdown().await;
        tree::require(
            report.outcomes().len() == trees,
            "every tree must drain as an outcome of its own",
        )?;
        let receipts = report
            .outcomes()
            .iter()
            .map(|drained| tree::settled(drained, 2))
            .collect::<Result<BTreeSet<_>, _>>()?;
        let mut released = true;
        for (dir, count) in dirs.iter().zip(&held) {
            released &= tree::all_released(dir.path(), *count).await;
        }
        Ok::<_, Box<dyn Error>>((receipts, released))
    })?;
    tree::require(
        released,
        "a descendant still held its file after the shutdown",
    )?;
    let mut hasher = Hasher::new();
    observe(
        &mut hasher,
        "shape",
        &format!("trees={trees} held={held:?}"),
    );
    observe(&mut hasher, "receipts", &format!("{receipts:?}"));
    Ok(hasher.finalize().to_hex())
}

#[test]
fn sim_trees_stopped_together_each_settle_their_own_job_t20() -> TestResult {
    sweep("trees stopped together", many_trees)
}

/// T21: tree A ends in a drawn way while tree B, under another supervisor, and a
/// process outside every job keep running; then B is stopped and the outsider
/// still runs.
fn unrelated(seed: u64) -> Result<String, Box<dyn Error>> {
    let mut rng = Seeded::from_seed(seed);
    let held_a = draw_held(&mut rng)?;
    let held_b = draw_held(&mut rng)?;
    let end = seed.rem_euclid(3);
    let dir_a = Scratch::new("sim-unrelated-a")?;
    let dir_b = Scratch::new("sim-unrelated-b")?;
    let mut outsider = tree::Outsider::start()?;
    let tail_a = if end == 2 {
        tree::exit_after_a_second(0)
    } else {
        String::from(tree::STAY)
    };
    let spec_a = cmd(&tree::script(held_a, &tail_a), dir_a.path());
    let spec_b = cmd(&tree::script(held_b, tree::STAY), dir_b.path());
    let receipts = lgwks_bot::block_on(async {
        let mut tree_a = Supervisor::default();
        let mut tree_b = Supervisor::default();
        tree_a.spawn_process(&spec_a).await?;
        tree_b.spawn_process(&spec_b).await?;
        marker(dir_a.path(), tree::UP).await?;
        marker(dir_b.path(), tree::UP).await?;
        let receipt_a = match end {
            0 => {
                tree_a.cancel();
                tree::settled(&outcome(&mut tree_a).await?, held_a.saturating_add(1))?
            }
            1 => tree::stopped_tree(&tree_a.shutdown().await, held_a)?,
            _ => {
                let exited = outcome(&mut tree_a).await?;
                tree::exited_with(&exited, 0)?;
                tree::settled(&exited, held_a)?
            }
        };
        tree::require(
            tree::all_released(dir_a.path(), held_a).await,
            "tree A still held a file after its stop",
        )?;
        tree::require(
            tree::all_held(dir_b.path(), held_b),
            "stopping tree A reached tree B",
        )?;
        tree::require(
            outsider.is_running(),
            "stopping tree A reached a process outside every job",
        )?;
        tree_b.cancel();
        let receipt_b = tree::settled(&outcome(&mut tree_b).await?, held_b.saturating_add(1))?;
        tree::require(
            tree::all_released(dir_b.path(), held_b).await,
            "tree B still held a file after its stop",
        )?;
        tree::require(
            outsider.is_running(),
            "stopping tree B reached a process outside every job",
        )?;
        Ok::<_, Box<dyn Error>>(format!("{receipt_a} {receipt_b}"))
    })?;
    let mut hasher = Hasher::new();
    observe(
        &mut hasher,
        "shape",
        &format!("a={held_a} b={held_b} end={end}"),
    );
    observe(&mut hasher, "receipts", &receipts);
    Ok(hasher.finalize().to_hex())
}

#[test]
fn sim_a_stop_never_reaches_an_unrelated_tree_or_process_t21() -> TestResult {
    sweep("unrelated survives", unrelated)
}
