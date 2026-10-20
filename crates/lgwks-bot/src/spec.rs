//! `spec` owns the bot builder and serializable spec, enforcing
//! INV-BOT-SPEC-SERIALIZABLE: every `BotSpec` round-trips through JSON and
//! INV-BOT-TUPLE-WIRE: causal chains are `(condition, action)` tuples.
//!
//! # Chains are typed against their source
//!
//! [`ObserveBuilder::on`] takes a condition that reads the source's output and
//! an action that takes exactly it, so a chain that cannot work does not build.
//!
//! `a_chain_typed_against_its_source_builds_under_an_effect_scope` (`tests/it/spec_front_door.rs`)
//! runs it: a source that reports a `u16`, a closure over `&u16` and an action whose `Input` is `u16`
//! build under an effect scope, and the bound `on` is stated against is satisfied by a shipped
//! condition as well as by the closure.
//!
//! An action that takes something else is a compile error — `E0271`, `type
//! mismatch resolving <Courier as Execute>::Input == u16` — rather than a
//! downcast miss discovered on a tick.
//!
//! `an_action_that_takes_another_type_is_refused_with_e0271` (`tests/it/t02_compile_surface.rs`) holds
//! it: a consumer crate declares an action whose `Input` is `u32` against a source that produces `u16`,
//! and the compiler refuses it with `E0271`.
//!
//! The condition is checked the same way, against the same `S::Output`.
//!
//! ## What this refuses
//!
//! A deliberate tightening, and it rejects chains that used to compile and then
//! fail on a tick. `EcsObserveBuilder` is generic over its source and
//! `on` has no free type parameter; before, it had one (`on<C, A, T>`) tied to
//! nothing at all, so a condition reading one type could sit in front of an
//! action expecting another. The failure was a downcast miss at tick time,
//! reported as a domain error — so it read as "the domain failed" and, before
//! certainty was carried, spent a whole retry budget on a defect no attempt
//! could repair. Two chains that previously compiled now do not:
//!
//! - a condition whose `Evaluate<T>` is not `Evaluate<S::Output>`;
//! - an action whose `Execute::Input` is not `S::Output`.
//!
//! Both are defects, not features, and neither had a working tick-time story to
//! preserve. Chains that were correct are unaffected.
//!
//! ## The rule a future verb has to keep
//!
//! One type across the chain is sound because every verb so far consumes its
//! input and produces a caller-visible one — [`Evaluate::check`](crate::verb::Evaluate::check) returns a
//! `bool`, a pure predicate with no derived output. **No stage may introduce a
//! caller-selected type parameter disconnected from its input.** A stage that
//! transforms the value must carry the transformation as an associated type
//! (`Transform<I>::Output`), so the next stage's input follows from the previous
//! stage's output instead of being chosen by the caller. A free parameter at
//! this seam is how the chain became unprovable the first time.
//!
//! [`ObserveBuilder::on`]: crate::spec::ObserveBuilder::on

mod admission;
#[cfg(test)]
mod tests;

pub use admission::{Admission, Need, NeedSet};

use lgwks_std::json::{Deserialize, Serialize};
use std::any::{Any, TypeId, type_name};

use super::cap::{Auth, Cap};
use super::error::BotError;
use super::gate::GrantSet;
use super::verb::RefreshReason;

// ── Serializable spec ──────────────────────────────────────────────────────

/// The serializable bot contract: what an AI emits and what a manifest
/// contains. `from_json` validates its shape; capability validation happens at
/// build time from a [`GrantSet`], not from a spec.
///
/// `#[non_exhaustive]`: this is a wire contract, so a new field is additive for
/// the crate and a compile error for a consumer that built the struct
/// literally. Construct with [`BotSpec::new`], or parse with
/// [`BotSpec::from_json`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields)]
#[non_exhaustive]
pub struct BotSpec {
    /// The document version this build materializes.
    ///
    /// Defaulted on parse, so a spec written before the field existed reads as
    /// [`BotSpec::CURRENT_VERSION`]. A version this build does not implement is
    /// refused by [`BotSpec::from_json`] and by the materializer rather than
    /// partially interpreted, because a later version may give an existing field
    /// a meaning this one does not have.
    #[serde(default = "current_spec_version")]
    pub version: u32,
    /// The bot's unique name. Must be non-empty: both builder entry points
    /// refuse an empty name with [`BotError::IncompleteSpec`]. `from_json`
    /// accepts one because it validates shape only; the build-time check is
    /// the one that binds. Read it with [`BotSpec::name`].
    pub(crate) name: String,
    /// Observation chains in declaration order. [`Bot::tick`] polls and fires in
    /// this order, so reordering changes which side effects run before an error.
    /// Empty is valid for the builder: a bot with no chains still serves direct
    /// [`Query`] and [`Execute`] calls.
    /// The materializer is stricter and refuses an empty list, because a
    /// materialized bot with nothing to observe is almost always a truncated
    /// document rather than an intentional one. Read it with [`BotSpec::chains`].
    ///
    /// Private with an accessor rather than a `pub` field: the wire contract is
    /// a transparent *view*, not a mutable handle a caller can reach into and
    /// invalidate a bound through. Serde still reads and writes it; a consumer
    /// reads it through [`BotSpec::chains`].
    pub(crate) chains: Vec<ChainSpec>,
}

/// The default `version` a parsed [`BotSpec`] carries when the field is absent.
///
/// A free function because `#[serde(default = "…")]` names a path, and a spec
/// written before the field existed must keep parsing as the current version.
fn current_spec_version() -> u32 {
    BotSpec::CURRENT_VERSION
}

impl BotSpec {
    /// The one document version this build materializes.
    ///
    /// Bump it when a field's meaning changes or a field is removed; a purely
    /// additive field is a compatible change and does not need a bump.
    pub const CURRENT_VERSION: u32 = 1;

    /// Assemble a spec from its parts, at [`BotSpec::CURRENT_VERSION`]. The
    /// arguments are taken verbatim; no shape validation runs here, because
    /// validation belongs to [`BotSpec::from_json`] and to the builder, not to
    /// construction.
    #[must_use]
    pub fn new(name: impl Into<String>, chains: Vec<ChainSpec>) -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            name: name.into(),
            chains,
        }
    }

    /// The bot's unique name.
    ///
    /// `a_spec_reads_back_its_name_its_version_and_its_chains` (`tests/it/spec_front_door.rs`) runs
    /// it: a spec parsed from JSON reads back its name, the current version and its empty chain list.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The observation chains, in declaration order.
    #[must_use]
    pub fn chains(&self) -> &[ChainSpec] {
        &self.chains
    }

    /// The document version this spec declares.
    #[must_use]
    pub fn version(&self) -> u32 {
        self.version
    }
}

/// One observation binding in a serializable spec.
///
/// `#[non_exhaustive]` for the same reason as [`BotSpec`]; construct with
/// [`ChainSpec::new`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields)]
#[non_exhaustive]
pub struct ChainSpec {
    /// The domain identifier of the observed source (e.g. `"gh::pr_status"`).
    /// The builder resolves it against a concrete [`Observe`]
    /// implementation; the spec itself never carries code. Read it with
    /// [`ChainSpec::source`].
    pub(crate) source: String,
    /// The target parameter for the source (e.g. `"owner/repo"`). Its meaning is
    /// defined by the source domain, not by the spec. Read it with
    /// [`ChainSpec::target`].
    pub(crate) target: String,
    /// Condition–action pairs: `[condition_id, { domain: target }]`. Evaluated
    /// in order, so the same pair placed earlier fires earlier on a tick. Read
    /// it with [`ChainSpec::on`].
    pub(crate) on: Vec<(String, ActionSpec)>,
}

impl ChainSpec {
    /// Assemble one chain binding from its parts, taken verbatim.
    #[must_use]
    pub fn new(
        source: impl Into<String>,
        target: impl Into<String>,
        on: Vec<(String, ActionSpec)>,
    ) -> Self {
        Self {
            source: source.into(),
            target: target.into(),
            on,
        }
    }

    /// The source domain identifier, spelled as the spec spells it.
    ///
    /// `a_chain_reads_back_its_source_its_target_and_its_actions` (`tests/it/spec_front_door.rs`) runs
    /// it: a chain built from a source, a target and one action reads all three back in order.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The source's target parameter, whose meaning the domain defines.
    #[must_use]
    pub fn target(&self) -> &str {
        &self.target
    }

    /// The `(condition, action)` pairs, in declaration order.
    #[must_use]
    pub fn on(&self) -> &[(String, ActionSpec)] {
        &self.on
    }
}

/// A serializable action reference.
///
/// `#[non_exhaustive]` for the same reason as [`BotSpec`]; construct with
/// [`ActionSpec::new`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields)]
#[non_exhaustive]
pub struct ActionSpec {
    /// The domain identifier (e.g. `"notify::slack"`). Read it with
    /// [`ActionSpec::domain`].
    pub(crate) domain: String,
    /// The target parameter (e.g. `"#deploys"`). Interpreted by `domain`. Read
    /// it with [`ActionSpec::target`].
    pub(crate) target: String,
}

impl ActionSpec {
    /// Assemble an action reference from its parts, taken verbatim.
    #[must_use]
    pub fn new(domain: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            domain: domain.into(),
            target: target.into(),
        }
    }

    /// The action's domain identifier.
    ///
    /// `an_action_reads_back_its_domain_and_its_target` (`tests/it/spec_front_door.rs`) runs it: an
    /// action built from a domain and a target reads both back unchanged.
    #[must_use]
    pub fn domain(&self) -> &str {
        &self.domain
    }

    /// The action's target parameter, whose meaning the domain defines.
    #[must_use]
    pub fn target(&self) -> &str {
        &self.target
    }
}

/// Build one `(condition, action)` tuple, erased for storage on a chain.
///
/// Shared rather than duplicated: the `Auth` issue, the downcast and the
/// type-mismatch error must not drift between call sites, and issuing the proof
/// *before* the downcast is the property that makes a type mismatch fail without
/// a side effect.
pub(crate) fn typed_entry<C, A, T>(condition: C, action: A) -> ChainEntry
where
    T: 'static,
    C: super::verb::Evaluate<T> + 'static,
    A: super::verb::Execute + 'static,
    A::Input: 'static,
    A::Output: 'static,
{
    ChainEntry {
        condition: Box::new(TypedEval {
            inner: condition,
            _marker: std::marker::PhantomData::<T>,
        }),
        action: Box::new(TypedExec::new(action)),
    }
}

/// Erases one [`Evaluate`] to [`EvaluateAny`], the way
/// [`TypedExec`] erases an action.
///
/// Both erasure wrappers have one implementation each: this one is reached both
/// by [`typed_entry`], where the condition and the action are typed against the
/// source together, and by [`Condition::new`](crate::Condition), where a
/// materializer pairs a condition it built against an already-erased source.
/// Keeping them on one type is what stops the two paths from diverging on the
/// downcast and the type-mismatch report.
pub(crate) struct TypedEval<C, T> {
    /// The condition, typed against `T`.
    pub(crate) inner: C,
    /// The type it was built for, carried only to tie `T` to this value.
    pub(crate) _marker: std::marker::PhantomData<T>,
}

impl<C: super::verb::Evaluate<T>, T: 'static> EvaluateAny for TypedEval<C, T> {
    fn check_any(&self, value: &Erased) -> Result<bool, BotError> {
        match value.as_any().downcast_ref::<T>() {
            Some(typed) => self.inner.check(typed),
            None => Err(BotError::EvaluateError {
                cause: format!(
                    "type mismatch in evaluate — expected {}, got {}",
                    type_name::<T>(),
                    value.witness.name(),
                ),
            }),
        }
    }
}

// ── Live bot ───────────────────────────────────────────────────────────────

#[cfg(feature = "ephemeral")]
pub use crate::ecs::EphemeralError;
/// A built bot: name, admitted capabilities, and the `bevy_ecs` world its
/// chains execute in.
///
/// **One bot, one executor.** `Bot` *is* the ECS bot: `tick` runs one schedule
/// step, and a condition is `Changed<Revision>` on the source entity rather than
/// a re-evaluation of a value that did not move. There is no second way to run a
/// bot, and no feature flag that adds one.
///
/// The rest of this re-export is the vocabulary of the substrate's ledger —
/// which work is outstanding, what is holding it, and how a caller settles an
/// effect that may or may not have happened. It is re-exported here, next to
/// `Bot`, because a caller reading [`Bot::pending`] or matching on
/// [`BotError::PendingTransition`] has to be
/// able to name what those return; `ecs` itself stays private, because it is the
/// implementation rather than a second way to run a bot.
pub use crate::ecs::{
    AbandonReason, EcsBot as Bot, EcsBuilder as BotBuilder, EcsObserveBuilder as ObserveBuilder,
    EffectEvidence, EffectScope, PendingWork, RetryPolicy, TransitionHold, WorkId,
};
#[cfg(feature = "ephemeral")]
pub use crate::effect::MintError;
pub use crate::effect::{EffectIdentity, EffectKey};

/// One `(condition, action)` tuple in a chain.
pub(crate) struct ChainEntry {
    /// The condition half. Type-erased because the builder accepts any
    /// `Evaluate<T>`; it downcasts the observed value back to `T` on check.
    pub(crate) condition: Box<dyn EvaluateAny>,
    /// The action half. Type-erased for the same reason; it downcasts the
    /// observed value back to the action's `Input` before running.
    pub(crate) action: Box<dyn ExecuteAny>,
}

impl ChainEntry {
    /// Assemble one entry from halves that are already erased.
    ///
    /// The materializer's seam: a chain built from wire data pairs a condition
    /// the [`Source`] built for its own output type with an
    /// action the registry built. The two arrive erased because that is the
    /// only shape a document can name, and this constructor is the one place
    /// they are joined, so the join cannot be re-implemented with a different
    /// pairing rule somewhere else.
    pub(crate) fn erased(condition: Box<dyn EvaluateAny>, action: Box<dyn ExecuteAny>) -> Self {
        Self { condition, action }
    }
}

// ── Type-erased verb wrappers ──────────────────────────────────────────────

/// What a value's type was, captured where the type was still a type parameter.
///
/// The erasure boundary is crossed in two places — the source is boxed into
/// `Box<dyn ObserveAny>` when the chain is declared, and the value it produces is
/// boxed into `Box<dyn Any>` when it is polled — and nothing in those types
/// survives to prove the two halves still agree. `Witness` is what does.
///
/// [`TypeId`] is the identity: two types are the same type exactly when their
/// ids are equal. The name is carried beside it only so a mismatch can be
/// reported as prose, and is never compared — the name is a hint, not an
/// identity, and shortening it can make two distinct types render alike.
///
/// The id is process-local and is not serializable, so this serves the Rust
/// path only. A durable identity for the same question — which type a chain's
/// source produces, readable by a materializer that has only wire data — is a
/// schema key, and no such key exists: [`DomainRegistry`]
/// maps an identifier to a constructor and stops there, so the type a source
/// produces is written down nowhere a document could name. Do not reach for
/// `TypeId` to answer it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Witness {
    /// The type, as an identity. This is the comparison.
    id: TypeId,
    /// The type, as prose. This is not.
    name: &'static str,
}

impl Witness {
    /// The witness for `T`, taken where `T` is still a type parameter.
    pub(crate) fn of<T: 'static>() -> Self {
        Self {
            id: TypeId::of::<T>(),
            name: type_name::<T>(),
        }
    }

    /// Whether these two witnesses name the same type.
    pub(crate) fn agrees_with(self, other: Self) -> bool {
        self.id == other.id
    }

    /// The type's name, for a diagnostic.
    pub(crate) fn name(self) -> &'static str {
        self.name
    }
}

/// A value at the erasure boundary, with the witness of the type it was erased
/// from.
///
/// One allocation, the same `Box<dyn Any>` the value had to become anyway; the
/// witness rides alongside it in the staging vector rather than in a second box.
/// It is not an `Option`: a value that was boxed here was boxed from a concrete
/// type, so the witness always exists.
///
/// The witness travels *with the value* rather than being read off it where it
/// arrives. `dyn Any` exposes `type_id()`, so the actual type was never out of
/// reach; what the carrier adds is the producer's own statement of what it
/// produced, made at the one point where the type was still a type parameter.
/// The rendezvous then compares one claim against another — a producer's against
/// a consumer's — which is the comparison this check exists for. Reading the
/// `type_id` instead would compare ground truth against an expectation, which is
/// a different and weaker thing: it can only say whether the value fits, never
/// whether the two halves were built to agree.
///
/// Two limits, both real, neither addressed here:
///
/// - [`TypeId`] is process-local and is not serializable, so this serves the
///   Rust path only. A materializer holding wire data instead of a `Witness`
///   needs a durable identity for the same question, and this field is where
///   that key goes once one exists — [`DomainRegistry`]
///   is where such a key would be declared. Do not reach for `TypeId` to answer
///   it.
/// - A witness is `TypeId::of::<S::Output>()`, so it distinguishes *types*, not
///   *chains*. Two chains that both produce a `u16` are indistinguishable to it:
///   a value produced by one and delivered to the other passes this check. It
///   proves the pairing is type-correct, which is all a type can prove, and that
///   is strictly more than the index pairing proved before it — but it is not a
///   chain identity, and the next reader should not assume it is one.
pub(crate) struct Erased {
    /// The value itself.
    pub(crate) value: Box<dyn Any>,
    /// What type it was before it was erased.
    pub(crate) witness: Witness,
}

impl Erased {
    /// Box `value` and record the type it is being erased from.
    pub(crate) fn new<T: 'static>(value: T) -> Self {
        Self {
            witness: Witness::of::<T>(),
            value: Box::new(value),
        }
    }

    /// The value as an erased reference, for a consumer that only needs `Any`.
    pub(crate) fn as_any(&self) -> &dyn Any {
        self.value.as_ref()
    }
}

/// Object-safe view of [`Observe`] that erases
/// `Output`. The blanket impl forwards each call to the concrete verb, so the
/// erasure costs one vtable hop and no extra allocation: `poll_any` boxes the
/// domain's own value at the type-erasure boundary, which is where it must be
/// boxed anyway. The caller's [`Auth`] is checked here, before the source is
/// touched, so an erasure bug cannot bypass the gate.
pub(crate) trait ObserveAny {
    /// Forwards to [`Observe::domain_id`].
    fn domain_id(&self) -> &str;
    /// Forwards to [`Observe::required_caps`].
    fn required_caps(&self) -> &[Cap];
    /// Forwards to
    /// [`Observe::cache_state`].
    ///
    /// Read on the same tick as `poll_any`, immediately after it resolves, so a
    /// source that reached a failure inside its own poll has already recorded it
    /// by the time this is asked. That ordering is why the reason is a
    /// *statement* about the source's caching rather than a second call: a
    /// second call could not see a poll that has not finished.
    fn cache_state(&self) -> Option<RefreshReason>;
    /// Forwards to [`Observe::revision`].
    ///
    /// Read by the substrate before the poll it may skip, on the calling thread
    /// and never beside a pending poll of this source, so the revision it
    /// compares against is the one the substrate committed with the value it
    /// holds. A source that reports none answers `None` and the substrate
    /// compares the value itself, which is the path every shipped source takes.
    fn revision(&self) -> Option<u64>;
    /// Issue an [`Auth`] for the observer's own caps and poll it.
    ///
    /// Returns `Ok(None)` when the source produced a value **equal** to
    /// `previous`, and `Ok(Some(_))` when it did not. Denies with
    /// [`BotError::CapabilityDenied`] before polling when the grant set does not
    /// cover the observer's caps.
    ///
    /// # Why the comparison is here and not at the caller
    ///
    /// This is the last point at which the output is a concrete `T::Output`.
    /// One frame further out it is a `Box<dyn Any>`, and `==` on two boxed
    /// values is an allocation, a memcpy and a free apiece, spent to answer a
    /// question that could be asked of the value in hand. In a steady-state bot
    /// the answer is "equal" on nearly every tick, so the box a caller-side
    /// comparison forces is pure waste: it is built, compared, and dropped.
    ///
    /// The boxed value carries its own [`Witness`], taken here, where the
    /// output type is still `T::Output`. That is the only place it can be
    /// taken: by the time the value reaches the chain that will consume it,
    /// both halves are erased and nothing remains to compare.
    ///
    /// # What `previous` is compared against
    ///
    /// `previous` is the payload the substrate is currently holding for this
    /// chain — the newest committed observation, or failing that the binding of
    /// a transition that owns it. A `None` return therefore means *equal to
    /// what the substrate already has*, which is the same question the chain's
    /// own `same` answers, and it is answered here by the same `PartialEq`.
    fn poll_any<'a>(
        &'a self,
        grants: &'a GrantSet,
        previous: Option<&'a Erased>,
    ) -> crate::BoxFuture<'a, Result<Option<Erased>, BotError>>;
}

/// The blanket impl carries `PartialEq` on the output because the comparison
/// above needs it. That is not a new requirement on a source: every chain is
/// closed by `EcsObserveBuilder::observe`, which already demands
/// `S::Output: PartialEq` for its change filter, so a source that could reach
/// this impl without it could not have been admitted to a chain anyway.
impl<T: super::verb::Observe + 'static> ObserveAny for T
where
    T::Output: PartialEq + 'static,
{
    fn domain_id(&self) -> &str {
        super::verb::Observe::domain_id(self)
    }

    fn required_caps(&self) -> &[Cap] {
        super::verb::Observe::required_caps(self)
    }

    fn cache_state(&self) -> Option<RefreshReason> {
        super::verb::Observe::cache_state(self)
    }

    fn revision(&self) -> Option<u64> {
        super::verb::Observe::revision(self)
    }

    fn poll_any<'a>(
        &'a self,
        grants: &'a GrantSet,
        previous: Option<&'a Erased>,
    ) -> crate::BoxFuture<'a, Result<Option<Erased>, BotError>> {
        Box::pin(async move {
            let auth: Auth = grants.issue(super::verb::Observe::required_caps(self))?;
            let value = self.poll((auth, ())).await?;
            // A `previous` of the wrong type is not equality and must not be
            // read as it: the downcast failing falls through to boxing the
            // value, which reports the chain as moved. That is the same answer
            // `same_output` gives for a failed downcast, so the two never
            // disagree about which one of them is authoritative.
            if let Some(previous) = previous
                && previous
                    .as_any()
                    .downcast_ref::<T::Output>()
                    .is_some_and(|previous| *previous == value)
            {
                return Ok(None);
            }
            Ok(Some(Erased::new(value)))
        })
    }
}

/// Object-safe view of [`Evaluate`] that erases the
/// evaluated type. `check_any` downcasts to the `T` the closure was registered
/// with; a mismatch is [`BotError::EvaluateError`], never a false result, so a
/// wiring bug cannot masquerade as a condition that simply did not fire.
pub(crate) trait EvaluateAny {
    /// Downcast `value` to this condition's `T` and evaluate it. Returns
    /// [`BotError::EvaluateError`] when the observed value is a different type,
    /// which is a chain-wiring bug rather than a domain failure.
    fn check_any(&self, value: &Erased) -> Result<bool, BotError>;
}

/// Object-safe view of [`Execute`] that erases both the
/// input and the output. `run_any` issues a fresh [`Auth`] from the grant set
/// for the action's own caps and checks the downcast input before the action
/// runs, so a type mismatch fails without a side effect.
pub(crate) trait ExecuteAny {
    /// Forwards to [`Execute::required_caps`].
    fn required_caps(&self) -> &[Cap];
    /// Forwards to [`Execute::effect_lifetime`].
    fn effect_lifetime(&self) -> crate::verb::EffectLifetime;
    /// Forwards to [`Execute::domain_id`].
    ///
    /// Erased alongside the input and output, and needed here for the same
    /// reason admission needs it: a capability denial names the domain that
    /// declared the requirement, and by the time the action is a
    /// `Box<dyn ExecuteAny>` this is the only way left to ask it which domain
    /// it is.
    fn domain_id(&self) -> &str;
    /// Issue an [`Auth`] for the action's caps, downcast `input` to the
    /// action's `Input`, run it, and box the output as `Any`. Denies with
    /// [`BotError::CapabilityDenied`] before acting; reports a type mismatch as
    /// [`BotError::TypeMismatch`] without acting.
    ///
    /// Takes the erased value rather than a bare `&dyn Any` so that every method
    /// on this boundary takes the same carrier. The witness belongs to the value
    /// — see [`Erased`] for why that is the shape this check needs — and the
    /// mismatch arm reports the producer's claim against what this action was
    /// built for, rather than reading a `type_id` off the value and comparing
    /// ground truth against an expectation.
    ///
    /// The mismatch arm is a backstop. The rendezvous in `observe_fold` compares
    /// the value's witness against the chain's before this is ever reached, so a
    /// mismatch here means the world moved behind the schedule's back. It still
    /// names both types when it fires, because a diagnostic that can say only
    /// "not the type this action wanted" leaves the reader to guess what it got.
    fn run_any<'a>(
        &'a self,
        grants: &'a GrantSet,
        input: &'a Erased,
    ) -> crate::BoxFuture<'a, Result<Box<dyn Any>, BotError>>;

    /// [`ExecuteAny::run_any`] on the clock of the flow that runs the action.
    ///
    /// The default is the plain call: an action that does not wait has nothing
    /// to measure. `TypedExec` overrides it to reach
    /// [`Execute::execute_action_on`](super::verb::Execute::execute_action_on),
    /// so a flow's `act` reaches a waiting action with the flow's own clock.
    ///
    /// Gated with its only caller, `script::act`: an ECS tick carries no scope
    /// and therefore no clock, and a method nothing there can call is not part
    /// of that build's surface.
    #[cfg(feature = "script")]
    fn run_any_on<'a>(
        &'a self,
        clock: &'a crate::clock::Clock,
        grants: &'a GrantSet,
        input: &'a Erased,
    ) -> crate::BoxFuture<'a, Result<Box<dyn Any>, BotError>> {
        let _ = clock;
        self.run_any(grants, input)
    }
}

/// Erases one [`Execute`] to [`ExecuteAny`].
///
/// The action half of a chain is stored erased, so a concrete action is wrapped
/// before it can be boxed. This is that wrapper, and it is the only one: an
/// action built by [`typed_entry`] and one built from a registry entry reach the
/// same erasure, so a correction to one cannot leave the other behind.
pub(crate) struct TypedExec<A>(A);

impl<A> TypedExec<A> {
    /// Wrap one concrete action for erasure.
    pub(crate) const fn new(action: A) -> Self {
        Self(action)
    }
}

impl<A: super::verb::Execute> TypedExec<A>
where
    A::Input: 'static,
    A::Output: 'static,
{
    /// The one erased call: downcast, prove authority, run on `clock` when the
    /// caller named one, and box the output.
    ///
    /// `None` is the call that carried no flow scope — the ECS tick and a
    /// pipeline built by hand — and it runs exactly as it always has. `Some` is
    /// the flow's `act`, whose scope names the clock its wait must spend.
    fn execute_erased<'a>(
        &'a self,
        clock: Option<&'a crate::clock::Clock>,
        grants: &'a GrantSet,
        input: &'a Erased,
    ) -> crate::BoxFuture<'a, Result<Box<dyn Any>, BotError>> {
        Box::pin(async move {
            match input.as_any().downcast_ref::<A::Input>() {
                Some(typed) => {
                    let auth: Auth = grants.issue(self.0.required_caps())?;
                    let value = match clock {
                        Some(clock) => self.0.execute_action_on(clock, (auth, typed)).await?,
                        None => self.0.execute_action((auth, typed)).await?,
                    };
                    let boxed: Box<dyn Any> = Box::new(value);
                    Ok(boxed)
                }
                None => Err(BotError::TypeMismatch {
                    site: "spec::typed_entry",
                    chain: None,
                    expected: type_name::<A::Input>(),
                    observed: input.witness.name(),
                }),
            }
        })
    }
}

impl<A: super::verb::Execute> ExecuteAny for TypedExec<A>
where
    A::Input: 'static,
    A::Output: 'static,
{
    fn required_caps(&self) -> &[Cap] {
        self.0.required_caps()
    }

    fn domain_id(&self) -> &str {
        self.0.domain_id()
    }

    fn effect_lifetime(&self) -> crate::verb::EffectLifetime {
        self.0.effect_lifetime()
    }

    fn run_any<'a>(
        &'a self,
        grants: &'a GrantSet,
        input: &'a Erased,
    ) -> crate::BoxFuture<'a, Result<Box<dyn Any>, BotError>> {
        self.execute_erased(None, grants, input)
    }

    #[cfg(feature = "script")]
    fn run_any_on<'a>(
        &'a self,
        clock: &'a crate::clock::Clock,
        grants: &'a GrantSet,
        input: &'a Erased,
    ) -> crate::BoxFuture<'a, Result<Box<dyn Any>, BotError>> {
        self.execute_erased(Some(clock), grants, input)
    }
}

// ── Builder ────────────────────────────────────────────────────────────────

// ── Serialization ──────────────────────────────────────────────────────────

/// Upper bound on a serialized spec accepted by [`BotSpec::from_json`]. A bot
/// manifest is small, so this is a defensive limit rather than a capability: it
/// keeps a hostile or runaway input from allocating without bound before schema
/// validation runs. Input exactly at the bound is still parsed; one byte over
/// is refused with [`BotError::SpecTooLarge`].
pub const MAX_SPEC_BYTES: usize = 1024 * 1024;

impl BotSpec {
    /// The spec as pretty-printed JSON; field order follows the struct
    /// declaration and enum variants serialize by name.
    pub fn to_json(&self) -> Result<String, crate::json::Error> {
        crate::json::to_string_pretty(self)
    }

    /// Parse a spec previously produced by [`BotSpec::to_json`]; a missing or
    /// unknown field is rejected rather than defaulted, an absent `version`
    /// reads as [`BotSpec::CURRENT_VERSION`], and input over
    /// [`MAX_SPEC_BYTES`] is refused before parsing.
    ///
    /// # Errors
    ///
    /// [`BotError::SpecTooLarge`] if `source` is longer than
    /// [`MAX_SPEC_BYTES`]; [`BotError::MalformedSpec`] if `source` is not
    /// schema-valid JSON; [`BotError::UnsupportedSpecVersion`] if it parses but
    /// declares a `version` this build does not implement. The malformed
    /// diagnostic is the parser's positional message with control characters
    /// escaped, because an unknown field name is attacker-chosen.
    pub fn from_json(source: &str) -> Result<Self, BotError> {
        if source.len() > MAX_SPEC_BYTES {
            let refusal = Err(BotError::SpecTooLarge {
                bytes: source.len(),
                limit: MAX_SPEC_BYTES,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "from_json: returning an error to the caller");
            return refusal;
        }
        let spec: Self =
            crate::json::from_str(source).map_err(|error| BotError::MalformedSpec {
                cause: error.to_string().escape_debug().to_string(),
            })?;
        if spec.version != Self::CURRENT_VERSION {
            let refusal = Err(BotError::UnsupportedSpecVersion {
                found: spec.version,
                supported: Self::CURRENT_VERSION,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "from_json: returning an error to the caller");
            return refusal;
        }
        Ok(spec)
    }
}
