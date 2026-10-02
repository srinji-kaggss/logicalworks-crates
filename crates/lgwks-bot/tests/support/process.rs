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
/// never share a path even if the clock does not move between them.
static DIR_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A temporary directory a scenario's own commands write pid files into.
pub struct PidDir(PathBuf);

impl PidDir {
    /// Create the directory for a test called `name`.
    pub fn new(name: &str) -> std::io::Result<Self> {
        let pid = std::process::id();
        let seq = DIR_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("lgwks-bot-proc-{pid}-{seq}-{name}"));
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

/// Whether the process group `pgid` is absent, through `kill_process_group`.
///
/// An `ESRCH` (`raw_os_error` 3) is the OS saying no such group exists. A zombie
/// leader pins the group and reports `EPERM` instead, so this is `false` and the
/// caller keeps waiting until the group is reaped.
pub fn group_is_gone(pgid: i32) -> bool {
    match lgwks_std::process::kill_process_group(pgid) {
        Ok(()) => false,
        Err(error) => error.raw_os_error() == Some(3),
    }
}

/// Wait for group `pgid` to disappear, up to `budget`.
pub fn wait_group_gone(pgid: i32, budget: Duration) -> bool {
    let deadline = std::time::Instant::now()
        .checked_add(budget)
        .unwrap_or_else(std::time::Instant::now);
    loop {
        if group_is_gone(pgid) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return group_is_gone(pgid);
        }
        std::thread::park_timeout(Duration::from_millis(2));
    }
}
