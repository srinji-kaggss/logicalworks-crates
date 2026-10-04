//! Reference solution — `pipeline`, NEW API (`lgwks_bot::script`).
//!
//! `each` runs `fetch_a` and `fetch_b` concurrently, fails fast and drops the
//! sibling when one fails; `within` carries the deadline and drops the running
//! body when it expires.

use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
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

pub async fn solve(stage: Stage, deadline: Duration) -> Result<Published, PipelineError> {
    let tenant = Tenant::new("pipeline").map_err(cancelled)?;
    let outer_scope = Scope::root(tenant);
    let bound = NonZeroUsize::new(2).ok_or(PipelineError::Cancelled)?;
    let failed: Arc<Mutex<Option<StageName>>> = Arc::new(Mutex::new(None));

    let scope_for_await = outer_scope.clone();
    let stage_for_await = stage;
    let failed_for_await = Arc::clone(&failed);
    let outcome = within(&outer_scope, "deadline", deadline, async move {
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
                            *failed
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(name);
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
        stage_for_await
            .run(StageName::Combine)
            .await
            .map_err(|error| FlowError::failed(format!("combine: {error}")))?;
        let published = stage_for_await
            .run(StageName::Publish)
            .await
            .map_err(|error| FlowError::failed(format!("publish: {error}")))?;
        Ok(Published::from(published))
    })
    .await;

    match outcome {
        Ok(published) => Ok(published),
        Err(FlowError::TimedOut { .. }) => Err(PipelineError::Deadline),
        Err(FlowError::Cancelled { .. }) => Err(PipelineError::Cancelled),
        Err(_) => {
            let name = *failed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Err(PipelineError::Stage {
                name: name.unwrap_or(StageName::FetchA),
            })
        }
    }
}
