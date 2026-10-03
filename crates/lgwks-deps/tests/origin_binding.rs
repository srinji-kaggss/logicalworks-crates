//! Origin binding, exercised through the public metadata + admission API.
//!
//! A source *class* is not an approved origin (issue #158 A1). These fixtures
//! build a real edge through `metadata::parse` and audit it with `audit_direct`
//! against a register that authors an `origin`, so the boundary a downstream
//! consumer uses is the boundary under test. Each substitution asserts the
//! refusal's exact approved and observed identities, not merely that something
//! was refused.

use std::error::Error;

use lgwks_deps::contract::Contract;
use lgwks_deps::metadata::{self, DirectEdge};
use lgwks_deps::{Refusal, audit_direct};

type TestResult = Result<(), Box<dyn Error>>;

/// One `app` → `engine` edge with the given dependency object body.
fn edges_for(dependency: &str) -> Result<Vec<DirectEdge>, Box<dyn Error>> {
    let document = format!(
        r#"{{"packages":[{{"id":"app","name":"app","repository":null,"manifest_path":"/repo/Cargo.toml","dependencies":[{dependency}]}}],"workspace_members":["app"]}}"#
    );
    Ok(metadata::parse(&document)?)
}

/// The dependency object for a registry source.
fn registry_dependency(source: &str) -> String {
    format!(
        r#"{{"name":"engine","source":"{source}","req":"1.0","kind":null,"optional":false,"path":null}}"#
    )
}

/// The dependency object for a Git source.
fn git_dependency(source: &str) -> String {
    format!(
        r#"{{"name":"engine","source":"{source}","req":"1.0","kind":null,"optional":false,"path":null}}"#
    )
}

/// The dependency object for a filesystem source.
fn path_dependency(path: &str) -> String {
    format!(
        r#"{{"name":"engine","source":null,"req":"1.0","kind":null,"optional":false,"path":"{path}"}}"#
    )
}

/// A register approving `engine` for `app` with the given class and origin.
fn register(source: &str, origin: &str) -> Result<Contract, Box<dyn Error>> {
    let text = format!(
        concat!(
            "[policy]\nenforce = true\n\n",
            "[[approved]]\n",
            "crate = \"engine\"\n",
            "tier = \"boundary\"\n",
            "version = \"1.0\"\n",
            "owner = \"app\"\n",
            "capability = \"engine.core\"\n",
            "source = \"{source}\"\n",
            "origin = \"{origin}\"\n",
            "allowed_consumers = \"app\"\n",
            "allowed_kinds = \"normal\"\n",
            "reason = \"The engine supplies a capability the standard library cannot express.\"\n",
            "approved_by = \"reviewer\"\n",
            "approved_on = \"2026-09-30\"\n",
            "review = \"tests/origin_binding.rs\"\n",
        ),
        source = source,
        origin = origin,
    );
    Ok(Contract::parse(&text)?)
}

/// The first refusal, or a test error naming the empty result.
fn first(refusals: &[Refusal]) -> Result<&Refusal, String> {
    refusals
        .first()
        .ok_or_else(|| "expected a refusal, got an empty admission".to_owned())
}

#[test]
fn a_substituted_git_repository_is_refused_with_both_origins() -> TestResult {
    let approved_origin = "git+https://approved.example/engine?rev=abc";
    let register = register("git", approved_origin)?;
    let approved = edges_for(&git_dependency(approved_origin))?;
    assert!(
        audit_direct(&approved, &register).is_empty(),
        "the unchanged approved Git origin must pass"
    );

    let substituted = "git+https://different.example/engine?rev=abc";
    let edges = edges_for(&git_dependency(substituted))?;
    let refusals = audit_direct(&edges, &register);
    match *first(&refusals)? {
        Refusal::OriginDrift {
            ref approved,
            ref declared,
            ..
        } => {
            assert_eq!(
                approved, approved_origin,
                "the approved origin is named exactly"
            );
            assert_eq!(
                declared, substituted,
                "the observed origin is named exactly"
            );
        }
        ref other => return Err(format!("expected OriginDrift, got {other:?}").into()),
    }
    Ok(())
}

#[test]
fn a_git_revision_change_is_refused_inside_the_approved_repository() -> TestResult {
    let register = register("git", "git+https://repo.example/engine?rev=abc")?;
    for substituted in [
        "git+https://repo.example/engine?rev=def",
        "git+https://repo.example/engine?branch=main",
        "git+https://repo.example/engine",
    ] {
        let edges = edges_for(&git_dependency(substituted))?;
        let refusals = audit_direct(&edges, &register);
        match *first(&refusals)? {
            Refusal::OriginDrift { ref declared, .. } => {
                assert_eq!(declared, substituted, "the changed revision is named");
            }
            ref other => {
                return Err(
                    format!("expected OriginDrift for {substituted}, got {other:?}").into(),
                );
            }
        }
    }
    Ok(())
}

#[test]
fn a_substituted_registry_is_refused_with_both_identities() -> TestResult {
    let approved_origin = "registry+https://approved.example/index";
    let register = register("registry", approved_origin)?;
    let approved = edges_for(&registry_dependency(approved_origin))?;
    assert!(audit_direct(&approved, &register).is_empty());

    let substituted = "registry+https://different.example/index";
    let edges = edges_for(&registry_dependency(substituted))?;
    let refusals = audit_direct(&edges, &register);
    match *first(&refusals)? {
        Refusal::OriginDrift {
            ref approved,
            ref declared,
            ..
        } => {
            assert_eq!(approved, approved_origin);
            assert_eq!(declared, substituted);
        }
        ref other => return Err(format!("expected OriginDrift, got {other:?}").into()),
    }
    Ok(())
}

#[test]
fn a_substituted_external_path_is_refused_with_both_authorities() -> TestResult {
    let register = register("path", "../vendor/engine")?;
    let approved = edges_for(&path_dependency("../vendor/engine"))?;
    assert!(audit_direct(&approved, &register).is_empty());

    let edges = edges_for(&path_dependency("../vendor/evil"))?;
    let refusals = audit_direct(&edges, &register);
    match *first(&refusals)? {
        Refusal::OriginDrift {
            ref approved,
            ref declared,
            ..
        } => {
            assert_eq!(approved, "../vendor/engine");
            assert_eq!(declared, "../vendor/evil");
        }
        ref other => return Err(format!("expected OriginDrift, got {other:?}").into()),
    }
    Ok(())
}
