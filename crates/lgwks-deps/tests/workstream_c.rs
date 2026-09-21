//! Workstream C acceptance coverage for the authored invariant register.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;

use lgwks_deps::invariants::{self, Audit, ErrorKind, Refusal, Register, Status};

type TestResult = Result<(), Box<dyn Error>>;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The committed register fixtures, embedded so the test needs no file access.
const FIXTURES: [(&str, &str); 8] = [
    ("valid", include_str!("fixtures/invariants/valid.toml")),
    (
        "missing-enforced-by",
        include_str!("fixtures/invariants/missing-enforced-by.toml"),
    ),
    (
        "missing-enforced-path",
        include_str!("fixtures/invariants/missing-enforced-path.toml"),
    ),
    (
        "unknown-scope",
        include_str!("fixtures/invariants/unknown-scope.toml"),
    ),
    (
        "duplicate-id",
        include_str!("fixtures/invariants/duplicate-id.toml"),
    ),
    (
        "malformed-id",
        include_str!("fixtures/invariants/malformed-id.toml"),
    ),
    (
        "review-enforcement",
        include_str!("fixtures/invariants/review-enforcement.toml"),
    ),
    (
        "intent-enforcement",
        include_str!("fixtures/invariants/intent-enforcement.toml"),
    ),
];

fn fixture(name: &str) -> Result<&'static str, String> {
    let mut found = None;
    for (key, text) in FIXTURES {
        if key == name {
            found = Some(text);
        }
    }
    found.ok_or_else(|| format!("no fixture named {name:?}"))
}

/// Resolves a fixture register against this repository.
///
/// The member list comes from Cargo's own metadata rather than from a literal,
/// so the audit resolves scopes exactly as `lgwks-deps check` does.
fn audit_fixture(name: &str) -> Result<Audit, Box<dyn Error>> {
    let root = workspace_root();
    let members = lgwks_deps::metadata::workspace_members(&root)?;
    let register = Register::parse(fixture(name)?)?;
    Ok(invariants::audit(&register, &root, &members))
}

fn refusals_of(name: &str) -> Result<Vec<Refusal>, Box<dyn Error>> {
    Ok(audit_fixture(name)?.refusals().to_vec())
}

#[test]
fn each_refusal_rule_has_a_register_fixture() -> TestResult {
    let cases = [
        ("missing-enforced-by", "INV-BOT-FOUR-VERBS"),
        ("missing-enforced-path", "INV-BOT-FOUR-VERBS"),
        ("unknown-scope", "INV-BOGUS-NOT-IN-WORKSPACE"),
        ("duplicate-id", "INV-BOT-FOUR-VERBS"),
        ("malformed-id", "not-an-invariant-id"),
    ];
    for (fixture_name, expected_id) in cases {
        let refusals = refusals_of(fixture_name)?;
        assert_eq!(refusals.len(), 1, "fixture {fixture_name}");
        assert_eq!(refusals[0].id(), expected_id, "fixture {fixture_name}");
    }
    Ok(())
}

/// A resolved register is not an enforced one.
///
/// This is the assertion the old `a_valid_register_passes` got wrong: it bound
/// the four-verbs statement to `invariants.rs`, a file with no relationship to
/// the bot's verb traits, and read the absence of refusals as a pass. A valid
/// reference now earns `Resolved`, which says a mechanism exists and nothing
/// more; `Attested` is reserved for an entry with a recorded run.
#[test]
fn a_valid_register_resolves_without_claiming_enforcement() -> TestResult {
    let audit = audit_fixture("valid")?;
    assert!(
        audit.refusals().is_empty(),
        "the valid fixture must resolve; got {:?}",
        audit.refusals()
    );
    assert_eq!(audit.registered(), 1);
    assert_eq!(audit.resolved(), 1, "a reference is not a run");
    assert_eq!(audit.attested(), 0);
    assert_eq!(audit.refused(), 0);
    assert_eq!(audit.outcomes()[0].status, Status::Resolved);
    Ok(())
}

#[test]
fn review_and_intent_are_not_enforcement_kinds() -> TestResult {
    for fixture_name in ["review-enforcement", "intent-enforcement"] {
        let result = Register::parse(fixture(fixture_name)?);
        match result {
            Err(error) => {
                let rendered = error.to_string();
                assert!(
                    rendered.contains("static-check or monitor"),
                    "fixture {fixture_name} must name the closed grammar; got {rendered}"
                );
                assert!(
                    matches!(error, ErrorKind::UnsupportedEnforcement { .. }),
                    "fixture {fixture_name} must be a typed refusal"
                );
            }
            Ok(_) => {
                return Err(format!("fixture {fixture_name} must be refused at load").into());
            }
        }
    }
    Ok(())
}

#[test]
fn non_monitorable_constraints_are_rejected_at_load() {
    let result = Register::parse(include_str!("fixtures/invariants/non-monitorable.toml"));
    assert!(matches!(
        result,
        Err(lgwks_deps::invariants::ErrorKind::NonMonitorable { .. })
    ));
}

#[test]
fn a_missing_register_is_not_a_failure() -> TestResult {
    let missing = workspace_root().join("crates/lgwks-deps/tests/fixtures/invariants/absent");
    let result = invariants::check(&missing)?;
    assert_eq!(result, None);
    Ok(())
}

#[test]
fn check_keeps_the_legacy_verdict_without_an_invariant_register() -> TestResult {
    let fixture_root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/check-no-invariants");
    let binary = std::env::var_os("CARGO_BIN_EXE_lgwks-deps")
        .ok_or("Cargo did not provide the lgwks-deps test binary")?;
    let output = Command::new(binary)
        .args([
            "check",
            fixture_root.to_str().ok_or("fixture path is not UTF-8")?,
        ])
        .output()?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stderr)?, "");
    assert!(String::from_utf8(output.stdout)?.contains("semantic approvals"));
    Ok(())
}

#[test]
fn check_reports_dependency_and_invariant_registers_together() -> TestResult {
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/check-both");
    let binary = std::env::var_os("CARGO_BIN_EXE_lgwks-deps")
        .ok_or("Cargo did not provide the lgwks-deps test binary")?;
    let output = Command::new(binary)
        .args([
            "check",
            fixture_root.to_str().ok_or("fixture path is not UTF-8")?,
        ])
        .output()?;
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("dependency register:"));
    assert!(stderr.contains("invariant register:"));
    Ok(())
}

/// The combined `check` verdict must never say an invariant is *enforced*.
///
/// The command reads files and manifests; it executes nothing. A reader who
/// sees `enforced` beside a pass has been told something no run produced, which
/// is the whole of issue 34.
#[test]
fn check_never_claims_an_invariant_is_enforced() -> TestResult {
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/check-both");
    let binary = std::env::var_os("CARGO_BIN_EXE_lgwks-deps")
        .ok_or("Cargo did not provide the lgwks-deps test binary")?;
    for args in [
        vec!["check", fixture_root.to_str().ok_or("path")?],
        vec!["check", fixture_root.to_str().ok_or("path")?, "--json"],
    ] {
        let output = Command::new(&binary).args(&args).output()?;
        let stdout = String::from_utf8(output.stdout)?;
        let stderr = String::from_utf8(output.stderr)?;
        for stream in [&stdout, &stderr] {
            assert!(
                !stream.contains("enforced"),
                "`{}` must not report an invariant as enforced: {stream}",
                args.join(" ")
            );
            assert!(
                !stream.contains("\"enforced\"") && !stream.contains("enforced\""),
                "no exported key may imply enforcement: {stream}"
            );
        }
    }
    Ok(())
}

/// The explicit `invariants` audit states the limit of its own verdict.
///
/// The resolved path is the one that matters. A reader who sees `OK` beside an
/// invariant has been told that a mechanism exists and nothing more — but only
/// if the output says so, and the audit that reported `enforced` said the
/// opposite. This repository's own register is the subject: it is the artifact
/// the gate protects, so a verdict on it is worth more than a fixture's.
#[test]
fn the_explicit_audit_reports_its_scope() -> TestResult {
    let binary = std::env::var_os("CARGO_BIN_EXE_lgwks-deps")
        .ok_or("Cargo did not provide the lgwks-deps test binary")?;
    let root = workspace_root();
    let resolved = Command::new(&binary)
        .args(["invariants", root.to_str().ok_or("path is not UTF-8")?])
        .output()?;
    assert_eq!(
        resolved.status.code(),
        Some(0),
        "the authored register must resolve: {}",
        String::from_utf8_lossy(&resolved.stderr)
    );
    let stdout = String::from_utf8(resolved.stdout)?;
    assert!(
        stdout.contains(invariants::SCOPE),
        "a resolved invariant is not an enforced one, and the verdict must say so: {stdout}"
    );
    assert!(
        !stdout.contains("enforced"),
        "the doctor executed nothing; it cannot report enforcement: {stdout}"
    );

    // The refusal path carries the same limit, on the stream that carries the
    // refusal, so a reader who only ever sees failures is told it too.
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/check-both");
    let refused = Command::new(&binary)
        .args([
            "invariants",
            fixture_root.to_str().ok_or("fixture path is not UTF-8")?,
        ])
        .output()?;
    assert_eq!(refused.status.code(), Some(2));
    let stderr = String::from_utf8(refused.stderr)?;
    assert!(
        stderr.contains(invariants::SCOPE),
        "a refusal states the same scope as a resolution; got {stderr}"
    );
    Ok(())
}
