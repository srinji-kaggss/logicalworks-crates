//! #263 on Windows: the supervisor owns a process tree through a job object.
//!
//! The Unix tests of this contract read the operating system's process table
//! after the stop (`process_fire`). A job object makes a stronger statement than
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
//! The same suite runs wherever `cargo nextest run --locked -p lgwks_bot
//! --features process` runs on a Windows host; on any other host the file is
//! empty.

#![cfg(all(
    windows,
    feature = "rt",
    feature = "time",
    feature = "sync",
    feature = "process"
))]

use crate::scratch::Scratch;

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
            return Err("the supervisor never reported the stopped job".into());
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
