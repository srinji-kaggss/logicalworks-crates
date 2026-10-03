//! Reference solution — `aggregate`, NEW API (`lgwks_bot::script`).
//!
//! This is the hand-written proof that the task is solvable on the task/script
//! facade, and the dry-run's canned solution. It is intentionally short: the
//! point of the measurement is how much orchestration the author must write.

use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ai_task_support::Fetcher;
use lgwks_bot::script::{FlowError, Scope, Tenant, each, within};

#[derive(Debug)]
pub enum SolveError {
    Fetch { id: u32 },
    Deadline,
    Cancelled,
}

pub async fn solve(ids: Vec<u32>, fetch: Fetcher, deadline: Duration) -> Result<u64, SolveError> {
    let tenant = Tenant::new("aggregate").map_err(|_| SolveError::Cancelled)?;
    let outer_scope = Scope::root(tenant);
    let bound = NonZeroUsize::new(4).ok_or(SolveError::Cancelled)?;
    let failed: Arc<Mutex<u32>> = Arc::new(Mutex::new(0));

    let scope_for_await = outer_scope.clone();
    let fetch_for_await = fetch;
    let failed_for_await = Arc::clone(&failed);
    let outcome = within(&outer_scope, "deadline", deadline, async move {
        let worker = {
            let fetch = fetch_for_await.clone();
            let failed = Arc::clone(&failed_for_await);
            move |_step: Scope, id: u32| {
                let fetch = fetch.clone();
                let failed = Arc::clone(&failed);
                async move {
                    match fetch.fetch(id).await {
                        Ok(value) => Ok(value),
                        Err(_) => {
                            *failed
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) = id;
                            Err(FlowError::failed("fetch failed"))
                        }
                    }
                }
            }
        };
        let values = each(&scope_for_await, "fetch", Some(bound), ids, worker).await?;
        Ok(values.into_iter().sum())
    })
    .await;

    match outcome {
        Ok(sum) => Ok(sum),
        Err(FlowError::TimedOut { .. }) => Err(SolveError::Deadline),
        Err(FlowError::Cancelled { .. }) => Err(SolveError::Cancelled),
        Err(_) => {
            let id = *failed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Err(SolveError::Fetch { id })
        }
    }
}
