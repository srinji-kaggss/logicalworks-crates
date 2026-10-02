//! The `domain_id -> constructor` registry: what a spec's strings resolve to.
//!
//! A [`BotSpec`](crate::BotSpec) carries identifiers, not code. A chain's
//! `source` and an action's `domain` are strings such as `"github::pr_status"`,
//! and the `target` beside each is a parameter whose meaning the domain itself
//! defines. Something has to turn those strings back into a running
//! [`Observe`](crate::verb::Observe) or [`Execute`](crate::verb::Execute), and
//! that is this module.
//!
//! # One list, in one place
//!
//! A registry is declared once, with [`domains!`](crate::domains), and that is
//! the only entry point. The list is data rather than registration: it is a
//! `static`, built at compile time, with no constructor to call and no global to
//! mutate, so two components cannot race to register a domain and the set a
//! binary can run is readable from its source rather than from its behaviour.
//! [`DomainRegistry`] carries the worked example.
//!
//! # What the registry does not decide
//!
//! A registry answers *which constructor* an identifier names. It never decides
//! what a bot is permitted to reach: authority still comes from the
//! [`GrantSet`](crate::GrantSet) the caller holds, and every erased verb checks
//! its own caps at the call site. A spec therefore cannot grant itself anything
//! by naming a domain, which is what makes it safe to accept a spec from wire
//! data at all.
//!
//! # Uniqueness
//!
//! Identifiers are expected to be unique within a list, and
//! [`DomainRegistry::validate`] refuses a list that declares one twice, naming
//! the identifier, the role and both positions. Every construction path checks
//! that first — [`DomainRegistry::build_source`], [`DomainRegistry::build_action`]
//! and the spec materializer — so a duplicate refuses the whole registry rather
//! than making dispatch depend on declaration order.
//!
//! The public lookups [`DomainRegistry::source`] and [`DomainRegistry::action`]
//! refuse an ambiguous identifier exactly as they refuse an absent one: neither
//! ever resolves to "whichever constructor was declared first", so a caller that
//! skips [`DomainRegistry::validate`] still cannot reach an ambiguous
//! constructor. One identifier used once per role is one domain with two roles,
//! not a duplicate.

use std::any::Any;

use crate::cap::Cap;
use crate::ecs::{AdmittedInput, identify_output, same_output};
use crate::effect::InputIdentity;
use crate::error::BotError;
use crate::spec::{EvaluateAny, ExecuteAny, ObserveAny, TypedEval, TypedExec, Witness};
use crate::verb::{Evaluate, Execute, Observe};

/// Builds one source from the `target` its spec names.
///
/// A function pointer rather than a closure: the registry is a `static`, and a
/// capturing closure cannot live in one.
pub type SourceCtor = fn(&str) -> Result<Source, BotError>;

/// Builds one action from the `target` its spec names.
///
/// A function pointer for the same reason as [`SourceCtor`].
pub type ActionCtor = fn(&str) -> Result<Action, BotError>;

/// A source, erased to the view the runner calls, plus the metadata a chain
/// needs and the type a condition must be built against.
///
/// The erase happens at the registry boundary because that is the last point at
/// which the concrete type is known, and it is also where the output's
/// `PartialEq` can still be demanded — see [`Source::new`]. The metadata the
/// chain needs (`same`, `identify`, `witness`) is captured in the same breath,
/// because it is a function of `O::Output` and this constructor is the last
/// place that type is a type parameter.
///
/// # Why the type key travels with the source
///
/// A spec names a condition as text, and a condition is generic over the value
/// it evaluates ([`Changed<T>`](crate::domain::eval::Changed) compares two
/// `T`s). The document cannot carry `T`, and a `TypeId` is process-local, so the
/// only durable statement of the type is the source that produces it: this
/// handle answers [`Source::condition`] for its *own* output type, so a
/// condition identifier resolves against the type it will actually see rather
/// than against a guess. See [`InputIdentity::SCHEMA_ID`] for the durable key
/// the change filter binds, and [`Source::new`] for the bounds this needs.
///
/// [`InputIdentity::SCHEMA_ID`]: crate::effect::InputIdentity::SCHEMA_ID
pub struct Source {
    /// The observer, erased to the view the runner calls.
    inner: Box<dyn ObserveAny>,
    /// Equality for this source's output, captured from `O::Output: PartialEq`
    /// where the type was still a parameter.
    same: fn(&dyn Any, &dyn Any) -> bool,
    /// The admitted-input identity of this source's output.
    identify: fn(&dyn Any) -> AdmittedInput,
    /// What type this source produces, taken here where it is a parameter.
    witness: Witness,
    /// Builds a condition for this source's own output type from a wire
    /// identifier, so a spec's condition resolves against the type it will see.
    condition: fn(&str) -> Result<Condition, BotError>,
}

impl Source {
    /// Erase a concrete source into the handle a registry entry returns.
    ///
    /// The bounds are what any chain already needs: the ECS builder demands
    /// `PartialEq + InputIdentity` for its change filter and its admitted-input
    /// identity, and `Clone` is what [`Changed`](crate::domain::eval::Changed)
    /// keeps the previous value with. A source built here answers the
    /// order-free wire conditions, `changed` and `always`, for any output type,
    /// a struct included. A source whose output is ordered and parseable uses
    /// [`Source::ordered`] to answer the threshold conditions as well.
    #[must_use]
    pub fn new<O>(source: O) -> Self
    where
        O: Observe + 'static,
        O::Output: Clone + PartialEq + InputIdentity + 'static,
    {
        Self::erase(source, make_condition::<O>)
    }

    /// Erase a source whose output is ordered, so a spec may also name
    /// `threshold::above(<n>)` and `threshold::below(<n>)` against it.
    ///
    /// `PartialOrd` is what [`Above`](crate::domain::eval::Above) and
    /// [`Below`](crate::domain::eval::Below) compare with, and `FromStr` is how
    /// the threshold's argument is read from wire text as the source's own
    /// output type. Kept apart from [`Source::new`] so those bounds bind only
    /// the sources that offer thresholds, rather than every source a registry
    /// can hold.
    #[must_use]
    pub fn ordered<O>(source: O) -> Self
    where
        O: Observe + 'static,
        O::Output: Clone + PartialEq + PartialOrd + std::str::FromStr + InputIdentity + 'static,
    {
        Self::erase(source, make_ordered_condition::<O>)
    }

    /// Capture the chain metadata where `O::Output` is still a type
    /// parameter, with the condition vocabulary the caller chose.
    fn erase<O>(source: O, condition: fn(&str) -> Result<Condition, BotError>) -> Self
    where
        O: Observe + 'static,
        O::Output: Clone + PartialEq + InputIdentity + 'static,
    {
        Self {
            inner: Box::new(source),
            same: same_output::<O>,
            identify: identify_output::<O>,
            witness: Witness::of::<O::Output>(),
            condition,
        }
    }

    /// The domain identifier the source declares for itself.
    #[must_use]
    pub fn domain_id(&self) -> &str {
        self.inner.domain_id()
    }

    /// The capabilities this source requires, for admission.
    #[must_use]
    pub fn required_caps(&self) -> &[Cap] {
        self.inner.required_caps()
    }

    /// Build the condition a spec names, for this source's output type.
    ///
    /// The vocabulary is closed and versioned with the type it evaluates:
    /// `changed` and `always` for every source, plus `threshold::above(<n>)`
    /// and `threshold::below(<n>)` for one built with [`Source::ordered`]. An
    /// identifier outside it is
    /// [`BotError::UnknownCondition`] — never a silently always-true condition,
    /// because an inert gate is how an effect a person expected to guard fires
    /// anyway.
    ///
    /// # Errors
    ///
    /// [`BotError::UnknownCondition`] when `condition_id` is not in the
    /// vocabulary, or when a threshold's argument does not parse as this
    /// source's output type.
    pub fn condition(&self, condition_id: &str) -> Result<Condition, BotError> {
        (self.condition)(condition_id)
    }

    /// Split the handle into the erased observer and the chain metadata.
    ///
    /// The one seam the ECS chain assembler uses. Kept crate-internal because
    /// the pieces are the substrate's own types, not a second public surface.
    pub(crate) fn into_parts(self) -> SourceParts {
        (self.inner, self.same, self.identify, self.witness)
    }
}

/// The erased pieces of a [`Source`]: the observer, then the `same`,
/// `identify` and `witness` metadata a chain needs.
///
/// One name for the tuple, so [`Source::into_parts`] and the chain assembler
/// agree on the order without either spelling out four types.
pub(crate) type SourceParts = (
    Box<dyn ObserveAny>,
    fn(&dyn Any, &dyn Any) -> bool,
    fn(&dyn Any) -> AdmittedInput,
    Witness,
);

/// Build the order-free wire condition `condition_id` names, for source `O`.
///
/// Monomorphized on `O`, so `O::Output` is the concrete type the condition
/// evaluates and no schema table has to be matched by hand.
fn make_condition<O>(condition_id: &str) -> Result<Condition, BotError>
where
    O: Observe + 'static,
    O::Output: Clone + PartialEq + 'static,
{
    let identifier = condition_id.trim();
    match identifier {
        "changed" => Ok(Condition::new::<O::Output, _>(
            crate::domain::eval::Changed::new(),
        )),
        "always" => Ok(Condition::new::<O::Output, _>(|_: &O::Output| true)),
        _ => Err(BotError::UnknownCondition {
            condition: identifier.to_owned(),
        }),
    }
}

/// Build the wire condition `condition_id` names for an ordered source `O`:
/// the two thresholds, then the order-free vocabulary of [`make_condition`].
fn make_ordered_condition<O>(condition_id: &str) -> Result<Condition, BotError>
where
    O: Observe + 'static,
    O::Output: Clone + PartialEq + PartialOrd + std::str::FromStr + 'static,
{
    let identifier = condition_id.trim();
    if let Some(argument) = parenthesized(identifier, "threshold::above") {
        let bound = parse_bound::<O::Output>(argument, identifier)?;
        return Ok(Condition::new::<O::Output, _>(
            crate::domain::eval::Above::new(bound),
        ));
    }
    if let Some(argument) = parenthesized(identifier, "threshold::below") {
        let bound = parse_bound::<O::Output>(argument, identifier)?;
        return Ok(Condition::new::<O::Output, _>(
            crate::domain::eval::Below::new(bound),
        ));
    }
    make_condition::<O>(identifier)
}

/// The argument inside `prefix(...)`, or `None` when `identifier` is not that.
fn parenthesized<'a>(identifier: &'a str, prefix: &str) -> Option<&'a str> {
    identifier
        .strip_prefix(prefix)?
        .strip_prefix('(')?
        .strip_suffix(')')
}

/// Parse a threshold argument as `T`, reporting the whole identifier on failure.
fn parse_bound<T: std::str::FromStr>(argument: &str, identifier: &str) -> Result<T, BotError> {
    argument
        .trim()
        .parse::<T>()
        .map_err(|_| BotError::UnknownCondition {
            condition: identifier.to_owned(),
        })
}

impl std::fmt::Debug for Source {
    /// Names the domain, not the source.
    ///
    /// The concrete type is erased and has no rendering of its own, so two
    /// sources of the same domain print alike. That is the honest answer rather
    /// than a gap: the erased value's contents are not part of this handle's
    /// contract, and a caller that needs them wants the domain's own accessor.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("Source")
            .field(&self.domain_id())
            .finish()
    }
}

/// An action, erased to the view the runner calls.
pub struct Action(Box<dyn ExecuteAny>);

impl Action {
    /// Erase a concrete action into the handle a registry entry returns.
    #[must_use]
    pub fn new<A>(action: A) -> Self
    where
        A: Execute + 'static,
        A::Input: 'static,
        A::Output: 'static,
    {
        Self(Box::new(TypedExec::new(action)))
    }

    /// The domain identifier the action declares for itself.
    #[must_use]
    pub fn domain_id(&self) -> &str {
        self.0.domain_id()
    }

    /// The capabilities this action requires, for admission.
    #[must_use]
    pub fn required_caps(&self) -> &[Cap] {
        self.0.required_caps()
    }

    /// Take the erased action, for the chain assembler.
    pub(crate) fn into_execute_any(self) -> Box<dyn ExecuteAny> {
        self.0
    }
}

impl std::fmt::Debug for Action {
    /// Names the domain, not the action, for the reason [`Source`]'s does.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("Action")
            .field(&self.domain_id())
            .finish()
    }
}

/// A condition, erased to the view a chain's walk calls.
///
/// A spec names a condition as text and [`Source::condition`] builds one for its
/// own output type; a native chain names one directly through
/// [`ObserveBuilder::on`](crate::spec::ObserveBuilder::on). Both arrive here, so
/// the two paths share one erasure and one type-mismatch report rather than each
/// carrying its own.
///
/// This is the *only* way a condition is built for a materialized chain. The
/// constructor is generic over the value it evaluates — as the verb traits are —
/// and the downcast to that value is checked when the condition runs, so a
/// condition built for one type and handed a value of another reports
/// [`BotError::EvaluateError`](crate::BotError::EvaluateError) rather than
/// answering a false.
pub struct Condition(Box<dyn EvaluateAny>);

impl Condition {
    /// Erase a concrete condition, typed against `T`.
    #[must_use]
    pub fn new<T, C>(condition: C) -> Self
    where
        T: 'static,
        C: Evaluate<T> + 'static,
    {
        Self(Box::new(TypedEval {
            inner: condition,
            _marker: std::marker::PhantomData::<T>,
        }))
    }

    /// Take the erased condition, for the chain assembler.
    pub(crate) fn into_evaluate_any(self) -> Box<dyn EvaluateAny> {
        self.0
    }
}

impl std::fmt::Debug for Condition {
    /// Prints `Condition`, because an erased condition carries no identity of
    /// its own: what it evaluates is the source's output type, which the chain,
    /// not this handle, is what names.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Condition")
    }
}

/// Every domain a spec may name, and what each identifier builds.
///
/// Built by [`domains!`](crate::domains), which is the only entry point:
///
/// ```rust
/// use lgwks_bot::{Auth, BotError, Cap, Observe, Source, domains};
///
/// /// A source that reports an open pull request count.
/// struct GithubPrStatus;
///
/// impl GithubPrStatus {
///     /// Build one from the `target` its spec names.
///     fn from_target(target: &str) -> Result<Source, BotError> {
///         let _ = target;
///         Ok(Source::new(Self))
///     }
/// }
///
/// impl Observe for GithubPrStatus {
///     type Output = u16;
///     fn required_caps(&self) -> &[Cap] { &[] }
///     async fn poll(&self, call: (Auth, ())) -> Result<u16, BotError> {
///         call.0.check(&[])?;
///         Ok(0)
///     }
///     fn domain_id(&self) -> &str { "github::pr_status" }
/// }
///
/// domains! {
///     /// The domains this bot can run.
///     pub DOMAINS {
///         observe {
///             "github::pr_status" => GithubPrStatus::from_target,
///         }
///         execute {}
///     }
/// }
///
/// assert!(DOMAINS.source("github::pr_status").is_some());
/// # Ok::<(), BotError>(())
/// ```
///
/// The two lists are separate because a source and an action are constructed
/// from the same `&str` but produce different erased traits, and one identifier
/// may legitimately name both — a domain that observes a repository and also
/// acts on one is one domain with two roles, not a name collision.
pub struct DomainRegistry {
    /// Registered sources, in declaration order.
    sources: &'static [(&'static str, SourceCtor)],
    /// Registered actions, in declaration order.
    actions: &'static [(&'static str, ActionCtor)],
}

impl DomainRegistry {
    /// Assemble a registry from the two declaration lists.
    #[must_use]
    pub const fn new(
        sources: &'static [(&'static str, SourceCtor)],
        actions: &'static [(&'static str, ActionCtor)],
    ) -> Self {
        Self { sources, actions }
    }

    /// A registry with nothing registered.
    ///
    /// Useful as the identity for a composition, and as the value a test builds
    /// when it wants to assert that an identifier is *not* known.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            sources: &[],
            actions: &[],
        }
    }

    /// The constructor registered for a source identifier, if any.
    ///
    /// `None` when the identifier is unknown *or* declared twice in the source
    /// list. An ambiguous identifier is deliberately not resolved to the first
    /// declaration: doing so would make which constructor a spec reaches depend
    /// on declaration order. [`Self::validate`] distinguishes an ambiguity from
    /// an absence by name.
    #[must_use]
    pub fn source(&self, domain_id: &str) -> Option<SourceCtor> {
        find(self.sources, domain_id)
    }

    /// The constructor registered for an action identifier, if any.
    ///
    /// `None` when the identifier is unknown *or* declared twice in the action
    /// list, for the reason [`Self::source`] gives.
    #[must_use]
    pub fn action(&self, domain_id: &str) -> Option<ActionCtor> {
        find(self.actions, domain_id)
    }

    /// Refuse a registry that declares one identifier twice within a role.
    ///
    /// Admission is fallible rather than a `const` assertion because the
    /// workspace forbids panics, and it lives on the registry rather than the
    /// macro so a hand-assembled [`DomainRegistry::new`] call is checked the
    /// same way as a `domains!` declaration. The two roles are checked
    /// independently: one identifier appearing once as a source and once as an
    /// action is one domain with two roles, not a duplicate. When a list does
    /// hold a pair, the first one is named with both positions; a caller
    /// repairs it and revalidates.
    ///
    /// Every build goes through this check, so a broken registry refuses all
    /// construction — the alternative, letting the first declaration win,
    /// would make dispatch depend on declaration order.
    pub fn validate(&self) -> Result<(), BotError> {
        if let Some((first, second)) = first_duplicate(self.sources) {
            return Err(BotError::DuplicateDomain {
                domain: self.sources[first].0.to_owned(),
                role: "source",
                first,
                second,
            });
        }
        if let Some((first, second)) = first_duplicate(self.actions) {
            return Err(BotError::DuplicateDomain {
                domain: self.actions[first].0.to_owned(),
                role: "action",
                first,
                second,
            });
        }
        Ok(())
    }

    /// Build the source a spec names, or refuse with the identifier.
    ///
    /// The refusal is typed rather than an `Option` because reaching it means
    /// the spec named a domain this binary cannot run, and a caller that has to
    /// report that should not also have to invent the wording.
    pub fn build_source(&self, domain_id: &str, target: &str) -> Result<Source, BotError> {
        self.validate()?;
        match self.source(domain_id) {
            Some(ctor) => ctor(target),
            None => Err(BotError::UnregisteredDomain {
                domain: domain_id.to_owned(),
            }),
        }
    }

    /// Build the action a spec names, or refuse with the identifier.
    pub fn build_action(&self, domain_id: &str, target: &str) -> Result<Action, BotError> {
        self.validate()?;
        match self.action(domain_id) {
            Some(ctor) => ctor(target),
            None => Err(BotError::UnregisteredDomain {
                domain: domain_id.to_owned(),
            }),
        }
    }

    /// The source identifiers this registry knows, in declaration order.
    pub fn source_ids(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.sources.iter().map(|&(domain_id, _)| domain_id)
    }

    /// The action identifiers this registry knows, in declaration order.
    pub fn action_ids(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.actions.iter().map(|&(domain_id, _)| domain_id)
    }
}

impl std::fmt::Debug for DomainRegistry {
    /// Lists the identifiers rather than the function pointers, which have no
    /// useful rendering and would make two registries with the same domains
    /// print differently.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DomainRegistry")
            .field("sources", &self.source_ids().collect::<Vec<&str>>())
            .field("actions", &self.action_ids().collect::<Vec<&str>>())
            .finish()
    }
}

/// Find an identifier in a declaration list, in declaration order.
///
/// `None` when the identifier is absent *or* declared more than once. The
/// second case is the point: an ambiguous identifier must not resolve to
/// whichever constructor the author happened to list first, because that makes
/// dispatch depend on declaration order (issue #122). [`DomainRegistry::validate`]
/// names the duplicate pair before any build, and refusing it here means a
/// caller that reaches for `source`/`action` directly still cannot construct
/// from an ambiguous list.
///
/// Linear because the list is a handful of entries and is read at most once per
/// spec, not per tick. A map would be a second structure to keep in step with
/// the first for no gain at this size.
fn find<T: Copy>(entries: &[(&'static str, T)], domain_id: &str) -> Option<T> {
    let mut found: Option<T> = None;
    for &(registered, ctor) in entries {
        if registered == domain_id {
            if found.is_some() {
                return None;
            }
            found = Some(ctor);
        }
    }
    found
}

/// The positions of the first identifier declared twice in one list, or `None`
/// when the list is distinct. Quadratic in a list of a handful of entries —
/// the check runs once per build, not per tick — and written with comparisons
/// rather than index arithmetic, which the workspace lints forbid.
fn first_duplicate<T: PartialEq>(entries: &[(&'static str, T)]) -> Option<(usize, usize)> {
    for (first, &(registered, _)) in entries.iter().enumerate() {
        for (second, &(other, _)) in entries.iter().enumerate() {
            if second > first && other == registered {
                return Some((first, second));
            }
        }
    }
    None
}

/// Declare the domains a binary can run, in one place.
///
/// The one entry point for the `domain_id -> constructor` mapping. A worked
/// example is in [`DomainRegistry`](crate::DomainRegistry)'s documentation.
///
/// The two halves are separate because a source and an action are built into
/// different erased traits. An empty half is written `{}` and is not an error: a
/// bot that only observes is a bot.
///
/// A constructor is any path with the shape of [`SourceCtor`] or [`ActionCtor`],
/// so it is usually an inherent function on the adapter that parses the spec's
/// `target` into the adapter's own fields.
#[macro_export]
macro_rules! domains {
    (
        $(#[$meta:meta])*
        $vis:vis $name:ident {
            observe { $( $source_id:literal => $source_ctor:path ),* $(,)? }
            execute { $( $action_id:literal => $action_ctor:path ),* $(,)? }
        }
    ) => {
        $(#[$meta])*
        $vis static $name: $crate::DomainRegistry = $crate::DomainRegistry::new(
            &[ $( ($source_id, $source_ctor as $crate::SourceCtor) ),* ],
            &[ $( ($action_id, $action_ctor as $crate::ActionCtor) ),* ],
        );
    };
}
