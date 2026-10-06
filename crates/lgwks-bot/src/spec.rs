//! `spec` owns the bot builder and serializable spec, enforcing
//! INV-BOT-SPEC-SERIALIZABLE: every `BotSpec` round-trips through JSON and
//! INV-BOT-TUPLE-WIRE: causal chains are `(condition, action)` tuples.
//!
//! # Chains are typed against their source
//!
//! [`ObserveBuilder::on`] takes a condition that reads the source's output and
//! an action that takes exactly it, so a chain that cannot work does not build:
//!
//! ```rust
//! use lgwks_bot::domain::eval::Above;
//! use lgwks_bot::{EffectLifetime, Auth, Bot, BotError, Cap, Evaluate, Execute, GrantSet, Observe};
//!
//! /// A source that reports a count.
//! struct Clock;
//! impl Observe for Clock {
//!     type Output = u16;
//!     fn required_caps(&self) -> &[Cap] { &[] }
//!     async fn poll(&self, call: (Auth, ())) -> Result<u16, BotError> {
//!         call.0.check(&[])?;
//!         Ok(3)
//!     }
//!     fn domain_id(&self) -> &str { "doc::clock" }
//! }
//!
//! /// An action whose input is a count.
//! struct Ring;
//! impl Execute for Ring {
//!     type Input = u16;
//!     type Output = ();
//!     fn required_caps(&self) -> &[Cap] { &[] }
//!     async fn execute_action(&self, call: (Auth, &u16)) -> Result<(), BotError> {
//!         call.0.check(&[])?;
//!         Ok(())
//!     }
//!     fn effect_lifetime(&self) -> EffectLifetime { EffectLifetime::Local }
//!     fn domain_id(&self) -> &str { "doc::ring" }
//! }
//!
//! // The bound `on` is stated against, independently of the builder: any
//! // triple that satisfies it is a chain this crate accepts. Instantiated
//! // with a shipped condition, so the bound is proven satisfiable by
//! // something other than the closure below.
//! fn assert_chain<S: Observe, C: Evaluate<S::Output>, A: Execute<Input = S::Output>>() {}
//! assert_chain::<Clock, Above<u16>, Ring>();
//!
//! # use lgwks_bot::broker::Broker;
//! # use lgwks_bot::effect::{EnvironmentId, FlowRevision, RunId};
//! # use lgwks_bot::journal::MemoryJournal;
//! # use lgwks_bot::spec::{EffectIdentity, EffectScope};
//! # fn scope() -> Result<EffectScope, Box<dyn std::error::Error>> {
//! #     let environment = EnvironmentId::from_hex("2122232425262728292a2b2c2d2e2f30")?;
//! #     let mut broker = Broker::new();
//! #     broker.register(environment)?;
//! #     Ok(EffectScope::new(
//! #         EffectIdentity::new(
//! #             RunId::from_hex("0102030405060708090a0b0c0d0e0f10")?,
//! #             environment,
//! #             FlowRevision::from_tagged(
//! #                 "blake3_256",
//! #                 "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
//! #             )?,
//! #         ),
//! #         broker,
//! #         Box::new(MemoryJournal::new()),
//! #     ))
//! # }
//! // Every bot is built against an effect scope: the run it is, the environment
//! // it acts on, and the journal a dispatch is written to before it leaves the
//! // process. There is no default, because a default identity is one the caller
//! // cannot recover against.
//! let bot = Bot::builder("doc")
//!     .observe(Clock)
//!     .on(|ticks: &u16| *ticks >= 3, Ring)
//!     .with_effects(scope()?)
//!     .build(&GrantSet::empty())?;
//! # let _ = bot;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! An action that takes something else is a compile error — `E0271`, `type
//! mismatch resolving <Courier as Execute>::Input == u16` — rather than a
//! downcast miss discovered on a tick:
//!
//! ```compile_fail,E0271
//! # use lgwks_bot::{Auth, Bot, BotError, Cap, Execute, GrantSet, Observe};
//! #
//! # struct Clock;
//! # impl Observe for Clock {
//! #     type Output = u16;
//! #     fn required_caps(&self) -> &[Cap] { &[] }
//! #     async fn poll(&self, call: (Auth, ())) -> Result<u16, BotError> {
//! #         call.0.check(&[])?;
//! #         Ok(3)
//! #     }
//! #     fn domain_id(&self) -> &str { "doc::clock" }
//! # }
//! #
//! /// An action that takes a `u32` — not what `Clock` produces.
//! struct Courier;
//! impl Execute for Courier {
//!     type Input = u32;
//!     type Output = ();
//!     fn required_caps(&self) -> &[Cap] { &[] }
//!     async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
//!         call.0.check(&[])?;
//!         Ok(())
//!     }
//!     fn domain_id(&self) -> &str { "doc::courier" }
//! }
//!
//! let bot = Bot::builder("doc")
//!     .observe(Clock)
//!     .on(|ticks: &u16| *ticks >= 3, Courier)
//!     .build(&GrantSet::empty())?;
//! ```
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
//! input and produces a caller-visible one — [`Evaluate::check`] returns a
//! `bool`, a pure predicate with no derived output. **No stage may introduce a
//! caller-selected type parameter disconnected from its input.** A stage that
//! transforms the value must carry the transformation as an associated type
//! (`Transform<I>::Output`), so the next stage's input follows from the previous
//! stage's output instead of being chosen by the caller. A free parameter at
//! this seam is how the chain became unprovable the first time.
//!
//! [`ObserveBuilder::on`]: crate::spec::ObserveBuilder::on

use lgwks_std::json::{Deserialize, Serialize};
use std::any::{Any, TypeId, type_name};

use super::cap::{Auth, Cap};
use super::error::{BotError, Escaped};
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
    /// # Example
    ///
    /// ```rust
    /// use lgwks_bot::BotSpec;
    ///
    /// let spec = BotSpec::from_json(r#"{"name":"larry","chains":[]}"#)?;
    /// assert_eq!(spec.name(), "larry");
    /// assert_eq!(spec.version(), BotSpec::CURRENT_VERSION);
    /// assert!(spec.chains().is_empty());
    /// # Ok::<(), lgwks_bot::BotError>(())
    /// ```
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
    /// # Example
    ///
    /// ```rust
    /// use lgwks_bot::spec::{ActionSpec, ChainSpec};
    ///
    /// let chain = ChainSpec::new(
    ///     "gh::pr_status",
    ///     "owner/repo",
    ///     vec![("changed".to_owned(), ActionSpec::new("notify::slack", "#deploys"))],
    /// );
    /// assert_eq!(chain.source(), "gh::pr_status");
    /// assert_eq!(chain.target(), "owner/repo");
    /// assert_eq!(chain.on()[0].1.domain(), "notify::slack");
    /// # Ok::<(), lgwks_bot::BotError>(())
    /// ```
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
    /// # Example
    ///
    /// ```rust
    /// use lgwks_bot::spec::ActionSpec;
    ///
    /// let action = ActionSpec::new("notify::slack", "#deploys");
    /// assert_eq!(action.domain(), "notify::slack");
    /// assert_eq!(action.target(), "#deploys");
    /// # Ok::<(), lgwks_bot::BotError>(())
    /// ```
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
        Box::pin(async move {
            match input.as_any().downcast_ref::<A::Input>() {
                Some(typed) => {
                    let auth: Auth = grants.issue(self.0.required_caps())?;
                    let value = self.0.execute_action((auth, typed)).await?;
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

// ── Admission: the complete NeedSet ─────────────────────────────────────────

/// Why a spec could not be materialized into a runnable bot.
///
/// Two refusals with two different repairs. [`Admission::Refused`] is a defect
/// in the document or the registry itself — a version this build does not
/// implement, an empty chain list, a registry that declares one identifier
/// twice — and no amount of authority closes it. [`Admission::Needs`] is a
/// complete, attributable report of everything a *well-formed* document asked
/// for that this host cannot supply yet; the repair is a decision the caller
/// makes (grant a capability, install an adapter), never something this crate
/// performs on its own.
///
/// Neither arm is a partial build. A refused spec produces no bot, so no source
/// is polled and no action runs: the materializer is all-or-nothing by
/// construction, because the object a caller would otherwise hold is one whose
/// chains are half-real.
#[non_exhaustive]
#[derive(Debug)]
pub enum Admission {
    /// The document or the registry is malformed, independently of authority.
    Refused(BotError),
    /// Every presently knowable unmet need, in declaration order.
    Needs(NeedSet),
}

impl std::fmt::Display for Admission {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::Refused(ref cause) => write!(formatter, "{cause}"),
            Self::Needs(ref needs) => write!(formatter, "admission refused: {needs}"),
        }
    }
}

impl std::error::Error for Admission {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Refused(ref cause) => Some(cause),
            Self::Needs(_) => None,
        }
    }
}

impl From<BotError> for Admission {
    fn from(cause: BotError) -> Self {
        Self::Refused(cause)
    }
}

/// Every presently knowable unmet need of one materialization, in one value.
///
/// The DX-08 report. A materializer that returned at the first unmet need made
/// admission a loop in which each refusal revealed one more requirement, so a
/// spec short of four things cost four round trips to learn and the repair could
/// not be written until the last of them. This is the whole difference computed
/// in one pass, each entry attributed to the chain index — and, where it belongs
/// to a specific action, the action index — that needs it.
///
/// **A report, never a grant.** [`NeedSet::proposed_grants`] derives the grant
/// set that would close the capability needs, and it is a *proposal*: the
/// trusted host accepts, narrows, or refuses it, and this crate never folds it
/// into authority on its own. No `grant_all` fallback exists, and a spec cannot
/// grant itself anything by naming a domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeedSet {
    /// The needs, in declaration order.
    needs: Vec<Need>,
}

impl NeedSet {
    /// Assemble a need set from its entries, taken verbatim and in order.
    #[must_use]
    pub fn new(needs: Vec<Need>) -> Self {
        Self { needs }
    }

    /// The needs, in declaration order.
    #[must_use]
    pub fn needs(&self) -> &[Need] {
        &self.needs
    }

    /// Every need, in declaration order.
    pub fn iter(&self) -> std::slice::Iter<'_, Need> {
        self.needs.iter()
    }

    /// How many needs the report carries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.needs.len()
    }

    /// Whether the report is empty. An empty set never reaches a caller — the
    /// materializer returns a bot instead — so this exists for a caller holding
    /// one, not as a "success" signal.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.needs.is_empty()
    }

    /// The grant set that would close the capability needs, as a **proposal**.
    ///
    /// A repair proposal, not authority: the returned set is data the caller may
    /// inspect, narrow, or refuse, and no code path in this crate admits a bot
    /// through it. Only the [`GrantSet`] the caller passes to the materializer
    /// supplies authority, so a spec still cannot choose what a bot reaches.
    /// Needs that are not about capabilities (an unknown domain, a rejected
    /// target) contribute nothing here, because no grant repairs them.
    #[must_use]
    pub fn proposed_grants(&self) -> GrantSet {
        let mut grants = GrantSet::empty();
        for need in &self.needs {
            if let Need::MissingCapability { ref capability, .. } = *need {
                grants = grants.grant(capability.clone());
            }
        }
        grants
    }
}

impl std::fmt::Display for NeedSet {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} unmet need(s)", self.needs.len())?;
        for (index, need) in self.needs.iter().enumerate() {
            formatter.write_str(if index == 0 { ": " } else { ", " })?;
            write!(formatter, "{need}")?;
        }
        Ok(())
    }
}

/// One unmet need, attributed to the chain (and action) that raised it.
///
/// The attribution is the repair: "an unknown domain" is a fact about the
/// document, while "chain 3, action 1 names unknown domain `notify::absent`" is
/// a location in it. Indices are declaration positions, zero-based, matching the
/// order [`BotSpec::chains`] and [`ChainSpec::on`] are walked.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Need {
    /// A chain's source identifier is not in the registry.
    UnknownSource {
        /// The chain's index in the spec.
        chain: usize,
        /// The identifier the spec named.
        domain: String,
    },
    /// An action's domain identifier is not in the registry.
    UnknownAction {
        /// The owning chain's index in the spec.
        chain: usize,
        /// The action's index within the chain's `on` list.
        action: usize,
        /// The identifier the spec named.
        domain: String,
    },
    /// A source constructor refused the target the spec gave it.
    SourceTargetRejected {
        /// The chain's index in the spec.
        chain: usize,
        /// The source identifier the spec named.
        domain: String,
        /// The constructor's own refusal, escaped.
        cause: String,
    },
    /// An action constructor refused the target the spec gave it.
    ActionTargetRejected {
        /// The owning chain's index in the spec.
        chain: usize,
        /// The action's index within the chain's `on` list.
        action: usize,
        /// The action identifier the spec named.
        domain: String,
        /// The constructor's own refusal, escaped.
        cause: String,
    },
    /// A chain names a condition outside the supported wire vocabulary.
    UnknownCondition {
        /// The owning chain's index in the spec.
        chain: usize,
        /// The condition's index within the chain's `on` list.
        action: usize,
        /// The condition identifier the spec spelled.
        condition: String,
    },
    /// A domain requires a capability the caller's grant set does not carry.
    MissingCapability {
        /// The owning chain's index in the spec.
        chain: usize,
        /// The action's index when the requirement is an action's, or `None`
        /// when it is the source's.
        action: Option<usize>,
        /// The domain that declared the requirement.
        domain: String,
        /// The capability it requires and was not granted.
        capability: Cap,
    },
}

impl Need {
    /// The chain index this need is attributed to.
    #[must_use]
    pub fn chain(&self) -> usize {
        match *self {
            Self::UnknownSource { chain, .. }
            | Self::UnknownAction { chain, .. }
            | Self::SourceTargetRejected { chain, .. }
            | Self::ActionTargetRejected { chain, .. }
            | Self::UnknownCondition { chain, .. }
            | Self::MissingCapability { chain, .. } => chain,
        }
    }

    /// The action index within the chain, when the need belongs to one action
    /// rather than the source or the chain as a whole.
    #[must_use]
    pub fn action(&self) -> Option<usize> {
        match *self {
            Self::UnknownAction { action, .. }
            | Self::ActionTargetRejected { action, .. }
            | Self::UnknownCondition { action, .. } => Some(action),
            Self::MissingCapability { action, .. } => action,
            Self::UnknownSource { .. } | Self::SourceTargetRejected { .. } => None,
        }
    }
}

impl std::fmt::Display for Need {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::UnknownSource { chain, ref domain } => write!(
                formatter,
                "chain {chain}: unknown source domain {}",
                Escaped(domain)
            ),
            Self::UnknownAction {
                chain,
                action,
                ref domain,
            } => write!(
                formatter,
                "chain {chain} action {action}: unknown action domain {}",
                Escaped(domain)
            ),
            Self::SourceTargetRejected {
                chain,
                ref domain,
                ref cause,
            } => write!(
                formatter,
                "chain {chain}: source {} refused its target: {}",
                Escaped(domain),
                Escaped(cause)
            ),
            Self::ActionTargetRejected {
                chain,
                action,
                ref domain,
                ref cause,
            } => write!(
                formatter,
                "chain {chain} action {action}: action {} refused its target: {}",
                Escaped(domain),
                Escaped(cause)
            ),
            Self::UnknownCondition {
                chain,
                action,
                ref condition,
            } => write!(
                formatter,
                "chain {chain} action {action}: unknown condition {}",
                Escaped(condition)
            ),
            Self::MissingCapability {
                chain,
                action,
                ref domain,
                ref capability,
            } => match action {
                Some(action) => write!(
                    formatter,
                    "chain {chain} action {action}: {} requires ungranted capability {}",
                    Escaped(domain),
                    capability
                ),
                None => write!(
                    formatter,
                    "chain {chain}: {} requires ungranted capability {}",
                    Escaped(domain),
                    capability
                ),
            },
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::DispatchCertainty;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// What a test returns when it can fail in more than one error domain.
    ///
    /// Building the scope a bot needs crosses `IdError` and `BrokerError` as
    /// well as `BotError`, and none of the three converts into another, so a
    /// test that hands over a scope reports through `Box<dyn Error>` and names
    /// the failure with `?` rather than flattening it into a variant it is not.
    type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

    /// The effect scope a test's bot runs under: the bot's own, so the spec
    /// tests and the ECS tests build one scope rather than two that could drift.
    ///
    /// A bot has no dispatch path without one, so every builder in this module
    /// hands over a scope; the ones whose subject is spec validation still do,
    /// because a refusal produced by "no scope" would be a refusal about the
    /// wrong thing and the assertion around it would read as the validation it
    /// claims to test.
    fn test_effects() -> TestResult<EffectScope> {
        crate::ecs::tests::test_effects()
    }

    /// The failure a test reports when its precondition did not hold. Tests
    /// return `Result` and propagate with `?`, so a mismatch is reported as a
    /// named assertion failure with the cause attached rather than as a bare
    /// unwind, and the cause names the invariant that was violated, not merely
    /// that something was.
    fn failed(cause: impl Into<String>) -> BotError {
        BotError::DomainError {
            domain: "spec::tests".into(),
            certainty: DispatchCertainty::NotDelivered,
            cause: cause.into(),
        }
    }

    /// Block the calling blocking-pool thread for `duration`.
    ///
    /// `rt::time::sleep` cannot be used here: `Bot::tick` drives
    /// `lgwks_std::task::block_on`, whose executor has no timer driver, and the
    /// caller is a `spawn_blocking` closure that has nothing to await on. The
    /// property under test is wall-clock overlap between two polls, and holding
    /// a real thread is the only way to express it on this executor.
    ///
    /// `park_timeout` and not `rt::time::sleep`: the async form has no timer
    /// driver on `lgwks_std::task::block_on`, and that absence is the thing under
    /// test. It is also the substitution the codebook names for a banned
    /// `std::thread::sleep`, so nothing here needs a suppression.
    ///
    /// What `sleep` promised and `park_timeout` does not is that it does not
    /// return early: the API allows a spurious wake-up, and a pool thread can
    /// carry an unpark token its executor left behind, which ends the next park
    /// at once. So the park repeats until the deadline has actually passed, and
    /// the thread is held for the whole of `duration` either way.
    fn hold_pool_thread_for(duration: std::time::Duration) {
        let started = std::time::Instant::now();
        while let Some(left) = duration.checked_sub(started.elapsed()) {
            if left.is_zero() {
                break;
            }
            std::thread::park_timeout(left);
        }
    }

    /// An observer that needs `bot.net` and resolves with a value the caller
    /// chooses.
    ///
    /// One double for both boundary tests. The callee refuses it under an empty
    /// proof and admits it under an issued one; the only thing that differs
    /// between the two runs is the value read back, which is a field here rather
    /// than a second declaration that could differ in the cap as well.
    struct NetSource {
        /// The caps this source requires.
        caps: Vec<Cap>,
        /// What `poll` resolves with.
        value: u32,
    }
    impl NetSource {
        /// A `bot.net` source resolving with `value`.
        fn net(value: u32) -> Self {
            Self {
                caps: vec![Cap::net()],
                value,
            }
        }
    }
    impl crate::verb::Observe for NetSource {
        type Output = u32;
        fn required_caps(&self) -> &[Cap] {
            &self.caps
        }
        async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
            call.0.check(crate::verb::Observe::required_caps(self))?;
            Ok(self.value)
        }
        fn domain_id(&self) -> &str {
            "test::net_source"
        }
    }

    /// An observer that resolves with `42` and needs `bot.net`.
    ///
    /// Declared once for both capability tests: two copies are two chances to
    /// change one arm and leave the other asserting the old thing, and the cap it
    /// requires *is* what those tests are about.
    struct FakeSource(Vec<Cap>);
    impl FakeSource {
        fn net() -> Self {
            Self(vec![Cap::net()])
        }
    }
    impl crate::verb::Observe for FakeSource {
        type Output = u32;
        fn required_caps(&self) -> &[Cap] {
            &self.0
        }
        async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
            call.0.check(crate::verb::Observe::required_caps(self))?;
            Ok(42)
        }
        fn domain_id(&self) -> &str {
            "test::source"
        }
    }

    /// An observer that resolves immediately with `1`.
    struct Immediate;
    impl crate::verb::Observe for Immediate {
        type Output = u32;
        fn required_caps(&self) -> &[Cap] {
            &[]
        }
        async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
            call.0.check(crate::verb::Observe::required_caps(self))?;
            Ok(1)
        }
        fn domain_id(&self) -> &str {
            "test::immediate"
        }
    }

    /// An observer whose poll always fails at the domain boundary.
    struct Failing;
    impl crate::verb::Observe for Failing {
        type Output = u32;
        fn required_caps(&self) -> &[Cap] {
            &[]
        }
        async fn poll(&self, _call: (Auth, ())) -> Result<u32, BotError> {
            Err(BotError::DomainError {
                domain: "test::failing".into(),
                certainty: DispatchCertainty::NotDelivered,
                cause: "boom".into(),
            })
        }
        fn domain_id(&self) -> &str {
            "test::failing"
        }
    }

    /// An observer that records how many polls overlap. Its body crosses a
    /// `spawn_blocking` thread, which is the only way two polls can run at the
    /// same wall-clock time on a one-thread executor.
    struct PeakSource {
        in_flight: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
    }
    impl crate::verb::Observe for PeakSource {
        type Output = u32;
        fn required_caps(&self) -> &[Cap] {
            &[]
        }
        async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
            call.0.check(crate::verb::Observe::required_caps(self))?;
            let in_flight = Arc::clone(&self.in_flight);
            let peak = Arc::clone(&self.peak);
            lgwks_std::task::spawn_blocking(move || {
                // Bound: the test drives at most two chains against this source,
                // so the previous count is 0 or 1 and the increment cannot reach
                // `usize::MAX`.
                let now = in_flight.fetch_add(1, Ordering::SeqCst).saturating_add(1);
                peak.fetch_max(now, Ordering::SeqCst);
                hold_pool_thread_for(std::time::Duration::from_millis(40));
                in_flight.fetch_sub(1, Ordering::SeqCst);
            })
            .await;
            Ok(1)
        }
        fn domain_id(&self) -> &str {
            "test::peak"
        }
    }

    /// An action that proves the caps, and counts how often it ran when it was
    /// given a counter to count into.
    ///
    /// One implementation for both jobs. The counting tests read the counter and
    /// the build-shape tests pass [`Action::new`]; two doubles differed only in
    /// whether they owned a counter, and that is a field rather than a type — a
    /// second impl is a second place for the two to disagree about what an action
    /// requires or how long its effect lives.
    #[derive(Clone)]
    struct Action {
        /// Where each invocation is recorded, when the test cares.
        counter: Option<Arc<AtomicUsize>>,
    }
    impl Action {
        /// An action that records nothing.
        fn new() -> Self {
            Self { counter: None }
        }

        /// An action that records each invocation into `counter`.
        fn counting(counter: Arc<AtomicUsize>) -> Self {
            Self {
                counter: Some(counter),
            }
        }
    }
    impl crate::verb::Execute for Action {
        type Input = u32;
        type Output = ();
        fn required_caps(&self) -> &[Cap] {
            &[]
        }

        fn effect_lifetime(&self) -> crate::verb::EffectLifetime {
            crate::verb::EffectLifetime::Local
        }
        async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
            call.0.check(crate::verb::Execute::required_caps(self))?;
            if let Some(ref counter) = self.counter {
                counter.fetch_add(1, Ordering::SeqCst);
            }
            Ok(())
        }
        fn domain_id(&self) -> &str {
            "test::action"
        }
    }

    #[test]
    fn spec_round_trips_json() -> Result<(), BotError> {
        let spec = BotSpec {
            version: BotSpec::CURRENT_VERSION,
            name: "larry".into(),
            chains: vec![ChainSpec {
                source: "gh::pr_status".into(),
                target: "owner/repo".into(),
                on: vec![(
                    "checks_changed".into(),
                    ActionSpec {
                        domain: "notify::slack".into(),
                        target: "#deploys".into(),
                    },
                )],
            }],
        };
        let json = spec
            .to_json()
            .map_err(|error| failed(format!("a well-formed spec must serialize: {error}")))?;
        let back = BotSpec::from_json(&json)?;
        assert_eq!(back.name, "larry");
        assert_eq!(back.chains.len(), 1);
        assert_eq!(back.chains[0].source, "gh::pr_status");
        assert_eq!(back.chains[0].on.len(), 1);
        assert_eq!(back.chains[0].on[0].0, "checks_changed");
        Ok(())
    }

    #[test]
    fn unknown_fields_are_rejected_at_every_level() {
        // deny_unknown_fields is what makes the from_json doc's "unknown field
        // is rejected" true; without it serde would silently ignore all three.
        let top = r#"{"name":"x","chains":[],"extra":1}"#;
        let chain = r#"{"name":"x","chains":[{"source":"a","target":"b","on":[],"extra":1}]}"#;
        let action = r#"{"name":"x","chains":[{"source":"a","target":"b","on":[["c",{"domain":"d","target":"e","extra":1}]]}]}"#;
        assert!(
            BotSpec::from_json(top).is_err(),
            "an unknown top-level field must be rejected, not ignored"
        );
        assert!(
            BotSpec::from_json(chain).is_err(),
            "an unknown ChainSpec field must be rejected, not ignored"
        );
        assert!(
            BotSpec::from_json(action).is_err(),
            "an unknown ActionSpec field must be rejected, not ignored"
        );
    }

    #[test]
    fn missing_required_fields_are_rejected() {
        assert!(
            BotSpec::from_json(r#"{"chains":[]}"#).is_err(),
            "a spec with no name must be rejected, not defaulted"
        );
    }

    #[test]
    fn spec_size_bound_is_checked_just_above_the_limit() {
        // Limit-adjacent partition for the defensive bound: exactly at the
        // bound the input is not refused for size (it is merely malformed);
        // one byte over is refused as SpecTooLarge and never parsed.
        let at = "x".repeat(MAX_SPEC_BYTES);
        assert!(matches!(
            BotSpec::from_json(&at),
            Err(BotError::MalformedSpec { .. })
        ));
        let over = "x".repeat(MAX_SPEC_BYTES + 1);
        assert!(matches!(
            BotSpec::from_json(&over),
            Err(BotError::SpecTooLarge { bytes, limit })
                if bytes == MAX_SPEC_BYTES + 1 && limit == MAX_SPEC_BYTES
        ));
    }

    #[test]
    fn malformed_spec_diagnostic_escapes_control_characters() -> Result<(), BotError> {
        // An unknown field name is attacker-chosen; a newline in it must not
        // survive into the diagnostic as a log-forging byte.
        let Err(error) = BotSpec::from_json("{\"name\":\"x\",\"chains\":[],\"a\\nb\":1}") else {
            return Err(failed(
                "a field name containing a raw newline must be rejected as malformed",
            ));
        };
        let cause = match error {
            BotError::MalformedSpec { cause } => cause,
            other => return Err(failed(format!("expected MalformedSpec, got {other:?}"))),
        };
        assert!(
            !cause.contains('\n') && !cause.contains('\r'),
            "cause must not carry raw control bytes: {cause:?}"
        );
        Ok(())
    }

    #[test]
    fn both_builder_entry_points_apply_the_same_admission() -> TestResult<()> {
        // `assemble` is shared, so the no-chains path and the with-chains path
        // must agree on both the empty-name rejection and the capability check.
        struct NeedsNet(Vec<Cap>);
        impl crate::verb::Observe for NeedsNet {
            type Output = u32;
            fn required_caps(&self) -> &[Cap] {
                &self.0
            }
            async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
                call.0.check(crate::verb::Observe::required_caps(self))?;
                Ok(0)
            }
            fn domain_id(&self) -> &str {
                "test::needs_net"
            }
        }
        // No-chains entry point (`BotBuilder::build`).
        assert!(
            matches!(
                Bot::builder("")
                    .with_effects(test_effects()?)
                    .build(&GrantSet::empty()),
                Err(BotError::IncompleteSpec {
                    field: "name",
                    cause: _,
                })
            ),
            "the no-chains entry point must reject an empty name"
        );
        // With-chains entry point (`ObserveBuilder::build`) rejects the same.
        assert!(
            matches!(
                Bot::builder("")
                    .observe(NeedsNet(vec![]))
                    .with_effects(test_effects()?)
                    .build(&GrantSet::empty()),
                Err(BotError::IncompleteSpec {
                    field: "name",
                    cause: _,
                })
            ),
            "the with-chains entry point must reject the same empty name"
        );
        // And both admit capabilities the same way.
        let denied = Bot::builder("x")
            .observe(NeedsNet(vec![Cap::net()]))
            .on(|_: &u32| true, Action::new())
            .with_effects(test_effects()?)
            .build(&GrantSet::empty());
        assert!(
            matches!(denied, Err(BotError::CapabilityDenied { .. })),
            "a source whose cap is not in the grant set must be denied at build"
        );
        Ok(())
    }

    #[test]
    fn empty_name_is_rejected() -> TestResult<()> {
        let result = Bot::builder("")
            .with_effects(test_effects()?)
            .build(&GrantSet::all_shipped());
        assert!(
            result.is_err(),
            "an empty name must be rejected even when every cap is granted"
        );
        Ok(())
    }

    #[test]
    fn capability_denied_without_grant() -> TestResult<()> {
        let result = Bot::builder("test")
            .observe(FakeSource::net())
            .on(|_: &u32| true, Action::new())
            .with_effects(test_effects()?)
            .build(&GrantSet::empty());
        assert!(
            result.is_err(),
            "`bot.net` must be denied when the grant set is empty"
        );
        Ok(())
    }

    #[test]
    fn capability_granted_builds_ok() -> TestResult<()> {
        let grants = GrantSet::empty().grant(Cap::net());
        let bot = Bot::builder("test")
            .observe(FakeSource::net())
            .on(|_: &u32| true, Action::new())
            .with_effects(test_effects()?)
            .build(&grants)?;
        assert_eq!(bot.name(), "test");
        assert_eq!(bot.source_domains().len(), 1);
        Ok(())
    }

    #[test]
    fn tick_fires_matching_actions() -> TestResult<()> {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountSource;
        impl crate::verb::Observe for CountSource {
            type Output = u32;
            fn required_caps(&self) -> &[Cap] {
                &[]
            }
            async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
                call.0.check(crate::verb::Observe::required_caps(self))?;
                Ok(10)
            }
            fn domain_id(&self) -> &str {
                "test::count"
            }
        }

        let counter = Arc::new(AtomicUsize::new(0));

        let mut bot = Bot::builder("ticker")
            .observe(CountSource)
            .on(
                |seen: &u32| *seen > 5,
                Action::counting(Arc::clone(&counter)),
            )
            .on(
                |seen: &u32| *seen > 100,
                Action::counting(Arc::clone(&counter)),
            )
            .with_effects(test_effects()?)
            .build(&GrantSet::empty())?;

        let fired = bot.tick()?;
        assert_eq!(fired, 1);
        assert_eq!(counter.load(Ordering::Relaxed), 1);
        Ok(())
    }

    #[test]
    fn issue_denies_what_was_never_granted() -> Result<(), BotError> {
        let grants = GrantSet::empty();
        match grants.issue(&[Cap::net()]) {
            Err(BotError::CapabilityDenied { deficit }) => {
                assert_eq!(deficit.first().required(), &Cap::net());
                Ok(())
            }
            other => Err(failed(format!("expected denial, got {other:?}"))),
        }
    }

    #[test]
    fn call_with_empty_proof_is_denied_at_the_callee() -> Result<(), BotError> {
        use crate::verb::Observe;

        let vacuous = GrantSet::empty().issue(&[])?;
        match lgwks_std::task::block_on(NetSource::net(1).poll((vacuous, ()))) {
            Err(BotError::CapabilityDenied { deficit }) => {
                assert_eq!(deficit.first().required(), &Cap::net());
                Ok(())
            }
            other => Err(failed(format!(
                "a proof covering nothing must be denied by a capped callee, got {other:?}"
            ))),
        }
    }

    #[test]
    fn wrong_scope_proof_is_denied_confused_deputy() -> Result<(), BotError> {
        use crate::verb::Observe;

        let fs_only = GrantSet::empty().grant(Cap::fs()).issue(&[Cap::fs()])?;
        match lgwks_std::task::block_on(NetSource::net(1).poll((fs_only, ()))) {
            Err(BotError::CapabilityDenied { deficit }) => {
                assert_eq!(deficit.first().required(), &Cap::net());
                Ok(())
            }
            other => Err(failed(format!(
                "a proof scoped to `bot.fs` must not authorize `bot.net`, got {other:?}"
            ))),
        }
    }

    #[test]
    fn issued_proof_authorizes_the_call() -> Result<(), BotError> {
        use crate::verb::Observe;

        let auth = GrantSet::empty().grant(Cap::net()).issue(&[Cap::net()])?;
        assert_eq!(
            lgwks_std::task::block_on(NetSource::net(7).poll((auth, ())))?,
            7
        );
        Ok(())
    }

    #[test]
    fn tick_drives_sources_in_one_step() -> TestResult<()> {
        let counter = Arc::new(AtomicUsize::new(0));
        let mut bot = Bot::builder("direct")
            .observe(Immediate)
            .on(|_: &u32| true, Action::counting(Arc::clone(&counter)))
            .with_effects(test_effects()?)
            .build(&GrantSet::empty())?;
        let fired = bot.tick()?;
        assert_eq!(fired, 1);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[test]
    fn tick_polls_sources_concurrently() -> TestResult<()> {
        // Both polls block on a dedicated thread, so the peak in-flight count
        // can only reach 2 if tick drives the two sources at the same time.
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let source = || PeakSource {
            in_flight: Arc::clone(&in_flight),
            peak: Arc::clone(&peak),
        };
        let mut bot = Bot::builder("concurrent")
            .observe(source())
            .on(
                |_: &u32| true,
                Action::counting(Arc::new(AtomicUsize::new(0))),
            )
            .observe(source())
            .on(
                |_: &u32| true,
                Action::counting(Arc::new(AtomicUsize::new(0))),
            )
            .with_effects(test_effects()?)
            .build(&GrantSet::empty())?;
        assert_eq!(bot.tick()?, 2);
        let observed = peak.load(Ordering::SeqCst);
        assert!(
            observed >= 2,
            "sources overlapped only {observed} at a time"
        );
        Ok(())
    }

    #[test]
    fn tick_waves_more_chains_than_the_in_flight_cap() -> TestResult<()> {
        // 40 chains exceed MAX_IN_FLIGHT_POLLS (32), so this only passes if
        // the wave loop polls every chain, not just the first wave.
        let counter = Arc::new(AtomicUsize::new(0));
        let mut builder = Bot::builder("waves")
            .observe(Immediate)
            .on(|_: &u32| true, Action::counting(Arc::clone(&counter)));
        for _ in 0..39 {
            builder = builder
                .observe(Immediate)
                .on(|_: &u32| true, Action::counting(Arc::clone(&counter)));
        }
        let mut bot = builder
            .with_effects(test_effects()?)
            .build(&GrantSet::empty())?;
        assert_eq!(bot.source_domains().len(), 40);
        assert_eq!(bot.tick()?, 40);
        assert_eq!(counter.load(Ordering::SeqCst), 40);
        Ok(())
    }

    #[test]
    fn a_failing_poll_fires_nothing_and_returns_the_first_error() -> TestResult<()> {
        let counter = Arc::new(AtomicUsize::new(0));
        let mut bot = Bot::builder("ordered")
            .observe(Immediate)
            .on(|_: &u32| true, Action::counting(Arc::clone(&counter)))
            .observe(Failing)
            .on(|_: &u32| true, Action::counting(Arc::clone(&counter)))
            .with_effects(test_effects()?)
            .build(&GrantSet::empty())?;
        match bot.tick() {
            Err(BotError::DomainError { domain, .. }) => assert_eq!(domain, "test::failing"),
            other => {
                return Err(
                    failed(format!("expected the failing chain's error, got {other:?}")).into(),
                );
            }
        }
        // All-or-nothing per tick: the observe system polls every source before
        // any effect runs, so a tick that errors commits nothing. The earlier
        // chain does not fire. That is stronger than the previous contract,
        // where chains declared before the failure had already acted.
        assert_eq!(counter.load(Ordering::SeqCst), 0);
        Ok(())
    }
}
