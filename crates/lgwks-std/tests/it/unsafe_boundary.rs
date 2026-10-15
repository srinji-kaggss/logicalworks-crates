//! The boundary around the one `unsafe` module (#345, Director 2026-10-12).
//!
//! `lgwks_std` carries its own copy of the workspace lint table because Cargo
//! cannot lower one inherited lint, and the copy differs in exactly one entry:
//! `unsafe_code = "allow"`, for the macOS `sysctl` syscall in
//! `src/process/darwin.rs`. The compiler no longer refuses `unsafe` anywhere in
//! this crate, so these two tests are what keep the exception to that file:
//! the table cannot drift from the workspace's, and no other source file may
//! write `unsafe`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The repository root, two levels above this crate's manifest.
fn workspace() -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or("crates/lgwks-std has no workspace above it")?
        .to_path_buf())
}

/// Every `name = "level"` entry under the tables whose header starts with
/// `prefix`, keyed by `<table suffix>.<name>`.
///
/// The manifests write each lint as one `name = "level"` line under a
/// `[...lints.<group>]` header, so a line reader is the whole grammar needed.
fn lint_entries(manifest: &str, prefix: &str) -> BTreeMap<String, String> {
    let mut entries = BTreeMap::new();
    let mut group: Option<String> = None;
    for line in manifest.lines().map(str::trim) {
        if let Some(header) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            group = header.strip_prefix(prefix).map(str::to_owned);
            continue;
        }
        let Some(group) = group.as_deref() else {
            continue;
        };
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some((name, level)) = line.split_once('=') {
            entries.insert(
                format!("{group}.{}", name.trim()),
                level.trim().trim_matches('"').to_owned(),
            );
        }
    }
    entries
}

/// Every entry equals the workspace's but `rust.unsafe_code`, which is the
/// one admitted difference; no entry is missing and none is added.
#[test]
fn lint_table_matches_the_workspace_but_for_unsafe() -> Result<(), Box<dyn std::error::Error>> {
    let root = workspace()?;
    let mut expected = lint_entries(
        &std::fs::read_to_string(root.join("Cargo.toml"))?,
        "workspace.lints.",
    );
    let mut actual = lint_entries(
        &std::fs::read_to_string(root.join("crates/lgwks-std/Cargo.toml"))?,
        "lints.",
    );
    assert!(
        expected.len() > 60,
        "the workspace table was read whole: {} entries",
        expected.len()
    );
    assert_eq!(
        expected.remove("rust.unsafe_code").as_deref(),
        Some("forbid"),
        "the workspace forbids unsafe everywhere else"
    );
    assert_eq!(
        actual.remove("rust.unsafe_code").as_deref(),
        Some("allow"),
        "lgwks_std admits the darwin syscall module"
    );
    assert_eq!(
        actual, expected,
        "every other lint in lgwks_std equals the workspace's"
    );
    Ok(())
}

/// Every `.rs` file under `dir`, recursively.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            rust_files(&path, out)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
    Ok(())
}

/// The spellings that open `unsafe` code: a block, a function, an extern
/// block, an impl, or a trait.
const UNSAFE_OPENERS: [&str; 5] = [
    "unsafe {",
    "unsafe fn",
    "unsafe extern",
    "unsafe impl",
    "unsafe trait",
];

/// The attribute that makes the compiler refuse `unsafe` code in a file, so a
/// file carrying it may hold the word only as text (the allocation probes
/// write `unsafe` into a downstream package they build separately).
const FORBIDS_UNSAFE: &str = "#![forbid(unsafe_code)]";

/// No source or test file of this crate writes `unsafe` except the darwin
/// syscall module and its own tests, and a file that forbids `unsafe` code
/// itself is held to that by the compiler.
#[test]
fn only_the_darwin_syscall_module_uses_unsafe() -> Result<(), Box<dyn std::error::Error>> {
    let crate_dir = workspace()?.join("crates/lgwks-std");
    let admitted = [
        crate_dir.join("src/process/darwin.rs"),
        crate_dir.join("src/process/darwin/tests.rs"),
    ];
    let mut files = Vec::new();
    rust_files(&crate_dir.join("src"), &mut files)?;
    rust_files(&crate_dir.join("tests"), &mut files)?;
    assert!(
        files.len() > 20,
        "the crate's sources were walked: {}",
        files.len()
    );
    let mut offenders = Vec::new();
    for file in files.iter().filter(|file| !admitted.contains(file)) {
        // This file names the spellings it looks for, as data.
        if file.ends_with("tests/it/unsafe_boundary.rs") {
            continue;
        }
        let text = std::fs::read_to_string(file)?;
        if !text.contains(FORBIDS_UNSAFE)
            && UNSAFE_OPENERS.iter().any(|opener| text.contains(opener))
        {
            offenders.push(file.display().to_string());
        }
    }
    assert!(
        offenders.is_empty(),
        "unsafe outside src/process/darwin.rs: {offenders:?}"
    );
    let darwin = std::fs::read_to_string(&admitted[0])?;
    assert!(
        darwin.contains("unsafe extern \"C\""),
        "the admitted module is the one that declares the syscall"
    );
    Ok(())
}
