//! The tunables for the adaptive window and the rules cache, validated where they are written.

use std::time::Duration;

use super::PolitenessError;

/// Tunables for the adaptive window and the rules cache.
///
/// Private fields and a validating constructor, not a bag of public integers: a
/// policy whose minimum exceeds its maximum, or whose window is zero, is a run
/// that stalls, and the place to refuse that is where it is written.
///
/// The values are constructor arguments rather than constants inside the gate,
/// because the right ones depend on the target and on the operator's
/// obligations. [`Self::DEFAULT`] is declared, not fitted, and until a
/// calibration exists (see `docs/general-bot-fold.md` §6 item 13) it is an
/// unmeasured default rather than a validated one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PolitenessPolicy {
    /// Smallest a host or origin window may shrink to.
    min_window: u32,
    /// Largest a host or origin window may grow to.
    max_window: u32,
    /// Width the egress constraint starts at.
    egress_window: u32,
    /// Consecutive successes required before a window grows by one.
    increase_after: u32,
    /// Multiplicative-decrease factor as an integer percentage.
    decrease_percent: u32,
    /// How long an unreachable rules file is honoured.
    unreachable_grace: Duration,
    /// How long to wait before re-fetching rules or re-resolving a name.
    retry_after_failure: Duration,
}

impl PolitenessPolicy {
    /// The declared default. Unmeasured — see the type documentation.
    ///
    /// The egress starts at [`Self::max_window`] rather than
    /// [`Self::min_window`], because the egress interface is the one resource
    /// the operator owns and can measure. A run that serialized itself to one
    /// request at a time against its own network would be over-polite in a way
    /// nothing adapts out of, since a window only grows on success.
    pub const DEFAULT: Self = Self {
        min_window: 1,
        max_window: 4,
        egress_window: 4,
        increase_after: 8,
        decrease_percent: 70,
        unreachable_grace: Duration::from_secs(2_592_000),
        retry_after_failure: Duration::from_secs(60),
    };

    /// Validates a policy.
    pub fn new(
        min_window: u32,
        max_window: u32,
        egress_window: u32,
        increase_after: u32,
        decrease_percent: u32,
    ) -> Result<Self, PolitenessError> {
        if min_window == 0 {
            let refusal = Err(PolitenessError::ZeroWindow);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        }
        if min_window > max_window {
            let refusal = Err(PolitenessError::InvertedWindow {
                min_window,
                max_window,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        }
        if egress_window == 0 || egress_window > max_window {
            let refusal = Err(PolitenessError::InvalidEgressWindow {
                egress_window,
                max_window,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        }
        if decrease_percent == 0 || decrease_percent > 100 {
            let refusal = Err(PolitenessError::InvalidDecreasePercent { decrease_percent });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        }
        Ok(Self {
            min_window,
            max_window,
            egress_window,
            increase_after,
            decrease_percent,
            ..Self::DEFAULT
        })
    }

    /// Returns the smallest a window may shrink to.
    #[must_use]
    pub const fn min_window(&self) -> u32 {
        self.min_window
    }

    /// Returns the largest a window may grow to.
    #[must_use]
    pub const fn max_window(&self) -> u32 {
        self.max_window
    }

    /// Returns the width the egress constraint starts at.
    #[must_use]
    pub const fn egress_window(&self) -> u32 {
        self.egress_window
    }

    /// Returns the consecutive successes required to grow a window.
    #[must_use]
    pub const fn increase_after(&self) -> u32 {
        self.increase_after
    }

    /// Returns the multiplicative-decrease factor as a percentage.
    #[must_use]
    pub const fn decrease_percent(&self) -> u32 {
        self.decrease_percent
    }

    /// Returns how long an unreachable rules file is honoured.
    #[must_use]
    pub const fn unreachable_grace(&self) -> Duration {
        self.unreachable_grace
    }

    /// Returns how long to wait before re-fetching rules or re-resolving a name.
    #[must_use]
    pub const fn retry_after_failure(&self) -> Duration {
        self.retry_after_failure
    }

    /// The window after a multiplicative decrease from `window`.
    ///
    /// Floors at [`Self::min_window`] and uses saturating fixed-point
    /// arithmetic, so no operation here can wrap or divide by zero.
    #[must_use]
    pub fn decrease(&self, window: u32) -> u32 {
        let scaled = window
            .saturating_mul(self.decrease_percent)
            .saturating_div(100);
        scaled.clamp(self.min_window, self.max_window)
    }

    /// The window after one more consecutive success.
    #[must_use]
    pub fn increase(&self, window: u32) -> u32 {
        window
            .saturating_add(1)
            .clamp(self.min_window, self.max_window)
    }
}

impl Default for PolitenessPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}
