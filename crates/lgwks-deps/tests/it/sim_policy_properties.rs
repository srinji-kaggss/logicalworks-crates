//! Seeded property sweeps over the dependency-policy model (issue #158 A1/A2).
//!
//! Every test drives the public metadata + admission API with a seed-determined
//! case and asserts the model's expectation, 64 seeds × 128 cases. Each property
//! is independent, so a failure names the property, the seed and the case.

use std::error::Error;

use crate::deps_sim;

use lgwks_deps::declared_scope;

use deps_sim::{REGISTRY, Rng, TestResult, alias_line, code_for, edge, register};

/// The seeded case a property asserts: `(expected admission, observed verdict,
/// description for a failure)`.
type Case = (bool, u8, String);

/// Run `case` over 64 seeds × 128 draws.
fn property(
    name: &str,
    mut case: impl FnMut(&mut Rng) -> Result<Case, Box<dyn Error>>,
) -> TestResult {
    for seed in 0..64_u64 {
        let mut rng = Rng::new(seed);
        for _ in 0..128 {
            let (expected, code, context) = case(&mut rng)?;
            assert_eq!(code, u8::from(!expected), "{name}: seed {seed} {context}");
        }
    }
    Ok(())
}

// ── Identity ────────────────────────────────────────────────────────────────

#[test]
fn an_exact_package_name_is_admitted() -> TestResult {
    property("exact-name", |_rng| {
        let approval = register("engine", "registry", "")?;
        let observed = edge("engine", Some(REGISTRY), &[], true, false, None, None)?;
        Ok((true, code_for(&approval, observed), "engine".to_owned()))
    })
}

#[test]
fn a_fold_alike_name_without_an_alias_is_refused() -> TestResult {
    property("fold-alike", |_rng| {
        let approval = register("engine-core", "registry", "")?;
        let observed = edge("engine_core", Some(REGISTRY), &[], true, false, None, None)?;
        Ok((
            false,
            code_for(&approval, observed),
            "engine-core vs engine_core".to_owned(),
        ))
    })
}

#[test]
fn a_case_difference_is_refused() -> TestResult {
    property("case", |_rng| {
        let approval = register("Engine", "registry", "")?;
        let observed = edge("engine", Some(REGISTRY), &[], true, false, None, None)?;
        Ok((
            false,
            code_for(&approval, observed),
            "Engine vs engine".to_owned(),
        ))
    })
}

#[test]
fn an_authored_alias_admits_exactly_its_spelling() -> TestResult {
    property("alias", |rng| {
        let homed = rng.coin();
        let observed = if homed { "engine_core" } else { "engine-c0re" };
        let approval = register("engine-core", "registry", &alias_line(Some("engine_core")))?;
        let observed_edge = edge(observed, Some(REGISTRY), &[], true, false, None, None)?;
        Ok((
            homed,
            code_for(&approval, observed_edge),
            observed.to_owned(),
        ))
    })
}

#[test]
fn a_rename_keeps_the_upstream_identity() -> TestResult {
    property("rename", |rng| {
        let rename = if rng.coin() {
            Some("local_engine")
        } else {
            None
        };
        let approval = register("engine", "registry", "")?;
        let observed = edge("engine", Some(REGISTRY), &[], true, false, None, rename)?;
        // Whatever the local spelling, the upstream identity is what is approved.
        Ok((
            true,
            code_for(&approval, observed),
            format!("rename {rename:?}"),
        ))
    })
}

#[test]
fn a_rename_does_not_create_a_second_identity() -> TestResult {
    property("rename-not-identity", |_rng| {
        let approval = register("local_engine", "registry", "")?;
        let observed = edge(
            "engine",
            Some(REGISTRY),
            &[],
            true,
            false,
            None,
            Some("local_engine"),
        )?;
        Ok((
            false,
            code_for(&approval, observed),
            "local-only approval".to_owned(),
        ))
    })
}

// ── Features ────────────────────────────────────────────────────────────────

#[test]
fn an_enabled_subset_of_the_allowed_features() -> TestResult {
    property("feature-subset", |rng| {
        let allowed = ["a", "b", "c"];
        let enabled: Vec<&str> = allowed.iter().copied().filter(|_| rng.coin()).collect();
        let policy = format!("features = \"{}\"\n", allowed.join(","));
        let approval = register("engine", "registry", &policy)?;
        let observed = edge("engine", Some(REGISTRY), &enabled, true, false, None, None)?;
        Ok((true, code_for(&approval, observed), format!("{enabled:?}")))
    })
}

#[test]
fn an_enabled_feature_outside_the_allowed_set_is_refused() -> TestResult {
    property("feature-outside", |rng| {
        let extra = *rng.pick_named("out-of-set features", &["x", "y", "z"])?;
        let enabled = ["a", extra];
        let approval = register("engine", "registry", "features = \"a,b\"\n")?;
        let observed = edge("engine", Some(REGISTRY), &enabled, true, false, None, None)?;
        Ok((false, code_for(&approval, observed), format!("{enabled:?}")))
    })
}

#[test]
fn a_required_feature_that_is_missing_is_refused() -> TestResult {
    property("required-missing", |_rng| {
        let approval = register(
            "engine",
            "registry",
            "features = \"a,b\"\nrequired_features = \"a\"\n",
        )?;
        let observed = edge("engine", Some(REGISTRY), &["b"], true, false, None, None)?;
        Ok((
            false,
            code_for(&approval, observed),
            "b without a".to_owned(),
        ))
    })
}

#[test]
fn a_required_feature_that_is_present_is_admitted() -> TestResult {
    property("required-present", |_rng| {
        let approval = register(
            "engine",
            "registry",
            "features = \"a,b\"\nrequired_features = \"a\"\n",
        )?;
        let observed = edge(
            "engine",
            Some(REGISTRY),
            &["a", "b"],
            true,
            false,
            None,
            None,
        )?;
        Ok((true, code_for(&approval, observed), "a and b".to_owned()))
    })
}

// ── Defaults, optionality, target ───────────────────────────────────────────

#[test]
fn the_default_features_bit_must_match() -> TestResult {
    property("default-bit", |rng| {
        let bit = rng.coin();
        let approval = register(
            "engine",
            "registry",
            &format!("uses_default_features = \"{bit}\"\n"),
        )?;
        let observed = edge("engine", Some(REGISTRY), &[], bit, false, None, None)?;
        Ok((true, code_for(&approval, observed), format!("bit {bit}")))
    })
}

#[test]
fn a_mismatched_default_features_bit_is_refused() -> TestResult {
    property("default-mismatch", |rng| {
        let bit = rng.coin();
        let approval = register(
            "engine",
            "registry",
            &format!("uses_default_features = \"{}\"\n", !bit),
        )?;
        let observed = edge("engine", Some(REGISTRY), &[], bit, false, None, None)?;
        Ok((false, code_for(&approval, observed), format!("bit {bit}")))
    })
}

#[test]
fn the_optionality_bit_must_match() -> TestResult {
    property("optional-bit", |rng| {
        let bit = rng.coin();
        let approval = register("engine", "registry", &format!("optional = \"{bit}\"\n"))?;
        let observed = edge("engine", Some(REGISTRY), &[], true, bit, None, None)?;
        Ok((
            true,
            code_for(&approval, observed),
            format!("optional {bit}"),
        ))
    })
}

#[test]
fn a_mismatched_optionality_bit_is_refused() -> TestResult {
    property("optional-mismatch", |rng| {
        let bit = rng.coin();
        let approval = register("engine", "registry", &format!("optional = \"{}\"\n", !bit))?;
        let observed = edge("engine", Some(REGISTRY), &[], true, bit, None, None)?;
        Ok((
            false,
            code_for(&approval, observed),
            format!("optional {bit}"),
        ))
    })
}

#[test]
fn the_target_scope_must_match() -> TestResult {
    property("target-match", |rng| {
        let target = *rng.pick_named(
            "target scopes",
            &[None, Some("cfg(unix)"), Some("cfg(windows)")],
        )?;
        let approval = register(
            "engine",
            "registry",
            &format!("target = \"{}\"\n", declared_scope(target)),
        )?;
        let observed = edge("engine", Some(REGISTRY), &[], true, false, target, None)?;
        Ok((
            true,
            code_for(&approval, observed),
            format!("target {target:?}"),
        ))
    })
}

/// Every scope pairing the register grammar admits renders a refusal naming both
/// sides, and Cargo's two spellings of an unconditional edge print the same
/// label.
///
/// One seed drives both draws, so each pair is reached from a sequence the test
/// records; a failure prints the seed and the pair it drew. The message is what
/// an operator acts on, so it is asserted here rather than only the verdict code
/// — a refusal that printed `""` for an unconditional edge would pass every
/// verdict assertion and be unreadable.
#[test]
fn seeded_scope_pairs_render_both_spellings_of_an_unconditional_edge() -> TestResult {
    let scopes = [None, Some(""), Some("cfg(unix)"), Some("cfg(windows)")];
    for seed in 0..64_u64 {
        let mut rng = Rng::new(seed);
        for _ in 0..8 {
            let approved = *rng.pick_named("approved scopes", &scopes)?;
            let declared = *rng.pick_named("observed scopes", &scopes)?;
            // The register speaks its own vocabulary, so an approved `None` is
            // the unconditional declaration and an approved `Some("")` is the
            // same declaration written explicitly.
            let approval = register(
                "engine",
                "registry",
                &format!("target = \"{}\"\n", declared_scope(approved)),
            )?;
            let observed = edge("engine", Some(REGISTRY), &[], true, false, declared, None)?;
            let refusals = lgwks_deps::audit_direct(&[observed], &approval);
            let expected_scope = |scope: Option<&str>| match scope {
                Some(scope) if !scope.is_empty() => scope.to_owned(),
                _ => "<none>".to_owned(),
            };
            let approved_label = expected_scope(approved);
            let declared_label = expected_scope(declared);
            match refusals.first() {
                Some(refusal) => {
                    assert_eq!(
                        refusal.to_string(),
                        format!(
                            "app declares engine for target {declared_label}, contract admits {approved_label}"
                        ),
                        "seed {seed}: approved {approved:?} against declared {declared:?}"
                    );
                }
                None => assert_eq!(
                    approved_label, declared_label,
                    "seed {seed}: no refusal for approved {approved:?} and declared {declared:?}"
                ),
            }
        }
    }
    Ok(())
}

#[test]
fn a_mismatched_target_scope_is_refused() -> TestResult {
    property("target-mismatch", |rng| {
        let target = *rng.pick_named("target scopes", &[None, Some("cfg(unix)")])?;
        let other = if target.is_none() { "cfg(unix)" } else { "" };
        let approval = register("engine", "registry", &format!("target = \"{other}\"\n"))?;
        let observed = edge("engine", Some(REGISTRY), &[], true, false, target, None)?;
        Ok((
            false,
            code_for(&approval, observed),
            format!("target {target:?} vs {other:?}"),
        ))
    })
}

#[test]
fn an_unconstrained_dimension_never_refuses() -> TestResult {
    property("grandfathered", |rng| {
        let approval = register("engine", "registry", "")?;
        let observed = edge(
            "engine",
            Some(REGISTRY),
            &["any", "feature"],
            rng.coin(),
            rng.coin(),
            *rng.pick_named("target scopes", &[None, Some("cfg(unix)")])?,
            None,
        )?;
        Ok((
            true,
            code_for(&approval, observed),
            "unconstrained".to_owned(),
        ))
    })
}

// ── Origin and tenant isolation ─────────────────────────────────────────────

#[test]
fn an_approved_git_origin_is_admitted_and_a_substitution_refused() -> TestResult {
    property("git-origin", |rng| {
        const REPOS: [&str; 2] = ["https://a.example/engine", "https://b.example/engine"];
        let repo = *rng.pick_named("git repositories", &REPOS)?;
        let observed_repo = *rng.pick_named("git repositories", &REPOS)?;
        let approval = register(
            "engine",
            "git",
            &format!("origin = \"git+{repo}?rev=abc\"\n"),
        )?;
        let observed = edge(
            "engine",
            Some(&format!("git+{observed_repo}?rev=abc")),
            &[],
            true,
            false,
            None,
            None,
        )?;
        Ok((
            repo == observed_repo,
            code_for(&approval, observed),
            format!("{repo} vs {observed_repo}"),
        ))
    })
}

#[test]
fn an_approved_registry_origin_refuses_another_registry() -> TestResult {
    property("registry-origin", |rng| {
        let registries = [
            "registry+https://a.example/index",
            "registry+https://b.example/index",
        ];
        let approved = *rng.pick_named("registry sources", &registries)?;
        let observed = *rng.pick_named("registry sources", &registries)?;
        let approval = register("engine", "registry", &format!("origin = \"{approved}\"\n"))?;
        let observed_edge = edge("engine", Some(observed), &[], true, false, None, None)?;
        Ok((
            approved == observed,
            code_for(&approval, observed_edge),
            format!("{approved} vs {observed}"),
        ))
    })
}

#[test]
fn a_class_only_git_entry_never_admits_a_git_edge() -> TestResult {
    property("class-only-git", |rng| {
        let rev = *rng.pick_named("revision policies", &["?rev=abc", "?branch=main", ""])?;
        // A git approval with no `origin` is insufficient for any git edge.
        let approval = register("engine", "git", "")?;
        let observed = edge(
            "engine",
            Some(&format!("git+https://a.example/engine{rev}")),
            &[],
            true,
            false,
            None,
            None,
        )?;
        Ok((false, code_for(&approval, observed), rev.to_owned()))
    })
}
