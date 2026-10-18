//! The README's usage flow and the consumer guide's walkthrough, run as tests (#526).
//!
//! Both pages used to carry them as doctests that named the crate by path; they are
//! tests now, so each claim runs under nextest with the rest of the suite.

#![cfg(feature = "lang-rust")]

use lgwks_ast::{
    Language, MAX_SOURCE_BYTES, ParseError, has_syntax_issues, inspect_ast, parse, try_parse,
};

/// What every fallible test here returns.
type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn the_readme_usage_flow_identifies_parses_and_inspects() -> TestResult {
    let language = Language::of_path("src/lib.rs");
    assert_eq!(
        language,
        Some(Language::Rust),
        "`.rs` resolves without parsing"
    );
    let parsed = try_parse("fn f() {}", Language::Rust)?;
    let metrics = inspect_ast(&parsed.root(), None);
    assert!(metrics.nodes > 1, "a `fn` item is more than one node");
    Ok(())
}

#[test]
fn the_guide_walkthrough_refuses_what_it_should() -> TestResult {
    let language = Language::of_path("src/lib.rs").ok_or("the rust feature is on")?;
    assert_eq!(language, Language::Rust);
    assert_eq!(language.name(), "rust");
    if !cfg!(feature = "lang-c") {
        assert_eq!(
            Language::of_path("main.c"),
            None,
            "a grammar this build did not select is not recognized at all"
        );
    }

    let parsed = try_parse("fn f() { let x = 1; }", language)?;
    let metrics = inspect_ast(&parsed.root(), None);
    assert!(
        !metrics.has_syntax_issues,
        "a clean parse carries no recovery node"
    );
    assert!(metrics.nodes > 5, "got {} nodes", metrics.nodes);
    assert!(metrics.max_depth >= 3, "got depth {}", metrics.max_depth);

    match try_parse("fn f( {", language) {
        Err(ParseError::InvalidSyntax {
            language,
            diagnostics,
            diagnostics_truncated,
            ..
        }) => {
            assert_eq!(language, "rust");
            assert!(!diagnostics.is_empty(), "the refusal carries a diagnostic");
            assert!(!diagnostics_truncated, "one recovery node is not truncated");
        }
        other => return Err(format!("expected InvalidSyntax, got {:?}", other.err()).into()),
    }

    let oversized = "a".repeat(MAX_SOURCE_BYTES.saturating_add(1));
    match try_parse(&oversized, language) {
        Err(ParseError::SourceTooLarge { actual, limit }) => {
            assert_eq!(limit, MAX_SOURCE_BYTES);
            assert_eq!(actual, MAX_SOURCE_BYTES.saturating_add(1));
        }
        other => return Err(format!("expected SourceTooLarge, got {:?}", other.err()).into()),
    }

    let recovered = parse("fn f( {", language);
    assert!(
        has_syntax_issues(&recovered.root()),
        "the unchecked parse hands back the recovered tree"
    );
    Ok(())
}
