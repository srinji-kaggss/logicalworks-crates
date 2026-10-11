//! Whether a failed attempt may be tried again.
//!
//! The vocabulary already existed. [`RetryClass`] says what a failure permits
//! and [`DispatchCertainty`] says what it
//! established, and `BotError::retry_class` is the total map from one to the
//! other that never inspects a rendered cause. What did not exist is the
//! decision: a class of `Safe` says the effect did not happen, and it says
//! nothing about whether the run can still afford an attempt, still holds
//! authority, still means the same thing, or whether an `Unsettled` failure can
//! be retried under a real deduplication contract.
//!
//! RQ-009 states the rule as four conjuncts:
//!
//! ```text
//! retry_admissible = budget_remaining
//!   AND live_authority
//!   AND unchanged_logical_intent
//!   AND (proven_not_applied
//!        OR remote_idempotency_contract_valid_for_same_key_and_payload)
//! ```
//!
//! This module is that rule and nothing else. Every input is a fact the caller
//! established, which is the same shape `DispatchCertainty` already has: a
//! producer states what it knows and the policy composes it. The function does
//! not reach for a clock, a broker or a journal, so the rule is a pure function
//! of its inputs and the tests below can pin every branch.
//!
//! # What a refusal reports
//!
//! Every conjunct that failed, not the first one. The estate already made this
//! argument for capabilities: a check that reports one missing item at a time
//! makes admission a loop in which each pass reveals one more word, and the
//! repair cannot be written until the last of them. The same holds here, and the
//! four conjuncts have four unrelated repairs.
//!
//! # The one conjunct with a real contract behind it
//!
//! `proven_not_applied` is evidence. The other half of that disjunct is a remote
//! deduplication contract, and it is where the interesting failure lives: a
//! contract whose retention window has passed does not rule out a second
//! application unless the remote demonstrably refuses a late duplicate. A remote
//! that applies it again, or whose behaviour past the window is unspecified,
//! gives no deduplication at all. A GUI click has no such contract in any form,
//! which is why it can never reach this disjunct.
//!
//! [`RetryClass`]: crate::RetryClass
//! [`DispatchCertainty`]: crate::DispatchCertainty

use core::fmt;
use core::time::Duration;

use crate::effect::{ActionDigest, ActionId, EffectKey};
use crate::error::RetryClass;
use crate::journal::EffectEvidence;
use crate::spec::RetryPolicy;

/// Ceiling on a deduplication scope label.
///
/// Long enough for a tenant, account or endpoint identifier and short enough
/// that a contract record stays bounded. A label beyond this is refused rather
/// than truncated, because a truncated scope silently merges two partitions.
pub const MAX_SCOPE_BYTES: usize = 256;

/// Why a scope label was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ScopeError {
    /// The observed length in bytes.
    len: usize,
}

impl ScopeError {
    /// The observed length of the refused label, in bytes.
    ///
    /// Named for what it measures rather than `len`, because this is the length
    /// of the input that was rejected and not the size of anything the caller
    /// holds.
    #[must_use]
    pub const fn observed_len(self) -> usize {
        self.len
    }
}

impl fmt::Display for ScopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "a deduplication scope must be at most {MAX_SCOPE_BYTES} bytes, got {}",
            self.len
        )
    }
}

impl std::error::Error for ScopeError {}

/// The partition a remote deduplication key is unique within.
///
/// Recorded on the contract because two remotes, or two tenants on one remote,
/// can use the same request key for different requests. It is recorded rather
/// than compared here: the caller that presents a contract is asserting it
/// covers this resend, exactly as the caller that reports live authority is
/// asserting that.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DedupScope(String);

impl DedupScope {
    /// A scope label, bounded by [`MAX_SCOPE_BYTES`].
    ///
    /// # Errors
    ///
    /// [`ScopeError`] when the label is longer than the ceiling.
    pub fn new(text: impl Into<String>) -> Result<Self, ScopeError> {
        let text = text.into();
        if text.len() > MAX_SCOPE_BYTES {
            let refusal = Err(ScopeError { len: text.len() });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        }
        Ok(Self(text))
    }

    /// The label.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DedupScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a remote system does with a request whose deduplication record has
/// expired.
///
/// This is the field that decides whether a contract is worth anything past its
/// window, and it is the field an adapter is most tempted to leave unstated.
/// "Unspecified" is not the same as "refused", and treating it as one is how a
/// duplicate gets sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LateArrival {
    /// The remote refuses a request whose deduplication record has expired, so
    /// a late duplicate cannot be applied a second time.
    Refused,
    /// The remote applies it again. The contract covers the retention window
    /// and nothing after it.
    Applied,
    /// The remote's behaviour past the window is not specified, or not known to
    /// the adapter.
    Unspecified,
}

impl LateArrival {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Refused => "refused",
            Self::Applied => "applied",
            Self::Unspecified => "unspecified",
        }
    }
}

impl fmt::Display for LateArrival {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a remote deduplicating API promises about a resend.
///
/// RQ-009 lists what must be recorded, and all five are fields here: the logical
/// request key, the payload binding, the scope, the retention window and the
/// late-arrival behaviour. The key and the payload binding reuse the effect
/// key's own vocabulary rather than minting new types, because they are already
/// exactly the right split: the [`ActionId`] is the logical intent that stays
/// fixed across a resend, and [`ActionDigest`] is the bound payload that must
/// not change with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeduplicationContract {
    /// The logical request key the remote deduplicates on. Fixed across a
    /// resend, which is what makes a resend a resend.
    action: ActionId,
    /// The bound payload. A resend with a different payload is a different
    /// request and the remote is not obliged to deduplicate it.
    payload: ActionDigest,
    /// The partition the key is unique within.
    scope: DedupScope,
    /// How long the remote remembers the key.
    retention: Duration,
    /// What happens to a duplicate that arrives after the window.
    late_arrival: LateArrival,
}

impl DeduplicationContract {
    /// Record a contract.
    #[must_use]
    pub const fn new(
        action: ActionId,
        payload: ActionDigest,
        scope: DedupScope,
        retention: Duration,
        late_arrival: LateArrival,
    ) -> Self {
        Self {
            action,
            payload,
            scope,
            retention,
            late_arrival,
        }
    }

    /// The logical request key.
    #[must_use]
    pub const fn action(&self) -> ActionId {
        self.action
    }

    /// The digest of the payload this deduplication contract is bound to.
    ///
    /// A resend with a different payload is a different request, and the remote
    /// is not obliged to deduplicate it -- so a caller confirming that a retry is
    /// still the same request compares this rather than the action id alone.
    #[must_use]
    pub const fn payload(&self) -> ActionDigest {
        self.payload
    }

    /// The partition the key is unique within.
    #[must_use]
    pub const fn scope(&self) -> &DedupScope {
        &self.scope
    }

    /// How long the remote remembers the key.
    #[must_use]
    pub const fn retention(&self) -> Duration {
        self.retention
    }

    /// What happens to a duplicate that arrives after the window.
    #[must_use]
    pub const fn late_arrival(&self) -> LateArrival {
        self.late_arrival
    }

    /// Whether this contract rules out a second application of a resend of
    /// `action` carrying `payload`, `elapsed` after the first attempt.
    ///
    /// False unless the key and the payload both match, because a contract for
    /// a different request says nothing about this one.
    ///
    /// Inside the retention window the remote deduplicates by construction, so
    /// the answer is yes. Past it, the answer is yes only when the remote
    /// refuses a late duplicate: `Applied` means a second application, and
    /// `Unspecified` means nobody knows, which is not a basis for sending.
    #[must_use]
    pub fn rules_out_duplicate(
        &self,
        action: ActionId,
        payload: ActionDigest,
        elapsed: Duration,
    ) -> bool {
        if self.action != action || self.payload != payload {
            return false;
        }
        if elapsed < self.retention {
            return true;
        }
        matches!(self.late_arrival, LateArrival::Refused)
    }
}

/// One conjunct of RQ-009's rule that did not hold.
///
/// One variant each, because the repairs are unrelated: a spent budget is
/// extended, absent authority is reacquired, a changed intent is re-admitted,
/// and a missing proof needs either evidence or a contract with the remote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RetryRefusal {
    /// The failure is permanent, so no retry can change the answer. This is
    /// [`RetryClass::Never`].
    PermanentFailure,
    /// The attempt budget is spent.
    BudgetExhausted {
        /// Attempts already used.
        attempts_used: u32,
        /// Attempts the policy allows.
        max_attempts: u32,
    },
    /// The caller no longer holds live authority for the target.
    AuthorityNotLive,
    /// The logical intent changed. A resend of a different intent is a
    /// different action, not a retry of this one.
    IntentChanged,
    /// Nothing established that the effect did not happen, and no deduplication
    /// contract rules out a second application.
    NotProvenUndone,
}

impl fmt::Display for RetryRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::PermanentFailure => f.write_str("the failure is permanent"),
            Self::BudgetExhausted {
                attempts_used,
                max_attempts,
            } => write!(
                f,
                "the budget is spent: {attempts_used} of {max_attempts} attempts used"
            ),
            Self::AuthorityNotLive => f.write_str("authority for the target is not live"),
            Self::IntentChanged => {
                f.write_str("the logical intent changed, so this is not a retry of that attempt")
            }
            Self::NotProvenUndone => f.write_str(
                "nothing established that the effect did not happen, and no deduplication \
                 contract rules out a second application",
            ),
        }
    }
}

/// What the rule decided.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RetryDecision {
    /// Retry. The effect demonstrably did not happen, so a resend is not a
    /// possible duplicate.
    Safe,
    /// Retry under the recorded contract, which rules out a second
    /// application.
    UnderContract,
    /// Do not retry, for every reason that applies.
    Refused(Vec<RetryRefusal>),
}

impl RetryDecision {
    /// Whether the decision permits an attempt.
    #[must_use]
    pub const fn permits_retry(&self) -> bool {
        !matches!(self, Self::Refused(_))
    }

    /// Every reason the decision refused, or an empty slice when it did not.
    #[must_use]
    pub fn refusals(&self) -> &[RetryRefusal] {
        // The scrutinee is `*self` so each pattern's type is the enum's own type
        // rather than a reference to it, and the one non-`Copy` payload is bound
        // by `ref`. `clippy::pattern_type_mismatch` is forbidden in this
        // workspace.
        match *self {
            Self::Refused(ref reasons) => reasons,
            Self::Safe | Self::UnderContract => &[],
        }
    }
}

impl fmt::Display for RetryDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Safe => f.write_str("retry: the effect did not happen"),
            Self::UnderContract => f.write_str("retry: a deduplication contract covers it"),
            Self::Refused(ref reasons) => {
                f.write_str("do not retry")?;
                for (index, reason) in reasons.iter().enumerate() {
                    f.write_str(if index == 0 { ": " } else { "; " })?;
                    write!(f, "{reason}")?;
                }
                Ok(())
            }
        }
    }
}

/// The facts the rule reads.
///
/// Every one is something a caller established rather than something this
/// module can check, which is the same contract [`DispatchCertainty`] has with
/// its producers. The defaults are the refusing direction wherever a fact is
/// optional, so a caller that sets only what it knows gets a refusal rather
/// than an accidental retry.
///
/// [`DispatchCertainty`]: crate::DispatchCertainty
#[derive(Debug, Clone)]
pub struct RetryFacts {
    /// The declared budget.
    policy: RetryPolicy,
    /// Attempts already used on this entry.
    attempts_used: u32,
    /// What the failure permits, from `BotError::retry_class`.
    class: RetryClass,
    /// The attempt that may have landed. Its action is the logical request key
    /// and its digest is the payload that was bound.
    dispatched: EffectKey,
    /// The payload binding of the intent as it stands now.
    proposed: ActionDigest,
    /// Whether the caller still holds live authority for the target.
    authority_live: bool,
    /// What the caller established about whether the effect landed.
    evidence: Option<EffectEvidence>,
    /// A remote contract covering a resend, where one exists.
    contract: Option<DeduplicationContract>,
    /// How long has passed since the attempt that may have landed.
    elapsed: Duration,
}

impl RetryFacts {
    /// The facts the rule cannot decide without.
    ///
    /// `dispatched` is the attempt that may have landed, and `proposed` is the
    /// payload binding the resend would carry. The two are separate arguments
    /// rather than one, because "the intent changed" is exactly the comparison
    /// between them and a caller should not be able to state that fact
    /// independently of the values it is about.
    #[must_use]
    pub const fn new(
        policy: RetryPolicy,
        attempts_used: u32,
        class: RetryClass,
        dispatched: EffectKey,
        proposed: ActionDigest,
    ) -> Self {
        Self {
            policy,
            attempts_used,
            class,
            dispatched,
            proposed,
            // The refusing direction. A caller that has not established live
            // authority has not established it.
            authority_live: false,
            evidence: None,
            contract: None,
            elapsed: Duration::ZERO,
        }
    }

    /// State that the caller still holds live authority for the target.
    ///
    /// There is no `false` case to express, so there is no argument to pass.
    /// Authority defaults to not live, which means a `bool` here would offer a
    /// toggle with exactly one meaningful setting and one that spelled out the
    /// default — the shape a reader has to consult this doc comment to decode,
    /// and the one that would let `with_authority(false)` look deliberate.
    #[must_use]
    pub const fn with_live_authority(mut self) -> Self {
        self.authority_live = true;
        self
    }

    /// State what the caller established about the effect.
    #[must_use]
    pub const fn with_evidence(mut self, evidence: EffectEvidence) -> Self {
        self.evidence = Some(evidence);
        self
    }

    /// Record the remote contract that covers a resend.
    #[must_use]
    pub fn with_contract(mut self, contract: DeduplicationContract) -> Self {
        self.contract = Some(contract);
        self
    }

    /// State how long has passed since the attempt that may have landed.
    ///
    /// Only the contract's retention window reads this. It defaults to zero,
    /// which is the case where a window of any length still covers the resend.
    #[must_use]
    pub const fn with_elapsed(mut self, elapsed: Duration) -> Self {
        self.elapsed = elapsed;
        self
    }

    /// Whether the caller proved the effect did not happen.
    fn proven_undone(&self) -> bool {
        matches!(self.evidence, Some(EffectEvidence::NotApplied))
    }

    /// Whether a recorded contract rules out a second application.
    fn covered_by_contract(&self) -> bool {
        match self.contract {
            Some(ref contract) => contract.rules_out_duplicate(
                self.dispatched.action(),
                self.dispatched.digest(),
                self.elapsed,
            ),
            None => false,
        }
    }
}

/// RQ-009's rule, as a pure function of the facts.
///
/// A permanent failure short-circuits: if no retry can change the answer, the
/// remaining conjuncts are moot and reporting them alongside it would be noise
/// rather than more information. Every other combination reports all of its
/// refusals.
#[must_use]
pub fn retry_admissible(facts: &RetryFacts) -> RetryDecision {
    if facts.class == RetryClass::Never {
        return RetryDecision::Refused(vec![RetryRefusal::PermanentFailure]);
    }

    let mut reasons = Vec::new();

    if facts.attempts_used >= facts.policy.max_attempts.max(1) {
        reasons.push(RetryRefusal::BudgetExhausted {
            attempts_used: facts.attempts_used,
            max_attempts: facts.policy.max_attempts.max(1),
        });
    }
    if !facts.authority_live {
        reasons.push(RetryRefusal::AuthorityNotLive);
    }
    if facts.proposed != facts.dispatched.digest() {
        reasons.push(RetryRefusal::IntentChanged);
    }

    let covered = facts.covered_by_contract();
    if !facts.proven_undone() && !covered {
        reasons.push(RetryRefusal::NotProvenUndone);
    }

    if !reasons.is_empty() {
        return RetryDecision::Refused(reasons);
    }
    if facts.proven_undone() {
        return RetryDecision::Safe;
    }
    RetryDecision::UnderContract
}

#[cfg(test)]
mod tests;
