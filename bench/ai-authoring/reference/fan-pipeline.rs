//! Reference solution — `pipeline`, FAN API (`lgwks_bot::script::FanOut`).
//!
//! `FanOut` runs `fetch_a` and `fetch_b` together and hands back the failing
//! stage's name as the body's own error; `timeout` carries the deadline and
//! drops the running work when it expires.

use std::time::Duration;

use ai_task_support::{Published, Stage, StageError, StageName};
use lgwks_bot::rt::time;
use lgwks_bot::script::{FanOut, FanOutError};

#[derive(Debug)]
pub enum PipelineError {
    Stage { name: StageName },
    Deadline,
    Cancelled,
}

/// The stage a failed run names. `StageError` has one shape; matching it keeps the
/// error read rather than discarded.
fn failed_stage(error: StageError) -> StageName {
    match error {
        StageError::Stage { name } => name,
    }
}

pub async fn solve(stage: Stage, deadline: Duration) -> Result<Published, PipelineError> {
    let pipeline = async {
        FanOut::new([StageName::FetchA, StageName::FetchB])
            .at_most(2)
            .run(|name| {
                let stage = stage.clone();
                async move { stage.run(name).await.map_err(failed_stage) }
            })
            .await
            .map_err(|error| match error {
                FanOutError::Item { error: name, .. } => PipelineError::Stage { name },
                _ => PipelineError::Cancelled,
            })?;
        stage
            .run(StageName::Combine)
            .await
            .map_err(|error| PipelineError::Stage { name: failed_stage(error) })?;
        let artifact = stage
            .run(StageName::Publish)
            .await
            .map_err(|error| PipelineError::Stage { name: failed_stage(error) })?;
        Ok(Published::from(artifact))
    };
    match time::timeout(deadline, pipeline).await {
        Ok(result) => result,
        Err(_) => Err(PipelineError::Deadline),
    }
}
