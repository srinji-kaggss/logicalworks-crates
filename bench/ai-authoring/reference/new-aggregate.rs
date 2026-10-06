//! Reference solution — `aggregate`, NEW API (`lgwks_bot::script`).
//!
//! This is the hand-written proof that the task is solvable on the task/script
//! facade, and the dry-run's canned solution. It is intentionally short: the
//! point of the measurement is how much orchestration the author must write.

use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use ai_task_support::Fetcher;
use lgwks_bot::script::{FlowError, Scope, Tenant, each, within};

#[derive(Debug)]
pub enum SolveError {
    Fetch { id: u32 },
    Deadline,
    Cancelled,
}

/// The caller's cancellation, naming why the tenant could not be opened.
fn cancelled(cause: impl std::fmt::Debug) -> SolveError {
    ai_task_support::diagnostic(format_args!("the aggregate tenant was refused: {cause:?}"));
    SolveError::Cancelled
}

/// Locks `mutex`, recovering from poisoning.
///
/// The guarded value is an `Option<u32>` that is only ever written as a whole
/// `Some(id)` or left alone, so a panic elsewhere cannot leave it half-written:
/// poisoning records that some thread unwound while holding the guard, not that
/// the recorded id is unreadable. Taking the poisoned guard's value keeps the
/// first failure the run actually saw, which is the id the caller is told.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

pub async fn solve(ids: Vec<u32>, fetch: Fetcher, deadline: Duration) -> Result<u64, SolveError> {
    let tenant = Tenant::new("aggregate").map_err(cancelled)?;
    let outer_scope = Scope::root(tenant);
    let bound = NonZeroUsize::new(4).ok_or(SolveError::Cancelled)?;
    // `None` means no fetch has failed. A plain `0` would say fetch id 0 had
    // failed whether or not any fetch had, and a failure of the tenant itself
    // would be reported as a fetch error against an id nobody asked for.
    let failed: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));

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
                            let mut recorded = lock(&failed);
                            // The *first* failure names the run, so a later
                            // failure cannot overwrite the id the caller hears.
                            if recorded.is_none() {
                                *recorded = Some(id);
                            }
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
        Err(cause) => match *lock(&failed) {
            Some(id) => Err(SolveError::Fetch { id }),
            None => {
                // No fetch failed, so the error came from the fan-out itself and
                // naming a fetch id would blame a fetch that succeeded.
                ai_task_support::diagnostic(format_args!(
                    "the aggregate fan-out failed with no fetch having failed: {cause}"
                ));
                Err(SolveError::Cancelled)
            }
        },
    }
}
