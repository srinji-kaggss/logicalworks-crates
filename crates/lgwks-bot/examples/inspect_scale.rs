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

#[path = "../tests/support/lock.rs"]
mod lock;
#[path = "support/measure.rs"]
mod measure;

use lock::take_unpoisoned;

/// The admission ceiling the tiers run under.
const CEILING: usize = 32;

/// Subject bytes the `alloc` mode inspects when the command line names none.
const DEFAULT_ALLOC_BYTES: usize = 4_096;

/// The tiers the hyperscale sweep runs.
const TIERS: [usize; 3] = [100, 1_000, 10_000];

/// A one-violation subject every tier inspects.
const SUBJECT: &str = "fn f() { g().unwrap(); }\n";

/// Run the sweep the command line names, or the tier sweep when it names none.
///
/// A mode that is not one of the two is refused rather than folded into the
/// tier sweep: a mistyped mode that silently measures the wrong thing is the
/// defect this arm exists to remove.
fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let mode = args.next();
    match mode.as_deref() {
        Some("alloc") => {
            let requested = args.next();
            let bytes = match requested {
                Some(text) => text.parse::<usize>().map_err(to_io)?,
                None => DEFAULT_ALLOC_BYTES,
            };
            allocation(bytes)
        }
        None => tiers(),
        Some(other) => {
            let mut err = std::io::stderr().lock();
            writeln!(err, "unknown mode {other:?}; expected `tiers` or `alloc`")?;
            Err(std::io::Error::other(format!(
                "unknown mode {other:?}; expected `tiers` or `alloc`"
            )))
        }
    }
}

/// Drive one concurrency tier through a bounded host and print its percentiles.
fn one_tier(out: &mut std::io::Stdout, jobs: usize) -> std::io::Result<()> {
    let ceiling = NonZeroUsize::new(CEILING)
        .ok_or_else(|| to_io("the admission ceiling must be non-zero"))?;
    let host = Host::builder("scale")
        .map_err(to_io)?
        .max_concurrent_tasks(ceiling)
        .default_deadline(Duration::from_secs(120))
        .build()
        .map_err(to_io)?;
    let task = inspection_task("inspect").map_err(to_io)?;
    let next = AtomicUsize::new(0);
    let latencies: Mutex<Vec<u128>> = Mutex::new(Vec::with_capacity(jobs));

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
                    {
                        // The width `as_micros` reports: a per-inspection latency
                        // is microseconds, and narrowing it would need a
                        // stand-in for a span of 584 942 years.
                        let micros = started.elapsed().as_micros();
                        take_unpoisoned(&latencies).push(micros);
                    }
                }
            });
        }
    });

    let mut values = take_unpoisoned(&latencies).clone();
    let summary = measure::Summary::of(&mut values);
    writeln!(
        out,
        "{}",
        summary.line(&format!(
            "tier={jobs} ceiling={CEILING} reached={} peak_in_flight={}",
            values.len(),
            host.admission().peak_in_flight()
        ))
    )?;
    Ok(())
}

/// Run each concurrency tier through one bounded host and print percentiles.
fn tiers() -> std::io::Result<()> {
    let mut out = std::io::stdout();
    for &jobs in &TIERS {
        one_tier(&mut out, jobs)?;
    }
    Ok(())
}

/// Inspect a subject of roughly `bytes` bytes and print the charged resources.
fn allocation(bytes: usize) -> std::io::Result<()> {
    let mut out = std::io::stdout();
    // One violation per line, and a line is about forty bytes, so the finding
    // count stays under `MAX_FINDINGS` across the sizes the report sweeps and
    // the retained output can be compared linearly.
    let lines = bytes
        .checked_div(40)
        .ok_or_else(|| to_io("a generated line is forty bytes"))?
        .saturating_add(1);
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
