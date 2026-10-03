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

/// Wait until `path` holds a pid, up to `budget`.
///
/// A pid file is the event a test waits on rather than a delay it guesses at: a
/// child that records its own pid has provably reached the point under test, and
/// a loaded host cannot make the test act before that point.
pub fn wait_for_pid(path: &Path, budget: Duration) -> Option<i32> {
    let deadline = std::time::Instant::now()
        .checked_add(budget)
        .unwrap_or_else(std::time::Instant::now);
    loop {
        if let Some(pid) = read_pid(path) {
            return Some(pid);
        }
        if std::time::Instant::now() >= deadline {
            return read_pid(path);
        }
        std::thread::park_timeout(Duration::from_millis(5));
    }
}

/// Whether a process with `pid` is still running, asked through `kill -0`.
///
/// `kill -0` delivers no signal, so the probe observes without disturbing — which
/// is the whole requirement for an unrelated process the supervisor must not
/// touch. It exits zero while the pid may be signalled and non-zero once it is
/// gone, so it answers "does this pid still exist" and nothing about *why*.
pub fn pid_is_alive(pid: i32) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .is_ok_and(|status| status.success())
}

/// Wait until `pid` is gone, up to `budget`.
///
/// `None` for a pid that is still running when the budget expires, so a caller
/// cannot report a survivor as cleaned up by reading the timeout as success.
pub fn wait_for_pid_gone(pid: i32, budget: Duration) -> Option<()> {
    let deadline = std::time::Instant::now()
        .checked_add(budget)
        .unwrap_or_else(std::time::Instant::now);
    while pid_is_alive(pid) {
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::park_timeout(Duration::from_millis(5));
    }
    Some(())
}

/// Assert that one captured stream kept exactly `ceiling` bytes of `total`.
///
/// The four facts a captured stream must report when a child wrote past its
/// ceiling, in one place, because every test that floods a pipe needs all four
/// and a test checking three would pass against a reader that kept growing.
/// `total` is the exact byte count the child wrote, so this distinguishes "kept
/// the ceiling and counted the rest" from "kept everything" and from "grew".
pub fn assert_capture_within_ceiling(
    name: &str,
    stream: &lgwks_bot::rt::process::CapturedStream,
    ceiling: usize,
    total: u64,
) {
    assert_eq!(
        stream.bytes().len(),
        ceiling,
        "{name} must retain exactly its first {ceiling} bytes"
    );
    assert!(
        stream.retained_capacity() <= ceiling,
        "{name} retained {} bytes of capacity, past the {ceiling}-byte ceiling",
        stream.retained_capacity()
    );
    assert!(
        stream.truncated(),
        "{name} wrote {total} bytes, over its {ceiling}-byte ceiling"
    );
    assert_eq!(
        stream.total_bytes(),
        total,
        "{name} must report the exact total it saw, past its own ceiling"
    );
}

/// The command that leaves the caller's process group, on this host.
///
/// A child that calls `setsid` becomes a session and group leader of its own, so
/// the supervisor's group kill cannot reach it: that is the escape T21 needs a
/// real process for, because an in-process fake proves nothing about whether the
/// syscall was actually issued.
///
/// `setsid(1)` is absent on macOS, so the interpreters follow it. `None` means
/// this host has none of the three, which is a recorded limit rather than a
/// silent skip — [`escape_unavailable_reason`] names it.
///
/// A table rather than three branches: the candidates are data, and a branch per
/// candidate would put the "which one did we use" fact in control flow instead of
/// where a reader can see all three at once.
pub fn escape_command() -> Option<EscapeCommand> {
    ESCAPE_COMMANDS
        .iter()
        .find(|candidate| which(candidate.program).is_some())
        .copied()
}

/// One way to leave the caller's process group: a program, a flag, and the source
/// it runs under that flag.
///
/// A flag of `-c` runs `source` as a shell command line; a flag of `-e` runs it
/// as interpreter source. Both call `setsid` and then sleep, which is all a
/// caller needs: the only fact asserted downstream is that the pid landed in
/// another group.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EscapeCommand {
    /// The interpreter or utility to run.
    pub program: &'static str,
    /// The single argument that selects the third field's meaning.
    pub flag: &'static str,
    /// The program, or the command line, that calls `setsid` and then sleeps.
    pub source: &'static str,
}

/// Every way this suite knows to leave a process group, in preference order.
const ESCAPE_COMMANDS: [EscapeCommand; 3] = [
    EscapeCommand {
        program: "setsid",
        flag: "sh",
        source: "-c 'sleep 30'",
    },
    EscapeCommand {
        program: "perl",
        flag: "-e",
        source: "use POSIX; setsid(); sleep 30",
    },
    EscapeCommand {
        program: "python3",
        flag: "-c",
        source: "import os,time; os.setsid(); time.sleep(30)",
    },
];

/// Why [`escape_command`] is unavailable, as a sentence a test may report.
///
/// The recorded reason a skip is allowed to carry. A test that cannot run its
/// subject says which host capability was missing instead of passing quietly,
/// because "the escape was not exercised" and "the escape did not happen" look
/// identical in a green suite.
pub fn escape_unavailable_reason() -> String {
    String::from(
        "no setsid(1), perl with POSIX, or python3 on this host, so a real session escape \
         could not be performed; T21's escape case was NOT exercised",
    )
}

/// Whether `program` is on `PATH`, without running it.
fn which(program: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// Wait for group `pgid` to disappear, up to `budget`.
pub fn wait_group_gone(pgid: i32, budget: Duration) -> bool {
    wait_for(pgid, budget, group_is_gone)
}

/// Wait for every process in group `pgid` to have ended, up to `budget`.
pub fn wait_group_stopped(pgid: i32, budget: Duration) -> bool {
    wait_for(pgid, budget, group_has_stopped)
}
