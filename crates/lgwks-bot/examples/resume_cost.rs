//! Matched-workload measurement: `remember` with and without a durable store.
//!
//! Runs the same workload twice — once on a host with no store, once on a host
//! with a real file-backed store — and prints p50/p95/p99 for the per-step cost
//! of each, plus peak RSS for 10,000 runs through the store. This is a
//! measurement harness, not a pass/fail gate: wall time varies with the machine,
//! the shape of the distribution does not.
//!
//! ```text
//! cargo run -p lgwks_bot --features script,ephemeral --example resume_cost
//! ```
//!
//! The output is written through `std::io::Write` rather than `println!` because
//! `clippy::print_stdout` is forbidden workspace-wide with no example-only
//! carve-out; an example that writes explicitly says the same thing fallibly.

use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use lgwks_bot::script::{FlowError, Scope, remember};
use lgwks_bot::task::{Host, task};

/// How many `remember` calls each measurement performs.
const STEPS: usize = 2_000;

/// How many runs the peak-RSS measurement performs.
const RUNS: usize = 10_000;

/// A percentile of a sorted sample, by nearest rank.
///
/// Nearest rank rather than interpolation: the report then names a sample that
/// was actually observed, which is the property a reader of a latency claim
/// needs and an interpolated number does not have.
fn percentile(sorted: &[u128], per_mille: usize) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = sorted.len().saturating_mul(per_mille).div_ceil(1_000);
    sorted[rank.clamp(1, sorted.len()).saturating_sub(1)]
}

/// The percentiles of one measurement, in microseconds.
struct Summary {
    /// Median.
    p50: u128,
    /// 95th.
    p95: u128,
    /// 99th.
    p99: u128,
}

impl Summary {
    /// The percentiles of `samples`, sorted in place.
    fn of(samples: &mut [u128]) -> Self {
        samples.sort_unstable();
        Self {
            p50: percentile(samples, 500),
            p95: percentile(samples, 950),
            p99: percentile(samples, 990),
        }
    }
}

/// Report one measurement.
fn report(label: &str, summary: &Summary) {
    let mut out = std::io::stdout().lock();
    let _written = writeln!(
        out,
        "{label}: p50={}us p95={}us p99={}us",
        summary.p50, summary.p95, summary.p99
    );
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let scratch = std::env::temp_dir().join(format!("lgwks-resume-bench-{}", std::process::id()));
    std::fs::create_dir_all(&scratch)?;

    // A scratch scope built by hand, so the "no store" measurement runs the same
    // body the stored one does rather than a different body that happens to be
    // cheaper.
    let scope = Scope::root(lgwks_bot::script::Tenant::new("bench")?);
    let store_dir: PathBuf = scratch.clone();

    // Measurement 1: no store.
    let mut plain = Vec::with_capacity(STEPS);
    for index in 0..STEPS {
        let started = Instant::now();
        let outcome = lgwks_bot::block_on(remember(&scope, "plain", || async move {
            Ok::<_, FlowError>(u32::try_from(index).unwrap_or_default())
        }));
        let elapsed = started.elapsed().as_micros();
        if outcome.is_err() {
            return Err("the unstored measurement step failed".into());
        }
        plain.push(elapsed);
    }
    report("no-store", &Summary::of(&mut plain));

    // Measurement 2: the same count through a real file-backed store, each step
    // under its own run so no step replays another step's record.
    let host = Host::builder("bench")?.run_store(&store_dir)?.build()?;
    let mut stored = Vec::with_capacity(STEPS);
    for index in 0..STEPS {
        let started = Instant::now();
        let report = lgwks_bot::block_on(
            host.run(&one_step_task()?, u32::try_from(index).unwrap_or_default()),
        );
        let elapsed = started.elapsed().as_micros();
        if !report.disposition().is_success() {
            return Err(format!("the stored measurement step failed: {:?}", report.error()).into());
        }
        stored.push(elapsed);
    }
    report("stored ", &Summary::of(&mut stored));

    // Measurement 3: 10,000 runs through one store, for peak RSS.
    let before = peak_rss_bytes();
    let mass_host = Host::builder("bench")?.run_store(&store_dir)?.build()?;
    let mass_start = Instant::now();
    for index in 0..RUNS {
        let report = lgwks_bot::block_on(
            mass_host.run(&one_step_task()?, u32::try_from(index).unwrap_or_default()),
        );
        if !report.disposition().is_success() {
            return Err(format!("run {index} failed: {:?}", report.error()).into());
        }
    }
    let after = peak_rss_bytes();
    let mut out = std::io::stdout().lock();
    let _written = writeln!(
        out,
        "{RUNS} runs through one store: wall={}ms peak-rss-before={}B peak-rss-after={}B \
         delta={}B",
        mass_start.elapsed().as_millis(),
        before,
        after,
        after.saturating_sub(before)
    );
    drop(std::fs::remove_dir_all(&scratch));
    Ok(())
}

/// The named future a one-step body's task returns.
type StepFuture = std::pin::Pin<Box<dyn std::future::Future<Output = Result<u32, FlowError>>>>;

/// The task type both measurements run.
type OneStep = lgwks_bot::task::Task<fn(Scope, u32) -> StepFuture>;

/// A task whose whole body is one `remember` call.
fn one_step_task() -> Result<OneStep, FlowError> {
    task("one", boxed_body)
}

/// The one-step body behind its nameable return type.
fn boxed_body(scope: Scope, value: u32) -> StepFuture {
    Box::pin(
        async move { remember(&scope, "v", || async move { Ok::<_, FlowError>(value) }).await },
    )
}

/// Peak resident set size in bytes, or zero where the platform does not report it.
///
/// Two sources, because the measurement has to run on both a Linux CI box and a
/// developer Mac and neither `/proc` nor `ps` is present on both. Where neither
/// answers, the number is zero and the report says so rather than printing a
/// plausible-looking guess: an unmeasured RSS reported as a measurement is worse
/// than an absent one.
fn peak_rss_bytes() -> u64 {
    if let Ok(text) = std::fs::read_to_string("/proc/self/status")
        && let Some(kib) = text.lines().find_map(|line| {
            let rest = line.strip_prefix("VmHWM:")?;
            rest.split_whitespace().next()?.parse::<u64>().ok()
        })
    {
        return kib.saturating_mul(1024);
    }
    // macOS and the BSDs: `ps -o rss=` is in kilobytes.
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p"])
        .arg(std::process::id().to_string())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|text| text.trim().parse::<u64>().ok())
        .map_or(0, |kib| kib.saturating_mul(1024))
}
