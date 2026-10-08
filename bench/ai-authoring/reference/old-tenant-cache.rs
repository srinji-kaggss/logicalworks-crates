//! Reference solution — `tenant-cache`, OLD API (hand-rolled correctly).
//!
//! Without a facade the author holds the two invariants by hand: the map key
//! carries the tenant, and the token is compared before anything is read or
//! stored. Correct, and the comparison with the `new` arm is exactly the cost
//! of holding both by hand on every call — one missed check away from the
//! `futures` arm's leak.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// This task's refusal type.
pub use ai_task_support::tenant_cache::CacheError;

/// One entry per tenant per resource: the tenant is part of the key, so one
/// tenant's bytes are unreachable from another's by construction.
fn entries() -> &'static Mutex<HashMap<(String, String), Vec<u8>>> {
    static CACHED: OnceLock<Mutex<HashMap<(String, String), Vec<u8>>>> = OnceLock::new();
    CACHED.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Whether `token` opens `tenant`'s reads and nothing else.
fn allowed(tenant: &str, token: &str) -> bool {
    token == format!("token-for-{tenant}")
}

pub async fn solve(tenant: &str, resource: &str, token: &str) -> Result<Vec<u8>, CacheError> {
    if !allowed(tenant, token) {
        return Err(CacheError::Refused);
    }
    let mut held = match entries().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let slot = (tenant.to_owned(), resource.to_owned());
    if let Some(found) = held.get(&slot) {
        return Ok(found.clone());
    }
    let value = format!("value-of-{tenant}-{resource}").into_bytes();
    held.insert(slot, value.clone());
    Ok(value)
}
