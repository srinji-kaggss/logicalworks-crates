//! Oracle for the `durable-retry` task. One test per contract clause.
//!
//! The crash clause is Unix-only: it `SIGKILL`s a child running the phase-1
//! test below, then recovers in-process. Everything else is portable. Bounded:
//! every wait polls a file with a bound, and the longest single bound is the
//! 30-second applied-line wait in the kill clause. The harness's own oracle
//! timeout is the outer bound.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ai_trial::{RecoverError, solve};
use lgwks_bot::rt::runtime::Runtime;

/// The effect key every clause uses.
const KEY: &str = "order-7";

/// A deadline no success path reaches.
const LONG: Duration = Duration::from_secs(30);

/// The runtime every test drives its futures on.
fn runtime() -> Result<Runtime, std::io::Error> {
    Runtime::new()
}

/// A fresh scratch directory for one test.
fn scratch(name: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())?;
    let dir = std::env::temp_dir().join(format!("durable-{name}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Every `applied:*` line the effect directory currently holds.
fn applied_lines(dir: &Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let path = dir.join("applied.log");
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(text.lines().map(str::to_owned).collect()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(format!("reading {}: {error}", path.display()).into()),
    }
}

/// Whether `applied.log` already holds `applied:{KEY}`.
fn is_applied(dir: &Path) -> Result<bool, Box<dyn std::error::Error>> {
    let wanted = format!("applied:{KEY}");
    Ok(applied_lines(dir)?.iter().any(|line| line == &wanted))
}

/// Wait until `applied.log` holds `applied:{KEY}`, or the bound runs out.
fn wait_for_applied(dir: &Path, bound: Duration) -> Result<bool, Box<dyn std::error::Error>> {
    let start = Instant::now();
    loop {
        if is_applied(dir)? {
            return Ok(true);
        }
        if start.elapsed() >= bound {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The phase-1 half of the crash clause, run in a child process.
///
/// A no-op unless `DURABLE_PHASE=1`, so the parent's full-oracle run records a
/// pass without blocking: only the child the kill clause spawns ever waits
/// here, and it waits until it is killed.
#[test]
fn phase1_blocks_until_released() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("DURABLE_PHASE").as_deref() != Ok("1") {
        return Ok(());
    }
    let dir = PathBuf::from(std::env::var("DURABLE_DIR").map_err(|_| "DURABLE_DIR is not set")?);
    let key = std::env::var("DURABLE_KEY").map_err(|_| "DURABLE_KEY is not set")?;
    let runtime = runtime()?;
    let _ = runtime.block_on(solve(dir, key, LONG));
    Ok(())
}

#[test]
fn a_fresh_call_applies_once_and_returns_applied() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = runtime()?;
    let dir = scratch("fresh")?;
    std::fs::write(dir.join("release"), b"")?;
    match runtime.block_on(solve(dir.clone(), KEY.to_owned(), LONG)) {
        Ok(status) => assert_eq!(status, format!("applied:{KEY}"), "the first call applies"),
        Err(error) => return Err(format!("expected Ok(applied), got {error:?}").into()),
    }
    assert_eq!(
        applied_lines(&dir)?,
        vec![format!("applied:{KEY}")],
        "exactly one application line"
    );
    Ok(())
}

#[test]
fn status_is_never_unknown() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = runtime()?;
    let dir = scratch("known")?;
    std::fs::write(dir.join("release"), b"")?;
    match runtime.block_on(solve(dir.clone(), KEY.to_owned(), LONG)) {
        Ok(status) => assert_ne!(status, "unknown", "the status is never unknown"),
        Err(RecoverError::Unknown) => return Err("Unknown is returned never".into()),
        Err(error) => return Err(format!("expected Ok, got {error:?}").into()),
    }
    Ok(())
}

#[test]
fn waiting_past_the_deadline_is_typed() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = runtime()?;
    let dir = scratch("deadline")?;
    // No release file: the wait must time out rather than hang the matrix.
    match runtime.block_on(solve(dir.clone(), KEY.to_owned(), Duration::from_millis(300))) {
        Err(RecoverError::Deadline) => {}
        other => return Err(format!("an unreleased wait past 300 ms is Deadline, got {other:?}").into()),
    }
    Ok(())
}

#[test]
fn dropping_mid_wait_applies_nothing_twice() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = runtime()?;
    let dir = scratch("drop")?;
    // Drive one solve until its applied line lands, drop it mid-wait, release
    // a second call and require exactly one line in total.
    {
        let mut run = Box::pin(solve(dir.to_path_buf(), KEY.to_owned(), LONG));
        runtime.block_on(std::future::poll_fn(|context| {
            use std::task::Poll;
            match is_applied(&dir) {
                Ok(true) => Poll::Ready(()),
                Ok(false) => {
                    if run.as_mut().poll(context).is_ready() {
                        Poll::Ready(())
                    } else {
                        context.waker().wake_by_ref();
                        Poll::Pending
                    }
                }
                Err(_) => Poll::Ready(()),
            }
        }));
    }
    std::fs::write(dir.join("release"), b"")?;
    match runtime.block_on(solve(dir.to_path_buf(), KEY.to_owned(), LONG)) {
        Ok(status) => assert_eq!(
            status,
            format!("recovered:{KEY}"),
            "the dropped call applied, so the next call recovers"
        ),
        Err(error) => return Err(format!("expected Ok(recovered), got {error:?}").into()),
    }
    assert_eq!(
        applied_lines(&dir)?.len(),
        1,
        "one application in total across the dropped call and its recovery"
    );
    Ok(())
}

/// The crash clause: SIGKILL mid-effect, then recover with exactly one
/// external effect and a non-unknown status.
///
/// Unix-only: the kill itself needs a Unix signal.
#[cfg(unix)]
#[test]
fn kill_mid_effect_recovers_exactly_once() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = runtime()?;
    let dir = scratch("kill")?;
    let exe = std::env::current_exe()?;
    let mut child = std::process::Command::new(exe)
        .args(["phase1_blocks_until_released", "--exact", "--nocapture"])
        .env("DURABLE_PHASE", "1")
        .env("DURABLE_DIR", &dir)
        .env("DURABLE_KEY", KEY)
        // Quiet: the child's libtest chatter would interleave mid-line with
        // this test's own status line, and the harness reads the verdict off
        // that line. The child's fate is observed through the record and its
        // exit, never through its output.
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    assert!(
        wait_for_applied(&dir, Duration::from_secs(30))?,
        "phase 1 applies before it is killed"
    );
    let killed = std::process::Command::new("kill")
        .args(["-9", &child.id().to_string()])
        .status()?;
    assert!(killed.success(), "SIGKILL is delivered to the phase-1 child");
    let _ = child.wait()?;
    // Recover in-process: the record survived the kill even though the answer
    // did not.
    std::fs::write(dir.join("release"), b"")?;
    match runtime.block_on(solve(dir.clone(), KEY.to_owned(), LONG)) {
        Ok(status) => assert_eq!(
            status,
            format!("recovered:{KEY}"),
            "the recovery reconciles rather than duplicates"
        ),
        Err(error) => return Err(format!("expected Ok(recovered), got {error:?}").into()),
    }
    assert_eq!(
        applied_lines(&dir)?,
        vec![format!("applied:{KEY}")],
        "exactly one external effect across the kill and the recovery"
    );
    Ok(())
}
