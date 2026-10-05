//! Schema migration and external-consumer compatibility (issue #158 A1/A2).
//!
//! Schema 1 is a class-only register; schema 2 adds the exact-origin and
//! admitted-capability-policy keys and the explicit alias list. The migration is
//! additive: a schema-1 register still parses under the same public API, the
//! committed register is schema 2, and an unknown future version is refused
//! rather than read under the older rules.

use std::error::Error;
use std::path::Path;

use lgwks_deps::contract::{Contract, ContractError};

type TestResult = Result<(), Box<dyn Error>>;

/// A minimal schema-1 register: no `schema` key, a class-only approval.
const SCHEMA_1: &str = concat!(
    "[policy]\nenforce = true\n\n",
    "[[approved]]\n",
    "crate = \"serde\"\ntier = \"boundary\"\nversion = \"^1\"\nowner = \"lgwks_std\"\n",
    "capability = \"encoding.serde\"\nlicense = \"MIT OR Apache-2.0\"\nsource = \"registry\"\n",
    "allowed_consumers = \"lgwks_std\"\nallowed_kinds = \"normal\"\n",
    "reason = \"Derive-based serialization needs compiler introspection the standard library does not expose.\"\n",
    "approved_by = \"maintainer\"\napproved_on = \"2026-08-30\"\nreview = \"https://example.invalid/1\"\n",
);

/// A downstream consumer reads the same public API for both schema versions.
#[test]
fn the_public_api_reads_both_schema_versions() -> TestResult {
    let v1 = Contract::parse(SCHEMA_1)?;
    assert_eq!(v1.schema(), 1, "an absent schema key is version 1");
    assert_eq!(v1.entry_count(), 1);
    assert!(
        v1.approval_for("serde").is_some(),
        "a schema-1 approval still resolves through the public lookup"
    );

    let v2 = Contract::parse(&SCHEMA_1.replace(
        "[policy]\nenforce = true",
        "[policy]\nschema = 2\nenforce = true",
    ))?;
    assert_eq!(v2.schema(), 2);
    assert_eq!(v2.entry_count(), 1);
    assert!(v2.approval_for("serde").is_some());
    Ok(())
}

/// An unknown future version is refused, never read under the older rules.
#[test]
fn an_unknown_future_schema_is_refused() {
    assert!(matches!(
        Contract::parse("[policy]\nschema = 99\n"),
        Err(ContractError::UnsupportedSchema { ref value, .. }) if value == "99"
    ));
}

/// The committed register was migrated in place to schema 2.
#[test]
fn the_committed_register_is_schema_two() -> TestResult {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contract/APPROVED.toml");
    let text = std::fs::read_to_string(path)?;
    let register = Contract::parse(&text)?;
    assert_eq!(
        register.schema(),
        2,
        "the committed register must author the migrated schema version"
    );
    assert!(
        register.entry_count() > 0,
        "the committed register carries approvals"
    );
    assert!(
        register.digest().starts_with("fnv1a128:"),
        "the register exposes its identity digest for a receipt"
    );
    Ok(())
}
