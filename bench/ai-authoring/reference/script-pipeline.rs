//! Reference solution — `pipeline`, SCRIPT arm (the `script!` language).
//!
//! The fetch pair is a flow: `together` runs both fetches concurrently and the
//! first failure stops the sibling, then `combine` and `publish` run in turn.
//! A shared cell records which stage failed first; the plain `solve` keeps the
//! tenant, the deadline and the error mapping, reading the cell only when the
//! flow itself cannot name the failure.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ai_task_support::{Published, Stage, StageName};
use lgwks_bot::script::{FlowError, Scope, Tenant, within};

/// The task's fixed stage error.
pub use ai_task_support::pipeline::PipelineError;

/// Remember the stage that failed first, leaving a recorded stage alone.
///
/// Whole-value assignment under the lock: the cell holds `None` or one name,
/// so a panic elsewhere cannot leave it half-written.
fn record_stage(failed: &Mutex<Option<StageName>>, name: StageName) {
    let mut guard = match failed.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if guard.is_none() {
        *guard = Some(name);
    }
}

/// Read the recorded stage, defaulting to cancellation when none failed.
///
/// A separate function rather than an inline match: the flow cannot name a
/// failure that never reached a stage, and blaming `FetchA` for the flow's
/// own error would indict a stage that ran and succeeded.
fn recorded_or_cancelled(failed: &Mutex<Option<StageName>>) -> PipelineError {
    match failed.lock() {
        Ok(guard) => match *guard {
            Some(name) => PipelineError::Stage { name },
            None => PipelineError::Cancelled,
        },
        Err(poisoned) => match *poisoned.into_inner() {
            Some(name) => PipelineError::Stage { name },
            None => PipelineError::Cancelled,
        },
    }
}

lgwks_bot::script! {
    /// Run one stage, recording its name when it fails.
    flow fetch_single(
        stage: ai_task_support::Stage,
        name: ai_task_support::StageName,
        failed: std::sync::Arc<std::sync::Mutex<Option<ai_task_support::StageName>>>,
    ) -> u64:
        let outcome = stage.run(name).await
        if outcome.is_err():
            record_stage(&failed, name)
            fail with "stage failed"
        give back outcome.or_fail()?.value()

    /// Run both fetches concurrently; the first failure stops the sibling.
    flow fetch_both(
        stage: ai_task_support::Stage,
        failed: std::sync::Arc<std::sync::Mutex<Option<ai_task_support::StageName>>>,
    ) -> (u64, u64):
        let other = stage.clone()
        let failed_a = failed.clone()
        together:
            let a = run fetch_single(stage, ai_task_support::StageName::FetchA, failed_a)
            let b = run fetch_single(other, ai_task_support::StageName::FetchB, failed)
        give back (a, b)

    /// Fetch, combine and publish in order.
    flow run_pipeline(
        stage: ai_task_support::Stage,
        failed: std::sync::Arc<std::sync::Mutex<Option<ai_task_support::StageName>>>,
    ) -> ai_task_support::Published:
        let worker = stage.clone()
        let failed_later = failed.clone()
        let _fetched = run fetch_both(worker, failed)
        let combined_outcome = stage.run(ai_task_support::StageName::Combine).await
        if combined_outcome.is_err():
            record_stage(&failed_later, ai_task_support::StageName::Combine)
            fail with "combine failed"
        let combined = combined_outcome.or_fail()?
        let published_outcome = stage.run(ai_task_support::StageName::Publish).await
        if published_outcome.is_err():
            record_stage(&failed_later, ai_task_support::StageName::Publish)
            fail with "publish failed"
        give back ai_task_support::Published::from(published_outcome.or_fail()?)
}

pub async fn solve(stage: Stage, deadline: Duration) -> Result<Published, PipelineError> {
    let tenant = Tenant::new("pipeline").map_err(|error| {
        ai_task_support::diagnostic(format_args!("pipeline script arm: tenant refused: {error:?}"));
        PipelineError::Cancelled
    })?;
    let outer = Scope::root(tenant);
    let failed: Arc<Mutex<Option<StageName>>> = Arc::new(Mutex::new(None));
    let failed_for_outcome = Arc::clone(&failed);
    let inner = outer.clone();
    let outcome = within(&outer, "deadline", deadline, async move {
        run_pipeline(&inner, stage, failed).await
    })
    .await;
    match outcome {
        Ok(published) => Ok(published),
        Err(FlowError::TimedOut { .. }) => Err(PipelineError::Deadline),
        Err(FlowError::Cancelled { .. }) => Err(PipelineError::Cancelled),
        Err(_) => Err(recorded_or_cancelled(&failed_for_outcome)),
    }
}
