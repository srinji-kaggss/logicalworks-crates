//! p50/p95/p99 of the per-run overheads the front door adds.
//!
//! ```text
//! cargo run --release -p lgwks_bot --example measure_overhead -- [runs]
//! ```
//!
//! Three lines are printed through the shared instrument every other measurement
//! harness in this crate uses (`support/measure.rs`), so the numbers here and
//! the numbers `resume_cost`, `tail_cost` and `poll_deadline_cost` print mean
//! the same thing: `host_run` is one `Host::run` of an immediately-ready body
//! and `process_run` is one `sys::Process` execute of `true`;
//! `process_run_pinned_path` is that execute with the child's `PATH` pinned to
//! the inherited one, the spawn whose bare program `std` forks for and the
//! supervisor resolves to take `posix_spawn` instead. Latency is in
//! microseconds per call, measured around the public entry point.
//! Peak RSS is read externally (`/usr/bin/time -l`), not from
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
use lgwks_bot::rt::process::ProcessSpec;
use lgwks_bot::script::{FlowError, Scope};
use lgwks_bot::task::{Host, task};
use lgwks_bot::{Auth, Cap, Execute, GrantSet};

#[path = "support/measure.rs"]
mod measure;

/// Runs each mechanism is measured over when the command line names no count.
const DEFAULT_RUNS: &str = "2000";

/// Summarise a sample as one report line, in microseconds.
///
/// `n` is the number of samples actually collected and `max` is the slowest one,
/// read off the sorted sample rather than from a running maximum the loop would
/// have had to keep. A sample with none in it prints `max=unmeasured` rather
/// than a zero nobody measured.
fn report(name: &str, mut samples: Vec<u128>) -> std::io::Result<()> {
    let summary = measure::Summary::of(&mut samples);
    let reached = samples.len();
    let mut out = std::io::stdout().lock();
    let _written = match samples.last() {
        Some(worst) => writeln!(out, "{} n={reached} max={worst}us", summary.line(name)),
        None => writeln!(out, "{} n={reached} max=unmeasured", summary.line(name)),
    };
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let requested = std::env::args().nth(1);
    let runs: usize = match requested {
        Some(text) => text.parse()?,
        None => DEFAULT_RUNS.parse()?,
    };
    // The measured body's input is a `u64`, so the run count is drawn in that
    // width once here: a count this host cannot address is a refusal, not a
    // wrapped index.
    let run_count = u64::try_from(runs)?;
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
        for input in 0..run_count {
            let started = Instant::now();
            let report = host.run(&ready, input).await;
            host_samples.push(started.elapsed().as_micros());
            assert_eq!(
                report.output().copied(),
                Some(input),
                "every measured run returned its input"
            );
        }
    });
    report("host_run", host_samples)?;

    // One sys::Process execute of `true`.
    let auth: Auth = GrantSet::empty().grant(Cap::sys()).issue(&[Cap::sys()])?;
    let process = Process::new("true");
    let mut process_samples = Vec::with_capacity(runs);
    runtime.block_on(async {
        for _ in 0..run_count {
            let started = Instant::now();
            let state = process.execute_action((auth.clone(), &())).await?;
            process_samples.push(started.elapsed().as_micros());
            assert_eq!(state.exit_code, Some(0), "the measured child exited zero");
        }
        Ok::<(), lgwks_bot::BotError>(())
    })?;
    report("process_run", process_samples)?;

    // The same execute with `PATH` pinned: the bare name std forks for.
    let inherited = std::env::var_os("PATH").ok_or("an inherited PATH to pin")?;
    let mut spec = ProcessSpec::new("true");
    spec.env("PATH", inherited);
    let pinned = Process::from_spec(spec);
    let mut pinned_samples = Vec::with_capacity(runs);
    runtime.block_on(async {
        for _ in 0..run_count {
            let started = Instant::now();
            let state = pinned.execute_action((auth.clone(), &())).await?;
            pinned_samples.push(started.elapsed().as_micros());
            assert_eq!(state.exit_code, Some(0), "the measured child exited zero");
        }
        Ok::<(), lgwks_bot::BotError>(())
    })?;
    report("process_run_pinned_path", pinned_samples)?;
    Ok(())
}
