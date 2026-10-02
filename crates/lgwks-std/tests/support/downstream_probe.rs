//! Shared driver for downstream consumer probes.
//!
//! Several contracts need to observe behaviour a downstream integration test
//! cannot: a counting global allocator needs `unsafe`, which every integration
//! binary here forbids, and a foreign crate's behaviour needs a foreign crate.
//! Each such probe is therefore a standalone package built and run by a test.
//! That build-and-run sequence is identical for all of them, so it lives here
//! once and is called from each probe's test rather than repeated per file.
//!
//! This module is `#[path]`-included by the probe tests; it is not a test
//! binary of its own.

#![allow(dead_code, reason = "each including binary uses a different subset")]

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Builds and runs a downstream package and returns its standard output.
///
/// `main_source` is the package's `src/main.rs`. `features` selects optional
/// `lgwks_std` features; the default is no features beyond `default`.
///
/// The workspace target directory is shared so the probe reuses compiled
/// dependencies instead of rebuilding them, and `--offline` keeps the run from
/// reaching a registry.
pub fn run_downstream_probe(
    package_name: &str,
    main_source: &str,
    features: &[&str],
) -> Result<String, Box<dyn Error>> {
    let directory = tempfile::tempdir()?;
    fs::create_dir(directory.path().join("src"))?;
    let manifest = downstream_manifest(package_name, features);
    let manifest_path = directory.path().join("Cargo.toml");
    fs::write(&manifest_path, manifest)?;
    fs::write(directory.path().join("src/main.rs"), main_source)?;

    let cargo = |args: &[&str]| cargo_in(directory.path(), &manifest_path, args);
    let lock = cargo(&["generate-lockfile", "--offline"])?;
    assert!(
        lock.status.success(),
        "probe lock generation failed: {}",
        String::from_utf8_lossy(&lock.stderr)
    );
    let run = cargo(&["run", "--locked", "--offline", "--quiet"])?;
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    assert!(
        run.status.success(),
        "probe `{package_name}` failed: {stdout}{}",
        String::from_utf8_lossy(&run.stderr)
    );
    Ok(stdout)
}

/// Renders a manifest that depends on the workspace's own `lgwks_std`.
fn downstream_manifest(package_name: &str, features: &[&str]) -> String {
    let features = if features.is_empty() {
        String::new()
    } else {
        format!(", features = [{}]", quoted_list(features))
    };
    format!(
        "[package]\nname = {package_name:?}\nversion = \"0.1.0\"\nedition = \"2024\"\n\n\
         [dependencies]\nlgwks_std = {{ path = {:?}, default-features = false{features} }}\n",
        workspace_crate_path()
    )
}

/// The manifest directory of the `lgwks_std` crate under test.
fn workspace_crate_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// The shared workspace target directory, so probes reuse built dependencies.
fn shared_target_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target")
}

/// Renders a comma-separated quoted list.
fn quoted_list(values: &[&str]) -> String {
    values
        .iter()
        .map(|value| format!("{value:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Invokes cargo against a probe manifest with the shared target directory.
fn cargo_in(
    directory: &Path,
    manifest_path: &Path,
    args: &[&str],
) -> std::io::Result<std::process::Output> {
    Command::new(env!("CARGO"))
        .args(args)
        .arg("--manifest-path")
        .arg(manifest_path)
        .current_dir(directory)
        .env("CARGO_TARGET_DIR", shared_target_dir())
        .output()
}

/// Parses `<label> <integer>` lines from probe output into a lookup.
///
/// A probe reports its measurements as `label value` lines; this turns them
/// into a map so a test can assert on a named measurement without re-parsing
/// in every probe.
pub fn measurements(stdout: &str) -> Vec<(String, u64)> {
    stdout
        .lines()
        .filter_map(|line| {
            let (label, value) = line.rsplit_once(' ')?;
            let parsed = value.trim().parse::<u64>().ok()?;
            Some((label.trim().to_owned(), parsed))
        })
        .collect()
}

/// Returns the measurement named `label`, or `u64::MAX` when it is absent.
///
/// An absent measurement must fail the assertions that use it rather than
/// silently reading as zero, which is why the sentinel is `u64::MAX`.
pub fn measurement(stdout: &str, label: &str) -> u64 {
    let mut found = None;
    for (name, value) in measurements(stdout) {
        if name.as_str() == label {
            found = Some(value);
            break;
        }
    }
    found.unwrap_or(u64::MAX)
}
