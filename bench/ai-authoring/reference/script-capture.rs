//! Reference solution — `capture`, SCRIPT arm (the `script!` language).
//!
//! The flow owns the run the way the `new` reference does, written as a flow:
//! a `step` scope around the supervised run, `fail with` for the two ways a
//! run never settles, and the outcome as data the plain `solve` maps onto the
//! task's errors. The `spawn:` / `cancelled` reason markers are this file's
//! own convention for carrying the run's verdict through `FlowError::Failed`,
//! which is the only error a flow returns.

use std::num::NonZeroUsize;
use std::time::Duration;

/// Re-exported task vocabulary for the oracle.
pub use ai_task_support::capture::CaptureResult;
/// Re-exported task error for the oracle.
pub use ai_task_support::capture::CaptureError;
use lgwks_bot::rt::process::{ProcessRunError, ProcessSpec};
use lgwks_bot::rt::supervise::Supervisor;
use lgwks_bot::script::{FlowError, Scope, Tenant};

lgwks_bot::script! {
    /// Run one command under supervision and hand back its outcome.
    flow supervised(
        program: String,
        args: Vec<String>,
        head_limit: usize,
        deadline: Duration,
    ) -> (Vec<u8>, u64, bool, bool):
        let limit = NonZeroUsize::new(head_limit).ok_or("zero head_limit").or_fail()?
        let mut detail = ProcessSpec::new(program)
        for arg in args:
            detail.arg(arg)
        detail.capture_stdout(limit)
        detail.capture_stderr(limit)
        detail.deadline(deadline)
        let mut owned = Supervisor::new(1)
        let outcome = owned.run_process(&detail).await
        if matches!(outcome, Err(ProcessRunError::NotStarted { .. })):
            fail with "spawn: the program did not start"
        if outcome.is_err():
            fail with "cancelled: stopped while running"
        let finished = outcome.or_fail()?
        let kept = finished.stdout()
        give back (
            kept.bytes().to_vec(),
            kept.total_bytes(),
            kept.truncated(),
            finished.deadline_fired(),
        )
}

pub async fn solve(
    argv: Vec<String>,
    head_limit: usize,
    deadline: Duration,
) -> Result<CaptureResult, CaptureError> {
    let (program, args) = match argv.split_first() {
        Some((first, rest)) => (first.clone(), rest.to_vec()),
        None => return Err(CaptureError::Spawn("empty argv".to_owned())),
    };
    let tenant = Tenant::new("capture").map_err(|error| CaptureError::Spawn(error.to_string()))?;
    let scope = Scope::root(tenant);
    match supervised(&scope, program, args, head_limit, deadline).await {
        Ok((head, total, truncated, fired)) => {
            if fired {
                return Err(CaptureError::Deadline);
            }
            Ok(CaptureResult::new(head, total, truncated))
        }
        Err(FlowError::Failed { reason, .. }) if reason.starts_with("spawn:") => {
            Err(CaptureError::Spawn(reason))
        }
        Err(FlowError::Failed { .. }) => Err(CaptureError::Cancelled),
        Err(FlowError::TimedOut { .. }) => Err(CaptureError::Deadline),
        Err(other) => Err(CaptureError::Spawn(other.to_string())),
    }
}
