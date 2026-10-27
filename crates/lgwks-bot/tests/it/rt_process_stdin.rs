//! A supervised child can be handed bytes on its standard input.
//!
//! The gap this closes was found driving SQLite's `sqlite3` program from the
//! estate's own supervisor (#700): with only `Inherit`, `Null` and `Capture`
//! there was no way to say "this child's standard input is this text", so a
//! tool that reads a program could not be run. `Capture` is a *read* of what
//! the child writes; [`StdioPolicy::Piped`](lgwks_bot::rt::process::StdioPolicy::Piped)
//! carries a *write* of what the child reads: the supervisor writes the bytes
//! while the child runs, under the run's own deadline, then closes the pipe.

#![cfg(all(
    unix,
    feature = "rt",
    feature = "time",
    feature = "sync",
    feature = "process"
))]

use std::num::NonZeroUsize;
use std::time::Duration;

use lgwks_bot::Runtime;
use lgwks_bot::rt::process::{ProcessRunError, ProcessSpec, StdioPolicy};
use lgwks_bot::rt::supervise::Supervisor;

use crate::scratch::Scratch;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Run `spec` to completion on a fresh supervisor.
fn run(
    spec: &ProcessSpec,
) -> Result<lgwks_bot::rt::process::ProcessRun, Box<dyn std::error::Error>> {
    Ok(Runtime::new()?.block_on(async {
        let mut supervisor = Supervisor::new(1);
        supervisor.run_process(spec).await
    })?)
}

fn capture_limit() -> Result<NonZeroUsize, Box<dyn std::error::Error>> {
    NonZeroUsize::new(64 * 1024).ok_or_else(|| "the capture ceiling is non-zero".into())
}

#[test]
fn piped_input_reaches_the_child() -> TestResult {
    let mut spec = ProcessSpec::new("cat");
    spec.stdin(StdioPolicy::Piped(b"select 1;".to_vec()));
    spec.capture_stdout(capture_limit()?);
    spec.deadline(Duration::from_secs(10));

    let run = run(&spec)?;
    assert_eq!(
        run.exit_code(),
        Some(0),
        "a child handed its input exits normally"
    );
    assert_eq!(
        run.stdout().bytes(),
        b"select 1;",
        "what the supervisor wrote is what the child read"
    );
    Ok(())
}

#[test]
fn empty_piped_input_reads_eof_at_once() -> TestResult {
    let mut spec = ProcessSpec::new("cat");
    spec.stdin(StdioPolicy::Piped(Vec::new()));
    spec.capture_stdout(capture_limit()?);
    spec.deadline(Duration::from_secs(10));

    let run = run(&spec)?;
    assert_eq!(
        run.exit_code(),
        Some(0),
        "a child handed end-of-file exits normally"
    );
    assert!(
        run.stdout().bytes().is_empty(),
        "no input means no output, not a hang"
    );
    Ok(())
}

#[test]
fn input_past_the_ceiling_never_forks() -> TestResult {
    let mut spec = ProcessSpec::new("cat");
    spec.stdin(StdioPolicy::Piped(vec![b'x'; 1024 * 1024 + 1]));
    spec.capture_stdout(capture_limit()?);
    spec.deadline(Duration::from_secs(10));

    match run(&spec) {
        Err(error) => match error.downcast::<ProcessRunError>() {
            Ok(boxed) => match *boxed {
                ProcessRunError::NotStarted { .. } => Ok(()),
                refused => {
                    Err(format!("an input past the ceiling must not start: got {refused:?}").into())
                }
            },
            Err(error) => {
                Err(format!("an input past the ceiling must not start: got {error}").into())
            }
        },
        Ok(_) => Err("an input past the ceiling must not run".into()),
    }
}

#[test]
fn piped_output_is_refused_before_the_fork() -> TestResult {
    let scratch = Scratch::new("rt-stdin-refused")?;
    let ran = scratch.path().join("ran");
    let mut spec = ProcessSpec::new("sh");
    spec.arg("-c").arg(format!("touch {}", ran.display()));
    spec.stdout(StdioPolicy::Piped(b"unused".to_vec()));

    let outcome = Runtime::new()?.block_on(async {
        let mut supervisor = Supervisor::new(1);
        supervisor.run_process(&spec).await
    });

    assert!(
        outcome.is_err(),
        "a piped output is refused, not run half-connected"
    );
    assert!(!ran.exists(), "no child ran");
    Ok(())
}
