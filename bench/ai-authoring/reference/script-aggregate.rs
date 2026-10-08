//! Reference solution — `aggregate`, SCRIPT arm (the `script!` language).
//!
//! The fan-out is a flow: `each` bounded by a named width, with the first
//! failure recorded into a cell the flow takes as an argument. The plain
//! `solve` keeps the deadline, the tenant and the error mapping; the flow
//! keeps the orchestration. A `record_first_failure` helper holds the only
//! lock, so the flow body stays straight-line Rust with no branching past one
//! `if`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The error type this task resolves.
pub use ai_task_support::aggregate::SolveError as SolveError;
use ai_task_support::Fetcher;
use lgwks_bot::script::{FlowError, Scope, Tenant, within};

/// Remember the first id whose fetch failed, ignoring every later one.
///
/// Whole-value assignment under the lock: the cell holds `None` or one id, so
/// a panic elsewhere cannot leave it half-written, and taking the poisoned
/// guard's value keeps the failure the run actually saw first.
fn record_first_failure(failed: &Mutex<Option<u32>>, id: u32) {
    let mut guard = match failed.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if guard.is_none() {
        *guard = Some(id);
    }
}

lgwks_bot::script! {
    /// Fetch every id, four at a time, and sum the values.
    flow gather(
        ids: Vec<u32>,
        fetch: ai_task_support::Fetcher,
        failed: std::sync::Arc<std::sync::Mutex<Option<u32>>>,
        width: usize,
    ) -> u64:
        let partials = each id in ids, at most (width) at once:
            let outcome = fetch.fetch(id).await
            if outcome.is_err():
                record_first_failure(&failed, id)
                fail with "fetch failed"
            outcome.or_fail()?
        give back partials.into_iter().sum()
}

pub async fn solve(ids: Vec<u32>, fetch: Fetcher, deadline: Duration) -> Result<u64, SolveError> {
    let tenant = Tenant::new("aggregate").map_err(|_| SolveError::Cancelled)?;
    let outer = Scope::root(tenant);
    let inner = outer.clone();
    let failed: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
    let failed_for_outcome = Arc::clone(&failed);
    let outcome = within(&outer, "deadline", deadline, async move {
        gather(&inner, ids, fetch, failed, 4).await
    })
    .await;
    if let Err(FlowError::TimedOut { .. }) = outcome {
        return Err(SolveError::Deadline);
    }
    if let Err(FlowError::Cancelled { .. }) = outcome {
        return Err(SolveError::Cancelled);
    }
    if outcome.is_ok() {
        return outcome.map_err(|_| SolveError::Cancelled);
    }
    match failed_for_outcome.lock() {
        Ok(guard) => match *guard {
            Some(id) => Err(SolveError::Fetch { id }),
            None => Err(SolveError::Cancelled),
        },
        Err(poisoned) => match *poisoned.into_inner() {
            Some(id) => Err(SolveError::Fetch { id }),
            None => Err(SolveError::Cancelled),
        },
    }
}
