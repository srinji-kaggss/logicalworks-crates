//! Matched public API costs for adversarial, literal, and empty-match patterns.

use std::error::Error;
use std::io::{self, Write};
use std::time::Instant;

use lgwks_std::pattern::Regex;

/// Number of samples collected for each matched operation.
const SAMPLES: usize = 31;

/// Labels and semantic evidence shared by one timed operation.
struct Workload<'a> {
    /// Scenario family name.
    scenario: &'a str,
    /// Implementation under measurement.
    implementation: &'a str,
    /// Public operation being measured.
    operation: &'a str,
    /// Haystack size in bytes.
    input_len: usize,
    /// Number of matches in the precomputed exact result.
    match_count: usize,
    /// Stable hash of exact match spans.
    span_hash: u64,
}

fn main() -> Result<(), Box<dyn Error>> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    writeln!(
        output,
        "scenario,implementation,operation,n,samples,p50_ns,p95_ns,p99_ns,work,match_count,span_hash"
    )?;

    let input = "A".repeat(512);
    for (scenario, pattern) in [
        ("greedy-adversary", ".*[^A-Z]|[A-Z]"),
        ("literal-control", "A"),
        ("empty-match", ""),
    ] {
        run_scenario(&mut output, scenario, pattern, &input)?;
    }
    Ok(())
}

/// Compare the public facade with the regex engine on identical inputs.
fn run_scenario(
    output: &mut impl Write,
    scenario: &str,
    pattern: &str,
    input: &str,
) -> Result<(), Box<dyn Error>> {
    let facade = Regex::new(pattern)?;
    let engine = regex::Regex::new(pattern)?;
    let facade_spans = facade
        .find_all(input)
        .map(|item| (item.start, item.end))
        .collect::<Vec<_>>();
    let engine_spans = engine
        .find_iter(input)
        .map(|item| (item.start(), item.end()))
        .collect::<Vec<_>>();
    if facade_spans != engine_spans {
        let refusal = Err(io::Error::other("facade and regex engine spans differ").into());
        #[cfg(feature = "trace")]
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "run_scenario: returning an error to the caller");
        return refusal;
    }
    let hash = span_hash(&facade_spans);
    let match_count = facade_spans.len();

    emit(
        output,
        Workload {
            scenario,
            implementation: "facade",
            operation: "single-find",
            input_len: input.len(),
            match_count,
            span_hash: hash,
        },
        || usize::from(facade.find(input).is_some()),
    )?;
    emit(
        output,
        Workload {
            scenario,
            implementation: "engine",
            operation: "single-find",
            input_len: input.len(),
            match_count,
            span_hash: hash,
        },
        || usize::from(engine.find(input).is_some()),
    )?;
    emit(
        output,
        Workload {
            scenario,
            implementation: "facade",
            operation: "find-all",
            input_len: input.len(),
            match_count,
            span_hash: hash,
        },
        || facade.find_all(input).count(),
    )?;
    emit(
        output,
        Workload {
            scenario,
            implementation: "engine",
            operation: "find-all",
            input_len: input.len(),
            match_count,
            span_hash: hash,
        },
        || engine.find_iter(input).count(),
    )?;
    emit(
        output,
        Workload {
            scenario,
            implementation: "facade",
            operation: "split",
            input_len: input.len(),
            match_count,
            span_hash: hash,
        },
        || facade.split(input).count(),
    )?;
    emit(
        output,
        Workload {
            scenario,
            implementation: "engine",
            operation: "split",
            input_len: input.len(),
            match_count,
            span_hash: hash,
        },
        || engine.split(input).count(),
    )?;
    emit(
        output,
        Workload {
            scenario,
            implementation: "facade",
            operation: "replace-all",
            input_len: input.len(),
            match_count,
            span_hash: hash,
        },
        || facade.replace_all(input, "X").len(),
    )?;
    emit(
        output,
        Workload {
            scenario,
            implementation: "engine",
            operation: "replace-all",
            input_len: input.len(),
            match_count,
            span_hash: hash,
        },
        || engine.replace_all(input, "X").len(),
    )?;
    Ok(())
}

/// Time one workload repeatedly and write its distribution and exact work count.
fn emit<F>(output: &mut impl Write, workload: Workload<'_>, mut operation_fn: F) -> io::Result<()>
where
    F: FnMut() -> usize,
{
    let mut elapsed = Vec::with_capacity(SAMPLES);
    let mut expected_work = None;
    for _ in 0..SAMPLES {
        let start = Instant::now();
        let work = std::hint::black_box(operation_fn());
        elapsed.push(start.elapsed().as_nanos());
        if let Some(expected) = expected_work {
            if work != expected {
                let refusal = Err(io::Error::other(
                    "operation work count changed between samples",
                ));
                #[cfg(feature = "trace")]
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "emit: returning an error to the caller");
                return refusal;
            }
        } else {
            expected_work = Some(work);
        }
    }
    elapsed.sort_unstable();
    let work = expected_work.ok_or_else(|| io::Error::other("no timing samples"))?;
    writeln!(
        output,
        "{},{},{},{},{SAMPLES},{},{},{},{work},{},{:016x}",
        workload.scenario,
        workload.implementation,
        workload.operation,
        workload.input_len,
        elapsed[15],
        elapsed[29],
        elapsed[30],
        workload.match_count,
        workload.span_hash,
    )
}

/// Hash exact `(start, end)` byte spans for cross-implementation comparison.
fn span_hash(spans: &[(usize, usize)]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for &(start, end) in spans {
        for byte in start.to_le_bytes().into_iter().chain(end.to_le_bytes()) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    hash
}
