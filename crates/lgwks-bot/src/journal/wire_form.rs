//! The two journal records whose archived form is an enum.
//!
//! They are declared here, in a private module, and re-exported from
//! `journal` under their own names. The archived companions are re-exported the
//! same way. Nothing else in this module is reachable from outside.
//!
//! # Why they are not declared beside the rest of the journal
//!
//! rkyv's `Archive` derive emits two companions for an enum: the archived type
//! and a *resolver*, the value that carries the not-yet-resolved shape through
//! serialization. Both are generated with the visibility of the type they come
//! from, and the derive gives a caller no way to annotate the resolver:
//! `#[rkyv(attr(..))]` reaches the archived type only, and there is no
//! variant-level `attr` at all. So for an enum declared `pub` in a public
//! module, `pub enum XResolver` is generated, exhaustive, with undocumented
//! fields, and none of that can be written differently at the declaration.
//!
//! That collides with this workspace's lint contract.
//! `clippy::exhaustive_enums` is forbidden, so an exported enum cannot be
//! exhaustive; `missing_docs` is denied, so an exported field cannot be
//! undocumented; and a `forbid` cannot be lowered at the use site, so there is
//! no `#[allow]` to reach for. The resolver is not a type any caller chose and
//! not one the journal's API means to offer — it is the derive's scratch space.
//!
//! A private module is what keeps it out of the API. Effective visibility is
//! per-item, so re-exporting these four names exports exactly those four: the
//! resolvers stay declared-but-unreachable, and the lint never sees them. The
//! alternative was to stop deriving the wire form for these two records, which
//! would put a hand-written encoder back beside the estate's, and that is the
//! thing this migration exists to remove.
//!
//! The structs that carry a wire form (`Verification`, `EffectKey`) stay where
//! they are: a struct's resolver is a struct, which no lint in the contract
//! refuses, so they do not need the treatment.

use crate::ecs::EffectEvidence;
use crate::effect::EffectKey;

use super::Verification;

/// Whether the named predicate held.
///
/// Both arms are recorded. A predicate that was evaluated and did not hold is a
/// fact about the run, and discarding it would leave the report unable to say
/// why an action was not verified.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    lgwks_std::wire::Archive,
    lgwks_std::wire::Serialize,
    lgwks_std::wire::Deserialize,
)]
#[rkyv(attr(non_exhaustive), crate = lgwks_std::wire::rkyv, compare(PartialEq), derive(Debug))]
#[non_exhaustive]
pub enum VerificationResult {
    /// The predicate held.
    Satisfied,
    /// The predicate did not hold. This is not a transport failure and not a
    /// statement about whether the effect landed: it is the answer to the
    /// question the predicate asked.
    NotSatisfied,
}

/// One fact about one attempt, in the order it has to be recorded.
///
/// The order is not a convention the journal trusts the caller to follow: the
/// ladder in [`EventKind::next`](crate::journal::EventKind::next) is enforced at
/// the append point, so an event that cannot follow what is already committed is
/// refused rather than stored.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    lgwks_std::wire::Archive,
    lgwks_std::wire::Serialize,
    lgwks_std::wire::Deserialize,
)]
#[rkyv(attr(non_exhaustive), crate = lgwks_std::wire::rkyv, compare(PartialEq), derive(Debug))]
#[non_exhaustive]
pub enum EffectEvent {
    /// The intent was frozen and admitted, before any authority was sought.
    IntentAdmitted {
        /// The attempt this fact is about.
        #[rkyv(attr(doc = "The attempt this fact is about."))]
        key: EffectKey,
    },
    /// Authority was obtained and this exact attempt was prepared for handoff.
    ///
    /// This is the append that makes a crash survivable. After it, a recovered
    /// controller knows an attempt *may* have reached the external system, and
    /// must treat the outcome as unknown until evidence settles it.
    DispatchPrepared {
        /// The attempt this fact is about.
        #[rkyv(attr(doc = "The attempt this fact is about."))]
        key: EffectKey,
    },
    /// What the evidence says about whether the effect landed.
    OutcomeObserved {
        /// The attempt this fact is about.
        #[rkyv(attr(doc = "The attempt this fact is about."))]
        key: EffectKey,
        /// The fact the caller established.
        #[rkyv(attr(doc = "The fact the caller established."))]
        evidence: EffectEvidence,
    },
    /// A named predicate was evaluated over observations newer than the effect.
    Verified {
        /// The attempt this fact is about.
        #[rkyv(attr(doc = "The attempt this fact is about."))]
        key: EffectKey,
        /// The predicate, its version, its input and its answer.
        #[rkyv(attr(doc = "The predicate, its version, its input and its answer."))]
        verification: Verification,
    },
}
