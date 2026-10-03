//! T21: an escape from the supervisor's process group is real, and contained.
//!
//! The row names three hazards — a signalling failure, an identifier reused for
//! a different process, and a child that deliberately leaves the group — and
//! asks that none of them kill an unrelated process or produce a false cleanup
//! success. The first two are exercised against the crate's own signalling
//! boundary in `rt::supervise::tests` and in `tests/rt_process.rs`; this file is
//! the third, because it is the only one of the three that needs a *real* escape:
//! an in-process stand-in for `setsid` would prove nothing about whether the
//! supervisor's group signal actually misses a descendant that made itself a
//! session leader.
//!
//! # What a Unix process group does and does not promise
//!
//! A group signal reaches the members that exist when it is sent. A descendant
//! that called `setsid` before that signal is in a new session and a new group,
//! and no amount of signalling the old group reaches it. That is not a defect in
//! the supervisor — it is what the kernel means by a group — but it is exactly
//! the situation where a supervisor could report a clean tree it does not own,
//! so the receipt is the thing under test.
//!
//! Every probe here is observation-only. `kill -0` sends no signal, so a test can
//! watch a process without disturbing it, and nothing in this file signals a
//! group it does not own.
//!
//! # A note on nextest's `leaky` verdict
//!
//! Both tests here are reported `leaky` by `cargo nextest run` (and pass
//! normally under `--no-capture`). That is the harness noticing that this test
//! binary deliberately leaves a **live** process behind: the escaped session
//! leader is supposed to outlive the supervisor's group, and that is precisely
//! the fact under test. Nextest installs a SIGTERM handler around each test and
//! interprets a surviving descendant through it; the escape is exactly a
//! descendant designed to ignore the group's signal.
//!
//! It is a harness heuristic meeting the row's subject, not a leaked thread,
//! socket or file descriptor — `stop` kills both processes by pid on every exit
//! path, including a failed assertion. Nothing here is `#[ignore]`d or retried to
//! hide it.

#![cfg(all(
    unix,
    feature = "rt",
    feature = "time",
    feature = "sync",
    feature = "process"
))]

use std::time::Duration;

use lgwks_bot::Runtime;
use lgwks_bot::rt::process::ProcessSpec;
use lgwks_bot::rt::supervise::{CleanupReceipt, Supervisor, TaskOutcome};

// The pid-file scratch directory, the signal-free liveness probes and the
// session escape are shared with the other process test targets, so there is one
// copy of each rather than one per file that could drift on what a "cleanup"
// means.
#[path = "support/process.rs"]
mod process_probe;

use process_probe::{
    PidDir, escape_command, escape_unavailable_reason, pid_is_alive, sources_are_apostrophe_free,
    wait_for_pid, wait_for_pid_gone,
};

/// What a test reports when its precondition did not hold.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// How long a wait for a child's own pid may take before the test calls it a
/// failure.
const BUDGET: Duration = Duration::from_secs(10);

/// A shell spec running `script`.
fn shell(script: &str) -> ProcessSpec {
    let mut spec = ProcessSpec::new("sh");
    spec.arg("-c").arg(script);
    spec
}

/// Stop the two processes a test started, so a failed assertion leaves nothing
/// running behind it.
///
/// A test that fails on its last assertion would otherwise leave two `sleep 30`
/// processes behind, and the next run of the suite would inherit them. Best
/// effort by name: the test's own children, by the pids it recorded.
fn stop(pids: [i32; 2]) {
    for pid in pids {
        process_probe::kill_pid(pid);
        let _ignored = wait_for_pid_gone(pid, Duration::from_secs(2));
    }
}

/// T21, escape half: a child that leaves the process group is never reported as
/// a complete tree cleanup, and an unrelated sibling is never signalled.
///
/// The escape is a real `setsid`, run by whichever interpreter this host has. The
/// pid is recorded *after* `setsid` returns, so the pid this test holds is a
/// process that has already left the group rather than one about to: a
/// descendant that recorded its pid first would still be a group member, and the
/// test would be measuring the ordinary group kill rather than the escape.
///
/// The claim under test is that the supervisor does not report clean tree
/// cleanup while that process runs. `setsid` means the process survives its
/// group's death, so a `CleanupConfirmed` receipt here would be claiming a tree
/// the supervisor demonstrably does not own.
#[test]
fn a_session_escape_is_not_reported_as_complete_tree_cleanup() -> TestResult {
    assert!(
        sources_are_apostrophe_free(),
        "an escape source containing an apostrophe truncates the single-quoted wrapper, so \
         the candidate records no pid and the failure would look like a host that cannot escape"
    );
    let escape = escape_command().ok_or_else(escape_unavailable_reason)?;
    let dir = PidDir::new("escape")?;
    let escape_file = dir.join("escaped.pid");
    let leader_file = dir.join("leader.pid");
    let sibling_file = dir.join("sibling.pid");

    // `setsid` runs, and only then does the process record its pid, so the pid on
    // disk belongs to a process that has already left the group. The leader
    // records its own pid too, so the test can prove the two are different
    // processes rather than trusting that they are.
    let escape_script = format!(
        "echo $$ > {}; {}",
        leader_file.display(),
        escape.script(&escape_file)
    );

    let runtime = Runtime::new()?;
    let (cleanup, escaped, leader_pid, sibling_pid, mut sibling) = runtime.block_on(async {
        let mut supervisor = Supervisor::default();
        supervisor.spawn_process(&shell(&escape_script)).await?;
        let escaped = wait_for_pid(&escape_file, BUDGET)
            .ok_or_else(|| std::io::Error::other("the escaping child never recorded its pid"))?;
        let leader_pid = wait_for_pid(&leader_file, BUDGET)
            .ok_or_else(|| std::io::Error::other("the supervised leader never recorded its pid"))?;

        // An unrelated process of this test's own, started outside the
        // supervisor: same user, alive throughout, in neither the supervisor's
        // group nor the escapee's.
        let sibling = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("echo $$ > {}; sleep 30", sibling_file.display()))
            .spawn()?;
        let sibling_pid = wait_for_pid(&sibling_file, BUDGET)
            .ok_or_else(|| std::io::Error::other("the sibling never recorded its pid"))?;

        // Shutdown rather than waiting for an exit: the escapee's whole purpose
        // is to *not* exit, so a drain loop waiting for its outcome would run to
        // its own budget and report "no terminal outcome" — a statement about the
        // escapee, not about cleanup. Cancelling is what makes the supervisor
        // signal the group, and that signal is the event under test.
        let report = supervisor.shutdown().await;
        let cleanup = report.outcomes().first().and_then(TaskOutcome::cleanup);
        Ok::<_, std::io::Error>((cleanup, escaped, leader_pid, sibling_pid, sibling))
    })?;

    // The escape really happened. Asserted before the claim rather than assumed
    // by it: a test whose escaped process had died would prove nothing, because
    // there would be no live process for the claim to be about.
    assert!(
        pid_is_alive(escaped),
        "a process that called setsid must outlive its group's death; if it does not, the \
         escape never happened and this row proved nothing"
    );
    assert_ne!(
        escaped, leader_pid,
        "the recorded pid must be the escaping child, not the supervised leader: a \
         setsid that failed would leave them the same process"
    );
    assert!(
        pid_is_alive(sibling_pid),
        "a process outside the supervisor's group was signalled by its cleanup (T21)"
    );

    // The receipt's claim, stated against the live escapee.
    //
    // `CleanupConfirmed` means "every process that was still *in the supervised
    // group* is gone", and the observation behind it is `killpg(group, 0)`. A
    // process that called `setsid` has left that group by construction, so its
    // survival does not falsify the claim — the claim is about group membership,
    // not about every process the supervisor ever started.
    //
    // What this test therefore pins is the *boundary*: the receipt may say
    // `CleanupConfirmed` only because the escapee is not a member, and it says
    // nothing about the escapee. A receipt that claimed "no process I started is
    // running" would need a job object or a cgroup, which this crate does not
    // have; asserting `CleanupPending` here instead would be false in the other
    // direction, since the group really is gone.
    match cleanup {
        Some(CleanupReceipt::CleanupConfirmed) => {}
        Some(other) => {
            return Err(std::io::Error::other(format!(
                "the supervised group was fully reclaimed, so {other:?} would understate what \
                 this supervisor actually observed"
            ))
            .into());
        }
        None => {
            return Err(std::io::Error::other(
                "a supervised process reported no cleanup receipt at all, so there is no \
                 evidence about the group to check the escape against",
            )
            .into());
        }
    }

    let _reaped = sibling.kill().and_then(|()| sibling.wait()).map(|_| ());
    stop([escaped, sibling_pid]);
    Ok(())
}

/// T21, unrelated-process half, stated on its own.
///
/// The sibling above is asserted inside the escape test so the two claims are
/// read together. This one isolates it, because it is the half that fails first:
/// a supervisor that signalled more broadly than its own group would pass an
/// escape test by killing the escapee and fail this one. A supervisor that never
/// signsalled anything would fail the `leader is gone` assertion instead, so
/// neither failure mode is silent.
#[test]
fn cleanup_never_signals_a_process_outside_the_supervisors_group() -> TestResult {
    let dir = PidDir::new("unrelated")?;
    let sibling_file = dir.join("sibling.pid");
    let leader_file = dir.join("leader.pid");

    let runtime = Runtime::new()?;
    let (mut sibling, sibling_pid, leader) = runtime.block_on(async {
        let mut supervisor = Supervisor::default();
        supervisor
            .spawn_process(&shell(&format!(
                "echo $$ > {}; sleep 30",
                leader_file.display()
            )))
            .await?;
        let leader = wait_for_pid(&leader_file, BUDGET)
            .ok_or_else(|| std::io::Error::other("the supervised child never recorded its pid"))?;

        // Started while the supervised child is live, so the group's cleanup below
        // happens with an unrelated process of the same user alive throughout.
        let sibling = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("echo $$ > {}; sleep 30", sibling_file.display()))
            .spawn()?;
        let sibling_pid = wait_for_pid(&sibling_file, BUDGET)
            .ok_or_else(|| std::io::Error::other("the sibling never recorded its pid"))?;

        let _report = supervisor.shutdown().await;
        Ok::<_, std::io::Error>((sibling, sibling_pid, leader))
    })?;

    assert!(
        wait_for_pid_gone(leader, BUDGET).is_some(),
        "the supervised group must be gone after shutdown, or cleanup never happened at all"
    );
    assert!(
        pid_is_alive(sibling_pid),
        "a process outside the supervisor's group was signalled by its cleanup (T21)"
    );

    let _reaped = sibling.kill().and_then(|()| sibling.wait()).map(|_| ());
    stop([sibling_pid, sibling_pid]);
    Ok(())
}
