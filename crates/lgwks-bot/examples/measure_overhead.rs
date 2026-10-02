//! p50/p95/p99 of the two per-run overheads the front door adds.
//!
//! ```text
//! cargo run --release -p lgwks_bot --example measure_overhead -- [runs]
//! ```
//!
//! Two lines are printed, one JSON object each: `host_run` (one `Host::run` of
//! an immediately-ready body) and `process_run` (one `sys::Process` execute of
//! `true`). Latency is in microseconds per call, measured around the public
//! entry point. Peak RSS is read externally (`/usr/bin/time -l`), not from
//! inside: a process id is an identity the OS reuses, which the estate's
//! std-first gate refuses.
//!
//! This is a measurement, not a gate: the separate `sim_task_axes` tests assert
//! the invariants, and this prints the numbers a reader needs to judge the
//! overhead. It is an example rather than a test so it may print.

use std::io::Write;
use std::num::NonZeroUsize;
use std::time::Instant;

use lgwks_bot::domain::sys::Process;
use lgwks_bot::script::{FlowError, Scope};
use lgwks_bot::task::{Host, task};
use lgwks_bot::{Auth, Cap, Execute, GrantSet};

/// The value at `permille` of a sorted sample, in microseconds.
fn percentile(sorted: &[u64], permille: usize) -> u64 {
    let rank = sorted
        .len()
        .saturating_sub(1)
        .saturating_mul(permille)
        .checked_div(1_000)
        .unwrap_or(0);
    sorted.get(rank).copied().unwrap_or(0)
}

/// Summarise a sample as one JSON line, in microseconds.
fn report(name: &str, mut samples: Vec<u64>, runs: usize) -> std::io::Result<()> {
    samples.sort_unstable();
    writeln!(
        std::io::stdout().lock(),
        "{{\"name\":\"{name}\",\"runs\":{runs},\"p50_us\":{},\"p95_us\":{},\"p99_us\":{},\
         \"max_us\":{}}}",
        percentile(&samples, 500),
        percentile(&samples, 950),
        percentile(&samples, 990),
        percentile(&samples, 1_000),
    )
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runs = std::env::args()
        .nth(1)
        .and_then(|text| text.parse::<usize>().ok())
        .unwrap_or(2_000);
    let runtime = lgwks_bot::Runtime::new()?;

    // One Host::run of an immediately-ready body.
    let host = Host::builder("analysis")?
        .max_concurrent_tasks(NonZeroUsize::new(1).ok_or("a ceiling of one")?)
        .build()?;
    let ready = task("ready", |_scope: Scope, value: u64| async move {
        Ok::<u64, FlowError>(value)
    })?;
    let mut host_samples = Vec::with_capacity(runs);
    runtime.block_on(async {
        for index in 0..runs {
            let input = u64::try_from(index).unwrap_or(0);
            let started = Instant::now();
            let report = host.run(&ready, input).await;
            host_samples.push(u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX));
            assert_eq!(
                report.output().copied(),
                Some(input),
                "every measured run returned its input"
            );
        }
    });
    report("host_run", host_samples, runs)?;

    // One sys::Process execute of `true`.
    let auth: Auth = GrantSet::empty().grant(Cap::sys()).issue(&[Cap::sys()])?;
    let process = Process::new("true");
    let mut process_samples = Vec::with_capacity(runs);
    runtime.block_on(async {
        for _ in 0..runs {
            let started = Instant::now();
            let state = process.execute_action((auth.clone(), &())).await?;
            process_samples.push(u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX));
            assert_eq!(state.exit_code, Some(0), "the measured child exited zero");
        }
        Ok::<(), lgwks_bot::BotError>(())
    })?;
    report("process_run", process_samples, runs)?;
    Ok(())
}
