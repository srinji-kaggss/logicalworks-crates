//! Reference solution — `aggregate`, OLD API (`lgwks_bot::rt`).
//!
//! The old primitive bounds concurrency but does not fail fast, so the author
//! must start work in bounded waves of four and stop between them. That extra
//! burden is the point of the comparison.

use std::time::Duration;

use ai_task_support::{FetchError, Fetcher};
use lgwks_bot::rt::task::join_all_bounded;
use lgwks_bot::rt::time;

#[derive(Debug)]
pub enum SolveError {
    Fetch { id: u32 },
    Deadline,
    Cancelled,
}

pub async fn solve(ids: Vec<u32>, fetch: Fetcher, deadline: Duration) -> Result<u64, SolveError> {
    let work = async move {
        let mut sum: u64 = 0;
        let mut next: usize = 0;
        while next < ids.len() {
            let end = (next + 4).min(ids.len());
            let futures: Vec<_> = ids[next..end]
                .iter()
                .map(|&id| {
                    let fetcher = fetch.clone();
                    async move { fetcher.fetch(id).await }
                })
                .collect();
            let results = join_all_bounded(4, futures).await;
            next = end;
            for result in results {
                sum = sum.saturating_add(result.map_err(|error| SolveError::Fetch { id: error.id() })?);
            }
        }
        Ok(sum)
    };
    match time::timeout(deadline, work).await {
        Ok(result) => result,
        Err(_) => Err(SolveError::Deadline),
    }
}
