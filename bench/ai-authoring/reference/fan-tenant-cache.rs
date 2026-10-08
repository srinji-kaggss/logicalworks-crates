//! Reference solution — `tenant-cache`, FAN API (one-call fan-out).
//!
//! The lightweight end of the facade: no host, one item through the fan-out,
//! with the token checked before anything is read. Isolation is the same
//! property as the `new` arm's — nothing shared between tenants — reached
//! without per-call admission: the fan-out owns the single read the way it
//! would own a thousand.

/// The task's vocabulary, glob-re-exported so the oracle resolves its names.
pub use ai_task_support::tenant_cache::*;
use lgwks_bot::script::FanOut;

pub async fn solve(tenant: &str, resource: &str, token: &str) -> Result<Vec<u8>, CacheError> {
    if token != format!("token-for-{tenant}") {
        return Err(CacheError::Refused);
    }
    let who = tenant.to_owned();
    let what = resource.to_owned();
    let mut values = FanOut::new([(who, what)])
        .at_most(1)
        .run(|(tenant, resource)| async move {
            Ok::<_, CacheError>(format!("value-of-{tenant}-{resource}").into_bytes())
        })
        .await
        .map_err(|_| CacheError::Refused)?;
    values.pop().ok_or(CacheError::Refused)
}
