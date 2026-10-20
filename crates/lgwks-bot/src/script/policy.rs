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
//! # Fan-out is sized to the machine
//!
//! `each x in xs:` with no bound runs at most [`Policy::fan_out`] bodies at
//! once: [`FAN_OUT_PER_CORE`] per available core, capped at
//! [`MAX_IN_FLIGHT`]. A body in `each` spends its
//! life waiting, so the bound is a ceiling on outstanding waits rather than on
//! CPU work; it scales with the host so the same flow neither starves a large
//! machine nor floods a small one. An external limit (an API that allows four
//! concurrent calls) is still said explicitly with `at most (limit)`.

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};

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

#[cfg(test)]
#[path = "sim_policy.rs"]
mod sim_policy;
