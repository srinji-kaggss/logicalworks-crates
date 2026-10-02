//! Decode Cargo's authored dependency dimensions from a real locked workspace.
//!
//! Issue #158 A2: `features`, `uses_default_features`, `optional`, `target` and
//! `rename` are authored facts a capability policy has to see. The fixture is a
//! real path-only workspace whose members each depend on one local `engine`
//! package and vary exactly one dimension from `baseline`; `baseline.json` is the
//! raw `cargo metadata` output, retained so the decode runs without Cargo and
//! re-checked against a fresh run so the retained bytes cannot silently drift
//! from what Cargo emits.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;

use lgwks_deps::metadata::{self, DirectEdge};

type TestResult = Result<(), Box<dyn Error>>;

/// The fixture workspace root.
fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cargo-metadata")
}

/// The retained raw metadata output, decoded through the public API.
fn baseline() -> Result<Vec<DirectEdge>, Box<dyn Error>> {
    let raw = std::fs::read_to_string(fixture().join("baseline.json"))?;
    Ok(metadata::parse(&raw)?)
}

/// The single edge each dimension member authors, by consumer.
fn edge_of<'a>(edges: &'a [DirectEdge], consumer: &str) -> Result<&'a DirectEdge, String> {
    edges
        .iter()
        .find(|edge| edge.consumer() == consumer)
        .ok_or_else(|| format!("no decoded edge for consumer {consumer:?}"))
}

/// Every declaration targets the same Cargo package identity: a rename is a
/// local spelling, never a second package.
#[test]
fn every_declaration_keeps_the_upstream_package_identity() -> TestResult {
    let edges = baseline()?;
    assert_eq!(edges.len(), 6, "six dimension members author one edge each");
    for edge in &edges {
        assert_eq!(
            edge.package(),
            "engine",
            "the Cargo package identity is the upstream name, not the rename"
        );
    }
    Ok(())
}

/// Each member varies exactly one dimension from `baseline`, and the decoder
/// reports that dimension.
#[test]
fn one_dimension_at_a_time_is_decoded() -> TestResult {
    let edges = baseline()?;
    let base = edge_of(&edges, "baseline")?;
    assert!(base.features().is_empty(), "baseline enables no feature");
    assert!(base.uses_default_features(), "baseline takes defaults");
    assert!(!base.optional, "baseline is mandatory");
    assert_eq!(base.target(), None, "baseline is unconditional");
    assert_eq!(base.rename(), None, "baseline is not renamed");

    let feature = edge_of(&edges, "feature")?;
    assert_eq!(feature.features(), ["extra".to_owned()].as_slice());
    assert!(feature.uses_default_features());
    assert!(!feature.optional);

    let default_off = edge_of(&edges, "default_off")?;
    assert!(default_off.features().is_empty());
    assert!(
        !default_off.uses_default_features(),
        "default-features = false must be visible"
    );

    let optional = edge_of(&edges, "optional")?;
    assert!(optional.optional, "optionality must be visible");
    assert!(optional.uses_default_features());

    #[cfg(unix)]
    {
        let target_scope = edge_of(&edges, "target_scope")?;
        assert_eq!(target_scope.target(), Some("cfg(unix)"));
        assert!(!target_scope.optional);
    }

    let renamed = edge_of(&edges, "renamed")?;
    assert_eq!(renamed.rename(), Some("alias_engine"));
    assert_eq!(
        renamed.package(),
        "engine",
        "a rename keeps the upstream identity in `package`"
    );
    Ok(())
}

/// The retained bytes are the bytes Cargo emits now: a fresh `cargo metadata`
/// over the fixture decodes to the same edges.
#[test]
fn the_retained_metadata_matches_a_fresh_locked_run() -> TestResult {
    let retained = baseline()?;
    let manifest = fixture().join("Cargo.toml");
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--no-deps", "--format-version", "1", "--locked"])
        .arg("--manifest-path")
        .arg(&manifest)
        .current_dir(fixture())
        .output()?;
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let fresh = metadata::parse(&String::from_utf8(output.stdout)?)?;
    assert_eq!(
        fresh, retained,
        "the retained baseline.json must match a fresh locked run"
    );
    Ok(())
}
