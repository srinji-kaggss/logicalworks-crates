//! Black-box acceptance for supervised child processes.
//!
//! `Supervisor::spawn_process` is the only way this crate starts a process, and
//! these pin the three properties that make it the only way: it is bounded by
//! the supervisor's in-flight ceiling (that half is `supervise.rs`'s own unit
//! tests), it is killed as a **whole process group** when the supervisor is
//! dropped or the task is cancelled, and its outcome is reported — so a command
//! that exited non-zero is distinguishable from one that was cancelled.
//!
//! # Why the kill is tested through a grandchild
//!
//! `Child::kill` reaches the process that was started and nothing it spawned. A
//! shell is the case that matters: `sh -c 'sleep 30 & …; wait'` is one child with
//! one of its own, and a supervisor that killed only the shell would leave the
//! `sleep` running for its full 30 seconds with nobody holding its handle. The
//! test therefore records both pids and asserts that **both** are gone, which is
//! the property `contract/APPROVED.toml` records rustix for.
#![cfg(all(
    unix,
    feature = "rt",
    feature = "time",
    feature = "sync",
    feature = "process"
))]

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lgwks_bot::Runtime;
use lgwks_bot::rt::process::ProcessSpec;
use lgwks_bot::rt::supervise::{Supervisor, TaskOutcome};
use lgwks_bot::rt::time::{Instant, sleep};

/// What a test reports when its precondition did not hold.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// How often the drain loop re-reaps while it waits for an outcome.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// How long a drain or a pid-wait may take before the test calls it a failure.
const BUDGET: Duration = Duration::from_secs(10);

/// Reap and read until this supervisor reports one terminal outcome.
///
/// This is the crate's own read path rather than a workaround: `reap` is public
/// precisely so a caller who has stopped spawning can settle the counters, and
/// `next_report` is the buffered stream of outcomes. A supervisor has no
/// `wait_for` — outcomes are drained, not awaited on a named task — so this loop
/// is what a consumer writes, and the test exercises it rather than reaching
/// past it.
async fn next_outcome(
    supervisor: &mut Supervisor,
    budget: Duration,
) -> Result<TaskOutcome, String> {
    let Some(deadline) = Instant::now().checked_add(budget) else {
        return Err(String::from("the deadline overflowed the clock"));
    };
    loop {
        supervisor.reap();
        if let Some(outcome) = supervisor.next_report() {
            return Ok(outcome);
        }
        if Instant::now() >= deadline {
            return Err(format!("no terminal outcome within {budget:?}"));
        }
        sleep(POLL_INTERVAL).await;
    }
}

/// Whether a process with `pid` is still alive, asked through `kill -0`.
///
/// `kill -0` sends no signal and reports only whether the caller may signal the
/// pid at all, which is the standard liveness probe: it exits zero for a live
/// process and non-zero once the pid is gone.
fn is_alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid)
        .status()
        .is_ok_and(|status| status.success())
}

/// Wait for `pid` to disappear, up to `budget`.
///
/// The wait is bounded and retried because a signal is delivered
/// asynchronously: the kill has been sent by the time the supervisor's task has
/// finished, but the kernel may not have finished tearing the processes down.
async fn wait_until_gone(pid: &str, budget: Duration) -> bool {
    let Some(deadline) = Instant::now().checked_add(budget) else {
        return false;
    };
    while Instant::now() < deadline {
        if !is_alive(pid) {
            return true;
        }
        sleep(POLL_INTERVAL).await;
    }
    !is_alive(pid)
}

/// Wait until `path` holds a pid, up to `budget`.
async fn wait_until_pid_written(path: &Path, budget: Duration) -> Option<String> {
    let deadline = Instant::now().checked_add(budget)?;
    loop {
        if let Some(pid) = read_pid(path) {
            return Some(pid);
        }
        if Instant::now() >= deadline {
            return None;
        }
        sleep(POLL_INTERVAL).await;
    }
}

/// Read a pid a test's own command wrote, or `None` while it is not there yet.
fn read_pid(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(String::from(trimmed))
    }
}

/// A temporary directory for a test's pid files.
///
/// Named from the process id and the test's own name so two tests running at
/// once cannot share one, and removed by its [`Drop`] so a passing run leaves
/// nothing behind.
struct PidDir {
    /// The directory the test's command writes into.
    path: PathBuf,
}

impl PidDir {
    /// Create the directory for a test called `name`.
    fn new(name: &str) -> std::io::Result<Self> {
        let path = std::env::temp_dir().join(format!("lgwks-bot-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    /// The path of one pid file inside it.
    fn join(&self, file: &str) -> PathBuf {
        self.path.join(file)
    }
}

impl Drop for PidDir {
    /// Remove the directory, best effort.
    ///
    /// A test that failed mid-way should not fail again on the way out, and the
    /// directory is named for this process, so a leftover is inert rather than
    /// shared.
    fn drop(&mut self) {
        let _ignored: std::io::Result<()> = std::fs::remove_dir_all(&self.path);
    }
}

#[test]
fn a_command_that_exits_zero_is_reported_completed() -> TestResult {
    let runtime = Runtime::new()?;
    let (outcome, succeeded, failed) = runtime.block_on(async {
        let mut supervisor = Supervisor::default();
        let mut command = ProcessSpec::new("sh");
        command.arg("-c").arg("exit 0");
        supervisor.spawn_process(&command).await?;
        let outcome = next_outcome(&mut supervisor, BUDGET)
            .await
            .map_err(std::io::Error::other)?;
        let stats = supervisor.stats();
        Ok::<_, std::io::Error>((outcome, stats.succeeded, stats.failed))
    })?;
    assert!(
        matches!(outcome, TaskOutcome::Completed { .. }),
        "a zero exit must be reported as a clean completion, not as {outcome:?}"
    );
    assert_eq!(succeeded, 1, "the success counter must see it");
    assert_eq!(failed, 0, "a zero exit is not a process failure");
    Ok(())
}

#[test]
fn a_non_zero_exit_is_reported_failed_with_its_status() -> TestResult {
    let runtime = Runtime::new()?;
    let (outcome, failed) = runtime.block_on(async {
        let mut supervisor = Supervisor::default();
        let mut command = ProcessSpec::new("sh");
        command.arg("-c").arg("exit 3");
        supervisor.spawn_process(&command).await?;
        let outcome = next_outcome(&mut supervisor, BUDGET)
            .await
            .map_err(std::io::Error::other)?;
        Ok::<_, std::io::Error>((outcome, supervisor.stats().failed))
    })?;

    assert!(
        outcome.is_process_failure(),
        "a command that exited 3 must be reported as a process failure: {outcome:?}"
    );
    assert!(
        !outcome.is_success(),
        "a process failure must not read as a completed task"
    );
    assert_eq!(
        outcome.exit_status().and_then(|status| status.code()),
        Some(3),
        "the exit status must survive into the report, so the cause is not a guess"
    );
    assert_eq!(
        failed, 1,
        "the failure counter must not fold it into success"
    );
    Ok(())
}

#[test]
fn a_cancelled_process_is_killed_with_its_grandchild() -> TestResult {
    let dir = PidDir::new("grandchild")?;
    let shell_pid_file = dir.join("shell.pid");
    let grandchild_pid_file = dir.join("grandchild.pid");
    let script = format!(
        "sleep 30 & echo $! > {}; echo $$ > {}; wait",
        grandchild_pid_file.display(),
        shell_pid_file.display()
    );

    let runtime = Runtime::new()?;
    let (outcomes, cancelled, failed) = runtime.block_on(async {
        let mut supervisor = Supervisor::default();
        let mut command = ProcessSpec::new("sh");
        command.arg("-c").arg(&script);
        supervisor
            .spawn_process(&command)
            .await?;

        // Wait until the shell has recorded both pids: cancelling before that
        // would test the kill against a command that never started its work.
        let (Some(shell), Some(grandchild)) = (
            wait_until_pid_written(&shell_pid_file, BUDGET).await,
            wait_until_pid_written(&grandchild_pid_file, BUDGET).await,
        ) else {
            return Err(std::io::Error::other(
                "the shell did not record both pids within the budget",
            ));
        };

        // Shutdown cancels the token, which is the path under test: the task
        // observes it, kills the group, and reports Cancelled.
        let mut report = supervisor.shutdown().await;
        let stats = report.stats();
        let cancelled = stats.cancelled;
        let failed = stats.failed;

        for pid in [&shell, &grandchild] {
            if !wait_until_gone(pid, BUDGET).await {
                return Err(std::io::Error::other(format!(
                    "pid {pid} is still running after the supervisor was shut down, so the kill did \
                     not reach the whole process group"
                )));
            }
        }
        let Some(deadline) = Instant::now().checked_add(BUDGET) else {
            return Err(std::io::Error::other("cleanup deadline overflowed"));
        };
        while report.pending_cleanup_count() > 0 && Instant::now() < deadline {
            report.reap_pending_cleanups();
            if report.pending_cleanup_count() > 0 {
                sleep(POLL_INTERVAL).await;
            }
        }
        if report.pending_cleanup_count() > 0 {
            return Err(std::io::Error::other(format!(
                "{} process-group cleanup owners remained pending after the observed pids exited",
                report.pending_cleanup_count()
            )));
        }
        let outcomes = report
            .into_outcomes()
            .map_err(|_| std::io::Error::other("process cleanup remained pending after shutdown"))?;
        Ok::<_, std::io::Error>((outcomes, cancelled, failed))
    })?;

    let task_outcome = outcomes
        .iter()
        .find(|outcome| !matches!(outcome, TaskOutcome::CleanupSettled { .. }));
    let cleanup_receipt = outcomes
        .iter()
        .find(|outcome| matches!(outcome, TaskOutcome::CleanupSettled { .. }));
    let Some(task_outcome) = task_outcome else {
        return Err(std::io::Error::other(format!("missing task report: {outcomes:?}")).into());
    };
    assert!(
        matches!(task_outcome, TaskOutcome::Cancelled { .. }),
        "a process this supervisor stopped is cancelled, not failed: {task_outcome:?}"
    );
    match (task_outcome.cleanup(), cleanup_receipt) {
        (Some(lgwks_bot::rt::supervise::CleanupReceipt::CleanupConfirmed), None) => {
            assert_eq!(outcomes.len(), 1, "immediate proof needs no later receipt");
        }
        (Some(_), Some(receipt)) => {
            assert_eq!(outcomes.len(), 2, "pending cleanup needs one later receipt");
            assert_eq!(task_outcome.task(), receipt.task());
            assert_eq!(
                receipt.cleanup(),
                Some(lgwks_bot::rt::supervise::CleanupReceipt::CleanupConfirmed),
                "only observed group absence may emit the terminal receipt"
            );
        }
        _ => {
            return Err(std::io::Error::other(format!(
                "cleanup must be confirmed immediately or by one later receipt: {outcomes:?}"
            ))
            .into());
        }
    }
    assert_eq!(cancelled, 1, "the cancellation counter must see it");
    assert_eq!(
        failed, 0,
        "a kill this supervisor ordered is not a process failure"
    );
    Ok(())
}

#[test]
fn process_deadline_terminates_the_child_and_reports_its_cleanup() -> TestResult {
    let runtime = Runtime::new()?;
    let outcome = runtime.block_on(async {
        let mut supervisor = Supervisor::default();
        let mut spec = ProcessSpec::new("sh");
        spec.arg("-c").arg("sleep 30");
        spec.deadline(Duration::from_millis(10));
        supervisor
            .spawn_process(&spec)
            .await
            .map_err(|error| error.to_string())?;
        next_outcome(&mut supervisor, BUDGET).await
    })?;
    assert!(
        matches!(outcome, TaskOutcome::Failed { status: None, .. }),
        "deadline expiration remains a failed, status-unknown outcome: {outcome:?}"
    );
    assert!(
        outcome.cleanup().is_some(),
        "the real process path must report its cleanup receipt"
    );
    Ok(())
}

#[test]
fn a_command_that_cannot_start_reports_the_failure_to_the_caller() -> TestResult {
    let runtime = Runtime::new()?;
    let error = runtime.block_on(async {
        let mut supervisor = Supervisor::default();
        supervisor
            .spawn_process(&ProcessSpec::new("/nonexistent/lgwks-bot-probe"))
            .await
            .err()
            .ok_or_else(|| std::io::Error::other("starting a missing program must fail"))
    })?;
    assert_eq!(
        error.kind(),
        ErrorKind::NotFound,
        "the caller must be able to tell a program that is not there from one that ran and failed"
    );

    // A failed start must not consume a slot: the supervisor still works.
    let outcome = runtime.block_on(async {
        let mut supervisor = Supervisor::default();
        let mut command = ProcessSpec::new("sh");
        command.arg("-c").arg("exit 0");
        supervisor.spawn_process(&command).await?;
        next_outcome(&mut supervisor, BUDGET)
            .await
            .map_err(std::io::Error::other)
    })?;
    assert!(
        matches!(outcome, TaskOutcome::Completed { .. }),
        "the supervisor must be usable after a refused start: {outcome:?}"
    );
    Ok(())
}
