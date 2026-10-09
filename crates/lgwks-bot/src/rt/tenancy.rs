//! Per-tenant capacity: the policy, the typed refusals, and the pure
//! deficit-round-robin core that decides which tenant a freed permit belongs
//! to.
//!
//! A runtime that serves many tenants has to isolate three things: identity,
//! data and **capacity**. [`Supervisor`](crate::rt::supervise::Supervisor)
//! admission was built around the first two generations of that problem — one
//! pool of permits, one ceiling — so a single tenant that submits ten thousand
//! slow tasks fills the pool and every other tenant waits behind it. That is
//! the noisy-neighbour failure, and no amount of per-call bounds
//! ([`each`](crate::script::each), [`FanOut`](crate::script::FanOut)) prevents
//! it, because they bound one call's fan-out rather than one tenant's share of
//! the supervisor.
//!
//! This module owns the pieces of the fix, split the way
//! [`RetryPolicy`](lgwks_std::retry::RetryPolicy) is split: a **pure policy**
//! and a **pure scheduler** any executor can drive, with no I/O anywhere.
//!
//! - [`TenancyPolicy`] — the per-tenant in-flight ceiling, the bounded
//!   per-tenant queue, and the per-tenant weights.
//! - [`DeficitRoundRobin`] — the scheduler. Deficit round robin
//!   (Shreedhar–Varghese) is the fair-queuing algorithm Kubernetes API
//!   Priority and Fairness builds on: each backlogged tenant holds a *deficit*
//!   that grows by its weight on its turn and is spent one permit at a time, so
//!   a tenant's share of freed permits converges on its share of the total
//!   weight while any tenant has work waiting. A decision is O(log T) work over
//!   the tenant map plus amortized-O(1) work over the ring.
//! - [`SpawnRefused`] — the typed refusal a tenant past its queue bound
//!   receives, naming the tenant and the bound, so a caller learns which tenant
//!   is loud and what easing it would take.
//!
//! The async half — the parked waiters, the permit handoff, the release hook
//! that runs when a supervised task ends — lives in
//! [`rt::supervise`](crate::rt::supervise), which wraps this core behind one
//! mutex. Everything here is deterministic and synchronous, which is what lets
//! the seeded simulation family drive the real scheduling decisions rather than
//! a copy of them.
//!
//! # What the ceiling isolates and what it does not
//!
//! `per_tenant_limit` bounds how many of the supervisor's permits **one
//! tenant** may hold at once. With a pool of `N` permits and a limit of `L`, one
//! tenant can strand at most `L` of them; `N - L` remain reachable by every
//! other tenant however loudly the first one submits. The global ceiling stays
//! what it always was — the bound on total in-flight work — and the
//! per-tenant ceilings sit inside it. A `per_tenant_limit` at or above the
//! supervisor's own bound isolates nothing, and that is the caller's
//! declaration rather than this module's to rewrite.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::num::NonZeroU32;

use crate::script::Tenant;

/// The most waiting admissions one tenant may hold.
///
/// A ceiling of a million is a ceiling in name only, so the number a caller may
/// write is itself bounded: this is the same bound [`each`](crate::script::each)
/// places on one fan-out's in-flight bodies. A tenant past the bound receives
/// [`SpawnRefused::TenantAtCapacity`] naming it, and the other tenants' queues
/// are untouched.
pub const MAX_QUEUE_PER_TENANT: usize = 65_536;

/// The heaviest weight one tenant may carry.
///
/// The weight is a *relative* share, and a weight that dwarfs every other
/// tenant's would make the fairness the algorithm exists for unobservable: a
/// tenant weighted 1,000 beside tenants weighted 1 takes a thousand permits per
/// turn, which is a reservation wearing a scheduler's name. A caller who needs
/// that should raise the per-tenant ceiling instead — the honest tool for "this
/// tenant is allowed to be big".
pub const MAX_TENANT_WEIGHT: u32 = 1_000;

/// The most waiting admissions one supervisor may hold across every tenant.
///
/// A per-tenant queue bound is not a supervisor bound: tenants multiply, and any
/// tenant name creates an entry, so a policy of per-tenant bounds admits
/// `tenants x queue_per_tenant` waiting work for a caller willing to name that
/// many tenants. This is the number that bounds the supervisor's own memory, and
/// it is a policy field rather than a derived product because a real deployment
/// knows its fleet size and its host and a test knows neither.
pub const MAX_TOTAL_QUEUE: usize = 65_536;

/// How many tenants one policy may name a weight for.
///
/// The default weight is 1 and needs no entry, so this bounds only the explicit
/// map. Bounded so a policy is a fixed, inspectable object rather than an
/// unbounded one a caller grows by accident; 5,000 is the estate's declared
/// tenant-provision tier.
pub const MAX_WEIGHTED_TENANTS: usize = 5_000;

/// Why a [`TenancyPolicy`] could not be built as asked.
///
/// Each arm names the exact dimension that refused, with the bound that was
/// exceeded, so the repair is a number the caller can compare against rather
/// than a message to parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TenancyError {
    /// The policy already names a weight for
    /// [`MAX_WEIGHTED_TENANTS`] tenants.
    TooManyWeightedTenants {
        /// The bound that was exceeded.
        limit: usize,
    },
    /// The weight is above [`MAX_TENANT_WEIGHT`].
    WeightTooHigh {
        /// The weight that was refused.
        weight: u32,
        /// The bound it exceeded.
        limit: u32,
    },
}

impl fmt::Display for TenancyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::TooManyWeightedTenants { limit } => {
                write!(
                    formatter,
                    "the policy names weights for more than {limit} tenants"
                )
            }
            Self::WeightTooHigh { weight, limit } => {
                write!(
                    formatter,
                    "a tenant weight of {weight} exceeds the bound of {limit}"
                )
            }
        }
    }
}

impl std::error::Error for TenancyError {}

/// How many of one supervisor's permits a tenant may hold at once, how many
/// admissions it may park waiting, and how heavy it is in the round.
///
/// Built through [`TenancyPolicy::new`] and [`TenancyPolicy::with_weight`]
/// rather than a struct literal: the queue bound and the weight map have
/// ceilings a literal could silently exceed, and a policy is the one object
/// that must describe the deal every tenant actually gets.
///
/// ```
/// use std::num::NonZeroU32;
///
/// use lgwks_bot::rt::tenancy::TenancyPolicy;
/// use lgwks_bot::script::Tenant;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let heavy = Tenant::new("acme")?;
/// let two: NonZeroU32 = 2_u32.try_into()?;
/// let policy = TenancyPolicy::new(8, 64).with_weight(&heavy, two)?;
/// // An unweighted tenant defaults to 1; the bounds are readable back.
/// assert_eq!(policy.per_tenant_limit(), 8);
/// assert_eq!(policy.queue_per_tenant(), 64);
/// assert_eq!(u32::from(policy.weight_of(&heavy)), 2);
/// assert_eq!(u32::from(policy.weight_of(&Tenant::new("quiet")?)), 1);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TenancyPolicy {
    /// How many in-flight admissions one tenant may hold at once. Zero at
    /// construction reads as one, the same reading
    /// [`Supervisor::new`](crate::rt::supervise::Supervisor::new) gives its own
    /// bound.
    per_tenant_limit: usize,
    /// How many waiting admissions one tenant may park. A bound of zero is
    /// legal and refuses every contended arrival, which is a real policy — a
    /// tenant that must never queue — rather than a mistake to rewrite.
    queue_per_tenant: usize,
    /// The most waiting admissions the supervisor holds across every tenant.
    /// A bound of zero is legal and refuses every contended arrival, which is a
    /// real policy -- a supervisor that must never queue -- rather than a
    /// mistake to rewrite.
    queue_total: usize,
    /// The DRR weight per tenant, defaulting to 1. A `BTreeMap` so the policy
    /// iterates in one order everywhere, which is what makes the seeded
    /// simulation of the scheduler reproducible -- and so the scheduler's own
    /// per-tenant state can use the same map, at O(log T) a lookup.
    weights: BTreeMap<Tenant, NonZeroU32>,
}

impl TenancyPolicy {
    /// A policy of `per_tenant_limit` in-flight admissions and
    /// `queue_per_tenant` waiting admissions per tenant, every tenant weighted
    /// 1.
    ///
    /// A `per_tenant_limit` of zero reads as one, and a `queue_per_tenant`
    /// above [`MAX_QUEUE_PER_TENANT`] is clamped to it — the same reading and
    /// the same clamp [`Supervisor::new`](crate::rt::supervise::Supervisor::new)
    /// applies to its own ceiling, and for the same reason: there is no
    /// argument that produces an unbounded tenant. The clamped values are what
    /// the accessors report, so a caller never believes it wrote a bound nobody
    /// enforced.
    #[must_use]
    pub fn new(per_tenant_limit: usize, queue_per_tenant: usize) -> Self {
        Self {
            per_tenant_limit: per_tenant_limit.max(1),
            queue_per_tenant: queue_per_tenant.min(MAX_QUEUE_PER_TENANT),
            queue_total: MAX_TOTAL_QUEUE,
            weights: BTreeMap::new(),
        }
    }

    /// Hold `queue_total` waiting admissions across every tenant.
    ///
    /// Additive on `Self`, so a policy is built where it is declared rather than
    /// mutated where it is used. A value of zero reads as the smallest queue a
    /// supervisor can have, and one above [`MAX_TOTAL_QUEUE`] is clamped to it,
    /// for the same reason [`TenancyPolicy::new`] clamps the per-tenant bound:
    /// there is no argument that produces an unbounded supervisor, and the
    /// accessors report what the supervisor actually holds.
    #[must_use]
    pub fn with_queue_total(mut self, queue_total: usize) -> Self {
        self.queue_total = queue_total.min(MAX_TOTAL_QUEUE);
        self
    }

    /// Carry `weight` for `tenant` in the round.
    ///
    /// The weight is relative: a tenant weighted 2 receives twice the freed
    /// permits of a tenant weighted 1 while both have work waiting, and a
    /// tenant with no entry is weighted 1. Additive on `Self`, so a policy is
    /// built where it is declared rather than mutated where it is used.
    ///
    /// # Errors
    ///
    /// [`TenancyError::WeightTooHigh`] past [`MAX_TENANT_WEIGHT`], and
    /// [`TenancyError::TooManyWeightedTenants`] when `tenant` is a new name that
    /// would take the map past [`MAX_WEIGHTED_TENANTS`]. A weight for a tenant
    /// already named replaces the old one and never refuses on the count.
    pub fn with_weight(
        mut self,
        tenant: &Tenant,
        weight: NonZeroU32,
    ) -> Result<Self, TenancyError> {
        let declared = weight.get();
        if declared > MAX_TENANT_WEIGHT {
            let refusal = Err(TenancyError::WeightTooHigh {
                weight: declared,
                limit: MAX_TENANT_WEIGHT,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "with_weight: returning an error to the caller");
            return refusal;
        }
        if !self.weights.contains_key(tenant) && self.weights.len() >= MAX_WEIGHTED_TENANTS {
            let refusal = Err(TenancyError::TooManyWeightedTenants {
                limit: MAX_WEIGHTED_TENANTS,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "with_weight: returning an error to the caller");
            return refusal;
        }
        self.weights.insert(tenant.clone(), weight);
        Ok(self)
    }

    /// How many in-flight admissions one tenant may hold at once.
    #[must_use]
    pub const fn per_tenant_limit(&self) -> usize {
        self.per_tenant_limit
    }

    /// How many waiting admissions one tenant may park.
    #[must_use]
    pub const fn queue_per_tenant(&self) -> usize {
        self.queue_per_tenant
    }

    /// The most waiting admissions one supervisor holds across every tenant.
    #[must_use]
    pub const fn queue_total(&self) -> usize {
        self.queue_total
    }

    /// The tenant's weight, 1 for a tenant the policy does not name.
    #[must_use]
    pub fn weight_of(&self, tenant: &Tenant) -> NonZeroU32 {
        match self.weights.get(tenant).copied() {
            Some(weight) => weight,
            None => NonZeroU32::MIN,
        }
    }
}

/// Why a tenant-scoped spawn did not start the work it was handed.
///
/// The two arms name different worlds, exactly as
/// [`TrySpawnRefusal`](crate::rt::supervise::TrySpawnRefusal) does for the
/// untenanted doors: [`Self::TenantAtCapacity`] means the *tenant* is healthy
/// but its queue is at the declared bound — the refusal names the tenant and
/// the bound, so a caller can tell which tenant is loud and what easing it
/// would take — and [`Self::Cancelled`] means the supervisor has stopped
/// admitting and the work should go somewhere else entirely.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SpawnRefused {
    /// The tenant's bounded queue is full: it already holds `limit` waiting
    /// admissions. Other tenants are unaffected.
    TenantAtCapacity {
        /// The tenant whose queue is at its bound.
        tenant: Tenant,
        /// The bound the tenant's queue reached.
        limit: usize,
    },
    /// The supervisor's own waiting bound is reached across every tenant. The
    /// tenant named in the call may still have room, so this arm carries the
    /// bound rather than the tenant: the load is the supervisor's and no single
    /// tenant can be named as the loud one.
    SupervisorQueueFull {
        /// The supervisor's declared waiting bound.
        limit: usize,
    },
    /// The supervisor has been cancelled and admits no new work.
    Cancelled,
}

impl fmt::Display for SpawnRefused {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::TenantAtCapacity { ref tenant, limit } => write!(
                formatter,
                "tenant {tenant} already holds {limit} waiting admissions"
            ),
            Self::SupervisorQueueFull { limit } => write!(
                formatter,
                "the supervisor already holds {limit} waiting admissions across its tenants"
            ),
            Self::Cancelled => {
                formatter.write_str("the supervisor is cancelled and admits no new work")
            }
        }
    }
}

impl std::error::Error for SpawnRefused {}

/// What one arrival into [`DeficitRoundRobin::try_arrive`] decided.
///
/// Two arms and no waiter: this is the door a caller that refuses rather than
/// waits comes through, and it can leave nothing behind. An arrival that cannot
/// be admitted at once is [`Self::Contended`] — the caller's own term for "the
/// supervisor is full", never a queue entry the scheduler would have to clean
/// up later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TryArrival<P> {
    /// Admitted at once: nobody is waiting, the tenant is under its ceiling and
    /// the pool had a permit. The caller owns the permit.
    Immediate(P),
    /// Not admitted: the pool is spent, the tenant is at its ceiling, or
    /// another tenant is already waiting. Nothing was retained.
    Contended,
}

/// What one arrival into [`DeficitRoundRobin::arrive`] decided.
/// A sum type rather than a `Result` because a queued arrival is not an error:
/// it is the ordinary outcome under load, and the caller's next move — park a
/// waker, or return a future that resolves when a permit lands — differs from
/// both an immediate admission and a refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Arrival<P> {
    /// Admitted at once: nobody was waiting, the tenant was under its ceiling,
    /// and `acquire` produced the permit. The caller owns the permit.
    Immediate(P),
    /// Parked in the tenant's bounded queue, behind the tenants already
    /// waiting. The caller owns the waiter and must be ready to receive one
    /// permit.
    Queued,
    /// The tenant's queue is at its bound; nothing was retained. `limit` is the
    /// bound that was reached.
    Refused {
        /// The tenant's declared queue bound.
        limit: usize,
    },
    /// The supervisor's own waiting bound is reached across every tenant, so
    /// this tenant still has room and is refused anyway; nothing was retained.
    /// `limit` is the supervisor's declared bound.
    ///
    /// A separate arm rather than another [`Self::Refused`] because the two name
    /// different owners of the load: one says this tenant is loud, the other says
    /// the supervisor is full and every tenant's share of the queue is spent.
    SupervisorFull {
        /// The supervisor's declared waiting bound.
        limit: usize,
    },
}

/// One permit handed to the tenant the round chose.
///
/// The waiter moves out with the permit, so the executor side delivers both to
/// the parked waker in one step; the scheduler holds no reference to either
/// afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Grant<W, P> {
    /// The tenant whose turn it was.
    pub tenant: Tenant,
    /// The head of that tenant's queue.
    pub waiter: W,
    /// The permit the executor offered.
    pub permit: P,
}

/// What [`DeficitRoundRobin::grant`] did with the permit it was offered.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum GrantOutcome<W, P> {
    /// The permit went to the tenant the round chose. That tenant's in-flight
    /// count is already charged; the caller delivers the permit to the waiter.
    Granted(Grant<W, P>),
    /// No tenant could take the permit — nobody is waiting under its ceiling —
    /// and the permit is handed back untouched, for the caller to return to the
    /// pool.
    Idle(P),
}

/// The fair-queuing state one tenant retains while it is not idle.
///
/// Private to the scheduler: a caller that could edit a deficit could hand
/// itself a turn, and the point of this type is that the order is decided by
/// the algorithm and nothing else.
#[derive(Debug)]
struct Entry<W> {
    /// Waiting admissions, oldest first.
    queue: VecDeque<W>,
    /// Waiters in `queue` whose owner has gone away, not yet skipped by a
    /// grant. They cost nothing to keep until the next grant passes them, and
    /// counting them separately is what keeps the queue bound honest — a bound
    /// that counted abandoned waiters would refuse live work on behalf of
    /// callers that left.
    abandoned: usize,
    /// Admissions currently charged to this tenant. Bounded by the policy's
    /// per-tenant ceiling by construction: it rises only in `arrive`'s
    /// immediate path and in `grant`, both of which check the ceiling first.
    in_flight: usize,
    /// The tenant's unspent turn, in permits. Grows by the weight when a turn
    /// opens, shrinks by one per grant, resets when the queue empties.
    deficit: u32,
    /// Whether a turn is open for this tenant — it is at the front of the ring
    /// with deficit remaining.
    turn_open: bool,
    /// Whether the tenant is on the active ring. Only tenants with waiters
    /// **under their ceiling** are on the ring; a tenant at its ceiling parks
    /// off it until one of its admissions completes.
    on_ring: bool,
}

impl<W> Default for Entry<W> {
    /// An idle entry: nothing waiting, nothing in flight, no turn, off the
    /// ring.
    ///
    /// Written by hand rather than derived, because the derive would demand
    /// `W: Default` and a waiter is whatever the executor parked — a wait
    /// slot, a number in a simulation — none of which is default-constructible
    /// and none of which needs to be.
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            abandoned: 0,
            in_flight: 0,
            deficit: 0,
            turn_open: false,
            on_ring: false,
        }
    }
}

impl<W> Entry<W> {
    /// Waiters still owned by somebody.
    fn live(&self) -> usize {
        self.queue.len().saturating_sub(self.abandoned)
    }

    /// Whether nothing of this tenant is retained.
    fn is_idle(&self) -> bool {
        self.queue.is_empty() && self.in_flight == 0 && !self.on_ring
    }
}

/// Deficit round robin over per-tenant queues: which tenant a freed permit
/// belongs to.
///
/// The scheduler is pure on purpose. It owns the queues, the deficits and the
/// active ring, and it never owns a permit: permits are handed in by the
/// executor ([`grant`](Self::grant)) and handed back ([`GrantOutcome::Idle`]
/// when nobody can take one), so the same type drives a live
/// [`Supervisor`](crate::rt::supervise::Supervisor) and
/// a seeded simulation with a counter for a pool — the simulation exercises the
/// real decision, not a copy of it.
///
/// # The algorithm
///
/// Each tenant with waiting work and room under its ceiling sits on an **active
/// ring**. A freed permit opens, or continues, the turn of the tenant at the
/// front: that tenant's *deficit* grows by its weight and the permit goes to
/// its head waiter, spending one unit of deficit. While deficit remains and the
/// queue does not, the tenant stays at the front — so a tenant weighted 2
/// takes two permits per turn against a tenant weighted 1, and the shares
/// converge on the weights. When the deficit is spent, the queue empties, or the
/// tenant reaches its ceiling, the turn closes and the tenant moves to the back
/// or off the ring.
///
/// # What one decision costs
///
/// **O(log T) plus amortized O(1)**, where `T` is the number of tenants with
/// state. The two halves are separate and neither is hidden in the other:
///
/// - The *ring* work is amortized O(1) in `T`. A ring member is there because it
///   had a live waiter and room, so a step past a member happens only when that
///   member's entry is retired or its abandoned heads are skipped — both paid
///   for by the arrival or the abandonment that created them. Tenants at their
///   ceilings are *not* on the ring; they are re-armed by
///   [`note_release`](Self::note_release), the one other event that can make a
///   tenant servable.
/// - The *map* work is O(log T) per lookup, and every decision does one: the
///   entries map is a [`BTreeMap`], so a tenant name costs a descent rather
///   than a hash. That is the price of an order two runs agree on, which is what
///   makes this scheduler reproducible, and it is paid deliberately rather than
///   avoided. Two orders of magnitude separate the two terms at the estate's
///   largest declared fleet (5,000 tenants: a descent is about thirteen
///   comparisons against a thirteen-step ring walk), so the ring is not what
///   dominates at the tiers this crate is built for.
///
/// `arrive` and `grant` additionally clone the tenant name onto the ring and
/// into the grant, which is a copy of the name rather than a constant, and
/// [`note_release`](Self::note_release) and [`note_abandoned`](Self::note_abandoned)
/// do one lookup each. A caller measuring this measures a name copy as well as
/// the comparisons.
///
/// # What it does not decide
///
/// The global ceiling. The executor's `acquire` closure is the pool: when it
/// returns `None` the arrival queues, and the scheduler never admits past what
/// the pool allowed. A supervisor of `N` permits with per-tenant ceilings of
/// `L` leaves `N - L` permits permanently reachable by every tenant but one,
/// which is the isolation property the noisy-neighbour tests measure.
#[derive(Debug)]
pub struct DeficitRoundRobin<W> {
    /// The ceilings, queue bounds and weights.
    policy: TenancyPolicy,
    /// One entry per tenant that has arrived and is not fully idle, so the map
    /// is bounded by *live* tenants rather than by lifetime tenants.
    entries: BTreeMap<Tenant, Entry<W>>,
    /// Tenants with waiting work under their ceiling, in turn order.
    ring: VecDeque<Tenant>,
    /// Waiters retained across every tenant, abandoned ones included. The
    /// supervisor's own memory, and therefore what the policy's total bound is
    /// checked against. A count rather than a sum over the entries because a sum
    /// is O(tenants) on every decision and this is read on every arrival.
    retained: usize,
}

impl<W> DeficitRoundRobin<W> {
    /// A scheduler enforcing `policy`.
    #[must_use]
    pub fn new(policy: TenancyPolicy) -> Self {
        Self {
            policy,
            entries: BTreeMap::new(),
            ring: VecDeque::new(),
            retained: 0,
        }
    }

    /// The ceilings, queue bounds and weights this round admits under, fixed
    /// when it was built; a refusal reads its limit from here so the number a
    /// caller is told is the number that was enforced.
    #[must_use]
    pub fn policy(&self) -> &TenancyPolicy {
        &self.policy
    }

    /// Whether any tenant could take a permit right now.
    ///
    /// The executor consults this before pulling a permit from the pool, so a
    /// contended pool is never drained by lookers. `true` is a promise that a
    /// permit *may* be grantable, not that it will be: a ring entry can turn
    /// out to hold only abandoned waiters, in which case the next
    /// [`grant`](Self::grant) skips them and answers [`GrantOutcome::Idle`].
    #[must_use]
    pub fn has_eligible(&self) -> bool {
        !self.ring.is_empty()
    }

    /// How many admissions are charged to `tenant` right now.
    #[must_use]
    pub fn in_flight_of(&self, tenant: &Tenant) -> usize {
        self.entries.get(tenant).map_or(0, |entry| entry.in_flight)
    }

    /// How many of `tenant`'s waiting admissions are still owned by somebody.
    #[must_use]
    pub fn queued_of(&self, tenant: &Tenant) -> usize {
        self.entries.get(tenant).map_or(0, Entry::live)
    }

    /// How many waiters `tenant` still holds, abandoned ones included.
    ///
    /// Not the same number as [`queued_of`](Self::queued_of): an abandoned
    /// waiter stops counting against the queue bound immediately, but it stays
    /// in the deque until a grant skips it, so this is the memory the scheduler
    /// is actually retaining. Reported so a test can assert that the retention
    /// stays bounded by the policy rather than growing with the number of
    /// callers that walked away.
    #[must_use]
    pub fn retained_of(&self, tenant: &Tenant) -> usize {
        self.entries
            .get(tenant)
            .map_or(0, |entry| entry.queue.len())
    }

    /// How many waiters this scheduler retains across every tenant.
    ///
    /// The load the supervisor's own memory bound is checked against, and the
    /// honest answer to "how full is this supervisor's queue".
    #[must_use]
    pub fn retained_total(&self) -> usize {
        self.retained
    }

    /// How many tenants hold at least one waiter, abandoned or not.
    ///
    /// The honest load number for a report: it counts the queues the scheduler
    /// still walks, which is the memory it holds, rather than only the live
    /// waiters.
    #[must_use]
    pub fn tenants_with_queues(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| !entry.queue.is_empty())
            .count()
    }

    /// Register one arrival for `tenant`, taking a permit immediately when
    /// nobody is waiting and the tenant is under its ceiling.
    ///
    /// `acquire` is the pool: it returns the permit an immediate admission
    /// consumes, or `None` when the pool is spent — in which case the arrival
    /// queues like any other. The immediate path runs only while **no** tenant
    /// is waiting, so a fresh arrival can never jump a queue that already
    /// exists; the moment anybody waits, every arrival joins its own tenant's
    /// queue and the round decides.
    pub fn arrive<P, F>(&mut self, tenant: &Tenant, waiter: W, acquire: F) -> Arrival<P>
    where
        F: FnOnce() -> Option<P>,
    {
        self.arrive_with(tenant, || waiter, acquire)
    }

    /// [`arrive`](Self::arrive), building the waiter only when the arrival
    /// actually parks.
    ///
    /// An arrival admitted at once or refused never needs a waiter, and under a
    /// flood those are most arrivals. Building the waiter up front made every
    /// decision pay an allocation and a free, so its cost rode on the
    /// allocator's contention rather than on the round (#375). `make_waiter`
    /// runs at most once, and only on the [`Arrival::Queued`] path.
    pub(crate) fn arrive_with<P, F, M>(
        &mut self,
        tenant: &Tenant,
        make_waiter: M,
        acquire: F,
    ) -> Arrival<P>
    where
        F: FnOnce() -> Option<P>,
        M: FnOnce() -> W,
    {
        let ceiling = self.policy.per_tenant_limit;
        let queue_limit = self.policy.queue_per_tenant;
        // The entry exists from the first arrival and is removed once idle, so
        // a tenant that arrives, drains and leaves costs nothing afterwards.
        let entry = self.entries.entry(tenant.clone()).or_default();
        if self.ring.is_empty()
            && entry.in_flight < ceiling
            && let Some(permit) = acquire()
        {
            entry.in_flight = entry.in_flight.saturating_add(1);
            return Arrival::Immediate(permit);
        }
        if entry.live() >= queue_limit {
            // The empty entry an arrival may have created must not outlive the
            // refusal that created it: an idle tenant with no queue is
            // indistinguishable from one that never arrived.
            if entry.is_idle() {
                self.entries.remove(tenant);
            }
            return Arrival::Refused { limit: queue_limit };
        }
        if self.retained >= self.policy.queue_total {
            // The supervisor's own bound, checked after the tenant's so the
            // refusal names the loudest owner of the pressure first: a caller who
            // is told "this tenant is at its bound" can fix it, and a caller told
            // "the supervisor is full" cannot. The entry is removed for the same
            // reason as above.
            if entry.is_idle() {
                self.entries.remove(tenant);
            }
            return Arrival::SupervisorFull {
                limit: self.policy.queue_total,
            };
        }
        entry.queue.push_back(make_waiter());
        self.retained = self.retained.saturating_add(1);
        // A tenant at its ceiling parks off the ring; `note_release` re-arms it
        // the moment one of its admissions completes. The waiter is still
        // counted against the queue bound, which is the bound that protects the
        // supervisor's memory while it waits.
        if !entry.on_ring && entry.in_flight < ceiling {
            entry.on_ring = true;
            self.ring.push_back(tenant.clone());
        }
        Arrival::Queued
    }

    /// Register one arrival for `tenant` that refuses rather than waits, and
    /// retains nothing.
    ///
    /// The non-blocking counterpart of [`arrive`](Self::arrive), for the door
    /// that reports capacity instead of waiting for it. An arrival is admitted
    /// only when every condition holds at once: nobody is waiting, the tenant
    /// is under its ceiling, its queue is empty, and `acquire` produced a
    /// permit. Anything else is [`TryArrival::Contended`] with **no** entry
    /// created and no queue slot taken, so a caller that keeps hammering a full
    /// supervisor costs the scheduler nothing per attempt — the refusal path has
    /// to be free of the queue it refuses to join.
    pub fn try_arrive<P, F>(&mut self, tenant: &Tenant, acquire: F) -> TryArrival<P>
    where
        F: FnOnce() -> Option<P>,
    {
        let ceiling = self.policy.per_tenant_limit;
        if !self.ring.is_empty() || self.entries.contains_key(tenant) {
            // A ring member or an existing entry means work is already waiting —
            // either by this tenant or another. Refuse rather than jump it.
            return TryArrival::Contended;
        }
        let Some(permit) = acquire() else {
            return TryArrival::Contended;
        };
        // The entry is created only on the admitted path, charged to the one
        // in-flight admission this takes; a drained tenant removes it again in
        // `note_release`.
        let entry = self.entries.entry(tenant.clone()).or_default();
        if entry.in_flight >= ceiling {
            // Unreachable: a fresh entry is at zero. Handled rather than
            // asserted, because a contended pool must not be charged an
            // admission it refused.
            return TryArrival::Contended;
        }
        entry.in_flight = entry.in_flight.saturating_add(1);
        TryArrival::Immediate(permit)
    }

    /// Record that one of `tenant`'s in-flight admissions completed.
    ///
    /// This is the release half of the contract: it does not hand out a permit
    /// — the caller still holds the completed task's permit and passes it to
    /// [`grant`](Self::grant) — it only makes the tenant servable again. A
    /// tenant parked at its ceiling with waiters goes back on the ring, and a
    /// fully idle entry is removed so the map stays bounded by live tenants.
    pub fn note_release(&mut self, tenant: &Tenant) {
        let Some(entry) = self.entries.get_mut(tenant) else {
            return;
        };
        entry.in_flight = entry.in_flight.saturating_sub(1);
        if !entry.queue.is_empty()
            && !entry.on_ring
            && entry.in_flight < self.policy.per_tenant_limit
        {
            entry.on_ring = true;
            self.ring.push_back(tenant.clone());
        }
        if entry.is_idle() {
            self.entries.remove(tenant);
        }
    }

    /// Record that a waiter still in `tenant`'s queue has been abandoned by its
    /// owner, and compact the queue when the abandoned outnumber the live.
    ///
    /// `is_live` decides whether a waiter is still owned, exactly as it does for
    /// [`grant`](Self::grant). Counting abandonment separately from removal is
    /// what makes the common case free: a [`VecDeque`] cannot remove from the
    /// middle in O(1), and a single abandoned waiter costs one counter.
    ///
    /// But a counter alone is not a bound. A tenant parked at its ceiling whose
    /// callers keep walking away never passes a grant that could skip its
    /// abandoned heads, so its deque grows by one entry per abandonment and
    /// nothing ever shrinks it. The compaction is what makes the retention a
    /// function of the policy rather than of how long the tenant has been
    /// unlucky: once the abandoned outnumber the live, the queue is rebuilt from
    /// the live waiters alone, which costs one pass over the queue and is paid
    /// for by the abandonments that made it necessary — amortized O(1) per
    /// abandonment, the same accounting the lazy skip already relies on.
    pub fn note_abandoned<F>(&mut self, tenant: &Tenant, is_live: &mut F)
    where
        F: FnMut(&W) -> bool,
    {
        let Some(entry) = self.entries.get_mut(tenant) else {
            return;
        };
        entry.abandoned = entry.abandoned.saturating_add(1).min(entry.queue.len());
        // "Abandoned outnumber the live" rather than "any abandoned": rebuilding
        // on every single abandonment would make the common case O(queue) and
        // leave nothing for the skip in `grant` to do.
        if entry.abandoned > entry.live() {
            let before = entry.queue.len();
            entry.queue.retain(|waiter| is_live(waiter));
            self.retained = self
                .retained
                .saturating_sub(before.saturating_sub(entry.queue.len()));
            // Everything retained answered live, so the count that survived them
            // is zero. Setting it rather than recomputing keeps the two from
            // disagreeing if a liveness predicate is stricter than the count.
            entry.abandoned = 0;
        }
    }

    /// Offer one permit to the round, skipping abandoned waiters.
    ///
    /// `is_live` decides whether a waiter is still owned; every waiter it
    /// refuses at the head of a queue is dropped from that queue and its
    /// abandonment count spent, so the skip is paid for by the abandonment that
    /// caused it. The tenant chosen is charged one in-flight admission, and the
    /// grant carries the permit so the caller delivers both to the waiter in one
    /// step.
    pub fn grant<P, F>(&mut self, permit: P, is_live: &mut F) -> GrantOutcome<W, P>
    where
        F: FnMut(&W) -> bool,
    {
        loop {
            let Some(front) = self.ring.front().cloned() else {
                return GrantOutcome::Idle(permit);
            };
            let Some(entry) = self.entries.get_mut(&front) else {
                // Unreachable — a ring member always has an entry — but handled
                // rather than asserted: a ring that outlived its entry would
                // spin, and dropping the stale member repairs it.
                self.ring.pop_front();
                continue;
            };
            // Skip the abandoned heads this tenant accumulated. Each skip spends
            // one unit of `abandoned`, so the count cannot drift.
            while entry.queue.front().is_some_and(|waiter| !is_live(waiter)) {
                entry.queue.pop_front();
                entry.abandoned = entry.abandoned.saturating_sub(1);
                self.retained = self.retained.saturating_sub(1);
            }
            if entry.queue.is_empty() {
                // Nothing left to serve: retire the member and try the next.
                self.ring.pop_front();
                entry.on_ring = false;
                entry.deficit = 0;
                entry.turn_open = false;
                if entry.is_idle() {
                    self.entries.remove(&front);
                }
                continue;
            }
            // Ring members are servable by construction — `arrive` and
            // `note_release` arm them only under the ceiling and `grant` parks
            // them at it — so the turn opens here without a second check.
            if !entry.turn_open {
                let weight = self.policy.weight_of(&front).get();
                entry.deficit = entry.deficit.saturating_add(weight);
                entry.turn_open = true;
            }
            let Some(waiter) = entry.queue.pop_front() else {
                continue;
            };
            self.retained = self.retained.saturating_sub(1);
            entry.deficit = entry.deficit.saturating_sub(1);
            entry.in_flight = entry.in_flight.saturating_add(1);
            let drained = entry.queue.is_empty();
            let at_ceiling = entry.in_flight >= self.policy.per_tenant_limit;
            if drained || at_ceiling || entry.deficit == 0 {
                // The turn closes. A drained queue retires the member; a ceiling
                // parks it for `note_release` to re-arm; a spent deficit
                // rotates it behind the tenants that have not had this turn yet.
                self.ring.pop_front();
                entry.turn_open = false;
                if drained || at_ceiling {
                    entry.on_ring = false;
                    entry.deficit = 0;
                    if entry.is_idle() {
                        self.entries.remove(&front);
                    }
                } else {
                    self.ring.push_back(front.clone());
                }
            }
            return GrantOutcome::Granted(Grant {
                tenant: front,
                waiter,
                permit,
            });
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{
        Arrival, DeficitRoundRobin, GrantOutcome, MAX_QUEUE_PER_TENANT, MAX_TENANT_WEIGHT,
        TenancyError, TenancyPolicy,
    };
    use crate::script::Tenant;
    use std::error::Error;
    use std::num::NonZeroU32;

    /// A scheduler over numbered waiters and numbered permits; the pool is a
    /// closure over a counter in each test.
    type Core = DeficitRoundRobin<u32>;

    /// The tenant named `name`, validated.
    fn tenant(name: &str) -> Result<Tenant, Box<dyn Error>> {
        Ok(Tenant::new(name)?)
    }

    /// Every waiter is owned by somebody.
    fn all_live(_waiter: &u32) -> bool {
        true
    }

    /// Whether an arrival parked rather than being admitted at once.
    ///
    /// A predicate rather than a value comparison because the queued arm of
    /// [`Arrival`] carries no permit, so there is nothing for the caller to
    /// pattern it against — asking the question is the comparison.
    fn parked<P>(arrival: Arrival<P>) -> bool {
        matches!(arrival, Arrival::Queued)
    }

    #[test]
    fn a_fresh_core_admits_immediately_from_the_pool() -> Result<(), Box<dyn Error>> {
        let mut core: Core = DeficitRoundRobin::new(TenancyPolicy::new(2, 4));
        let t0 = tenant("t-0")?;
        assert_eq!(
            core.arrive(&t0, 100, || Some(7_u32)),
            Arrival::Immediate(7),
            "an empty ring and an under-ceiling tenant admit at once"
        );
        assert_eq!(
            core.in_flight_of(&t0),
            1,
            "the immediate admission is charged to the tenant"
        );
        let queued: Arrival<u32> = Arrival::Queued;
        assert_eq!(
            core.arrive(&t0, 101, || None),
            queued,
            "with a ring member present, the next arrival queues rather than jumping it"
        );
        Ok(())
    }

    /// The waiter is built only when an arrival parks (#375): an immediate
    /// admission and both refusals never call `make_waiter`, and a queued
    /// arrival calls it exactly once.
    #[test]
    fn a_waiter_is_built_only_for_an_arrival_that_parks() -> Result<(), Box<dyn Error>> {
        let mut core: Core = DeficitRoundRobin::new(TenancyPolicy::new(1, 1).with_queue_total(1));
        let t0 = tenant("t-0")?;
        let t1 = tenant("t-1")?;
        let built = std::cell::Cell::new(0_u32);
        let make = || {
            built.set(built.get().saturating_add(1));
            built.get()
        };
        assert_eq!(
            core.arrive_with(&t0, make, || Some(7_u32)),
            Arrival::Immediate(7),
            "a free pool admits at once"
        );
        assert_eq!(built.get(), 0, "an immediate admission built no waiter");
        assert!(
            parked(core.arrive_with(&t0, make, || None::<u32>)),
            "a spent pool parks the arrival"
        );
        assert_eq!(
            built.get(),
            1,
            "the parked arrival built exactly one waiter"
        );
        assert_eq!(
            core.arrive_with(&t0, make, || None::<u32>),
            Arrival::Refused { limit: 1 },
            "a full tenant queue refuses"
        );
        assert_eq!(
            core.arrive_with(&t1, make, || None::<u32>),
            Arrival::SupervisorFull { limit: 1 },
            "a full supervisor refuses"
        );
        assert_eq!(built.get(), 1, "neither refusal built a waiter");
        Ok(())
    }

    #[test]
    fn a_refusal_names_the_queue_bound_and_retains_nothing() -> Result<(), Box<dyn Error>> {
        let mut core: Core = DeficitRoundRobin::new(TenancyPolicy::new(1, 2));
        let t0 = tenant("t-0")?;
        // Pool of one: the first arrival takes it, the rest queue behind it. Each
        // arrival is offered the same shared cell through its own closure, so the
        // one permit is spent exactly once across four calls.
        let cell = std::cell::Cell::new(1_u32);
        let take = || {
            let left = cell.get();
            if left == 0 {
                return None;
            }
            cell.set(left.saturating_sub(1));
            Some(left.saturating_sub(1))
        };
        assert_eq!(
            core.arrive(&t0, 1, take),
            Arrival::Immediate(0),
            "the pool's one permit is taken at once"
        );
        let queued: Arrival<u32> = Arrival::Queued;
        assert_eq!(core.arrive(&t0, 2, || None), queued, "the second waits");
        assert_eq!(core.arrive(&t0, 3, || None), queued, "the third waits");
        let spent = || None::<u32>;
        assert_eq!(
            core.arrive(&t0, 4, spent),
            Arrival::Refused { limit: 2 },
            "the fourth is past the queue bound and the refusal names it"
        );
        assert_eq!(
            core.queued_of(&t0),
            2,
            "only the two live waiters are counted"
        );
        Ok(())
    }

    #[test]
    fn two_backlogged_tenants_alternate_permits() -> Result<(), Box<dyn Error>> {
        let mut core: Core = DeficitRoundRobin::new(TenancyPolicy::new(4, 8));
        let first = tenant("a")?;
        let second = tenant("b")?;
        // Three waiters each, so both tenants stay backlogged for the whole
        // sweep: alternation is a property of two *waiting* tenants, and a
        // tenant whose queue empties has nothing to be denied.
        let first_arrival = core.arrive(&first, 1, || None::<u32>);
        assert!(parked(first_arrival), "the first tenant queues");
        let second_arrival = core.arrive(&second, 2, || None::<u32>);
        assert!(parked(second_arrival), "the second tenant queues");
        for (tenant, waiter) in [(&first, 3_u32), (&second, 4_u32)] {
            let arrived = core.arrive(tenant, waiter, || None::<u32>);
            assert!(parked(arrived), "the backlog grows on both tenants");
        }
        let first_third = core.arrive(&first, 5, || None::<u32>);
        assert!(parked(first_third), "the first tenant queues a third");
        let second_third = core.arrive(&second, 6, || None::<u32>);
        assert!(parked(second_third), "the second tenant queues a third");
        // Four permits from a six-waiter backlog: enough grants to see the
        // pattern repeat without draining either queue.
        let mut served: Vec<u32> = Vec::new();
        for permit in 0..4_u32 {
            let mut live = all_live;
            let outcome = core.grant(permit, &mut live);
            let GrantOutcome::Granted(grant) = outcome else {
                return Err("a queued tenant must take the permit".into());
            };
            served.push(grant.waiter);
            core.note_release(&grant.tenant);
        }
        assert_eq!(
            served,
            vec![1, 2, 3, 4],
            "equal weights alternate strictly: first, second, first, second"
        );
        Ok(())
    }

    #[test]
    fn a_double_weight_takes_two_permits_per_turn() -> Result<(), Box<dyn Error>> {
        let heavy = tenant("heavy")?;
        let light = tenant("light")?;
        let two: NonZeroU32 = 2_u32.try_into()?;
        let policy = TenancyPolicy::new(8, 16).with_weight(&heavy, two)?;
        let mut core: Core = DeficitRoundRobin::new(policy);
        let queued: Arrival<u32> = Arrival::Queued;
        // Both tenants stay backlogged for the whole sweep, which is what makes
        // the two-per-turn share measurable at all.
        assert_eq!(core.arrive(&heavy, 1, || None), queued, "heavy queues");
        assert_eq!(core.arrive(&light, 2, || None), queued, "light queues");
        assert_eq!(
            core.arrive(&heavy, 3, || None),
            queued,
            "heavy queues again"
        );
        assert_eq!(
            core.arrive(&light, 4, || None),
            queued,
            "light queues again"
        );
        // Six waiters each, a deep backlog, so the 2:1 share is observable over
        // many turns rather than in one lucky interleaving.
        for waiter in 5..11_u32 {
            let _heavy_arrival = core.arrive(&heavy, waiter, || None::<u32>);
        }
        for waiter in 11..17_u32 {
            let _light_arrival = core.arrive(&light, waiter, || None::<u32>);
        }
        let mut heavy_served: u32 = 0;
        let mut light_served: u32 = 0;
        for permit in 0..9_u32 {
            let mut live = all_live;
            let outcome = core.grant(permit, &mut live);
            let GrantOutcome::Granted(grant) = outcome else {
                return Err("a backlogged tenant must take the permit".into());
            };
            if grant.tenant == heavy {
                heavy_served = heavy_served.saturating_add(1);
            } else {
                light_served = light_served.saturating_add(1);
            }
            core.note_release(&grant.tenant);
        }
        // Nine permits against weights 2 and 1: the heavy tenant's share is its
        // weight over the total, so 6 heavy and 3 light is the fair outcome, and
        // any deviation means the round is not honouring the weights.
        assert_eq!(
            (heavy_served, light_served),
            (6, 3),
            "weight 2 takes two permits for every one weight 1 takes"
        );
        Ok(())
    }

    #[test]
    fn a_tenant_at_its_ceiling_is_parked_until_it_releases() -> Result<(), Box<dyn Error>> {
        let mut core: Core = DeficitRoundRobin::new(TenancyPolicy::new(1, 8));
        let loud = tenant("loud")?;
        let quiet = tenant("quiet")?;
        assert_eq!(
            core.arrive(&loud, 1, || Some(10_u32)),
            Arrival::Immediate(10),
            "the loud tenant takes the pool's one permit"
        );
        let queued: Arrival<u32> = Arrival::Queued;
        assert_eq!(core.arrive(&loud, 2, || None), queued, "its next waits");
        assert_eq!(
            core.arrive(&quiet, 3, || None),
            queued,
            "the quiet tenant waits"
        );
        // A permit frees. The loud tenant is at its ceiling (one in flight of
        // one), so the quiet tenant is the one the round must serve.
        core.note_release(&loud);
        let mut live = all_live;
        let first = core.grant(20, &mut live);
        let GrantOutcome::Granted(grant) = first else {
            return Err("the quiet tenant is eligible and must be served".into());
        };
        assert_eq!(
            grant.waiter, 3,
            "the permit went to the tenant under its ceiling"
        );
        core.note_release(&grant.tenant);
        // Both tenants are under their ceiling again; the loud one is next on
        // the ring, so its waiter runs now.
        let second = core.grant(21, &mut live);
        let GrantOutcome::Granted(next) = second else {
            return Err("the loud tenant is eligible again and must be served".into());
        };
        assert_eq!(
            next.waiter, 2,
            "the loud tenant's waiter runs as soon as it has room"
        );
        Ok(())
    }

    #[test]
    fn an_abandoned_waiter_is_skipped_and_stops_spending_the_bound() -> Result<(), Box<dyn Error>> {
        let mut core: Core = DeficitRoundRobin::new(TenancyPolicy::new(1, 1));
        let t0 = tenant("t-0")?;
        assert_eq!(
            core.arrive(&t0, 7, || Some(1_u32)),
            Arrival::Immediate(1),
            "one in flight"
        );
        let queued: Arrival<u32> = Arrival::Queued;
        assert_eq!(
            core.arrive(&t0, 8, || None),
            queued,
            "the queue bound has room"
        );
        let spent = || None::<u32>;
        assert_eq!(
            core.arrive(&t0, 9, spent),
            Arrival::Refused { limit: 1 },
            "the bound is reached while the waiter lives"
        );
        // Waiter 8 is the one whose owner walked away, and the liveness predicate says
        // so: the scheduler compacts on abandonment using the same knowledge the
        // grant skip uses, and a predicate that denied nothing would keep an
        // entry the count has already written off.
        let mut gone = |waiter: &u32| *waiter != 8;
        core.note_abandoned(&t0, &mut gone);
        assert_eq!(
            core.arrive(&t0, 10, || None),
            queued,
            "an abandoned waiter no longer spends the tenant's queue room"
        );
        core.note_release(&t0);
        let mut live = |waiter: &u32| *waiter != 8;
        let outcome = core.grant(2, &mut live);
        let GrantOutcome::Granted(grant) = outcome else {
            return Err("the live waiter must be served".into());
        };
        assert_eq!(
            grant.waiter, 10,
            "the abandoned head was skipped, not served"
        );
        assert_eq!(
            core.queued_of(&t0),
            0,
            "the skipped waiter is gone from the queue and the count with it"
        );
        Ok(())
    }

    #[test]
    fn an_idle_tenant_costs_nothing_after_it_drains() -> Result<(), Box<dyn Error>> {
        let mut core: Core = DeficitRoundRobin::new(TenancyPolicy::new(2, 2));
        let t0 = tenant("t-0")?;
        assert_eq!(
            core.arrive(&t0, 1, || Some(1_u32)),
            Arrival::Immediate(1),
            "admitted"
        );
        core.note_release(&t0);
        assert_eq!(
            core.tenants_with_queues(),
            0,
            "a drained tenant holds no queue and no entry"
        );
        Ok(())
    }

    #[test]
    fn an_idle_pool_answers_idle_rather_than_spinning() -> Result<(), Box<dyn Error>> {
        let mut core: Core = DeficitRoundRobin::new(TenancyPolicy::new(4, 8));
        let t0 = tenant("t-0")?;
        assert_eq!(
            core.arrive(&t0, 1, || Some(1)),
            Arrival::Immediate(1),
            "one in flight"
        );
        core.note_release(&t0);
        assert!(
            !core.has_eligible(),
            "with nothing waiting and the tenant drained, no tenant is eligible"
        );
        let mut live = all_live;
        let outcome = core.grant(9, &mut live);
        let GrantOutcome::Idle(permit) = outcome else {
            return Err("a permit nobody can take is handed back".into());
        };
        assert_eq!(
            permit, 9,
            "the idle permit is returned to the caller's pool"
        );
        Ok(())
    }

    #[test]
    fn the_declared_bounds_are_clamped_to_their_ceilings() {
        let policy = TenancyPolicy::new(0, MAX_QUEUE_PER_TENANT.saturating_add(1));
        assert_eq!(
            policy.per_tenant_limit(),
            1,
            "a zero ceiling reads as one, as Supervisor::new reads its own bound"
        );
        assert_eq!(
            policy.queue_per_tenant(),
            MAX_QUEUE_PER_TENANT,
            "a queue bound past the ceiling is clamped to it"
        );
    }

    #[test]
    fn a_weight_past_its_bound_is_refused_naming_both_numbers() -> Result<(), Box<dyn Error>> {
        let heavy = tenant("heavy")?;
        let past = MAX_TENANT_WEIGHT.saturating_add(1);
        let Some(too_heavy) = NonZeroU32::new(past) else {
            return Err("a weight one past its bound is never zero".into());
        };
        let outcome = TenancyPolicy::new(1, 1).with_weight(&heavy, too_heavy);
        assert_eq!(
            outcome.err(),
            Some(TenancyError::WeightTooHigh {
                weight: past,
                limit: MAX_TENANT_WEIGHT,
            }),
            "a weight above the ceiling is refused with the weight and the bound"
        );
        Ok(())
    }
}
