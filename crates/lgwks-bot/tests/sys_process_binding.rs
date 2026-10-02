//! Black-box acceptance for `Supervisor::run_process` and the `domain::sys`
//! process binding.
//!
//! `run_process` is the result-bearing counterpart of `spawn_process`: it runs
//! one child to completion under the same in-flight ceiling, process-group
//! ownership and deadline machinery, and reports what it observed. These tests
//! pin the properties that make its report usable:
//!
//! - the exit code and the signal are reported distinctly, and neither is a
//!   claim about the work the command was asked to do (T35);
//! - stdout and stderr are captured exactly up to their ceiling, a saturating
//!   child is drained and truncated rather than blocked or retained without
//!   bound, and the exact total is still reported (T05);
//! - a deadline stops the whole group and is reported as a deadline, not as a
//!   normal exit;
//! - a parent's zero exit does not fabricate tree cleanup: the descendant is
//!   gone when the report says cleanup is confirmed (T19);
//! - dropping the run future after the fork leaves no orphan (T20);
//! - the sanctioned path stays bounded under many concurrent processes;
//! - a refusal before the fork and a failure after it are different types.
//!
//! The `domain::sys::Process` half drives the same machinery through the verb
//! traits and checks the dispatch certainty it reports.
#![cfg(all(
    unix,
    feature = "rt",
    feature = "time",
    feature = "sync",
    feature = "process"
))]

use std::num::NonZeroUsize;
use std::time::Duration;

use lgwks_bot::domain::sys::{DEFAULT_CAPTURE_LIMIT, DEFAULT_DEADLINE, Process};
use lgwks_bot::rt::process::{ProcessRunError, ProcessSpec, StdioPolicy};
use lgwks_bot::rt::supervise::{CleanupReceipt, Supervisor};
use lgwks_bot::rt::time::sleep;
use lgwks_bot::{Auth, BotError, Cap, DispatchCertainty, Execute, GrantSet, Observe, Query};

// The pid-file scratch directory and the group-absence wait are shared with the
// other process test targets, so there is one copy of each.
#[path = "support/process.rs"]
mod process_probe;

use process_probe::{PidDir, drop_after_pid, read_pid, wait_group_gone};

/// What a test reports when a precondition did not hold.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// How long a liveness wait may take before the test calls it a failure.
const BUDGET: Duration = Duration::from_secs(10);

// ── Spec helpers ────────────────────────────────────────────────────────────

/// A shell spec running `script`, with stderr on stdout's stream when asked.
fn shell(script: &str) -> ProcessSpec {
    let mut spec = ProcessSpec::new("sh");
    spec.arg("-c").arg(script);
    spec
}

/// A shell spec with the given capture ceilings and no deadline.
fn captured_shell(script: &str, limit: usize) -> ProcessSpec {
    let mut spec = shell(script);
    let limit = NonZeroUsize::new(limit).unwrap_or(NonZeroUsize::MIN);
    spec.capture_stdout(limit);
    spec.capture_stderr(limit);
    spec
}

/// A `domain::sys::Process` for `script`, at the documented defaults.
fn process_for(script: &str) -> Process {
    let mut spec = shell(script);
    spec.capture_stdout(DEFAULT_CAPTURE_LIMIT);
    spec.capture_stderr(DEFAULT_CAPTURE_LIMIT);
    spec.deadline(DEFAULT_DEADLINE);
    Process::from_spec(spec)
}

/// An `Auth` covering `bot.sys`.
fn sys_auth() -> Result<Auth, BotError> {
    GrantSet::empty().grant(Cap::sys()).issue(&[Cap::sys()])
}

/// An `Auth` covering nothing, for the missing-capability case.
fn empty_auth() -> Result<Auth, BotError> {
    GrantSet::empty().issue(&[])
}

// ── run_process: exit status, streams, deadline ─────────────────────────────

#[test]
fn run_process_reports_an_exit_code_and_a_signal_distinctly() -> TestResult {
    let runtime = lgwks_bot::Runtime::new()?;
    runtime.block_on(async {
        let mut supervisor = Supervisor::new(1);

        let exited_zero = supervisor.run_process(&shell("exit 0")).await?;
        assert_eq!(
            exited_zero.exit_code(),
            Some(0),
            "exit zero is reported as exit zero, not as a completed task (T35)"
        );
        assert_eq!(exited_zero.signal(), None, "a clean exit is not a signal");
        assert!(!exited_zero.deadline_fired(), "no deadline was set");

        let exited_seven = supervisor.run_process(&shell("exit 7")).await?;
        assert_eq!(exited_seven.exit_code(), Some(7), "the code survives");

        let signalled = supervisor.run_process(&shell("kill -TERM $$")).await?;
        assert_eq!(
            signalled.exit_code(),
            None,
            "a signal death has no exit code"
        );
        assert_eq!(
            signalled.signal(),
            Some(15),
            "the terminating signal is reported, not folded into a code"
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;
    Ok(())
}

#[test]
fn run_process_captures_stdout_and_stderr_exactly_under_the_limit() -> TestResult {
    let runtime = lgwks_bot::Runtime::new()?;
    runtime.block_on(async {
        let mut supervisor = Supervisor::new(1);
        let run = supervisor
            .run_process(&captured_shell("printf abc; printf def >&2", 4096))
            .await?;
        assert_eq!(run.stdout().bytes(), b"abc", "stdout is captured verbatim");
        assert_eq!(run.stderr().bytes(), b"def", "stderr is captured verbatim");
        assert!(!run.stdout().truncated() && !run.stderr().truncated());
        assert_eq!(run.stdout().total_bytes(), 3, "the exact total is reported");
        assert_eq!(run.stderr().total_bytes(), 3, "the exact total is reported");
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;
    Ok(())
}

/// T05: a child that writes far more than the ceiling is drained and truncated,
/// not blocked, and its retained buffer never exceeds the ceiling.
#[test]
fn a_chatty_child_is_drained_and_truncated_within_its_ceiling() -> TestResult {
    const TOTAL: u64 = 50 * 1024 * 1024;
    const LIMIT: usize = 4096;
    // Two independent 50 MiB writes, one to each pipe. `/dev/zero` gives a
    // stream of one byte (`0`), so the retained head is exactly that byte and
    // the assertion can name what it kept. Fifty mebibytes through each pipe
    // proves the child is drained while it runs rather than blocking on a full
    // pipe.
    let script = "head -c 52428800 /dev/zero; head -c 52428800 /dev/zero 1>&2";
    let runtime = lgwks_bot::Runtime::new()?;
    runtime.block_on(async {
        let mut supervisor = Supervisor::new(1);
        let run = supervisor
            .run_process(&captured_shell(script, LIMIT))
            .await?;
        for (name, stream) in [("stdout", run.stdout()), ("stderr", run.stderr())] {
            assert_eq!(
                stream.bytes().len(),
                LIMIT,
                "{name} must retain exactly the first {LIMIT} bytes"
            );
            assert!(
                stream.bytes().iter().all(|byte| *byte == 0),
                "{name} must retain the head of the stream"
            );
            assert!(
                stream.truncated(),
                "{name} wrote {TOTAL} bytes, over its {LIMIT}-byte ceiling"
            );
            assert_eq!(
                stream.total_bytes(),
                TOTAL,
                "{name} must report the exact total it saw"
            );
            assert!(
                stream.retained_capacity() <= LIMIT,
                "{name} retained {} bytes of capacity, over the {LIMIT}-byte ceiling",
                stream.retained_capacity()
            );
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;
    Ok(())
}

/// T19, deadline half: a deadline stops the whole group and says so.
#[test]
fn a_deadline_kill_is_reported_as_a_deadline_and_reaps_the_group() -> TestResult {
    let dir = PidDir::new("deadline")?;
    let pid_file = dir.join("shell.pid");
    let script = format!("echo $$ > {}; sleep 60", pid_file.display());
    let mut spec = shell(&script);
    spec.deadline(Duration::from_millis(200));
    let runtime = lgwks_bot::Runtime::new()?;
    let (deadline_fired, cleanup, leader) = runtime.block_on(async {
        let mut supervisor = Supervisor::new(1);
        let run = supervisor.run_process(&spec).await?;
        let leader = read_pid(&pid_file)
            .ok_or_else(|| std::io::Error::other("the shell never recorded its pid"))?;
        Ok::<_, Box<dyn std::error::Error>>((run.deadline_fired(), run.cleanup(), leader))
    })?;
    assert!(
        deadline_fired,
        "a stop this supervisor ordered must be reported as a deadline, not as an exit"
    );
    assert_ne!(
        cleanup,
        CleanupReceipt::CleanupFailed,
        "the deadline kill is delivered and the receipt says so, not that it was refused"
    );
    assert!(
        wait_group_gone(leader, BUDGET),
        "the deadline kill must reach the whole group"
    );
    Ok(())
}

/// T19, zero-exit half: a parent that exits zero does not fabricate tree
/// cleanup while a descendant lives.
#[test]
fn a_zero_exit_reaps_a_living_descendant_before_claiming_cleanup() -> TestResult {
    let dir = PidDir::new("zero-exit")?;
    let child_file = dir.join("child.pid");
    let shell_file = dir.join("shell.pid");
    let script = format!(
        "sleep 60 & echo $! > {}; echo $$ > {}; exit 0",
        child_file.display(),
        shell_file.display()
    );
    let runtime = lgwks_bot::Runtime::new()?;
    let (exit_code, cleanup, leader, descendant) = runtime.block_on(async {
        let mut supervisor = Supervisor::new(1);
        let run = supervisor.run_process(&shell(&script)).await?;
        let descendant = read_pid(&child_file)
            .ok_or_else(|| std::io::Error::other("the descendant pid was not recorded"))?;
        let leader = read_pid(&shell_file)
            .ok_or_else(|| std::io::Error::other("the shell pid was not recorded"))?;
        Ok::<_, Box<dyn std::error::Error>>((run.exit_code(), run.cleanup(), leader, descendant))
    })?;
    assert_eq!(exit_code, Some(0), "the parent exited zero");
    assert_ne!(
        cleanup,
        CleanupReceipt::CleanupFailed,
        "cleanup must not have failed"
    );
    assert!(
        wait_group_gone(leader, BUDGET),
        "the descendant group must be gone once the report claims cleanup: descendant {descendant}"
    );
    Ok(())
}

// ── run_process: refusals before and after the fork ─────────────────────────

#[test]
fn a_program_that_cannot_start_is_a_typed_pre_fork_refusal() -> TestResult {
    let runtime = lgwks_bot::Runtime::new()?;
    runtime.block_on(async {
        let mut supervisor = Supervisor::new(1);
        match supervisor
            .run_process(&ProcessSpec::new("/nonexistent/lgwks-bot-probe"))
            .await
        {
            Err(ProcessRunError::NotStarted { source }) => assert_eq!(
                source.kind(),
                std::io::ErrorKind::NotFound,
                "a missing program must be reported as a failed start, not a run: {source}"
            ),
            other => {
                return Err(std::io::Error::other(format!(
                    "expected a pre-fork refusal, got {other:?}"
                ))
                .into());
            }
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;
    Ok(())
}

#[test]
fn a_cancelled_supervisor_refuses_before_starting_anything() -> TestResult {
    let runtime = lgwks_bot::Runtime::new()?;
    runtime.block_on(async {
        let mut supervisor = Supervisor::new(1);
        supervisor.cancel();
        match supervisor.run_process(&shell("exit 1")).await {
            Err(ProcessRunError::Refused) => {}
            other => {
                return Err(std::io::Error::other(format!(
                    "a cancelled supervisor must refuse with Refused, got {other:?}"
                ))
                .into());
            }
        }
        assert_eq!(
            supervisor.stats().spawned,
            0,
            "a refusal before the fork must start nothing"
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;
    Ok(())
}

// ── run_process: bounded concurrency and drop safety ────────────────────────

/// Two hundred processes through one supervisor with a bound of sixteen: all
/// complete, and the live count never exceeds the bound.
#[test]
fn many_processes_stay_within_the_in_flight_bound() -> TestResult {
    const PROCESSES: usize = 200;
    const BOUND: usize = 16;
    // `Stats::in_flight` counts tasks not yet reaped, which can briefly include
    // one whose permit is already released, so it is not the live count. The
    // children count themselves instead: each marks itself live, records how
    // many live markers it saw, and moves its marker out before exiting. The
    // highest record is a lower bound on the real concurrency, so it can only
    // exceed the bound if the bound was not enforced.
    let live = PidDir::new("bound-live")?;
    let seen = PidDir::new("bound-seen")?;
    let done = PidDir::new("bound-done")?;
    let script = format!(
        "touch {live}/$$; ls {live} | wc -l > {seen}/$$; sleep 0.02; mv {live}/$$ {done}/",
        live = live.join("").display(),
        seen = seen.join("").display(),
        done = done.join("").display(),
    );
    let runtime = lgwks_bot::Runtime::new()?;
    let (completed, succeeded) = runtime.block_on(async {
        let mut supervisor = Supervisor::new(BOUND);
        for _ in 0..PROCESSES {
            supervisor.spawn_process(&shell(&script)).await?;
        }
        let deadline = std::time::Instant::now()
            .checked_add(BUDGET)
            .unwrap_or_else(std::time::Instant::now);
        while supervisor.stats().in_flight() > 0 {
            supervisor.reap();
            if std::time::Instant::now() >= deadline {
                return Err(std::io::Error::other(
                    "processes did not settle within the budget",
                ));
            }
            sleep(Duration::from_millis(2)).await;
        }
        let stats = supervisor.stats();
        Ok::<_, std::io::Error>((stats.completed, stats.succeeded))
    })?;
    let mut high_water = 0_usize;
    let mut records = 0_usize;
    for entry in std::fs::read_dir(seen.join(""))? {
        let count = std::fs::read_to_string(entry?.path())?;
        high_water = high_water.max(count.trim().parse::<usize>()?);
        records = records.saturating_add(1);
    }
    assert_eq!(records, PROCESSES, "every child records what it saw");
    assert!(
        high_water <= BOUND,
        "the supervisor ran {high_water} processes at once against a bound of {BOUND}"
    );
    assert_eq!(
        completed,
        u64::try_from(PROCESSES)?,
        "every process must complete"
    );
    assert_eq!(
        succeeded,
        u64::try_from(PROCESSES)?,
        "every zero exit is a clean run"
    );
    Ok(())
}

/// T20: dropping the run future after the fork leaves no orphan.
#[test]
fn dropping_a_run_future_after_the_fork_leaves_no_orphan() -> TestResult {
    let dir = PidDir::new("drop")?;
    let pid_file = dir.join("shell.pid");
    let script = format!("echo $$ > {}; sleep 60", pid_file.display());
    let runtime = lgwks_bot::Runtime::new()?;
    // The run future is dropped the moment its child reports its pid: the fork
    // has happened, the child is asleep, and the future goes away mid-flight.
    // The outer timeout only bounds a runner that never wakes again.
    let leader = runtime.block_on(async {
        let mut supervisor = Supervisor::new(1);
        let spec = shell(&script);
        lgwks_bot::rt::time::timeout(
            BUDGET,
            drop_after_pid(supervisor.run_process(&spec), &pid_file),
        )
        .await
        .ok()
        .flatten()
    });
    let leader =
        leader.ok_or("the run future never started its child, or ended before the drop")?;
    assert!(
        wait_group_gone(leader, BUDGET),
        "dropping the run future must kill the process group it owned"
    );
    Ok(())
}

/// Dropping the run future before its first poll starts nothing at all.
#[test]
fn dropping_a_run_future_before_its_first_poll_starts_nothing() -> TestResult {
    let dir = PidDir::new("pre-poll")?;
    let marker = dir.join("marker");
    let script = format!("touch {}", marker.display());
    let mut supervisor = Supervisor::new(1);
    let spec = shell(&script);

    // Building a future and dropping it un-polled runs no code from it, so no
    // fork can have happened. The parked wait gives a mis-designed eager spawn
    // time to appear.
    let future = supervisor.run_process(&spec);
    drop(future);
    std::thread::park_timeout(Duration::from_millis(50));
    assert!(
        !marker.exists(),
        "a run future dropped before its first poll must start no process"
    );
    Ok(())
}

// ── domain::sys::Process ────────────────────────────────────────────────────

#[test]
fn execute_reports_the_exit_code_and_the_captured_streams() -> TestResult {
    let process = process_for("printf out; printf err >&2; exit 3");
    let state = lgwks_bot::block_on(async { process.execute_action((sys_auth()?, &())).await })?;
    assert_eq!(
        state.exit_code,
        Some(3),
        "the state carries the code; judging it is the caller's job (T35)"
    );
    assert!(!state.running, "a returned one-shot run is not running");
    assert_eq!(state.stdout(), "out", "stdout is reported");
    assert_eq!(state.stderr(), "err", "stderr is reported");
    assert!(!state.stdout_truncated && !state.stderr_truncated);
    assert_eq!(state.stdout_total_bytes, 3);
    assert_eq!(state.stderr_total_bytes, 3);
    assert!(!state.deadline_fired);
    Ok(())
}

#[test]
fn exit_zero_is_reported_as_zero_not_success() -> TestResult {
    let process = process_for("exit 0");
    let state = lgwks_bot::block_on(async { process.execute_action((sys_auth()?, &())).await })?;
    assert_eq!(
        state.exit_code,
        Some(0),
        "exit zero is an exit code, not a success verdict (T35)"
    );
    Ok(())
}

#[test]
fn a_signal_death_is_reported_in_the_state() -> TestResult {
    let process = process_for("kill -TERM $$");
    let state = lgwks_bot::block_on(async { process.execute_action((sys_auth()?, &())).await })?;
    assert_eq!(state.exit_code, None, "a signal has no exit code");
    assert_eq!(state.signal, Some(15), "the signal is reported");
    Ok(())
}

#[test]
fn a_deadline_is_reported_in_the_state() -> TestResult {
    let mut spec = shell("sleep 60");
    spec.capture_stdout(DEFAULT_CAPTURE_LIMIT);
    spec.capture_stderr(DEFAULT_CAPTURE_LIMIT);
    spec.deadline(Duration::from_millis(200));
    let process = Process::from_spec(spec);
    let state = lgwks_bot::block_on(async { process.execute_action((sys_auth()?, &())).await })?;
    assert!(state.deadline_fired, "the deadline stop is reported");
    assert_eq!(
        state.exit_code, None,
        "the deadline stopped the process; it did not exit"
    );
    Ok(())
}

#[test]
fn the_observed_and_query_forms_run_the_command() -> TestResult {
    let process = process_for("exit 0");
    let (observed, queried) = lgwks_bot::block_on(async {
        let observed = process.poll((sys_auth()?, ())).await?;
        let queried = process.query((sys_auth()?, &())).await?;
        Ok::<_, BotError>((observed, queried))
    })?;
    assert_eq!(
        observed.exit_code,
        Some(0),
        "poll is not a silent no-op: it runs and reports"
    );
    assert_eq!(
        queried.exit_code,
        Some(0),
        "query is not a silent no-op: it runs and reports"
    );
    Ok(())
}

#[test]
fn a_missing_program_is_a_refusal_with_nothing_run() -> TestResult {
    let process = Process::from_spec(ProcessSpec::new("/nonexistent/lgwks-bot-probe"));
    let error = lgwks_bot::block_on(async { process.execute_action((sys_auth()?, &())).await })
        .err()
        .ok_or_else(|| std::io::Error::other("a missing program must be refused"))?;
    assert_eq!(
        error.dispatch_certainty(),
        DispatchCertainty::Refused,
        "a program that never started establishes that nothing ran: {error}"
    );
    Ok(())
}

#[test]
fn a_missing_capability_is_refused_before_any_spawn() -> TestResult {
    let dir = PidDir::new("cap")?;
    let marker = dir.join("marker");
    let process = process_for(&format!("touch {}", marker.display()));
    let error = lgwks_bot::block_on(async { process.execute_action((empty_auth()?, &())).await })
        .err()
        .ok_or_else(|| std::io::Error::other("the capability check must deny"))?;
    assert!(
        matches!(error, BotError::CapabilityDenied { .. }),
        "a call without bot.sys must be refused as a capability deficit: {error}"
    );
    std::thread::park_timeout(Duration::from_millis(50));
    assert!(
        !marker.exists(),
        "the cap check must precede any spawn, so the command never ran"
    );
    Ok(())
}

/// The default constructor bounds its streams and its runtime even for a child
/// that would not bound itself.
#[test]
fn the_default_constructor_runs_a_real_child() -> TestResult {
    let process = Process::new("true");
    let state = lgwks_bot::block_on(async { process.execute_action((sys_auth()?, &())).await })?;
    assert_eq!(
        state.exit_code,
        Some(0),
        "the default constructor runs a real child"
    );
    assert!(
        !state.deadline_fired,
        "an ordinary child finishes before the default deadline"
    );
    Ok(())
}

/// Concurrent verb calls on one `Process` share its ceiling: a burst of calls
/// waits for slots instead of forking a burst of children. Each child counts
/// the live children (its own marker included) when it starts, so the highest
/// count any child printed is a lower bound on the real concurrency and can
/// only exceed the ceiling if the ceiling was not enforced.
#[test]
fn concurrent_calls_on_one_process_share_its_ceiling() -> TestResult {
    const CALLS: usize = 64;
    const CEILING: usize = 4;
    let live = PidDir::new("ceiling-live")?;
    let done = PidDir::new("ceiling-done")?;
    let live_dir = live.join("");
    let done_dir = done.join("");
    let script = format!(
        "touch {live}/$$; ls {live} | wc -l; sleep 0.2; mv {live}/$$ {done}/",
        live = live_dir.display(),
        done = done_dir.display(),
    );
    let process = process_for(&script)
        .max_concurrent(NonZeroUsize::new(CEILING).unwrap_or(NonZeroUsize::MIN));
    let auth = sys_auth()?;
    let states = lgwks_bot::block_on(lgwks_std::task::join_all(
        (0..CALLS).map(|_| process.execute_action((auth.clone(), &()))),
    ));
    let mut highest = 0_usize;
    for state in states {
        let state = state?;
        assert_eq!(
            state.exit_code,
            Some(0),
            "every call runs its child to a zero exit"
        );
        highest = highest.max(state.stdout().trim().parse::<usize>()?);
    }
    assert!(
        highest <= CEILING,
        "a child saw {highest} live children against a ceiling of {CEILING}"
    );
    assert!(
        highest >= 2,
        "the calls must overlap up to the ceiling, not run one at a time"
    );
    Ok(())
}

/// The capture builders set the data policy the runner reads, and the ceiling
/// is non-zero by its type.
#[test]
fn capture_builders_set_the_declared_policy() {
    let limit = NonZeroUsize::new(1024).unwrap_or(NonZeroUsize::MIN);
    let mut spec = ProcessSpec::new("true");
    spec.capture_stdout(limit);
    spec.capture_stderr(limit);
    assert_eq!(
        spec.stdout_policy(),
        StdioPolicy::Capture(limit),
        "capture_stdout must set the Capture policy"
    );
    assert_eq!(
        spec.stderr_policy(),
        StdioPolicy::Capture(limit),
        "capture_stderr must set the Capture policy"
    );
}
