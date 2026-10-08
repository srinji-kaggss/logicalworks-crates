//! Reference solution — `durable-retry`, FUTURES API (the ecosystem standard).
//!
//! The negative control: it appends on every call without ever reading the
//! record back, so a restart after a kill applies the effect a second time.
//! It must FAIL the oracle's exactly-once clauses, which is what makes the
//! oracle's pass on the other arms evidence about the idempotency record
//! rather than the task.

use std::path::PathBuf;
use std::time::Duration;

use ai_task_support::durable::{append_application, is_released};

pub use ai_task_support::durable::RecoverError; // futures arm

pub async fn solve(
    effect_dir: PathBuf,
    key: String,
    deadline: Duration,
) -> Result<String, RecoverError> {
    std::fs::create_dir_all(&effect_dir).map_err(|_| RecoverError::Deadline)?;
    // No read-before-apply: every call appends, so a recovery duplicates.
    append_application(&effect_dir, &key).map_err(|_| RecoverError::Deadline)?;
    let started = std::time::Instant::now();
    loop {
        if is_released(&effect_dir) {
            return Ok(format!("applied:{key}"));
        }
        if started.elapsed() > deadline {
            return Err(RecoverError::Deadline);
        }
        lgwks_bot::rt::time::sleep(Duration::from_millis(10)).await;
    }
}
