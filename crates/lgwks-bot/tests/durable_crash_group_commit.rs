//! A real `SIGKILL` mid-batch: the reopen holds exactly the acknowledged prefix.
//!
//! #152's group commit changed *when* a durable answer is minted, so this file
//! exists to prove the change did not weaken the guarantee underneath it. Before
//! group commit one record was one flush, so a kill could only land between two
//! records. Now a batch's members share a flush, so a kill can land *inside* one —
//! after some of a batch's bytes are written and before the flush that answers any
//! of them. That is a strictly larger window, and it is the window this test
//! targets.
//!
//! The kill is a real `SIGKILL` of a real child process (this test binary
//! re-executing itself in probe mode, the pattern the journal family uses). The
//! store is a real [`RunStore`] over a real file with real `fsync`s.
//!
//! What makes the observation discriminating is the ordering the child writes: it
//! drives `Host::run` — the real front door, through `remember` and the storage
//! owner's batched flush — and only after a run has been *acknowledged* does it
//! append that run's id to a marker file. So:
//!
//! * every id in the marker was acknowledged, and must be on the disk after the
//!   kill;
//! * any record the reopen holds beyond the marker was never acknowledged, and a
//!   group commit that answered before its covering flush would leave exactly one
//!   there.
//!
//! The row is therefore an equality in both directions, not a lower bound.
//!
//! | Test | What it pins |
//! |---|---|
//! | `a_real_kill_mid_batch_holds_exactly_the_acknowledged_prefix` | the reopened store's records are exactly the acknowledged set — none lost, none invented |
//! | `a_killed_run_with_no_acknowledgment_leaves_no_record` | a child killed after opening its store and before its first run leaves a clean, reopenable store holding only its header |
//! | `the_group_commit_probe_child_is_killed_not_exited` | the observation is a kill: a probe that died on its own would make the row a courtesy |

#![cfg(all(feature = "script", feature = "ephemeral"))]

#[path = "support/journal.rs"]
mod measure;

#[path = "support/resume.rs"]
mod shared;

use std::error::Error;
use std::io::Write;
use std::path::{Path, PathBuf};

use lgwks_bot::effect::RunId;
use lgwks_bot::task::{Host, RunStore};

use measure::{ProbeGuard, TempGuard, pause, scratch_dir};
use shared::{Scratch, one_step_task};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Turns this binary into the probe child.
const PROBE_ENV: &str = "LGWKS_GROUP_COMMIT_PROBE";
/// The directory the run store is installed in.
const PROBE_STORE: &str = "LGWKS_PROBE_STORE";
/// The file the child appends each acknowledged run id to.
const PROBE_MARKER: &str = "LGWKS_PROBE_MARKER";
/// How many durable runs the child is ordered to commit.
const PROBE_RUNS: &str = "LGWKS_PROBE_RUNS";
/// Milliseconds the child holds, after its host is built and before its first run.
///
/// Set only by the no-acknowledgment row. Without it that row raced the child: on a
/// fast runner the child got from creating its store to its first acknowledgment
/// inside one of the parent's poll intervals, the "store exists and nothing is
/// acknowledged" moment was never observed, and the row failed on its bound.
const PROBE_HOLD_MS: &str = "LGWKS_PROBE_HOLD_MS";
/// The hold the no-acknowledgment row asks for. Longer than the parent's whole poll
/// bound, so the kill lands inside it however slow the runner is; it costs nothing,
/// because the parent kills the child the moment it announces the hold.
const NO_RECORD_HOLD_MS: u64 = 10_000;

/// The file a holding child creates once its host is built, beside the marker.
fn ready_path(marker: &Path) -> PathBuf {
    marker.with_extension("ready")
}

/// The row that kills the child after its first acknowledgment.
const TEST_MID_BATCH: &str = "a_real_kill_mid_batch_holds_exactly_the_acknowledged_prefix";
/// The row that kills the child before its first acknowledgment.
const TEST_NO_RECORD: &str = "a_killed_run_with_no_acknowledgment_leaves_no_record";

/// One acknowledged run's id, as the marker records it.
///
/// A text line rather than the run id's own type, because the marker is the
/// handoff between two processes and the file is the only thing they share. The
/// id's hex spelling is its canonical form everywhere else in this crate.
fn note_acknowledged(marker: &Path, run: RunId) -> Result<(), Box<dyn Error>> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(marker)?;
    writeln!(file, "{}", run.id().to_hex())?;
    // The marker's own durability matters as much as the store's: a marker byte in
    // a page cache the kill discarded would report an acknowledgment nobody made.
    file.sync_all()?;
    Ok(())
}

/// Every run id the marker names, in the order the child recorded them.
fn acknowledged(marker: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    if !marker.exists() {
        return Ok(Vec::new());
    }
    Ok(std::fs::read_to_string(marker)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect())
}

/// Drive `runs` durable runs through the real front door, noting each acknowledgment.
///
/// The body of the probe child. Every run is a separate `Host::run`, so every run
/// is a separate durable record on one store, and the acknowledgment is whatever
/// the front door reported — never a promise this function makes on its own.
fn probe_body() -> TestResult {
    let store = std::env::var_os(PROBE_STORE)
        .ok_or("the probe child was started without a store directory")?;
    let marker = PathBuf::from(
        std::env::var_os(PROBE_MARKER)
            .ok_or("the probe child was started without a marker path")?,
    );
    let runs: u32 = std::env::var(PROBE_RUNS)
        .map_err(|_| "the probe child was started without a run count")?
        .parse()
        .map_err(|_| "the probe child's run count was not a number")?;

    let host = Host::builder("probe")?.run_store(&store)?.build()?;
    if let Some(hold) = std::env::var_os(PROBE_HOLD_MS) {
        let hold: u64 = hold
            .to_str()
            .and_then(|millis| millis.parse().ok())
            .ok_or("the probe child's hold was not a number of milliseconds")?;
        // Announce the hold only once the store is open, so the parent's kill lands
        // after the store exists and before any record was appended to it.
        std::fs::write(ready_path(&marker), b"")?;
        pause(hold);
    }
    for index in 0..runs {
        let report = lgwks_bot::rt::runtime::block_on(host.run(&one_step_task()?, index));
        assert!(
            report.disposition().is_success(),
            "the probe child's run {index} failed: {:?}",
            report.error()
        );
        let run = report.run_id().ok_or("a stored run must name a run id")?;
        // Only now, and only after `sync_all`, is the run's id written down. The
        // parent's assertion "the reopen holds exactly this set" is therefore a
        // claim about what the *device* was told, not about what the child intended.
        note_acknowledged(&marker, run)?;
        // Give the parent a window to kill between records as well as inside a
        // batch: the kill's exact instant is the parent's, not the child's.
        pause(2);
    }

    // Park until the parent kills us. Bounded, so a parent that never kills cannot
    // leave a stray process behind.
    for _ in 0..600 {
        pause(100);
    }
    Err("the probe child parked for its whole bound and was never killed".into())
}

/// This test binary re-invoked as a named test with the probe's environment.
fn probe_command(test_name: &str) -> Result<std::process::Command, Box<dyn Error>> {
    let executable = std::env::current_exe()?;
    let mut command = std::process::Command::new(executable);
    command.args([test_name, "--exact", "--nocapture"]);
    Ok(command)
}

/// Spawn this test binary as a probe child ordered to commit `runs` durable runs.
fn spawn_probe(
    test_name: &str,
    store: &Path,
    marker: &Path,
    runs: u32,
    hold_ms: Option<u64>,
) -> Result<ProbeGuard, Box<dyn Error>> {
    let mut command = probe_command(test_name)?;
    command
        .env(PROBE_ENV, "1")
        .env(PROBE_STORE, store)
        .env(PROBE_MARKER, marker)
        .env(PROBE_RUNS, runs.to_string());
    if let Some(hold) = hold_ms {
        command.env(PROBE_HOLD_MS, hold.to_string());
    }
    Ok(ProbeGuard(Some(command.spawn()?)))
}

/// Run the probe body instead of the test when this process is the child.
fn dispatch_to_probe() -> Result<bool, Box<dyn Error>> {
    if std::env::var_os(PROBE_ENV).is_some() {
        probe_body()?;
        return Ok(true);
    }
    Ok(false)
}

/// The reopen after a kill holds exactly the acknowledged prefix.
///
/// Two directions, and both matter. Every id the marker names must be in the
/// reopened store — a lost record means an acknowledgment that was a lie. And the
/// reopened store must hold nothing beyond the marker — an invented record means an
/// answer minted before the flush that covers its bytes, which is the one way group
/// commit could have made things worse.
#[test]
fn a_real_kill_mid_batch_holds_exactly_the_acknowledged_prefix() -> TestResult {
    if dispatch_to_probe()? {
        return Ok(());
    }
    // Enough runs that the kill lands somewhere inside the history rather than at
    // its edge: a one-run child can only be killed before or after its single batch.
    let KillScenario {
        guard: _kept,
        store,
        marker,
        mut child,
    } = kill_scenario("gc-kill", TEST_MID_BATCH, 64, None)?;
    // The kill waits for the marker to carry *content*, not merely to exist: the
    // child opens it in append mode before its first line is flushed, so a file
    // that exists can still be empty — and killing on an empty marker would leave
    // the parent with no acknowledged run to compare a reopen against, which is the
    // comparison this row exists to make. The shared guard kills on existence, so
    // the wait is this file's own.
    kill_when(&mut child, TEST_MID_BATCH, || {
        Ok(!acknowledged(&marker)?.is_empty())
    })?;

    let expected = acknowledged(&marker)?;
    assert!(
        !expected.is_empty(),
        "the child recorded no acknowledged run, so there is no prefix to hold"
    );
    let expected: Vec<String> = expected.into_iter().map(|run| run.to_lowercase()).collect();

    let reopened = RunStore::open_in(&store, "probe")?;
    let mut held = Vec::new();
    for run in expected.iter() {
        let parsed = RunId::from_hex(run)?;
        if reopened.record_count(parsed) == 1 {
            held.push(run.clone());
        }
    }
    assert_eq!(
        held.len(),
        expected.len(),
        "the marker names {} acknowledged runs and a reopened store holds {} of them; \
         the lost ones were acknowledged before the kill",
        expected.len(),
        held.len()
    );

    // And nothing beyond them. Each acknowledged run must hold exactly one record,
    // so a run carrying two would be an unacknowledged frame the kill left readable.
    for run in &expected {
        let parsed = RunId::from_hex(run)?;
        assert_eq!(
            reopened.record_count(parsed),
            1,
            "run {run} was acknowledged once and the reopened store holds {} of its records; \
             an unacknowledged frame survived the kill",
            reopened.record_count(parsed)
        );
    }
    Ok(())
}

/// One kill scenario's setup: a scratch directory, the paths the child and the
/// parent share, and the probe child itself.
///
/// Both rows need exactly this, so it is built once: a row that spelled its own
/// setup would be the place the two drift into watching different files, and a kill
/// observation that watches a different marker than it wrote is not an observation.
struct KillScenario {
    /// Keeps the directory alive for as long as the child and the reopen need it.
    guard: TempGuard,
    /// The directory the run store is installed in.
    store: PathBuf,
    /// The file the child appends each acknowledged run id to.
    marker: PathBuf,
    /// The child, so the caller decides when it dies.
    child: ProbeGuard,
}

/// Prepare the scenario for `test_name`, over `runs` durable runs.
///
/// # Errors
///
/// Whatever the scratch directory or the spawn reports.
fn kill_scenario(
    tag: &str,
    test_name: &str,
    runs: u32,
    hold_ms: Option<u64>,
) -> Result<KillScenario, Box<dyn Error>> {
    let dir = scratch_dir(tag)?;
    let store = dir.join("store");
    let marker = dir.join("acknowledged");
    let child = spawn_probe(test_name, &store, &marker, runs, hold_ms)?;
    Ok(KillScenario {
        guard: TempGuard(dir),
        store,
        marker,
        child,
    })
}

/// A child killed before any run was acknowledged leaves a clean, reopenable store.
///
/// The negative control for the row above. A store that reopens cleanly is the
/// claim: bytes written without a covering flush may exist as a torn tail, and the
/// store must trim that without refusing the open or inventing a record. No
/// acknowledged record exists; the one record that may exist is the single
/// in-flight frame the device took before the kill.
#[test]
fn a_killed_run_with_no_acknowledgment_leaves_no_record() -> TestResult {
    if dispatch_to_probe()? {
        return Ok(());
    }
    // The child opens its store, announces a bounded hold and waits; the kill lands
    // inside that hold, so it has had no chance to acknowledge anything — the window
    // before its first flush, reached deterministically rather than by outracing it.
    let KillScenario {
        guard: _kept,
        store,
        marker,
        mut child,
    } = kill_scenario("gc-kill-empty", TEST_NO_RECORD, 64, Some(NO_RECORD_HOLD_MS))?;
    kill_when(&mut child, TEST_NO_RECORD, || {
        Ok(ready_path(&marker).exists() && acknowledged(&marker)?.is_empty())
    })?;

    assert!(
        acknowledged(&marker)?.is_empty(),
        "the child acknowledged a run before it was killed, so this row proves nothing"
    );
    // The kill landed inside the hold, after the store was opened and before the
    // first run began, so nothing was appended: the reopen holds exactly the header.
    // (A kill after a run's append returned but before its id reached the marker
    // would leave one unacknowledged record a reopen replays — at-least-once,
    // INV-BOT-130 — which is why the hold sits before the first run rather than
    // between a run and its acknowledgment.) A retained torn tail, any record or a
    // refused open is a failure, so the oracle is an equality rather than a bound.
    let reopened = RunStore::open_in(&store, "probe")?;
    let empty = header_len();
    assert_eq!(
        reopened.committed_bytes(),
        empty,
        "a store whose child was killed before its first run holds only its header \
         ({empty} bytes)"
    );
    Ok(())
}

/// Wait until `reached` says the child has got far enough, then `SIGKILL` it.
///
/// One kill for both rows here, because the two differ only in *which* moment they
/// choose to end the child's life — after its first acknowledgment, or before any —
/// and the part that actually kills and reaps is identical. `Child::kill` sends
/// `SIGKILL`: no cleanup, no destructors, no flushing, and the signal check below is
/// what distinguishes a kill from a child that happened to exit first.
///
/// The loop is bounded and reports a child that died on its own, so a probe that
/// failed before reaching its moment produces the row's own error rather than a
/// hang.
///
/// # Errors
///
/// When the child was already gone, when it exited before `reached` was true, or
/// when `reached` never became true within the bound. The last is a `reached` that
/// names something the child cannot reach, which is a broken fixture rather than a
/// timing flake.
fn kill_when(
    guard: &mut ProbeGuard,
    test_name: &str,
    reached: impl Fn() -> Result<bool, Box<dyn Error>>,
) -> Result<(), Box<dyn Error>> {
    let Some(mut child) = guard.take() else {
        return Err(format!("the probe child for {test_name} was gone before the kill").into());
    };
    for _ in 0..2_000 {
        if reached()? {
            return sigkill(&mut child, test_name);
        }
        if let Some(status) = child.try_wait()? {
            return Err(format!(
                "the probe child for {test_name} exited on its own before its moment: {status}"
            )
            .into());
        }
        pause(1);
    }
    drop(sigkill(&mut child, test_name));
    Err(format!("the probe child for {test_name} never reached the moment it was killed at").into())
}

/// `SIGKILL` `child` and reap it, asserting the signal rather than an exit.
///
/// # Errors
///
/// Whatever the kill or the reap reports.
fn sigkill(child: &mut std::process::Child, test_name: &str) -> Result<(), Box<dyn Error>> {
    child.kill()?;
    let status = child.wait()?;
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        assert_eq!(
            status.signal(),
            Some(9),
            "the probe for {test_name} must have been killed, not exited: {status}"
        );
    }
    Ok(())
}

/// How many bytes a freshly opened, never-written run store holds.
///
/// Measured from a real store rather than hard-coded, so the assertion above is
/// about "only the header" and not about a constant this file happens to believe.
/// A store that cannot be opened answers with the file's own length, which is the
/// same fact read one layer lower.
fn header_len() -> u64 {
    let Ok(scratch) = Scratch::new("gc-header") else {
        return 0;
    };
    match RunStore::open_in(&scratch.path().join("store"), "probe") {
        Ok(store) => store.committed_bytes(),
        Err(_) => 0,
    }
}
