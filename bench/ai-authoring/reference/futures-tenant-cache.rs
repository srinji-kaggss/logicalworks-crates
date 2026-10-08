//! Reference solution — `tenant-cache`, FUTURES API (the ecosystem standard).
//!
//! The negative control: one process-wide map keyed by the resource alone,
//! with the token accepted unread. The first tenant to ask for a resource
//! decides what every later tenant observes, and any token opens any tenant's
//! reads. It must FAIL the oracle's isolation and refusal clauses, which is
//! what makes the oracle's pass on the other arms evidence about the
//! guarantee rather than the task.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// The task's fixed refusal.
pub use ai_task_support::tenant_cache::CacheError;

/// The whole cache: resource to bytes, with no tenant anywhere in the key and
/// no authority anywhere in the call.
fn shared() -> &'static Mutex<HashMap<String, Vec<u8>>> {
    static CACHED: OnceLock<Mutex<HashMap<String, Vec<u8>>>> = OnceLock::new();
    CACHED.get_or_init(|| Mutex::new(HashMap::new()))
}

pub async fn solve(tenant: &str, resource: &str, _token: &str) -> Result<Vec<u8>, CacheError> {
    let mut held = match shared().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(found) = held.get(resource) {
        return Ok(found.clone());
    }
    let value = format!("value-of-{tenant}-{resource}").into_bytes();
    held.insert(resource.to_owned(), value.clone());
    Ok(value)
}
