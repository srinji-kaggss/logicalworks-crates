//! Consumer-level checks for the compiled and one-call glob APIs.

use lgwks_std::glob::{
    GlobDialect, GlobPattern, GlobScratch, PatternError, PatternErrorKind, matches,
};

#[test]
fn public_compile_reuse_and_legacy_call_share_text_semantics() -> Result<(), PatternError> {
    let compiled = GlobPattern::compile("src/**/[a-z]?.rs")?;
    let mut scratch = GlobScratch::new();
    for (path, expected) in [
        ("src/a1.rs", true),
        ("src/nested/a1.rs", true),
        ("src/nested/ab1.rs", false),
    ] {
        assert_eq!(
            compiled.is_match_with(path, &mut scratch),
            expected,
            "compiled matcher returns the declared result"
        );
        assert_eq!(
            compiled.is_match(path),
            expected,
            "compiled one-call matcher agrees"
        );
    }
    assert!(
        matches("a**b", "a/x/b"),
        "legacy one-call entry keeps embedded double-star migration behavior"
    );
    let legacy = GlobPattern::compile_with_dialect("a**b", GlobDialect::Legacy)?;
    assert!(
        legacy.is_match_with("a/x/b", &mut scratch),
        "named legacy policy preserves embedded double-star"
    );
    Ok(())
}

#[test]
fn public_checked_compile_distinguishes_malformed_input_from_no_match() {
    let malformed = GlobPattern::compile("[unterminated")
        .err()
        .map(|error| (error.kind, error.byte_offset));
    assert_eq!(
        malformed,
        Some((PatternErrorKind::UnclosedClass, 0)),
        "malformed syntax has a typed error and byte offset"
    );
    assert_eq!(
        GlobPattern::compile("[a-z]").map(|pattern| pattern.is_match("7")),
        Ok(false),
        "valid non-matching input is an ordinary false result"
    );
}

#[test]
fn public_wildcards_and_classes_count_unicode_scalars() -> Result<(), PatternError> {
    let one = GlobPattern::compile("?")?;
    let two = GlobPattern::compile("??")?;
    let bengali = GlobPattern::compile("[অ-ঊ]")?;
    let supplementary = GlobPattern::compile("[😀-🙏]")?;
    let decomposed = GlobPattern::compile("??")?;
    assert!(
        one.is_match("é"),
        "one question consumes one multibyte scalar"
    );
    assert!(
        !two.is_match("é"),
        "two questions do not consume one scalar"
    );
    assert!(
        bengali.is_match("ঈ"),
        "Bengali scalar range is ordered by Unicode scalar value"
    );
    assert!(
        supplementary.is_match("😃"),
        "supplementary scalar ranges are supported"
    );
    assert!(
        decomposed.is_match("e\u{301}"),
        "decomposed text contains two scalars without normalization"
    );
    assert!(
        !GlobPattern::compile("[!a]")?.is_match("/"),
        "negated classes exclude slash"
    );
    Ok(())
}

/// The syntax this matcher shares with the `glob` crate answers as `glob`
/// 0.3.4 does. The two lists are that release's `Pattern::matches` verdicts for
/// ten cases, read by compiling its `src/lib.rs` and running them when the
/// vendored tree was dropped (#266), so the oracle lives beside the cases and no
/// checkout has to carry a copy of the crate. Semantics:
/// <https://docs.rs/glob/0.3.4/glob/struct.Pattern.html>.
#[test]
fn shared_subset_matches_the_recorded_answers_of_glob_0_3_4() {
    let upstream_accepts = [
        ("literal", "literal"),
        ("a?c", "abc"),
        ("a*b", "ab"),
        ("a*b", "axyb"),
        ("[a-c]", "b"),
        ("[!a-c]", "z"),
    ];
    let upstream_refuses = [
        ("a?c", "ac"),
        ("a*b", "axyc"),
        ("[a-c]", "z"),
        ("[!a-c]", "b"),
    ];
    for (pattern, path) in upstream_accepts {
        assert!(
            matches(pattern, path),
            "`{pattern}` against `{path}`: glob 0.3.4 matches, so must this matcher"
        );
    }
    for (pattern, path) in upstream_refuses {
        assert!(
            !matches(pattern, path),
            "`{pattern}` against `{path}`: glob 0.3.4 refuses, so must this matcher"
        );
    }
}
