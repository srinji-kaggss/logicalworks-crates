//! p50/p95/p99 of the per-run overheads the front door adds.
//!
//! ```text
//! cargo run --locked --release -p lgwks_bot --features "script rt time sync process" \
//!     --example measure_overhead -- [runs]
//! ```
//!
//! Four lines are printed through the shared instrument every other measurement
//! harness in this crate uses (`support/measure.rs`), so the numbers here and
//! the numbers `resume_cost`, `tail_cost` and `poll_deadline_cost` print mean
//! the same thing: `host_run` is one `Host::run` of an immediately-ready body;
//! `script_line` is what one line of a `script!` flow costs the runtime, in
//! nanoseconds (a run of [`lines`] divided by the lines it ran: a `for` item, a
//! `step`, a `within` and a `retry` per pass, each succeeding at once, so the
//! cost is the runtime's per-line bookkeeping and not the body's work);
//! `process_run` is one `sys::Process` execute of `true`;
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
use lgwks_bot::script::{FlowError, Scope, Tenant};
use lgwks_bot::task::{Host, task};
use lgwks_bot::{Auth, Cap, Execute, GrantSet};

#[path = "support/measure.rs"]
mod measure;

/// Runs each mechanism is measured over when the command line names no count.
const DEFAULT_RUNS: &str = "2000";

/// Passes `lines` makes per measured run.
const PASSES: u64 = 256;

/// Script lines each pass of `lines` enters: a `for` item, a `step`, a
/// `within` and a `retry`.
const LINES_PER_PASS: u128 = 4;

lgwks_bot::script! {
    /// One line of each block word per pass, each succeeding at once: what a
    /// script line costs the runtime, with no work of its own to hide it.
    flow lines(passes: u64) -> u64:
        let mut total = 0_u64
        for pass in 0..passes:
            step pass_step:
                let bounded = within 1s:
                    retry up to 2 times, waiting 1ms:
                        pass
                total = total.wrapping_add(bounded)
        give back total
}

/// The unit a sample was taken in.
#[derive(Clone, Copy)]
enum Unit {
    /// Microseconds: a whole call through a public entry point.
    Micros,
    /// Nanoseconds: one script line, far below a microsecond.
    Nanos,
}

impl Unit {
    /// The suffix the report prints after each number.
    const fn suffix(self) -> &'static str {
        match self {
            Self::Micros => "us",
            Self::Nanos => "ns",
        }
    }
}

/// Summarise a sample as one report line, in `unit`.
///
/// `n` is the number of samples actually collected and `max` is the slowest one,
/// read off the sorted sample rather than from a running maximum the loop would
/// have had to keep. A sample with none in it prints `max=unmeasured` rather
/// than a zero nobody measured.
fn report(name: &str, unit: Unit, mut samples: Vec<u128>) -> std::io::Result<()> {
    let summary = measure::Summary::of(&mut samples);
    let line = match unit {
        Unit::Micros => summary.line(name),
        Unit::Nanos => summary.line_in(name, unit.suffix()),
    };
    let unit = unit.suffix();
    let reached = samples.len();
    let mut out = std::io::stdout().lock();
    let _written = match samples.last() {
        Some(worst) => writeln!(out, "{line} n={reached} max={worst}{unit}"),
        None => writeln!(out, "{line} n={reached} max=unmeasured"),
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
    report("host_run", Unit::Micros, host_samples)?;

    // One script line: a run of `lines` on a fresh root, per line it entered.
    let tenant = Tenant::new("overhead")?;
    let lines_per_run = LINES_PER_PASS.saturating_mul(u128::from(PASSES));
    let expected: u64 = (0..PASSES).sum();
    let mut line_samples = Vec::with_capacity(runs);
    runtime.block_on(async {
        for _ in 0..run_count {
            let scope = Scope::root(tenant.clone());
            let started = Instant::now();
            let total = lines(&scope, PASSES).await?;
            line_samples.push(started.elapsed().as_nanos().div_euclid(lines_per_run));
            assert_eq!(total, expected, "every measured run summed its passes");
        }
        Ok::<(), FlowError>(())
    })?;
    report("script_line", Unit::Nanos, line_samples)?;

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
    report("process_run", Unit::Micros, process_samples)?;

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
    report("process_run_pinned_path", Unit::Micros, pinned_samples)?;
    Ok(())
}
