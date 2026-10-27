//! The coordinator: a lease holder that commits every dispatch row to the
//! durable store before the lane starts, fenced by whichever authority it
//! reaches — the one on this host, or one across an authenticated channel.

use std::fmt;

use super::{AuthorityHandle, ChannelError, Lease, LeaseError};
#[cfg(feature = "wire")]
use crate::tenant_store::{RunStart, TenantStore, TenantStoreError};

/// Why a coordinated grant was refused.
#[derive(Debug)]
#[non_exhaustive]
pub enum DispatchError {
    /// The lease is stale: a successor holds the grant, or it was revoked.
    /// Nothing was written.
    Lease(LeaseError),
    /// The store refused: the device, the lock, a bound, or a row that no
    /// longer matches. Nothing was half-written. With `wire`, which the store
    /// persists through.
    #[cfg(feature = "wire")]
    Store(TenantStoreError),
    /// The grant got no authenticated answer from a remote authority, so it
    /// is unvalidated, and an unvalidated grant is not a grant. Nothing was
    /// written; the caller waits and asks again rather than proceeding.
    Unvalidated(ChannelError),
}

impl fmt::Display for DispatchError {
    /// The refusal, in the vocabulary of the layer that raised it.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Lease(ref cause) => write!(formatter, "the grant is stale: {cause}"),
            #[cfg(feature = "wire")]
            Self::Store(ref cause) => write!(formatter, "the run store refused: {cause}"),
            Self::Unvalidated(ref cause) => write!(formatter, "the grant is unvalidated: {cause}"),
        }
    }
}

impl std::error::Error for DispatchError {
    /// The lease's, the store's or the channel's own error.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Lease(ref cause) => Some(cause),
            #[cfg(feature = "wire")]
            Self::Store(ref cause) => Some(cause),
            Self::Unvalidated(ref cause) => Some(cause),
        }
    }
}

impl From<LeaseError> for DispatchError {
    /// A stale lease reaches the caller as a stale grant, not as a store
    /// refusal: the store was never touched.
    fn from(cause: LeaseError) -> Self {
        Self::Lease(cause)
    }
}

#[cfg(feature = "wire")]
impl From<TenantStoreError> for DispatchError {
    /// A store refusal reaches the caller as itself, so a device fault never
    /// reads as a fencing decision.
    fn from(cause: TenantStoreError) -> Self {
        Self::Store(cause)
    }
}

impl From<ChannelError> for DispatchError {
    /// A lost or unauthenticated answer reaches the caller as unvalidated,
    /// never as a stale lease and never as a grant.
    fn from(cause: ChannelError) -> Self {
        Self::Unvalidated(cause)
    }
}

/// Where a coordinator's grants are checked: the one lease authority of its
/// coordination domain, on this host or across the network.
///
/// [`AuthorityHandle`] checks inline; under the `net` feature the TCP grant
/// channel asks the authority on another host. A grant is admitted only for
/// a current lease, and any other answer — including no answer — writes
/// nothing.
pub trait Fence {
    /// Admit `lease`, or say why not.
    ///
    /// # Errors
    ///
    /// [`DispatchError::Lease`] when the lease is stale;
    /// [`DispatchError::Unvalidated`] when the authority gave no
    /// authenticated answer.
    fn admit(&self, lease: &Lease) -> Result<(), DispatchError>;
}

impl Fence for AuthorityHandle {
    /// The inline check: a stale epoch is refused, and nothing else can fail.
    fn admit(&self, lease: &Lease) -> Result<(), DispatchError> {
        Ok(self.check(lease)?)
    }
}

/// One coordinator: a lease holder that commits every dispatch row to the
/// durable store before the lane starts and reconciles dead coordinators
/// after they die.
///
/// Every method takes the lease and checks it first: a stale coordinator
/// cannot begin, dispatch, end, finish, or reconcile, and a refused grant
/// writes nothing. The store is the [`TenantStore`](crate::tenant_store) the
/// coordinator was built over, so the run state that survives coordinator
/// loss is the keyed history, and a successor that reopens the same directory
/// sees every commit the dead coordinator acknowledged.
///
/// `F` is where every lease is checked: by default the [`AuthorityHandle`] on
/// this host, or any [`Fence`] through [`with_fence`](Self::with_fence).
#[cfg(feature = "wire")]
#[derive(Debug, Clone)]
pub struct Coordinator<F = AuthorityHandle> {
    /// Where every grant is checked.
    fence: F,
    /// The durable keyed store every row is committed to.
    store: TenantStore,
    /// The host this coordinator runs on, and reconciles for.
    host: String,
}

#[cfg(feature = "wire")]
impl Coordinator {
    /// A coordinator over `store` on `host`, with its own fresh authority.
    ///
    /// The authority is fresh per coordinator: two coordinators sharing one
    /// authority coordinate, and two coordinators each minting are two
    /// domains that must never share a store directory.
    #[must_use]
    pub fn new(store: TenantStore, host: &str) -> Self {
        Self::with_fence(store, host, AuthorityHandle::new())
    }

    /// A coordinator over `store` on `host`, joining the domain `handle`
    /// already validates.
    ///
    /// The multi-coordinator form of [`new`](Self::new): two coordinators over
    /// one store share one handle, so a takeover stales the predecessor's
    /// lease instead of forking the domain. Sharing a store across two
    /// handles is the split-brain this type refuses by construction — see the
    /// second-host obligations in the module documentation.
    #[must_use]
    pub fn with_authority(store: TenantStore, host: &str, authority: AuthorityHandle) -> Self {
        Self::with_fence(store, host, authority)
    }

    /// The authority this coordinator validates against, shared with the
    /// channels that carry its grants.
    #[must_use]
    pub fn handle(&self) -> AuthorityHandle {
        self.fence.clone()
    }

    /// Grant to the named coordinator, staling every outstanding lease.
    #[must_use]
    pub fn acquire(&self, holder: &str) -> Lease {
        self.fence.acquire(holder)
    }
}

#[cfg(feature = "wire")]
impl<F: Fence> Coordinator<F> {
    /// A coordinator over `store` on `host` whose every grant is checked by
    /// `fence` before the store is touched.
    ///
    /// The remote form: with a channel to an authority on another host, a
    /// partition withholds grants rather than letting this host decide alone.
    /// The store must be the domain's one store — the shared directory or a
    /// frame-for-frame replica, per the module documentation.
    #[must_use]
    pub fn with_fence(store: TenantStore, host: &str, fence: F) -> Self {
        Self {
            fence,
            store,
            host: host.to_owned(),
        }
    }

    /// The host this coordinator runs on.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Check the lease, then commit through `commit`.
    ///
    /// One gate for the five grants — begin, dispatch, end, finish, recover —
    /// so the fence cannot drift between them: the store is touched only past
    /// a current lease, and a stale or unvalidated one is refused before a
    /// byte moves.
    #[cfg(feature = "wire")]
    fn checked<T>(
        &self,
        lease: &Lease,
        commit: impl FnOnce() -> Result<T, TenantStoreError>,
    ) -> Result<T, DispatchError> {
        self.fence.admit(lease)?;
        Ok(commit()?)
    }

    /// Record a run and its lanes as pending, before any lane starts.
    ///
    /// # Errors
    ///
    /// [`DispatchError::Lease`] when the grant is stale;
    /// [`DispatchError::Store`] when the store refuses;
    /// [`DispatchError::Unvalidated`] when the fence gave no answer.
    #[cfg(feature = "wire")]
    pub fn begin_run(
        &self,
        lease: &Lease,
        run: &RunStart<'_>,
        lanes: &[&str],
    ) -> Result<(), DispatchError> {
        self.checked(lease, || self.store.begin_run(run, lanes))
    }

    /// Durable intent: the lane is about to start.
    ///
    /// Committed before the lane's process exists (V2-04): the caller starts
    /// the process only after this returns, so a coordinator killed at any
    /// instant leaves a record the next run reconciles.
    ///
    /// # Errors
    ///
    /// [`DispatchError::Lease`] when the grant is stale;
    /// [`DispatchError::Store`] when the row is gone or past dispatch;
    /// [`DispatchError::Unvalidated`] when the fence gave no answer.
    #[cfg(feature = "wire")]
    pub fn dispatch_lane(
        &self,
        lease: &Lease,
        run_id: &str,
        lane: usize,
    ) -> Result<(), DispatchError> {
        self.checked(lease, || self.store.lane_dispatched(run_id, lane))
    }

    /// The lane is terminal; its outcome is recorded once.
    ///
    /// # Errors
    ///
    /// [`DispatchError::Lease`] when the grant is stale;
    /// [`DispatchError::Store`] when the row is gone or already ended;
    /// [`DispatchError::Unvalidated`] when the fence gave no answer.
    #[cfg(feature = "wire")]
    pub fn end_lane(
        &self,
        lease: &Lease,
        run_id: &str,
        lane: usize,
        outcome: &str,
        detail: &str,
        tail: &[u8],
    ) -> Result<(), DispatchError> {
        self.checked(lease, || {
            self.store.lane_ended(run_id, lane, outcome, detail, tail)
        })
    }

    /// Store the sealed record, or `false` when the run already ended.
    ///
    /// # Errors
    ///
    /// [`DispatchError::Lease`] when the grant is stale;
    /// [`DispatchError::Store`] when the store refuses;
    /// [`DispatchError::Unvalidated`] when the fence gave no answer.
    #[cfg(feature = "wire")]
    pub fn finish_run(
        &self,
        lease: &Lease,
        run_id: &str,
        verdict: &str,
        finished_at: &str,
        record: &str,
        seal: &str,
    ) -> Result<bool, DispatchError> {
        self.checked(lease, || {
            self.store
                .finish_run(run_id, verdict, finished_at, record, seal)
        })
    }

    /// Close every unfinished run on `dead_host`, transactionally each.
    ///
    /// The fencing half of recovery: the lease is checked first, so a stale
    /// coordinator cannot reconcile — only the current grant may declare
    /// another coordinator dead. Dispatched-but-unended lanes close as
    /// unknown, never as a pass; undispatched lanes close as unmeasured.
    /// Returns how many runs were open.
    ///
    /// # Errors
    ///
    /// [`DispatchError::Lease`] when the grant is stale;
    /// [`DispatchError::Store`] when the store refuses;
    /// [`DispatchError::Unvalidated`] when the fence gave no answer.
    #[cfg(feature = "wire")]
    pub fn recover(
        &self,
        lease: &Lease,
        dead_host: &str,
        now: &str,
    ) -> Result<usize, DispatchError> {
        self.checked(lease, || {
            let mut closed = 0usize;
            for open in self.store.open_runs(dead_host)? {
                if self.store.abandon(open.run_id(), now)? {
                    closed = closed.saturating_add(1);
                }
            }
            Ok(closed)
        })
    }
}
