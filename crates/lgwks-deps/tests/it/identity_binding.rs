//! Cargo-identity-exact approval matching and the explicit, collision-checked
//! alias (issue #158 A2).
//!
//! `-`/`_` is Cargo's *collision* fold for published names, not package
//! identity. An approval must be byte-exact against the Cargo-authored package
//! name; a compatibility alias is authored, singular, and collision-checked; and
//! a genuine `package =` rename stays a local spelling of the upstream identity.

use std::error::Error;

use lgwks_deps::contract::{Contract, ContractError};
use lgwks_deps::metadata::{self, DirectEdge};
use lgwks_deps::{Refusal, audit_direct};

type TestResult = Result<(), Box<dyn Error>>;

/// A registry source the approvals name.
const REGISTRY: &str = "registry+https://github.com/rust-lang/crates.io-index";

/// One `app` → package edge for the given upstream package, with an optional
/// local rename key, through the public parser.
fn edge(
    package: &str,
    rename: Option<&str>,
    source: Option<&str>,
) -> Result<Vec<DirectEdge>, Box<dyn Error>> {
    let source_json = source.map_or("null".to_owned(), |value| format!("\"{value}\""));
    let rename_json = rename.map_or("null".to_owned(), |value| format!("\"{value}\""));
    let document = format!(
        r#"{{"packages":[{{"id":"app","name":"app","repository":null,"manifest_path":"/repo/Cargo.toml","dependencies":[{{"name":"{package}","source":{source_json},"req":"1.0","kind":null,"rename":{rename_json},"optional":false,"uses_default_features":true,"features":[],"target":null,"path":null}}]}},{{"id":"lic-{package}","name":"{package}","license":"MIT OR Apache-2.0","manifest_path":"/dep/Cargo.toml","dependencies":[]}}],"workspace_members":["app"]}}"#
    );
    Ok(metadata::parse(&document)?)
}

/// One `app` → package filesystem edge with the given local path authority.
fn path_edge(package: &str, path: &str) -> Result<Vec<DirectEdge>, Box<dyn Error>> {
    let document = format!(
        r#"{{"packages":[{{"id":"app","name":"app","repository":null,"manifest_path":"/repo/Cargo.toml","dependencies":[{{"name":"{package}","source":null,"req":"1.0","kind":null,"rename":null,"optional":false,"uses_default_features":true,"features":[],"target":null,"path":"{path}"}}]}},{{"id":"lic-{package}","name":"{package}","license":"MIT OR Apache-2.0","manifest_path":"/dep/Cargo.toml","dependencies":[]}}],"workspace_members":["app"]}}"#
    );
    Ok(metadata::parse(&document)?)
}

/// One `[[approved]]` block approving `krate` for `app`, with an optional alias.
fn entry_text(krate: &str, aliases: Option<&str>) -> String {
    let alias_line = aliases.map_or(String::new(), |value| format!("aliases = \"{value}\"\n"));
    format!(
        concat!(
            "[[approved]]\n",
            "crate = \"{krate}\"\n",
            "tier = \"boundary\"\n",
            "version = \"1.0\"\n",
            "owner = \"app\"\n",
            "capability = \"engine.core\"\n",
            "license = \"MIT OR Apache-2.0\"\n",
            "source = \"registry\"\n",
            "{aliases}",
            "allowed_consumers = \"app\"\n",
            "allowed_kinds = \"normal\"\n",
            "reason = \"The engine supplies a capability the standard library cannot express.\"\n",
            "approved_by = \"reviewer\"\n",
            "approved_on = \"2026-09-30\"\n",
            "review = \"tests/identity_binding.rs\"\n",
        ),
        krate = krate,
        aliases = alias_line,
    )
}

/// A single-entry register for `krate`.
fn register(krate: &str, aliases: Option<&str>) -> Result<Contract, Box<dyn Error>> {
    Ok(Contract::parse(&format!(
        "[policy]\nschema = 2\nenforce = true\naccepted_licenses = \"MIT, Apache-2.0\"\n\n{}",
        entry_text(krate, aliases)
    ))?)
}

#[test]
fn a_fold_alike_package_cannot_borrow_an_approval() -> TestResult {
    let register = register("engine-core", None)?;
    let approved = edge("engine-core", None, Some(REGISTRY))?;
    assert!(audit_direct(&approved, &register).is_empty());

    let impostor = edge("engine_core", None, Some(REGISTRY))?;
    assert!(
        matches!(
            audit_direct(&impostor, &register).first(),
            Some(Refusal::UnregisteredEdge { krate, .. }) if krate == "engine_core"
        ),
        "a fold-alike package is a different package and must not be admitted"
    );
    Ok(())
}

#[test]
fn an_explicit_alias_admits_the_alias_spelling_only() -> TestResult {
    let register = register("engine-core", Some("engine_core"))?;
    assert!(
        audit_direct(&edge("engine-core", None, Some(REGISTRY))?, &register).is_empty(),
        "the canonical identity stays admitted"
    );
    assert!(
        audit_direct(&edge("engine_core", None, Some(REGISTRY))?, &register).is_empty(),
        "the authored alias admits the alias spelling"
    );
    // The alias is one spelling, not a licence for a third fold-alike.
    assert!(matches!(
        audit_direct(&edge("engine.core", None, Some(REGISTRY))?, &register).first(),
        Some(Refusal::UnregisteredEdge { .. })
    ));
    Ok(())
}

/// Two fold-alike names are two approvals, and neither admits the other.
#[test]
fn two_fold_alike_packages_are_two_authorities() -> TestResult {
    let text = format!(
        "[policy]\nschema = 2\nenforce = true\naccepted_licenses = \"MIT, Apache-2.0\"\n\n{}{}",
        entry_text("engine-core", None),
        entry_text("engine_core", None)
    );
    let register = Contract::parse(&text)?;
    let mut both = edge("engine-core", None, Some(REGISTRY))?;
    both.extend(edge("engine_core", None, Some(REGISTRY))?);
    assert!(
        audit_direct(&both, &register).is_empty(),
        "each fold-alike edge is admitted by exactly its own approval"
    );
    Ok(())
}

/// A rename is a local spelling: the audit reads the upstream identity, and an
/// approval for the local alias alone does not admit the edge.
#[test]
fn a_rename_is_preserved_but_is_not_the_upstream_identity() -> TestResult {
    let upstream = register("engine-core", None)?;
    let renamed = edge("engine-core", Some("local_engine"), Some(REGISTRY))?;
    assert_eq!(renamed[0].rename(), Some("local_engine"));
    assert!(audit_direct(&renamed, &upstream).is_empty());

    let alias_only = register("local_engine", None)?;
    assert!(
        matches!(
            audit_direct(&renamed, &alias_only).first(),
            Some(Refusal::UnregisteredEdge { krate, .. }) if krate == "engine-core"
        ),
        "the local rename must not become the approved identity"
    );
    Ok(())
}

/// Alternate git and path origins keep the same exact package identity rule.
#[test]
fn git_and_path_edges_use_the_same_identity_rule() -> TestResult {
    let register = register("engine-core", None)?;
    let git = edge(
        "engine-core",
        None,
        Some("git+https://example.invalid/engine"),
    )?;
    assert!(
        !matches!(
            audit_direct(&git, &register).first(),
            Some(Refusal::UnregisteredEdge { .. })
        ),
        "the git edge names the approved package identity"
    );
    let path = path_edge("engine-core", "../engine")?;
    assert!(!matches!(
        audit_direct(&path, &register).first(),
        Some(Refusal::UnregisteredEdge { .. })
    ));
    Ok(())
}

#[test]
fn an_alias_that_collides_with_another_package_is_refused_at_load() {
    let text = format!(
        "[policy]\nschema = 2\nenforce = true\naccepted_licenses = \"MIT, Apache-2.0\"\n\n{}{}",
        entry_text("engine-core", Some("engine_core")),
        entry_text("engine_core", None)
    );
    assert!(
        matches!(
            Contract::parse(&text),
            Err(ContractError::AliasCollision { ref alias, .. }) if alias == "engine_core"
        ),
        "an alias equal to another approval's name is a collision"
    );
}
