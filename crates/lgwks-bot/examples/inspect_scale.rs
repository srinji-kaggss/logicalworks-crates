//! Measurement harness for typed inspection (#150): hyperscale tiers and
//! allocation counters.
//!
//! Print-only, like every example. Wall-clock percentiles are measured here;
//! peak RSS is read by running the binary under `/usr/bin/time -l`:
//!
//! ```text
//! cargo build -p lgwks_bot --example inspect_scale --features inspect,script
//! /usr/bin/time -l target/debug/examples/inspect_scale tiers
//! /usr/bin/time -l target/debug/examples/inspect_scale alloc 4096
//! ```
//!
//! The output goes through `std::io::Write` rather than `println!` because
//! `clippy::print_stdout` is forbidden workspace-wide with no example-only
//! carve-out; writing fallibly says the same thing and satisfies the lint.

use std::io::Write;
use std::num::NonZeroUsize;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use lgwks_bot::domain::inspect::{InspectionJob, inspection_task};
use lgwks_bot::inspect::{Budgets, InspectRequest, inspect};
use lgwks_bot::task::Host;

/// The admission ceiling the tiers run under.
const CEILING: usize = 32;

/// The tiers the hyperscale sweep runs.
const TIERS: [usize; 3] = [100, 1_000, 10_000];

/// A one-violation subject every tier inspects.
const SUBJECT: &str = "fn f() { g().unwrap(); }\n";

fn main() -> std::io::Result<()> {
    let mode = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "tiers".to_owned());
    match mode.as_str() {
        "alloc" => {
            let bytes = std::env::args()
                .nth(2)
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(4096);
            allocation(bytes)
        }
        _ => tiers(),
    }
}

/// The `quantile`-th percentile of a sorted slice, in microseconds.
fn percentile(values: &[u64], quantile: usize) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let index = values
        .len()
        .saturating_sub(1)
        .saturating_mul(quantile)
        .checked_div(100)
        .unwrap_or(0);
    values.get(index).copied().unwrap_or(0)
}

/// Run each concurrency tier through one bounded host and print percentiles.
fn tiers() -> std::io::Result<()> {
    let mut out = std::io::stdout();
    for &jobs in &TIERS {
        let host = Host::builder("scale")
            .map_err(to_io)?
            .max_concurrent_tasks(NonZeroUsize::new(CEILING).unwrap_or(NonZeroUsize::MIN))
            .default_deadline(Duration::from_secs(120))
            .build()
            .map_err(to_io)?;
        let task = inspection_task("inspect").map_err(to_io)?;
        let next = AtomicUsize::new(0);
        let latencies: Mutex<Vec<u64>> = Mutex::new(Vec::with_capacity(jobs));

        std::thread::scope(|scope| {
            for _ in 0..CEILING {
                scope.spawn(|| {
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        if index >= jobs {
                            break;
                        }
                        let started = Instant::now();
                        if let Ok(report) =
                            host.block_on(&task, InspectionJob::new("scale.rs", SUBJECT))
                            && report.into_result().is_ok()
                            && let Ok(mut guard) = latencies.lock()
                        {
                            guard.push(
                                u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
                            );
                        }
                    }
                });
            }
        });

        let mut values = latencies.into_inner().unwrap_or_default();
        values.sort_unstable();
        writeln!(
            out,
            "tier={jobs} ceiling={CEILING} reached={} peak_in_flight={} p50={}us p95={}us p99={}us",
            values.len(),
            host.admission().peak_in_flight(),
            percentile(&values, 50),
            percentile(&values, 95),
            percentile(&values, 99),
        )?;
    }
    Ok(())
}

/// Inspect a subject of roughly `bytes` bytes and print the charged resources.
fn allocation(bytes: usize) -> std::io::Result<()> {
    let mut out = std::io::stdout();
    // One violation per line, and a line is about forty bytes, so the finding
    // count stays under `MAX_FINDINGS` across the sizes the report sweeps and
    // the retained output can be compared linearly.
    let lines = bytes.checked_div(40).unwrap_or(1).saturating_add(1);
    let mut source = String::with_capacity(bytes);
    for index in 0..lines {
        let line = format!("fn f{index}() {{ let value = input.unwrap(); }}\n");
        source.push_str(&line);
    }
    // The default findings cap (1,024) would truncate the largest sizes and
    // make the walk stop early; a budget above the density keeps every finding
    // retained so the counters reflect the whole input.
    let report = inspect(
        &InspectRequest::new("alloc.rs", &source).budgets(Budgets::new().with_findings(1_048_576)),
    );
    let resources = report.resources();
    writeln!(
        out,
        "requested={bytes} subject_bytes={} nodes={} depth={} findings={} output_bytes={}",
        source.len(),
        resources.nodes,
        resources.max_depth,
        resources.findings,
        resources.output_bytes,
    )?;
    Ok(())
}

/// The `io::Error` a `BotError` or `HostError` becomes for `main`.
fn to_io(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(error.to_string())
}
