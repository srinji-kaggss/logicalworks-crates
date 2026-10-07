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
//! and no amount of signalling the old group reaches it. That is what the kernel
//! means by a group, so the supervisor does not rely on the group alone: while the
//! leader is alive it captures every process descended from it
//! (`lgwks_std::process::capture_descendants`) and signals each captured pid. The
//! escapee is still a *descendant* after it leaves the *group*, so the capture
//! names it and a pid signal stops it (#263). The receipt and its containment
//! report are the thing under test, against a real escape.
//!
//! Every probe here is observation-only. `kill -0` sends no signal, so a test can
//! watch a process without disturbing it, and nothing in this file signals a
//! group it does not own.
//!
//! # Why the sibling `exec`s its sleep
//!
//! Both tests start a sibling *outside* the supervisor, whose survival is the T21
//! claim. It runs `exec sleep 30`, so the pid it records is the process that holds
//! the test's output pipe. A sibling that forked `sleep` instead left it orphaned
//! when the shell was killed, still holding that pipe, and nextest rightly
//! reported both tests `leaky`: that was a real leak, not a heuristic. `stop`
//! kills every process a test started by pid on every exit path, including a
//! failed assertion, and nothing here is `#[ignore]`d or retried.

#![cfg(all(
    unix,
    feature = "rt",
    feature = "time",
    feature = "sync",
    feature = "process"
))]

use crate::scratch::Scratch;

use std::time::Duration;

use lgwks_bot::Runtime;
use lgwks_bot::rt::process::ProcessSpec;
use lgwks_bot::rt::supervise::{
    CleanupReceipt, ContainmentMechanism, ResidualRisk, Supervisor, TaskOutcome,
};

// The pid-file scratch directory, the signal-free liveness probes and the
// session escape are shared with the other process tests, so there is one copy
// of each rather than one per file that could drift on what a "cleanup" means.
use crate::process_probe;

use process_probe::{
    escape_command, escape_unavailable_reason, pid_is_alive, sources_are_apostrophe_free,
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
fn stop(pids: &[i32]) {
    for &pid in pids {
        process_probe::kill_pid(pid);
        let _ignored = wait_for_pid_gone(pid, Duration::from_secs(2));
    }
}

/// T21, escape half: a child that leaves the process group is captured while its
/// leader lives and stopped by pid, and an unrelated sibling is never signalled.
///
/// The escape is a real `setsid`, run by whichever interpreter this host has. The
/// pid is recorded *after* `setsid` returns, so the pid this test holds is a
/// process that has already left the group rather than one about to: a
/// descendant that recorded its pid first would still be a group member, and the
/// test would be measuring the ordinary group kill rather than the escape.
///
/// The claim under test is #263's: a group signal cannot reach the escapee, so
/// only the capture can, and a `CleanupConfirmed` receipt is earned only when the
/// escapee is observed gone and the containment report names the mechanism that
/// read the tree, counts the escapee as captured, and names no survivor.
#[test]
fn a_session_escape_is_captured_and_stopped_by_pid() -> TestResult {
    assert!(
        sources_are_apostrophe_free(),
        "an escape source containing an apostrophe truncates the single-quoted wrapper, so \
         the candidate records no pid and the failure would look like a host that cannot escape"
    );
    let escape = escape_command().ok_or_else(escape_unavailable_reason)?;
    let dir = Scratch::new("escape")?;
    let escape_file = dir.path().join("escaped.pid");
    let leader_file = dir.path().join("leader.pid");
    let sibling_file = dir.path().join("sibling.pid");

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
    let (cleanup, containment, observed, escaped, leader_pid, sibling_pid, mut sibling) =
        runtime.block_on(async {
            let mut supervisor = Supervisor::default();
            supervisor.spawn_process(&shell(&escape_script)).await?;
            let escaped = wait_for_pid(&escape_file, BUDGET).ok_or_else(|| {
                std::io::Error::other("the escaping child never recorded its pid")
            })?;
            let leader_pid = wait_for_pid(&leader_file, BUDGET).ok_or_else(|| {
                std::io::Error::other("the supervised leader never recorded its pid")
            })?;

            // An unrelated process of this test's own, started outside the
            // supervisor: same user, alive throughout, in neither the supervisor's
            // group nor the escapee's.
            let sibling = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!(
                    "echo $$ > {}; exec sleep 30",
                    sibling_file.display()
                ))
                .spawn()?;
            let sibling_pid = wait_for_pid(&sibling_file, BUDGET)
                .ok_or_else(|| std::io::Error::other("the sibling never recorded its pid"))?;

            // The escape really happened, and is observed before the cleanup rather
            // than assumed by it: an escapee that had already died would make "the
            // cleanup stopped it" a claim about nothing.
            if !pid_is_alive(escaped) {
                return Err(std::io::Error::other(
                    "the escapee died before the cleanup ran, so the row would prove nothing",
                ));
            }

            // Shutdown rather than waiting for an exit: the escapee's whole purpose
            // is to *not* exit, so a drain loop waiting for its outcome would run to
            // its own budget and report "no terminal outcome" — a statement about the
            // escapee, not about cleanup. Cancelling is what makes the supervisor
            // capture the tree and signal it, and that is the event under test.
            let report = supervisor.shutdown().await;
            let outcome = report.outcomes().first();
            let cleanup = outcome.and_then(TaskOutcome::cleanup).cloned();
            let containment = outcome.and_then(TaskOutcome::containment).cloned();
            // The whole report, for a failure message: which outcome arrived and
            // what the shutdown still held is the diagnosis when a field is absent.
            let observed = format!("{report:?}");
            Ok::<_, std::io::Error>((
                cleanup,
                containment,
                observed,
                escaped,
                leader_pid,
                sibling_pid,
                sibling,
            ))
        })?;

    assert_ne!(
        escaped, leader_pid,
        "the recorded pid must be the escaping child, not the supervised leader: a \
         setsid that failed would leave them the same process"
    );
    assert!(
        pid_is_alive(sibling_pid),
        "a process outside the supervisor's group was signalled by its cleanup (T21)"
    );

    // The escapee left the group, so no group signal reached it: only the pid
    // signal the capture made possible did. `kill -0` is the observation #263's
    // acceptance names.
    let escapee_gone = wait_for_pid_gone(escaped, BUDGET).is_some();
    if !escapee_gone {
        let _reaped = sibling.kill().and_then(|()| sibling.wait()).map(|_| ());
        stop(&[escaped, sibling_pid]);
        return Err(std::io::Error::other(format!(
            "the setsid escapee is still running after cleanup, so the capture did not reach \
             it; ps for it: {}",
            process_probe::describe_pid(escaped)
        ))
        .into());
    }

    // The report behind the receipt: which mechanism read the tree, that it named
    // the escapee, and that nothing it named is still running.
    let containment = containment.ok_or_else(|| {
        std::io::Error::other(format!(
            "a supervised process reported no containment report at all: {observed}"
        ))
    })?;
    assert_ne!(
        containment.mechanism(),
        ContainmentMechanism::ProcessGroupOnly,
        "the escapee could only have been reached by reading the process table: {containment:?}"
    );
    assert!(
        containment.captured() >= 1,
        "the capture must have named the escapee: {containment:?}"
    );
    assert!(
        containment.is_complete(),
        "every captured descendant stopped and the reading was whole: {containment:?}"
    );

    // With the group gone and every captured descendant stopped, the receipt is
    // the confirmed one; anything weaker understates what was observed.
    match cleanup {
        Some(CleanupReceipt::CleanupConfirmed) => {}
        Some(other) => {
            return Err(std::io::Error::other(format!(
                "the group is gone and the escapee was stopped, so {other:?} would understate \
                 what this supervisor actually observed"
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
    stop(&[escaped, sibling_pid]);
    Ok(())
}

/// A leader that exits on its own does not claim the tree it left behind.
///
/// The leader starts a `setsid` escapee and exits 0 once the escapee has
/// recorded its pid. Its children were re-parented at that exit, so no walk from
/// the leader can name the escapee, and a report that said `is_complete()` would
/// be claiming a tree it never saw (#347). The report must instead name the
/// limit, read no table, and leave the escapee to whoever owns it — here, this
/// test, which stops it by pid on every exit path.
#[test]
fn a_leader_that_exits_on_its_own_does_not_claim_the_tree_it_left() -> TestResult {
    let escape = escape_command().ok_or_else(escape_unavailable_reason)?;
    let dir = Scratch::new("exited")?;
    let escape_file = dir.path().join("escaped.pid");

    let runtime = Runtime::new()?;
    let outcome = runtime.block_on(async {
        let mut supervisor = Supervisor::default();
        crate::rt_process::run_shell(&mut supervisor, &escape.script_then_exit(&escape_file)).await
    });
    let escaped = wait_for_pid(&escape_file, BUDGET)
        .ok_or_else(|| std::io::Error::other("the escaping child never recorded its pid"))?;
    let alive_after_cleanup = pid_is_alive(escaped);
    stop(&[escaped]);
    let outcome = outcome?;

    assert!(
        matches!(outcome, TaskOutcome::Completed { .. }),
        "the leader exited 0 on its own, so its task completed: {outcome:?}"
    );
    assert!(
        alive_after_cleanup,
        "the escapee left the group before the leader exited, so no group signal reached it \
         and no capture could name it; it must still be running when the outcome arrives"
    );
    let containment = outcome.containment().ok_or_else(|| {
        std::io::Error::other(format!(
            "a supervised process reported no containment: {outcome:?}"
        ))
    })?;
    assert_eq!(
        containment.residual_risk(),
        Some(ResidualRisk::LeaderExited),
        "the report must name why it cannot account for the tree: {containment:?}"
    );
    assert_eq!(
        containment.mechanism(),
        ContainmentMechanism::ProcessGroupOnly,
        "a leader that had exited has no tree to read, so no table was read: {containment:?}"
    );
    assert!(
        !containment.is_complete(),
        "a report that never saw the tree must not claim it whole: {containment:?}"
    );
    Ok(())
}

/// T21, unrelated-process half, stated on its own.
///
/// The sibling above is asserted inside the escape test so the two claims are
/// read together. This one isolates it, because it is the half that fails first:
/// a supervisor that signalled more broadly than its own group would pass an
/// escape test by killing the escapee and fail this one. A supervisor that never
/// signalled anything would fail the `leader is gone` assertion instead, so
/// neither failure mode is silent.
#[test]
fn cleanup_never_signals_a_process_outside_the_supervisors_group_t21() -> TestResult {
    let dir = Scratch::new("unrelated")?;
    let sibling_file = dir.path().join("sibling.pid");
    let leader_file = dir.path().join("leader.pid");

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
            .arg(format!(
                "echo $$ > {}; exec sleep 30",
                sibling_file.display()
            ))
            .spawn()?;
        let sibling_pid = wait_for_pid(&sibling_file, BUDGET)
            .ok_or_else(|| std::io::Error::other("the sibling never recorded its pid"))?;

        let _report = supervisor.shutdown().await;
        Ok::<_, std::io::Error>((sibling, sibling_pid, leader))
    })?;

    assert!(
        wait_for_pid_gone(leader, BUDGET).is_some(),
        "the supervised group must be gone after shutdown, or cleanup never happened at all; \
         ps for the leader and its group (stat Z is an unreaped zombie): {}",
        process_probe::describe_pid(leader)
    );
    assert!(
        pid_is_alive(sibling_pid),
        "a process outside the supervisor's group was signalled by its cleanup (T21)"
    );

    let _reaped = sibling.kill().and_then(|()| sibling.wait()).map(|_| ());
    stop(&[sibling_pid]);
    Ok(())
}

/// Shutdown waits out a supervised process's bounded cleanup rather than
/// aborting it part-way.
///
/// Answering the token *is* a process task's cleanup: captures of the process
/// table, signals, the reap and a post-reap observation. Thirty-two trees shut
/// down at once make thirty-two of those drains contend for one host, which takes
/// far longer than the 50 ms a body that ignores its token is given. Before
/// `PROCESS_CLEANUP_GRACE`, shutdown aborted the drains still running when that
/// grace ran out and reported them `Aborted` with no receipt, although each had
/// cooperated — the intermittent `process_escape` failure #263 asked to explain.
#[test]
fn shutdown_reports_every_draining_cleanup_rather_than_aborting_it() -> TestResult {
    /// Trees shut down at once: enough concurrent drains to outlast 50 ms.
    const TREES: usize = 32;
    let dir = Scratch::new("drain")?;
    let runtime = Runtime::new()?;
    let (report, children) = runtime.block_on(async {
        let mut supervisor = Supervisor::new(TREES);
        let mut files = Vec::with_capacity(TREES);
        for index in 0..TREES {
            let file = dir.path().join(format!("{index}.pid"));
            supervisor
                .spawn_process(&shell(&format!(
                    "sleep 30 & echo $! > {}; wait",
                    file.display()
                )))
                .await?;
            files.push(file);
        }
        // Every tree has its child before the shutdown, so each drain has a
        // descendant to capture and stop.
        let mut children = Vec::with_capacity(TREES);
        for file in &files {
            children.push(wait_for_pid(file, BUDGET).ok_or_else(|| {
                std::io::Error::other("a supervised tree never recorded its child")
            })?);
        }
        Ok::<_, std::io::Error>((supervisor.shutdown().await, children))
    })?;

    let aborted = report
        .outcomes()
        .iter()
        .filter(|outcome| matches!(outcome, TaskOutcome::Aborted { .. }))
        .count();
    let unreported = report
        .outcomes()
        .iter()
        .filter(|outcome| outcome.cleanup().is_none() || outcome.containment().is_none())
        .count();
    let survivors: Vec<i32> = children
        .iter()
        .copied()
        .filter(|child| wait_for_pid_gone(*child, BUDGET).is_none())
        .collect();
    stop(&survivors);
    assert_eq!(
        report.outcomes().len(),
        TREES,
        "every tree reports one outcome: {report:?}"
    );
    assert_eq!(
        aborted, 0,
        "a process task draining its cleanup cooperated and must not be reported aborted"
    );
    assert_eq!(
        unreported, 0,
        "every outcome carries its receipt and its containment report"
    );
    assert!(
        survivors.is_empty(),
        "every tree's child is stopped by its own cleanup: {survivors:?}"
    );
    Ok(())
}
