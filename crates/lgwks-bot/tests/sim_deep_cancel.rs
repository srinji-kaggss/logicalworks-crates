//! Deterministic simulation of caller-shaped deep cancellation under an
//! external process watchdog.
//!
//! #43's repair made the waiting, dropping and destruction paths of a
//! cancellation ancestry stack-bounded at caller-shaped depth. Bounded stack
//! use cannot be inferred from a single passing deep input: a recursive shape
//! merely has to overflow before whatever depth the test happens to use. So
//! each seeded journey runs on a small, stated thread stack inside a
//! re-executed child process, and a parent with a wall-clock deadline reports a
//! stack overflow, a panic or a hang as a failure. The child cannot report its
//! own stack exhaustion — the abort is the report — so the parent is the
//! reporter.
//!
//! One seed picks the depth, a journey records the observations it made into a
//! trace, and the same seed must reproduce the same trace across two child
//! processes. A failure names the seed.
#![cfg(feature = "sync")]

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Waker};
use std::time::Duration;

use lgwks_bot::rt::sync::CancellationToken;

/// What a test reports when a precondition did not hold.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Set in the child so a re-executed binary takes the child branch.
const CHILD_ENV: &str = "LGWKS_BOT_SIM_DEEP_CANCEL_CHILD";
/// The seed the child must run.
const SEED_ENV: &str = "LGWKS_BOT_SIM_DEEP_CANCEL_SEED";
/// Where the child writes the trace of what it observed.
const TRACE_ENV: &str = "LGWKS_BOT_SIM_DEEP_CANCEL_TRACE";

/// This test's own name, for the child's `--exact` filter.
const TEST_NAME: &str = "seeded_deep_cancel_is_stack_bounded_under_a_process_watchdog";

/// The seeds this family runs; each gets two child processes.
const SEEDS: [u64; 5] = [1, 7, 42, 1_337, 65_535];

/// The stack the journey thread is given. Small on purpose, so a
/// depth-proportional stack path is the thing that fails rather than the
/// harness runnning on a default-sized stack.
const SMALL_STACK: usize = 256 * 1024;

/// How many times the parent looks at a child before declaring a hang.
const GUARD_POLLS: u32 = 600;

/// How long the parent waits between looks.
const GUARD_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The shallowest chain this family builds.
const MIN_DEPTH: usize = 20_000;

/// How much depth each successive seed adds, cycling through five values.
const DEPTH_STEP: usize = 5_000;

/// The depth a seed selects, in `MIN_DEPTH .. MIN_DEPTH + 5 * DEPTH_STEP`.
fn depth_for(seed: u64) -> usize {
    let steps = usize::try_from(seed.checked_rem(5).unwrap_or(0)).unwrap_or(0);
    steps
        .checked_mul(DEPTH_STEP)
        .and_then(|added| MIN_DEPTH.checked_add(added))
        .unwrap_or(MIN_DEPTH)
}

/// Build a `depth`-link ancestry and return its root and leaf.
fn chain_of(depth: usize) -> (CancellationToken, CancellationToken) {
    let root = CancellationToken::new();
    let mut leaf = root.clone();
    for _ in 0..depth {
        leaf = leaf.child_token();
    }
    (root, leaf)
}

/// Poll `future` exactly once, reporting whether it completed.
///
/// No runtime, no waking, no waiting: the journeys that observe a pending wait,
/// cancel, and observe it complete want the poll path alone on a stack whose
/// size the test chose. The no-op waker is correct because the test polls again
/// itself.
fn polls_ready<F: Future + ?Sized>(future: &mut Pin<Box<F>>) -> bool {
    let mut context = Context::from_waker(Waker::noop());
    future.as_mut().poll(&mut context).is_ready()
}

/// One seeded journey: four deep-cancellation shapes, recorded into a trace.
///
/// Every branch records what it observed, so the trace is a fingerprint of the
/// run rather than a stream of successes.
fn journey(seed: u64) -> String {
    let depth = depth_for(seed);
    let mut trace = format!("seed={seed} depth={depth}");

    // A: a fresh wait is pending; the root cancel resolves it.
    {
        let (root, leaf) = chain_of(depth);
        let mut wait = Box::pin(leaf.cancelled_owned());
        trace.push_str(if polls_ready(&mut wait) {
            " A:early"
        } else {
            " A:pending"
        });
        root.cancel();
        trace.push_str(if polls_ready(&mut wait) {
            " A:ready"
        } else {
            " A:stuck"
        });
        trace.push_str(if leaf.is_cancelled() {
            " A:leaf"
        } else {
            " A:missed"
        });
    }

    // B: dropping a pending wait must not cancel the chain.
    {
        let (root, leaf) = chain_of(depth);
        let mut wait = Box::pin(leaf.cancelled_owned());
        trace.push_str(if polls_ready(&mut wait) {
            " B:early"
        } else {
            " B:pending"
        });
        drop(wait);
        trace.push_str(if root.is_cancelled() {
            " B:cancelled"
        } else {
            " B:dropped"
        });
    }

    // C: a cancel at the middle reaches the leaf and leaves the root alone.
    {
        let root = CancellationToken::new();
        let mut leaf = root.clone();
        let mut middle = root.clone();
        let target = depth.checked_div(2).unwrap_or(0);
        for index in 0..depth {
            leaf = leaf.child_token();
            if index == target {
                middle = leaf.clone();
            }
        }
        middle.cancel();
        trace.push_str(if leaf.is_cancelled() {
            " C:leaf"
        } else {
            " C:missed"
        });
        trace.push_str(if root.is_cancelled() {
            " C:root"
        } else {
            " C:quiet"
        });
    }

    // D: the last owner of a deep ancestry destroys it without a nested drop
    // chain. Only the leaf is kept, so dropping it frees the whole ancestry.
    {
        let leaf = {
            let (_root, leaf) = chain_of(depth);
            leaf
        };
        drop(leaf);
        trace.push_str(" D:destroyed");
    }

    trace
}

/// Run [`journey`] on a thread with [`SMALL_STACK`] and record its trace.
///
/// The journey's own stack is what is measured, so it does not run on the
/// harness's stack. A stack overflow here aborts the whole child process, which
/// is why the parent, not this thread, is the reporter.
fn trace_on_small_stack(seed: u64) -> TestResult {
    let started = std::thread::Builder::new()
        .name(String::from("sim-deep-cancel"))
        .stack_size(SMALL_STACK)
        .spawn(move || journey(seed));
    let handle = started?;
    let trace = handle
        .join()
        .map_err(|_payload| "the deep-cancel journey panicked")?;
    let path = std::env::var(TRACE_ENV)?;
    std::fs::write(path, trace)?;
    Ok(())
}

/// Re-execute this binary as a guarded child and return the trace it wrote.
///
/// The parent enforces a deadline by killing the child: a recursive poll, drop
/// or destruction path that does not overflow may still not finish, and the
/// child cannot report a hang from the stack it was hung on.
fn run_child(seed: u64, run: u32) -> Result<String, Box<dyn std::error::Error>> {
    let executable = std::env::current_exe()?;
    let trace = std::env::temp_dir().join(format!("lgwks-bot-sim-deep-cancel-{seed}-{run}.trace"));
    discard(&trace)?;
    let mut child = std::process::Command::new(executable)
        .args([TEST_NAME, "--exact", "--nocapture"])
        .env(CHILD_ENV, "1")
        .env(SEED_ENV, seed.to_string())
        .env(TRACE_ENV, &trace)
        .spawn()?;
    for _ in 0..GUARD_POLLS {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                return Err(format!(
                    "seed {seed} run {run}: the child exited with {status}; a stack overflow or \
                     panic in the deep-cancel journey is the defect this bounds"
                )
                .into());
            }
            let recorded = std::fs::read_to_string(&trace)?;
            discard(&trace)?;
            return Ok(recorded);
        }
        pause_between_polls();
    }
    child.kill()?;
    child.wait()?;
    Err(format!(
        "seed {seed} run {run}: the child did not finish within {GUARD_POLLS} polls of \
         {GUARD_POLL_INTERVAL:?} and was killed. A journey that never returns is the defect this \
         bounds."
    )
    .into())
}

/// Remove a trace file, tolerating its absence.
///
/// Absence is the expected state before a run, so a missing file is not an
/// error; anything else is, because a trace that cannot be cleared would let a
/// previous run's success stand in for this one's.
fn discard(path: &std::path::Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Wait one poll interval without blocking an executor.
///
/// The parent branch of the watchdog is a plain process with no runtime and no
/// reactor, so the workspace's ban on `std::thread::sleep` — which exists
/// because blocking an executor thread stalls every task on it — has nothing to
/// protect here: there is no executor, and the wait is this watchdog's subject
/// rather than its scaffolding.
#[expect(
    clippy::disallowed_methods,
    reason = "the watchdog's parent branch is a plain process with no async runtime and no \
              reactor to stall; a poll interval is the wait's subject, and `rt::time::sleep` \
              cannot be awaited here"
)]
fn pause_between_polls() {
    std::thread::sleep(GUARD_POLL_INTERVAL);
}

/// The seeded family: every journey finishes on a small stack, twice, and the
/// same seed reproduces the same trace.
///
/// A child process runs the branch the parent selected: with [`CHILD_ENV`] set
/// the test body becomes the journey itself, so a stack overflow or a hang is
/// observed by the parent that spawned it rather than by the failing stack.
#[test]
fn seeded_deep_cancel_is_stack_bounded_under_a_process_watchdog() -> TestResult {
    if std::env::var_os(CHILD_ENV).is_some() {
        let seed: u64 = std::env::var(SEED_ENV)?.parse()?;
        return trace_on_small_stack(seed);
    }

    for seed in SEEDS {
        let first = run_child(seed, 1)?;
        let second = run_child(seed, 2)?;
        if first != second {
            return Err(format!(
                "seed {seed}: the trace changed between two runs,\n  first:  {first}\n  \
                 second: {second}"
            )
            .into());
        }
        for marker in ["A:ready", "B:dropped", "C:leaf", "D:destroyed"] {
            if !first.contains(marker) {
                return Err(format!("seed {seed}: trace `{first}` never reached {marker}").into());
            }
        }
    }
    Ok(())
}
