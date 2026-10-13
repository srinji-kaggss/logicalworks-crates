//! Child processes for the gate: every child leads its own process group and is
//! recorded, so stopping the gate stops what it started.
//!
//! Before this, killing the coordinator left its nextest run building and testing
//! on, holding the target lock and the CPU the next run was measured against.
//! `std` can start a child in a new group (`CommandExt::process_group`) but
//! cannot catch a signal, so the gate that is itself killed is covered by a
//! watchdog: a small `bash` process in a group of its own that reads the groups
//! the gate started over a pipe, and stops them all when that pipe closes.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

/// The longest one child may run.
const RUN_BOUND: Duration = Duration::from_secs(3600);

/// How long a group gets between `TERM` and `KILL`.
const GRACE: Duration = Duration::from_secs(10);

/// The exit code of a child that could not be started or ran past its bound.
const UNRUNNABLE: i32 = 125;

/// The exit code of a run the gate stopped before it started.
const STOPPED: i32 = 130;

/// The watchdog: reads `add <pgid>` and `del <pgid>` lines, stops every group it
/// still holds when its input closes or a `quit` arrives.
const WATCHDOG: &str = r#"groups=" "
while read -r op pg; do
  case "$op" in
    add) groups="${groups}${pg} " ;;
    del) groups="${groups/ ${pg} / }" ;;
    quit) exit 0 ;;
  esac
done
[ "$groups" = " " ] && exit 0
for pg in $groups; do kill -s TERM -- "-$pg" 2>/dev/null; done
sleep 10
for pg in $groups; do kill -s KILL -- "-$pg" 2>/dev/null; done
"#;

/// What a supervised child came to.
pub(super) struct Captured {
    /// The exit code.
    pub(super) code: i32,
    /// Standard output.
    pub(super) stdout: String,
    /// Standard error.
    pub(super) stderr: String,
}

/// Locks `mutex`, taking the data from a poisoned one: the registry holds plain
/// ids, which a panicking holder cannot leave half-written.
fn guard<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(held) => held,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Waits up to `limit` for `slot` to hold a value; whether it does.
fn wait_filled<T>(slot: &Mutex<Option<T>>, ready: &Condvar, limit: Duration) -> bool {
    let held = guard(slot);
    match ready.wait_timeout_while(held, limit, |value| value.is_none()) {
        Ok(waited) => waited.0.is_some(),
        Err(poisoned) => poisoned.into_inner().0.is_some(),
    }
}

/// The exit code a status stands for; a signal reads as `128 + signal`.
fn code_of(status: ExitStatus) -> i32 {
    match status.code() {
        Some(code) => code,
        None => match status.signal() {
            Some(signal) => 128_i32.saturating_add(signal),
            None => UNRUNNABLE,
        },
    }
}

/// Every child the gate has started and not yet reaped.
pub(super) struct Children {
    /// The process-group ids of the live children.
    live: Mutex<BTreeSet<u32>>,
    /// Signalled whenever `live` shrinks.
    changed: Condvar,
    /// Set once the gate is stopping: no new child starts after it.
    stopping: AtomicBool,
    /// The watchdog, when one is running.
    watchdog: Mutex<Option<Watchdog>>,
}

/// The process that stops the gate's groups if the gate dies.
struct Watchdog {
    /// The process, reaped by [`Children::finish`].
    child: Child,
    /// Where the groups are reported.
    input: ChildStdin,
}

impl Children {
    /// A registry; with `watch`, a watchdog covers the gate's own death.
    pub(super) fn new(watch: bool) -> Self {
        let running = if watch { start_watchdog() } else { None };
        Self {
            live: Mutex::new(BTreeSet::new()),
            changed: Condvar::new(),
            stopping: AtomicBool::new(false),
            watchdog: Mutex::new(running),
        }
    }

    /// Whether the gate is stopping.
    pub(super) fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// Tells the watchdog about a group.
    fn tell(&self, op: &str, pgid: u32) {
        let mut running = guard(&self.watchdog);
        if let Some(dog) = running.as_mut()
            && let Err(error) = writeln!(dog.input, "{op} {pgid}").and_then(|()| dog.input.flush())
        {
            lgwks_std::trace::debug!(%error, "the watchdog could not be told about a group");
        }
    }

    /// Records a live group.
    fn register(&self, pgid: u32) {
        guard(&self.live).insert(pgid);
        self.changed.notify_all();
        self.tell("add", pgid);
    }

    /// Forgets a reaped group.
    fn forget(&self, pgid: u32) {
        self.tell("del", pgid);
        guard(&self.live).remove(&pgid);
        self.changed.notify_all();
    }

    /// Sends `signal` to the whole group `pgid`.
    fn signal(&self, pgid: u32, signal: &str) {
        let outcome = Command::new("kill")
            .args(["-s", signal, "--"])
            .arg(format!("-{pgid}"))
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if let Err(error) = outcome {
            lgwks_std::trace::debug!(%error, pgid, signal, "a group could not be signalled");
        }
    }

    /// Runs `work`, which owns the child `pgid` leads, bounded by [`RUN_BOUND`].
    /// `None` when the bound passed and the group had to be stopped.
    fn supervise<T: Send>(&self, pgid: u32, work: impl FnOnce() -> io::Result<T> + Send) -> Option<io::Result<T>> {
        self.register(pgid);
        let slot: Mutex<Option<io::Result<T>>> = Mutex::new(None);
        let ready = Condvar::new();
        let (slot_ref, ready_ref) = (&slot, &ready);
        let outcome = thread::scope(|scope| {
            scope.spawn(move || {
                let result = work();
                *guard(slot_ref) = Some(result);
                ready_ref.notify_all();
            });
            if wait_filled(slot_ref, ready_ref, RUN_BOUND) {
                return guard(slot_ref).take();
            }
            self.signal(pgid, "TERM");
            if !wait_filled(slot_ref, ready_ref, GRACE) {
                self.signal(pgid, "KILL");
                if !wait_filled(slot_ref, ready_ref, GRACE) {
                    lgwks_std::trace::debug!(pgid, "a stopped child never reported");
                }
            }
            None
        });
        self.forget(pgid);
        outcome
    }

    /// Runs `argv` in its own process group, its output to `out` when given.
    pub(super) fn run(&self, argv: &[String], root: &Path, env: &BTreeMap<String, String>, out: Option<&File>) -> i32 {
        if self.is_stopping() {
            return STOPPED;
        }
        let Some(program) = argv.first() else {
            return UNRUNNABLE;
        };
        let mut command = Command::new(program);
        command
            .args(argv.iter().skip(1))
            .current_dir(root)
            .env_clear()
            .envs(env)
            .process_group(0);
        if let Some(file) = out {
            match (file.try_clone(), file.try_clone()) {
                (Ok(stdout), Ok(stderr)) => {
                    command.stdout(Stdio::from(stdout)).stderr(Stdio::from(stderr));
                }
                (Err(error), _) | (_, Err(error)) => {
                    lgwks_std::trace::debug!(%error, "a lane's log could not be shared with its child");
                    return UNRUNNABLE;
                }
            }
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                lgwks_std::trace::debug!(%error, program, "a child could not be started");
                return UNRUNNABLE;
            }
        };
        let pgid = child.id();
        match self.supervise(pgid, move || child.wait()) {
            Some(Ok(status)) => code_of(status),
            Some(Err(error)) => {
                lgwks_std::trace::debug!(%error, program, "a child could not be waited on");
                UNRUNNABLE
            }
            None => UNRUNNABLE,
        }
    }

    /// Runs `argv` in its own process group and captures what it prints.
    pub(super) fn capture(&self, argv: &[String], root: &Path, env: &BTreeMap<String, String>) -> Captured {
        let refused = |why: String| Captured {
            code: UNRUNNABLE,
            stdout: String::new(),
            stderr: why,
        };
        if self.is_stopping() {
            return refused("the gate is stopping".to_owned());
        }
        let Some(program) = argv.first() else {
            return refused("no command".to_owned());
        };
        let spawned = Command::new(program)
            .args(argv.iter().skip(1))
            .current_dir(root)
            .env_clear()
            .envs(env)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let child: Child = match spawned {
            Ok(child) => child,
            Err(error) => return refused(error.to_string()),
        };
        let pgid = child.id();
        match self.supervise(pgid, move || child.wait_with_output()) {
            Some(Ok(output)) => from_output(&output),
            Some(Err(error)) => refused(error.to_string()),
            None => refused("timed out".to_owned()),
        }
    }

    /// Stops every live group: `TERM`, a grace period, then `KILL`.
    pub(super) fn stop_all(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        let first: Vec<u32> = guard(&self.live).iter().copied().collect();
        for pgid in first {
            self.signal(pgid, "TERM");
        }
        self.wait_empty();
        let rest: Vec<u32> = guard(&self.live).iter().copied().collect();
        for pgid in rest {
            self.signal(pgid, "KILL");
        }
        self.wait_empty();
    }

    /// Waits up to [`GRACE`] for the live set to empty.
    fn wait_empty(&self) {
        let held = guard(&self.live);
        if self.changed.wait_timeout_while(held, GRACE, |live| !live.is_empty()).is_err() {
            lgwks_std::trace::debug!("the live set was poisoned while waiting");
        }
    }

    /// Ends the watchdog: nothing is left for it to stop.
    pub(super) fn finish(&self) {
        let mut running = guard(&self.watchdog);
        if let Some(mut dog) = running.take() {
            if let Err(error) = writeln!(dog.input, "quit 0").and_then(|()| dog.input.flush()) {
                lgwks_std::trace::debug!(%error, "the watchdog could not be told to quit");
            }
            drop(dog.input);
            if let Err(error) = dog.child.wait() {
                lgwks_std::trace::debug!(%error, "the watchdog could not be reaped");
            }
        }
    }

    /// Waits until `count` groups are live; for tests that must not race a start.
    #[cfg(test)]
    pub(super) fn wait_until_live(&self, count: usize) {
        let held = guard(&self.live);
        if self.changed.wait_while(held, |live| live.len() < count).is_err() {
            lgwks_std::trace::debug!("the live set was poisoned while waiting");
        }
    }
}

/// A captured run's code and text.
fn from_output(output: &Output) -> Captured {
    Captured {
        code: code_of(output.status),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// Starts the watchdog.
fn start_watchdog() -> Option<Watchdog> {
    let spawned = Command::new("bash")
        .arg("-c")
        .arg(WATCHDOG)
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match spawned {
        Ok(mut child) => child.stdin.take().map(|input| Watchdog { child, input }),
        Err(error) => {
            lgwks_std::trace::debug!(%error, "no watchdog could be started");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Children, code_of};
    use std::collections::BTreeMap;
    use std::error::Error;
    use std::path::Path;
    use std::thread;

    type TestResult = Result<(), Box<dyn Error>>;

    /// A command line from string slices.
    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_owned()).collect()
    }

    /// The environment a child needs to find `sh` and `sleep`.
    fn env() -> BTreeMap<String, String> {
        std::env::vars().collect()
    }

    #[test]
    fn a_child_reports_its_own_exit_code() {
        let children = Children::new(false);
        let code = children.run(&argv(&["sh", "-c", "exit 3"]), Path::new("."), &env(), None);
        assert_eq!(code, 3, "the code is the child's");
    }

    #[test]
    fn a_missing_program_is_unrunnable_not_a_pass() {
        let children = Children::new(false);
        let code = children.run(&argv(&["definitely-not-a-real-program"]), Path::new("."), &env(), None);
        assert_eq!(code, 125, "a child that cannot start is 125");
    }

    #[test]
    fn captured_output_and_code_come_back_together() {
        let children = Children::new(false);
        let done = children.capture(&argv(&["sh", "-c", "echo out; echo err >&2; exit 4"]), Path::new("."), &env());
        assert_eq!(done.code, 4, "the code");
        assert_eq!(done.stdout.trim(), "out", "stdout");
        assert_eq!(done.stderr.trim(), "err", "stderr");
    }

    #[test]
    fn stopping_the_gate_stops_what_it_started() -> TestResult {
        let children = Children::new(false);
        let code = thread::scope(|scope| -> Result<i32, Box<dyn Error>> {
            let runner = scope.spawn(|| children.run(&argv(&["sh", "-c", "exec sleep 60"]), Path::new("."), &env(), None));
            children.wait_until_live(1);
            children.stop_all();
            runner.join().map_err(|_| "the runner panicked".into())
        })?;
        assert_eq!(code, 143, "TERM reads as 128 + 15");
        assert_eq!(children.run(&argv(&["true"]), Path::new("."), &env(), None), 130, "nothing starts after a stop");
        Ok(())
    }

    #[test]
    fn a_signal_death_reads_as_128_plus_the_signal() -> TestResult {
        let done = std::process::Command::new("sh").args(["-c", "kill -s KILL $$"]).status()?;
        assert_eq!(code_of(done), 137, "KILL is 9");
        Ok(())
    }
}
