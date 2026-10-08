//! Reference solution — `durable-retry`, FAN API (record + bounded `retry`).
//!
//! The same record discipline as the `new` arm: the file is the idempotency
//! key, read before anything is appended. The wait differs in how it is
//! bounded — attempts derived from the task's own deadline plus margin, so a
//! short deadline means a short wait rather than a fixed hundred thousand
//! polls, and exhaustion reads as the deadline it was derived from.

use std::path::PathBuf;
use std::time::Duration;

use ai_task_support::durable::{append_application, has_application, is_released};
use lgwks_bot::script::{FlowError, Scope, Tenant, attempts, retry};

pub use ai_task_support::durable::RecoverError; // fan arm

pub async fn solve(
    base: PathBuf,
    ticket: String,
    cap: Duration,
) -> Result<String, RecoverError> {
    std::fs::create_dir_all(&base).map_err(|_| RecoverError::Deadline)?;
    // A line already there means an earlier attempt applied and died before
    // answering: reconcile instead of duplicating.
    let applied_before = has_application(&base, &ticket).map_err(|_| RecoverError::Deadline)?;
    if !applied_before {
        append_application(&base, &ticket).map_err(|_| RecoverError::Deadline)?;
    }
    let outcome = if applied_before {
        format!("recovered:{ticket}")
    } else {
        format!("applied:{ticket}")
    };
    let scope = Scope::root(Tenant::new("durable").map_err(|_| RecoverError::Deadline)?);
    let opened = std::time::Instant::now();
    // Attempts cover the deadline at a hundred milliseconds apart, capped at
    // the thousand the bound allows: the elapsed check below enforces the
    // task's deadline, so exhaustion past the cap still reads as the
    // deadline it was derived from.
    let polls = u32::try_from(cap.as_millis() / 100 + 100)
        .map(|over| over.min(1000))
        .map_err(|_| RecoverError::Deadline)?;
    retry(
        &scope,
        "await-release",
        attempts(polls).map_err(|_| RecoverError::Deadline)?,
        Duration::from_millis(100),
        |_, _| {
            let base = base.clone();
            async move {
                if is_released(&base) {
                    Ok(())
                } else if opened.elapsed() < cap {
                    Err(FlowError::transient("release not present yet"))
                } else {
                    Err(FlowError::failed("deadline"))
                }
            }
        },
    )
    .await
    .map_err(|_| RecoverError::Deadline)?;
    Ok(outcome)
}
