//! Child processes the gate and the benchmark runner own: every child leads its
//! own process group and is recorded, so stopping the owner stops what it started.
//!
//! Before this, killing the coordinator left its nextest run building and testing
//! on, holding the target lock and the CPU the next run was measured against.
//! `std` can start a child in a new group (`CommandExt::process_group`) but
//! cannot catch a signal, so the owner that is itself killed is covered by a
//! watchdog: a small `bash` process in a group of its own that reads the groups
//! the owner started over a pipe, and stops them all when that pipe closes.
//!
//! Every child runs under a bound its caller names. A child still running at the
//! bound is stopped as a group (`TERM`, a grace period, `KILL`), and what it had
//! printed by then comes back with the [`TIMED_OUT`] code, so a caller can grade
//! the part of a run that finished.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

/// How long a group gets between `TERM` and `KILL`.
const GRACE: Duration = Duration::from_secs(10);

/// The exit code of a child that could not be started.
pub(crate) const UNRUNNABLE: i32 = 125;

/// The exit code of a child still running at its bound, as coreutils `timeout`
/// reports one.
pub(crate) const TIMED_OUT: i32 = 124;

/// The exit code of a run stopped before it started.
pub(crate) const STOPPED: i32 = 130;

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

/// What Cargo sets, by exact name, for the crate it runs: the Cargo Book's
/// "Environment variables Cargo sets for crates". `CARGO` is not listed: Cargo
/// forwards it so that a nested `cargo` finds the one that is running.
const CRATE_NAMES: [&str; 4] = [
    "CARGO_CRATE_NAME",
    "CARGO_BIN_NAME",
    "CARGO_PRIMARY_PACKAGE",
    "CARGO_TARGET_TMPDIR",
];

/// The families of those variables, one per manifest field or binary. A bare
/// `CARGO_TARGET_` is not here: `CARGO_TARGET_DIR` and the per-triple linker and
/// runner variables are configuration, and a child keeps configuration.
const CRATE_PREFIXES: [&str; 3] = ["CARGO_PKG_", "CARGO_MANIFEST_", "CARGO_BIN_EXE_"];

/// Whether `name` describes the crate Cargo launched the owner for.
fn describes_the_launched_crate(name: &str) -> bool {
    CRATE_NAMES.contains(&name) || CRATE_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

/// The environment the owner was started in, as text.
pub(crate) fn inherited_env() -> BTreeMap<String, String> {
    std::env::vars_os()
        .map(|entry| {
            (
                entry.0.to_string_lossy().into_owned(),
                entry.1.to_string_lossy().into_owned(),
            )
        })
        .collect()
}

/// Removes from `env` what `cargo run` exports to describe the crate it
/// launched the owner for. A nested `cargo` takes it for its own, and Cargo
/// fingerprints those values, so every crate that reads one is rebuilt.
pub(crate) fn forget_launched_crate(env: &mut BTreeMap<String, String>) {
    env.retain(|name, _| !describes_the_launched_crate(name));
}

/// One child to run.
pub(crate) struct Launch<'run> {
    /// The program and its arguments.
    pub(crate) argv: &'run [String],
    /// The directory it runs in.
    pub(crate) root: &'run Path,
    /// The whole environment it sees.
    pub(crate) env: &'run BTreeMap<String, String>,
    /// What it reads as standard input, when the caller names a file.
    pub(crate) stdin: Option<&'run File>,
    /// The longest it may run.
    pub(crate) bound: Duration,
}

/// What a supervised child came to.
pub(crate) struct Captured {
    /// The exit code; [`TIMED_OUT`] when the bound stopped it.
    pub(crate) code: i32,
    /// Standard output, whole or as far as it got.
    pub(crate) stdout: String,
    /// Standard error, whole or as far as it got.
    pub(crate) stderr: String,
}

/// Locks `mutex`, taking the data from a poisoned one: the registry holds plain
/// ids, which a panicking holder cannot leave half-written.
pub(crate) fn guard<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
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

/// The signal that ended a process, where the platform has signals.
#[cfg(unix)]
fn signal_of(status: ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

/// The signal that ended a process, where the platform has signals.
#[cfg(not(unix))]
fn signal_of(_status: ExitStatus) -> Option<i32> {
    None
}

/// Makes `command` lead a process group of its own, where the platform has groups.
#[cfg(unix)]
fn lead_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

/// Makes `command` lead a process group of its own, where the platform has groups.
#[cfg(not(unix))]
fn lead_group(_command: &mut Command) {}

/// The exit code a status stands for; a signal reads as `128 + signal`.
fn code_of(status: ExitStatus) -> i32 {
    match status.code() {
        Some(code) => code,
        None => match signal_of(status) {
            Some(signal) => 128_i32.saturating_add(signal),
            None => UNRUNNABLE,
        },
    }
}

/// What supervising one child produced.
struct Supervised<T> {
    /// What the work returned, when it returned.
    result: Option<io::Result<T>>,
    /// Whether the bound passed and the group had to be stopped.
    timed_out: bool,
}

/// Every child the owner has started and not yet reaped.
pub(crate) struct Children {
    /// The process-group ids of the live children.
    live: Mutex<BTreeSet<u32>>,
    /// Signalled whenever `live` shrinks.
    changed: Condvar,
    /// Set once the owner is stopping: no new child starts after it.
    stopping: AtomicBool,
    /// The watchdog, when one is running.
    watchdog: Mutex<Option<Watchdog>>,
}

/// The process that stops the owner's groups if the owner dies.
struct Watchdog {
    /// The process, reaped by [`Children::finish`].
    child: Child,
    /// Where the groups are reported.
    input: ChildStdin,
}

impl Children {
    /// A registry; with `watch`, a watchdog covers the owner's own death.
    pub(crate) fn new(watch: bool) -> Self {
        let running = if watch { start_watchdog() } else { None };
        Self {
            live: Mutex::new(BTreeSet::new()),
            changed: Condvar::new(),
            stopping: AtomicBool::new(false),
            watchdog: Mutex::new(running),
        }
    }

    /// Whether the owner is stopping.
    pub(crate) fn is_stopping(&self) -> bool {
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

    /// Runs `work`, which owns the child `pgid` leads, for at most `bound`.
    fn supervise<T: Send>(
        &self,
        pgid: u32,
        bound: Duration,
        work: impl FnOnce() -> io::Result<T> + Send,
    ) -> Supervised<T> {
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
            if wait_filled(slot_ref, ready_ref, bound) {
                return Supervised {
                    result: guard(slot_ref).take(),
                    timed_out: false,
                };
            }
            self.signal(pgid, "TERM");
            if !wait_filled(slot_ref, ready_ref, GRACE) {
                self.signal(pgid, "KILL");
                if !wait_filled(slot_ref, ready_ref, GRACE) {
                    lgwks_std::trace::debug!(pgid, "a stopped child never reported");
                }
            }
            Supervised {
                result: guard(slot_ref).take(),
                timed_out: true,
            }
        });
        self.forget(pgid);
        outcome
    }

    /// The command `launch` describes, leading a group of its own.
    fn command_of(launch: &Launch<'_>) -> Option<Command> {
        let program = launch.argv.first()?;
        let mut command = Command::new(program);
        command
            .args(launch.argv.iter().skip(1))
            .current_dir(launch.root)
            .env_clear()
            .envs(launch.env);
        lead_group(&mut command);
        Some(command)
    }

    /// Runs `launch` in its own process group, its output to `out` when given.
    pub(crate) fn run(&self, launch: &Launch<'_>, out: Option<&File>) -> i32 {
        if self.is_stopping() {
            return STOPPED;
        }
        let Some(mut command) = Self::command_of(launch) else {
            return UNRUNNABLE;
        };
        if let Some(file) = launch.stdin {
            match file.try_clone() {
                Ok(input) => {
                    command.stdin(Stdio::from(input));
                }
                Err(error) => {
                    lgwks_std::trace::debug!(%error, "a child's input could not be shared with it");
                    return UNRUNNABLE;
                }
            }
        }
        if let Some(file) = out {
            match (file.try_clone(), file.try_clone()) {
                (Ok(stdout), Ok(stderr)) => {
                    command
                        .stdout(Stdio::from(stdout))
                        .stderr(Stdio::from(stderr));
                }
                (Err(error), _) | (_, Err(error)) => {
                    lgwks_std::trace::debug!(%error, "a log could not be shared with its child");
                    return UNRUNNABLE;
                }
            }
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                lgwks_std::trace::debug!(%error, argv = ?launch.argv, "a child could not be started");
                return UNRUNNABLE;
            }
        };
        let pgid = child.id();
        let supervised = self.supervise(pgid, launch.bound, move || child.wait());
        if supervised.timed_out {
            return TIMED_OUT;
        }
        match supervised.result {
            Some(Ok(status)) => code_of(status),
            Some(Err(error)) => {
                lgwks_std::trace::debug!(%error, argv = ?launch.argv, "a child could not be waited on");
                UNRUNNABLE
            }
            None => UNRUNNABLE,
        }
    }

    /// Runs `launch` in its own process group and captures what it prints.
    pub(crate) fn capture(&self, launch: &Launch<'_>) -> Captured {
        let refused = |why: String| Captured {
            code: UNRUNNABLE,
            stdout: String::new(),
            stderr: why,
        };
        if self.is_stopping() {
            return refused("stopping".to_owned());
        }
        let Some(mut command) = Self::command_of(launch) else {
            return refused("no command".to_owned());
        };
        let input = match launch.stdin {
            Some(file) => match file.try_clone() {
                Ok(input) => Stdio::from(input),
                Err(error) => return refused(error.to_string()),
            },
            None => Stdio::null(),
        };
        let spawned = command
            .stdin(input)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let child: Child = match spawned {
            Ok(child) => child,
            Err(error) => return refused(error.to_string()),
        };
        let pgid = child.id();
        let supervised = self.supervise(pgid, launch.bound, move || child.wait_with_output());
        let timed_out = supervised.timed_out;
        let captured = match supervised.result {
            Some(Ok(output)) => from_output(&output),
            Some(Err(error)) => refused(error.to_string()),
            None => refused("no report".to_owned()),
        };
        if timed_out {
            Captured {
                code: TIMED_OUT,
                ..captured
            }
        } else {
            captured
        }
    }

    /// Stops every live group: `TERM`, a grace period, then `KILL`.
    pub(crate) fn stop_all(&self) {
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
        if self
            .changed
            .wait_timeout_while(held, GRACE, |live| !live.is_empty())
            .is_err()
        {
            lgwks_std::trace::debug!("the live set was poisoned while waiting");
        }
    }

    /// Ends the watchdog: nothing is left for it to stop.
    pub(crate) fn finish(&self) {
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
    pub(crate) fn wait_until_live(&self, count: usize) {
        let held = guard(&self.live);
        if self
            .changed
            .wait_while(held, |live| live.len() < count)
            .is_err()
        {
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
    let mut command = Command::new("bash");
    command.arg("-c").arg(WATCHDOG);
    lead_group(&mut command);
    let spawned = command
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

#[cfg(all(test, unix))]
mod tests {
    use super::{Children, Launch, TIMED_OUT, code_of};
    use std::collections::BTreeMap;
    use std::error::Error;
    use std::path::Path;
    use std::thread;
    use std::time::{Duration, Instant};

    type TestResult = Result<(), Box<dyn Error>>;

    /// The bound tests give a child that is expected to finish.
    const PLENTY: Duration = Duration::from_secs(60);

    /// The bound tests give a child that is expected to be stopped.
    const SHORT: Duration = Duration::from_millis(500);

    /// A command line from string slices.
    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_owned()).collect()
    }

    /// The environment a child needs to find `sh` and `sleep`.
    fn env() -> BTreeMap<String, String> {
        std::env::vars().collect()
    }

    /// A launch of `parts` in the current directory under `bound`.
    fn launch<'run>(
        parts: &'run [String],
        env: &'run BTreeMap<String, String>,
        bound: Duration,
    ) -> Launch<'run> {
        Launch {
            argv: parts,
            root: Path::new("."),
            env,
            stdin: None,
            bound,
        }
    }

    #[test]
    fn a_child_reports_its_own_exit_code() {
        let children = Children::new(false);
        let (parts, env) = (argv(&["sh", "-c", "exit 3"]), env());
        let code = children.run(&launch(&parts, &env, PLENTY), None);
        assert_eq!(code, 3, "the code is the child's");
    }

    #[test]
    fn a_missing_program_is_unrunnable_not_a_pass() {
        let children = Children::new(false);
        let (parts, env) = (argv(&["definitely-not-a-real-program"]), env());
        let code = children.run(&launch(&parts, &env, PLENTY), None);
        assert_eq!(code, 125, "a child that cannot start is 125");
    }

    #[test]
    fn captured_output_and_code_come_back_together() {
        let children = Children::new(false);
        let (parts, env) = (argv(&["sh", "-c", "echo out; echo err >&2; exit 4"]), env());
        let done = children.capture(&launch(&parts, &env, PLENTY));
        assert_eq!(done.code, 4, "the code");
        assert_eq!(done.stdout.trim(), "out", "stdout");
        assert_eq!(done.stderr.trim(), "err", "stderr");
    }

    #[test]
    fn a_child_past_its_bound_is_stopped_and_keeps_what_it_printed() {
        let children = Children::new(false);
        let (parts, env) = (argv(&["sh", "-c", "echo before; exec sleep 600"]), env());
        let done = children.capture(&launch(&parts, &env, SHORT));
        assert_eq!(done.code, TIMED_OUT, "the bound reads as 124");
        assert_eq!(
            done.stdout.trim(),
            "before",
            "what finished before the bound is kept"
        );
    }

    /// A grandchild that survived the group stop would hold the output pipe open
    /// for its whole 30 s, so the capture would return after 30 s, not after the
    /// half-second bound and the stop that follows it.
    #[test]
    fn a_stopped_group_takes_its_grandchildren_with_it() {
        let children = Children::new(false);
        let (parts, env) = (argv(&["sh", "-c", "sleep 30 & wait"]), env());
        let started = Instant::now();
        let done = children.capture(&launch(&parts, &env, SHORT));
        assert_eq!(done.code, TIMED_OUT, "the bound reads as 124");
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the grandchild held the pipe open past the group stop"
        );
    }

    #[test]
    fn a_child_reads_the_input_file_it_was_given() -> TestResult {
        let children = Children::new(false);
        let nonce = lgwks_std::hex::encode(lgwks_std::random::bytes::<8>()?);
        let path = std::env::temp_dir().join(format!("children-input-{nonce}"));
        std::fs::write(&path, "fed through a file\n")?;
        let file = std::fs::File::open(&path)?;
        let (parts, env) = (argv(&["cat"]), env());
        let done = children.capture(&Launch {
            argv: &parts,
            root: Path::new("."),
            env: &env,
            stdin: Some(&file),
            bound: PLENTY,
        });
        std::fs::remove_file(&path)?;
        assert_eq!(done.stdout, "fed through a file\n", "the file is its stdin");
        Ok(())
    }

    #[test]
    fn stopping_the_owner_stops_what_it_started() -> TestResult {
        let children = Children::new(false);
        let (parts, env) = (argv(&["sh", "-c", "exec sleep 60"]), env());
        let code = thread::scope(|scope| -> Result<i32, Box<dyn Error>> {
            let runner = scope.spawn(|| children.run(&launch(&parts, &env, PLENTY), None));
            children.wait_until_live(1);
            children.stop_all();
            runner.join().map_err(|_| "the runner panicked".into())
        })?;
        assert_eq!(code, 143, "TERM reads as 128 + 15");
        let quick = argv(&["true"]);
        assert_eq!(
            children.run(&launch(&quick, &env, PLENTY), None),
            130,
            "nothing starts after a stop"
        );
        Ok(())
    }

    #[test]
    fn a_signal_death_reads_as_128_plus_the_signal() -> TestResult {
        let done = std::process::Command::new("sh")
            .args(["-c", "kill -s KILL $$"])
            .status()?;
        assert_eq!(code_of(done), 137, "KILL is 9");
        Ok(())
    }
}
