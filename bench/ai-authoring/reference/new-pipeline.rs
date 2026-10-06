//! Reference solution — `pipeline`, NEW API (`lgwks_bot::script`).
//!
//! `each` runs `fetch_a` and `fetch_b` concurrently, fails fast and drops the
//! sibling when one fails; `within` carries the deadline and drops the running
//! body when it expires.

use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use ai_task_support::{Published, Stage, StageName};
use lgwks_bot::script::{FlowError, Scope, Tenant, each, within};

#[derive(Debug)]
pub enum PipelineError {
    Stage { name: StageName },
    Deadline,
    Cancelled,
}

/// The caller's cancellation, naming why the tenant could not be opened.
fn cancelled(cause: impl std::fmt::Debug) -> PipelineError {
    ai_task_support::diagnostic(format_args!("the pipeline tenant was refused: {cause:?}"));
    PipelineError::Cancelled
}

/// Locks `mutex`, recovering from poisoning.
///
/// The guarded value is an `Option<StageName>` that is only ever written as a
/// whole `Some(name)` or left alone, so a panic elsewhere cannot leave it
/// half-written: poisoning records that some thread unwound while holding the
/// guard, not that the recorded stage is unreadable. Taking the poisoned guard's
/// value keeps the first stage the run actually failed at, which is the name the
/// caller is told.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Records `name` as the stage this run failed at, if none has been yet.
fn record(failed: &Mutex<Option<StageName>>, name: StageName) {
    let mut recorded = lock(failed);
    // The *first* failure names the run, so a later failure cannot overwrite the
    // stage the caller hears about.
    if recorded.is_none() {
        *recorded = Some(name);
    }
}

pub async fn solve(stage: Stage, deadline: Duration) -> Result<Published, PipelineError> {
    let tenant = Tenant::new("pipeline").map_err(cancelled)?;
    let outer_scope = Scope::root(tenant);
    let bound = NonZeroUsize::new(2).ok_or(PipelineError::Cancelled)?;
    let failed: Arc<Mutex<Option<StageName>>> = Arc::new(Mutex::new(None));

    let scope_for_await = outer_scope.clone();
    let stage_for_await = stage;
    let failed_for_await = Arc::clone(&failed);
    let outcome = within(&outer_scope, "deadline", deadline, async move {
        // Two handles to one cell: the fan-out's worker moves its own into every
        // concurrent fetch, and the serial stages below record through this one.
        // Both name the same run, so whichever fails first is the one recorded.
        let failed_for_stages = Arc::clone(&failed_for_await);
        let worker = {
            let stage = stage_for_await.clone();
            let failed = Arc::clone(&failed_for_await);
            move |_step: Scope, name: StageName| {
                let stage = stage.clone();
                let failed = Arc::clone(&failed);
                async move {
                    match stage.run(name).await {
                        Ok(artifact) => Ok(artifact.value()),
                        Err(_) => {
                            record(&failed, name);
                            Err(FlowError::failed("stage failed"))
                        }
                    }
                }
            }
        };
        each(
            &scope_for_await,
            "fetch",
            Some(bound),
            [StageName::FetchA, StageName::FetchB],
            worker,
        )
        .await?;
        // `combine` and `publish` can fail for their own sake — an input no
        // earlier stage produced — so their failures are recorded under their
        // own names rather than left unnamed for the caller to default.
        stage_for_await
            .run(StageName::Combine)
            .await
            .map_err(|error| {
                record(&failed_for_stages, StageName::Combine);
                FlowError::failed(format!("combine: {error}"))
            })?;
        let published = stage_for_await
            .run(StageName::Publish)
            .await
            .map_err(|error| {
                record(&failed_for_stages, StageName::Publish);
                FlowError::failed(format!("publish: {error}"))
            })?;
        Ok(Published::from(published))
    })
    .await;

    match outcome {
        Ok(published) => Ok(published),
        Err(FlowError::TimedOut { .. }) => Err(PipelineError::Deadline),
        Err(FlowError::Cancelled { .. }) => Err(PipelineError::Cancelled),
        Err(cause) => match *lock(&failed) {
            Some(name) => Err(PipelineError::Stage { name }),
            None => {
                // No stage failed, so the error came from the flow itself. A
                // default of `FetchA` here would blame a stage that ran and
                // succeeded.
                ai_task_support::diagnostic(format_args!(
                    "the pipeline flow failed with no stage having failed: {cause}"
                ));
                Err(PipelineError::Cancelled)
            }
        },
    }
}
