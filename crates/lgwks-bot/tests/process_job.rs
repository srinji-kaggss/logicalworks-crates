//! #263 on Windows: the supervisor owns a process tree through a job object.
//!
//! The Linux fire test (`tests/it/process_fire.rs`) reads the operating system's
//! process table after the stop. A job object makes a stronger statement than
//! any table read: the kernel keeps the membership, so the proof is the job
//! reading empty after its terminate. These tests drive the supervisor's real
//! Windows path and assert on that proof, which the receipt carries.
//!
//! - A `cmd.exe` that starts a grandchild (`start /b ping`) and then runs a
//!   second `ping` of its own is cancelled once it has said both are up: the
//!   receipt is `CleanupConfirmed`, the mechanism is `JobObject`, the job held
//!   the shell and its grandchild, and no survivor is named.
//! - A deadline stops a tree the same way and the run reports the deadline.
//! - A spec that captures a stream is refused before anything starts.
//!
//! The acceptance rows T19, T20 and T21 (`docs/acceptance/t-rows-windows.toml`)
//! are answered here for the part a job object answers:
//!
//! - T19: a shell that exits by itself, with zero or with seven, while a
//!   descendant runs, is not reported as a clean tree. The job is settled and
//!   its receipt says what it read.
//! - T20: a stop requested before the manager task has run stops the tree.
//! - T21: a stop reaches no process outside the job. No escape from the job is
//!   driven, because a child can leave a job only through a breakaway the
//!   supervisor's jobs do not grant and this suite cannot attempt without
//!   `unsafe`, so the row stays `present` (`docs/orchestration-acceptance.spec.md`).
//!
//! Each of those has a named test and a seeded simulation (`tests/process_job/
//! sims.rs`) over real jobs. `tests/process_job/tree.rs` holds the trees they
//! start and the second witness they read, a file a descendant holds open.
//!
//! This is a binary of its own, apart from the crate's `it` binary, so the
//! `windows-latest` job that executes it compiles this file and the library it
//! drives and not the other four thousand tests (the `Process containment on
//! Windows` job of `.github/workflows/ci.yml`). On any other host the file is
//! empty.

#![cfg(all(
    windows,
    feature = "rt",
    feature = "time",
    feature = "sync",
    feature = "process"
))]

#[path = "support/scratch.rs"]
mod scratch;
#[path = "process_job/sims.rs"]
mod sims;
#[path = "process_job/tree.rs"]
mod tree;

use scratch::Scratch;

use std::num::NonZeroUsize;
use std::time::Duration;

use lgwks_bot::Runtime;
use lgwks_bot::rt::process::{ProcessRunError, ProcessSpec};
use lgwks_bot::rt::supervise::{CleanupReceipt, ContainmentMechanism, Supervisor, TaskOutcome};
use lgwks_bot::rt::time::{Instant, sleep};

/// What a test reports when a precondition did not hold.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// How long a wait for the child's own marker, or for its report, may take.
const BUDGET: Duration = Duration::from_secs(30);

/// A `cmd.exe` spec running `script`, from `dir` so a marker needs no quoting.
fn cmd(script: &str, dir: &std::path::Path) -> ProcessSpec {
    let mut spec = ProcessSpec::new("cmd");
    spec.arg("/c").arg(script).current_dir(dir);
    spec
}

/// Wait for the file `name` under `dir` to exist.
async fn marker(dir: &std::path::Path, name: &str) -> TestResult {
    let started = Instant::now();
    while started.elapsed() < BUDGET {
        if dir.join(name).exists() {
            return Ok(());
        }
        sleep(Duration::from_millis(10)).await;
    }
    Err(format!("the child never wrote {name}").into())
}

/// The task's terminal outcome, reaped from `supervisor`.
async fn outcome(supervisor: &mut Supervisor) -> Result<TaskOutcome, Box<dyn std::error::Error>> {
    let started = Instant::now();
    loop {
        supervisor.reap();
        if let Some(outcome) = supervisor.next_report() {
            return Ok(outcome);
        }
        if started.elapsed() >= BUDGET {
            break Err("the supervisor never reported the stopped job".into());
        }
        sleep(Duration::from_millis(5)).await;
    }
}

#[test]
fn a_cancelled_job_holds_a_grandchild_and_settles_empty() -> TestResult {
    let dir = Scratch::new("job-cancel")?;
    let script =
        "start /b ping -n 600 127.0.0.1 >nul & echo up> up.txt & ping -n 600 127.0.0.1 >nul";
    let runtime = Runtime::new()?;
    runtime.block_on(async {
        let mut supervisor = Supervisor::default();
        supervisor.spawn_process(&cmd(script, dir.path())).await?;
        marker(dir.path(), "up.txt").await?;
        supervisor.cancel();
        let outcome = outcome(&mut supervisor).await?;
        assert!(
            matches!(outcome, TaskOutcome::Cancelled { .. }),
            "a stopped job is reported cancelled, got {outcome:?}"
        );
        assert_eq!(
            outcome.cleanup(),
            Some(&CleanupReceipt::CleanupConfirmed),
            "the job read empty after its terminate"
        );
        let Some(containment) = outcome.containment() else {
            return Err("a cancelled process carries a containment report".into());
        };
        assert_eq!(containment.mechanism(), ContainmentMechanism::JobObject);
        assert!(
            containment.captured() >= 2,
            "the job held the shell and the grandchild it started, got {}",
            containment.captured()
        );
        assert!(containment.survivors().is_empty());
        assert!(containment.is_complete());
        Ok(())
    })
}

#[test]
fn a_deadline_stops_a_job_and_the_run_reports_it() -> TestResult {
    let dir = Scratch::new("job-deadline")?;
    let mut spec = cmd("ping -n 600 127.0.0.1 >nul", dir.path());
    spec.deadline(Duration::from_secs(1));
    let runtime = Runtime::new()?;
    runtime.block_on(async {
        let mut supervisor = Supervisor::default();
        let run = supervisor.run_process(&spec).await?;
        assert!(run.deadline_fired(), "the deadline stopped the run");
        assert_eq!(run.cleanup(), &CleanupReceipt::CleanupConfirmed);
        assert_eq!(
            run.containment().mechanism(),
            ContainmentMechanism::JobObject
        );
        assert_eq!(supervisor.stats().failed, 1);
        Ok(())
    })
}

#[test]
fn a_spec_that_captures_a_stream_is_refused_before_anything_starts() -> TestResult {
    let dir = Scratch::new("job-capture")?;
    let mut spec = cmd("echo captured", dir.path());
    spec.capture_stdout(NonZeroUsize::MIN);
    let runtime = Runtime::new()?;
    runtime.block_on(async {
        let mut supervisor = Supervisor::default();
        let refused = supervisor.run_process(&spec).await;
        assert!(
            matches!(
                refused,
                Err(ProcessRunError::NotStarted { ref source })
                    if source.kind() == std::io::ErrorKind::Unsupported
            ),
            "a captured stream is refused as unsupported, got {refused:?}"
        );
        assert_eq!(supervisor.stats().spawned, 0, "nothing was started");
        Ok(())
    })
}

#[test]
fn a_zero_exit_with_a_live_descendant_still_settles_the_job_t19() -> TestResult {
    let dir = Scratch::new("job-zero")?;
    let script = tree::script(2, &tree::exit_after_a_second(0));
    let runtime = Runtime::new()?;
    runtime.block_on(async {
        let mut supervisor = Supervisor::default();
        supervisor.spawn_process(&cmd(&script, dir.path())).await?;
        let outcome = outcome(&mut supervisor).await?;
        tree::exited_with(&outcome, 0)?;
        tree::settled(&outcome, 2)?;
        assert!(
            tree::all_released(dir.path(), 2).await,
            "the descendants still held their files after the job settled"
        );
        Ok(())
    })
}

#[test]
fn a_nonzero_exit_is_a_failure_and_still_settles_the_job_t19() -> TestResult {
    let dir = Scratch::new("job-seven")?;
    let script = tree::script(1, &tree::exit_after_a_second(7));
    let runtime = Runtime::new()?;
    runtime.block_on(async {
        let mut supervisor = Supervisor::default();
        supervisor.spawn_process(&cmd(&script, dir.path())).await?;
        let outcome = outcome(&mut supervisor).await?;
        tree::exited_with(&outcome, 7)?;
        tree::settled(&outcome, 1)?;
        assert!(
            tree::all_released(dir.path(), 1).await,
            "the descendant still held its file after the job settled"
        );
        Ok(())
    })
}

#[test]
fn a_stop_before_the_manager_task_runs_still_stops_the_tree_t20() -> TestResult {
    let dir = Scratch::new("job-pre-poll")?;
    let script = tree::script(2, tree::STAY);
    // A current-thread runtime, and no await between starting the tree and
    // stopping it: the manager task that owns the job has not run when the stop
    // is requested, which is the state T20 names.
    lgwks_bot::block_on(async {
        let mut supervisor = Supervisor::default();
        supervisor.spawn_process(&cmd(&script, dir.path())).await?;
        tree::up_without_yielding(dir.path())?;
        assert!(
            tree::all_held(dir.path(), 2),
            "the background pings hold their files before the stop"
        );
        tree::stopped_tree(&supervisor.shutdown().await, 2)?;
        assert!(
            tree::all_released(dir.path(), 2).await,
            "the descendants still held their files after the stop"
        );
        Ok(())
    })
}

#[test]
fn a_stop_leaves_an_unrelated_process_running_t21() -> TestResult {
    let dir = Scratch::new("job-outsider")?;
    let script = tree::script(1, tree::STAY);
    let mut outsider = tree::Outsider::start()?;
    let runtime = Runtime::new()?;
    runtime.block_on(async {
        let mut supervisor = Supervisor::default();
        supervisor.spawn_process(&cmd(&script, dir.path())).await?;
        marker(dir.path(), tree::UP).await?;
        supervisor.cancel();
        let outcome = outcome(&mut supervisor).await?;
        tree::settled(&outcome, 2)?;
        assert!(
            tree::all_released(dir.path(), 1).await,
            "the descendant still held its file after the stop"
        );
        assert!(
            outsider.is_running(),
            "the stop reached a process that was never in the job"
        );
        Ok(())
    })
}
