//! The `script!` reader through its public interface (#526).
//!
//! The module header of `lgwks_ast::script` used to carry this as a doctest that
//! named the crate by path; it is a test now, so the claim it makes runs under
//! nextest with the rest of the suite. A tool turns plain source text into the
//! tree the macro would build, refusals included, with each token's line and
//! column kept.

#![cfg(feature = "script")]

use std::str::FromStr;

use lgwks_ast::script::{Kind, parse};
use lgwks_deps::proc_macro2::TokenStream;

/// What every fallible test here returns.
type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn a_tool_reads_the_macros_map_from_plain_text() -> TestResult {
    let source = "flow sizes(pages: Vec<String>) -> usize:\n    \
                  let sizes = each page in pages:\n        page.len()\n    \
                  give back sizes.len()\n";
    let tokens = TokenStream::from_str(source).map_err(|error| error.to_string())?;
    let script = parse(tokens).map_err(|refusal| refusal.to_string())?;
    let flow = script.flows().first().ok_or("one flow")?;
    assert_eq!(
        flow.shape().signature(),
        "sizes(pages: Vec<String>) -> usize",
        "the signature is read back as written"
    );
    let kinds: Vec<Kind> = flow
        .shape()
        .steps()
        .iter()
        .map(|step| step.kind())
        .collect();
    assert_eq!(
        kinds,
        [Kind::Each, Kind::GiveBack],
        "the map, in source order"
    );
    Ok(())
}

#[test]
fn a_refusal_is_located_at_its_token_as_the_compiler_reports_it() -> TestResult {
    let refused = TokenStream::from_str("flow f():\n    std::process::exit(1)\n")
        .map_err(|error| error.to_string())?;
    let refusal = parse(refused).err().ok_or("exit is refused")?;
    assert!(
        refusal.message().contains("ending the process"),
        "the refusal names what it refuses: {refusal}"
    );
    assert_eq!(
        refusal.line(),
        2,
        "located at the token, as the compiler reports it"
    );
    Ok(())
}
