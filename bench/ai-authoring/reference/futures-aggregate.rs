//! Reference solution — `aggregate`, FUTURES API (`futures::stream`).
//!
//! The ecosystem-standard answer: `buffer_unordered` bounds the fan-out and
//! `try_fold` stops at the first failure, dropping every fetch still running.
//! `buffer_unordered` rather than `buffered` so a fast failure is observed even
//! while an earlier, slower fetch is still in flight.

use std::time::Duration;

use ai_task_support::Fetcher;
use futures::{StreamExt, TryStreamExt, stream};
use lgwks_bot::rt::time::timeout;

#[derive(Debug)]
pub enum SolveError {
    Fetch { id: u32 },
    Deadline,
    Cancelled,
}

pub async fn solve(ids: Vec<u32>, fetch: Fetcher, deadline: Duration) -> Result<u64, SolveError> {
    let work = stream::iter(ids)
        .map(|id| {
            let fetch = fetch.clone();
            async move {
                fetch
                    .fetch(id)
                    .await
                    .map_err(|error| SolveError::Fetch { id: error.id() })
            }
        })
        .buffer_unordered(4)
        .try_fold(0_u64, |sum, value| async move { Ok(sum + value) });
    timeout(deadline, work)
        .await
        .ok()
        .ok_or(SolveError::Deadline)?
}
