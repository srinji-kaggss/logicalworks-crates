//! A child's output can go to a file, and a file that cannot be opened refuses the
//! start instead of running the child with its output lost.
//!
//! The gap this closes was found by migrating a real consumer: `llvm-cov export`
//! writes its result to stdout and has no output-path flag, and with only
//! `Inherit`, `Null` and `Capture` the one faithful translation was a silent
//! `Null`, which kept the build green and emptied every tracefile.

#![cfg(all(
    unix,
    feature = "rt",
    feature = "time",
    feature = "sync",
    feature = "process"
))]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use lgwks_bot::Runtime;
use lgwks_bot::rt::process::ProcessSpec;
use lgwks_bot::rt::supervise::Supervisor;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// A scratch directory unique to this process and test, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Result<Self, Box<dyn std::error::Error>> {
        // Nanos plus a counter, never a process id: the OS reuses pids, and a
        // reused id would make two runs share one directory.
        static SCRATCH: AtomicU64 = AtomicU64::new(0);
        let unique = SCRATCH.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("lgwks-rt-files-{name}-{nanos}-{unique}"));
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.0));
    }
}

fn shell(script: &str) -> ProcessSpec {
    let mut spec = ProcessSpec::new("/bin/sh");
    spec.arg("-c").arg(script);
    spec
}

#[test]
fn stdout_and_stderr_land_in_their_files() -> TestResult {
    let scratch = Scratch::new("both")?;
    let out = scratch.0.join("out.txt");
    let err = scratch.0.join("err.txt");
    std::fs::write(&out, "stale contents that must not survive")?;
    let mut spec = shell("printf result; printf problem >&2");
    spec.stdout_to_file(&out).stderr_to_file(&err);

    let run = Runtime::new()?.block_on(async {
        let mut supervisor = Supervisor::new(1);
        supervisor.run_process(&spec).await
    })?;

    assert_eq!(run.exit_code(), Some(0));
    assert_eq!(
        std::fs::read_to_string(&out)?,
        "result",
        "truncated, then written"
    );
    assert_eq!(std::fs::read_to_string(&err)?, "problem");
    assert!(
        run.stdout().bytes().is_empty(),
        "a stream sent to a file is not also captured"
    );
    Ok(())
}

#[test]
fn a_later_policy_replaces_the_file() -> TestResult {
    let scratch = Scratch::new("replaced")?;
    let out = scratch.0.join("out.txt");
    let mut spec = shell("printf captured");
    spec.stdout_to_file(&out)
        .capture_stdout(std::num::NonZeroUsize::new(64).ok_or("non-zero")?);

    let run = Runtime::new()?.block_on(async {
        let mut supervisor = Supervisor::new(1);
        supervisor.run_process(&spec).await
    })?;

    assert_eq!(run.stdout().bytes(), b"captured");
    assert!(
        !out.exists(),
        "the file redirect was replaced, so the file was never opened"
    );
    Ok(())
}

#[test]
fn an_unopenable_path_refuses_the_start_and_runs_nothing() -> TestResult {
    let scratch = Scratch::new("refused")?;
    let ran = scratch.0.join("ran");
    let mut spec = shell(&format!("touch {}", ran.display()));
    spec.stdout_to_file(scratch.0.join("missing-directory").join("out.txt"));

    let outcome = Runtime::new()?.block_on(async {
        let mut supervisor = Supervisor::new(1);
        supervisor.run_process(&spec).await
    });

    assert!(
        outcome.is_err(),
        "the start is refused, not run with output lost"
    );
    assert!(!ran.exists(), "no child ran");
    Ok(())
}
