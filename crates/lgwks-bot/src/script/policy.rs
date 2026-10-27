//! What the computer decides, so the author of a flow does not have to.
//!
//! Two numbers are wrong whenever a person types them: how many things to run
//! at once, and how often to retry. Both depend on the machine and on what
//! every other call site is doing at the same moment, which no call site can
//! see. So a flow does not choose them; the root [`Scope`]
//! owns one [`Policy`] and every step beneath it shares that policy.
//!
//! # Retries are a budget for the run, not a count per call site
//!
//! Per-call-site retry counts are the documented cause of retry storms: when
//! every tier retries a correlated failure, load multiplies exactly when the
//! system can least absorb it. In a 2026 study of 200 open-source services
//! (arXiv:2608.25403), a naive retry policy under correlated failure *lowered*
//! the success rate from 55.4% to 41.5% relative to not retrying at all, while
//! a budget-constrained policy stayed near the no-retry baseline and still
//! recovered transient faults. The same feedback loop is the mechanism behind
//! metastable failures, where load stays high after the trigger is gone
//! (arXiv:2510.03551).
//!
//! The budget here is the production design Finagle ships as `RetryBudget`: a
//! fixed reserve, plus a fraction of first attempts. Every first attempt made
//! by a `retry` block deposits [`RETRY_RATIO_MILLI`] thousandths of a retry;
//! every retry withdraws one; the budget starts holding [`RETRY_RESERVE`]
//! retries. So a single flaky step keeps its full `retry up to N times`
//! (N ≤ the reserve), while a thousand items failing together spend at most
//! `reserve + 0.2 × 1000` retries, not `1000 × (N - 1)`. It counts operations
//! rather than reading a wall clock, so a replay under simulation is exact;
//! the one place a budget reads a clock at all — the shared host window
//! below — reads the estate's injectable clock, never the wall.
//!
//! # One budget per run, or one per host
//!
//! The budget above belongs to one run: a root scope mints it, and two
//! concurrent runs hold two budgets. That is the right shape for a run, and
//! the wrong shape for a host that admits many runs against one failing
//! backend: the host admits cores × 64 runs at once, so N concurrent runs
//! would hold N reserves and multiply the load a retry storm adds exactly
//! when the backend can least absorb it. So a host installs a second shape
//! of the same policy: [`Policy::for_this_host`] gives every admitted run
//! one shared window. Credit above the reserve is still earned by first
//! attempts at [`RETRY_RATIO_MILLI`], but it is earned by the host's
//! traffic, it expires after [`RETRY_TTL_NANOS`] of clock time without a
//! first attempt, and the balance never exceeds [`RETRY_CEILING_MILLI`] —
//! twice the reserve — so the host's instant exposure to one backend is
//! bounded no matter how many runs it admits. That is the per-server shape
//! of gRPC's retry throttle, which tracks one `token_count` per server
//! rather than per call, beside Finagle's per-client budget whose tokens
//! expire after ten seconds.
//!
//! # Fan-out draws from one process-wide pool
//!
//! `each x in xs:` with no bound runs at most [`Policy::fan_out`] bodies at
//! once per root: [`FAN_OUT_PER_CORE`] per available core, capped at
//! [`MAX_IN_FLIGHT`]. That per-root width is not a per-process limit on its
//! own: every `Scope::root` and every standalone [`FanOut`](super::FanOut)
//! mints a fresh policy, so N concurrent roots would hold N widths — on a
//! 16-core host a default `Host` admits 1,024 runs, each able to hold a
//! 1,024-wide `each`, about a million outstanding bodies from one process.
//! The width is therefore drawn from the process-wide [`HostPool`]: 64 per
//! available core, capped at [`MAX_IN_FLIGHT`], shared by every root in the
//! process, so concurrent roots divide the allowance instead of multiplying
//! it. A body in `each` spends its life waiting, so the pool bounds
//! outstanding waits rather than CPU work; it scales with the host so the
//! same flow neither starves a large machine nor floods a small one. An
//! external limit (an API that allows four concurrent calls) is still said
//! explicitly with `at most (limit)`, which keeps its own per-call width and
//! does not draw from the pool: a declared bound is the author's, the pool
//! is the machine's.

use std::num::NonZeroUsize;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::rt::clock::{Clock, duration_to_nanos};

use super::MAX_IN_FLIGHT;

/// Concurrent waits one `each` allows per available core when no bound is given.
const FAN_OUT_PER_CORE: usize = 64;

/// Retries a run may make before its first attempts have earned any.
const RETRY_RESERVE: u64 = 10;

/// Thousandths of a retry each first attempt earns: 200 is one retry per five
/// first attempts, Finagle's default `percentCanRetry` of 20%.
const RETRY_RATIO_MILLI: u64 = 200;

/// Thousandths per whole retry.
const MILLI: u64 = 1_000;

/// The reserve in thousandths: what a budget starts with, and what credit
/// older than the expiry is clamped back to.
const RETRY_RESERVE_MILLI: u64 = RETRY_RESERVE * MILLI;

/// The most credit first attempts may hold above the reserve, in thousandths:
/// the reserve itself, so a shared budget's instant exposure is at most twice
/// the reserve.
const RETRY_EARNED_CAP_MILLI: u64 = RETRY_RESERVE_MILLI;

/// The most a shared budget may hold at once, in thousandths: the reserve plus
/// the earned cap.
const RETRY_CEILING_MILLI: u64 = RETRY_RESERVE_MILLI + RETRY_EARNED_CAP_MILLI;

/// How long credit above the reserve stays spendable, in clock nanoseconds:
/// ten seconds, Finagle's default token time to live.
const RETRY_TTL_NANOS: u64 = 10_000_000_000;

/// The decisions shared by every step under one root scope.
#[derive(Debug)]
pub(crate) struct Policy {
    /// Bodies an unbounded `each` may run at once.
    fan_out: NonZeroUsize,
    /// Retries left, in thousandths.
    retries: AtomicU64,
    /// The shared window, when this policy belongs to a host rather than to
    /// one run.
    window: Option<Window>,
}

/// The shared window behind a host's budget: when credit above the reserve
/// was last earned, and the clock that says when it has expired.
#[derive(Debug)]
struct Window {
    /// The last clock time, in nanoseconds, a first attempt earned credit.
    ///
    /// Updated with `fetch_max`, so two runs that read the clock out of
    /// order cannot move the stamp backwards and make earned credit look
    /// older than it is.
    earned_at: AtomicU64,
    /// The host's clock, read through `now` and never advanced here.
    clock: Clock,
}

impl Window {
    /// A fresh window: nothing earned yet, so the stamp is the epoch. A
    /// budget that has earned nothing holds only the reserve, which is
    /// exactly what a stamp of zero says on any clock that starts at or
    /// above the expiry.
    const fn new(clock: Clock) -> Self {
        Self {
            earned_at: AtomicU64::new(0),
            clock,
        }
    }
}

impl Policy {
    /// The policy for the machine this runs on.
    pub(super) fn for_this_machine() -> Self {
        match Self::machine_fan_out() {
            Some(fan_out) => Self {
                fan_out,
                retries: AtomicU64::new(RETRY_RESERVE_MILLI),
                window: None,
            },
            None => Self::one_body_per_core_placeholder(),
        }
    }

    /// The policy every run a host admits shares.
    ///
    /// One budget for the host, not one per run: the reserve is the host's,
    /// credit above it is earned by the host's first attempts and expires
    /// after [`RETRY_TTL_NANOS`] of clock time, and the balance is capped at
    /// [`RETRY_CEILING_MILLI`]. The fan-out is still sized to the machine —
    /// sharing a retry budget says nothing about how many bodies one `each`
    /// may wait on at once.
    pub(crate) fn for_this_host(clock: Clock) -> Self {
        let fan_out = match Self::machine_fan_out() {
            Some(fan_out) => fan_out,
            // A machine that reported no parallelism runs one body per core
            // it is known to have, which is one.
            None => NonZeroUsize::MIN,
        };
        Self {
            fan_out,
            retries: AtomicU64::new(RETRY_RESERVE_MILLI),
            window: Some(Window::new(clock)),
        }
    }

    /// The fan-out this machine can honestly run, or `None` when it reported
    /// no parallelism at all.
    fn machine_fan_out() -> Option<NonZeroUsize> {
        let cores = match std::thread::available_parallelism() {
            Ok(cores) => cores.get(),
            Err(_) => 1,
        };
        // The fan-out is non-zero by construction rather than by substitution:
        // `cores` is at least one, `FAN_OUT_PER_CORE` is non-zero and
        // `MAX_IN_FLIGHT` is one or more, so the product is one or more before
        // the ceiling and the ceiling itself is one or more after it. A
        // `NonZeroUsize` here is the invariant, not a fallback for a value
        // that failed to arrive.
        NonZeroUsize::new(cores.saturating_mul(FAN_OUT_PER_CORE).min(MAX_IN_FLIGHT))
    }

    /// The policy a machine that reported no parallelism gets.
    ///
    /// One body per core with no core reported: a fan-out of zero would refuse
    /// every item and hang a run that has to make progress, and a fan-out of one
    /// is what this machine can honestly be said to run at once.
    fn one_body_per_core_placeholder() -> Self {
        Self {
            fan_out: NonZeroUsize::MIN,
            retries: AtomicU64::new(RETRY_RESERVE_MILLI),
            window: None,
        }
    }

    /// How many bodies an `each` with no bound runs at once.
    pub(super) const fn fan_out(&self) -> NonZeroUsize {
        self.fan_out
    }
    /// A `retry` block is making its first attempt: earn a fraction of a retry.
    pub(super) fn first_attempt(&self) {
        match self.window.as_ref() {
            Some(window) => self.earn_windowed(window),
            None => {
                // Relaxed: the balance is a counter, not a guard for other memory.
                let _previous =
                    self.retries
                        .try_update(Ordering::Relaxed, Ordering::Relaxed, |held| {
                            Some(held.saturating_add(RETRY_RATIO_MILLI))
                        });
            }
        }
    }

    /// Spend one retry if the run still has one; `false` means the failure is
    /// systemic and repeating it would only add load.
    pub(super) fn take_retry(&self) -> bool {
        match self.window.as_ref() {
            Some(window) => self.spend_windowed(window),
            None => self
                .retries
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |held| {
                    held.checked_sub(MILLI)
                })
                .is_ok(),
        }
    }

    /// Earn credit into a shared window: clamp credit older than the expiry
    /// back to the reserve, add the ratio, cap the balance at the ceiling,
    /// and stamp the window with now.
    fn earn_windowed(&self, window: &Window) {
        let now = duration_to_nanos(window.clock.now());
        let stamp = window.earned_at.load(Ordering::Relaxed);
        // The staleness decision is read once, before the update, on purpose.
        // A concurrent first attempt that refreshes the stamp between this
        // read and the update can make freshly earned credit look stale once,
        // which clamps it back to the reserve — bounded, and the next attempt
        // earns again. Re-reading inside the closure could not see the stamp
        // it clamps against without a lock, and the retry path takes none.
        let stale = now.saturating_sub(stamp) > RETRY_TTL_NANOS;
        let _previous = self
            .retries
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |held| {
                let held = if stale {
                    held.min(RETRY_RESERVE_MILLI)
                } else {
                    held
                };
                Some(
                    held.saturating_add(RETRY_RATIO_MILLI)
                        .min(RETRY_CEILING_MILLI),
                )
            });
        // `fetch_max`, not a store: two runs that read the clock out of order
        // must not move the stamp backwards, which would make earned credit
        // look older than it is.
        window.earned_at.fetch_max(now, Ordering::Relaxed);
    }

    /// Spend one retry from a shared window, with the same staleness clamp
    /// the earning side applies.
    ///
    /// Spending does not refresh the stamp: the surplus is earned by traffic,
    /// so a host that only retries — no first attempts — watches its surplus
    /// lapse at the expiry, which is the point.
    fn spend_windowed(&self, window: &Window) -> bool {
        let now = duration_to_nanos(window.clock.now());
        let stamp = window.earned_at.load(Ordering::Relaxed);
        let stale = now.saturating_sub(stamp) > RETRY_TTL_NANOS;
        self.retries
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |held| {
                let held = if stale {
                    held.min(RETRY_RESERVE_MILLI)
                } else {
                    held
                };
                held.checked_sub(MILLI)
            })
            .is_ok()
    }
}

// ── The host fan-out pool ────────────────────────────────────────────────────

/// The process-wide ceiling on bodies an unbounded `each` may hold at once.
///
/// One pool for the process, not one per root: the per-root [`Policy::fan_out`]
/// width is what a lone root may reach, and this pool is what N concurrent
/// roots divide. The ceiling is the machine's allowance
/// ([`FAN_OUT_PER_CORE`] per available core under [`MAX_IN_FLIGHT`]), read
/// once at first use. Atomics only, so no lock is ever held across an await;
/// the count is a guard for admission, not for other memory, so every access
/// is relaxed.
#[doc(hidden)]
#[derive(Debug)]
pub(crate) struct HostPool {
    /// Bodies an unbounded `each` may hold at once, across every root.
    ceiling: usize,
    /// Bodies currently held, across every root.
    outstanding: AtomicUsize,
}

#[doc(hidden)]
impl HostPool {
    /// A pool with `ceiling` permits, none held.
    #[must_use]
    pub(crate) fn with_ceiling(ceiling: usize) -> Self {
        Self {
            ceiling,
            outstanding: AtomicUsize::new(0),
        }
    }

    /// Hold one body when fewer than the ceiling are held; `false` holds
    /// nothing and changes nothing.
    pub(crate) fn try_acquire(&self) -> bool {
        self.outstanding
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |held| {
                (held < self.ceiling).then(|| held.saturating_add(1))
            })
            .is_ok()
    }

    /// Hand back one body. A release with nothing held leaves the count at
    /// zero rather than wrapping: the pairing is constructed (every acquire
    /// is followed by exactly one release on the body's completion, failure
    /// or drop), and a counter that wrapped would refuse every future
    /// admission, which is the failure this pool exists to prevent.
    pub(crate) fn release(&self) {
        let released = self
            .outstanding
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |held| {
                held.checked_sub(1)
            });
        debug_assert!(
            released.is_ok(),
            "a host-pool release with nothing held: an acquire went unreleased or a body released twice"
        );
    }
}

/// The process-wide pool, read once from the machine at first use.
static HOST_POOL: OnceLock<HostPool> = OnceLock::new();

/// The process-wide pool every unbounded `each` draws from.
///
/// The ceiling is what this machine honestly runs at once
/// ([`FAN_OUT_PER_CORE`] per available core under [`MAX_IN_FLIGHT`], one
/// body when no parallelism was reported), fixed at first use so every root
/// divides the same allowance for the life of the process.
#[doc(hidden)]
pub(crate) fn host_pool() -> &'static HostPool {
    HOST_POOL.get_or_init(|| {
        let ceiling = match Policy::machine_fan_out() {
            Some(fan_out) => fan_out.get(),
            None => 1,
        };
        HostPool::with_ceiling(ceiling)
    })
}

/// The permits one unbounded `each` holds from the process-wide pool.
///
/// Acquired one body at a time as items are admitted, settled against the
/// bodies still running as they complete, and released whole on failure,
/// cancellation or drop: dropping the fan-out without settling it still hands
/// every permit back, so an abandoned `each` cannot starve the roots that
/// outlive it.
///
/// A fan-out with no body running may always start one, permit or not. Without
/// that floor an `each` whose pool is empty has nothing of its own to wake it
/// — another root's release wakes nobody — so it would park for good, and an
/// unbounded `each` nested inside another's body would deadlock the moment the
/// outer one held every permit. The floor costs at most one body per running
/// fan-out past the ceiling, and it is what makes every `each` finish.
#[doc(hidden)]
#[derive(Debug)]
pub(crate) struct HostLease<'pool> {
    /// The pool the permits come from.
    pool: &'pool HostPool,
    /// Permits currently held; never more than the bodies running.
    held: usize,
}

#[doc(hidden)]
impl<'pool> HostLease<'pool> {
    /// No permits held, from `pool`.
    #[must_use]
    pub(crate) fn take(pool: &'pool HostPool) -> Self {
        Self { pool, held: 0 }
    }

    /// Whether one more body may start while `running` of this fan-out's
    /// bodies are in flight: with a permit when the pool has one, and without
    /// one when nothing of this fan-out is running (the floor above). `false`
    /// holds nothing and changes nothing.
    pub(crate) fn admit(&mut self, running: usize) -> bool {
        if self.pool.try_acquire() {
            self.held = self.held.saturating_add(1);
            true
        } else {
            running == 0
        }
    }

    /// Hand back every permit beyond the `running` bodies still in flight, so
    /// a finished body returns its permit and the floor's body returns none.
    pub(crate) fn settle(&mut self, running: usize) {
        while self.held > running {
            self.held = self.held.saturating_sub(1);
            self.pool.release();
        }
    }

    /// Hand back every permit held.
    pub(crate) fn release_all(&mut self) {
        while self.held > 0 {
            self.held = self.held.saturating_sub(1);
            self.pool.release();
        }
    }
}

impl Drop for HostLease<'_> {
    /// Hand back whatever is still held: a fan-out dropped without settling
    /// — cancelled, failed, or abandoned by its caller — keeps no permit.
    fn drop(&mut self) {
        self.release_all();
    }
}

#[cfg(test)]
#[path = "sim_policy.rs"]
mod sim_policy;

/// The host fan-out pool (#648), checked against exact counts.
///
/// Sources: the per-server retry throttle the retry budget follows (one shared
/// budget per host, not one per run), and the semaphore discipline of pairing
/// every acquisition with its release on every failure path.
#[cfg(test)]
mod pool_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{FAN_OUT_PER_CORE, HostLease, HostPool};
    use crate::script::{FlowError, MAX_IN_FLIGHT, Scope, Tenant, each};
    use crate::sim_stream::Stream;

    /// What every test here returns.
    type Outcome = Result<(), String>;

    /// The seeds every family replays.
    const SEEDS: [u64; 4] = [
        0xB0D6_0000_0000_0001,
        0xB0D6_0000_0000_0002,
        0xB0D6_FFFF_0000_0003,
        0x5701_B0D6_0000_0004,
    ];

    #[test]
    /// A pool accounts every acquire against its release: the ceiling admits,
    /// one past it refuses, a release admits again, and a drained pool admits
    /// the whole ceiling afresh, so every acquire met exactly one release.
    fn sim_a_pool_accounts_every_acquire_against_its_release() -> Outcome {
        for seed in SEEDS {
            let mut stream = Stream::new(seed);
            let ceiling = stream.below(64)?.saturating_add(1);
            let pool = HostPool::with_ceiling(ceiling);
            for held in 1..=ceiling {
                assert!(
                    pool.try_acquire(),
                    "seed {seed:#x}: acquire {held} of {ceiling}"
                );
            }
            assert!(
                !pool.try_acquire(),
                "seed {seed:#x}: one past {ceiling} is refused"
            );
            pool.release();
            assert!(
                pool.try_acquire(),
                "seed {seed:#x}: a released permit admits again"
            );
            for _ in 0..ceiling {
                pool.release();
            }
            for held in 1..=ceiling {
                assert!(
                    pool.try_acquire(),
                    "seed {seed:#x}: readmit {held} after the drain"
                );
            }
        }
        Ok(())
    }

    #[test]
    /// The floor and the settle: a fan-out with nothing running starts one
    /// body from an empty pool and holds no permit for it, one with a body
    /// running waits, settling hands back exactly the permits past the bodies
    /// in flight, and dropped leases leave the pool whole.
    fn a_lease_admits_one_body_from_an_empty_pool_and_settles_exactly() {
        let pool = HostPool::with_ceiling(2);
        {
            let mut first = HostLease::take(&pool);
            assert!(first.admit(0), "the pool admits its first permit");
            assert!(first.admit(1), "the pool admits its second permit");
            assert!(
                !first.admit(2),
                "a full pool refuses a fan-out already running"
            );
            let mut second = HostLease::take(&pool);
            assert!(
                second.admit(0),
                "nothing of this fan-out runs: the floor starts one"
            );
            assert!(
                !second.admit(1),
                "with its floor body running it waits for a permit"
            );
            second.settle(0);
            assert!(
                !pool.try_acquire(),
                "the floor body held no permit to hand back"
            );
            first.settle(1);
            assert!(
                second.admit(1),
                "a settled permit admits the waiting fan-out"
            );
        }
        assert!(
            pool.try_acquire(),
            "dropped leases hand the first permit back"
        );
        assert!(pool.try_acquire(), "and the second");
        assert!(!pool.try_acquire(), "and no permit was made from nothing");
    }

    #[test]
    /// Roots racing on one pool neither wedge it nor leak a permit: afterwards
    /// the whole ceiling admits afresh, however the threads interleaved.
    fn sim_racing_roots_neither_wedge_nor_leak_the_pool() -> Outcome {
        for seed in SEEDS {
            let mut stream = Stream::new(seed);
            let ceiling = stream.below(128)?.saturating_add(8);
            let rounds = stream.below(512)?.saturating_add(128);
            let threads = stream.below(5)?.saturating_add(2);
            let pool = HostPool::with_ceiling(ceiling);
            std::thread::scope(|scope| {
                for _ in 0..threads {
                    scope.spawn(|| {
                        let mut lease = HostLease::take(&pool);
                        for round in 0..rounds {
                            if lease.admit(round % 2) {
                                lease.settle(0);
                            }
                        }
                    });
                }
            });
            for held in 1..=ceiling {
                assert!(
                    pool.try_acquire(),
                    "seed {seed:#x}: readmit {held} of {ceiling}"
                );
            }
        }
        Ok(())
    }

    /// A future that returns `Pending` once, waking itself, and then resolves.
    struct YieldOnce(bool);

    impl std::future::Future for YieldOnce {
        type Output = ();

        fn poll(
            self: std::pin::Pin<&mut Self>,
            context: &mut std::task::Context<'_>,
        ) -> std::task::Poll<()> {
            let this = self.get_mut();
            if this.0 {
                return std::task::Poll::Ready(());
            }
            this.0 = true;
            context.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    }

    /// What the bodies of concurrent roots count through.
    #[derive(Default)]
    struct Watch {
        /// Bodies in flight now.
        live: AtomicUsize,
        /// The most bodies ever in flight at once.
        peak: AtomicUsize,
        /// Bodies started.
        started: AtomicUsize,
    }

    impl Watch {
        /// One body: counted in, yielded once, counted out.
        async fn body(&self, index: usize) -> Result<usize, FlowError> {
            self.started.fetch_add(1, Ordering::Relaxed);
            let now = self.live.fetch_add(1, Ordering::Relaxed).saturating_add(1);
            self.peak.fetch_max(now, Ordering::Relaxed);
            YieldOnce(false).await;
            self.live.fetch_sub(1, Ordering::Relaxed);
            Ok(index)
        }
    }

    /// The machine's pool ceiling, by the formula `host_pool` reads.
    fn machine_ceiling() -> usize {
        let cores = match std::thread::available_parallelism() {
            Ok(cores) => cores.get(),
            Err(_) => 1,
        };
        cores.saturating_mul(FAN_OUT_PER_CORE).min(MAX_IN_FLIGHT)
    }

    #[test]
    /// An unbounded `each` nested in another's bodies finishes even when the
    /// outer fan-out holds every permit: each inner fan-out starts its floor
    /// body, so the outer bodies complete and every value comes back in order.
    /// Without the floor an inner `each` facing an empty pool parks with
    /// nothing to wake it, and the outer never finishes.
    fn a_nested_unbounded_each_finishes_when_the_outer_holds_every_permit() -> Outcome {
        let items = machine_ceiling().saturating_add(1);
        let tenant = Tenant::new("pool-nested").map_err(|error| error.to_string())?;
        let root = Scope::root(tenant);
        let values = crate::block_on(each(
            &root,
            "outer",
            None,
            0..items,
            |scope, index| async move {
                let inner = each(&scope, "inner", None, 0..3_usize, |_, step| async move {
                    YieldOnce(false).await;
                    Ok::<usize, FlowError>(step)
                })
                .await?;
                Ok::<usize, FlowError>(index.saturating_add(inner.iter().sum::<usize>()))
            },
        ))
        .map_err(|error| error.to_string())?;
        let expected: Vec<usize> = (0..items).map(|index| index.saturating_add(3)).collect();
        assert_eq!(values, expected, "every outer value comes back, in order");
        Ok(())
    }

    #[test]
    /// Concurrent roots divide the one pool: four roots each demanding more
    /// than the ceiling peak at no more than it plus one floor body per root,
    /// and every value comes back in input order on every root. The bound
    /// holds in every schedule: the pool refuses past the ceiling atomically,
    /// and a root takes its floor body only with nothing of its own running.
    fn sim_concurrent_roots_divide_the_one_pool() -> Outcome {
        const ROOTS: usize = 4;
        let ceiling = machine_ceiling();
        let items = ceiling.saturating_add(1);
        let watch = Watch::default();
        let watch = &watch;
        let failures: Vec<String> = std::thread::scope(|threads| {
            // Every root is spawned before any is joined, so the four run at once.
            let mut handles = Vec::with_capacity(ROOTS);
            for root in 0..ROOTS {
                handles.push(threads.spawn(move || {
                    let tenant =
                        Tenant::new(&format!("pool-{root}")).map_err(|error| error.to_string())?;
                    let scope = Scope::root(tenant);
                    let values =
                        crate::block_on(each(&scope, "fan", None, 0..items, |_, index| {
                            watch.body(index)
                        }))
                        .map_err(|error| format!("root {root} failed: {error}"))?;
                    if values == (0..items).collect::<Vec<_>>() {
                        Ok(())
                    } else {
                        Err(format!("root {root}: values came back out of order"))
                    }
                }));
            }
            handles
                .into_iter()
                .filter_map(|handle| match handle.join() {
                    Ok(Ok(())) => None,
                    Ok(Err(failure)) => Some(failure),
                    Err(_) => Some("a root's thread failed".to_owned()),
                })
                .collect()
        });
        assert!(
            failures.is_empty(),
            "concurrent roots failed: {}",
            failures.join("; ")
        );
        assert_eq!(
            watch.started.load(Ordering::Relaxed),
            ROOTS.saturating_mul(items),
            "every item ran exactly once on every root"
        );
        assert_eq!(watch.live.load(Ordering::Relaxed), 0, "none left in flight");
        assert!(
            watch.peak.load(Ordering::Relaxed) <= ceiling.saturating_add(ROOTS),
            "four roots peaked past the one shared pool plus their floor bodies"
        );
        Ok(())
    }
}
