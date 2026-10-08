//! A cross-host lease with fencing epochs, a bounded dispatch queue, and the
//! durable run state behind distributed CI.
//!
//! Estate issue #319: `logical_ci` runs every lane on one host, and the
//! KEEL-SPEC puts the cross-host lease, queue and durable run state in scope
//! as estate gaps to be upstreamed. This module is that upstream: a lease
//! authority that mints fencing epochs, a [`WorkQueue`] that refuses
//! rather than grows, and a [`Coordinator`] that commits every dispatch row to
//! the [`TenantStore`](crate::tenant_store) before the lane starts and closes
//! dead coordinators' runs transactionally after they die.
//!
//! # The fencing rule
//!
//! One authority mints leases; every lease carries the epoch it was minted
//! under, and every grant — dispatch, end, finish, reconcile — is checked
//! against the authority's current epoch first. A second `acquire` (a new
//! coordinator taking over) or a `revoke` moves the epoch, and every lease
//! from before it is stale: presenting one is [`LeaseError::StaleEpoch`],
//! never a grant. A stale coordinator cannot dispatch, cannot finish, and
//! cannot reconcile — the AT07 property — because the check runs before the
//! store is touched, and a refused grant writes nothing.
//!
//! Epochs are monotone saturating counters, the same tradeoff the readiness
//! generations make (INV-BOT-60): they never go backwards, and saturation
//! reuses rather than wraps into the past. Leases do not expire by time,
//! because cross-host clocks skew and skew is unmeasured (INV-BOT-30):
//! revocation is explicit and epoch-bound, so a slow clock can delay nothing
//! and a fast clock can grant nothing early.
//!
//! # The queue's overflow strategy
//!
//! [`WorkQueue`] declares its capacity at construction and refuses past it:
//! its `try_push` hands the item back in
//! [`Overfull`] rather than dropping it, blocking it, or growing. Backpressure
//! is the caller's decision with the item in hand — retry, shed, or report —
//! and never a policy the queue discovers at 3am. Zero capacity is refused at
//! construction: a queue that holds nothing is a refusal, not a queue.
//!
//! # The distribution seam
//!
//! Single-host correctness is fully wired: the [`Coordinator`] validates the
//! lease inline against its [`AuthorityHandle`] and commits to the
//! [`TenantStore`](crate::tenant_store), so the durable run state that
//! survives coordinator loss is the keyed store, not a second log. Across
//! hosts the validation travels: [`GrantChannel`] carries a grant to the
//! authority and an answer back, and [`LoopbackChannel`] is the single-host
//! decision inline. A partition is a send that never arrives — the grant waits
//! rather than proceeds — which is what makes "no local grants during a
//! partition" the rule rather than the hope.
//!
//! # What a second executor host must implement
//!
//! 1. Share one [`TenantStore`](crate::tenant_store) directory (a shared
//!    filesystem) or replicate its record stream frame for frame. Every
//!    mutation is one synced frame; a host that replays the frames sees every
//!    commit.
//! 2. Run exactly one lease authority per coordination domain, behind one
//!    [`AuthorityHandle`]. Two authorities for one domain are two current
//!    epochs, and fencing cannot survive that: minting is the one thing that
//!    must not be distributed.
//! 3. Implement [`GrantChannel`] over the host transport with authenticated,
//!    per-sender-ordered epoch announcements. Validate every grant against
//!    the authority before acting on it — dispatch, end, finish, reconcile —
//!    and treat a validation that never arrives as a wait, never as a grant.
//! 4. Bound revoke-then-act windows by announcement delivery, and make the
//!    window safe through the store: a dispatched-but-unended lane reconciles
//!    as unknown, never as a pass, so a grant that slipped through just
//!    before its revocation arrived leaves a record, not a verdict.
//! 5. Never add expiry by wall time. A TTL bounds how long a partition can
//!    delay an observed revocation; it does not replace the epoch check, and
//!    a skewed clock must not be able to grant.
//!
//! [`WorkQueue`]: crate::dist_lease::WorkQueue
//! [`Coordinator`]: crate::dist_lease::Coordinator
//! [`LeaseError::StaleEpoch`]: crate::dist_lease::LeaseError::StaleEpoch
//! [`Overfull`]: crate::dist_lease::Overfull
//! [`AuthorityHandle`]: crate::dist_lease::AuthorityHandle
//! [`GrantChannel`]: crate::dist_lease::GrantChannel
//! [`LoopbackChannel`]: crate::dist_lease::LoopbackChannel
//!
//! ```rust
//! use lgwks_bot::dist_lease::AuthorityHandle;
//!
//! let authority = AuthorityHandle::new();
//! let first = authority.acquire("host-a");
//! let second = authority.acquire("host-b");
//! assert!(authority.check(&first).is_err(), "the first lease must go stale");
//! assert!(authority.check(&second).is_ok(), "the second lease must be current");
//! assert_eq!(authority.epoch(), second.epoch(), "the epoch must be the second mint");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::tenant_store::{RunStart, TenantStore, TenantStoreError};

/// Who holds a lease: the host and coordinator the authority granted to.
///
/// Opaque by construction — minted from a name, compared by value — so a
/// coordinator cannot forge another's identity by spelling it alike: the fence
/// is the epoch, and the name is what the refusal attributes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct HostId {
    /// The coordinator's name. Private because identity is minted, not edited.
    name: String,
}

impl HostId {
    /// The identity a coordinator presents when it acquires.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
        }
    }

    /// The coordinator's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// A grant to coordinate, valid under exactly one epoch.
///
/// Minted only by the authority's acquire and checked only by its check:
/// equality of epochs is validity, and nothing else is consulted, so a
/// skewed clock anywhere in the system cannot move the fence.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Lease {
    /// Who the authority granted to. Carried for attribution — logs,
    /// recovery, refusals — rather than for the check, which is the epoch.
    holder: HostId,
    /// The authority's epoch when this lease was minted.
    epoch: u64,
}

impl Lease {
    /// A lease exactly as the authority minted it.
    ///
    /// Not constructible by hand outside this module: a lease whose epoch the
    /// authority never issued would fence nothing.
    fn minted(holder: HostId, epoch: u64) -> Self {
        Self { holder, epoch }
    }

    /// Who the authority granted to.
    #[must_use]
    pub fn holder(&self) -> &HostId {
        &self.holder
    }

    /// The authority's epoch when this lease was minted.
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
}

/// Why a grant was refused.
///
/// A refusal is never absence: every arm is an error the caller must see, and
/// a refused grant writes nothing anywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LeaseError {
    /// The lease was minted under an epoch the authority has moved past: a
    /// successor took over, or the lease was revoked. The grant is refused;
    /// the store is untouched.
    StaleEpoch {
        /// The epoch the lease carries.
        presented: u64,
        /// The authority's current epoch.
        current: u64,
    },
}

impl fmt::Display for LeaseError {
    /// The refusal, naming both epochs so the caller sees how far behind its
    /// lease is.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::StaleEpoch { presented, current } => write!(
                formatter,
                "lease epoch {presented} is behind the authority's epoch {current}; \
                 a successor holds the grant, and this one is refused"
            ),
        }
    }
}

impl std::error::Error for LeaseError {}

/// The one minter of leases for a coordination domain.
///
/// In-process by design: minting is the one thing that must not be
/// distributed, so two authorities for one domain is the split-brain this
/// type exists to refuse by construction. Shared through
/// [`AuthorityHandle`]; never cloned, never copied.
#[derive(Debug)]
struct LeaseAuthority {
    /// The current epoch. Moves forward on every acquire and revoke, and
    /// saturates rather than wraps: a reused epoch after 2^64 mints is the
    /// documented tradeoff, not a second fence.
    epoch: u64,
    /// Who holds the current grant, if anyone.
    holder: Option<HostId>,
}

impl LeaseAuthority {
    /// A fresh authority: epoch zero, no holder, nothing granted.
    fn new() -> Self {
        Self {
            epoch: 0,
            holder: None,
        }
    }

    /// Grant to `holder`, moving the epoch past every lease minted before.
    ///
    /// Every outstanding lease goes stale at once: there is exactly one
    /// current grant, and it is this one.
    fn acquire(&mut self, holder: HostId) -> Lease {
        self.epoch = self.epoch.saturating_add(1);
        self.holder = Some(holder.clone());
        Lease::minted(holder, self.epoch)
    }

    /// Whether `lease` is the current grant.
    ///
    /// Epoch equality is the whole check: the authority mints every lease it
    /// will ever honor, so equal epochs imply the same mint and no holder
    /// comparison can add anything a second spelling of the epoch would not.
    fn check(&self, lease: &Lease) -> Result<(), LeaseError> {
        if lease.epoch == self.epoch {
            Ok(())
        } else {
            Err(LeaseError::StaleEpoch {
                presented: lease.epoch,
                current: self.epoch,
            })
        }
    }

    /// Withdraw the grant without naming a successor, moving the epoch past
    /// every outstanding lease.
    fn revoke(&mut self) {
        self.epoch = self.epoch.saturating_add(1);
        self.holder = None;
    }
}

/// The shared door onto the one lease authority for a coordination domain.
///
/// Cloning shares the authority through an `Arc`, so every coordinator and
/// every channel on this host validates against the same epoch rather than
/// each holding its own opinion about who may grant.
#[derive(Debug, Clone)]
pub struct AuthorityHandle {
    /// The one authority this host validates against.
    inner: Arc<Mutex<LeaseAuthority>>,
}

impl Default for AuthorityHandle {
    /// A fresh authority behind a fresh handle, as [`new`](Self::new).
    fn default() -> Self {
        Self::new()
    }
}

impl AuthorityHandle {
    /// A fresh authority behind a fresh handle: epoch zero, no holder.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(LeaseAuthority::new())),
        }
    }

    /// The authority, recovering a poison.
    ///
    /// Recovering is right for the reason it is right in `journal::owner`:
    /// no arm here panics while the lock is held, so a poison is a bug in an
    /// unrelated thread, and propagating it would brick an authority that is
    /// still perfectly able to fence.
    fn lock(&self) -> MutexGuard<'_, LeaseAuthority> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poison) => poison.into_inner(),
        }
    }

    /// Grant to the named coordinator, staling every outstanding lease.
    #[must_use]
    pub fn acquire(&self, holder: &str) -> Lease {
        self.lock().acquire(HostId::new(holder))
    }

    /// Whether `lease` is the current grant.
    ///
    /// # Errors
    ///
    /// [`LeaseError::StaleEpoch`] when the authority moved past the lease.
    pub fn check(&self, lease: &Lease) -> Result<(), LeaseError> {
        self.lock().check(lease)
    }

    /// Withdraw the grant without naming a successor.
    pub fn revoke(&self) {
        self.lock().revoke();
    }

    /// The authority's current epoch.
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.lock().epoch
    }

    /// Who holds the current grant, if anyone.
    #[must_use]
    pub fn holder(&self) -> Option<HostId> {
        self.lock().holder.clone()
    }
}

/// The wire between an executor host and the lease authority: a grant travels
/// there and an answer travels back.
///
/// The loopback implementation decides inline; a real second host implements
/// this over its transport. A partition is a validation that never arrives —
/// [`ChannelError`] — and the caller waits rather than proceeds, which is what
/// makes "no local grants during a partition" the rule rather than the hope.
pub trait GrantChannel {
    /// Whether `lease` is the authority's current grant.
    ///
    /// `Ok(true)` grants, `Ok(false)` refuses as stale, and `Err` means the
    /// question never arrived: the grant is unvalidated, not granted.
    ///
    /// # Errors
    ///
    /// [`ChannelError`] when the hosts cannot reach each other.
    fn validate(&self, lease: &Lease) -> Result<bool, ChannelError>;
}

/// Why a grant validation never answered: the hosts are partitioned.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChannelError {
    /// The private marker that keeps construction inside this module: a
    /// partition is observed, never minted.
    _private: (),
}

impl ChannelError {
    /// The partition, observed by a channel whose send never arrived.
    fn partitioned() -> Self {
        Self { _private: () }
    }
}

impl fmt::Display for ChannelError {
    /// The refusal, naming the fact rather than the cause: the grant is
    /// unvalidated, and an unvalidated grant is not a grant.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "the grant never reached the lease authority: the hosts are partitioned, \
             and an unvalidated grant is not a grant",
        )
    }
}

impl std::error::Error for ChannelError {}

/// The single-host channel: validation decided inline against the shared
/// authority, with a partition flag the simulation drives.
///
/// The flag is an [`AtomicBool`] rather than a rebuild because a partition is
/// a fault the test injects mid-run: the channel stays the same channel while
/// the network underneath it changes.
#[derive(Debug)]
pub struct LoopbackChannel {
    /// The authority validation decides against.
    handle: AuthorityHandle,
    /// Whether sends arrive. Set by the simulation; production never sets it.
    partitioned: AtomicBool,
}

impl LoopbackChannel {
    /// A channel over the shared authority, initially connected.
    #[must_use]
    pub fn new(handle: AuthorityHandle) -> Self {
        Self {
            handle,
            partitioned: AtomicBool::new(false),
        }
    }

    /// Inject or heal a partition: `true` drops every validation.
    pub fn set_partitioned(&self, partitioned: bool) {
        self.partitioned.store(partitioned, Ordering::SeqCst);
    }

    /// Whether validations currently arrive.
    #[must_use]
    pub fn partitioned(&self) -> bool {
        self.partitioned.load(Ordering::SeqCst)
    }
}

impl GrantChannel for LoopbackChannel {
    /// Validate inline, or refuse to answer while partitioned.
    ///
    /// # Errors
    ///
    /// [`ChannelError`] while the partition flag is set.
    fn validate(&self, lease: &Lease) -> Result<bool, ChannelError> {
        if self.partitioned() {
            return Err(ChannelError::partitioned());
        }
        Ok(self.handle.check(lease).is_ok())
    }
}

/// Why the queue refused.
///
/// A refusal is never a loss: the refused item travels back inside the error,
/// so backpressure is the caller's decision with the item in hand.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum QueueError {
    /// A queue that holds nothing is a refusal, not a queue.
    ZeroCapacity,
}

impl fmt::Display for QueueError {
    /// The refusal, naming the bound the caller asked for.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::ZeroCapacity => formatter
                .write_str("a work queue with zero capacity holds nothing; refusing to build it"),
        }
    }
}

impl std::error::Error for QueueError {}

/// A refused push, carrying the item back.
///
/// The item travels in the error rather than being dropped, blocked on, or
/// buffered past the bound: the queue's overflow strategy is refusal, stated
/// once in [`WorkQueue::try_push`], and this is the type that makes the caller
/// live with it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Overfull<T> {
    /// The item the queue would not take. Private because the queue stated
    /// the bound and the caller stated the item; readers take it back whole
    /// through [`into_item`](Self::into_item).
    item: T,
    /// The capacity that refused it.
    capacity: usize,
}

impl<T> Overfull<T> {
    /// The refused push, naming what would not fit and where.
    fn refused(item: T, capacity: usize) -> Self {
        Self { item, capacity }
    }

    /// Take the refused item back.
    #[must_use]
    pub fn into_item(self) -> T {
        self.item
    }

    /// The capacity that refused the item.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

impl<T> fmt::Display for Overfull<T> {
    /// The refusal, naming the bound. The item is not rendered: a report must
    /// not carry work a reader did not ask to see.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "the work queue is full at capacity {}; the item was refused, not dropped",
            self.capacity
        )
    }
}

impl<T: fmt::Debug> std::error::Error for Overfull<T> {}

/// A bounded dispatch queue: the lanes admitted but not yet granted.
///
/// Capacity is declared at construction and enforced at every push; the queue
/// never grows past it, never blocks, and never drops. What it holds is the
/// caller's business — run ids, lane indices, sealed envelopes — and the bound
/// is the queue's.
#[derive(Debug, Clone)]
pub struct WorkQueue<T> {
    /// The admitted items, oldest first. Private because the bound is the
    /// queue's to keep: every push and pop goes through the capacity check.
    queue: VecDeque<T>,
    /// The most items the queue ever holds.
    capacity: usize,
}

impl<T> WorkQueue<T> {
    /// A queue that holds at most `capacity` items.
    ///
    /// # Errors
    ///
    /// [`QueueError::ZeroCapacity`] when `capacity` is zero.
    pub fn new(capacity: usize) -> Result<Self, QueueError> {
        if capacity == 0 {
            return Err(QueueError::ZeroCapacity);
        }
        Ok(Self {
            queue: VecDeque::with_capacity(capacity),
            capacity,
        })
    }

    /// Admit `item`, or hand it back when the queue is full.
    ///
    /// The backpressure door: a full queue refuses with the item inside
    /// [`Overfull`], and the caller retries, sheds, or reports with the work
    /// still in hand.
    pub fn try_push(&mut self, item: T) -> Result<(), Overfull<T>> {
        if self.queue.len() >= self.capacity {
            return Err(Overfull::refused(item, self.capacity));
        }
        self.queue.push_back(item);
        Ok(())
    }

    /// Take the oldest admitted item, or `None` when the queue is empty.
    ///
    /// Empty is not an error: a queue with nothing admitted has nothing to
    /// refuse and nothing to hand back.
    #[must_use]
    pub fn pop(&mut self) -> Option<T> {
        self.queue.pop_front()
    }

    /// How many items the queue holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Whether the queue holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Whether the queue holds all it can.
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.queue.len() >= self.capacity
    }

    /// The most items the queue ever holds.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

/// Why a coordinated grant was refused.
#[derive(Debug)]
#[non_exhaustive]
pub enum DispatchError {
    /// The lease is stale: a successor holds the grant, or it was revoked.
    /// Nothing was written.
    Lease(LeaseError),
    /// The store refused: the device, the lock, a bound, or a row that no
    /// longer matches. Nothing was half-written.
    Store(TenantStoreError),
}

impl fmt::Display for DispatchError {
    /// The refusal, in the vocabulary of the layer that raised it.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Lease(ref cause) => write!(formatter, "the grant is stale: {cause}"),
            Self::Store(ref cause) => write!(formatter, "the run store refused: {cause}"),
        }
    }
}

impl std::error::Error for DispatchError {
    /// The lease's or the store's own error.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Lease(ref cause) => Some(cause),
            Self::Store(ref cause) => Some(cause),
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

impl From<TenantStoreError> for DispatchError {
    /// A store refusal reaches the caller as itself, so a device fault never
    /// reads as a fencing decision.
    fn from(cause: TenantStoreError) -> Self {
        Self::Store(cause)
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
#[derive(Debug, Clone)]
pub struct Coordinator {
    /// The authority every grant is checked against.
    authority: AuthorityHandle,
    /// The durable keyed store every row is committed to.
    store: TenantStore,
    /// The host this coordinator runs on, and reconciles for.
    host: String,
}

impl Coordinator {
    /// A coordinator over `store` on `host`, with its own fresh authority.
    ///
    /// The authority is fresh per coordinator: two coordinators sharing one
    /// authority coordinate, and two coordinators each minting are two
    /// domains that must never share a store directory.
    #[must_use]
    pub fn new(store: TenantStore, host: &str) -> Self {
        Self {
            authority: AuthorityHandle::new(),
            store,
            host: host.to_owned(),
        }
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
        Self {
            authority,
            store,
            host: host.to_owned(),
        }
    }

    /// The authority this coordinator validates against, shared with the
    /// channels that carry its grants.
    #[must_use]
    pub fn handle(&self) -> AuthorityHandle {
        self.authority.clone()
    }

    /// The host this coordinator runs on.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Grant to the named coordinator, staling every outstanding lease.
    #[must_use]
    pub fn acquire(&self, holder: &str) -> Lease {
        self.authority.acquire(holder)
    }

    /// Check the lease, then commit through `commit`.
    ///
    /// One gate for the five grants — begin, dispatch, end, finish, recover —
    /// so the fence cannot drift between them: the store is touched only past
    /// a current lease, and a stale one is refused before a byte moves.
    fn checked<T>(
        &self,
        lease: &Lease,
        commit: impl FnOnce() -> Result<T, TenantStoreError>,
    ) -> Result<T, DispatchError> {
        self.authority.check(lease)?;
        Ok(commit()?)
    }

    /// Record a run and its lanes as pending, before any lane starts.
    ///
    /// # Errors
    ///
    /// [`DispatchError::Lease`] when the grant is stale;
    /// [`DispatchError::Store`] when the store refuses.
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
    /// [`DispatchError::Store`] when the row is gone or past dispatch.
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
    /// [`DispatchError::Store`] when the row is gone or already ended.
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
    /// [`DispatchError::Store`] when the store refuses.
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
    /// [`DispatchError::Store`] when the store refuses.
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
