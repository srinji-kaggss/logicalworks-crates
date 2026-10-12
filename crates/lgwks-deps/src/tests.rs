//! The gate's own tests: the audit over small registers and edges.

use super::*;
use crate::edge::{CRATES_IO_INDEX, CRATES_IO_SPARSE};
use crate::license::{declared_expression, rejected_license_identifiers};

/// What every test here returns.
///
/// The workspace forbids `unwrap`/`expect` outright, with no test exemption,
/// so a test propagates its failure with `?` instead of aborting the run.
/// The message is carried in the error rather than in an added `.expect`.
type TestResult = Result<(), Box<dyn std::error::Error>>;

const REGISTER: &str = concat!(
    "[policy]\nenforce = true\naccepted_licenses = \"0BSD, Apache-2.0, Apache-2.0 WITH LLVM-exception, BSD-3-Clause, CC0-1.0, MIT, Zlib\"\n\n",
    "[[approved]]\n",
    "crate = \"serde\"\n",
    "tier = \"boundary\"\n",
    "version = \"1.0\"\n",
    "owner = \"lgwks_std\"\n",
    "capability = \"json.serialization\"\n",
    "license = \"MIT OR Apache-2.0\"\n",
    "source = \"registry\"\n",
    "allowed_consumers = \"lgwks_std\"\n",
    "allowed_kinds = \"normal\"\n",
    "reason = \"Derive-based serialization needs compiler introspection std lacks.\"\n",
    "approved_by = \"reviewer\"\n",
    "approved_on = \"2026-08-19\"\n",
    "review = \"docs/ADMISSION.md\"\n",
);

fn edge(consumer: &str, package: &str, requirement: &str) -> DirectEdge {
    DirectEdge {
        consumer: consumer.into(),
        package: package.into(),
        requirement: requirement.into(),
        kind: metadata::DependencyKind::Normal,
        source: metadata::DependencySource::Registry(
            "registry+https://github.com/rust-lang/crates.io-index".into(),
        ),
        optional: false,
        workspace: false,
        target_repository: None,
        features: Vec::new(),
        uses_default_features: true,
        target: None,
        rename: None,
        license: Some("MIT OR Apache-2.0".into()),
        license_file: None,
    }
}

/// One approval for `engine`, owned by `app`, with the given source class
/// and optional exact origin.
///
/// `origin` is `None` for a legacy class-only entry, which is the shape the
/// origin tests must show is insufficient rather than permissive.
fn register_with(source: &str, origin: Option<&str>) -> Result<Contract, contract::ContractError> {
    register_tiered("boundary", source, origin)
}

/// [`register_with`] for an approval claiming `tier`.
fn register_tiered(
    tier: &str,
    source: &str,
    origin: Option<&str>,
) -> Result<Contract, contract::ContractError> {
    // An entry with no `origin` key is a legacy class-only approval, and the
    // register text says so by not carrying the line: an empty line would be
    // `origin = ""`, which `is_supported_origin` refuses as an unknown shape.
    let origin_lines = match origin {
        Some(origin) => format!("origin = \"{origin}\"\n"),
        None => String::new(),
    };
    Contract::parse(&format!(
        concat!(
            "[policy]\nenforce = true\naccepted_licenses = \"0BSD, Apache-2.0, Apache-2.0 WITH LLVM-exception, BSD-3-Clause, CC0-1.0, MIT, Zlib\"\n\n",
            "[[approved]]\n",
            "crate = \"engine\"\n",
            "tier = \"{tier}\"\n",
            "version = \"1.0\"\n",
            "owner = \"app\"\n",
            "capability = \"engine.core\"\n",
            "license = \"MIT OR Apache-2.0\"\n",
            "source = \"{source}\"\n",
            "{origin}",
            "allowed_consumers = \"app\"\n",
            "allowed_kinds = \"normal\"\n",
            "reason = \"The engine supplies a capability the standard library cannot express.\"\n",
            "approved_by = \"reviewer\"\n",
            "approved_on = \"2026-09-30\"\n",
            "review = \"tests/origin_binding.rs\"\n",
        ),
        tier = tier,
        source = source,
        origin = origin_lines,
    ))
}

/// Every line of a manifest's `[dependencies]` table, up to the next table.
///
/// One reader for both manifest checks: they differ in which manifest they
/// read and what they assert, and two copies of the same six-line walk are
/// two places for a table header to be read wrongly.
fn dependency_declarations<'a>(
    manifest: &'a str,
    crate_label: &str,
) -> Result<Vec<&'a str>, Box<dyn std::error::Error>> {
    let after = manifest
        .split("[dependencies]")
        .nth(1)
        .ok_or_else(|| format!("{crate_label} declares no [dependencies]"))?;
    Ok(after
        .lines()
        .map(str::trim)
        .take_while(|line| !line.starts_with('['))
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect())
}

/// The package name a `key = "value"` dependency line declares.
fn declared_name(line: &str) -> &str {
    line.split_once('=').map_or(line, |(name, _)| name).trim()
}

/// The `app` → `engine` edge with a chosen source.
fn app_edge(source: metadata::DependencySource) -> DirectEdge {
    let mut edge = edge("app", "engine", "1.0");
    edge.source = source;
    edge
}

/// The five surfaces this repository's register declares (INV-DEP-1).
const SURFACES: [&str; 5] = [
    "lgwks_std",
    "lgwks_bot",
    "lgwks_deps",
    "lgwks_ast",
    "lgwks_macros",
];

/// [`REGISTER`] declaring a closed surface set in its own `[policy]`.
fn surface_register() -> String {
    REGISTER.replacen(
        "enforce = true\n",
        &format!("enforce = true\nsurfaces = \"{}\"\n", SURFACES.join(", ")),
        1,
    )
}

/// Issue #207: an approval owned by a surface INV-DEP-1 does not list is
/// refused by name, where the identifier check alone admitted it.
#[test]
fn a_rogue_approval_owner_is_refused_by_name() -> TestResult {
    let register = Contract::parse(
        &surface_register().replace("owner = \"lgwks_std\"", "owner = \"lgwks_rogue\""),
    )?;
    let refusals = surface_refusals(&[], &register);
    assert_eq!(
        refusals,
        vec![Refusal::UnknownSurface {
            krate: "lgwks_rogue".into(),
            role: "approval owner".into(),
        }]
    );
    assert!(
        refusals[0].to_string().contains("lgwks_rogue"),
        "the refusal names the rogue surface"
    );
    Ok(())
}

/// Issue #207: a sixth workspace member is refused by name, and the five
/// real surfaces are not.
#[test]
fn a_sixth_workspace_member_is_refused_by_name() -> TestResult {
    let register = Contract::parse(&surface_register())?;
    let mut members: Vec<String> = SURFACES.iter().map(|name| (*name).to_owned()).collect();
    assert!(
        surface_refusals(&members, &register).is_empty(),
        "the five enumerated surfaces are admitted"
    );
    members.push("lgwks_sixth".into());
    assert_eq!(
        surface_refusals(&members, &register),
        vec![Refusal::UnknownSurface {
            krate: "lgwks_sixth".into(),
            role: "workspace member".into(),
        }]
    );
    Ok(())
}

/// No closed surface set is compiled in: a register that declares no
/// `surfaces` refuses no member or owner for its name, whatever it is.
#[test]
fn a_register_without_surfaces_keeps_no_closed_set() -> TestResult {
    let register = Contract::parse(&REGISTER.replace("lgwks_std", "lgwks_rogue"))?;
    let members = vec!["anything".to_owned(), "lgwks_sixth".to_owned()];
    assert!(surface_refusals(&members, &register).is_empty());
    Ok(())
}

/// The freeze is the register's: `frozen_surfaces` and `frozen_tier` name
/// what is frozen and at what tier, and without them nothing is.
#[test]
fn the_freeze_is_declared_by_the_register() -> TestResult {
    let vendored = REGISTER
        .replace("lgwks_std", "lgwks_ast")
        .replace("tier = \"boundary\"", "tier = \"vendor\"");
    let unfrozen = Contract::parse(&vendored)?;
    assert!(frozen_surface_tier_refusals(&unfrozen).is_empty());
    let frozen = Contract::parse(&vendored.replacen(
        "enforce = true\n",
        "enforce = true\nfrozen_surfaces = \"lgwks_ast\"\nfrozen_tier = \"boundary\"\n",
        1,
    ))?;
    assert_eq!(
        frozen_surface_tier_refusals(&frozen),
        vec![Refusal::FrozenSurfaceTier {
            consumer: "lgwks_ast".into(),
            krate: "serde".into(),
            tier: "vendor".into(),
        }]
    );
    Ok(())
}

/// Accepting `MIT` does not accept `MIT-0`: a term matches whole.
#[test]
fn an_accepted_term_does_not_accept_its_prefix_extension() {
    let accepted = vec!["MIT".to_owned()];
    assert_eq!(
        rejected_license_identifiers("MIT-0", &accepted),
        vec!["MIT-0".to_owned()]
    );
    assert!(rejected_license_identifiers("MIT OR MIT", &accepted).is_empty());
}

/// Cargo's deprecated `MIT/Apache-2.0` is `MIT OR Apache-2.0`: an approval
/// recorded in SPDX over a package declaring the slash form is neither
/// drift nor an unknown identifier.
#[test]
fn a_slash_license_matches_its_spdx_or_approval() -> TestResult {
    let register = Contract::parse(REGISTER)?;
    let mut serde = edge("lgwks_std", "serde", "1.0");
    serde.license = Some("MIT/Apache-2.0".into());
    assert_eq!(license_refusals(&[serde], &register), Vec::new());
    Ok(())
}

/// The slash only separates alternatives; it does not launder one. A
/// declared `GPL-3.0/MIT` with only `MIT` accepted still names `GPL-3.0`.
#[test]
fn a_slash_license_still_rejects_an_unaccepted_alternative() -> TestResult {
    let accepted = vec!["MIT".to_owned()];
    assert_eq!(
        rejected_license_identifiers(&declared_expression(Some("GPL-3.0/MIT")), &accepted),
        vec!["GPL-3.0".to_owned()]
    );
    let register = Contract::parse(&REGISTER.replace(
        "accepted_licenses = \"0BSD, Apache-2.0, Apache-2.0 WITH LLVM-exception, BSD-3-Clause, CC0-1.0, MIT, Zlib\"",
        "accepted_licenses = \"MIT\"",
    ))?;
    let mut serde = edge("lgwks_std", "serde", "1.0");
    serde.license = Some("GPL-3.0/MIT".into());
    assert_eq!(
        license_refusals(&[serde], &register),
        vec![Refusal::LicenseNotAccepted {
            krate: "serde".into(),
            approved: "MIT OR Apache-2.0".into(),
            declared: "GPL-3.0/MIT".into(),
            rejected: vec!["GPL-3.0".into()],
        }]
    );
    Ok(())
}

/// Issue #210: `tier` is read. A `vendor` approval over an edge that
/// resolves from a registry is refused by name, because nothing was checked
/// into the workspace for it to describe.
#[test]
fn a_vendor_tier_approval_over_a_registry_edge_is_refused_by_name() -> TestResult {
    let register = register_tiered("vendor", "registry", None)?;
    let edge = app_edge(metadata::DependencySource::Registry(
        "registry+https://github.com/rust-lang/crates.io-index".into(),
    ));
    assert!(
        audit_direct(&[edge], &register).contains(&Refusal::VendorTierConflict {
            krate: "engine".into(),
            source: "registry".into(),
        }),
        "a vendor-tier approval must not admit a registry edge"
    );
    Ok(())
}

/// The other side of the same reader: a vendored crate is a path
/// dependency, and a `vendor` approval over one is the ordinary case.
#[test]
fn a_vendor_tier_approval_over_a_path_edge_is_admitted() -> TestResult {
    let register = register_tiered("vendor", "path", Some("../vendor/engine"))?;
    let edge = app_edge(metadata::DependencySource::Path("../vendor/engine".into()));
    assert!(
        audit_direct(&[edge], &register).is_empty(),
        "a vendored path edge under a vendor approval is admitted"
    );
    Ok(())
}

#[test]
fn direct_edge_requires_an_allowed_consumer() -> TestResult {
    let register = Contract::parse(REGISTER)?;
    assert_eq!(
        audit_direct(&[edge("braid-cli", "serde", "1.0")], &register),
        vec![
            Refusal::ConsumerNotAllowed {
                consumer: "braid-cli".into(),
                krate: "serde".into(),
            },
            Refusal::UnusedApproval {
                krate: "serde".into(),
                owner: "lgwks_std".into(),
                capability: "json.serialization".into(),
            },
        ]
    );
    Ok(())
}

#[test]
fn exact_owned_direct_edge_passes() -> TestResult {
    let register = Contract::parse(REGISTER)?;
    assert!(audit_direct(&[edge("lgwks_std", "serde", "1.0")], &register).is_empty());
    Ok(())
}

#[test]
fn unregistered_path_copy_is_refused() -> TestResult {
    let register = Contract::parse(REGISTER)?;
    let mut copied = edge("app", "braid-ir", "*");
    copied.source = metadata::DependencySource::Path("../copied-braid-ir".into());
    assert!(matches!(
        audit_direct(&[copied], &register).first(),
        Some(Refusal::UnregisteredEdge { source, .. }) if source.starts_with("path:")
    ));
    Ok(())
}

#[test]
fn copied_foreign_workspace_member_is_refused() -> TestResult {
    // `repository` goes *inside* the register's one `[policy]` block: the
    // reader refuses a second `[policy]`, which is the point of the
    // duplicate-section rule.
    let register = Contract::parse(&REGISTER.replace(
        "[policy]\nenforce = true",
        "[policy]\nrepository = \"https://example.invalid/consumer\"\nenforce = true",
    ))?;
    let mut copied = edge("app", "braid-ir", "*");
    copied.source = metadata::DependencySource::Path("vendor/braid-ir".into());
    copied.workspace = true;
    copied.target_repository = Some("https://github.com/srinji-kaggss/Braid".into());
    assert!(matches!(
        audit_direct(&[copied], &register).first(),
        Some(Refusal::ForeignWorkspaceMember { .. })
    ));
    Ok(())
}

/// The gate's own foundation is exempt because Cargo says it is a member of
/// this workspace, not because of what it is called. This is the positive
/// control for the name-only exemption the audit no longer has.
#[test]
fn a_workspace_member_is_exempt_from_external_admission() -> TestResult {
    let register = Contract::parse(REGISTER)?;
    let mut foundation = edge("lgwks_deps", "lgwks_std", "^0.6.6");
    foundation.source = metadata::DependencySource::Path("../lgwks-std".into());
    foundation.workspace = true;
    let refusals = audit_direct(&[foundation], &register);
    assert!(
        !refusals
            .iter()
            .any(|refusal| refusal.krate() == "lgwks_std"),
        "a validated workspace member needs no external approval; got {refusals:?}"
    );
    Ok(())
}

/// A package that is *not* a workspace member cannot buy admission with a
/// name. Both the spelling the issue used and Cargo's `-`/`_` equivalent
/// must reach the same refusal.
#[test]
fn an_external_package_claiming_a_self_exempt_name_is_refused() -> TestResult {
    let register = Contract::parse(REGISTER)?;
    for name in ["lgwks_std", "lgwks-std", "lgwks_deps", "LGWKS-STD"] {
        let mut impostor = edge("consumer", name, "*");
        impostor.source = metadata::DependencySource::Path("/outside-workspace/unreviewed".into());
        let refusals = audit_direct(&[impostor], &register);
        assert!(
            matches!(
                refusals.first(),
                Some(Refusal::UnregisteredEdge { source, .. })
                    if source == "path:/outside-workspace/unreviewed"
            ),
            "an external {name} must be refused with the source it came from; got {refusals:?}"
        );
    }
    Ok(())
}

/// The same name from a Git origin is refused too, and the refusal carries
/// the Git URL so an approval cannot be satisfied by the spelling.
#[test]
fn an_external_git_package_claiming_a_self_exempt_name_is_refused() -> TestResult {
    let register = Contract::parse(REGISTER)?;
    let mut impostor = edge("consumer", "lgwks_std", "*");
    impostor.source = metadata::DependencySource::Git(
        "git+https://example.invalid/not-logical-works?rev=deadbeef".into(),
    );
    assert!(
        matches!(
            audit_direct(&[impostor], &register).first(),
            Some(Refusal::UnregisteredEdge { source, .. })
                if source == "git:git+https://example.invalid/not-logical-works?rev=deadbeef"
        ),
        "a same-named Git source is an ordinary external edge"
    );
    Ok(())
}

/// Renaming the manifest key does not help either: metadata reports the
/// upstream package name, which is what the audit reads.
#[test]
fn a_manifest_renamed_edge_is_refused_under_its_upstream_name() -> TestResult {
    let register = Contract::parse(REGISTER)?;
    let mut impostor = edge("consumer", "lgwks_std", "*");
    impostor.source =
        metadata::DependencySource::Registry("registry+https://example.invalid".into());
    assert!(matches!(
        audit_direct(&[impostor], &register).first(),
        Some(Refusal::UnregisteredEdge { krate, .. }) if krate == "lgwks_std"
    ));
    Ok(())
}

#[test]
fn registry_to_git_source_drift_is_refused() -> TestResult {
    let register = Contract::parse(REGISTER)?;
    let mut git = edge("lgwks_std", "serde", "1.0");
    git.source =
        metadata::DependencySource::Git("git+https://example.invalid/serde?rev=abc#abc".into());
    assert!(
        audit_direct(&[git], &register)
            .iter()
            .any(|refusal| matches!(refusal, Refusal::SourceDrift { .. }))
    );
    Ok(())
}

#[test]
fn unused_approval_is_refused() -> TestResult {
    let register = Contract::parse(REGISTER)?;
    assert!(matches!(
        audit_direct(&[], &register).as_slice(),
        [Refusal::UnusedApproval { .. }]
    ));
    Ok(())
}

/// An authored Git origin admits its own repository and refuses another,
/// with both identities in the refusal.
#[test]
fn an_approved_git_origin_admits_only_that_repository() -> TestResult {
    let register = register_with("git", Some("git+https://approved.example/engine?rev=abc"))?;
    let approved = app_edge(metadata::DependencySource::Git(
        "git+https://approved.example/engine?rev=abc".into(),
    ));
    assert!(
        audit_direct(&[approved], &register).is_empty(),
        "the approved Git origin must pass"
    );

    let substituted = app_edge(metadata::DependencySource::Git(
        "git+https://different.example/engine?rev=abc".into(),
    ));
    let refusals = audit_direct(&[substituted], &register);
    assert!(
        matches!(
            refusals.first(),
            Some(Refusal::OriginDrift { approved, declared, .. })
                if approved.contains("approved.example") && declared.contains("different.example")
        ),
        "a substituted Git repository must be an OriginDrift naming both origins: {refusals:?}"
    );
    Ok(())
}

/// A Git revision/reference policy change inside the approved repository is
/// a drift: the repository is admitted, the pin is not.
#[test]
fn a_git_revision_policy_change_is_an_origin_drift() -> TestResult {
    let register = register_with("git", Some("git+https://repo.example/engine?rev=abc"))?;
    for substituted in [
        "git+https://repo.example/engine?rev=def",
        "git+https://repo.example/engine?branch=main",
        "git+https://repo.example/engine",
    ] {
        let edge = app_edge(metadata::DependencySource::Git(substituted.into()));
        let refusals = audit_direct(&[edge], &register);
        assert!(
            matches!(refusals.first(), Some(Refusal::OriginDrift { declared, .. }) if declared == substituted),
            "a changed revision policy ({substituted}) must be an OriginDrift: {refusals:?}"
        );
    }
    Ok(())
}

/// A different registry is refused even though the source class matches.
#[test]
fn an_approved_registry_origin_refuses_a_different_registry() -> TestResult {
    let register = register_with("registry", Some("registry+https://approved.example/index"))?;
    let approved = app_edge(metadata::DependencySource::Registry(
        "registry+https://approved.example/index".into(),
    ));
    assert!(audit_direct(&[approved], &register).is_empty());

    let substituted = app_edge(metadata::DependencySource::Registry(
        "registry+https://different.example/index".into(),
    ));
    let refusals = audit_direct(&[substituted], &register);
    assert!(
        matches!(
            refusals.first(),
            Some(Refusal::OriginDrift { declared, .. }) if declared.contains("different.example")
        ),
        "a different registry must be an OriginDrift: {refusals:?}"
    );
    Ok(())
}

/// A legacy class-only registry entry is exact for crates.io and nothing
/// else: it is not implicit approval of every registry.
#[test]
fn a_class_only_registry_approval_admits_crates_io_only() -> TestResult {
    let register = register_with("registry", None)?;
    let crates_io = app_edge(metadata::DependencySource::Registry(CRATES_IO_INDEX.into()));
    assert!(
        audit_direct(&[crates_io], &register).is_empty(),
        "the one deterministic registry stays admitted by a class-only entry"
    );
    let sparse = app_edge(metadata::DependencySource::Registry(
        CRATES_IO_SPARSE.into(),
    ));
    assert!(
        audit_direct(&[sparse], &register).is_empty(),
        "the sparse spelling names the same crates.io registry"
    );

    let other = app_edge(metadata::DependencySource::Registry(
        "registry+https://different.example/index".into(),
    ));
    let refusals = audit_direct(&[other], &register);
    assert!(
        matches!(
            refusals.first(),
            Some(Refusal::OriginDrift { approved, .. }) if approved == CRATES_IO_INDEX
        ),
        "a class-only registry entry must admit crates.io only: {refusals:?}"
    );
    Ok(())
}

/// A class-only Git entry cannot grant exact-origin assurance: every Git
/// origin is refused until an origin is authored.
#[test]
fn a_class_only_git_approval_is_insufficient_for_exact_origin() -> TestResult {
    let register = register_with("git", None)?;
    let edge = app_edge(metadata::DependencySource::Git(
        "git+https://any.example/engine?rev=abc".into(),
    ));
    let refusals = audit_direct(&[edge], &register);
    assert!(
        matches!(
            refusals.first(),
            Some(Refusal::OriginDrift { approved, .. }) if approved.contains("class-only")
        ),
        "a class-only Git entry must be insufficient, not permissive: {refusals:?}"
    );
    Ok(())
}

/// An authored external path authority admits only that path.
#[test]
fn an_approved_path_origin_refuses_a_different_path() -> TestResult {
    let register = register_with("path", Some("../vendor/engine"))?;
    let approved = app_edge(metadata::DependencySource::Path("../vendor/engine".into()));
    assert!(audit_direct(&[approved], &register).is_empty());

    let substituted = app_edge(metadata::DependencySource::Path("../vendor/evil".into()));
    let refusals = audit_direct(&[substituted], &register);
    assert!(
        matches!(
            refusals.first(),
            Some(Refusal::OriginDrift { approved, declared, .. })
                if approved == "../vendor/engine" && declared == "../vendor/evil"
        ),
        "a different external path must be an OriginDrift: {refusals:?}"
    );
    Ok(())
}

/// An unknown scheme is not an ordinary admitted origin: it cannot be
/// authored, and an edge carrying it does not match a registry approval.
#[test]
fn an_unknown_scheme_is_not_an_admitted_origin() -> TestResult {
    assert!(
        matches!(
            register_with("registry", Some("svn+https://example.invalid/x")),
            Err(contract::ContractError::InvalidField {
                field: "origin",
                ..
            })
        ),
        "an unknown origin scheme must be refused at register load"
    );
    let register = register_with("registry", None)?;
    let edge = app_edge(metadata::DependencySource::Other(
        "svn+https://example.invalid/x".into(),
    ));
    let refusals = audit_direct(&[edge], &register);
    assert!(
        matches!(refusals.first(), Some(Refusal::SourceDrift { .. })),
        "an unknown scheme is a class drift, never an ordinary admitted origin: {refusals:?}"
    );
    Ok(())
}

/// With several approvals for one crate, the reported drift is the one on
/// the approval that admits the edge's source class, not the first source
/// mismatch from an unrelated class.
#[test]
fn multiple_approvals_report_the_relevant_failed_dimension() -> TestResult {
    let text = concat!(
        "[policy]\nenforce = true\naccepted_licenses = \"0BSD, Apache-2.0, Apache-2.0 WITH LLVM-exception, BSD-3-Clause, CC0-1.0, MIT, Zlib\"\n\n",
        "[[approved]]\n",
        "crate = \"engine\"\ntier = \"boundary\"\nversion = \"1.0\"\nowner = \"app\"\n",
        "capability = \"engine.git\"\nlicense = \"MIT OR Apache-2.0\"\nsource = \"git\"\n",
        "origin = \"git+https://repo.example/engine\"\n",
        "allowed_consumers = \"app\"\nallowed_kinds = \"normal\"\n",
        "reason = \"The engine supplies a capability the standard library cannot express.\"\n",
        "approved_by = \"reviewer\"\napproved_on = \"2026-09-30\"\nreview = \"tests\"\n\n",
        "[[approved]]\n",
        "crate = \"engine\"\ntier = \"boundary\"\nversion = \"1.0\"\nowner = \"app\"\n",
        "capability = \"engine.registry\"\nlicense = \"MIT OR Apache-2.0\"\nsource = \"registry\"\n",
        "origin = \"registry+https://approved.example/index\"\n",
        "allowed_consumers = \"app\"\nallowed_kinds = \"normal\"\n",
        "reason = \"The engine supplies a capability the standard library cannot express.\"\n",
        "approved_by = \"reviewer\"\napproved_on = \"2026-09-30\"\nreview = \"tests\"\n",
    );
    let register = Contract::parse(text)?;

    // The edge's class is registry and the registry approval is present, so
    // the relevant drift is the kind, never the unrelated git source.
    let mut dev = app_edge(metadata::DependencySource::Registry(
        "registry+https://approved.example/index".into(),
    ));
    dev.kind = metadata::DependencyKind::Dev;
    let refusals = audit_direct(&[dev], &register);
    assert!(
        matches!(refusals.first(), Some(Refusal::KindNotAllowed { .. })),
        "the relevant kind drift must be reported, not the unrelated git source: {refusals:?}"
    );

    // A wrong requirement on the registry edge is likewise reported against
    // the registry approval, not the git half.
    let mut wrong_version = app_edge(metadata::DependencySource::Registry(
        "registry+https://approved.example/index".into(),
    ));
    wrong_version.requirement = "2.0".into();
    let refusals = audit_direct(&[wrong_version], &register);
    assert!(
        matches!(
            refusals.first(),
            Some(Refusal::RequirementDrift { approved, declared, .. })
                if approved == "1.0" && declared == "2.0"
        ),
        "the registry approval's requirement must be the one compared: {refusals:?}"
    );
    Ok(())
}

/// INV-STDPLUS-APPROVED-ONLY: lgwks-std may depend on vetted leaf crates
/// whose transitive trees bottom out at zero external deps. The gate itself
/// stays zero-dep (self-refuting otherwise). This test enforces both.
#[test]
fn deps_are_approved_leaves() -> TestResult {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("crates/lgwks-deps has no parent")?
        .parent()
        .ok_or("crates/ has no parent")?;

    // The deps storefront may depend only on the workspace facade it enforces,
    // plus the reviewed scan exception, plus optional storefront features
    // that the end user selects. The default build stays zero-dependency:
    // every third-party edge other than lgwks_std must be `optional = true`,
    // so `cargo build`/`cargo install` with no features pulls nothing.
    {
        let manifest = std::fs::read_to_string(workspace.join("crates/lgwks-deps/Cargo.toml"))?;
        let declared = dependency_declarations(&manifest, "gate")?;
        let names: Vec<&str> = declared.iter().copied().map(declared_name).collect();
        assert_eq!(
            names,
            [
                "lgwks_std",
                "proc-macro2",
                "quote",
                "gpui",
                "appcui",
                "bevy_ecs",
                "bevy_app",
                "bevy_time",
                "bevy_state",
                "tokio",
                "candle-core",
                "candle-nn",
                "candle-transformers",
                "tokenizers",
            ],
            "unexpected gate dependencies: {declared:?}"
        );
        for line in &declared {
            if !line.starts_with("lgwks_std") {
                assert!(
                    line.contains("optional = true"),
                    "storefront dependency must stay optional so the default build is \
                     zero-dependency: {line}"
                );
            }
        }
        assert!(declared[0].starts_with("lgwks_std ="));
    }

    // lgwks-std may only declare deps on this approved list.
    {
        const APPROVED: &[&str] = &[
            "blake3",
            "ed25519-dalek",
            "getrandom",
            "iri-string",
            "regex",
            "rkyv",
            "ron",
            "rustix",
            "serde",
            "serde_json",
            "tracing",
            "tracing-subscriber",
            "ureq",
        ];

        let manifest = std::fs::read_to_string(workspace.join("crates/lgwks-std/Cargo.toml"))?;
        let declared = dependency_declarations(&manifest, "std")?;
        for line in &declared {
            let name = declared_name(line);
            assert!(
                APPROVED.contains(&name),
                "lgwks-std declares unapproved dependency `{name}` — \
                 add it to APPROVED in this test after review"
            );
        }
    }
    Ok(())
}

#[test]
fn a_repository_root_is_the_nearest_ancestor_holding_a_lock() -> TestResult {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = repository_root(here)?;
    assert!(root.join("Cargo.lock").is_file());
    Ok(())
}
