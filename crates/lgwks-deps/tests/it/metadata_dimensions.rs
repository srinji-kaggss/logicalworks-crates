//! Decode Cargo's authored dependency dimensions from a real locked workspace.
//!
//! Issue #158 A2: `features`, `uses_default_features`, `optional`, `target` and
//! `rename` are authored facts a capability policy has to see. The fixture is a
//! real path-only workspace whose members each depend on one local `engine`
//! package and vary exactly one dimension from `baseline`; `baseline.json` is the
//! retained `cargo metadata` output (its host-specific fixture root replaced by
//! `__FIXTURE_ROOT__`), decoded without Cargo and re-checked against a fresh run
//! so the retained bytes cannot silently drift from what Cargo emits.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;

use lgwks_deps::metadata::{self, DirectEdge};

type TestResult = Result<(), Box<dyn Error>>;

/// The token `baseline.json` carries where a host's absolute fixture root would
/// otherwise be. A retained capture holding a developer's `/Users/…` path only
/// decoded on the machine that produced it; the token makes the same bytes
/// portable to every checkout.
const FIXTURE_ROOT_TOKEN: &str = "__FIXTURE_ROOT__";

/// The fixture workspace root.
fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cargo-metadata")
}

/// The retained raw metadata output, decoded through the public API with the
/// placeholder root resolved to `root`. Every path-bearing field (`id`,
/// `src_path`, `manifest_path`, dependency `path`, `target_directory`,
/// `workspace_root`) is covered by the one substitution, because they all spell
/// the same fixture root.
fn baseline_at(root: &Path) -> Result<Vec<DirectEdge>, Box<dyn Error>> {
    let raw = std::fs::read_to_string(fixture().join("baseline.json"))?;
    let resolved = raw.replace(FIXTURE_ROOT_TOKEN, &root.to_string_lossy());
    Ok(metadata::parse(&resolved)?)
}

/// The retained output resolved against the fixture's own directory.
fn baseline() -> Result<Vec<DirectEdge>, Box<dyn Error>> {
    baseline_at(&fixture())
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

/// Copies the fixture workspace to `destination`, member directories and lock
/// file included. Cargo's build output (`target/`) is derived and host-specific,
/// so it is not copied: it is not part of the metadata subject.
fn copy_fixture(source: &Path, destination: &Path) -> Result<(), Box<dyn Error>> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        copy_entry(&entry?, destination)?;
    }
    Ok(())
}

/// Copies one directory entry into `destination`, recursing into a directory and
/// leaving cargo's `target/` behind.
fn copy_entry(entry: &std::fs::DirEntry, destination: &Path) -> Result<(), Box<dyn Error>> {
    let name = entry.file_name();
    if name.to_string_lossy() == "target" {
        return Ok(());
    }
    let to = destination.join(&name);
    if entry.file_type()?.is_dir() {
        copy_fixture(&entry.path(), &to)
    } else {
        std::fs::copy(entry.path(), &to)?;
        Ok(())
    }
}

/// A distinguishable scratch name: wall-clock nanos plus a monotone sequence.
/// A process id or a bare timestamp would be reused by the OS, so neither is an
/// identity (INV-DEP-6).
fn scratch_suffix() -> Result<String, Box<dyn Error>> {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    // A clock that reports before the Unix epoch is refused rather than floored
    // to the epoch: a scratch directory named for 1970 is a name this process
    // cannot tell from any other process reading the same clock, which is the
    // identity the suffix exists to provide.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|before_epoch| {
            format!("the wall clock reads before the Unix epoch: {before_epoch}")
        })?
        .as_nanos();
    let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(format!("{nanos}-{sequence}"))
}

/// A scratch tree that is removed when the test finishes, panic or not.
///
/// The suffix makes the name unique per run, so no run can inherit another's
/// records; the guard is what keeps the tree from outliving the run at all,
/// which a leading `remove_dir_all` alone cannot do for a test that fails
/// part way through.
struct Scratch {
    /// The absolute root the fixture was copied into.
    root: PathBuf,
}

impl Scratch {
    /// A fresh scratch tree under the build's own temporary directory.
    fn new() -> Result<Self, Box<dyn Error>> {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("cargo-metadata-{}", scratch_suffix()?));
        Ok(Self { root })
    }

    /// The tree's root.
    fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // A scratch tree this run owns is this run's to remove. `Drop` has no
        // channel to return through, so a removal that fails is *reported* on
        // the debug stream rather than discarded: a tree left behind is a fact
        // the next run should be able to see, and the suffix guarantees the next
        // run cannot read it as its own.
        if let Err(cause) = std::fs::remove_dir_all(&self.root) {
            let refused: Result<(), _> = Err(cause);
            lgwks_std::trace::debug!(
                error = ?refused.as_ref().err(),
                root = %self.root.display(),
                "Scratch::drop: the scratch tree could not be removed"
            );
        }
    }
}

/// The retained capture carries no host path: the same `baseline.json` decodes
/// to the same edges after the fixture is copied to a different absolute
/// directory and Cargo is re-run there. This is the portability proof for the
/// hosted Linux runner, whose checkout root is not the developer's.
#[test]
fn the_retained_metadata_is_host_independent() -> TestResult {
    let raw = std::fs::read_to_string(fixture().join("baseline.json"))?;
    assert!(
        !raw.contains("/Users/") && !raw.contains("/home/"),
        "the retained baseline must carry no host path"
    );

    let scratch = Scratch::new()?;
    let root = scratch.root();
    copy_fixture(&fixture(), root)?;

    let retained = baseline_at(root)?;
    let manifest = root.join("Cargo.toml");
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--no-deps", "--format-version", "1", "--locked"])
        .arg("--manifest-path")
        .arg(&manifest)
        .current_dir(root)
        .output()?;
    assert!(
        output.status.success(),
        "cargo metadata failed in the copied fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let fresh = metadata::parse(&String::from_utf8(output.stdout)?)?;
    assert_eq!(
        fresh, retained,
        "one retained baseline must decode identically at any absolute root"
    );
    std::fs::remove_dir_all(root)?;
    Ok(())
}
