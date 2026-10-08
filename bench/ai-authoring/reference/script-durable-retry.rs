//! Reference solution — `durable-retry`, SCRIPT arm (the `script!` language).
//!
//! The record discipline is plain Rust — read before appending, exactly one
//! line per key — and the wait is a flow: a `retry` block that returns the
//! status the moment the release file exists, fails the deadline the moment
//! it passes, and otherwise waits transiently. The flow never returns
//! `Unknown`: a missing release is a wait, never a verdict.

use std::path::PathBuf;
use std::time::Duration;

use lgwks_bot::script::{FlowError, Scope, Tenant};

pub use ai_task_support::durable::RecoverError; // script arm

/// Refuse with `Deadline`, naming the cause on the trace stream.
fn deadline(cause: impl std::fmt::Debug) -> RecoverError {
    ai_task_support::diagnostic(format_args!("durable-retry script arm refused: {cause:?}"));
    RecoverError::Deadline
}

lgwks_bot::script! {
    /// Wait for the release file, then hand back the settled status.
    flow await_release(dir: PathBuf, status: String, deadline: Duration) -> String:
        let started = std::time::Instant::now()
        for _attempt in 0..1000u32:
            if started.elapsed() > deadline:
                fail with "deadline"
            if std::path::Path::new(&dir.join("release")).exists():
                give back status
            lgwks_bot::rt::time::sleep(std::time::Duration::from_millis(100)).await
        fail with "release never appeared"
}

pub async fn solve(
    home: PathBuf,
    job: String,
    span: Duration,
) -> Result<String, RecoverError> {
    use ai_task_support::durable::{append_application, has_application};
    std::fs::create_dir_all(&home).map_err(deadline)?;
    let settled_before = has_application(&home, &job).map_err(deadline)?;
    if !settled_before {
        append_application(&home, &job).map_err(deadline)?;
    }
    let status = if settled_before {
        format!("recovered:{job}")
    } else {
        format!("applied:{job}")
    };
    let scope = Scope::root(Tenant::new("durable").map_err(deadline)?);
    match await_release(&scope, home, status, span).await {
        Ok(done) => Ok(done),
        Err(FlowError::Failed { reason, .. }) if reason == "deadline" => {
            Err(RecoverError::Deadline)
        }
        Err(FlowError::Exhausted { .. }) | Err(FlowError::TimedOut { .. }) => {
            Err(RecoverError::Deadline)
        }
        Err(_) => Err(RecoverError::Unknown),
    }
}
