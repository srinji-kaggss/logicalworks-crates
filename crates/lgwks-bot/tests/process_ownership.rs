//! Regressions for process ownership across leader exit and task cancellation.

#![cfg(all(
    feature = "rt",
    feature = "time",
    feature = "sync",
    feature = "process"
))]

use std::path::{Path, PathBuf};
use std::time::Duration;

use lgwks_bot::Runtime;
use lgwks_bot::rt::process::ProcessSpec;
use lgwks_bot::rt::supervise::{Supervisor, TaskOutcome};
use lgwks_bot::rt::time::{Instant, sleep};

const BUDGET: Duration = Duration::from_secs(10);

struct PidDir(PathBuf);

/// Monotone per-process sequence, so two `PidDir`s in one test binary never
/// share a path even if the clock does not move between them.
static DIR_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl PidDir {
    fn new(name: &str) -> std::io::Result<Self> {
        // Wall-clock nanos plus a monotone sequence. Not a process or thread id
        // (both are reused by the OS) and not `lgwks_std::random` (that module
        // is behind the `random`/`ephemeral` features, and the feature matrix
        // builds this test without them).
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or(0);
        let seq = DIR_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("lgwks-bot-ownership-{nanos}-{seq}-{name}"));
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for PidDir {
    fn drop(&mut self) {
        let _ignored = std::fs::remove_dir_all(&self.0);
    }
}

fn is_alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid)
        .status()
        .is_ok_and(|status| status.success())
}

async fn pid(path: &Path) -> Option<String> {
    let deadline = Instant::now().checked_add(BUDGET)?;
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            let text = text.trim();
            if !text.is_empty() {
                return Some(String::from(text));
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        sleep(Duration::from_millis(5)).await;
    }
}

async fn outcome(supervisor: &mut Supervisor) -> Result<TaskOutcome, std::io::Error> {
    let deadline = Instant::now()
        .checked_add(BUDGET)
        .ok_or_else(|| std::io::Error::other("clock deadline overflowed"))?;
    loop {
        supervisor.reap();
        if let Some(outcome) = supervisor.next_report() {
            return Ok(outcome);
        }
        if Instant::now() >= deadline {
            return Err(std::io::Error::other("process outcome did not arrive"));
        }
        sleep(Duration::from_millis(5)).await;
    }
}

async fn gone(pid: &str) -> bool {
    let deadline = Instant::now().checked_add(BUDGET);
    while deadline.is_some_and(|value| Instant::now() < value) {
        if !is_alive(pid) {
            return true;
        }
        sleep(Duration::from_millis(5)).await;
    }
    !is_alive(pid)
}

#[test]
fn zero_exit_does_not_fabricate_tree_cleanup() -> Result<(), Box<dyn std::error::Error>> {
    let dir = PidDir::new("zero")?;
    let child_file = dir.file("child.pid");
    let script = format!("sleep 60 & echo $! > {}; exit 0", child_file.display());
    let runtime = Runtime::new()?;
    let (outcome, child) = runtime.block_on(async {
        let mut supervisor = Supervisor::default();
        let mut spec = ProcessSpec::new("sh");
        spec.arg("-c").arg(&script);
        supervisor.spawn_process(&spec).await?;
        let child = pid(&child_file)
            .await
            .ok_or_else(|| std::io::Error::other("descendant pid was not recorded"))?;
        let outcome = outcome(&mut supervisor).await?;
        Ok::<_, std::io::Error>((outcome, child))
    })?;
    assert!(outcome.cleanup().is_some());
    assert!(runtime.block_on(gone(&child)));
    Ok(())
}

#[test]
fn nonzero_exit_does_not_fabricate_tree_cleanup() -> Result<(), Box<dyn std::error::Error>> {
    let dir = PidDir::new("nonzero")?;
    let child_file = dir.file("child.pid");
    let script = format!("sleep 60 & echo $! > {}; exit 7", child_file.display());
    let runtime = Runtime::new()?;
    let (outcome, child) = runtime.block_on(async {
        let mut supervisor = Supervisor::default();
        let mut spec = ProcessSpec::new("sh");
        spec.arg("-c").arg(&script);
        supervisor.spawn_process(&spec).await?;
        // Block this executor thread after native spawn without yielding, so
        // the manager future cannot receive its first poll before shutdown is
        // requested. The shell still runs: it forks the descendant and records
        // the pid. Wait for that record with a deadline rather than a fixed
        // pause, because a one-shot read after 25ms loses the race on a loaded
        // machine and reports a missing descendant that is merely late.
        let deadline = std::time::Instant::now() + BUDGET;
        let child = loop {
            if let Ok(text) = std::fs::read_to_string(&child_file) {
                let text = text.trim();
                if !text.is_empty() {
                    break String::from(text);
                }
            }
            if std::time::Instant::now() >= deadline {
                return Err(std::io::Error::other("descendant pid was not recorded"));
            }
            std::hint::spin_loop();
        };
        let outcome = outcome(&mut supervisor).await?;
        Ok::<_, std::io::Error>((outcome, child))
    })?;
    assert!(outcome.is_process_failure());
    assert!(outcome.cleanup().is_some());
    assert!(runtime.block_on(gone(&child)));
    Ok(())
}

#[test]
fn cancellation_before_manager_task_poll_keeps_descendant_owned()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = PidDir::new("pre-poll")?;
    let child_file = dir.file("child.pid");
    let script = format!("sleep 60 & echo $! > {}; wait", child_file.display());
    let runtime = Runtime::new()?;
    let child = runtime.block_on(async {
        let mut supervisor = Supervisor::default();
        let mut spec = ProcessSpec::new("sh");
        spec.arg("-c").arg(&script);
        supervisor.spawn_process(&spec).await?;
        let child = pid(&child_file)
            .await
            .ok_or_else(|| std::io::Error::other("descendant pid was not recorded"))?;
        let _report = supervisor.shutdown().await;
        Ok::<_, std::io::Error>(child)
    })?;
    assert!(runtime.block_on(gone(&child)));
    Ok(())
}
