//! Reference solution — `tenant-cache`, NEW API (per-tenant Host authority).
//!
//! Authority lives in the host's grants, not in an `if` the author remembers:
//! a valid token mints a grant for the cache capability and anything else
//! mints nothing, so the body's `require` is where an unauthorised call stops.
//! Isolation needs no map at all — each tenant's host scopes its own run, and
//! the value is derived from the tenant and resource the run was admitted for.

/// The task's fixed refusal.
pub use ai_task_support::tenant_cache::CacheError;
use lgwks_bot::cap::Cap;
use lgwks_bot::gate::GrantSet;
use lgwks_bot::script::{FlowError, Scope};
use lgwks_bot::task::{Host, task};

/// The capability a cache read reaches for.
const READ: &str = "cache.read";

/// The grants a token buys: the cache capability for the tenant's own token,
/// nothing for anything else.
fn grants_for(tenant: &str, token: &str) -> GrantSet {
    if token == format!("token-for-{tenant}") {
        GrantSet::empty().grant(Cap::new(READ))
    } else {
        GrantSet::empty()
    }
}

pub async fn solve(tenant: &str, resource: &str, token: &str) -> Result<Vec<u8>, CacheError> {
    let host = Host::builder(tenant)
        .map_err(|_| CacheError::Refused)?
        .grants(grants_for(tenant, token))
        .build()
        .map_err(|_| CacheError::Refused)?;
    let owned_tenant = tenant.to_owned();
    let owned_resource = resource.to_owned();
    let work = task("serve", move |scope: Scope, _: ()| {
        let tenant = owned_tenant.clone();
        let resource = owned_resource.clone();
        async move {
            scope.require(&[Cap::new(READ)])?;
            Ok::<_, FlowError>(format!("value-of-{tenant}-{resource}").into_bytes())
        }
    })
    .map_err(|_| CacheError::Refused)?;
    let report = host.run(&work, ()).await;
    if report.disposition().is_success() {
        report.into_result().map_err(|_| CacheError::Refused)
    } else {
        Err(CacheError::Refused)
    }
}
