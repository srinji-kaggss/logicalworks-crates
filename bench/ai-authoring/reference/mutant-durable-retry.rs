//! Deliberately wrong solution — `durable-retry`, checked yet duplicated.
//!
//! This is a mutant input for the harness, not an example: it reads the
//! record and then appends regardless, so the check exists and changes
//! nothing. A recovery applies the effect a second time, and the oracle's
//! exactly-once clause must fail it. It is never built by the estate
//! workspace (this directory is not a member), so it is not held to the
//! estate lint contract.

use std::path::PathBuf;
use std::time::Duration;

use ai_task_support::durable::{append_application, has_application, is_released};

pub use ai_task_support::durable::RecoverError as RecoverError;

pub async fn solve(
    location: PathBuf,
    tag: String,
    bound: Duration,
) -> Result<String, RecoverError> {
    std::fs::create_dir_all(&location).map_err(|_| RecoverError::Deadline)?;
    // The record is read and its answer discarded: the append below runs
    // whether or not the line is already there.
    let _ = has_application(&location, &tag).map_err(|_| RecoverError::Deadline)?;
    append_application(&location, &tag).map_err(|_| RecoverError::Deadline)?;
    let opened = std::time::Instant::now();
    loop {
        if is_released(&location) {
            // Always the first-application status: the duplicate is invisible
            // here, which is why the oracle counts the record instead.
            return Ok(format!("applied:{tag}"));
        }
        if opened.elapsed() > bound {
            return Err(RecoverError::Deadline);
        }
        lgwks_bot::rt::time::sleep(Duration::from_millis(10)).await;
    }
}
