//! Reference solution — `pipeline`, OLD API (`lgwks_bot::rt`).
//!
//! The two fetches run concurrently through `join_all_bounded`; the error wave
//! is inspected in input order so a failure names its stage; `timeout` carries
//! the deadline and drops the whole pipeline when it expires.

use std::time::Duration;

use ai_task_support::{Artifact, Published, Stage, StageName};
use lgwks_bot::rt::task::join_all_bounded;
use lgwks_bot::rt::time;

#[derive(Debug)]
pub enum PipelineError {
    Stage { name: StageName },
    Deadline,
    Cancelled,
}

async fn run_stage(stage: Stage, name: StageName) -> Result<Artifact, PipelineError> {
    stage
        .run(name)
        .await
        .map_err(|_| PipelineError::Stage { name })
}

pub async fn solve(stage: Stage, deadline: Duration) -> Result<Published, PipelineError> {
    let work = async move {
        let names = [StageName::FetchA, StageName::FetchB];
        let futures: Vec<_> = names
            .into_iter()
            .map(|name| {
                let stage = stage.clone();
                async move { run_stage(stage, name).await }
            })
            .collect();
        let fetched = join_all_bounded(2, futures).await;
        for result in fetched {
            if let Err(error) = result {
                return Err(error);
            }
        }
        let _combined = run_stage(stage.clone(), StageName::Combine).await?;
        let published = run_stage(stage, StageName::Publish).await?;
        Ok(Published::from(published))
    };
    match time::timeout(deadline, work).await {
        Ok(result) => result,
        Err(_) => Err(PipelineError::Deadline),
    }
}
