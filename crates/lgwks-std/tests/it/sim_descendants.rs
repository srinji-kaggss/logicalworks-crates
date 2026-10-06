//! Seeded simulation of the `process` feature's descendant capture (#263).
//!
//! A group kill reaches the members that exist when it is sent. A descendant
//! that called `setsid` is in a new session by the time the signal lands, so the
//! only way to stop one is to name its pid — which is what
//! [`capture_descendants`] does and what [`running_processes`] then checks. Both
//! halves have edges a real cleanup depends on: the capture is a snapshot of the
//! *parent relation*, so a descendant that forked but has not yet been recorded
//! is a race rather than an absence; and the observation distinguishes a running
//! process from an unreaped zombie, so a child that has ended is not reported as
//! a survivor.
//!
//! One seed draws every tree here — its depth, how many children each level
//! forks, which fork leaves the group, and which of capture, signal and observe
//! happens next — and the model written from the documented contract says what
//! each call must answer. Every answer the OS gives is checked against it, and
//! only the model's answers are folded into the trace, so the same seed replays
//! the same trace although process timing and pid assignment are the OS's.
//!
//! The processes are real, not a mock: the properties under test are a fork's
//! parentage, a `setsid` leaving the group, and a killed child waiting to be
//! reaped, and none of the three is observable through a stub.

#![cfg(all(unix, feature = "process"))]

use std::error::Error;
use std::io::ErrorKind;
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use lgwks_std::process::{
    MAX_CAPTURED_DESCENDANTS, capture_descendants, kill_process, kill_process_group,
    process_exists, running_processes,
};

use crate::rng::Rng;
use crate::seeded_sweep::{SWEEP_SEEDS, fold, fold_usize, initial_trace};

/// What every test here returns: a fixture that cannot be built fails the test
/// with its cause instead of panicking.
type TestResult = Result<(), Box<dyn Error>>;

/// The deepest tree a seed may draw.
const MAX_DEPTH: usize = 3;

/// Most children one level may fork.
const MAX_BREADTH: usize = 2;

/// Scheduled operations per seeded sweep.
const STEPS: usize = 12;

/// How long a killed descendant may take to stop running.
///
/// The wait is for *running* to end, not for the id to disappear: a child killed
/// while its parent is alive becomes an unreaped zombie first, and reaping it is
/// the platform's work rather than this test's.
const SETTLE: Duration = Duration::from_secs(10);

/// How long a freshly forked child may take to record its own pid.
const RECORD: Duration = Duration::from_secs(10);

/// The id no supported OS hands out, above every pid ceiling.
const VACANT: i32 = i32::MAX;

/// The tree shape one seed drew: how many children each level forks, and which
/// one of them leaves the group.
///
/// Drawn before anything is started, so the model of what the tree will contain
/// is known before the tree exists — the capture is then checked against what the
/// seed asked for rather than against whatever the OS happened to fork.
#[derive(Clone, Debug)]
struct Shape {
    /// Children per level, outermost first; the length is the depth.
    breadths: Vec<usize>,
    /// The `(level, child)` that calls `setsid`, when the seed drew one.
    escape: Option<(usize, usize)>,
}

impl Shape {
    /// Draw the shape `seed` names.
    fn draw(seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let depth = rng.below(MAX_DEPTH).saturating_add(1);
        let breadths: Vec<usize> = (0..depth)
            .map(|_| rng.below(MAX_BREADTH).saturating_add(1))
            .collect();
        let escape = if rng.below(2) == 0 {
            let level = rng.below(depth);
            let child = rng.below(breadths[level]);
            Some((level, child))
        } else {
            None
        };
        Self { breadths, escape }
    }

    /// How many forks the shape contains.
    fn forks(&self) -> usize {
        self.breadths.iter().sum()
    }

    /// Whether the shape draws an escapee.
    fn escapes(&self) -> bool {
        self.escape.is_some()
    }
}

/// A tree the seed built: a leader and every pid below it.
struct Tree {
    /// The leader, unreaped: its pid pins the tree for the whole test.
    leader: Child,
    /// The leader's pid, which is also its group id.
    root: i32,
    /// The shape the seed drew.
    shape: Shape,
    /// Every pid the tree forked, sorted.
    pids: Vec<i32>,
    /// The directory holding the pid files.
    dir: std::path::PathBuf,
}

impl Tree {
    /// Grow the tree `seed` draws and wait until every fork has recorded its pid.
    fn grow(seed: u64) -> Result<Self, Box<dyn Error>> {
        let shape = Shape::draw(seed);
        let dir =
            std::env::temp_dir().join(format!("lgwks-std-desc-{}-{seed:#x}", std::process::id()));
        let _cleared = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let mut script = String::new();
        for (level, breadth) in shape.breadths.iter().enumerate() {
            for child in 0..*breadth {
                let file = dir.join(format!("{level}-{child}.pid"));
                let source = if shape.escape == Some((level, child)) {
                    escape_source(&file)?
                } else {
                    hold_source(&file)
                };
                script.push_str(&source);
            }
        }
        script.push_str("exec sleep 30");
        let leader = Command::new("sh")
            .arg("-c")
            .arg(&script)
            // Its own group, or the drop-time group kill would name a group this
            // tree does not lead and every sleeper would outlive the test.
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let root = i32::try_from(leader.id())?;
        let mut pids = Vec::with_capacity(shape.forks());
        for (level, breadth) in shape.breadths.iter().enumerate() {
            for child in 0..*breadth {
                pids.push(recorded_pid(&dir.join(format!("{level}-{child}.pid")))?);
            }
        }
        pids.sort_unstable();
        Ok(Self {
            leader,
            root,
            shape,
            pids,
            dir,
        })
    }
}

impl Drop for Tree {
    /// Leave nothing running: the group kill takes everything still in the group,
    /// and every recorded pid is taken by name for whatever left it.
    fn drop(&mut self) {
        for pid in &self.pids {
            let _killed = kill_process(*pid);
        }
        let _killed = kill_process_group(self.root);
        let _reaped = self.leader.wait();
        let _removed = std::fs::remove_dir_all(&self.dir);
    }
}

/// A backgrounded child that records its own pid and then holds the group open.
fn hold_source(pid_file: &std::path::Path) -> String {
    // `exec` and not a second process: the recorded pid is then the only process
    // this fork contributes, so the model is comparable to the capture.
    format!("sh -c 'echo $$ > {}; exec sleep 30' & ", pid_file.display())
}

/// A backgrounded child that leaves the group before it records its pid.
///
/// The ordering is the whole point and is why the recording lives inside the
/// escaped program rather than being appended by the caller: a pid recorded
/// *before* `setsid` names a process that is still a group member, and a model
/// that believed it had escaped would be measuring the ordinary group kill.
///
/// The backgrounded fork is what makes the escape possible at all: `setsid`
/// fails with `EPERM` for a process-group leader, and the leader's own direct
/// child is not one.
fn escape_source(pid_file: &std::path::Path) -> Result<String, Box<dyn Error>> {
    Ok(format!(
        "python3 -c 'import os,time; os.setsid(); open(\"{}\",\"w\").write(str(os.getpid())); time.sleep(30)' & ",
        pid_file.display()
    ))
}

/// The pid `file` records, within [`RECORD`].
fn recorded_pid(file: &std::path::Path) -> Result<i32, Box<dyn Error>> {
    let deadline = Instant::now()
        .checked_add(RECORD)
        .ok_or("the record deadline overflows the clock")?;
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(file)
            && let Ok(pid) = text.trim().parse::<i32>()
        {
            return Ok(pid);
        }
        std::thread::park_timeout(Duration::from_millis(5));
    }
    Err(format!("a fork never recorded its pid in {}", file.display()).into())
}

/// Whether every pid in `pids` has stopped running, within [`SETTLE`].
fn settle_stopped(pids: &[i32]) -> Result<bool, Box<dyn Error>> {
    let deadline = Instant::now()
        .checked_add(SETTLE)
        .ok_or("the settle deadline overflows the clock")?;
    while Instant::now() < deadline {
        if running_processes(pids)?.is_empty() {
            return Ok(true);
        }
        std::thread::park_timeout(Duration::from_millis(5));
    }
    Ok(running_processes(pids)?.is_empty())
}

/// Whether every pid in `pids` is still running, within [`SETTLE`].
fn settle_running(pids: &[i32]) -> Result<bool, Box<dyn Error>> {
    let deadline = Instant::now()
        .checked_add(SETTLE)
        .ok_or("the settle deadline overflows the clock")?;
    while Instant::now() < deadline {
        if running_processes(pids)?.len() == pids.len() {
            return Ok(true);
        }
        std::thread::park_timeout(Duration::from_millis(5));
    }
    Ok(running_processes(pids)?.len() == pids.len())
}

/// One seeded sweep of capture, signal and observe over one tree, as its trace.
///
/// The model is the tree the seed drew: a capture must return exactly the pids
/// the tree recorded, a signal must stop exactly the pid it named, and an
/// observation must report every live pid as running and nothing stopped as such.
fn sweep(seed: u64) -> Result<u64, Box<dyn Error>> {
    let mut rng = Rng::new(seed);
    let mut trace = initial_trace();
    let tree = Tree::grow(seed)?;
    let all = tree.pids.clone();
    // The tree's shape, not its pids: pids are the OS's to assign, and a trace
    // that folded them could not replay on a second run of the same seed.
    fold_usize(&mut trace, all.len());
    fold_usize(&mut trace, tree.shape.breadths.len());
    fold(&mut trace, u64::from(tree.shape.escapes()));
    let mut stopped: Vec<i32> = Vec::new();
    for step in 0..STEPS {
        let at = format!("seed {seed:#x} step {step}");
        let live: Vec<i32> = all
            .iter()
            .copied()
            .filter(|pid| !stopped.contains(pid))
            .collect();
        // A schedule that only ever captured would never exercise the signal
        // half, and one that only signalled would never exercise the capture.
        let operation = if live.is_empty() { 0 } else { rng.below(3) };
        fold(&mut trace, u64::try_from(operation)?);
        match operation {
            0 => {
                let captured = capture_descendants(tree.root)?;
                fold(&mut trace, u64::from(captured.is_truncated()));
                // Containment, not equality: the capture walks the *parent
                // relation*, and a child this sweep already stopped is still a
                // child until the kernel reaps it. Which of the captured pids are
                // running is the observation's question, and folding the
                // captured count here would make the trace depend on when the
                // platform got round to a reap.
                for pid in &live {
                    assert!(
                        captured.contains(*pid),
                        "{at}: the capture must hold live pid {pid}, got {:?}",
                        captured.pids()
                    );
                }
                for pid in captured.pids() {
                    assert!(
                        all.contains(pid),
                        "{at}: the capture reached pid {pid}, which is not in the tree the \
                         seed built: a supervisor may only signal what it started"
                    );
                }
                assert!(
                    captured.mechanism().read_a_table(),
                    "{at}: a real capture must name a mechanism that read a table, got {:?}",
                    captured.mechanism()
                );
                assert!(
                    captured.pids().len() <= MAX_CAPTURED_DESCENDANTS,
                    "{at}: a capture is bounded by MAX_CAPTURED_DESCENDANTS, got {}",
                    captured.pids().len()
                );
                assert!(
                    !captured.contains(tree.root),
                    "{at}: the root leads the group and is never in its own capture"
                );
            }
            1 => {
                let slot = rng.below(live.len());
                let pid = live
                    .get(slot)
                    .copied()
                    .ok_or_else(|| format!("{at}: slot {slot} is out of range"))?;
                fold_usize(&mut trace, slot);
                kill_process(pid)?;
                assert!(
                    settle_stopped(&[pid])?,
                    "{at}: pid {pid} was signalled and must have stopped running"
                );
                stopped.push(pid);
                fold(&mut trace, 1);
            }
            _ => {
                let running = running_processes(&live)?;
                assert_eq!(
                    running.iter().copied().collect::<Vec<i32>>(),
                    live.clone(),
                    "{at}: every live pid the model holds must be reported running"
                );
                fold(&mut trace, 2);
            }
        }
        fold_usize(&mut trace, stopped.len());
    }
    // Whatever is still running at the end is stopped and checked, so a failing
    // sweep leaves no process behind for the next one to inherit.
    let tail: Vec<i32> = all
        .iter()
        .copied()
        .filter(|pid| !stopped.contains(pid))
        .collect();
    for pid in &tail {
        let _killed = kill_process(*pid);
    }
    assert!(
        settle_stopped(&tail)?,
        "seed {seed:#x}: every descendant must be stoppable by pid; {tail:?} still run"
    );
    fold_usize(&mut trace, tail.len());
    Ok(trace)
}

#[test]
/// Every seeded sweep of capture, signal and observe agrees with the model at
/// every step.
fn a_seeded_sweep_agrees_with_the_model_at_every_step() -> TestResult {
    for seed in SWEEP_SEEDS {
        sweep(seed)?;
    }
    Ok(())
}

#[test]
/// The replay oracle: one seed, one sweep trace.
fn the_same_seed_replays_the_same_sweep_trace() -> TestResult {
    for seed in SWEEP_SEEDS {
        assert_eq!(
            sweep(seed)?,
            sweep(seed)?,
            "seed {seed:#x}: the same seed must replay the same sweep"
        );
    }
    Ok(())
}

#[test]
/// The divergence oracle: a sweep that ignored its seed would pass the replay.
fn distinct_seeds_drive_distinct_sweeps() -> TestResult {
    let [first, second, ..] = SWEEP_SEEDS;
    assert_ne!(
        sweep(first)?,
        sweep(second)?,
        "seeds {first:#x} and {second:#x} must schedule different sweeps"
    );
    Ok(())
}

#[test]
/// Whatever the seed drew, a descendant is captured and stopped by pid — which is
/// the only way to stop one that has left the group.
fn a_seeded_descendant_is_captured_and_stopped_whatever_the_seed_draws() -> TestResult {
    for seed in SWEEP_SEEDS {
        let tree = Tree::grow(seed)?;
        let Some(first) = tree.pids.first().copied() else {
            return Err(format!("seed {seed:#x}: the tree forked nothing").into());
        };
        let captured = capture_descendants(tree.root)?;
        assert!(
            captured.contains(first),
            "seed {seed:#x}: the capture must hold pid {first} wherever the seed put it"
        );
        assert!(
            !captured.contains(tree.root),
            "seed {seed:#x}: the root leads the group and is never in its own capture"
        );
        kill_process(first)?;
        assert!(
            settle_stopped(&[first])?,
            "seed {seed:#x}: a per-process signal must stop pid {first}, which no group \
             signal could reach once it had left the group"
        );
    }
    Ok(())
}

#[test]
/// Two trees on one host stay separate: neither capture reaches the other's
/// forks, and stopping one leaves the other running.
fn two_trees_never_capture_each_other() -> TestResult {
    for seed in SWEEP_SEEDS {
        let first = Tree::grow(seed)?;
        let second = Tree::grow(seed.wrapping_add(1))?;
        let (first_pids, second_pids) = (first.pids.clone(), second.pids.clone());
        let first_capture = capture_descendants(first.root)?;
        let second_capture = capture_descendants(second.root)?;
        for pid in &second_pids {
            assert!(
                !first_capture.contains(*pid),
                "seed {seed:#x}: the first capture reached the second tree's pid {pid}"
            );
        }
        for pid in &first_pids {
            assert!(
                !second_capture.contains(*pid),
                "seed {seed:#x}: the second capture reached the first tree's pid {pid}"
            );
        }
        let victim = first_pids
            .first()
            .copied()
            .ok_or("the first tree forked nothing to signal")?;
        kill_process(victim)?;
        assert!(
            settle_stopped(&[victim])?,
            "seed {seed:#x}: the signalled pid {victim} must have stopped running"
        );
        assert!(
            settle_running(&second_pids)?,
            "seed {seed:#x}: the second tree must be untouched by the first tree's signals"
        );
    }
    Ok(())
}

#[test]
/// A root that names no process captures nothing, and the mechanism says the
/// table was read — an empty capture must never read as "there were no children".
fn a_vacant_root_captures_nothing_and_names_its_mechanism() -> TestResult {
    for seed in SWEEP_SEEDS {
        let captured = capture_descendants(VACANT)?;
        assert!(
            captured.is_empty(),
            "seed {seed:#x}: an id above every pid ceiling descends from nothing"
        );
        assert_eq!(
            captured.len(),
            0,
            "seed {seed:#x}: an empty capture has no ids to report"
        );
        assert!(
            captured.mechanism().read_a_table(),
            "seed {seed:#x}: the table was read, so the mechanism must say so, got {:?}",
            captured.mechanism()
        );
        assert!(
            !captured.is_truncated(),
            "seed {seed:#x}: an empty tree is inside every bound"
        );
        assert!(
            !process_exists(VACANT)?,
            "seed {seed:#x}: an id above every pid ceiling names no process"
        );
        assert_eq!(
            capture_descendants(0).map_err(|error| error.kind()),
            Err(ErrorKind::InvalidInput),
            "seed {seed:#x}: root 0 names the caller, so no tree is walked"
        );
    }
    Ok(())
}
