//! Reference solution — `durable-retry`, OLD API (hand-rolled record).
//!
//! The record is an ordinary file and the wait is an ordinary sleep loop: no
//! facade is needed for exactly-once when the oracle never applies
//! concurrently. What the facade would add — atomicity under concurrency —
//! the test conditions never exercise, which the file says plainly. The
//! comparison with the `futures` arm is the one line that matters: read the
//! record before appending, and a recovery reconciles instead of duplicating.

use std::path::PathBuf;
use std::time::Duration;

use ai_task_support::durable::{append_application, has_application, is_released};

pub use ai_task_support::durable::RecoverError; // old arm

pub async fn solve(
    dir: PathBuf,
    name: String,
    bound: Duration,
) -> Result<String, RecoverError> {
    std::fs::create_dir_all(&dir).map_err(|_| RecoverError::Deadline)?;
    // Read before appending: a line already there means an earlier attempt
    // applied and died before answering, so this call reconciles.
    let fresh = !has_application(&dir, &name).map_err(|_| RecoverError::Deadline)?;
    if fresh {
        append_application(&dir, &name).map_err(|_| RecoverError::Deadline)?;
    }
    let status = if fresh {
        format!("applied:{name}")
    } else {
        format!("recovered:{name}")
    };
    let started = std::time::Instant::now();
    loop {
        if is_released(&dir) {
            return Ok(status);
        }
        if started.elapsed() > bound {
            return Err(RecoverError::Deadline);
        }
        lgwks_bot::rt::time::sleep(Duration::from_millis(10)).await;
    }
}
