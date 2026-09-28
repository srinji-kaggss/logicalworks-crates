//! Public API regressions compiled as a downstream consumer crate.

#[cfg(feature = "lang-rust")]
use lgwks_ast::{
    ContentDetection, InspectionStopReason, Language, MAX_SYNTAX_DIAGNOSTICS, ParseError,
    SyntaxIssueKind, inspect_ast, try_detect_content, try_parse,
};
#[cfg(all(feature = "lang-python", not(feature = "lang-rust")))]
use lgwks_ast::{Language, inspect_ast, try_parse};

#[cfg(feature = "lang-rust")]
#[test]
fn external_consumer_sees_duplicate_invariant_detection_and_complete_metrics()
-> Result<(), Box<dyn std::error::Error>> {
    let selected = try_detect_content("fn main() {}", &[Language::Rust, Language::Rust])?;
    assert_eq!(
        selected,
        ContentDetection::Unique(Language::Rust),
        "duplicate candidates yield one grammar"
    );
    assert_eq!(
        try_detect_content("fn main() {}", &[])?,
        ContentDetection::NoMatch,
        "an empty candidate set is distinct from ambiguity"
    );

    let parsed = try_parse("fn main() {}", Language::Rust)?;
    let metrics = inspect_ast(&parsed.root(), None);
    assert!(metrics.complete, "unlimited public inspection completes");
    assert_eq!(metrics.node_limit, None, "unlimited inspection has no cap");

    let partial = inspect_ast(&parsed.root(), Some(0));
    assert!(
        !partial.complete,
        "zero-cap public inspection is explicitly partial"
    );
    assert_eq!(partial.nodes, 1, "root is retained as the overflow witness");
    assert_eq!(
        partial.stop_reason,
        Some(InspectionStopReason::NodeLimitExceeded),
        "public metrics expose the reason for incomplete inspection"
    );
    Ok(())
}

#[cfg(all(feature = "lang-rust", feature = "lang-python"))]
#[test]
fn external_consumer_keeps_real_cross_grammar_ambiguity() {
    assert_eq!(
        try_detect_content("", &[Language::Rust, Language::Python]),
        Ok(ContentDetection::Ambiguous),
        "both grammars accept the empty source, so detection remains ambiguous"
    );
    assert_eq!(
        try_detect_content("fn main() {}", &[Language::Python]),
        Ok(ContentDetection::NoMatch),
        "invalid syntax is negative evidence for that candidate"
    );
}

#[cfg(feature = "lang-rust")]
#[test]
fn external_consumer_receives_bounded_recovery_kind_and_byte_span()
-> Result<(), Box<dyn std::error::Error>> {
    let source = "fn main() {\n    let café = ;\n}\n";
    let error = try_parse(source, Language::Rust);
    let diagnostics = match error {
        Err(ParseError::InvalidSyntax { diagnostics, .. }) => diagnostics,
        other => return Err(format!("expected InvalidSyntax, got {:?}", other.err()).into()),
    };
    let recovery = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.kind == SyntaxIssueKind::Error)
        .ok_or_else(|| std::io::Error::other("expected an ERROR diagnostic"))?;
    assert!(
        diagnostics.len() <= MAX_SYNTAX_DIAGNOSTICS,
        "the public parse refusal respects the diagnostic ceiling"
    );
    assert!(
        source.is_char_boundary(recovery.start_byte),
        "start offset is a UTF-8 boundary"
    );
    assert!(
        source.is_char_boundary(recovery.end_byte),
        "end offset is a UTF-8 boundary"
    );
    assert!(
        source.get(recovery.start_byte..recovery.end_byte).is_some(),
        "span slices source"
    );
    Ok(())
}

#[cfg(all(feature = "lang-python", not(feature = "lang-rust")))]
#[test]
fn python_only_consumer_uses_its_selected_grammar() -> Result<(), Box<dyn std::error::Error>> {
    let language = Language::of_path("main.py").ok_or("Python feature did not select .py")?;
    assert_eq!(
        language,
        Language::Python,
        "Python-only build resolves its own grammar"
    );
    let parsed = try_parse("def main():\n    return 1\n", language)?;
    assert!(
        inspect_ast(&parsed.root(), None).complete,
        "Python-only build parses through the public checked API"
    );
    Ok(())
}
