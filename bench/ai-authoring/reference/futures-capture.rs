//! Reference solution — `capture`, FUTURES API (the ecosystem standard).
//!
//! The negative control: a plain blocking `Command::output` retains everything
//! the child writes and waits for every process holding the pipes, so a flood
//! overruns any bound, an orphaned grandchild outlives the call, and a sleep
//! past the deadline cannot be hurried. It must FAIL the oracle's bound,
//! orphan and deadline clauses, which is what makes the oracle's pass on the
//! other arms evidence about the guarantee rather than the task.

use std::time::Duration;

/// The task's vocabulary, glob-re-exported so the oracle resolves its names.
pub use ai_task_support::capture::*;

pub async fn solve(
    argv: Vec<String>,
    head_limit: usize,
    deadline: Duration,
) -> Result<CaptureResult, CaptureError> {
    let mut words = argv;
    if words.is_empty() {
        return Err(CaptureError::Spawn("empty argv".to_owned()));
    }
    let program = words.remove(0);
    let mut command = std::process::Command::new(program);
    for arg in words {
        command.arg(arg);
    }
    // Blocking, unbounded and unowned: the call returns when the pipes close,
    // with everything retained and nobody reaped.
    let started = std::time::Instant::now();
    let output = command
        .output()
        .map_err(|source| CaptureError::Spawn(source.to_string()))?;
    if started.elapsed() > deadline {
        return Err(CaptureError::Deadline);
    }
    let total = u64::try_from(output.stdout.len())
        .map_err(|_| CaptureError::Spawn("output too large to count".to_owned()))?;
    let kept: Vec<u8> = output.stdout.iter().copied().take(head_limit).collect();
    Ok(CaptureResult::new(kept, total, false))
}
