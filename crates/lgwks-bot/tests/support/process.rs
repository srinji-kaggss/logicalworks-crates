//! Shared probes for the test targets that run a supervised child.
//!
//! One copy of the pid-file scratch directory and the process-group absence
//! wait, included by path from each test target that needs them. A copy per
//! file is how two targets drift into asserting different things about the same
//! OS facility.

#![allow(
    dead_code,
    reason = "each including test target uses a different subset of these probes"
)]

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Monotone per-process sequence, so two scratch directories in one binary
/// never share a path even if the random tag does.
static DIR_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A temporary directory a scenario's own commands write pid files into.
pub struct PidDir(PathBuf);

impl PidDir {
    /// Create the directory for a test called `name`.
    pub fn new(name: &str) -> std::io::Result<Self> {
        let tag = lgwks_std::random::bytes::<8>().map_or(0, u64::from_le_bytes);
        let seq = DIR_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("lgwks-bot-proc-{tag:016x}-{seq}-{name}"));
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }

    /// The path of one file inside it.
    pub fn join(&self, file: &str) -> PathBuf {
        self.0.join(file)
    }
}

impl Drop for PidDir {
    /// Remove the directory, best effort.
    fn drop(&mut self) {
        let _ignored = std::fs::remove_dir_all(&self.0);
    }
}

/// Read a pid a scenario's own child wrote, or `None` while it is absent.
pub fn read_pid(path: &Path) -> Option<i32> {
    let text = std::fs::read_to_string(path).ok()?;
    text.trim().parse::<i32>().ok()
}

/// Drive `run` until its child has written its pid to `pid_file`, then drop
/// `run` mid-flight and return that pid.
///
/// The drop happens on the event (the child is alive and has said so), not
/// after a guessed delay, so a loaded host cannot make the drop land before
/// the fork. `None` means `run` finished before the child wrote its pid,
/// which a caller expecting a mid-flight drop reports as a failure.
pub async fn drop_after_pid<F: std::future::Future>(run: F, pid_file: &Path) -> Option<i32> {
    let mut run = std::pin::pin!(run);
    std::future::poll_fn(|cx| {
        if run.as_mut().poll(cx).is_ready() {
            return std::task::Poll::Ready(None);
        }
        // The runner wakes itself on its own observation interval while the
        // child lives, so this check reruns on each of those polls.
        read_pid(pid_file).map_or(std::task::Poll::Pending, |pid| {
            std::task::Poll::Ready(Some(pid))
        })
    })
    .await
}

/// The state letter of every process in group `pgid`, read from the process
/// table with `ps` and no signal.
///
/// The probe must only observe: a probe that signalled the group (as a
/// `killpg` existence check does) would itself kill a group the supervisor
/// failed to clean up, and the test would pass on the probe's work. `None`
/// means the table could not be read, which no caller treats as absence.
fn group_members(pgid: i32) -> Option<Vec<char>> {
    let output = std::process::Command::new("ps")
        .args(["-A", "-o", "pgid=", "-o", "stat="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let group = fields.next()?.parse::<i32>().ok()?;
                let state = fields.next()?.chars().next()?;
                (group == pgid).then_some(state)
            })
            .collect(),
    )
}

/// Every process table row of group `pgid` (pid, parent, state, command), for
/// a failure message that names what survived.
pub fn describe_group(pgid: i32) -> String {
    let Ok(output) = std::process::Command::new("ps")
        .args(["-A", "-o", "pgid=,pid=,ppid=,stat=,command="])
        .output()
    else {
        return String::from("<process table unreadable>");
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| {
            line.split_whitespace()
                .next()
                .and_then(|group| group.parse::<i32>().ok())
                == Some(pgid)
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Whether no process at all remains in group `pgid`.
pub fn group_is_gone(pgid: i32) -> bool {
    group_members(pgid).is_some_and(|members| members.is_empty())
}

/// Whether every process left in group `pgid` has ended.
///
/// A zombie (`Z`) has run to its end and waits only to be reaped. A run future
/// dropped mid-flight kills its group but no longer owns the leader to reap
/// it, so the engine's orphan reaper collects it later; until then the leader
/// is a zombie, which is no orphan running work.
pub fn group_has_stopped(pgid: i32) -> bool {
    group_members(pgid).is_some_and(|members| members.iter().all(|state| *state == 'Z'))
}

/// Wait until `done(pgid)` holds, up to `budget`.
fn wait_for(pgid: i32, budget: Duration, done: fn(i32) -> bool) -> bool {
    let deadline = std::time::Instant::now()
        .checked_add(budget)
        .unwrap_or_else(std::time::Instant::now);
    loop {
        if done(pgid) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return done(pgid);
        }
        std::thread::park_timeout(Duration::from_millis(5));
    }
}

/// Wait for group `pgid` to disappear, up to `budget`.
pub fn wait_group_gone(pgid: i32, budget: Duration) -> bool {
    wait_for(pgid, budget, group_is_gone)
}

/// Wait for every process in group `pgid` to have ended, up to `budget`.
pub fn wait_group_stopped(pgid: i32, budget: Duration) -> bool {
    wait_for(pgid, budget, group_has_stopped)
}
