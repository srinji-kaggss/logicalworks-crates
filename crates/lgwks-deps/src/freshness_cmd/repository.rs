//! A package's upstream repository, from the copy Cargo already unpacked.
//!
//! The sparse index records versions, not where a crate lives, and asking the
//! registry's API for it is a second request per crate behind the rate limit the
//! first one is built to stay under. Cargo keeps every crate it has built in
//! `$CARGO_HOME/registry/src/<index>/<name>-<version>/`, and the `Cargo.toml`
//! there names the repository the publisher declared. A crate Cargo has not
//! unpacked on this machine has no such file, and its repository is reported as
//! not known rather than looked up.

use std::path::{Path, PathBuf};

/// Where Cargo unpacks registry sources: `$CARGO_HOME/registry/src`, with
/// `CARGO_HOME` defaulting to `~/.cargo` as Cargo's own default does.
pub(super) fn registry_sources() -> Option<PathBuf> {
    let home = match std::env::var_os("CARGO_HOME") {
        Some(home) if !home.is_empty() => PathBuf::from(home),
        _ => {
            let user = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
            Path::new(&user).join(".cargo")
        }
    };
    Some(home.join("registry").join("src"))
}

/// The repository named by `name` at `version` in any registry under `sources`.
///
/// `sources` holds one directory per index Cargo has used, and the same package
/// can be unpacked under more than one of them. They are read in name order so
/// the answer does not depend on directory order.
pub(super) fn cached(sources: &Path, name: &str, version: &str) -> Option<String> {
    let mut indexes: Vec<PathBuf> = std::fs::read_dir(sources)
        .ok()?
        .filter_map(|entry| entry.ok().map(|found| found.path()))
        .collect();
    indexes.sort();
    let unpacked = format!("{name}-{version}");
    indexes.iter().find_map(|index| {
        let manifest = std::fs::read_to_string(index.join(&unpacked).join("Cargo.toml")).ok()?;
        repository_of(&manifest)
    })
}

/// The `repository` of a manifest's `[package]` table, when it is a plain
/// string.
///
/// This is a reader for the one key of a normalised manifest, not a TOML
/// parser. A value with an escape in it is refused rather than half-read: a
/// repository that is not the exact string the publisher wrote is not worth
/// printing.
fn repository_of(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package = trimmed == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        let Some(rest) = trimmed.strip_prefix("repository") else {
            continue;
        };
        let Some(value) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let (text, tail) = value.trim().strip_prefix('"')?.split_once('"')?;
        let tail = tail.trim();
        let clean =
            !text.is_empty() && !text.contains('\\') && (tail.is_empty() || tail.starts_with('#'));
        return clean.then(|| text.to_owned());
    }
    None
}

#[cfg(test)]
mod sim_repository;
