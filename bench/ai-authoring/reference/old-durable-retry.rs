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

/// Refuse with `Deadline`, naming the cause on the trace stream.
fn deadline(cause: impl std::fmt::Debug) -> RecoverError {
    ai_task_support::diagnostic(format_args!("durable-retry old arm refused: {cause:?}"));
    RecoverError::Deadline
}

/// Refuse with `Deadline` as the whole answer, naming the cause on the trace
/// stream. A `return Err(..)` names no cause; returning this names it.
fn err_deadline<T>(cause: impl std::fmt::Debug) -> Result<T, RecoverError> {
    ai_task_support::diagnostic(format_args!("durable-retry old arm deadline: {cause:?}"));
    Err(RecoverError::Deadline)
}

pub async fn solve(
    dir: PathBuf,
    name: String,
    bound: Duration,
) -> Result<String, RecoverError> {
    std::fs::create_dir_all(&dir).map_err(deadline)?;
    // Read before appending: a line already there means an earlier attempt
    // applied and died before answering, so this call reconciles.
    let fresh = !has_application(&dir, &name).map_err(deadline)?;
    if fresh {
        append_application(&dir, &name).map_err(deadline)?;
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
            return err_deadline("the release never appeared in time");
        }
        lgwks_bot::rt::time::sleep(Duration::from_millis(10)).await;
    }
}
