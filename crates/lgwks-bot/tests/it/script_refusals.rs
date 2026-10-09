//! `script!`'s refusals, held by a real downstream consumer's compiler.
//!
//! The macro refuses banned calls by spelling (`lgwks_ast::script` reads them), and
//! those refusals are asserted by message in `lgwks_macros`' own tests. What a
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

/// The script tool and the compiler refuse one construct with one message at
/// one place (#384): `lgwks-ast script check` reads the consumer's source
/// through the function the macro calls, so its refusal's line, column and
/// message are the ones the compiler prints for the same file. The probe runs
/// under the same clippy pass as this file's other consumers, so it reuses
/// their build instead of paying for a second one.
#[test]
fn the_tool_reports_the_refusal_the_compiler_reports() -> TestResult {
    let source = consumer("", "", "std::process::exit(code)");
    let invocations = lgwks_ast::script::read_source(&source)?;
    assert!(
        invocations.len() == 1
            && invocations
                .iter()
                .all(|invocation| invocation.read().is_err()),
        "the tool finds the consumer's one script! and refuses it: {invocations:?}"
    );
    let output = clippy_probe("script-refusal-located", "", &source)?;
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !output.status.success(),
        "the compiler refused the flow too:\n{printed}"
    );
    for refusal in invocations
        .iter()
        .filter_map(|invocation| invocation.read().err())
    {
        let location = format!(
            "src/main.rs:{}:{}",
            refusal.line(),
            refusal.column().saturating_add(1)
        );
        assert!(
            printed.contains(&location),
            "the compiler reports the refusal at the tool's {location}:\n{printed}"
        );
        assert!(
            refusal
                .message()
                .lines()
                .next()
                .is_some_and(|first| printed.contains(&format!("error: {first}"))),
            "the compiler reports the tool's message `{}`:\n{printed}",
            refusal.message()
        );
    }
    Ok(())
}
