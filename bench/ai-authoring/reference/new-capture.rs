//! Reference solution — `capture`, NEW API (task facade + supervised run).
//!
//! The sanctioned runner does the owning: `run_process` captures at most the
//! declared head bytes however much the child writes, stops the whole process
//! group past the deadline, and dropping the future stops the tree it named.
//! The solution maps that report onto the task's error vocabulary and adds
//! nothing of its own.

use std::num::NonZeroUsize;
use std::time::Duration;

/// The task's fixed vocabulary, re-exported: the oracle names these items,
/// and one definition serves every arm.
pub use ai_task_support::capture::{CaptureError, CaptureResult};
use lgwks_bot::rt::process::{ProcessRunError, ProcessSpec};
use lgwks_bot::rt::supervise::Supervisor;

/// Refuse with `Deadline` as the whole answer, naming the cause on the trace
/// stream. A `return Err(..)` names no cause; returning this names it.
fn err_deadline<T>(cause: impl std::fmt::Debug) -> Result<T, CaptureError> {
    ai_task_support::diagnostic(format_args!("capture new arm deadline: {cause:?}"));
    Err(CaptureError::Deadline)
}

pub async fn solve(
    argv: Vec<String>,
    head_limit: usize,
    deadline: Duration,
) -> Result<CaptureResult, CaptureError> {
    let mut parts = argv.into_iter();
    let program = parts.next().ok_or_else(|| CaptureError::Spawn("empty argv".to_owned()))?;
    let limit = NonZeroUsize::new(head_limit)
        .ok_or_else(|| CaptureError::Spawn("head_limit is zero".to_owned()))?;
    let mut spec = ProcessSpec::new(program);
    for arg in parts {
        spec.arg(arg);
    }
    // Both streams are captured so a chatty stderr cannot wedge the child on a
    // full pipe while nobody reads it.
    spec.capture_stdout(limit);
    spec.capture_stderr(limit);
    spec.deadline(deadline);
    let mut supervisor = Supervisor::new(4);
    match supervisor.run_process(&spec).await {
        Ok(run) => {
            if run.deadline_fired() {
                return err_deadline("the run outlived its deadline");
            }
            let captured = run.stdout();
            Ok(CaptureResult::new(
                captured.bytes().to_vec(),
                captured.total_bytes(),
                captured.truncated(),
            ))
        }
        Err(ProcessRunError::NotStarted { source }) => {
            Err(CaptureError::Spawn(source.to_string()))
        }
        Err(_) => Err(CaptureError::Cancelled),
    }
}
