//! `vendor` owns lockfile-to-tree coverage and enforces
//! INV-VENDOR-SINGLE-TREE: every registry package a repo's `Cargo.lock`
//! resolves must be present in the single shared vendor tree with
//! the exact bytes the lock pins, or the offline build it feeds is a lie.
//!
//! The check binds on hashes, not names: a lock package carries the sha256 of
//! its `.crate` file, and each tree directory carries the same hash in its
//! `.cargo-checksum.json` `package` field. A directory with the right name
//! but the wrong bytes is missing, not covered.
//!
//! Like every reader here this module is line-oriented: no JSON or TOML crate
//! is taken in order to police dependencies.

use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::lock;

// ── Report ──────────────────────────────────────────────────────────────────

/// One lock package with no matching bytes in the tree.
///
/// Non-exhaustive: the report compares on name and version, so a diagnostic
/// field added later is an additive change for consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Missing {
    /// Package name exactly as `Cargo.lock` spells it.
    pub name: String,
    /// Resolved version.
    pub version: String,
}

/// The coverage verdict for one repository.
///
/// The three fields partition the lock's non-empty package list: `covered`
/// plus `missing.len()` is every non-local package, and `skipped_local` is
/// every local one. A report is only a pass when `missing` is empty.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Report {
    /// Lock registry packages matched by hash (or by manifest for sourceless hashes).
    pub covered: usize,
    /// Local packages skipped: workspace members and path dependencies.
    pub skipped_local: usize,
    /// Lock packages with no matching bytes in the tree.
    pub missing: Vec<Missing>,
}

// ── Errors ──────────────────────────────────────────────────────────────────

/// Why coverage could not be verified. Every variant is a refusal, not a
/// pass: a gate that passes when it cannot read its own inputs reports
/// success for the one condition it exists to catch.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum VendorError {
    /// The lock file could not be read.
    Lock(lock::LockError),
    /// The repo carries no vendored-sources directory to check against.
    NoTree {
        /// Config path that names no tree.
        config: PathBuf,
    },
    /// The tree directory itself could not be listed.
    TreeUnreadable {
        /// Tree root that could not be listed.
        tree: PathBuf,
        /// Underlying I/O cause.
        cause: String,
    },
    /// A tree directory carries no readable package hash.
    ChecksumUnreadable {
        /// Directory with the broken manifest.
        dir: PathBuf,
        /// What was wrong.
        cause: String,
    },
}

impl fmt::Display for VendorError {
    /// Each arm names the path that failed, so a refusal identifies the tree or
    /// config to repair rather than only the class of failure.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Lock(ref cause) => write!(f, "Cargo.lock: {cause}"),
            Self::NoTree { ref config } => write!(
                f,
                "{} names no [source.vendored-sources] directory — vendor check needs a tree",
                config.display()
            ),
            Self::TreeUnreadable {
                ref tree,
                ref cause,
            } => {
                write!(f, "cannot list {}: {cause}", tree.display())
            }
            Self::ChecksumUnreadable { ref dir, ref cause } => {
                write!(f, "{}: {cause}", dir.display())
            }
        }
    }
}

impl Error for VendorError {}

impl From<lock::LockError> for VendorError {
    fn from(cause: lock::LockError) -> Self {
        Self::Lock(cause)
    }
}

// ── Tree location ───────────────────────────────────────────────────────────

/// Reads the vendored-sources directory out of a cargo config: the same file
/// cargo itself uses for source replacement, so the check can never drift to
/// a tree cargo is not resolving.
fn tree_from_config(config: &Path) -> Result<Option<PathBuf>, VendorError> {
    let text = std::fs::read_to_string(config).map_err(|_| VendorError::NoTree {
        config: config.to_path_buf(),
    })?;
    let mut in_section = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line == "[source.vendored-sources]";
        } else if in_section && line.starts_with("directory") {
            let value = line
                .split_once('=')
                .map(|(_, value)| value.trim().trim_matches(['"', '\'']))
                .filter(|value| !value.is_empty())
                .ok_or_else(|| VendorError::NoTree {
                    config: config.to_path_buf(),
                })?;
            let directory = PathBuf::from(value);
            let directory = if directory.is_absolute() {
                directory
            } else {
                config
                    .parent()
                    .and_then(|parent| parent.parent())
                    .map(|root| root.join(directory))
                    .ok_or_else(|| VendorError::NoTree {
                        config: config.to_path_buf(),
                    })?
            };
            return Ok(Some(directory));
        }
    }
    Ok(None)
}

/// Locates the vendor tree a repository resolves: the `directory` of its
/// `[source.vendored-sources]`, relative entries resolved against the repo
/// root (the parent of `.cargo/`).
pub fn tree_for(repo: &Path) -> Result<PathBuf, VendorError> {
    let config = repo.join(".cargo/config.toml");
    match tree_from_config(&config)? {
        Some(tree) => match std::fs::canonicalize(&tree) {
            Ok(canonical) => Ok(canonical),
            Err(_) => Ok(tree),
        },
        None => Err(VendorError::NoTree { config }),
    }
}

// ── Tree index ──────────────────────────────────────────────────────────────

/// Extracts the `"package"` hash from a `.cargo-checksum.json` by key search.
/// The file is machine-written (`cargo vendor` format), so locating one
/// `"package": "<hex>"` pair needs no general parser. Anything else is a
/// broken tree, refused rather than guessed at.
fn package_hash(checksum_file: &Path) -> Result<String, String> {
    let text =
        std::fs::read_to_string(checksum_file).map_err(|cause| format!("unreadable: {cause}"))?;
    let key = "\"package\"";
    // Splitting on the key takes everything after its first occurrence, which
    // is exactly the tail `find` plus `key.len()` would have addressed, with
    // no index arithmetic to overflow.
    let (_, tail) = text
        .split_once(key)
        .ok_or_else(|| "no \"package\" hash".to_owned())?;
    let rest = tail.trim_start();
    let rest = rest
        .strip_prefix(':')
        .ok_or_else(|| "no \"package\" hash".to_owned())?;
    let rest = rest.trim_start();
    let rest = rest
        .strip_prefix('"')
        .ok_or_else(|| "no \"package\" hash".to_owned())?;
    let end = rest
        .find('"')
        .ok_or_else(|| "no \"package\" hash".to_owned())?;
    let hash = rest[..end].to_string();
    if hash.is_empty() {
        return Err("no \"package\" hash".to_owned());
    }
    Ok(hash)
}

/// Reads a vendored `Cargo.toml` `[package]` name and version by line scan.
/// Registry manifests are standalone (no workspace inheritance), so the
/// first `name`/`version` pair under `[package]` is authoritative.
fn manifest_identity(manifest: &Path) -> Option<(String, String)> {
    let text = std::fs::read_to_string(manifest).ok()?;
    let mut in_package = false;
    let (mut name, mut version) = (None, None);
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
        } else if in_package && let Some((key, value)) = line.split_once('=') {
            let value = value.trim().trim_matches('"').to_owned();
            match key.trim() {
                "name" if name.is_none() => name = Some(value),
                "version" if version.is_none() => version = Some(value),
                _ => {}
            }
        }
        if name.is_some() && version.is_some() {
            break;
        }
    }
    name.zip(version)
}

/// Indexes the tree once: package-hash to directory, plus manifest
/// identities for the hashless (git-source) fallback.
struct Index {
    /// `.cargo-checksum.json` `"package"` sha256 to the directory carrying it.
    /// This is the binding that matters: a directory whose name matches but
    /// whose bytes differ hashes differently, so it is absent here and the
    /// package is reported missing rather than covered.
    by_hash: std::collections::HashMap<String, PathBuf>,
    /// `(name, version)` from the vendored `Cargo.toml` to its directory.
    /// Consulted only for lock packages that carry no checksum: git sources,
    /// which Cargo pins by revision rather than by `.crate` hash.
    by_name_version: std::collections::HashMap<(String, String), PathBuf>,
}

/// Walks `tree` once and builds the hash and identity indexes.
///
/// Directories are visited in sorted order so a duplicate hash resolves
/// deterministically rather than by filesystem enumeration order. A directory
/// with no readable `"package"` hash is refused, not skipped: a partial index
/// would silently turn unreadable bytes into a pass, which is the condition
/// this check exists to catch.
fn index_tree(tree: &Path) -> Result<Index, VendorError> {
    let entries = std::fs::read_dir(tree).map_err(|cause| VendorError::TreeUnreadable {
        tree: tree.to_path_buf(),
        cause: cause.to_string(),
    })?;
    let mut index = Index {
        by_hash: std::collections::HashMap::new(),
        by_name_version: std::collections::HashMap::new(),
    };
    let mut dirs: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let dir = entry
            .map(|entry| entry.path())
            .map_err(|cause| VendorError::TreeUnreadable {
                tree: tree.to_path_buf(),
                cause: cause.to_string(),
            })?;
        if dir.is_dir() {
            dirs.push(dir);
        }
    }
    dirs.sort();
    for dir in dirs {
        let hash = package_hash(&dir.join(".cargo-checksum.json")).map_err(|cause| {
            VendorError::ChecksumUnreadable {
                dir: dir.clone(),
                cause,
            }
        })?;
        index.by_hash.insert(hash, dir.clone());
        if let Some(identity) = manifest_identity(&dir.join("Cargo.toml")) {
            index.by_name_version.insert(identity, dir);
        }
    }
    Ok(index)
}

// ── The audit ───────────────────────────────────────────────────────────────

/// Verifies every non-local package in a lockfile against a vendor tree.
/// Hashless non-local packages (git sources) fall back to manifest identity;
/// local packages are out of scope and counted, never passed silently.
pub fn check_coverage(lock_text: &str, tree: &Path) -> Result<Report, VendorError> {
    let resolved = lock::parse(lock_text)?;
    let index = index_tree(tree)?;
    let mut report = Report {
        covered: 0,
        skipped_local: 0,
        missing: Vec::new(),
    };
    for package in &resolved {
        if package.local {
            // Both counters are bounded by `resolved.len()`, and a `Vec` cannot
            // exceed `isize::MAX` bytes, so neither can reach `usize::MAX` and
            // this cannot saturate.
            report.skipped_local = report.skipped_local.saturating_add(1);
            continue;
        }
        let covered = match package.checksum.as_deref() {
            Some(hash) => index.by_hash.contains_key(hash),
            None => index
                .by_name_version
                .contains_key(&(package.name.clone(), package.version.clone())),
        };
        if covered {
            report.covered = report.covered.saturating_add(1);
        } else {
            report.missing.push(Missing {
                name: package.name.clone(),
                version: package.version.clone(),
            });
        }
    }
    // Sorted so the refusal message and the exit code are stable across runs
    // and across platforms, whatever order the lockfile listed packages in.
    report
        .missing
        .sort_by(|left, right| (&left.name, &left.version).cmp(&(&right.name, &right.version)));
    Ok(report)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const LOCK: &str = r#"
version = 4

[[package]]
name = "workspace-member"
version = "0.1.0"

[[package]]
name = "covered-crate"
version = "1.2.3"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "aaaabbbbccccddddeeeeffff0000111122223333444455556666777788889999"

[[package]]
name = "missing-crate"
version = "4.5.6"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "ffffeeeeddddccccbbbbaaaa9999888877776666555544443333222211110000"

[[package]]
name = "git-crate"
version = "0.0.0"
source = "git+https://example.com/org/git-crate#abc123"
"#;

    /// Test bodies propagate with `?` rather than panicking: `unwrap` is
    /// forbidden workspace-wide, and a refused input should surface as the
    /// `VendorError` it is, not as a panic with no variant attached.
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        /// Creates a throwaway tree holding one hash-matched crate and one
        /// hashless git crate, plus a `.cargo-checksum.json` for each.
        ///
        /// The root is unique per call via pid and nanosecond timestamp so
        /// concurrent test binaries cannot share a directory. The `Drop` impl
        /// removes it, so a failing assertion cannot leak temp state.
        fn create() -> Result<Fixture, Box<dyn std::error::Error>> {
            let root = std::env::temp_dir().join(format!(
                "lgwks-deps-vendor-test-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|elapsed| elapsed.subsec_nanos())
                    .unwrap_or(0)
            ));
            std::fs::create_dir_all(root.join("tree/covered-crate"))?;
            std::fs::write(
                root.join("tree/covered-crate/.cargo-checksum.json"),
                "{\"files\": {}, \"package\": \"aaaabbbbccccddddeeeeffff0000111122223333444455556666777788889999\"}\n",
            )?;
            std::fs::write(
                root.join("tree/covered-crate/Cargo.toml"),
                "[package]\nname = \"covered-crate\"\nversion = \"1.2.3\"\n",
            )?;
            std::fs::create_dir_all(root.join("tree/git-crate"))?;
            std::fs::write(
                root.join("tree/git-crate/.cargo-checksum.json"),
                "{\"files\": {}, \"package\": \"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"}\n",
            )?;
            std::fs::write(
                root.join("tree/git-crate/Cargo.toml"),
                "[package]\nname = \"git-crate\"\nversion = \"0.0.0\"\n",
            )?;
            Ok(Fixture { root })
        }

        /// The `tree/` directory inside the fixture root.
        fn tree(&self) -> PathBuf {
            self.root.join("tree")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).ok();
        }
    }

    #[test]
    fn hash_matched_packages_are_covered_and_locals_skipped() -> TestResult {
        let fixture = Fixture::create()?;
        let report = check_coverage(LOCK, &fixture.tree())?;
        assert_eq!(report.covered, 2);
        assert_eq!(report.skipped_local, 1);
        assert_eq!(
            report.missing,
            vec![Missing {
                name: "missing-crate".to_owned(),
                version: "4.5.6".to_owned(),
            }]
        );
        Ok(())
    }

    #[test]
    fn a_tree_directory_without_a_package_hash_is_refused() -> TestResult {
        let fixture = Fixture::create()?;
        std::fs::create_dir_all(fixture.tree().join("broken-crate"))?;
        std::fs::write(
            fixture.tree().join("broken-crate/.cargo-checksum.json"),
            "{\"files\": {}}\n",
        )?;
        let Err(error) = check_coverage(LOCK, &fixture.tree()) else {
            return Err("a hashless tree directory must be refused, not reported covered".into());
        };
        assert!(matches!(error, VendorError::ChecksumUnreadable { .. }));
        Ok(())
    }

    #[test]
    fn a_missing_tree_is_refused_not_passed() -> TestResult {
        let fixture = Fixture::create()?;
        let Err(error) = check_coverage(LOCK, &fixture.root.join("no-such-tree")) else {
            return Err("an unlistable tree must be refused, not reported covered".into());
        };
        assert!(matches!(error, VendorError::TreeUnreadable { .. }));
        Ok(())
    }

    #[test]
    fn tree_for_reads_the_same_config_cargo_resolves() -> TestResult {
        let fixture = Fixture::create()?;
        std::fs::create_dir_all(fixture.root.join("repo/.cargo"))?;
        std::fs::write(
            fixture.root.join("repo/.cargo/config.toml"),
            "[source.crates-io]\nreplace-with = \"vendored-sources\"\n\n[source.vendored-sources]\ndirectory = \"../tree\"\n",
        )?;
        assert_eq!(
            tree_for(&fixture.root.join("repo"))?,
            fixture.tree().canonicalize()?
        );
        Ok(())
    }

    #[test]
    fn tree_for_without_a_vendored_section_is_refused() -> TestResult {
        let fixture = Fixture::create()?;
        std::fs::create_dir_all(fixture.root.join("bare/.cargo"))?;
        std::fs::write(
            fixture.root.join("bare/.cargo/config.toml"),
            "[net]\noffline = true\n",
        )?;
        assert!(matches!(
            tree_for(&fixture.root.join("bare")),
            Err(VendorError::NoTree { .. })
        ));
        Ok(())
    }

    #[test]
    fn tree_for_without_any_config_is_refused() -> TestResult {
        let fixture = Fixture::create()?;
        assert!(matches!(
            tree_for(&fixture.root.join("no-config-here")),
            Err(VendorError::NoTree { .. })
        ));
        Ok(())
    }
}
