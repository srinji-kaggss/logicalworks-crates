//! The documented `cargo run -p lgwks_deps -- <command>` runs one binary.
//!
//! `lgwks_deps` authors several binaries, and `cargo run` in a package with more
//! than one refuses to guess ("could not determine which binary to run") unless
//! the manifest's `default-run` names one (the Cargo reference, "The `default-run`
//! field"). Every doctrine page spells its commands without `--bin`:
//! `cargo run -p lgwks_deps -- check .`, `tiers` and `request`. When the gate
//! scripts became binaries (#537) the key was missing and each of those commands
//! failed. This holds the manifest to what the pages promise (INV-DEP-18); it
//! reads the manifest as text, so no cargo process runs.

/// This crate's own manifest, embedded so the test needs no file access.
const MANIFEST: &str = include_str!("../../Cargo.toml");

/// The binary the doctrine pages mean by a bare `cargo run -p lgwks_deps`.
const DOCUMENTED_BINARY: &str = "lgwks-deps";

/// The quoted value of a `key = "value"` line, when the line is that key.
fn string_value<'text>(line: &'text str, key: &str) -> Option<&'text str> {
    let (name, value) = line.split_once('=')?;
    if name.trim() != key {
        return None;
    }
    value.trim().strip_prefix('"')?.strip_suffix('"')
}

/// Every `key = "value"` string inside the tables headed `header`, in manifest
/// order. A table runs from its header to the next line that opens a table.
fn table_values<'text>(manifest: &'text str, header: &str, key: &str) -> Vec<&'text str> {
    let mut values = Vec::new();
    let mut inside = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            inside = line == header;
        } else if inside && let Some(value) = string_value(line, key) {
            values.push(value);
        }
    }
    values
}

#[test]
fn the_manifest_names_the_binary_the_documentation_runs() {
    let named = table_values(MANIFEST, "[package]", "default-run");
    assert_eq!(
        named,
        [DOCUMENTED_BINARY],
        "a bare `cargo run -p lgwks_deps` must run `{DOCUMENTED_BINARY}`"
    );
}

#[test]
fn the_default_binary_is_a_declared_binary() {
    let bins = table_values(MANIFEST, "[[bin]]", "name");
    assert!(
        bins.contains(&DOCUMENTED_BINARY),
        "default-run names `{DOCUMENTED_BINARY}`, which no [[bin]] declares: {bins:?}"
    );
}

#[test]
fn the_reader_takes_a_name_only_from_the_table_it_was_asked_for() {
    let sample = "\
[package]
name = \"crate\"
default-run = \"first\"
keywords = [\"a\", \"b\"]

[[bin]]
name = \"first\"

[[bin]]
name = unquoted

[[bin]]
name = \"second\"

[lib]
name = \"library\"
";
    assert_eq!(table_values(sample, "[[bin]]", "name"), ["first", "second"]);
    assert_eq!(table_values(sample, "[package]", "name"), ["crate"]);
    assert_eq!(table_values(sample, "[package]", "default-run"), ["first"]);
    assert_eq!(table_values(sample, "[lib]", "name"), ["library"]);
    assert!(table_values(sample, "[package]", "missing").is_empty());
}
