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
//! Identifiers are expected to be unique within a list. A duplicate is not
//! refused: lookup is in declaration order and the first entry wins. The check
//! that would make this a compile error is a `const` assertion, and it is not
//! written because `clippy::panic` is forbidden workspace-wide, which leaves no
//! way to fail a `const` evaluation from this module.

use crate::error::BotError;
use crate::spec::{ExecuteAny, ObserveAny, TypedExec};
use crate::verb::{Execute, Observe};

/// Builds one source from the `target` its spec names.
///
/// A function pointer rather than a closure: the registry is a `static`, and a
/// capturing closure cannot live in one.
pub type SourceCtor = fn(&str) -> Result<Source, BotError>;

/// Builds one action from the `target` its spec names.
///
/// A function pointer for the same reason as [`SourceCtor`].
pub type ActionCtor = fn(&str) -> Result<Action, BotError>;

/// A source, erased to the view the runner calls.
///
/// The erase happens at the registry boundary because that is the last point at
/// which the concrete type is known, and it is also where the output's
/// `PartialEq` can still be demanded — see [`Source::new`].
pub struct Source(Box<dyn ObserveAny>);

impl Source {
    /// Erase a concrete source into the handle a registry entry returns.
    ///
    /// The `PartialEq` bound is not a new requirement on a source: the ECS
    /// builder already demands it for its change filter, so a source that could
    /// reach this constructor without it could not have been put in a chain
    /// anyway. It is stated here rather than discovered at the blanket impl.
    #[must_use]
    pub fn new<O>(source: O) -> Self
    where
        O: Observe + 'static,
        O::Output: PartialEq + 'static,
    {
        Self(Box::new(source))
    }

    /// The domain identifier the source declares for itself.
    #[must_use]
    pub fn domain_id(&self) -> &str {
        self.0.domain_id()
    }
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
    #[must_use]
    pub fn source(&self, domain_id: &str) -> Option<SourceCtor> {
        find(self.sources, domain_id)
    }

    /// The constructor registered for an action identifier, if any.
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
/// Linear because the list is a handful of entries and is read at most once per
/// spec, not per tick. A map would be a second structure to keep in step with
/// the first for no gain at this size.
fn find<T: Copy>(entries: &[(&'static str, T)], domain_id: &str) -> Option<T> {
    let mut found = None;
    for &(registered, ctor) in entries {
        if registered == domain_id {
            found = Some(ctor);
            break;
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
