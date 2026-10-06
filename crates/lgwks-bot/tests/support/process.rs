//! Shared probes for the test targets that run a supervised child.
//!
//! One copy of the pid-file reader and the process-group absence wait,
//! included by path from each test target that needs them; the directory the
//! pid files go in is the shared `scratch.rs`. A copy per
//! file is how two targets drift into asserting different things about the same
//! OS facility.

use std::path::Path;
use std::time::Duration;

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
    let started = std::time::Instant::now();
    loop {
        if done(pgid) {
            return true;
        }
        if started.elapsed() >= budget {
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
    let started = std::time::Instant::now();
    loop {
        if let Some(pid) = read_pid(path) {
            return Some(pid);
        }
        if started.elapsed() >= budget {
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

/// What `ps` reports for `pid` and for every process in the group it leads, one
/// `pid ppid pgid stat comm` row each, or `absent` when nothing matches.
///
/// A failed "this pid is gone" wait can mean three different things: the process
/// is still running, it is a zombie nobody has reaped (`kill -0` succeeds for
/// both), or a member of its group outlived it. The `stat` column tells them
/// apart (`Z` is a zombie), so the assertion carries it instead of a bare
/// "still alive" that cannot be told from either.
pub fn describe_pid(pid: i32) -> String {
    let listing = match std::process::Command::new("ps")
        .args(["-eo", "pid=,ppid=,pgid=,stat=,comm="])
        .output()
    {
        Ok(output) => String::from_utf8_lossy(&output.stdout).into_owned(),
        Err(unlisted) => return format!("pid {pid}: `ps` could not list processes: {unlisted}"),
    };
    let wanted = pid.to_string();
    let rows: Vec<&str> = listing
        .lines()
        .filter(|row| {
            let mut columns = row.split_whitespace();
            let (row_pid, _parent, row_group) = (columns.next(), columns.next(), columns.next());
            row_pid == Some(wanted.as_str()) || row_group == Some(wanted.as_str())
        })
        .collect();
    if rows.is_empty() {
        "absent".to_owned()
    } else {
        rows.join(" | ")
    }
}

/// Send SIGKILL to `pid` and reap whatever the platform reports.
///
/// The signal is sent by the `kill` utility rather than by a syscall because
/// this crate builds with `unsafe_code` forbidden and the test targets inherit
/// that. `status()` waits for it, so the helper leaves no unreaped child for the
/// harness's own leak check to find.
pub fn kill_pid(pid: i32) {
    let _ignored = std::process::Command::new("kill")
        .arg("-9")
        .arg(pid.to_string())
        .status();
}

/// Wait until `pid` is gone, up to `budget`.
///
/// `None` for a pid that is still running when the budget expires, so a caller
/// cannot report a survivor as cleaned up by reading the timeout as success.
pub fn wait_for_pid_gone(pid: i32, budget: Duration) -> Option<()> {
    let started = std::time::Instant::now();
    while pid_is_alive(pid) {
        if started.elapsed() >= budget {
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

/// One way to leave the caller's process group, and to record the pid only
/// after leaving it.
///
/// The ordering is the whole point and is why the recording is part of each
/// candidate's program rather than something the caller appends: a pid recorded
/// *before* `setsid` belongs to a process that is still a member of the caller's
/// group, and a test holding that pid would be measuring the ordinary group kill
/// while believing it measured an escape.
///
/// A flag of `-c` runs `source` as a shell command line; a flag of `-e` runs it
/// as interpreter source. Both call `setsid`, then record, then sleep.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EscapeCommand {
    /// The interpreter or utility to run.
    pub program: &'static str,
    /// The argument text that selects the third field's meaning: one flag for
    /// an interpreter, or a shell and its flag for a utility that runs one.
    pub flag: &'static str,
    /// The program, or the command line, that escapes and then records `$$`.
    ///
    /// `{file}` is replaced with the pid path. It is a placeholder rather than a
    /// second argument because each interpreter takes its program as one
    /// argument: a trailing path would be `@ARGV` to perl and ignored.
    source: &'static str,
}

impl EscapeCommand {
    /// The shell command line that escapes the group and records its pid.
    ///
    /// The interpreter runs in the **background** (`&`), and that is the whole
    /// design rather than an incidental shell feature. `setsid()` fails with
    /// `EPERM` when its caller is already a process-group leader, and it fails
    /// *silently* here — a perl one-liner that ignores the return value simply
    /// carries on in the supervisor's group and is killed with it, so the test
    /// sees an escapee that never escaped. Since the supervisor calls
    /// `process_group(0)`, its direct child **is** a group leader, so the only
    /// process that can successfully leave the group is a *child* of that leader.
    /// Backgrounding makes exactly that: the leader forks, and the fork is not a
    /// group leader, so its `setsid` succeeds.
    ///
    /// The leader is kept alive by the trailing `wait`, so the supervised child
    /// does not exit immediately and hand the supervisor a terminal outcome
    /// before the escapee has recorded anything.
    ///
    /// The program is passed as a single **single-quoted** shell word, the only
    /// quoting that leaves the sources intact in both directions:
    ///
    /// - Double quotes would let this shell expand `$f` and `$$` before perl or
    ///   python ever saw them, so `open(my $f, …)` arrives as `open(my , …)` and
    ///   the candidate dies of a syntax error.
    /// - Single quotes are therefore required, which is why every source below is
    ///   written with double quotes and no apostrophe: a source containing `'`
    ///   would terminate the wrapper early.
    ///   [`sources_are_apostrophe_free`] keeps that true rather than leaving it to
    ///   a comment.
    pub fn script(&self, pid_file: &Path) -> String {
        let source = self
            .source
            .replace("{file}", &pid_file.display().to_string());
        format!("{} {} '{source}' & wait", self.program, self.flag)
    }
}

/// Whether every escape source is free of the apostrophe that would break the
/// single-quoted wrapper in [`EscapeCommand::script`].
///
/// A check rather than a comment, because the failure it prevents is invisible
/// from outside: a mis-quoted source makes the candidate record nothing, and a
/// test that waits for a pid then reports "this host cannot escape" — a claim
/// about the host that is really about this file's quoting.
pub fn sources_are_apostrophe_free() -> bool {
    ESCAPE_COMMANDS
        .iter()
        .all(|candidate| !candidate.source.contains('\''))
}

/// Every way this suite knows to leave a process group, in preference order.
///
/// `setsid(1)` first because it is the utility that does exactly this and
/// nothing else. macOS ships no `setsid(1)`, so the interpreters follow: POSIX
/// `setsid` is in both, and each takes its program as one argument after a
/// single flag.
///
/// Each source records the pid *after* `setsid` returns, so the pid a test reads
/// back belongs to a process that has already left the group. That ordering is
/// the difference between measuring an escape and measuring the ordinary group
/// kill, and it is why the recording lives here rather than in the caller.
const ESCAPE_COMMANDS: [EscapeCommand; 3] = [
    EscapeCommand {
        program: "setsid",
        // `setsid(1)` runs a program with its arguments, so the "flag" here is
        // the shell plus its `-c`: without `-c`, `sh` reads the source as a
        // *script file name* and records nothing. macOS has no `setsid(1)` and
        // falls through to perl, so only a Linux host exercises this entry.
        flag: "sh -c",
        source: "echo $$ > {file}; sleep 30",
    },
    EscapeCommand {
        program: "perl",
        flag: "-e",
        // The flush is load-bearing, not decoration: perl's filehandles are
        // block-buffered, so an unflushed `print` leaves the pid in a buffer
        // until the process exits. The test reads the file while the process is
        // alive, sees a zero-length file, and concludes the escape never
        // recorded anything — a conclusion that is really about a missing flush.
        source: "use POSIX; setsid(); open(my $f, \">\", \"{file}\"); print $f $$; $f->flush(); \
                 sleep 30",
    },
    EscapeCommand {
        program: "python3",
        flag: "-c",
        source: "import os,time; os.setsid(); open(\"{file}\",\"w\").write(str(os.getpid())); \
                 time.sleep(30)",
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
