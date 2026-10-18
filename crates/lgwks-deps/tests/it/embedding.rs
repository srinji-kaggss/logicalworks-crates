//! The embedding guide's flow and `declared_scope`'s two spellings, run as tests (#526).
//!
//! Both used to be doctests that named the crate by path; they are tests now, so each claim
//! runs under nextest with the rest of the suite.

use std::error::Error;
use std::path::{Path, PathBuf};

/// What every fallible test here returns.
type TestResult = Result<(), Box<dyn Error>>;

/// The workspace this crate is built in: the one tree whose register the gate must pass.
fn workspace_root() -> Result<PathBuf, Box<dyn Error>> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| "the crate manifest must be nested in the workspace".into())
}

#[test]
fn the_embedding_flow_reads_the_register_and_reports_refusals() -> TestResult {
    let root = workspace_root()?;
    let ((contract, refusals), unresolved) = lgwks_deps::check_dependencies(&root)?.into_parts();
    assert!(
        unresolved.is_none(),
        "a clean collection leaves no cleanup to report: {unresolved:?}"
    );
    assert!(
        contract.entry_count() > 0,
        "the committed register approves at least one edge"
    );
    assert!(
        refusals.is_empty(),
        "this repository passes its own gate, so the refusal list is empty: {refusals:?}"
    );
    Ok(())
}

#[test]
fn an_absent_target_is_spelled_empty_and_a_scope_is_kept() {
    assert_eq!(lgwks_deps::declared_scope(None), "");
    assert_eq!(lgwks_deps::declared_scope(Some("cfg(unix)")), "cfg(unix)");
}
