//! Matched-semantics async comparison: `lgwks_bot::rt::supervise::Supervisor`
//! against pinned raw Tokio `JoinSet` + `Semaphore` + `CancellationToken`.
//!
//! # The fairness gate, and why it exists here
//!
//! A comparison between a facade and the engine it wraps is meaningless unless
//! both sides are doing **the same work, with the same bounds, and reporting the
//! same failures**. The existing rig (`main.rs`) already gates the synchronous
//! comparison on identical effect counts *and* identical condition evaluations;
//! this binary carries the same gate for the async comparison, and adds the two
//! facts a facade comparison is usually quietly missing:
//!
//! - **Same in-flight bound.** Both sides hold a permit for a task's whole life.
//!   A `JoinSet` alone has no ceiling — spawning 10,000 futures into it is
//!   *unbounded* — so the baseline here is `JoinSet` plus a `Semaphore`, which is
//!   the closest honest raw equivalent. A baseline without the bound would be
//!   measuring "the facade is slower" while the facade is the only one enforcing
//!   anything.
//! - **Same failure and cancellation semantics.** Every task that ends is
//!   accounted the same way: a returning body is a completion, a body dropped at
//!   a suspension point is an abort, and a cancellation is a cancellation. A
//!   baseline that dropped its `JoinError` would be trivially faster and would
//!   be measuring a program that does less.
//!
//! If either side's work count or bound differs by even one, the run **aborts**
//! rather than printing a ratio. A ratio computed on unequal work is the exact
//! failure the existing rig's evaluation counter was written to catch.
//!
//! # What this rig does not claim
//!
//! - It measures *this host*, at this profile, with this toolchain. Absolute
//!   numbers here are not a cross-platform claim.
//! - It compares the facade against a hand-written raw equivalent, not against a
//!   different scheduler. There is no third engine in this comparison, so this is
//!   a cost-decomposition measurement, not a leaderboard.
//! - The bare-loop floor is a separate scenario, reported as its own line and
//!   never multiplied into the facade/baseline ratio.
//!
//! # Running it
//!
//! ```sh
//! CARGO_TARGET_DIR=/tmp/lgwks-bench-async \
//!   cargo run --release --manifest-path bench/Cargo.toml --bin lgwks-bench-async
//! ```
//!
//! `--json=<path>` writes the machine-readable record; `--rounds=<n>` sets the
//! paired-round count (default 15); `--seed=<n>` fixes the seed so an interval
//! can be reproduced exactly.

// The rig's own statistics module, included by path rather than re-declared.
//
// One copy, for the same reason `sim/seed.rs` exists once and is included by
// every `sim_*` target: a second copy of the quantile and bootstrap arithmetic
// would drift from the one the synchronous rig reports with, and a reader
// comparing the two rigs would be comparing two different statistics under the
// same name. Both binaries call every item in it, so it needs no lint exception.
#[path = "../src/stats.rs"]
mod async_stats;

use std::future::Future;
use std::time::Duration;

use lgwks_bot::rt::runtime::{Builder as RuntimeBuilder, Runtime};
pub(crate) use lgwks_std::trace::info;
// ── Allocation counting ─────────────────────────────────────────────────────

// The allocation counter, shared with `bench/` by path inclusion.
//
// It lives in its own file because a second copy of an instrument that produces
// published numbers is a second instrument, and a reader comparing the two rigs
// would be comparing two different measurements under one name. Its counters sit
// behind a flag, so a timed round pays nothing for the counter — see that file's
// module docs for why that separation is the whole point.
#[path = "../src/alloc_count.rs"]
mod alloc_count;

// ── The open-loop half ───────────────────────────────────────────────────────

// The open-loop generator, the log-bucketed recorder and the seeded model they
// are both checked against. One module, because the measurement and the model of
// the measurement share the schedule arithmetic (`intended_arrival`) and a second
// copy of that function is a second definition of what "the intended start" is.
mod openloop;

// `--flood-split`: INV-BOT-151's wall-clock half, a report beside the
// deterministic `sim_tenancy_flood` gate (#375).
mod flood;

// ── The split sections ──────────────────────────────────────────────────────

mod allocation;
mod driver;
mod engine;
mod gate;
mod inflight;
mod ladder;
mod matrix;
mod overload;
mod saturation;
mod sides;
mod support;

#[cfg(test)]
mod sim_openloop;

use crate::allocation::allocation_attribution;
use crate::engine::Admission;
use crate::inflight::in_flight_tiers;
use crate::ladder::tier_ladder;
use crate::matrix::workload_matrix;
use crate::openloop::{SimSpec, simulate};
use crate::overload::{OverloadShape, overload_and_recovery};
use crate::saturation::{DEFAULT_CAPACITY_TARGET, body_for_bound, saturation_sweep};
use crate::sides::{ScenarioResult, bare_floor, baseline_side, facade_side, measure, mutant_check};
use crate::support::{
    OVERLOAD_DEFAULT_BOUND, RecoveryLength, host_load_average, micros, print_columns, print_row,
    refuse_failed,
};
pub(crate) use crate::support::widen;

// The seeded simulation family names these at the crate root, exactly as it did
// before the split; the re-export keeps those paths resolving to the one item.
#[cfg(test)]
pub(crate) use crate::gate::SLO_P99_NANOS;
#[cfg(test)]
pub(crate) use crate::saturation::{
    GENERATOR_PLACEMENT_CEILING, MAX_OFFERED_RATE, SWEEP_MULTIPLIERS, capacity_per_second,
    ladder_clamped, sweep_arrivals, sweep_ladder,
};

/// Run one open-loop mode on a runtime of its own.
///
/// The open-loop modes drive a generator across every worker thread and are written
/// as one async body each, so each gets its own `block_on` here rather than being
/// nested inside a runtime the caller already owns: calling `block_on` from inside a
/// runtime is the documented way to deadlock a multi-threaded scheduler, and a
/// measurement rig that hangs is worse than one that refuses.
///
/// `workers` pins the scheduler's thread count and `None` takes the discovered default.
/// That is the closest this host comes to the estate's 1–2 vCPU profile, and the label
/// is deliberately narrower than that profile: see [`build_runtime`].
fn block_on_mode<F, Fut>(workers: Option<usize>, body: F) -> Result<(), Box<dyn std::error::Error>>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<(), Box<dyn std::error::Error>>>,
{
    build_runtime(workers)?.block_on(body())
}

/// The runtime this rig measures on, and what a pinned worker count does and does not mean.
///
/// **What it is:** the scheduler pinned to `workers` threads, which bounds the parallelism
/// the *work* under test can reach — the generator, every arrival's body, the drain, and
/// Tokio's timer wheel all run on those threads and nowhere else.
///
/// **What it is not, and the reason the label matters:** this is not the estate's
/// 1–2 vCPU / 1–2 GB VPS profile. A thread count bounds how much of this host runs at
/// once; a vCPU count is a property of a machine whose cores, cache, memory bandwidth and
/// scheduler are all smaller. macOS exposes no cgroup, no `taskset`, no `taskpolicy` CPU
/// set and no `cpulimit` — all four were checked on the reference host and all four are
/// absent — so no run on this host can be presented as that profile's numbers. A run with
/// `workers = 2` is reported as *two worker threads on an Apple M5 Pro*, and the README
/// says so where the figure is published.
fn build_runtime(workers: Option<usize>) -> std::io::Result<Runtime> {
    match workers {
        Some(count) => {
            let Some(count) = std::num::NonZeroUsize::new(count) else {
                let refusal = Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "lgwks-bench-async: --workers=0 would build a runtime with no worker, \
                     which cannot make progress",
                ));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build_runtime: refusing a zero worker count");
                return refusal;
            };
            RuntimeBuilder::new().worker_threads(Some(count)).build()
        }
        None => Runtime::new(),
    }
}

/// The seeded open-loop model, run over the same bounds the sweep uses at every bound
/// small enough to sweep exhaustively.
///
/// Every world is simulated twice from its own seed, and the run exits non-zero unless
/// the two agree on the trace hash *and* on every reported figure. That is the same
/// property `tests/it/sim_*` families assert in-process, checked here on the path a
/// reader runs: a generator whose numbers cannot be reproduced from a seed are numbers
/// with no provenance.
fn open_loop_simulation() -> Result<(), Box<dyn std::error::Error>> {
    info!("seeded open-loop simulation — the model the live driver is checked against");
    info!("one seed drives arrival jitter and body length; every world is replayed\n");
    print_columns(&[
        "seed",
        "bound",
        "world",
        "offered/s",
        "admitted",
        "refused",
        "p50 intended us",
        "p99 intended us",
        "p99 actual us",
        "trace hash",
    ]);

    // A permit of a 5 µs body sustains about 200,000 arrivals a second, so each
    // bound's ladder is derived from its own ceiling: a tenth of capacity (quiet), five
    // times capacity (past the knee), and the same five times on the refusal door. The
    // rates are derived rather than typed so a bound is never swept at a rate that does
    // not reach its knee — which would print a saturation arm that saturated nothing.
    const PERMITS_PER_SECOND: u64 = 200_000;
    let mut failures = Vec::new();
    for seed in [
        0x0000_0000_0000_0001_u64,
        0x5EED_0000_0000_0001,
        0xDEAD_BEEF_0000_0001,
    ] {
        for bound in [1_usize, 4, 64] {
            let permits = widen(bound);
            let ladder = [
                (
                    "quiet",
                    permits.saturating_mul(PERMITS_PER_SECOND / 10).max(1),
                    false,
                ),
                (
                    "past the knee",
                    permits.saturating_mul(PERMITS_PER_SECOND * 5),
                    false,
                ),
                (
                    "refusal door",
                    permits.saturating_mul(PERMITS_PER_SECOND * 5),
                    true,
                ),
            ];
            for (what, offered_rate, refuse) in ladder {
                let spec = SimSpec {
                    offered_rate,
                    arrivals: permits.saturating_mul(8),
                    bound,
                    service_min_nanos: 2_000,
                    service_max_nanos: 8_000,
                    jitter: true,
                    refuse_at_bound: refuse,
                    seed,
                };
                let first = simulate(&spec);
                let second = simulate(&spec);
                let replayed = first.trace_hash == second.trace_hash && first == second;
                if !replayed {
                    failures.push(format!(
                        "seed {seed} at bound {bound} at {offered_rate}/s did not replay: \
                         hash {} then {}",
                        first.trace_hash, second.trace_hash
                    ));
                }
                print_row(&[
                    format!("{seed:#x}"),
                    bound.to_string(),
                    what.to_string(),
                    offered_rate.to_string(),
                    first.admitted.to_string(),
                    first.refused.to_string(),
                    micros(first.p50_intended_nanos),
                    micros(first.p99_intended_nanos),
                    micros(first.p99_actual_nanos),
                    format!("{:#018x}", first.trace_hash),
                ]);
            }
        }
    }
    info!("");

    if !failures.is_empty() {
        let refusal = Err(refuse_failed(
            "seeded open-loop model",
            "seeded world(s) did not replay",
            &failures,
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "open_loop_simulation: returning an error to the caller");
        return refusal;
    }
    info!(
        "every seeded world replayed to the same trace hash; the p99-intended and \
         p99-actual columns are the coordinated-omission gap, measured on one world read \
         two ways"
    );
    Ok(())
}

/// Parses one numeric flag value, naming what the flag expects on a refusal.
fn number<T>(value: &str, expectation: &str) -> Result<T, String>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .map_err(|error| format!("{expectation}: {error}"))
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The report is this binary's output, and it goes through the estate's tracing
    // like every other line the workspace emits: levelled, filterable through
    // `LGWKS_LOG`, and written through a handle whose errors the subscriber owns
    // rather than through `println!`, which panics on a closed pipe.
    lgwks_std::trace::install_default("lgwks-bench-async")?;
    let mut rounds: usize = 15;
    let mut json: Option<String> = None;
    let mut alloc_report = false;
    let mut mutant = false;
    let mut tiers = false;
    let mut matrix = false;
    let mut saturation = false;
    let mut refusal = false;
    let mut inflight = false;
    let mut overload = false;
    let mut attribution = false;
    let mut simulation = false;
    let mut flood_split = false;
    let mut window_seconds: u64 = 2;
    let mut tier: Option<usize> = None;
    let mut knee: u64 = 32_768;
    let mut baseline_seconds: u64 = 4;
    let mut overload_seconds: u64 = 30;
    let mut recovery = RecoveryLength::SameAsBaseline;
    let mut overload_bound = OVERLOAD_DEFAULT_BOUND;
    let mut body_micros: u64 = 0;
    let mut capacity_target: u64 = DEFAULT_CAPACITY_TARGET;
    let mut workers: Option<usize> = None;
    // The first refusal is carried as a value and returned once after the loop,
    // so the loop is one decision per argument rather than one `?` per flag.
    let mut parsed: Result<(), String> = Ok(());
    for arg in std::env::args().skip(1) {
        if parsed.is_err() {
            break;
        }
        parsed = if let Some(value) = arg.strip_prefix("--rounds=") {
            number(value, "rounds must be a number").map(|count| rounds = count)
        } else if let Some(value) = arg.strip_prefix("--json=") {
            json = Some(value.to_string());
            Ok(())
        } else if let Some(value) = arg.strip_prefix("--window=") {
            number(value, "window must be a number of seconds")
                .map(|seconds| window_seconds = seconds)
        } else if let Some(value) = arg.strip_prefix("--tier=") {
            number(value, "tier must be a number").map(|count| tier = Some(count))
        } else if let Some(value) = arg.strip_prefix("--knee=") {
            number(value, "knee must be an offered rate").map(|rate| knee = rate)
        } else if let Some(value) = arg.strip_prefix("--baseline-seconds=") {
            number(value, "baseline-seconds must be a number")
                .map(|seconds| baseline_seconds = seconds)
        } else if let Some(value) = arg.strip_prefix("--overload-seconds=") {
            number(value, "overload-seconds must be a number")
                .map(|seconds| overload_seconds = seconds)
        } else if let Some(value) = arg.strip_prefix("--recovery-seconds=") {
            number(value, "recovery-seconds must be a number")
                .map(|seconds| recovery = RecoveryLength::Seconds(seconds))
        } else if let Some(value) = arg.strip_prefix("--body-micros=") {
            number(value, "body-micros must be a number").map(|micros| body_micros = micros)
        } else if let Some(value) = arg.strip_prefix("--capacity-target=") {
            number(value, "capacity-target must be a rate").map(|rate| capacity_target = rate)
        } else if let Some(value) = arg.strip_prefix("--bound=") {
            number(value, "bound must be a number").map(|count| overload_bound = count)
        } else if let Some(value) = arg.strip_prefix("--workers=") {
            number(value, "workers must be a number").map(|count| workers = Some(count))
        } else if let Some(arg) = arg.strip_prefix("--") {
            match arg {
                "alloc-report" => alloc_report = true,
                "mutant-check" => mutant = true,
                "tiers" => tiers = true,
                "matrix" => matrix = true,
                "saturation" => saturation = true,
                "refusal" => refusal = true,
                "inflight" => inflight = true,
                "overload" => overload = true,
                "alloc-attribution" => attribution = true,
                "sim-open-loop" => simulation = true,
                "flood-split" => flood_split = true,
                // An unknown flag is refused rather than ignored: a mistyped
                // `--saturaton` that silently ran the default suite would report a
                // number for a run nobody asked for.
                other => parsed = Err(format!("unknown flag --{other}")),
            }
            parsed
        } else {
            Ok(())
        };
    }
    if let Err(reason) = parsed {
        let refusal = Err(reason.into());
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "main: refusing an argument");
        return refusal;
    }

    // The provenance every mode's numbers need, printed before the mode's own table so a
    // pasted excerpt carries it. `available_parallelism` is what the runtime would
    // otherwise discover for itself, so the discovered case is printed as the same
    // number the scheduler would have used rather than as the word "default".
    let (load, load_source) = host_load_average();
    // Structured fields rather than an interpolated line: a value the host would not
    // report is absent from the record, not printed as a placeholder a reader could take
    // for a measurement.
    info!(
        cores = std::thread::available_parallelism()
            .ok()
            .map(std::num::NonZero::get),
        workers,
        workers_source = if workers.is_some() {
            "named"
        } else {
            "discovered (available_parallelism)"
        },
        load = load.as_deref(),
        load_source,
        "host"
    );

    if flood_split {
        // A report, never a gate: the claim's gate is `sim_tenancy_flood`, and a
        // wall-clock ratio on a shared host reads the host (#375).
        return block_on_mode(workers, || flood::flood_split(rounds));
    }

    if simulation {
        // The seeded model runs alone and exits non-zero when a replay diverges: a
        // simulation whose traces do not reproduce is a simulation whose numbers
        // cannot be attributed to a seed, and the gate on it has to be as loud as
        // the fairness gate's.
        return open_loop_simulation();
    }

    if saturation {
        return block_on_mode(workers, || {
            saturation_sweep(
                Admission::Backpressure,
                Duration::from_secs(window_seconds),
                capacity_target,
                body_micros,
                workers,
                json.as_deref(),
            )
        });
    }

    if refusal {
        return block_on_mode(workers, || {
            saturation_sweep(
                Admission::Refuse,
                Duration::from_secs(window_seconds),
                capacity_target,
                body_micros,
                workers,
                json.as_deref(),
            )
        });
    }

    if inflight {
        return block_on_mode(workers, || in_flight_tiers(tier, workers, json.as_deref()));
    }

    if overload {
        let at_bound = overload_bound;
        // The overload run names its own bound, so the body cost is resolved here rather
        // than derived by the sweep: the same `body_for_bound` arithmetic, applied once.
        let at_body = body_for_bound(at_bound, capacity_target, body_micros);
        return block_on_mode(workers, || {
            overload_and_recovery(
                OverloadShape {
                    bound: at_bound,
                    knee_rate: knee,
                    body_micros: at_body,
                    baseline_window: Duration::from_secs(baseline_seconds),
                    overload_window: Duration::from_secs(overload_seconds),
                    recovery_seconds: Duration::from_secs(recovery.seconds(baseline_seconds)),
                },
                json.as_deref(),
            )
        });
    }

    if attribution {
        return block_on_mode(workers, || allocation_attribution(json.as_deref()));
    }

    if matrix {
        // The matrix runs against a scratch directory of its own, under the
        // host's own temp root rather than a path baked into the source, so it
        // is portable and leaves nothing in the working tree.
        let dir = std::env::temp_dir().join(format!("lgwks-matrix-{}", std::process::id()));
        // `workload_matrix` drives each row with its own `block_on`, so it is
        // called *outside* a runtime rather than from inside one: nesting the
        // two is the documented way to deadlock a multi-thread scheduler.
        let outcome = workload_matrix(&build_runtime(workers)?, &dir, json.as_deref());
        drop(std::fs::remove_dir_all(&dir));
        return outcome;
    }

    if mutant {
        // The negative control runs alone and exits: a run that also produced a
        // table would leave a reader unable to tell which number the refusal
        // referred to.
        return mutant_check(&build_runtime(workers)?);
    }

    if tiers {
        return tier_ladder(&build_runtime(workers)?, json.as_deref());
    }

    info!("lgwks_bot async matched-semantics comparison (facade vs raw tokio)");
    info!("Every round is paired and gated on identical work; a mismatch aborts the run.\n");

    let scenarios: [(&str, usize, usize); 4] = [
        ("quiet-async-bot", 256, 8),
        ("high-fanout", 2048, 32),
        ("at-capacity", 512, 4),
        ("single-permit", 512, 1),
    ];

    // The bare floor is measured first, before any runtime is built, so it is
    // genuinely the same counter bump with no scheduler underneath it.
    let (floor_time, floor_units) = bare_floor(4096);

    let mut results: Vec<ScenarioResult> = Vec::new();
    if alloc_report {
        let runtime = build_runtime(workers)?;
        runtime.block_on(async {
            // Counting runs in its own process-wide window, taken strictly
            // around one round of each side and never mixed with a timed round.
            // The counter itself is a relaxed atomic add on the allocation path,
            // so counting during a timing run would attribute the counter's cost
            // to the code under test — which is how a measurement instrument ends
            // up reporting its own overhead as a result.
            alloc_count::reset();
            alloc_count::start();
            let _ = facade_side(1_024, 8).await;
            alloc_count::stop();
            let after_facade = alloc_count::snapshot();

            alloc_count::reset();
            alloc_count::start();
            let _ = baseline_side(1_024, 8).await;
            alloc_count::stop();
            let after_baseline = alloc_count::snapshot();

            // Each window was reset immediately before it was counted, so the
            // snapshot after it *is* the window's total; nothing is subtracted.
            let (facade_allocs, facade_bytes) = after_facade;
            let (baseline_allocs, baseline_bytes) = after_baseline;

            info!("allocation report (1024 tasks at bound 8, counted separately from timing):");
            info!("  facade   {facade_allocs:>8} allocations, {facade_bytes:>10} bytes");
            info!("  baseline {baseline_allocs:>8} allocations, {baseline_bytes:>10} bytes");
            let per_task_facade = facade_allocs as f64 / 1_024.0;
            let per_task_base = baseline_allocs as f64 / 1_024.0;
            info!(
                "  per task: facade {per_task_facade:.2}, baseline {per_task_base:.2} \
                 (a ratio, and an absolute; both are reported)"
            );
        });
        info!("");
    }

    let runtime = build_runtime(workers)?;
    for (name, total, bound) in scenarios {
        let result = runtime.block_on(measure(name, total, bound, rounds))?;
        results.push(result);
    }

    // The report.
    info!(
        "{:<16} {:>7} {:>7} {:>11} {:>11} {:>11} {:>11} {:>9}",
        "scenario", "tasks", "bound", "facade p50", "facade p99", "base p50", "base p99", "ratio"
    );
    let mut json_rows = String::new();
    for result in &results {
        let mut f = result.facade.clone();
        let mut b = result.baseline.clone();
        let f50 = async_stats::quantile(&mut f, 0.50);
        let f99 = async_stats::quantile(&mut f, 0.99);
        let b50 = async_stats::quantile(&mut b, 0.50);
        let b99 = async_stats::quantile(&mut b, 0.99);
        let ratio = f50 / b50;
        // Paired ratio distribution: the unit of analysis is the per-round
        // ratio, so drift that moves both legs of a round cancels.
        let paired: Vec<f64> = result
            .facade
            .iter()
            .zip(result.baseline.iter())
            .map(|(facade, base)| facade / base)
            .collect();
        let (lo, hi) = async_stats::bootstrap_median_ci(&paired, 2_000, 0.95, 0x5EED);
        let distinguishes = async_stats::distinguishes_parity(lo, hi);
        info!(
            "{:<16} {:>7} {:>7} {:>11.6} {:>11.6} {:>11.6} {:>11.6} {:>8.2}x",
            result.name, result.total, result.bound, f50, f99, b50, b99, ratio
        );
        let parity = if distinguishes {
            "distinguishes"
        } else {
            "spans parity"
        };
        info!("                 95% CI on the paired ratio: [{lo:.2}, {hi:.2}] ({parity})");
        if !json_rows.is_empty() {
            json_rows.push('\n');
        }
        json_rows.push_str(&format!(
            "{{\"scenario\":\"{}\",\"tasks\":{},\"bound\":{},\"facade_p50\":{},\
             \"facade_p99\":{},\"baseline_p50\":{},\"baseline_p99\":{},\
             \"ratio\":{},\"ci_lo\":{},\"ci_hi\":{},\"distinguishes\":{},\
             \"completed\":{},\"cancelled\":{},\"aborted\":{},\"work_units\":{}}}",
            result.name,
            result.total,
            result.bound,
            f50,
            f99,
            b50,
            b99,
            ratio,
            lo,
            hi,
            distinguishes,
            result.tally.completed,
            result.tally.cancelled,
            result.tally.aborted,
            result.tally.work_units,
        ));
    }
    info!(
        "\nbare synchronous floor (no scheduler): {floor_time:.6}s for {floor_units} units.\n\
         It is a reference line, never a multiplier on the ratio above."
    );

    if let Some(path) = json {
        // The rows are joined rather than concatenated with a separator: a
        // trailing comma is a syntax error and a missing one is a syntax error
        // too, and a results file that only parses for some row counts is a
        // record nobody can rely on.
        let rows: Vec<&str> = json_rows
            .split('\n')
            .filter(|row| !row.is_empty())
            .collect();
        let joined = rows.join(",\n");
        let body = format!(
            "{{\n\"tool\":\"lgwks-bench-async\",\n\"rounds\":{rounds},\n\
             \"fairness\":\"paired; aborted on any work-count mismatch\",\n\
             \"floor_seconds\":{floor_time},\n\"scenarios\":[\n{joined}]\n}}\n"
        );
        std::fs::write(&path, body)?;
        info!("wrote {path}");
    }
    Ok(())
}
