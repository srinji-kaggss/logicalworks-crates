//! Reference solution — `aggregate`, FAN API (`lgwks_bot::script::FanOut`).
//!
//! One call: bounded, fail-fast, drop-safe, with the failing id coming back as
//! the body's own error. No scope, no tenant, no shared cell.

use std::time::Duration;

use ai_task_support::Fetcher;
use lgwks_bot::script::{FanOut, FanOutError};

#[derive(Debug)]
pub enum SolveError {
    Fetch { id: u32 },
    Deadline,
    Cancelled,
}

pub async fn solve(ids: Vec<u32>, fetch: Fetcher, deadline: Duration) -> Result<u64, SolveError> {
    FanOut::new(ids)
        .at_most(4)
        .within(deadline)
        .run(|id| {
            let fetch = fetch.clone();
            async move { fetch.fetch(id).await.map_err(|error| error.id()) }
        })
        .await
        .map(|values| values.into_iter().sum())
        .map_err(|error| match error {
            FanOutError::Item { error: id, .. } => SolveError::Fetch { id },
            FanOutError::TimedOut { .. } => SolveError::Deadline,
            _ => SolveError::Cancelled,
        })
}
