//! unlogged-err-return evidence: the log has to carry the error (#699).
//!
//! What makes a log evidence for a return, and the lines that stay reported.
//!
//! The issue's own example leads: `eprintln!("zero"); return Err(e);` emits a
//! word and says nothing about the error, so it is not evidence (#699). A log
//! that names the error — a format capture, a field, an argument — is, and so is
//! a log carrying context of its own beyond a bare message, which is how a
//! refusal is announced with the frame it happened at. An error carrying no
//! value has nothing to name, so the log on the path is the whole signal. A name
//! the error does not carry is not the error, and a mention inside a longer word
//! (`errors`) is not a mention of `e`.
//!
//! What a log names is read from its arguments and format captures alone. The
//! macro path (`tracing::error!`) and the prose of the message name nothing, a
//! literal beside the message is not context, a label is not the value it
//! labels, a doubled brace is text, a macro that only ends like a log macro
//! (`my_info!`, `catalog::emit!`) is not one, and a log stays run for every later
//! statement of its block, whatever sits between it and the return. Each row is
//! one source and the lines the rule must report on it. The argument and field
//! syntax is `tracing`'s (`tracing::field`) and the captures are the implicit
//! named arguments of Rust RFC 2795 (`std::fmt`).

#![cfg(feature = "scan")]

use lgwks_ast::scan::{Hit, ScanError, scan_source};

/// Propagates a failure with `?` rather than aborting the run.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Scans one fixture string.
fn scan(text: &str) -> Result<Vec<Hit>, ScanError> {
    scan_source(text, "test.rs")
}

/// Lines the unlogged-error rule reports, ascending.
fn unlogged_lines(hits: &[Hit]) -> Vec<usize> {
    hits.iter()
        .filter(|hit| hit.rule() == "unlogged-err-return")
        .map(|hit| hit.line())
        .collect()
}

/// Every fixture and the lines it must report.
const EVIDENCE_CASES: [(&str, &[usize]); 23] = [
    (
        "fn f(x: bool) -> Result<(), E> {\n    if x {\n        let e = E::Flat;\n        eprintln!(\"zero\");\n        return Err(e);\n    }\n    Ok(())\n}\n",
        &[5],
    ),
    (
        "fn f(x: bool) -> Result<(), E> {\n    if x {\n        let e = E::Flat;\n        tracing::error!(\"failed: {e}\");\n        return Err(e);\n    }\n    Ok(())\n}\n",
        &[],
    ),
    (
        "fn f(x: bool) -> Result<(), E> {\n    if x {\n        let e = E::Flat;\n        tracing::error!(%e, \"failed\");\n        return Err(e);\n    }\n    Ok(())\n}\n",
        &[],
    ),
    (
        "fn f() -> Result<(), E> {\n    eprintln!(\"zero\");\n    return Err(E::Flat);\n}\n",
        &[],
    ),
    (
        "fn f() -> Result<(), E> {\n    let other = 1_u32;\n    eprintln!(\"saw {other}\");\n    let e = E::Flat;\n    return Err(e);\n}\n",
        &[5],
    ),
    (
        "fn f() -> Result<(), E> {\n    let e = E::Flat;\n    eprintln!(\"no errors today\");\n    return Err(e);\n}\n",
        &[4],
    ),
    (
        "fn f(index: u64) -> Result<(), E> {\n    let cause = E::Flat;\n    tracing::debug!(index, \"the frame did not decode\");\n    return Err(cause);\n}\n",
        &[],
    ),
    (
        "fn f() -> Result<(), E> {\n    let e = E::Flat;\n    tracing::error!(%e, \"failed\");\n    drop(1_u32);\n    return Err(e);\n}\n",
        &[],
    ),
    (
        "fn f(x: bool) -> Result<(), E> {\n    let e = E::Flat;\n    tracing::error!(%e, \"failed\");\n    if x {\n        step();\n    }\n    return Err(e);\n}\n",
        &[],
    ),
    (
        "fn f() -> Result<(), E> {\n    let error = E::Flat;\n    tracing::error!(\"request failed\");\n    return Err(error);\n}\n",
        &[4],
    ),
    (
        "fn f() -> Result<(), E> {\n    let file = E::Flat;\n    warn!(\"could not read the file\");\n    return Err(file);\n}\n",
        &[4],
    ),
    (
        "fn f() -> Result<(), E> {\n    let e = E::Flat;\n    eprintln!(\"failed {}\", 0);\n    return Err(e);\n}\n",
        &[4],
    ),
    (
        "fn f() -> Result<(), E> {\n    let e = E::Flat;\n    eprintln!(\"failed {}\", true);\n    return Err(e);\n}\n",
        &[4],
    ),
    (
        "fn f() -> Result<(), E> {\n    let error = E::Flat;\n    tracing::error!(error = \"boom\");\n    return Err(error);\n}\n",
        &[4],
    ),
    (
        "fn f() -> Result<(), E> {\n    let cause = E::Flat;\n    tracing::error!(error = ?cause);\n    return Err(cause);\n}\n",
        &[],
    ),
    (
        "fn f() -> Result<(), E> {\n    my_info!(\"x\");\n    return Err(E::Flat);\n}\n",
        &[3],
    ),
    (
        "fn f() -> Result<(), E> {\n    catalog::emit!(\"x\");\n    return Err(E::Flat);\n}\n",
        &[3],
    ),
    (
        "fn f() -> Result<(), E> {\n    let e = E::Flat;\n    eprintln!(\"{{e}}\");\n    return Err(e);\n}\n",
        &[4],
    ),
    (
        "fn f() -> Result<(), E> {\n    let e = E::Flat;\n    tracing::event!(Level::ERROR, \"x\");\n    return Err(e);\n}\n",
        &[4],
    ),
    (
        "fn f() -> Result<(), E> {\n    let e = E::Flat;\n    log::warn!(\"failed {}\", e.kind());\n    return Err(e);\n}\n",
        &[],
    ),
    (
        "fn f() -> Result<(), E> {\n    let e = E::Flat;\n    eprintln!(\"{}\", format!(\"{e}\"));\n    return Err(e);\n}\n",
        &[],
    ),
    (
        "fn f(header: &[u8; 4]) -> Result<(), E> {\n    tracing::warn!(found = header[0], \"refused\");\n    return Err(E::Version { found: header[0] });\n}\n",
        &[],
    ),
    (
        "fn f() -> Result<(), E> {\n    let e = E::Flat;\n    tracing::warn!(found = count(), \"refused\");\n    return Err(e);\n}\n",
        &[4],
    ),
];

#[test]
fn the_log_must_carry_the_error() -> TestResult {
    for (source, expected) in EVIDENCE_CASES {
        let hits = scan(source)?;
        assert_eq!(unlogged_lines(&hits), expected, "{source:?} -> {hits:?}");
    }
    Ok(())
}
