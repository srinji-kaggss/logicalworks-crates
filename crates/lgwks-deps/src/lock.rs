//! `lock` owns reading `Cargo.lock` and enforces INV-LOCK-RESOLVED-TRUTH: the
//! gate audits the *resolved* graph, not the declared one, so a crate that
//! arrives only as somebody else's transitive dependency is still audited.
//!
//! The reader is line-oriented on purpose. Taking a TOML parser as a dependency
//! in order to police dependencies would be self-refuting, and the subset
//! `Cargo.lock` uses — `[[package]]` blocks of `key = "value"` pairs — needs no
//! general parser. Anything outside that subset is skipped rather than guessed
//! at, and a `[[package]]` block missing a name is reported, never silently
//! dropped.
//!
//! A package with no `source` key is local: a workspace member or a path
//! dependency. That is Cargo's own encoding of "this came from the filesystem,
//! not a registry", and it is what separates the crates the estate wrote from
//! the crates it took.

// ── The resolved package ────────────────────────────────────────────────────

/// One entry from the resolved dependency graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// Package name exactly as `Cargo.lock` spells it.
    pub name: String,
    /// Resolved version.
    pub version: String,
    /// True when the package has no `source` key, meaning Cargo resolved it
    /// from the filesystem — a workspace member or a path dependency.
    pub local: bool,
}

/// A `[[package]]` block that could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockError {
    /// A package block declared no `name`.
    NamelessPackage {
        /// Line where the offending block opened.
        line: usize,
    },
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NamelessPackage { line } => {
                write!(f, "[[package]] block at line {line} has no name")
            }
        }
    }
}

impl std::error::Error for LockError {}

// ── Reading ─────────────────────────────────────────────────────────────────

#[derive(Default)]
struct Pending {
    opened_at: usize,
    name: Option<String>,
    version: Option<String>,
    has_source: bool,
    open: bool,
}

fn handle_header(
    line: &str,
    index: usize,
    pending: &mut Pending,
    out: &mut Vec<Resolved>,
) -> Result<(), LockError> {
    flush(pending, out)?;
    *pending = Pending {
        opened_at: index + 1,
        open: line == "[[package]]",
        ..Pending::default()
    };
    Ok(())
}

fn apply_key_value(key: &str, value: &str, pending: &mut Pending) {
    match key {
        "name" => pending.name = Some(value.to_string()),
        "version" => pending.version = Some(value.to_string()),
        "source" => pending.has_source = true,
        _ => {}
    }
}

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
        apply_key_value(key, value, pending);
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

fn extract_version(pending: &mut Pending) -> String {
    // If a package has no version declared in lockfile, default to empty string.
    pending.version.take().unwrap_or_default()
}

fn flush(pending: &mut Pending, out: &mut Vec<Resolved>) -> Result<(), LockError> {
    if !pending.open {
        return Ok(());
    }
    let name = pending.name.take().ok_or(LockError::NamelessPackage {
        line: pending.opened_at,
    })?;
    let version = extract_version(pending);
    out.push(Resolved {
        name,
        version,
        local: !pending.has_source,
    });
    pending.open = false;
    Ok(())
}

fn valid_toml_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
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

    #[test]
    fn an_empty_lock_resolves_to_nothing() {
        assert_eq!(parse("").unwrap(), Vec::<Resolved>::new());
    }

    #[test]
    fn the_top_level_version_key_is_not_read_as_a_package() {
        assert_eq!(parse("version = 4\n").unwrap(), Vec::<Resolved>::new());
    }

    #[test]
    fn a_package_without_a_source_is_local() {
        let pkgs = parse(SAMPLE).unwrap();
        let local = pkgs.iter().find(|p| p.name == "lgwks_std").unwrap();
        assert!(local.local);
        assert_eq!(local.version, "0.1.0");
    }

    #[test]
    fn a_registry_package_carries_its_resolved_version() {
        let pkgs = parse(SAMPLE).unwrap();
        let serde = pkgs.iter().find(|p| p.name == "serde").unwrap();
        assert!(!serde.local);
        assert_eq!(serde.version, "1.0.219");
    }

    #[test]
    fn a_nameless_package_block_is_refused() {
        let input = "[[package]]\nversion = \"1.0.0\"\n";
        assert_eq!(parse(input), Err(LockError::NamelessPackage { line: 1 }));
    }

    #[test]
    fn a_v1_metadata_table_is_ignored() {
        let input = r#"
[[package]]
name = "lgwks_std"
version = "0.1.0"

[metadata]
"checksum foo 0.1.0 (registry+https://...)" = "abc123"
"#;
        let pkgs = parse(input).unwrap();
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "lgwks_std");
    }
}
