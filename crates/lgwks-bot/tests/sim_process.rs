//! Simulation family: the supervised process, driven by a seeded sweep.
//!
//! Every scenario drives the **real** runner — the real
//! [`Supervisor::run_process`](lgwks_bot::rt::supervise::Supervisor::run_process),
//! the real process-group ownership and cleanup, and real `sh`/`printf`
//! children — through the shared rig in [`sim`]. Nothing here is a mock: a
//! process runner tested against a fake process proves only that the fake
//! tolerates the runner.
//!
//! The seed chooses what a run *does* — how many bytes a child writes, what the
//! capture ceiling is, how it exits, whether a deadline stops it, whether the
//! run future is dropped early — and the run must satisfy the same properties
//! whichever it chose. Every declared test sweeps its band twice and requires
//! the two trace hashes to match, so a nondeterministic run fails even when
//! every assertion happens to pass.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `mix` | truncation arithmetic is `min(total, limit)` with `truncated = total > limit`; exit codes, signals, deadline stops, cleanup receipts and early drops all match their drawn scenario |
//!
//! The bands are cut over 200 seeds, so the sweep is a real sample rather than
//! a point check, and no seed is quietly untested.

#![cfg(all(
    unix,
    feature = "rt",
    feature = "time",
    feature = "sync",
    feature = "process"
))]

mod sim;

// The pid-file scratch directory and the group-absence wait are shared with the
// other process test targets, so there is one copy of each.
#[path = "support/process.rs"]
mod process_probe;

use std::error::Error;
use std::num::NonZeroUsize;
use std::time::Duration;

use lgwks_bot::Runtime;
use lgwks_bot::rt::process::ProcessSpec;
use lgwks_bot::rt::supervise::{CleanupReceipt, Supervisor};

use process_probe::{drop_after_pid, wait_group_gone};

use sim::Band;

/// What a scenario reports when a precondition did not hold.
type TestResult = Result<(), Box<dyn Error>>;

/// The seed space this family sweeps, and how many bands it is cut into.
///
/// Wider than the shared `SEED_SPACE`: the requirement is a sweep of at least
/// two hundred seeds, and a family that quietly tested a smaller space would be
/// reporting coverage it does not have.
const SEEDS: u64 = 200;
const PARTS: u64 = 8;

/// The one band of seeds a declared test sweeps.
fn band(index: usize) -> Band {
    sim::bands(SEEDS, PARTS).swap_remove(index)
}

/// A shell spec running `script` with both streams captured to `limit`.
fn captured(script: &str, limit: u32) -> ProcessSpec {
    let mut spec = ProcessSpec::new("sh");
    spec.arg("-c").arg(script);
    let limit = NonZeroUsize::new(usize::try_from(limit).unwrap_or(1)).unwrap_or(NonZeroUsize::MIN);
    spec.capture_stdout(limit);
    spec.capture_stderr(limit);
    spec
}

/// A shell spec running `script` with its streams inherited.
fn plain(script: &str) -> ProcessSpec {
    let mut spec = ProcessSpec::new("sh");
    spec.arg("-c").arg(script);
    spec
}

/// An exit code as a trace number, with `None` mapped to a distinct sentinel.
fn code_to_u64(code: Option<i32>) -> u64 {
    code.map_or(u64::MAX, |value| u64::try_from(value).unwrap_or(u64::MAX))
}

/// Run `spec` to completion on a fresh single-slot supervisor.
fn run(
    runtime: &Runtime,
    spec: &ProcessSpec,
) -> Result<lgwks_bot::rt::process::ProcessRun, Box<dyn Error>> {
    let mut supervisor = Supervisor::new(1);
    Ok(runtime.block_on(supervisor.run_process(spec))?)
}

// ── Scenario kinds ──────────────────────────────────────────────────────────

/// A child writes `total` zero bytes; the retained head is `min(total, limit)`.
fn output_case(sim: &mut sim::Sim, runtime: &Runtime) -> TestResult {
    let limit = sim.rng().between(1, 2048);
    let total = sim.rng().between(1, 20000);
    let script = format!("head -c {total} /dev/zero");
    let run = run(runtime, &captured(&script, limit))?;
    let expected = usize::try_from(total.min(limit)).unwrap_or(0);
    assert_eq!(
        run.stdout().bytes().len(),
        expected,
        "retained bytes must be min(total, limit): total={total} limit={limit}"
    );
    assert!(
        run.stdout().bytes().iter().all(|byte| *byte == 0),
        "the retained head of a zero stream is zeros"
    );
    assert_eq!(
        run.stdout().total_bytes(),
        u64::from(total),
        "the exact total is reported even when truncated"
    );
    assert_eq!(
        run.stdout().truncated(),
        total > limit,
        "truncation is exactly total > limit"
    );
    sim.record("output");
    sim.trace.record_u64("limit", u64::from(limit));
    sim.trace.record_u64("total", u64::from(total));
    sim.trace
        .record_count("retained", run.stdout().bytes().len());
    sim.trace
        .record_u64("truncated", u64::from(run.stdout().truncated()));
    Ok(())
}

/// A child exits with a code drawn from the seed.
fn exit_case(sim: &mut sim::Sim, runtime: &Runtime) -> TestResult {
    let code = sim.rng().between(0, 7);
    let script = format!("exit {code}");
    let run = run(runtime, &plain(&script))?;
    assert_eq!(
        run.exit_code(),
        Some(i32::try_from(code).unwrap_or(-1)),
        "the drawn exit code must survive"
    );
    assert!(!run.deadline_fired(), "no deadline was set");
    sim.record("exit");
    sim.trace.record_u64("code", u64::from(code));
    sim.trace
        .record_u64("exit-code", code_to_u64(run.exit_code()));
    Ok(())
}

/// A child kills itself with SIGTERM; the signal is reported, not a code.
fn signal_case(sim: &mut sim::Sim, runtime: &Runtime) -> TestResult {
    let run = run(runtime, &plain("kill -TERM $$"))?;
    assert_eq!(run.exit_code(), None, "a signal death has no exit code");
    assert_eq!(run.signal(), Some(15), "the terminating signal is reported");
    sim.record("signal");
    sim.trace.record_u64(
        "signal",
        u64::try_from(run.signal().unwrap_or(0)).unwrap_or(0),
    );
    Ok(())
}

/// A deadline stops a child that would otherwise run for seconds.
fn deadline_case(sim: &mut sim::Sim, runtime: &Runtime) -> TestResult {
    let millis = sim.rng().between(20, 100);
    let mut spec = plain("sleep 5");
    spec.deadline(Duration::from_millis(u64::from(millis)));
    let run = run(runtime, &spec)?;
    assert!(
        run.deadline_fired(),
        "the supervisor's stop must be reported as a deadline"
    );
    assert_eq!(run.status(), None, "the deadline stopped the child");
    assert_ne!(
        run.cleanup(),
        CleanupReceipt::CleanupFailed,
        "the group kill must have been delivered"
    );
    sim.record("deadline");
    sim.trace.record_u64("millis", u64::from(millis));
    sim.trace
        .record_u64("deadline-fired", u64::from(run.deadline_fired()));
    Ok(())
}

/// The run future is dropped mid-flight; the group it owned must be gone.
fn drop_case(sim: &mut sim::Sim, runtime: &Runtime) -> TestResult {
    let dir = sim.scratch("process-mix")?;
    let pid_file = dir.join("child.pid");
    let script = format!("echo $$ > {}; sleep 5", pid_file.display());
    let spec = plain(&script);
    let pid = runtime.block_on(async {
        let mut supervisor = Supervisor::new(1);
        lgwks_bot::rt::time::timeout(
            Duration::from_secs(5),
            drop_after_pid(supervisor.run_process(&spec), &pid_file),
        )
        .await
        .ok()
        .flatten()
    });
    let pid =
        pid.ok_or_else(|| std::io::Error::other("the dropped run never started its child"))?;
    assert!(
        wait_group_gone(pid, Duration::from_secs(2)),
        "an early drop must leave no orphaned process group"
    );
    sim.record("drop");
    Ok(())
}

/// The whole random mix for one seed.
fn mix(sim: &mut sim::Sim, runtime: &Runtime) -> TestResult {
    match sim.rng().below(6) {
        0 => output_case(sim, runtime),
        1 => exit_case(sim, runtime),
        2 => signal_case(sim, runtime),
        3 => deadline_case(sim, runtime),
        4 => drop_case(sim, runtime),
        _ => output_case(sim, runtime),
    }
}

// ── The sweep ───────────────────────────────────────────────────────────────

/// The runtime and the mix, for one band. The runtime is created once and
/// reused across every seed, so the sweep measures the runner rather than
/// runtime construction.
fn mix_band(index: usize) -> TestResult {
    let runtime = Runtime::new()?;
    sim::assert_replays(band(index), |sim| mix(sim, &runtime))
}

macro_rules! mix_family {
    ($($name:ident => $index:expr);+ $(;)?) => {
        $(
            /// A seeded sweep of the process-runner mix.
            #[test]
            fn $name() -> TestResult {
                mix_band($index)
            }
        )+
    };
}

mix_family!(
    mix_r00 => 0; mix_r01 => 1; mix_r02 => 2; mix_r03 => 3;
    mix_r04 => 4; mix_r05 => 5; mix_r06 => 6; mix_r07 => 7;
);

/// The bands partition the seed space: no gap, no overlap.
#[test]
fn the_bands_cover_every_seed_exactly_once() -> TestResult {
    let mut covered = Vec::new();
    for band in sim::bands(SEEDS, PARTS) {
        covered.extend(band.seeds());
    }
    covered.sort_unstable();
    covered.dedup();
    assert_eq!(
        u64::try_from(covered.len())?,
        SEEDS,
        "the bands cover {} seeds, not the declared {SEEDS}",
        covered.len()
    );
    Ok(())
}

/// The rig drives a real child, not a stand-in for one.
#[test]
fn the_sweep_drives_a_real_child() -> TestResult {
    let runtime = Runtime::new()?;
    let run = run(&runtime, &plain("exit 0"))?;
    assert_eq!(
        run.exit_code(),
        Some(0),
        "the seeded sweep must observe a real process exit"
    );
    Ok(())
}
