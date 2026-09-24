//! The `domain_id -> constructor` registry, and what an unregistered name does.
//!
//! A spec carries identifiers rather than code, so the registry is the whole of
//! the translation between the two. What these tests hold is that the list a
//! binary declares is the list it runs — no more and no fewer — and that a
//! document naming something outside it is refused with the name it used.

use lgwks_bot::{
    Action, Auth, BotError, Cap, DomainRegistry, EffectLifetime, Execute, Observe, Source, domains,
};

/// A source whose identity is fixed, so a test can tell it was built.
struct Repository;

impl Repository {
    /// Build one from the `target` its spec names.
    ///
    /// The target is validated rather than stored. A source that kept it could
    /// not be asked for it once erased, so the only observable way for a test to
    /// prove the target arrived is a constructor that acts on it.
    fn from_target(target: &str) -> Result<Source, BotError> {
        if target.is_empty() {
            return Err(BotError::IncompleteSpec { field: "target" });
        }
        Ok(Source::new(Self))
    }
}

impl Observe for Repository {
    type Output = u16;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u16, BotError> {
        call.0.check(&[])?;
        Ok(0)
    }

    fn domain_id(&self) -> &str {
        "github::repository"
    }
}

/// An action whose identity is fixed, for the same reason as `Repository`.
struct SlackNotify;

impl SlackNotify {
    /// Build one from the `target` its spec names.
    fn from_target(target: &str) -> Result<Action, BotError> {
        if target.is_empty() {
            return Err(BotError::IncompleteSpec { field: "target" });
        }
        Ok(Action::new(Self))
    }
}

impl Execute for SlackNotify {
    type Input = u16;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &u16)) -> Result<(), BotError> {
        call.0.check(&[])?;
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "notify::slack"
    }
}

domains! {
    /// The domains these tests run.
    pub TEST_DOMAINS {
        observe {
            "github::repository" => Repository::from_target,
        }
        execute {
            "notify::slack" => SlackNotify::from_target,
        }
    }
}

/// A registry that declares nothing, to pin the empty case.
const NOTHING: DomainRegistry = DomainRegistry::empty();

#[test]
fn a_declared_source_is_found_by_its_identifier() {
    assert!(
        TEST_DOMAINS.source("github::repository").is_some(),
        "the declared source was not found"
    );
    assert!(
        TEST_DOMAINS.source("github::absent").is_none(),
        "an undeclared source was found"
    );
}

#[test]
fn the_two_lists_do_not_bleed_into_each_other() {
    // A source identifier is not an action identifier. Lookup is per role, so a
    // spec cannot reach an action by naming a source, which is the property
    // that keeps a read from becoming a write.
    assert!(
        TEST_DOMAINS.action("github::repository").is_none(),
        "a source identifier resolved as an action"
    );
    assert!(
        TEST_DOMAINS.source("notify::slack").is_none(),
        "an action identifier resolved as a source"
    );
}

#[test]
fn building_a_source_reaches_the_domain() -> Result<(), Box<dyn std::error::Error>> {
    let source = TEST_DOMAINS.build_source("github::repository", "owner/repo")?;
    assert_eq!(
        source.domain_id(),
        "github::repository",
        "the built source was not the declared one"
    );
    // And the target reached it, which is the only evidence available once the
    // source is erased.
    let refused = TEST_DOMAINS.build_source("github::repository", "");
    assert!(
        matches!(refused, Err(BotError::IncompleteSpec { field }) if field == "target"),
        "the constructor's own refusal did not surface: {refused:?}"
    );
    Ok(())
}

#[test]
fn the_target_reaches_the_constructor() -> Result<(), Box<dyn std::error::Error>> {
    // The constructor is the only place a spec's `target` is interpreted, so a
    // constructor that refuses an empty one must refuse it through the registry
    // too. Reaching the constructor is what this asserts.
    let refused = TEST_DOMAINS.build_action("notify::slack", "");
    assert!(
        matches!(refused, Err(BotError::IncompleteSpec { field }) if field == "target"),
        "the constructor's own refusal did not surface: {refused:?}"
    );
    assert!(
        TEST_DOMAINS
            .build_action("notify::slack", "#deploys")
            .is_ok(),
        "a well-formed target was refused"
    );
    Ok(())
}

#[test]
fn an_unregistered_identifier_is_refused_by_name() {
    let source = TEST_DOMAINS.build_source("github::absent", "owner/repo");
    assert_eq!(
        source.err().map(|error| error.to_string()),
        Some("unregistered domain: github::absent".to_owned()),
        "the refusal did not name the identifier"
    );

    let action = TEST_DOMAINS.build_action("notify::absent", "#deploys");
    assert!(
        matches!(action, Err(BotError::UnregisteredDomain { .. })),
        "an undeclared action was built: {action:?}"
    );
}

#[test]
fn an_unregistered_refusal_escapes_a_hostile_identifier() {
    // The identifier comes from the document. A newline in it would forge a
    // line in whatever log carries the refusal.
    let hostile = "github::a\nforged";
    let rendered = NOTHING
        .build_source(hostile, "")
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    assert!(
        !rendered.contains('\n'),
        "the refusal carried a raw newline: {rendered:?}"
    );
}

#[test]
fn an_empty_registry_refuses_everything() {
    assert!(
        NOTHING.source("github::repository").is_none(),
        "an empty registry knew a source"
    );
    assert_eq!(
        NOTHING.source_ids().count(),
        0,
        "an empty registry listed sources"
    );
    assert_eq!(
        NOTHING.action_ids().count(),
        0,
        "an empty registry listed actions"
    );
}

#[test]
fn the_registry_lists_what_it_declares_in_order() {
    let ids: Vec<&str> = TEST_DOMAINS.source_ids().collect();
    assert_eq!(
        ids,
        vec!["github::repository"],
        "the source list is not what was declared"
    );
    let ids: Vec<&str> = TEST_DOMAINS.action_ids().collect();
    assert_eq!(
        ids,
        vec!["notify::slack"],
        "the action list is not what was declared"
    );
}

/// A second source type registered under [`Repository`]'s identifier, so a
/// duplicate declaration cannot be mistaken for one constructor listed twice:
/// the two would behave differently if either were silently picked.
struct RepositoryV2;

impl RepositoryV2 {
    /// Build one from the `target` its spec names, delegating to keep the
    /// fixture small; the type, not the behavior, is what the duplicate tests
    /// observe.
    fn from_target(target: &str) -> Result<Source, BotError> {
        Repository::from_target(target)
    }
}

impl Observe for RepositoryV2 {
    type Output = u32;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        call.0.check(&[])?;
        Ok(0)
    }

    fn domain_id(&self) -> &str {
        "github::repository"
    }
}

domains! {
    /// Two source types share one identifier. If the registry admits this,
    /// dispatch depends on declaration order and no spec can name the
    /// ambiguity.
    pub DUPLICATE_SOURCES {
        observe {
            "github::repository" => Repository::from_target,
            "github::repository" => RepositoryV2::from_target,
        }
        execute {}
    }
}

domains! {
    /// The same duplicate pair, declared in the opposite order.
    pub REVERSED_DUPLICATES {
        observe {
            "github::repository" => RepositoryV2::from_target,
            "github::repository" => Repository::from_target,
        }
        execute {}
    }
}

domains! {
    /// Two actions share one identifier: the action half of the same defect.
    pub DUPLICATE_ACTIONS {
        observe {}
        execute {
            "notify::slack" => SlackNotify::from_target,
            "notify::slack" => SlackNotify::from_target,
        }
    }
}

domains! {
    /// One identifier used once per role: a domain that observes a repository
    /// and also acts on one is one domain with two roles, not a collision.
    pub MIXED_ROLES {
        observe {
            "shared::domain" => Repository::from_target,
        }
        execute {
            "shared::domain" => SlackNotify::from_target,
        }
    }
}

#[test]
fn a_duplicate_source_identifier_is_refused_with_both_positions() {
    let refusal = DUPLICATE_SOURCES.build_source("github::repository", "owner/repo");
    assert!(
        matches!(
            refusal,
            Err(BotError::DuplicateDomain {
                role,
                first: 0,
                second: 1,
                ..
            }) if role == "source"
        ),
        "a duplicated source id built instead of refusing: {refusal:?}"
    );
    assert_eq!(
        refusal.err().map(|error| error.to_string()),
        Some("duplicate source domain github::repository declared at positions 0 and 1".to_owned()),
        "the refusal did not name the identifier and both positions"
    );
}

#[test]
fn a_duplicate_action_identifier_is_refused_the_same_way() {
    let refusal = DUPLICATE_ACTIONS.build_action("notify::slack", "#deploys");
    assert!(
        matches!(
            refusal,
            Err(BotError::DuplicateDomain {
                role,
                first: 0,
                second: 1,
                ..
            }) if role == "action"
        ),
        "a duplicated action id built instead of refusing: {refusal:?}"
    );
}

#[test]
fn refusal_is_independent_of_declaration_order() {
    // The reversed registry must refuse identically: if the refusal depended
    // on order, reordering declarations would silently alternate behavior.
    // The rendered message carries the identifier, the role and both
    // positions, so equal renderings mean equal refusals.
    let reversed = REVERSED_DUPLICATES
        .build_source("github::repository", "owner/repo")
        .err()
        .map(|error| error.to_string());
    let declared = DUPLICATE_SOURCES
        .build_source("github::repository", "owner/repo")
        .err()
        .map(|error| error.to_string());
    assert_eq!(
        reversed, declared,
        "the refusal changed with declaration order"
    );
    assert!(
        reversed.is_some(),
        "a duplicated registry stopped refusing once reversed"
    );
}

#[test]
fn one_identifier_across_both_roles_remains_valid() {
    assert!(
        matches!(MIXED_ROLES.validate(), Ok(())),
        "a cross-role id was refused: {:?}",
        MIXED_ROLES.validate()
    );
    let source = MIXED_ROLES.build_source("shared::domain", "owner/repo");
    // The registry key and the adapter's self-declared identity are
    // independent: `Repository` is registered here under "shared::domain" and
    // still names itself "github::repository" once built. What matters is
    // that the build succeeds under a key that also names an action.
    assert!(
        matches!(source, Ok(ref built) if built.domain_id() == "github::repository"),
        "the cross-role source did not build: {source:?}"
    );
    let action = MIXED_ROLES.build_action("shared::domain", "#deploys");
    assert!(
        action.is_ok(),
        "the cross-role action did not build: {action:?}"
    );
}

#[test]
fn validate_names_the_first_duplicate_pair_and_passes_clean_lists() {
    assert!(
        matches!(NOTHING.validate(), Ok(())),
        "the empty registry did not validate: {:?}",
        NOTHING.validate()
    );
    assert!(
        matches!(TEST_DOMAINS.validate(), Ok(())),
        "a registry with distinct ids did not validate: {:?}",
        TEST_DOMAINS.validate()
    );
    // Only the first pair is reported; the count, not the pair, is what the
    // caller repairs first.
    assert!(
        matches!(
            DUPLICATE_SOURCES.validate(),
            Err(BotError::DuplicateDomain {
                first: 0,
                second: 1,
                ..
            })
        ),
        "validate did not name the duplicate pair: {:?}",
        DUPLICATE_SOURCES.validate()
    );
}
