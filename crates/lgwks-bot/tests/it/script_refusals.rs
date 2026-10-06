//! `script!`'s refusals, held by a real downstream consumer's compiler.
//!
//! The macro refuses banned calls by spelling (`lgwks_macros::refuse`), and
//! those refusals are asserted by message in that crate's own tests. What a
//! token check cannot see is a name imported outside the script under another
//! spelling: `use std::process::exit;` above the macro, `exit(1)` inside it.
//! For that half every generated flow carries `#[forbid(..)]` lint attributes,
//! and this file proves a consumer's `cargo clippy` enforces them, through the
//! same one-file-consumer harness `t22_process_surface` uses.
//!
//! Each refusal is paired with a positive control through the identical path,
//! so a failure to resolve or build the probe can never pass as a refusal.

#![cfg(feature = "script")]

type TestResult = Result<(), Box<dyn std::error::Error>>;

use crate::compile;

use compile::{assert_compiles, assert_refused_by_lint, clippy_probe};

/// A consumer with `imports` above one script whose single flow runs `body`.
fn consumer(imports: &str, attribute: &str, body: &str) -> String {
    format!(
        "{imports}\nlgwks_bot::script! {{\n    {attribute}\n    pub flow halt(code: i32, guard: String) -> i32:\n        {body}\n}}\n\nfn main() {{\n    let _ = halt;\n}}\n"
    )
}

#[test]
fn a_correct_flow_passes_the_consumers_clippy() -> TestResult {
    let output = clippy_probe(
        "script-refusal-control",
        "",
        &consumer(
            "",
            "",
            "let empty = guard.is_empty()\n        give back code + i32::from(empty)",
        ),
    )?;
    assert_compiles(&output);
    Ok(())
}

#[test]
fn an_exit_imported_outside_the_script_is_refused_inside_it() -> TestResult {
    let output = clippy_probe(
        "script-refusal-exit",
        "",
        &consumer("use std::process::exit;", "", "exit(code)"),
    )?;
    assert_refused_by_lint(&output, "process::exit");
    Ok(())
}

#[test]
fn an_allow_written_above_the_flow_cannot_lower_its_forbid() -> TestResult {
    let output = clippy_probe(
        "script-refusal-allow",
        "",
        &consumer(
            "use std::process::exit;",
            "#[allow(clippy::exit)]",
            "exit(code)",
        ),
    )?;
    assert_refused_by_lint(&output, "process::exit");
    Ok(())
}

#[test]
fn a_forget_renamed_outside_the_script_is_refused_inside_it() -> TestResult {
    let output = clippy_probe(
        "script-refusal-forget",
        "",
        &consumer(
            "use std::mem::forget as keep;",
            "",
            "keep(guard)\n        give back code",
        ),
    )?;
    assert_refused_by_lint(&output, "mem::forget");
    Ok(())
}
