//! The `process` primitives answer the same on Linux, macOS and Windows (#263).
//!
//! The Director's order of 2026-10-12 was to prove system parity on all three
//! operating systems. This module is one body of assertions that CI runs on
//! `ubuntu-latest`, `macos-latest` and `windows-latest`, each against its own
//! kernel: a process table read (`/proc`, `sysctl(KERN_PROC)`, Toolhelp32), a
//! process identity (`/proc` start tick, `kinfo_proc` start time,
//! `GetProcessTimes`), a termination (`kill(2)`, `TerminateProcess`) and the
//! exited-but-held state (a zombie, a process object a handle still holds).
//!
//! The contract each test states is the one the doc comments in
//! `lgwks_std::process` promise on every target: a non-positive id is
//! `InvalidInput` before anything is read, a vacant id is absent and not an
//! error, an exited child still holds its id until it is waited on, and a
//! recorded leader identity stops its whole tree and nothing else.
//!
//! The processes are real: parentage, an exit that has not been reaped and a
//! reissued-id check are properties of the kernel, and none is observable
//! through a stub. Waits are bounded by a deadline and poll with
//! `yield_now`, as the other process modules here do.

#![cfg(feature = "process")]

use std::error::Error;
use std::io::ErrorKind;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use lgwks_std::process::{
    DescendantSet, OrphanReap, ProcessIdentity, capture_descendants,
    child_has_exited_without_reaping, identify_process, kill_process, kill_process_group,
    process_exists, process_group_exists, reap_orphaned_group, running_processes,
};

/// What every test here returns.
type TestResult = Result<(), Box<dyn Error>>;

/// An id no supported OS hands out: above Linux's `PID_MAX_LIMIT`, macOS's
/// 99,999 and any Windows process id in use.
const VACANT: i32 = i32::MAX;

/// How long a wait for the kernel may take before the test fails.
const SETTLE: Duration = Duration::from_secs(60);

/// Poll `check` until it holds or [`SETTLE`] passes; whether it held.
fn wait_until(
    mut check: impl FnMut() -> Result<bool, Box<dyn Error>>,
) -> Result<bool, Box<dyn Error>> {
    let started = Instant::now();
    loop {
        if check()? {
            return Ok(true);
        }
        if started.elapsed() >= SETTLE {
            return Ok(false);
        }
        std::thread::yield_now();
    }
}

/// A leader with one long-lived descendant, on any OS.
fn spawn_tree() -> std::io::Result<Child> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        Command::new("sh")
            .args(["-c", "sleep 600 & wait"])
            .process_group(0)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    }
    #[cfg(windows)]
    {
        Command::new("cmd")
            .args(["/C", "ping -n 600 127.0.0.1 > NUL"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    }
}

/// A child that exits at once with success, on any OS.
fn spawn_exiting() -> std::io::Result<Child> {
    #[cfg(unix)]
    let mut command = Command::new("true");
    #[cfg(windows)]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/C", "exit 0"]);
        command
    };
    command.stdout(Stdio::null()).stderr(Stdio::null()).spawn()
}

#[test]
fn a_non_positive_id_is_invalid_input_before_anything_is_read() {
    let kinds = [
        kill_process_group(0).map_err(|error| error.kind()),
        kill_process_group(-1).map_err(|error| error.kind()),
        process_group_exists(0)
            .map(|_| ())
            .map_err(|error| error.kind()),
        child_has_exited_without_reaping(0)
            .map(|_| ())
            .map_err(|error| error.kind()),
        kill_process(0).map_err(|error| error.kind()),
        process_exists(-1).map(|_| ()).map_err(|error| error.kind()),
        capture_descendants(0)
            .map(|_| ())
            .map_err(|error| error.kind()),
        identify_process(0)
            .map(|_| ())
            .map_err(|error| error.kind()),
    ];
    for (at, kind) in kinds.into_iter().enumerate() {
        assert_eq!(
            kind,
            Err(ErrorKind::InvalidInput),
            "call {at} refuses a non-positive id"
        );
    }
}

#[test]
fn a_vacant_id_is_absent_not_an_error_and_cannot_be_signalled() -> TestResult {
    assert!(!process_exists(VACANT)?, "no process holds the id");
    assert!(!process_group_exists(VACANT)?, "and no tree is under it");
    assert_eq!(identify_process(VACANT)?, None, "so it has no identity");
    assert!(
        running_processes(&[VACANT])?.is_empty(),
        "and nothing runs under it"
    );
    assert!(kill_process(VACANT).is_err(), "a signal to nothing says so");
    assert!(
        kill_process_group(VACANT).is_err(),
        "and so does a tree kill"
    );
    assert!(
        child_has_exited_without_reaping(VACANT).is_err(),
        "an id that is not ours is an error, never a fabricated exit"
    );
    assert!(
        running_processes(&[])?.is_empty(),
        "asking about nothing answers nothing"
    );
    Ok(())
}

#[test]
fn an_exited_child_still_holds_its_id_until_it_is_waited_on() -> TestResult {
    let mut child = spawn_exiting()?;
    let pid = i32::try_from(child.id())?;
    assert!(
        wait_until(|| Ok(child_has_exited_without_reaping(pid)?))?,
        "the exit is observed without consuming it"
    );
    assert!(
        process_exists(pid)?,
        "an exited, unreaped child still holds its id"
    );
    assert!(
        running_processes(&[pid])?.is_empty(),
        "and it is not running: it does no work"
    );
    assert!(
        child.wait()?.success(),
        "the real status is still there to reap"
    );
    Ok(())
}

#[test]
fn a_recorded_leader_has_its_whole_tree_stopped_and_nothing_else() -> TestResult {
    let mut child = spawn_tree()?;
    let pid = i32::try_from(child.id())?;
    let mut seen = DescendantSet::default();
    assert!(
        wait_until(|| {
            seen.absorb(&capture_descendants(pid)?);
            Ok(!seen.is_empty())
        })?,
        "the leader's descendant is named by the process table"
    );
    assert!(
        seen.mechanism().read_a_table(),
        "the capture says it read a table"
    );
    assert!(!seen.contains(pid), "the root is not its own descendant");
    let identity = identify_process(pid)?.ok_or("a live leader has an identity")?;
    assert_eq!(identity.pid(), pid);
    assert_eq!(
        identity.to_string().parse::<ProcessIdentity>()?,
        identity,
        "the identity survives a round trip through its text"
    );
    assert!(process_exists(pid)?, "the leader holds its id");
    assert!(running_processes(&[pid])?.contains(&pid), "and runs");
    let before = seen.pids().to_vec();
    let OrphanReap::Signalled { descendants } = reap_orphaned_group(&identity)? else {
        return Err("a matching live leader is signalled".into());
    };
    for descendant in &before {
        assert!(
            descendants.contains(*descendant),
            "descendant {descendant} was captured by the reap too"
        );
    }
    assert!(
        !child.wait()?.success(),
        "the leader was stopped, not finished"
    );
    let mut everyone = before;
    everyone.push(pid);
    assert!(
        wait_until(|| Ok(running_processes(&everyone)?.is_empty()))?,
        "no member of the tree is still running"
    );
    Ok(())
}
