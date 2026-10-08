//! Reference solution — `capture`, FAN API (`FanOut` + supervised run).
//!
//! The fan-out surface owns the deadline where the `new` reference hands it
//! to the spec: one item, at most one at a time, within the task's deadline.
//! A `TimedOut` fan-out is the task's `Deadline`, and the supervisor still
//! owns the tree underneath it.

use std::num::NonZeroUsize;
use std::time::Duration;

/// Re-exported vocabulary so the oracle's `ai_trial::CaptureError` resolves.
pub use ai_task_support::capture::CaptureError;
/// Re-exported vocabulary so the oracle's `ai_trial::CaptureResult` resolves.
pub use ai_task_support::capture::CaptureResult;
use lgwks_bot::rt::process::{ProcessRunError, ProcessSpec};
use lgwks_bot::rt::supervise::Supervisor;
use lgwks_bot::script::{FanOut, FanOutError};

pub async fn solve(
    argv: Vec<String>,
    head_limit: usize,
    deadline: Duration,
) -> Result<CaptureResult, CaptureError> {
    let mut words = argv.into_iter();
    let binary = words.next().ok_or_else(|| CaptureError::Spawn("empty argv".to_owned()))?;
    let ceiling = NonZeroUsize::new(head_limit)
        .ok_or_else(|| CaptureError::Spawn("head_limit is zero".to_owned()))?;
    let rest: Vec<String> = words.collect();
    // One item: the deadline belongs to the fan-out, not the spec, so a slow
    // child is stopped by the surface that bounded it.
    let runs = FanOut::new([rest])
        .at_most(1)
        .within(deadline)
        .run(|args| {
            let binary = binary.clone();
            async move {
                let mut detail = ProcessSpec::new(binary);
                for arg in args {
                    detail.arg(arg);
                }
                detail.capture_stdout(ceiling);
                detail.capture_stderr(ceiling);
                let mut owned = Supervisor::new(1);
                owned.run_process(&detail).await.map_err(|failure| match failure {
                    ProcessRunError::NotStarted { source } => source.to_string(),
                    _ => "the supervisor stopped before the run settled".to_owned(),
                })
            }
        })
        .await
        .map_err(|failure| match failure {
            FanOutError::TimedOut { .. } => CaptureError::Deadline,
            FanOutError::Item { error, .. } => CaptureError::Spawn(error),
            _ => CaptureError::Cancelled,
        })?;
    let run = runs.into_iter().next().ok_or(CaptureError::Cancelled)?;
    if run.deadline_fired() {
        return Err(CaptureError::Deadline);
    }
    let kept = run.stdout();
    Ok(CaptureResult::new(
        kept.bytes().to_vec(),
        kept.total_bytes(),
        kept.truncated(),
    ))
}
