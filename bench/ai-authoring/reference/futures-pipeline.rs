//! Reference solution — `pipeline`, FUTURES API (`futures::future`).
//!
//! `try_join` runs the two fetches concurrently and drops the survivor on the
//! first failure; the rest is sequential; `timeout` carries the deadline and drops
//! the whole pipeline when it expires.

use std::time::Duration;

use ai_task_support::{Artifact, Published, Stage, StageError, StageName};
use futures::future::try_join;
use lgwks_bot::rt::time::timeout;

#[derive(Debug)]
pub enum PipelineError {
    Stage { name: StageName },
    Deadline,
    Cancelled,
}

async fn run_stage(stage: &Stage, name: StageName) -> Result<Artifact, PipelineError> {
    stage.run(name).await.map_err(|error| match error {
        StageError::Stage { name } => PipelineError::Stage { name },
    })
}

pub async fn solve(stage: Stage, deadline: Duration) -> Result<Published, PipelineError> {
    let work = async {
        try_join(
            run_stage(&stage, StageName::FetchA),
            run_stage(&stage, StageName::FetchB),
        )
        .await?;
        run_stage(&stage, StageName::Combine).await?;
        run_stage(&stage, StageName::Publish).await.map(Published::from)
    };
    timeout(deadline, work)
        .await
        .map_err(|_| PipelineError::Deadline)?
}
