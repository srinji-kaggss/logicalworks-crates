//! `lgwks-ast scan`, driven as the gate drives it (INV-SCAN-ZERO, #408).
//!
//! Issues #48 and #49 are one question asked from two directions (what is the
//! evidence that a bound error value was actually used), so both are asserted
//! through the entry point the gate uses, over sources rather than bare
//! snippets. The rest is what a gate must hold: a refused file is counted and
//! the files after it are still read, build output and vendored trees are not
//! scanned, an agent gets one JSON document, and the exit status is the verdict.

#![cfg(all(feature = "scan", feature = "tool"))]

use std::error::Error;
use std::path::Path;
use std::process::Output;

use lgwks_std::json::Value;

use crate::cli::{plant, text, tool};
use crate::scratch::Scratch;

type TestResult = Result<(), Box<dyn Error>>;

/// Issue #48's two literal false-positive sources, together in one file.
/// Neither value carries an error: `None` on an `Option` means "use zero", and
/// a literal has no error to lose.
const INFALLIBLE_CONTROLS: &str = r#"pub fn default_count(value: Option<usize>) -> usize {
    value.unwrap_or_default()
}

pub fn discard_number() {
    let _ = 7usize;
}
"#;

/// Issue #49's two complete sources, plus a `Result` default: the shadowing
/// fallback that used to be missed, and the unshadowed control that was always
/// reported, in one file so both directions are read from one run.
const LOST_ERRORS: &str = r#"pub fn read_count() -> Result<usize, E> {
    Ok(1)
}

pub fn count() -> usize {
    read_count().unwrap_or_default()
}

pub fn load(path: &str) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|_error| {
        let _error = Vec::new();
        _error
    })
}

pub fn load_plain(path: &str) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|_error| Vec::new())
}
"#;

/// Run `lgwks-ast scan` with `args` in `dir`.
fn scan(dir: &Path, args: &[&str]) -> Result<Output, Box<dyn Error>> {
    let mut words = vec!["scan"];
    words.extend_from_slice(args);
    tool(dir, &words)
}

#[test]
fn cli_clears_the_option_and_infallible_controls() -> TestResult {
    let root = Scratch::new("scan-infallible-controls")?;
    plant(root.path(), "controls.rs", INFALLIBLE_CONTROLS)?;
    let output = scan(root.path(), &[])?;
    let (stdout, stderr) = text(&output);
    assert_eq!(
        output.status.code(),
        Some(0),
        "the infallible controls must scan clean: stdout {stdout} stderr {stderr}"
    );
    assert!(stdout.contains("scan clean"), "{stdout}");
    Ok(())
}

#[test]
fn cli_reports_the_lost_errors_and_their_evidence() -> TestResult {
    let root = Scratch::new("scan-lost-errors")?;
    plant(root.path(), "lost.rs", LOST_ERRORS)?;
    let output = scan(root.path(), &[])?;
    let (stdout, stderr) = text(&output);
    assert_eq!(
        output.status.code(),
        Some(1),
        "findings on a tree every file of which was read exit 1 (#698): stdout {stdout} stderr {stderr}"
    );
    assert!(stderr.contains("REFUSED"), "{stderr}");
    assert!(stdout.contains("[ERROR-SWALLOW]"), "{stdout}");
    assert!(
        stdout.contains("lost.rs:6: [ERROR-SWALLOW]"),
        "a finding is path:line: [rule]: {stdout}"
    );
    assert!(
        stdout.contains("the `Result` returned by fn `read_count`"),
        "the Result default states its witness: {stdout}"
    );
    assert!(
        stdout.contains(".unwrap_or_else("),
        "the shadowed fallback is reported: {stdout}"
    );
    Ok(())
}

/// The status is the verdict, so the two failures have two codes: a tree the
/// scan read and refused is 1, and a scan that could not read part of the tree
/// is 2, with or without findings beside it (#698).
#[test]
fn the_exit_status_separates_a_finding_from_a_file_that_could_not_be_read() -> TestResult {
    let root = Scratch::new("scan-verdicts")?;
    plant(root.path(), "a_lost.rs", LOST_ERRORS)?;
    let findings_only = scan(root.path(), &[])?;
    let (stdout, stderr) = text(&findings_only);
    assert_eq!(findings_only.status.code(), Some(1), "{stdout} {stderr}");
    assert!(
        !stderr.contains("could not be read"),
        "nothing was unread in this run: {stderr}"
    );

    plant(root.path(), "b_torn.rs", "fn torn(\n")?;
    let with_unread = scan(root.path(), &[])?;
    let (stdout, stderr) = text(&with_unread);
    assert_eq!(
        with_unread.status.code(),
        Some(2),
        "an unread file outranks the findings beside it: {stdout} {stderr}"
    );
    assert!(
        stderr.contains("UNMEASURED") && stderr.contains("1 of 2 files could not be read"),
        "the summary says what the status says: {stderr}"
    );
    assert!(
        stdout.contains("[ERROR-SWALLOW]"),
        "the findings the scan did read are still printed: {stdout}"
    );
    Ok(())
}

/// A tree the scan could not read at all, with no finding beside it, is still
/// 2: "not measured" is a verdict of its own and never 0.
#[test]
fn a_tree_with_only_an_unreadable_file_exits_two() -> TestResult {
    let root = Scratch::new("scan-unread-only")?;
    plant(root.path(), "a_torn.rs", "fn torn(\n")?;
    let output = scan(root.path(), &[])?;
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(2), "{stdout} {stderr}");
    assert!(stderr.contains("UNMEASURED"), "{stderr}");
    Ok(())
}

#[test]
fn a_refused_file_is_counted_and_the_files_after_it_are_still_read() -> TestResult {
    let root = Scratch::new("scan-refused-file")?;
    plant(root.path(), "a_torn.rs", "fn torn(\n")?;
    plant(root.path(), "b_lost.rs", LOST_ERRORS)?;
    let output = scan(root.path(), &[])?;
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(2), "{stdout} {stderr}");
    assert!(
        stderr.contains("scan error:") && stderr.contains("a_torn.rs"),
        "the unparseable file is named: {stderr}"
    );
    assert!(
        stdout.contains("b_lost.rs:6: [ERROR-SWALLOW]"),
        "the file after it was still scanned: {stdout}"
    );
    assert!(
        stderr.contains("UNMEASURED") && stderr.contains("1 of 2 files could not be read"),
        "the summary counts the refusal in the word the status carries: {stderr}"
    );
    Ok(())
}

#[test]
fn a_deeply_nested_file_is_refused_by_name_and_does_not_end_the_run() -> TestResult {
    let root = Scratch::new("scan-deep-file")?;
    plant(
        root.path(),
        "deep.rs",
        &format!(
            "fn f() {{ let _ = {}1{}; }}\n",
            "(".repeat(10_000),
            ")".repeat(10_000)
        ),
    )?;
    plant(root.path(), "fine.rs", INFALLIBLE_CONTROLS)?;
    let output = scan(root.path(), &[])?;
    let (stdout, stderr) = text(&output);
    assert_eq!(
        output.status.code(),
        Some(2),
        "a nest the parser refuses fails the gate and does not abort it: {stdout} {stderr}"
    );
    assert!(
        stderr.contains("deep.rs") && stderr.contains("nests deeper than 512 levels"),
        "the bound that refused it is named: {stderr}"
    );
    assert!(
        !stderr.contains("fine.rs"),
        "the clean file is not blamed: {stderr}"
    );
    Ok(())
}

#[test]
fn build_output_and_vendored_trees_are_not_scanned() -> TestResult {
    let root = Scratch::new("scan-skipped-trees")?;
    plant(root.path(), "src/lib.rs", INFALLIBLE_CONTROLS)?;
    plant(root.path(), "target/debug/build/out.rs", LOST_ERRORS)?;
    plant(root.path(), "vendor/dep/lib.rs", LOST_ERRORS)?;
    let output = scan(root.path(), &[])?;
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout} {stderr}");
    assert!(stdout.contains("1 files, zero findings"), "{stdout}");
    Ok(())
}

#[test]
fn the_json_document_carries_the_findings_and_the_refusals() -> TestResult {
    let root = Scratch::new("scan-json")?;
    plant(root.path(), "a_torn.rs", "fn torn(\n")?;
    plant(root.path(), "b_lost.rs", LOST_ERRORS)?;
    let output = scan(root.path(), &["--json"])?;
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(2), "{stdout} {stderr}");
    let document: Value = lgwks_std::json::from_str(&stdout)?;
    assert_eq!(document["verb"], "scan", "{stdout}");
    assert_eq!(document["verdict"], "unread", "{stdout}");
    assert_eq!(document["files"], 2, "{stdout}");
    assert_eq!(document["refused"], 1, "{stdout}");
    assert_eq!(document["findings"], 3, "{stdout}");
    let hits = document["hits"]
        .as_array()
        .ok_or("the document lists its hits")?;
    assert!(
        hits.iter().all(|hit| hit["rule"] == "ERROR-SWALLOW"),
        "every hit names its rule: {stdout}"
    );
    assert_eq!(
        document["unscanned"].as_array().map(Vec::len),
        Some(1),
        "the torn file is listed as unscanned: {stdout}"
    );
    Ok(())
}

#[test]
fn an_unknown_option_is_a_usage_error_and_scans_nothing() -> TestResult {
    let root = Scratch::new("scan-usage")?;
    plant(root.path(), "lost.rs", LOST_ERRORS)?;
    let output = scan(root.path(), &["--wat"])?;
    let (stdout, stderr) = text(&output);
    assert_eq!(
        output.status.code(),
        Some(64),
        "a command line the tool cannot read is EX_USAGE, never a verdict (#698): {stdout} {stderr}"
    );
    assert!(
        stderr.contains("unknown option `--wat`") && stdout.is_empty(),
        "a misuse prints the usage and no findings: {stdout} {stderr}"
    );
    Ok(())
}

#[test]
fn a_path_that_does_not_exist_is_refused_not_passed() -> TestResult {
    let root = Scratch::new("scan-missing")?;
    let output = scan(root.path(), &["nowhere.rs"])?;
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(2), "{stdout} {stderr}");
    assert!(stderr.contains("nowhere.rs"), "the path is named: {stderr}");
    Ok(())
}

/// One root that cannot be walked is one refusal, whichever readers the scan
/// owns: the tree is listed once and not once per reader (#699).
#[test]
fn one_unreadable_root_is_one_refusal() -> TestResult {
    let root = Scratch::new("scan-one-refusal")?;
    let output = scan(root.path(), &["nowhere"])?;
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(2), "{stdout} {stderr}");
    assert!(
        stderr.contains("1 of 1 files could not be read"),
        "one root, one refusal: {stderr}"
    );
    Ok(())
}

/// A source file named on the command line is read as Rust and as nothing else:
/// it is one file, not one for each reader the scan owns.
#[test]
fn a_named_source_file_is_not_read_as_a_manifest() -> TestResult {
    let root = Scratch::new("scan-named-source")?;
    plant(root.path(), "controls.rs", INFALLIBLE_CONTROLS)?;
    let output = scan(root.path(), &["controls.rs"])?;
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout} {stderr}");
    assert!(stdout.contains("1 files, zero findings"), "{stdout}");
    Ok(())
}

/// A manifest named on the command line is read for its lint tables and never
/// parsed as Rust, which it is not.
#[test]
fn a_named_manifest_is_read_as_a_manifest_and_not_as_rust() -> TestResult {
    let root = Scratch::new("scan-named-manifest")?;
    plant(
        root.path(),
        "Cargo.toml",
        "[lints.rust]\ndead_code = \"allow\"\n",
    )?;
    let output = scan(root.path(), &["Cargo.toml"])?;
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(1), "{stdout} {stderr}");
    assert!(
        stdout.contains("Cargo.toml:2: [ALLOW-SILENCE] [lints.rust] dead_code = \"allow\""),
        "{stdout}"
    );
    assert!(stderr.contains("across 1 files"), "{stderr}");
    Ok(())
}

/// Only ALLOW-SILENCE reads a manifest, so naming one while that rule is left
/// out has no detector to read it with: it is refused by name, never read as
/// Rust and never passed unread.
#[test]
fn a_named_manifest_without_allow_silence_is_refused_by_name() -> TestResult {
    let root = Scratch::new("scan-named-manifest-unselected")?;
    plant(
        root.path(),
        "Cargo.toml",
        "[lints.rust]\ndead_code = \"allow\"\n",
    )?;
    let output = scan(root.path(), &["--rule", "ERROR-SWALLOW", "Cargo.toml"])?;
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(2), "{stdout} {stderr}");
    assert!(
        stderr.contains("Cargo.toml") && stderr.contains("only ALLOW-SILENCE reads one"),
        "{stderr}"
    );
    Ok(())
}

/// A walk keeps a manifest only when ALLOW-SILENCE is selected, and keeps it
/// beside the Rust files in the one listing.
#[test]
fn a_walk_lists_manifests_once_and_only_for_allow_silence() -> TestResult {
    let root = Scratch::new("scan-walk-manifests")?;
    plant(root.path(), "Cargo.toml", "[package]\nname = \"x\"\n")?;
    plant(root.path(), "src/lib.rs", INFALLIBLE_CONTROLS)?;
    let everything = scan(root.path(), &[])?;
    let (stdout, stderr) = text(&everything);
    assert_eq!(everything.status.code(), Some(0), "{stdout} {stderr}");
    assert!(stdout.contains("2 files, zero findings"), "{stdout}");
    let narrowed = scan(root.path(), &["--rule", "ERROR-SWALLOW"])?;
    let (stdout, stderr) = text(&narrowed);
    assert_eq!(narrowed.status.code(), Some(0), "{stdout} {stderr}");
    assert!(stdout.contains("1 files, zero findings"), "{stdout}");
    Ok(())
}
