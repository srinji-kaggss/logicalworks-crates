//! Reference solution — `durable-retry`, NEW API (record + `retry` wait).
//!
//! The record discipline is the same file the `old` arm keeps: read before
//! appending, exactly one line per key. What the facade adds here is the wait:
//! `retry` polls the release file with a bounded backoff instead of a
//! hand-rolled sleep loop, and a missed deadline is a typed `Failed` rather
//! than a branch the author remembers to write.

use std::path::PathBuf;
use std::time::Duration;

use ai_task_support::durable::{append_application, has_application};
use lgwks_bot::script::{FlowError, Scope, Tenant, attempts, retry};

pub use ai_task_support::durable::RecoverError; // new arm

pub async fn solve(
    root: PathBuf,
    tag: String,
    wait: Duration,
) -> Result<String, RecoverError> {
    std::fs::create_dir_all(&root).map_err(|_| RecoverError::Deadline)?;
    let fresh = !has_application(&root, &tag).map_err(|_| RecoverError::Deadline)?;
    if fresh {
        append_application(&root, &tag).map_err(|_| RecoverError::Deadline)?;
    }
    let status = if fresh {
        format!("applied:{tag}")
    } else {
        format!("recovered:{tag}")
    };
    let scope = Scope::root(Tenant::new("durable").map_err(|_| RecoverError::Deadline)?);
    let started = std::time::Instant::now();
    let release = root.join("release");
    // A thousand polls a hundred milliseconds apart: a hundred seconds of
    // waiting, so the attempt bound never decides a run the deadline still
    // owns. The elapsed check below is what enforces the task's deadline.
    retry(
        &scope,
        "wait-release",
        attempts(1000).map_err(|_| RecoverError::Deadline)?,
        Duration::from_millis(100),
        |_, _| {
            let release = release.clone();
            async move {
                if release.exists() {
                    Ok(())
                } else if started.elapsed() > wait {
                    Err(FlowError::failed("deadline"))
                } else {
                    Err(FlowError::transient("waiting for release"))
                }
            }
        },
    )
    .await
    .map_err(|_| RecoverError::Deadline)?;
    Ok(status)
}
