//! CLI acceptance for the ERROR-SWALLOW evidence predicate.
//!
//! Issues #48 and #49 are one question asked from two directions — what is the
//! evidence that a bound error value was actually used — so both directions are
//! asserted through the same entry point the gate uses, the `scan` subcommand,
//! and over sources rather than bare snippets.

#![cfg(feature = "scan")]

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

/// A uniquely named scratch directory under the system temp directory.
///
/// The caller removes it. A directory left behind by a failed assertion is
/// harmless and cannot be read as another test's fixture, because the name
/// belongs to one test and the next run clears it before writing.
fn scratch(label: &str) -> Result<PathBuf, Box<dyn Error>> {
    let path = std::env::temp_dir().join(format!("lgwks-deps-scan-{label}"));
    if path.exists() {
        std::fs::remove_dir_all(&path)?;
    }
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

/// Runs the scanner over a path through the CLI the gate invokes.
fn run_scan(target: &Path) -> Result<Output, Box<dyn Error>> {
    let binary = std::env::var_os("CARGO_BIN_EXE_lgwks-deps")
        .ok_or("Cargo did not provide the lgwks-deps test binary")?;
    Ok(Command::new(binary)
        .args(["scan", target.to_str().ok_or("scratch path is not UTF-8")?])
        .output()?)
}

#[test]
fn cli_clears_the_option_and_infallible_controls() -> TestResult {
    let root = scratch("infallible-controls")?;
    std::fs::write(root.join("controls.rs"), INFALLIBLE_CONTROLS)?;
    let output = run_scan(&root)?;
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "the infallible controls must scan clean: stdout {stdout} stderr {stderr}"
    );
    assert!(stdout.contains("scan clean"), "{stdout}");
    std::fs::remove_dir_all(&root)?;
    Ok(())
}

#[test]
fn cli_reports_the_lost_errors_and_their_evidence() -> TestResult {
    let root = scratch("lost-errors")?;
    std::fs::write(root.join("lost.rs"), LOST_ERRORS)?;
    let output = run_scan(&root)?;
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;
    assert_eq!(
        output.status.code(),
        Some(2),
        "a lost error must fail the gate: stdout {stdout} stderr {stderr}"
    );
    assert!(stderr.contains("REFUSED"), "{stderr}");
    assert!(stdout.contains("[ERROR-SWALLOW]"), "{stdout}");
    assert!(
        stdout.contains("the `Result` returned by fn `read_count`"),
        "the Result default states its witness: {stdout}"
    );
    assert!(
        stdout.contains(".unwrap_or_else("),
        "the shadowed fallback is reported: {stdout}"
    );
    std::fs::remove_dir_all(&root)?;
    Ok(())
}
