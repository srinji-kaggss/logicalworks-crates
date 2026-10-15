//! The admission lease a task holds for its whole run.

#[cfg(feature = "script")]
use super::tenancy_support;
use crate::rt::sync::OwnedSemaphorePermit;
#[cfg(feature = "script")]
use crate::script::Tenant;
#[cfg(feature = "script")]
use std::sync::Arc;
/// One in-flight admission: the permit, and — when the supervisor was built
/// with a [`TenancyPolicy`] — the tenant the permit is charged to.
///
/// The permit and the tenant travel together because their lifetimes are one
/// fact: a task that ends returns its permit *to the round* rather than to the
/// pool, and the round needs to know which tenant just freed a slot. A
/// supervisor without a policy never charges a tenant, so its leases carry no
/// charge and their drop is exactly the bare permit's drop.
#[cfg(feature = "script")]
pub(super) struct Lease {
    /// The pool permit, taken out of the pool when the admission was granted,
    /// and taken out of the lease only by its own drop.
    permit: Option<OwnedSemaphorePermit>,
    /// The tenant this admission is charged to, when the supervisor installed a
    /// policy. `None` for every untenanted supervisor.
    charge: Option<TenantCharge>,
}

/// A build without tenancy has no round, so its lease is the permit alone: held
/// for the task's life and returned to the pool by its own drop.
#[cfg(not(feature = "script"))]
pub(super) struct Lease {
    /// The pool permit, held only so that dropping the lease releases it.
    _permit: OwnedSemaphorePermit,
}

/// The tenant half of a [`Lease`], handed to the round when the lease drops.
#[cfg(feature = "script")]
pub(super) struct TenantCharge {
    /// The shell that admitted this lease.
    shell: Arc<tenancy_support::TenancyShell>,
    /// The tenant charged for the permit.
    tenant: Tenant,
}

#[cfg(feature = "script")]
impl Lease {
    /// A lease for a supervisor with no policy: dropping it returns the permit
    /// to the pool, exactly as the raw permit did.
    pub(super) fn plain(permit: OwnedSemaphorePermit) -> Self {
        Self {
            permit: Some(permit),
            charge: None,
        }
    }

    /// A lease charged to `tenant`, so its drop returns the permit to the round
    /// rather than to the pool.
    pub(super) fn tenanted(
        permit: OwnedSemaphorePermit,
        shell: Arc<tenancy_support::TenancyShell>,
        tenant: Tenant,
    ) -> Self {
        Self {
            permit: Some(permit),
            charge: Some(TenantCharge { shell, tenant }),
        }
    }
}

/// The plain constructor for a `sync`-only build, where there is no tenancy and
/// the lease is the permit and nothing else.
impl Lease {
    /// A lease for a supervisor with no policy.
    #[cfg(not(feature = "script"))]
    pub(super) fn plain(permit: OwnedSemaphorePermit) -> Self {
        Self { _permit: permit }
    }
}

/// Only a build with tenancy has a round to hand a permit to. Without `script`
/// there is no `Drop` at all: the lease is its `permit` field, and that field's
/// own drop returns the permit to the pool.
#[cfg(feature = "script")]
impl Drop for Lease {
    /// Hand a tenanted lease's permit to the round, which decides whose waiter
    /// it serves next. An untenanted lease does nothing here: its `permit` field
    /// drops with it and the permit returns to the pool.
    fn drop(&mut self) {
        if let Some(charge) = self.charge.take()
            && let Some(permit) = self.permit.take()
        {
            charge.shell.release(&charge.tenant, permit);
        }
    }
}

/// The tenant a call that named none is charged to.
///
/// One reserved name, so the untenanted calls share a ceiling and a queue rather
/// than reaching every tenant's permit — which is what "untenanted calls behave
/// as one implicit tenant" has to mean for the accounting to be readable. The
/// leading underscore marks it as the crate's own rather than any caller's.
///
/// Only consulted when a [`TenancyPolicy`] is installed: without one there is no
/// per-tenant state at all and every call shares the single pool, exactly as it
/// always did.
#[cfg(feature = "script")]
pub(super) const IMPLICIT_TENANT: &str = "_implicit";

/// The tenant an untenanted call is charged to.
#[cfg(feature = "script")]
pub(super) fn implicit_tenant() -> Tenant {
    Tenant::assumed(IMPLICIT_TENANT)
}
