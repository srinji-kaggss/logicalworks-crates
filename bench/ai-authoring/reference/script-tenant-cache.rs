//! Reference solution — `tenant-cache`, SCRIPT arm (the `script!` language).
//!
//! Authority still lives in the host's grants, but the read itself is a flow:
//! a `step` scope around the require and the value, called from a thin task
//! body. The flow is where an author would hang more steps — audit, fallback,
//! fan-out — without touching the admission boundary around it.

/// The task's refusal, aliased so the oracle resolves it.
pub use ai_task_support::tenant_cache::CacheError as CacheError;
use lgwks_bot::gate::GrantSet;
use lgwks_bot::script::Scope;
use lgwks_bot::task::{Host, task};

lgwks_bot::script! {
    /// Read one tenant's value under the run's authority.
    flow serve_value(tenant: String, resource: String) -> Vec<u8>:
        step authorised:
            scope.require(&[lgwks_bot::cap::Cap::new("cache.read")])?
            format!("value-of-{tenant}-{resource}").into_bytes()
}

pub async fn solve(tenant: &str, resource: &str, token: &str) -> Result<Vec<u8>, CacheError> {
    // The token buys the cache capability for the tenant's own token and
    // nothing otherwise; the decision is inline because this arm keeps the
    // whole admission where the host is built.
    let admitted = if token == format!("token-for-{tenant}") {
        GrantSet::empty().grant(lgwks_bot::cap::Cap::new("cache.read"))
    } else {
        GrantSet::empty()
    };
    let host = Host::builder(tenant)
        .map_err(|_| CacheError::Refused)?
        .grants(admitted)
        .build()
        .map_err(|_| CacheError::Refused)?;
    let owned_tenant = tenant.to_owned();
    let owned_resource = resource.to_owned();
    let work = task("serve", move |scope: Scope, _: ()| {
        let owned_tenant = owned_tenant.clone();
        let owned_resource = owned_resource.clone();
        async move { serve_value(&scope, owned_tenant, owned_resource).await }
    })
    .map_err(|_| CacheError::Refused)?;
    // Any non-success — Blocked, Failed, Cancelled — is a refusal: the value
    // is only readable under the run's authority.
    host.run(&work, ()).await.into_result().map_err(|_| CacheError::Refused)
}
