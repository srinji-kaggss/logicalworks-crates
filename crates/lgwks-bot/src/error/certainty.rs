//! What a failure establishes about its effect: DispatchCertainty, RetryClass, and the total map from BotError to both that a retry policy reads.

use crate::ecs::EffectEvidence;

use super::BotError;

/// What a failed attempt establishes about the effect it was making.
///
/// The one fact a retry decision turns on, carried *by* the failure rather than
/// inferred from it. A consumer that has to read a cause string to learn
/// whether it may retry will eventually get it wrong, and the price of getting
/// it wrong is a duplicated merge, message, or process launch.
///
/// The arms are ordered by what they permit: [`Self::Refused`] permits nothing,
/// [`Self::NotDelivered`] permits a retry as a retry, and [`Self::Unsettled`]
/// permits a retry only against the caller's evidence.
///
/// The third case is deliberately its own variant rather than a
/// [`BotError::DomainError`] carrying [`Self::Unsettled`]: an effect that may
/// or may not have happened is [`BotError::EffectIndeterminate`], which is not
/// a domain error and must not become part of one — that is the catch-all this
/// vocabulary exists to prevent. No `DomainError` this crate constructs carries
/// `Unsettled`; the arm is here because it is the third answer to the question
/// this type asks, and a consumer reading the type should see all three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DispatchCertainty {
    /// Nothing was dispatched and nothing ever will be: the failure refused the
    /// input itself — a parse or schema refusal, a target that cannot be
    /// resolved, an action that will not accept the value it was handed, a
    /// schedule that will not validate.
    ///
    /// Permanent, and before dispatch. The same input produces the same
    /// refusal on every attempt, so a retry spends an attempt to learn an
    /// answer that was already known.
    Refused,
    /// The attempt was made and nothing reached the peer: no bytes could have
    /// reached a previous or live connection for this attempt, so the effect
    /// definitely did not happen.
    ///
    /// A retry is a retry, bounded by the caller's declared budget. This is the
    /// arm a connection that was never established, or a local read that failed
    /// before it produced anything, belongs to.
    NotDelivered,
    /// The attempt reached the peer, and whether it took effect is not knowable
    /// from the failure. A retry is a possible duplicate.
    Unsettled,
    /// The effect definitely happened.
    ///
    /// Produced only by a post-effect recording failure
    /// ([`BotError::EffectUnrecorded`]) whose evidence is
    /// [`Applied`](EffectEvidence::Applied): the action returned success, so the
    /// occurrence is a fact even though the journal has no record of it yet.
    /// A retry of the *effect* is forbidden — that is how a known success
    /// becomes a duplicate. Retrying the *append* is the repair, and it does not
    /// go through this type's retry map.
    Occurred,
}

impl DispatchCertainty {
    /// What this certainty permits a caller to do about the failure.
    #[must_use]
    pub const fn retry_class(self) -> RetryClass {
        match self {
            Self::Refused | Self::Occurred => RetryClass::Never,
            Self::NotDelivered => RetryClass::Safe,
            Self::Unsettled => RetryClass::RequiresEvidence,
        }
    }
}

/// What a failure permits a caller to do about it.
///
/// Read from the failure through [`BotError::retry_class`], which is total over
/// the vocabulary and never inspects a rendered cause. A caller that branches
/// on this is not choosing a retry policy by string-matching, which is the
/// thing the type exists to make unnecessary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RetryClass {
    /// No retry can change the answer: the failure is a refusal or a wiring
    /// violation, and repeating it produces the same refusal at the cost of an
    /// attempt.
    Never,
    /// The effect definitely did not happen, so a retry is a retry. Bounded by
    /// the caller's budget, which is what [`crate::spec::RetryPolicy`] declares.
    Safe,
    /// The effect may have happened, so a retry is a possible duplicate. Not
    /// attempted again without the caller reporting evidence.
    RequiresEvidence,
}

impl BotError {
    /// What this failure establishes about the effect it was making.
    ///
    /// Exhaustive over the vocabulary and computed from the value, never from
    /// the rendered cause. Every variant that is not a
    /// [`DomainError`](Self::DomainError) or an
    /// [`EffectIndeterminate`](Self::EffectIndeterminate) names a refusal that
    /// happens *before* an effect is attempted — a capability that is not
    /// granted, an erased verb handed the wrong type, a spec that does not
    /// validate, a tick that may not park the thread it was called on — so all
    /// of them answer [`DispatchCertainty::Refused`].
    #[must_use]
    pub fn dispatch_certainty(&self) -> DispatchCertainty {
        match *self {
            Self::DomainError { certainty, .. } => certainty,
            Self::EffectIndeterminate { .. } => DispatchCertainty::Unsettled,
            // The action ran and the outcome is a fact. Refused means "nothing
            // left the process", and NotDelivered means "a retry is a retry" —
            // both are lies about an effect that already returned.
            Self::EffectUnrecorded { evidence, .. } => match evidence {
                EffectEvidence::Applied => DispatchCertainty::Occurred,
                EffectEvidence::NotApplied => DispatchCertainty::NotDelivered,
            },
            // A cancelled observation read nothing, so nothing left the process
            // on this account and the next tick is a plain retry. Folding it
            // into the `Refused` arm above would make a caller classify a source
            // that recovers on the next tick as one whose wiring will never work.
            Self::PollStalled { .. } => DispatchCertainty::NotDelivered,
            // An unsettled reading is the same fact as a cancelled poll: the
            // value that was to be replaced is still the one the substrate
            // holds, so nothing was committed and a later tick is a plain retry.
            Self::UnstableObservation { .. } => DispatchCertainty::NotDelivered,
            // A lapsed credential is a refusal, and so is a rejected one: both
            // happen before any effect is attempted, and neither is repaired by
            // presenting the same authority again.
            Self::CredentialExpired { .. } | Self::CredentialRejected { .. } => {
                DispatchCertainty::Refused
            }
            // A lost write changed nothing: the winner's value stands and the
            // loser holds both versions, so the same expectation refused again
            // is the same refusal rather than a second attempt at the effect.
            Self::Conflict { .. } => DispatchCertainty::Refused,
            _ => DispatchCertainty::Refused,
        }
    }

    /// What this failure permits a caller to do about it.
    #[must_use]
    pub fn retry_class(&self) -> RetryClass {
        // A recording failure never re-enters the action, whatever the outcome
        // was: the append is the only thing left to do, and it is not an effect
        // attempt. `Never` here means "never dispatch again for this failure",
        // not "abandon the entry" — `failure_state` maps this variant to a
        // pending-record hold rather than to terminal abandonment.
        match *self {
            Self::EffectUnrecorded { .. } => RetryClass::Never,
            _ => self.dispatch_certainty().retry_class(),
        }
    }
}
