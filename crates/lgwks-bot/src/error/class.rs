//! How a failure classifies: what was dispatched, and what a retry may do.

use std::fmt;

use super::BotError;
use super::Escaped;
use crate::cap::{Auth, Cap, Deficit};
use crate::ecs::EffectEvidence;

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

/// `Deficit`'s rendering lives here rather than in `cap.rs`, and the reason is
/// the escaping contract below rather than the module it describes: every
/// untrusted field a deficit carries is a capability name or a domain id, both
/// of which are `String`s a caller supplied, so both go through `Escaped`, the
/// private helper below. It is named here rather than linked because it is not
/// reachable from the public docs, and a link a reader cannot follow is worse
/// than the plain name.
/// Putting the arm next to `Escaped` is what keeps a later field from being
/// interpolated without it.
impl fmt::Display for Deficit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = self.len();
        // The count comes first because it is the number a person triaging
        // wants: "one thing is missing" and "nine things are missing" call for
        // different next actions, and the list alone does not say which.
        write!(f, "{count} ungranted")?;
        for (index, shortage) in self.shortages().enumerate() {
            f.write_str(if index == 0 { ": " } else { ", " })?;
            write!(f, "{}", Escaped(shortage.required().as_str()))?;
            if let Some(demand) = shortage.demand() {
                write!(f, " (required by {})", Escaped(demand.domain()))?;
            }
        }
        Ok(())
    }
}

impl From<Deficit> for BotError {
    fn from(deficit: Deficit) -> Self {
        Self::CapabilityDenied { deficit }
    }
}

/// The one answer a domain with no bound transport gives, in two parts.
///
/// Authority first: a caller that was never granted the capability is refused by
/// [`Auth::check`] as [`BotError::CapabilityDenied`], which carries the whole
/// shortfall rather than one word of it, and only a caller that *was* granted it
/// reaches the second part. The order matters — a missing capability is the
/// repairable fault, and reporting "binding required" to a caller whose grant
/// was short would send them looking for a transport that was never the problem.
///
/// Then the `binding required` refusal, which is [`BotError::DomainError`] at
/// [`DispatchCertainty::Refused`]: the domain is constructed with a channel, a
/// path or a destination and no client behind it, so nothing was dispatched and
/// nothing ever will be. `Refused` rather than `NotDelivered` because a retry of
/// this call produces the same refusal at the cost of an attempt.
///
/// One function rather than one construction per domain, for two reasons that
/// point the same way. A sixth site that spelled it again would drift — a site
/// that forgot [`DispatchCertainty`] would compile, because the field is a
/// variant of a `#[non_exhaustive]` enum the caller may match loosely, and the
/// drift would only be visible in a retry policy that then re-dispatches a
/// permanent refusal. And the trace emission has to be here rather than at the
/// call sites: `scan` refuses an unlogged `return Err(..)`, so a site that built
/// the value itself would either be a finding or would need its own copy of the
/// emission, and the second is the first defect wearing a hat.
///
/// `verb` is the operation the caller asked for in its own words — `polling`,
/// `querying`, `notifying` — and `target` is the untrusted channel, path or
/// destination it asked about, rendered through [`Escaped`] by `Display` for the
/// same reason every other untrusted field is.
pub(crate) fn unbound_domain<T>(
    auth: &Auth,
    required: &[Cap],
    domain: &str,
    verb: &str,
    target: &str,
) -> Result<T, BotError> {
    auth.check(required)?;
    let refusal = Err(BotError::DomainError {
        domain: domain.to_owned(),
        certainty: DispatchCertainty::Refused,
        cause: format!("{verb} {target:?} — binding required"),
    });
    lgwks_std::trace::debug!(
        error = ?refusal.as_ref().err(),
        %domain,
        "unbound domain: refusing a call with no bound transport",
    );
    refusal
}
