//! What the computer decides, so the author of a flow does not have to.
//!
//! Two numbers are wrong whenever a person types them: how many things to run
//! at once, and how often to retry. Both depend on the machine and on what
//! every other call site is doing at the same moment, which no call site can
//! see. So a flow does not choose them; the root [`Scope`](crate::script::Scope)
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
//! rather than reading a clock, so a replay under simulation is exact.
//!
//! # Fan-out is sized to the machine
//!
//! `each x in xs:` with no bound runs at most [`Policy::fan_out`] bodies at
//! once: [`FAN_OUT_PER_CORE`] per available core, capped at
//! [`MAX_IN_FLIGHT`](crate::script::MAX_IN_FLIGHT). A body in `each` spends its
//! life waiting, so the bound is a ceiling on outstanding waits rather than on
//! CPU work; it scales with the host so the same flow neither starves a large
//! machine nor floods a small one. An external limit (an API that allows four
//! concurrent calls) is still said explicitly with `at most (limit)`.

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};

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

/// The decisions shared by every step under one root scope.
#[derive(Debug)]
pub(super) struct Policy {
    /// Bodies an unbounded `each` may run at once.
    fan_out: NonZeroUsize,
    /// Retries left, in thousandths.
    retries: AtomicU64,
}

impl Policy {
    /// The policy for the machine this runs on.
    pub(super) fn for_this_machine() -> Self {
        let cores = std::thread::available_parallelism().map_or(1, NonZeroUsize::get);
        let fan_out = cores.saturating_mul(FAN_OUT_PER_CORE).min(MAX_IN_FLIGHT);
        Self {
            fan_out: NonZeroUsize::new(fan_out).unwrap_or(NonZeroUsize::MIN),
            retries: AtomicU64::new(RETRY_RESERVE.saturating_mul(MILLI)),
        }
    }

    /// How many bodies an `each` with no bound runs at once.
    pub(super) const fn fan_out(&self) -> NonZeroUsize {
        self.fan_out
    }

    /// A `retry` block is making its first attempt: earn a fraction of a retry.
    pub(super) fn first_attempt(&self) {
        // Relaxed: the balance is a counter, not a guard for other memory.
        let _previous = self
            .retries
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |held| {
                Some(held.saturating_add(RETRY_RATIO_MILLI))
            });
    }

    /// Spend one retry if the run still has one; `false` means the failure is
    /// systemic and repeating it would only add load.
    pub(super) fn take_retry(&self) -> bool {
        self.retries
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |held| {
                held.checked_sub(MILLI)
            })
            .is_ok()
    }
}
