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
//! use lgwks_bot::eval::Above;
//! use lgwks_bot::{Auth, Bot, BotError, Cap, Evaluate, Execute, GrantSet, Observe};
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
//! let bot = Bot::builder("doc")
//!     .observe(Clock)
//!     .on(|ticks: &u16| *ticks >= 3, Ring)
//!     .build(&GrantSet::empty())?;
//! # let _ = bot;
//! # Ok::<(), BotError>(())
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
use super::error::BotError;
use super::gate::GrantSet;

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
    /// The bot's unique name. Must be non-empty: both builder entry points
    /// refuse an empty name with [`BotError::IncompleteSpec`]. `from_json`
    /// accepts one because it validates shape only; the build-time check is
    /// the one that binds.
    pub name: String,
    /// Observation chains in declaration order. [`Bot::tick`] polls and fires in
    /// this order, so reordering changes which side effects run before an error.
    /// Empty is valid: a bot with no chains still serves direct
    /// [`Query`](crate::verb::Query) and [`Execute`](crate::verb::Execute) calls.
    pub chains: Vec<ChainSpec>,
}

impl BotSpec {
    /// Assemble a spec from its parts. The arguments are taken verbatim; no
    /// shape validation runs here, because validation belongs to
    /// [`BotSpec::from_json`] and to the builder, not to construction.
    #[must_use]
    pub fn new(name: impl Into<String>, chains: Vec<ChainSpec>) -> Self {
        Self {
            name: name.into(),
            chains,
        }
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
    /// The builder resolves it against a concrete [`Observe`](crate::verb::Observe)
    /// implementation; the spec itself never carries code.
    pub source: String,
    /// The target parameter for the source (e.g. `"owner/repo"`). Its meaning is
    /// defined by the source domain, not by the spec.
    pub target: String,
    /// Condition–action pairs: `[condition_id, { domain: target }]`. Evaluated
    /// in order, so the same pair placed earlier fires earlier on a tick.
    pub on: Vec<(String, ActionSpec)>,
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
}

/// A serializable action reference.
///
/// `#[non_exhaustive]` for the same reason as [`BotSpec`]; construct with
/// [`ActionSpec::new`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields)]
#[non_exhaustive]
pub struct ActionSpec {
    /// The domain identifier (e.g. `"notify::slack"`).
    pub domain: String,
    /// The target parameter (e.g. `"#deploys"`). Interpreted by `domain`.
    pub target: String,
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
    struct TypedEval<C, T> {
        inner: C,
        _marker: std::marker::PhantomData<T>,
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

    ChainEntry {
        condition: Box::new(TypedEval {
            inner: condition,
            _marker: std::marker::PhantomData::<T>,
        }),
        action: Box::new(TypedExec::new(action)),
    }
}

// ── Live bot ───────────────────────────────────────────────────────────────

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
/// [`BotError::PendingTransition`](crate::BotError::PendingTransition) has to be
/// able to name what those return; `ecs` itself stays private, because it is the
/// implementation rather than a second way to run a bot.
pub use crate::ecs::{
    AbandonReason, EcsBot as Bot, EcsBuilder as BotBuilder, EcsObserveBuilder as ObserveBuilder,
    EffectEvidence, PendingWork, RetryPolicy, TransitionHold, WorkId,
};

/// One `(condition, action)` tuple in a chain.
pub(crate) struct ChainEntry {
    /// The condition half. Type-erased because the builder accepts any
    /// `Evaluate<T>`; it downcasts the observed value back to `T` on check.
    pub(crate) condition: Box<dyn EvaluateAny>,
    /// The action half. Type-erased for the same reason; it downcasts the
    /// observed value back to the action's `Input` before running.
    pub(crate) action: Box<dyn ExecuteAny>,
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
/// schema key, and no such key exists: [`DomainRegistry`](crate::DomainRegistry)
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
///   that key goes once one exists — [`DomainRegistry`](crate::DomainRegistry)
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

/// Object-safe view of [`Observe`](crate::verb::Observe) that erases
/// `Output`. The blanket impl forwards each call to the concrete verb, so the
/// erasure costs one vtable hop and no extra allocation: `poll_any` boxes the
/// domain's own value at the type-erasure boundary, which is where it must be
/// boxed anyway. The caller's [`Auth`] is checked here, before the source is
/// touched, so an erasure bug cannot bypass the gate.
pub(crate) trait ObserveAny {
    /// Forwards to [`Observe::domain_id`](crate::verb::Observe::domain_id).
    fn domain_id(&self) -> &str;
    /// Forwards to [`Observe::required_caps`](crate::verb::Observe::required_caps).
    fn required_caps(&self) -> &[Cap];
    /// Forwards to [`Observe::fingerprint`](crate::verb::Observe::fingerprint).
    ///
    /// Erased alongside the rest of the source, and deliberately on the trait
    /// rather than reached through a downcast: this is called once per chain per
    /// tick on the hot path, and the whole point of it is that the tick can
    /// answer "has it moved?" *without* constructing the value. A method that
    /// required the concrete type to call would put the downcast back on the
    /// path it exists to shorten.
    fn fingerprint(&self) -> Option<u128>;
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

    fn fingerprint(&self) -> Option<u128> {
        super::verb::Observe::fingerprint(self)
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

/// Object-safe view of [`Evaluate`](crate::verb::Evaluate) that erases the
/// evaluated type. `check_any` downcasts to the `T` the closure was registered
/// with; a mismatch is [`BotError::EvaluateError`], never a false result, so a
/// wiring bug cannot masquerade as a condition that simply did not fire.
pub(crate) trait EvaluateAny {
    /// Downcast `value` to this condition's `T` and evaluate it. Returns
    /// [`BotError::EvaluateError`] when the observed value is a different type,
    /// which is a chain-wiring bug rather than a domain failure.
    fn check_any(&self, value: &Erased) -> Result<bool, BotError>;
}

/// Object-safe view of [`Execute`](crate::verb::Execute) that erases both the
/// input and the output. `run_any` issues a fresh [`Auth`] from the grant set
/// for the action's own caps and checks the downcast input before the action
/// runs, so a type mismatch fails without a side effect.
pub(crate) trait ExecuteAny {
    /// Forwards to [`Execute::required_caps`](crate::verb::Execute::required_caps).
    fn required_caps(&self) -> &[Cap];
    /// Forwards to [`Execute::domain_id`](crate::verb::Execute::domain_id).
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

/// Erases one [`Execute`](crate::verb::Execute) to [`ExecuteAny`].
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
    /// unknown field is rejected rather than defaulted, and input over
    /// [`MAX_SPEC_BYTES`] is refused before parsing.
    ///
    /// # Errors
    ///
    /// [`BotError::SpecTooLarge`] if `source` is longer than
    /// [`MAX_SPEC_BYTES`]; [`BotError::MalformedSpec`] if `source` is not
    /// schema-valid JSON. The malformed diagnostic is the parser's positional
    /// message with control characters escaped, because an unknown field name is
    /// attacker-chosen.
    pub fn from_json(source: &str) -> Result<Self, BotError> {
        if source.len() > MAX_SPEC_BYTES {
            return Err(BotError::SpecTooLarge {
                bytes: source.len(),
                limit: MAX_SPEC_BYTES,
            });
        }
        crate::json::from_str(source).map_err(|error| BotError::MalformedSpec {
            cause: error.to_string().escape_debug().to_string(),
        })
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::DispatchCertainty;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
    /// The suppression is the narrow, test-scoped exception `Cargo.toml`
    /// documents for the two `deny` API bans: no replacement exists for a
    /// blocking sleep on a pool thread under a thread-parking executor.
    #[expect(
        clippy::disallowed_methods,
        reason = "a test of a thread-parking executor must block a thread, and the sync executor \
                  offers no timed wait to do it with"
    )]
    fn hold_pool_thread_for(duration: std::time::Duration) {
        std::thread::sleep(duration);
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

    /// A counting action.
    #[derive(Clone)]
    struct Counting(Arc<AtomicUsize>);
    impl crate::verb::Execute for Counting {
        type Input = u32;
        type Output = ();
        fn required_caps(&self) -> &[Cap] {
            &[]
        }
        async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
            call.0.check(crate::verb::Execute::required_caps(self))?;
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn domain_id(&self) -> &str {
            "test::counting"
        }
    }

    #[test]
    fn spec_round_trips_json() -> Result<(), BotError> {
        let spec = BotSpec {
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
    fn both_builder_entry_points_apply_the_same_admission() {
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
        struct Noop;
        impl crate::verb::Execute for Noop {
            type Input = u32;
            type Output = ();
            fn required_caps(&self) -> &[Cap] {
                &[]
            }
            async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
                call.0.check(crate::verb::Execute::required_caps(self))?;
                Ok(())
            }
            fn domain_id(&self) -> &str {
                "test::noop"
            }
        }

        // No-chains entry point (`BotBuilder::build`).
        assert!(
            matches!(
                Bot::builder("").build(&GrantSet::empty()),
                Err(BotError::IncompleteSpec { field: "name" })
            ),
            "the no-chains entry point must reject an empty name"
        );
        // With-chains entry point (`ObserveBuilder::build`) rejects the same.
        assert!(
            matches!(
                Bot::builder("")
                    .observe(NeedsNet(vec![]))
                    .build(&GrantSet::empty()),
                Err(BotError::IncompleteSpec { field: "name" })
            ),
            "the with-chains entry point must reject the same empty name"
        );
        // And both admit capabilities the same way.
        let denied = Bot::builder("x")
            .observe(NeedsNet(vec![Cap::net()]))
            .on(|_: &u32| true, Noop)
            .build(&GrantSet::empty());
        assert!(
            matches!(denied, Err(BotError::CapabilityDenied { .. })),
            "a source whose cap is not in the grant set must be denied at build"
        );
    }

    #[test]
    fn empty_name_is_rejected() {
        let result = Bot::builder("").build(&GrantSet::all_shipped());
        assert!(
            result.is_err(),
            "an empty name must be rejected even when every cap is granted"
        );
    }

    #[test]
    fn capability_denied_without_grant() {
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

        struct FakeAction;
        impl crate::verb::Execute for FakeAction {
            type Input = u32;
            type Output = ();
            fn required_caps(&self) -> &[Cap] {
                &[]
            }
            async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
                call.0.check(crate::verb::Execute::required_caps(self))?;
                Ok(())
            }
            fn domain_id(&self) -> &str {
                "test::action"
            }
        }

        let result = Bot::builder("test")
            .observe(FakeSource::net())
            .on(|_: &u32| true, FakeAction)
            .build(&GrantSet::empty());
        assert!(
            result.is_err(),
            "`bot.net` must be denied when the grant set is empty"
        );
    }

    #[test]
    fn capability_granted_builds_ok() -> Result<(), BotError> {
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

        struct FakeAction;
        impl crate::verb::Execute for FakeAction {
            type Input = u32;
            type Output = ();
            fn required_caps(&self) -> &[Cap] {
                &[]
            }
            async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
                call.0.check(crate::verb::Execute::required_caps(self))?;
                Ok(())
            }
            fn domain_id(&self) -> &str {
                "test::action"
            }
        }

        let grants = GrantSet::empty().grant(Cap::net());
        let bot = Bot::builder("test")
            .observe(FakeSource::net())
            .on(|_: &u32| true, FakeAction)
            .build(&grants)?;
        assert_eq!(bot.name(), "test");
        assert_eq!(bot.source_domains().len(), 1);
        Ok(())
    }

    #[test]
    fn tick_fires_matching_actions() -> Result<(), BotError> {
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

        #[derive(Clone)]
        struct CountAction(Arc<AtomicUsize>);
        impl crate::verb::Execute for CountAction {
            type Input = u32;
            type Output = ();
            fn required_caps(&self) -> &[Cap] {
                &[]
            }
            async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
                call.0.check(crate::verb::Execute::required_caps(self))?;
                self.0.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            fn domain_id(&self) -> &str {
                "test::count_action"
            }
        }

        let counter = Arc::new(AtomicUsize::new(0));

        let mut bot = Bot::builder("ticker")
            .observe(CountSource)
            .on(|seen: &u32| *seen > 5, CountAction(Arc::clone(&counter)))
            .on(|seen: &u32| *seen > 100, CountAction(Arc::clone(&counter)))
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

        struct NetSource([Cap; 1]);
        impl Observe for NetSource {
            type Output = u32;
            fn required_caps(&self) -> &[Cap] {
                &self.0
            }
            async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
                call.0.check(crate::verb::Observe::required_caps(self))?;
                Ok(1)
            }
            fn domain_id(&self) -> &str {
                "test::net"
            }
        }

        let vacuous = GrantSet::empty().issue(&[])?;
        match lgwks_std::task::block_on(NetSource([Cap::net()]).poll((vacuous, ()))) {
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

        struct NetSource([Cap; 1]);
        impl Observe for NetSource {
            type Output = u32;
            fn required_caps(&self) -> &[Cap] {
                &self.0
            }
            async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
                call.0.check(crate::verb::Observe::required_caps(self))?;
                Ok(1)
            }
            fn domain_id(&self) -> &str {
                "test::net"
            }
        }

        let fs_only = GrantSet::empty().grant(Cap::fs()).issue(&[Cap::fs()])?;
        match lgwks_std::task::block_on(NetSource([Cap::net()]).poll((fs_only, ()))) {
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

        struct NetSource([Cap; 1]);
        impl Observe for NetSource {
            type Output = u32;
            fn required_caps(&self) -> &[Cap] {
                &self.0
            }
            async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
                call.0.check(crate::verb::Observe::required_caps(self))?;
                Ok(7)
            }
            fn domain_id(&self) -> &str {
                "test::net"
            }
        }

        let auth = GrantSet::empty().grant(Cap::net()).issue(&[Cap::net()])?;
        assert_eq!(
            lgwks_std::task::block_on(NetSource([Cap::net()]).poll((auth, ())))?,
            7
        );
        Ok(())
    }

    #[test]
    fn tick_drives_sources_in_one_step() -> Result<(), BotError> {
        let counter = Arc::new(AtomicUsize::new(0));
        let mut bot = Bot::builder("direct")
            .observe(Immediate)
            .on(|_: &u32| true, Counting(Arc::clone(&counter)))
            .build(&GrantSet::empty())?;
        let fired = bot.tick()?;
        assert_eq!(fired, 1);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[test]
    fn tick_polls_sources_concurrently() -> Result<(), BotError> {
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
            .on(|_: &u32| true, Counting(Arc::new(AtomicUsize::new(0))))
            .observe(source())
            .on(|_: &u32| true, Counting(Arc::new(AtomicUsize::new(0))))
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
    fn tick_waves_more_chains_than_the_in_flight_cap() -> Result<(), BotError> {
        // 40 chains exceed MAX_IN_FLIGHT_POLLS (32), so this only passes if
        // the wave loop polls every chain, not just the first wave.
        let counter = Arc::new(AtomicUsize::new(0));
        let mut builder = Bot::builder("waves")
            .observe(Immediate)
            .on(|_: &u32| true, Counting(Arc::clone(&counter)));
        for _ in 0..39 {
            builder = builder
                .observe(Immediate)
                .on(|_: &u32| true, Counting(Arc::clone(&counter)));
        }
        let mut bot = builder.build(&GrantSet::empty())?;
        assert_eq!(bot.source_domains().len(), 40);
        assert_eq!(bot.tick()?, 40);
        assert_eq!(counter.load(Ordering::SeqCst), 40);
        Ok(())
    }

    #[test]
    fn a_failing_poll_fires_nothing_and_returns_the_first_error() -> Result<(), BotError> {
        let counter = Arc::new(AtomicUsize::new(0));
        let mut bot = Bot::builder("ordered")
            .observe(Immediate)
            .on(|_: &u32| true, Counting(Arc::clone(&counter)))
            .observe(Failing)
            .on(|_: &u32| true, Counting(Arc::clone(&counter)))
            .build(&GrantSet::empty())?;
        match bot.tick() {
            Err(BotError::DomainError { domain, .. }) => assert_eq!(domain, "test::failing"),
            other => {
                return Err(failed(format!(
                    "expected the failing chain's error, got {other:?}"
                )));
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
