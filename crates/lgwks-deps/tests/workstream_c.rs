//! Workstream C acceptance coverage for the authored invariant register.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;

use lgwks_deps::invariants::{self, Refusal, Register};

type TestResult = Result<(), Box<dyn Error>>;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture(name: &str) -> &'static str {
    match name {
        "valid" => include_str!("fixtures/invariants/valid.toml"),
        "missing-enforced-by" => {
            include_str!("fixtures/invariants/missing-enforced-by.toml")
        }
        "missing-enforced-path" => {
            include_str!("fixtures/invariants/missing-enforced-path.toml")
        }
        "unknown-scope" => include_str!("fixtures/invariants/unknown-scope.toml"),
        "duplicate-id" => include_str!("fixtures/invariants/duplicate-id.toml"),
        "malformed-id" => include_str!("fixtures/invariants/malformed-id.toml"),
        "review-enforcement" => include_str!("fixtures/invariants/review-enforcement.toml"),
        "intent-enforcement" => include_str!("fixtures/invariants/intent-enforcement.toml"),
        _ => "",
    }
}

fn audit_fixture(name: &str) -> Result<Vec<Refusal>, Box<dyn Error>> {
    let root = workspace_root();
    let packages = lgwks_deps::metadata::workspace_package_names(&root)?;
    let register = Register::parse(fixture(name))?;
    Ok(invariants::audit(&register, &root, &packages))
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
        let refusals = audit_fixture(fixture_name)?;
        assert_eq!(refusals.len(), 1, "fixture {fixture_name}");
        assert_eq!(refusals[0].id(), expected_id, "fixture {fixture_name}");
    }
    Ok(())
}

#[test]
fn a_valid_register_passes() -> TestResult {
    let refusals = audit_fixture("valid")?;
    assert_eq!(refusals.len(), 0);
    Ok(())
}

#[test]
fn review_and_intent_are_not_enforcement_kinds() -> TestResult {
    for fixture_name in ["review-enforcement", "intent-enforcement"] {
        let refusals = audit_fixture(fixture_name)?;
        assert_eq!(refusals.len(), 1, "fixture {fixture_name}");
        assert!(refusals[0].to_string().contains("static-check or monitor"));
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
