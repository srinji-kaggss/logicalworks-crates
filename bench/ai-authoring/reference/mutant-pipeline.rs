//! Deliberately wrong solution — `pipeline`, detached spawn.
//!
//! This is a mutant input for the harness, not an example. It is the OLD-API
//! reference solution, included verbatim as `mod reference` (the runner copies
//! `reference/old-pipeline.rs` beside it), with exactly one mutation: the
//! reference's `solve` runs on a `JoinSet` handed to a process-lifetime holder
//! instead of being owned by the call, so dropping this `solve` leaves the stages
//! running. The oracle's `dropping_the_future_leaves_no_stage_live` test must
//! fail it, and only that clause, since the work itself is the reference's.

mod reference;

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use ai_task_support::{Published, Stage};
use lgwks_bot::rt::sync::oneshot;
use lgwks_bot::rt::task::JoinSet;

pub use reference::PipelineError;

/// The mutation: task sets that outlive every call that started them.
static DETACHED: OnceLock<Mutex<Vec<JoinSet<()>>>> = OnceLock::new();

pub async fn solve(stage: Stage, deadline: Duration) -> Result<Published, PipelineError> {
    let mut set = JoinSet::new();
    let (sender, receiver) = oneshot::channel();
    set.spawn(async move {
        let _ = sender.send(reference::solve(stage, deadline).await);
    });
    if let Ok(mut held) = DETACHED.get_or_init(|| Mutex::new(Vec::new())).lock() {
        held.push(set);
    }
    match receiver.await {
        Ok(result) => result,
        Err(_) => Err(PipelineError::Cancelled),
    }
}
