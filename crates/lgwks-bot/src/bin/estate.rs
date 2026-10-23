//! The estate evening as a smoke: every tier, both doors, every refusal, one report.
//!
//! Run it on a built artifact to prove the assembly there. It builds each tier, runs through
//! `run_task` and `run_blocking`, cancels, drains both ways, drops an estate inside a runtime,
//! exercises every typed refusal, and prints the terminal record. Every number it prints is also
//! asserted: the first deviation records a mismatch and the exit is non-zero, so a clean exit is a
//! proved evening. `the_smoke_binary_proves_the_evening` (`tests/it/estate.rs`) runs it on every
//! test run and reads the same exit.

use std::io::Write as _;

use lgwks_bot::rt::estate::{Estate, EstateError, EstateReport};
use lgwks_bot::rt::runtime::Runtime;
use lgwks_bot::task::{Disposition, task};

/// One line to the caller's locked stdout; a write error reaches `main` as an I/O error.
fn say(line: &str) -> std::io::Result<()> {
    writeln!(std::io::stdout().lock(), "{line}")
}

/// What one run proved, owned so the async evening can hand it back to `main`.
#[derive(Debug)]
struct RunCheck {
    /// Whether the run succeeded.
    succeeded: bool,
    /// The value it answered, when it succeeded.
    output: Option<u64>,
    /// The located failure, when it did not.
    error: Option<String>,
}

/// Record the first deviation and keep it: the first mismatch is the root cause, and later ones
/// are its symptoms.
fn record(mismatch: &mut Option<String>, message: String) {
    if mismatch.is_none() {
        *mismatch = Some(message);
    }
}

/// The smoke's scratch directory, removed on every exit — an early `?` return included.
struct Scratch(std::path::PathBuf);

impl Drop for Scratch {
    /// Remove the directory; a refusal is logged, because a destructor has nowhere to return it.
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            lgwks_std::trace::debug!(error = ?error, "estate-smoke: the scratch dir was not removed");
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut mismatch: Option<String> = None;
    let driver = Runtime::new()?;
    let mut estate = Estate::supervised("acme", 8)?;
    let supervised = estate.supervisor().is_some();
    say(&format!(
        "estate: tenant={} tier={} runtime={} supervisor={supervised} store={}",
        estate.tenant(),
        estate.tier(),
        estate.handle().is_some(),
        estate.run_store().is_some(),
    ))?;
    let probe = task("estate-probe", |_scope, input: u64| async move {
        Ok(input.saturating_add(1))
    })
    .map_err(|error| format!("the probe task is refused: {error}"))?;
    let (first, second, report): (RunCheck, RunCheck, EstateReport) = driver.block_on(async {
        let run = estate.run_task(&probe, 41).await;
        let first = RunCheck {
            succeeded: matches!(run.disposition(), Disposition::Succeeded),
            output: run.output().copied(),
            error: run.error().map(|error| error.to_string()),
        };
        let run = estate.run_task(&probe, 42).await;
        let second = RunCheck {
            succeeded: matches!(run.disposition(), Disposition::Succeeded),
            output: run.output().copied(),
            error: run.error().map(|error| error.to_string()),
        };
        estate.cancel();
        (first, second, estate.shutdown().await)
    });
    say(&format!(
        "run 41: succeeded={} output={:?} error={:?}",
        first.succeeded, first.output, first.error,
    ))?;
    say(&format!(
        "run 42: succeeded={} output={:?} error={:?}",
        second.succeeded, second.output, second.error,
    ))?;
    if !first.succeeded || first.output != Some(42u64) {
        record(
            &mut mismatch,
            format!(
                "run 41 is not a successful 42: succeeded={} output={:?} error={:?}",
                first.succeeded, first.output, first.error,
            ),
        );
    }
    if !second.succeeded || second.output != Some(43u64) {
        record(
            &mut mismatch,
            format!(
                "run 42 is not a successful 43: succeeded={} output={:?} error={:?}",
                second.succeeded, second.output, second.error,
            ),
        );
    }
    say(&format!(
        "shutdown: tenant={} tier={} admitted={} refused={} supervised={}",
        report.tenant(),
        report.tier(),
        report.runs_admitted(),
        report.runs_refused(),
        report.supervised().succeeded,
    ))?;
    if report.runs_admitted() != 2 || report.runs_refused() != 0 {
        record(
            &mut mismatch,
            format!(
                "the shutdown accounts for two admitted runs, not {} admitted and {} refused",
                report.runs_admitted(),
                report.runs_refused(),
            ),
        );
    }

    let mut estate = Estate::supervised("acme", 8)?;
    estate.cancel();
    let report = estate
        .shutdown_blocking()
        .map_err(|error| format!("the blocking drain failed: {error}"))?;
    say(&format!(
        "blocking drain: completed={} admitted={}",
        report.supervised().completed,
        report.runs_admitted(),
    ))?;
    if report.supervised().completed != 0 || report.runs_admitted() != 0 {
        record(
            &mut mismatch,
            "the empty cancel drain is not empty".to_owned(),
        );
    }

    let mut bare = Estate::in_process("acme")?;
    say(&format!(
        "in-process: runtime={} supervisor={}",
        bare.handle().is_some(),
        bare.supervisor().is_some(),
    ))?;
    let run = bare
        .run_blocking(&probe, 6)
        .map_err(|error| format!("the in-process run failed: {error}"))?;
    say(&format!(
        "in-process run: disposition={} output={:?}",
        run.disposition(),
        run.output(),
    ))?;
    if !matches!(run.disposition(), Disposition::Succeeded) || run.output() != Some(&7u64) {
        record(
            &mut mismatch,
            format!(
                "the in-process run is not a successful 7: {:?}",
                run.error()
            ),
        );
    }
    let report = bare
        .shutdown_blocking()
        .map_err(|error| format!("the in-process drain failed: {error}"))?;
    say(&format!(
        "in-process drain: tier={} admitted={}",
        report.tier(),
        report.runs_admitted(),
    ))?;
    if report.runs_admitted() != 1 {
        record(
            &mut mismatch,
            format!(
                "the in-process drain accounts for one run, not {}",
                report.runs_admitted(),
            ),
        );
    }
    // The run above built the host's own reactor; dropping the estate inside a runtime is the
    // panic tokio raises for a plain runtime drop, so reaching the next line proves the host ends
    // its reactor in the background.
    driver.block_on(async move { drop(bare) });
    say("in-process drop inside a runtime: ok")?;

    let unique = lgwks_std::random::bytes::<8>()
        .map_err(|error| format!("the scratch name failed: {error}"))?;
    let hex: String = unique.iter().map(|byte| format!("{byte:02x}")).collect();
    let dir = std::env::temp_dir().join(format!("lgwks-estate-smoke-{hex}"));
    let _scratch = Scratch(dir.clone());
    let durable = Estate::durable("acme", 8, &dir)?;
    say(&format!("durable: store={}", durable.run_store().is_some()))?;
    let run = durable
        .run_blocking(&probe, 7)
        .map_err(|error| format!("the durable run failed: {error}"))?;
    say(&format!(
        "durable run: disposition={} output={:?}",
        run.disposition(),
        run.output(),
    ))?;
    if !matches!(run.disposition(), Disposition::Succeeded) || run.output() != Some(&8u64) {
        record(
            &mut mismatch,
            format!("the durable run is not a successful 8: {:?}", run.error()),
        );
    }
    let report = driver.block_on(async { durable.shutdown().await });
    say(&format!(
        "durable drain: admitted={}",
        report.runs_admitted(),
    ))?;
    if report.runs_admitted() != 1 {
        record(
            &mut mismatch,
            format!(
                "the durable drain accounts for one run, not {}",
                report.runs_admitted(),
            ),
        );
    }

    match Estate::supervised("", 8) {
        Err(EstateError::InvalidTenant { .. }) => {
            say("refused: an empty tenant")?;
        }
        Err(error) => {
            record(
                &mut mismatch,
                format!("an empty tenant is InvalidTenant, not {error}"),
            );
        }
        Ok(_) => {
            record(&mut mismatch, "an empty tenant must not build".to_owned());
        }
    }
    match Estate::supervised("acme", 0) {
        Err(EstateError::Host { .. }) => {
            say("refused: a zero bound")?;
        }
        Err(error) => {
            record(&mut mismatch, format!("a zero bound is Host, not {error}"));
        }
        Ok(_) => {
            record(&mut mismatch, "a zero bound must not build".to_owned());
        }
    }
    match Estate::supervised("acme", usize::MAX) {
        Err(EstateError::Host { .. }) => {
            say("refused: a bound past the ceiling")?;
        }
        Err(error) => {
            record(
                &mut mismatch,
                format!("a bound past the ceiling is Host, not {error}"),
            );
        }
        Ok(_) => {
            record(
                &mut mismatch,
                "a bound past the ceiling must not build".to_owned(),
            );
        }
    }
    let blocker = dir.join("blocker");
    std::fs::write(&blocker, b"a file, not a directory")
        .map_err(|error| format!("the blocker file failed: {error}"))?;
    match Estate::durable("acme", 8, &blocker) {
        Err(EstateError::Store { .. }) => {
            say("refused: an unopenable store")?;
        }
        Err(error) => {
            record(
                &mut mismatch,
                format!("an unopenable store is Store, not {error}"),
            );
        }
        Ok(_) => {
            record(
                &mut mismatch,
                "an unopenable store must not build".to_owned(),
            );
        }
    }

    if let Some(mismatch) = mismatch {
        lgwks_std::trace::debug!(
            mismatch = mismatch.as_str(),
            "estate-smoke: the evening proved nothing"
        );
        return Err(mismatch.into());
    }
    Ok(())
}
