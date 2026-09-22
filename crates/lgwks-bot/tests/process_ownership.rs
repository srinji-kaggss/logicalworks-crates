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
use lgwks_bot::rt::process::Command;
use lgwks_bot::rt::supervise::{Supervisor, TaskOutcome};
use lgwks_bot::rt::time::{Instant, sleep};

const BUDGET: Duration = Duration::from_secs(10);

struct PidDir(PathBuf);

impl PidDir {
    fn new(name: &str) -> std::io::Result<Self> {
        let path =
            std::env::temp_dir().join(format!("lgwks-bot-ownership-{}-{name}", std::process::id()));
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
        let mut command = Command::new("sh");
        supervisor
            .spawn_process(command.arg("-c").arg(&script))
            .await?;
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
        let mut command = Command::new("sh");
        supervisor
            .spawn_process(command.arg("-c").arg(&script))
            .await?;
        let child = pid(&child_file)
            .await
            .ok_or_else(|| std::io::Error::other("descendant pid was not recorded"))?;
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
        let mut command = Command::new("sh");
        supervisor
            .spawn_process(command.arg("-c").arg(&script))
            .await?;
        let child = pid(&child_file)
            .await
            .ok_or_else(|| std::io::Error::other("descendant pid was not recorded"))?;
        let _report = supervisor.shutdown().await;
        Ok::<_, std::io::Error>(child)
    })?;
    assert!(runtime.block_on(gone(&child)));
    Ok(())
}
