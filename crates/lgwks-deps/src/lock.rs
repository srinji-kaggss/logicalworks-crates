//! `lock` owns reading `Cargo.lock` and enforces INV-LOCK-RESOLVED-TRUTH: the
//! gate audits the *resolved* graph, not the declared one, so a crate that
//! arrives only as somebody else's transitive dependency is still audited.
//!
//! The reader is line-oriented on purpose. Taking a TOML parser as a dependency
//! in order to police dependencies would be self-refuting, and the subset
//! `Cargo.lock` uses (`[[package]]` blocks of `key = "value"` pairs) needs no
//! general parser. Anything outside that subset is skipped rather than guessed
//! at, and a `[[package]]` block missing a name is reported, never silently
//! dropped.
//!
//! A package with no `source` key is local: a workspace member or a path
//! dependency. That is Cargo's own encoding of "this came from the filesystem,
//! not a registry", and it is what separates crates written in this workspace
//! from crates taken from a registry.

// ── The resolved package ────────────────────────────────────────────────────

/// One entry from the resolved dependency graph.
///
/// Non-exhaustive: the audit reads `name`, `version`, and `local`, so a future
/// key Cargo adds to a package block is an additive change here.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Resolved {
    /// Package name exactly as `Cargo.lock` spells it.
    name: String,
    /// Resolved version.
    version: String,
    /// True when the package has no `source` key, meaning Cargo resolved it
    /// from the filesystem: a workspace member or a path dependency.
    local: bool,
    /// The `checksum` key when present: sha256 of the `.crate` file for
    /// registry packages. Absent for local packages and for git sources,
    /// which Cargo tracks by revision instead.
    checksum: Option<String>,
}

impl Resolved {
    /// Package name exactly as `Cargo.lock` spells it.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The version Cargo resolved this package to.
    ///
    /// A resolved package always has one: a block that declares no version is
    /// refused as [`LockError::VersionlessPackage`] rather than read with an
    /// invented one, because every comparison this value feeds is a comparison
    /// about the version the build actually resolved.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Whether Cargo resolved this package from the filesystem: a workspace
    /// member or a path dependency, which is a package with no `source` key.
    #[must_use]
    pub const fn is_local(&self) -> bool {
        self.local
    }

    /// The `checksum` key when present: sha256 of the `.crate` file for a
    /// registry package, and `None` for a local package and for a git source,
    /// which Cargo tracks by revision instead.
    #[must_use]
    pub fn checksum(&self) -> Option<&str> {
        self.checksum.as_deref()
    }
}

/// A `[[package]]` block that could not be read.
///
/// Non-exhaustive so a future refusal (a malformed quoted key, say) is
/// additive. Every variant is a refusal rather than a skip: shrinking the
/// resolved graph silently is the one failure this reader must never make.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LockError {
    /// A package block declared no `name`.
    NamelessPackage {
        /// Line where the offending block opened.
        line: usize,
    },
    /// A package block declared no `version`.
    ///
    /// Refused rather than read with an empty version: a resolved package is an
    /// identity of name *and* version, and a version that no line declared is a
    /// value every downstream comparison would then decide on. Cargo writes
    /// `version` for every locked package, so this is a hand-edited or truncated
    /// file, which is the shape this reader refuses everywhere else.
    VersionlessPackage {
        /// Line where the offending block opened.
        line: usize,
    },
    /// A package block assigned one of the keys the audit reads twice.
    ///
    /// Refused rather than resolved, because either answer is a guess: the
    /// first assignment is what a reader scanning from the top believes, the
    /// last is what a reader that overwrites believes, and an audit that picks
    /// one can be shown a different package than the build resolved.
    DuplicateKey {
        /// The repeated key.
        key: String,
        /// One-based line of the second assignment.
        line: usize,
    },
}

impl std::fmt::Display for LockError {
    /// Reports the one-based line of the opening header, so the refusal points
    /// at the block to repair rather than at the file.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::NamelessPackage { line } => {
                write!(f, "[[package]] block at line {line} has no name")
            }
            Self::VersionlessPackage { line } => {
                write!(f, "[[package]] block at line {line} has no version")
            }
            Self::DuplicateKey { ref key, line } => {
                write!(
                    f,
                    "line {line}: {key:?} is assigned twice in one [[package]] block"
                )
            }
        }
    }
}

impl std::error::Error for LockError {}

// ── Reading ─────────────────────────────────────────────────────────────────

/// The `[[package]]` block currently being accumulated.
///
/// A block runs from its header to the next header or to end of input, so
/// every field belongs to exactly one package: `flush` clears the whole value
/// by assigning a fresh one, and the `Default` impl is what makes that reset
/// complete. `open` is false before the first header and after a flush, which
/// is how the top-level `version = 4` key and a `[metadata]` table are ignored
/// instead of being read as package fields.
#[derive(Default)]
struct Pending {
    /// One-based line number of the opening `[[package]]` header, carried so a
    /// nameless block can be reported at its own line rather than at EOF.
    opened_at: usize,
    /// The `name` key. Absent until the block declares one; a block that ends
    /// without it is refused.
    name: Option<String>,
    /// The `version` key. A block that omits it is refused at its own line
    /// rather than reported with a version no line declared.
    version: Option<String>,
    /// Whether a `source` key was seen. Its absence is Cargo's encoding of a
    /// filesystem-resolved package, which is what `Resolved::local` reports.
    has_source: bool,
    /// The `checksum` key, present only for registry packages; git sources are
    /// tracked by revision and carry none.
    checksum: Option<String>,
    /// Whether the reader is inside a `[[package]]` block. False for top-level
    /// keys and for every other table.
    open: bool,
}

/// Closes the block in progress and opens a new one at `index`.
///
/// Every line starting with `[` reaches here, including headers that are not
/// `[[package]]`; such a header still flushes the block before it, then opens
/// one with `open == false` that `process_line` discards. `index` is the
/// zero-based position from `str::lines`, stored as a one-based line number.
fn handle_header(
    line: &str,
    index: usize,
    pending: &mut Pending,
    out: &mut Vec<Resolved>,
) -> Result<(), LockError> {
    flush(pending, out)?;
    *pending = Pending {
        // `index` counts lines already yielded by `text.lines()`, and a `&str`
        // is at most `isize::MAX` bytes with every line costing at least one
        // byte, so `index` cannot approach `usize::MAX` and this cannot
        // saturate.
        opened_at: index.saturating_add(1),
        open: line == "[[package]]",
        ..Pending::default()
    };
    Ok(())
}

/// Records one `key = "value"` pair into the block being read.
///
/// Unknown keys are ignored rather than refused: a package block also carries
/// `dependencies` rows and `[metadata]`-era keys, and the lock format is
/// Cargo's to extend, so refusing an unrecognised key would refuse a lockfile
/// Cargo itself wrote.
///
/// A second assignment of a key the audit reads is refused at its own line
/// (`line` is one-based): see [`LockError::DuplicateKey`].
fn apply_key_value(
    key: &str,
    value: &str,
    line: usize,
    pending: &mut Pending,
) -> Result<(), LockError> {
    let already = match key {
        "name" => pending.name.replace(value.to_owned()).is_some(),
        "version" => pending.version.replace(value.to_owned()).is_some(),
        "source" => core::mem::replace(&mut pending.has_source, true),
        "checksum" => pending.checksum.replace(value.to_owned()).is_some(),
        _ => false,
    };
    if already {
        let refusal = Err(LockError::DuplicateKey {
            key: key.to_owned(),
            line,
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "apply_key_value: a package key is assigned twice");
        return refusal;
    }
    Ok(())
}

/// Feeds one raw lockfile line into the reader.
///
/// A line starting with `[` is a header and always closes the current block.
/// Any other line is read as a field only while a `[[package]]` block is open,
/// which is what keeps the top-level `version = 4` key and a `[metadata]`
/// table out of the resolved graph.
fn process_line(
    index: usize,
    raw_line: &str,
    pending: &mut Pending,
    out: &mut Vec<Resolved>,
) -> Result<(), LockError> {
    let line = raw_line.trim();
    if line.starts_with('[') {
        handle_header(line, index, pending, out)?;
        return Ok(());
    }
    if !pending.open {
        return Ok(());
    }
    if let Some((key, value)) = key_and_value(line) {
        // `index` is zero-based and bounded by the input length (see
        // `handle_header`), so the one-based line cannot saturate.
        apply_key_value(key, value, index.saturating_add(1), pending)?;
    }
    Ok(())
}

/// Reads every `[[package]]` block out of a `Cargo.lock`.
pub fn parse(text: &str) -> Result<Vec<Resolved>, LockError> {
    let mut out = Vec::new();
    let mut pending = Pending::default();

    for (index, raw) in text.lines().enumerate() {
        process_line(index, raw, &mut pending, &mut out)?;
    }
    flush(&mut pending, &mut out)?;
    Ok(out)
}

/// Emits the block in progress and clears the accumulator.
///
/// A block that opened but never declared a `name` or a `version` is a hard
/// refusal at its own line: dropping it would shrink the audited graph, and
/// reading it with an invented version would put a value into the resolved
/// graph that no line declared. Both are the failures this reader exists to
/// prevent. A no-op when no block is open, so the trailing call at end of input
/// is safe.
fn flush(pending: &mut Pending, out: &mut Vec<Resolved>) -> Result<(), LockError> {
    if !pending.open {
        return Ok(());
    }
    let name = match pending.name.take() {
        Some(name) => name,
        None => {
            let refusal = Err(LockError::NamelessPackage {
                line: pending.opened_at,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "flush: a package block declares no name");
            return refusal;
        }
    };
    let version = match pending.version.take() {
        Some(version) => version,
        None => {
            let refusal = Err(LockError::VersionlessPackage {
                line: pending.opened_at,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "flush: a package block declares no version");
            return refusal;
        }
    };
    out.push(Resolved {
        name,
        version,
        local: !pending.has_source,
        checksum: pending.checksum.take(),
    });
    pending.open = false;
    Ok(())
}

/// Whether `key` is a bare TOML key the reader will accept.
///
/// Only bare keys are read. A quoted key, which is how a v1 `[metadata]` table
/// spells its `"checksum foo 0.1.0 (registry+…)"` rows, fails this test, so
/// those rows are skipped rather than mistaken for package fields.
fn valid_toml_key(key: &str) -> bool {
    !key.is_empty()
        && key.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
}

/// Splits `key = "value"` into its parts. Returns `None` for anything else,
/// including the quoted-key lines a v1 `[metadata]` table carries.
fn key_and_value(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.split_once('=')?;
    let key = key.trim();
    if !valid_toml_key(key) {
        return None;
    }
    let value = value.trim();
    let unquoted = value.strip_prefix('"')?.strip_suffix('"')?;
    Some((key, unquoted))
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
# This file is automatically @generated by Cargo.
version = 4

[[package]]
name = "lgwks_std"
version = "0.1.0"

[[package]]
name = "serde"
version = "1.0.219"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#;

    /// Test bodies propagate with `?` rather than panicking: `unwrap` is
    /// forbidden workspace-wide, and a parse failure should surface as the
    /// `LockError` it is, not as a panic with no variant attached.
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn an_empty_lock_resolves_to_nothing() -> TestResult {
        assert_eq!(parse("")?, Vec::<Resolved>::new());
        Ok(())
    }

    #[test]
    fn the_top_level_version_key_is_not_read_as_a_package() -> TestResult {
        assert_eq!(parse("version = 4\n")?, Vec::<Resolved>::new());
        Ok(())
    }

    #[test]
    fn a_package_without_a_source_is_local() -> TestResult {
        let pkgs = parse(SAMPLE)?;
        let local = pkgs
            .iter()
            .find(|package| package.name() == "lgwks_std")
            .ok_or("lgwks_std should be in the resolved graph")?;
        assert!(local.is_local());
        assert_eq!(local.version(), "0.1.0");
        Ok(())
    }

    #[test]
    fn a_registry_package_carries_its_resolved_version() -> TestResult {
        let pkgs = parse(SAMPLE)?;
        let serde = pkgs
            .iter()
            .find(|package| package.name() == "serde")
            .ok_or("serde should be in the resolved graph")?;
        assert!(!serde.is_local());
        assert_eq!(serde.version(), "1.0.219");
        Ok(())
    }

    #[test]
    fn a_checksum_key_is_captured_for_registry_packages() -> TestResult {
        let input = "[[package]]\nname = \"serde\"\nversion = \"1.0.219\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"abc123def456\"\n";
        let pkgs = parse(input)?;
        assert_eq!(pkgs[0].checksum(), Some("abc123def456"));
        Ok(())
    }

    #[test]
    fn a_package_without_a_checksum_reports_none() -> TestResult {
        let pkgs = parse(SAMPLE)?;
        let local = pkgs
            .iter()
            .find(|package| package.name() == "lgwks_std")
            .ok_or("lgwks_std should be in the resolved graph")?;
        assert_eq!(local.checksum(), None);
        Ok(())
    }

    #[test]
    fn a_nameless_package_block_is_refused() {
        let input = "[[package]]\nversion = \"1.0.0\"\n";
        assert_eq!(parse(input), Err(LockError::NamelessPackage { line: 1 }));
    }

    /// A block with a name and no version is not a resolved package: it is
    /// refused at its own line, where the version belongs, rather than read
    /// with an empty version every later comparison would then trust.
    #[test]
    fn a_versionless_package_block_is_refused() {
        let input = "[[package]]\nname = \"serde\"\n";
        assert_eq!(parse(input), Err(LockError::VersionlessPackage { line: 1 }));
    }

    #[test]
    fn a_v1_metadata_table_is_ignored() -> TestResult {
        let input = r#"
[[package]]
name = "lgwks_std"
version = "0.1.0"

[metadata]
"checksum foo 0.1.0 (registry+https://...)" = "abc123"
"#;
        let pkgs = parse(input)?;
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name(), "lgwks_std");
        Ok(())
    }
}
