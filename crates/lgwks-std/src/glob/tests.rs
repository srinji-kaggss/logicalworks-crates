//! Unit tests for `glob`, moved verbatim out of glob.rs.

use super::*;

#[test]
fn empty_and_literal_inputs_keep_exact_results() {
    assert!(matches("", ""), "empty pattern matches the empty path");
    assert!(!matches("", "a"), "empty pattern rejects non-empty path");
    assert!(matches("hello", "hello"), "literal text matches itself");
    assert!(
        !matches("hello", "world"),
        "different literal text does not match"
    );
    assert!(!matches("hello", "hello/world"), "matching is anchored");
    assert!(matches("[", "["), "literal bracket remains matchable");
    assert!(
        matches("[abc", "[abc"),
        "legacy unclosed bracket remains literal"
    );
}

#[test]
fn question_and_star_preserve_separator_boundaries() {
    assert!(matches("a?c", "abc"), "question consumes one scalar");
    assert!(matches("a*c", "abbc"), "star consumes a same-segment run");
    assert!(!matches("a?c", "a/c"), "question never consumes slash");
    assert!(!matches("a*c", "a/b/c"), "single star never crosses slash");
    assert!(
        matches("*.rs", ".hidden.rs"),
        "leading dots are ordinary text"
    );
    assert!(matches("a\\b", "a\\b"), "backslash is an ordinary literal");
}

#[test]
fn classes_are_scalar_ordered_and_never_match_slash() {
    assert!(matches("[é]", "é"), "a class matches one multibyte scalar");
    assert!(
        matches("[zyxwvutsrqponmlkjihgfedcba]", "a"),
        "large unordered classes are compiled for logarithmic lookup"
    );
    assert!(
        matches("[a-zb-c]", "y"),
        "overlapping ranges merge before binary-search membership"
    );
    assert!(matches("[অ-ঊ]", "ঈ"), "Bengali range follows scalar order");
    assert!(
        matches("[😀-🙏]", "😃"),
        "supplementary range follows scalar order"
    );
    assert!(
        !matches("?", "e\u{301}"),
        "question does not collapse a decomposed two-scalar sequence"
    );
    assert!(
        matches("??", "e\u{301}"),
        "two questions consume the decomposed base and combining scalar"
    );
    assert!(
        !matches("?", "éx"),
        "one question does not consume two scalars"
    );
    assert!(!matches("??", "é"), "two questions do not match one scalar");
    assert!(!matches("[!a]", "/"), "negated classes still exclude slash");
    assert!(!matches("[a-z]", "/"), "ranges exclude slash");
    assert!(!matches("?", "/"), "question excludes slash");
    assert!(!matches("*", "a/b"), "star does not cross slash");
}

#[test]
fn double_star_legacy_forms_keep_exact_directory_reach() {
    assert!(
        matches("a/**/b", "a/b"),
        "double-star slash accepts zero directories"
    );
    assert!(
        matches("a/**/b", "a/x/y/b"),
        "double-star slash accepts nested directories"
    );
    assert!(
        matches("**/b", "b"),
        "leading double-star slash accepts no directory"
    );
    assert!(
        matches("**/b", "a/b"),
        "leading double-star slash accepts one directory"
    );
    assert!(
        matches("**/b", "x/y/b"),
        "leading double-star slash accepts nested directories"
    );
    assert!(
        matches("a/**", "a"),
        "trailing slash double-star accepts no suffix"
    );
    assert!(
        matches("a**b", "a/x/y/b"),
        "legacy embedded double-star may cross slash"
    );
    assert!(
        !GlobPattern::compile("a**b").is_ok(),
        "strict dialect rejects embedded double-star"
    );
}

/// Asserts that strict compilation refuses `pattern` with `kind` at
/// `byte_offset`.
///
/// One helper rather than a copy per malformed pattern: the refusals below
/// differ only in the pattern, the kind and the message, and a copy per case
/// is how one of them drifts away from the others.
fn assert_compile_refusal(
    pattern: &str,
    kind: PatternErrorKind,
    byte_offset: usize,
    message: &str,
) {
    assert_eq!(
        GlobPattern::compile(pattern).map(|_| ()),
        Err(PatternError { kind, byte_offset }),
        "{message}"
    );
}

#[test]
fn checked_compilation_reports_each_malformed_pattern_class() {
    assert_compile_refusal(
        "[abc",
        PatternErrorKind::UnclosedClass,
        0,
        "strict compilation reports an unclosed class offset",
    );
    assert_compile_refusal(
        "[]",
        PatternErrorKind::UnclosedClass,
        0,
        "strict compilation reports the unclosed leading-bracket class",
    );
    assert_compile_refusal(
        "[z-a]",
        PatternErrorKind::DescendingRange,
        0,
        "strict compilation reports a descending range",
    );
    assert!(
        !matches("[z-a]", "a"),
        "legacy descending range remains a no-match class"
    );
    assert!(
        matches("[]]", "]"),
        "leading close bracket remains a class member"
    );
    assert!(
        matches("[!]]", "a"),
        "negated leading close bracket class remains valid"
    );
}

#[test]
fn compiled_and_one_call_paths_have_identical_public_semantics() -> Result<(), PatternError> {
    let pattern = GlobPattern::compile_with_dialect("a**[b-d]?", GlobDialect::Legacy)?;
    let mut scratch = GlobScratch::new();
    for (path, expected) in [("axy/bc", true), ("abz", true), ("a/x/dé", true)] {
        assert_eq!(
            matches("a**[b-d]?", path),
            expected,
            "one-call result is stable"
        );
        assert_eq!(
            pattern.is_match_with(path, &mut scratch),
            expected,
            "compiled result matches one-call result"
        );
    }
    Ok(())
}

#[test]
fn each_transition_has_linear_measured_work_for_n_2n_4n() -> Result<(), PatternError> {
    let pattern = GlobPattern::compile_with_dialect("*a*", GlobDialect::Legacy)?;
    let mut previous = 0_usize;
    for size in [256_usize, 512, 1024] {
        let path = "a".repeat(size);
        let mut scratch = GlobScratch::new();
        assert!(
            pattern.is_match_with(&path, &mut scratch),
            "repeated a path matches"
        );
        let measured = scratch.work.transition;
        assert!(
            measured <= pattern.tokens.len().saturating_mul(size.saturating_add(1)),
            "work is bounded by token_count * scalar_count: {measured}"
        );
        if previous != 0 {
            assert!(
                measured
                    <= previous
                        .saturating_mul(2)
                        .saturating_add(pattern.tokens.len()),
                "doubling input does not produce quadratic work: {previous} -> {measured}"
            );
        }
        previous = measured;
    }
    Ok(())
}

#[test]
fn every_double_star_transition_scales_linearly() -> Result<(), PatternError> {
    for source in ["**", "**/x", "a/**", "a/**/b"] {
        let pattern = GlobPattern::compile_with_dialect(source, GlobDialect::Legacy)?;
        let mut previous = 0_usize;
        for size in [256_usize, 512, 1024] {
            let path = format!("a/{}/b", "x/".repeat(size.div_ceil(2)));
            let mut scratch = GlobScratch::new();
            let matched = pattern.is_match_with(&path, &mut scratch);
            assert_eq!(
                matched,
                source != "**/x",
                "double-star transition preserves expected path result"
            );
            let measured = scratch.work.transition;
            assert!(
                measured
                    <= pattern
                        .tokens
                        .len()
                        .saturating_mul(path.chars().count().saturating_add(1)),
                "{source} transition work is linear: {measured}"
            );
            if previous != 0 {
                assert!(
                    measured
                        <= previous
                            .saturating_mul(2)
                            .saturating_add(pattern.tokens.len()),
                    "{source} work growth is linear: {previous} -> {measured}"
                );
            }
            previous = measured;
        }
    }
    Ok(())
}

#[test]
fn unmatched_class_parser_work_is_linear_and_controls_are_explicit() -> Result<(), PatternError> {
    let mut previous = 0_usize;
    for size in [256_usize, 512, 1024] {
        let source = "[".repeat(size);
        let mut work = Work::default();
        let pattern = compile_pattern(&source, GlobDialect::Legacy, &mut work)?;
        let measured = work.parse;
        assert!(
            measured <= size.saturating_mul(3),
            "parser inspections remain linear: {measured}"
        );
        if previous != 0 {
            assert!(
                measured <= previous.saturating_mul(2).saturating_add(3),
                "doubling pattern does not cause suffix rescans: {previous} -> {measured}"
            );
        }
        assert_eq!(
            pattern.tokens.len(),
            size,
            "each unmatched bracket remains a literal"
        );
        previous = measured;
    }
    assert!(
        GlobPattern::compile("[abc]").is_ok(),
        "normal class control compiles"
    );
    assert!(
        matches("[abc", "[abc"),
        "literal malformed-class control uses legacy behavior"
    );
    assert_eq!(
        GlobPattern::compile("[abc").map(|_| ()),
        Err(PatternError {
            kind: PatternErrorKind::UnclosedClass,
            byte_offset: 0,
        }),
        "strict malformed-class control is explicit"
    );
    Ok(())
}

#[test]
fn a_compiled_pattern_is_shareable_across_threads_by_construction() {
    // The claim on `GlobPattern` is not a comment: the assertion is
    // checked at compile time, and this is what keeps it from being
    // deleted as an uncalled constant. It also proves the pattern is
    // shareable across real OS threads, not only that the bounds hold.
    assert_shared_across_threads();
    let compiled = GlobPattern::compile_with_dialect(SHARED_PATTERN, GlobDialect::Legacy);
    let Ok(pattern) = compiled else {
        return;
    };
    let shared = std::sync::Arc::new(pattern);
    let observed: Vec<bool> = (0..8)
        .map(|index| {
            let shared = std::sync::Arc::clone(&shared);
            match std::thread::Builder::new()
                .name(format!("glob-share-{index}"))
                .stack_size(64 * 1024)
                .spawn(move || {
                    let mut scratch = GlobScratch::new();
                    let path = format!("a/{}b7z", "x/".repeat(index));
                    shared.is_match_with(&path, &mut scratch)
                }) {
                // A thread that panicked has not answered the question, so
                // its panic is re-raised here rather than read as a
                // non-match; the assertion below then fails with the
                // panicking thread's own message instead of a quiet false.
                Ok(joined) => match joined.join() {
                    Ok(matched) => matched,
                    Err(panic) => std::panic::resume_unwind(panic),
                },
                Err(_) => false,
            }
        })
        .collect();
    assert!(
        observed.iter().all(|matched| *matched),
        "every caller must match the shared pattern identically: {observed:?}"
    );
}

/// The pattern the shareability test above matches with, kept next to it so
/// the two cannot drift apart.
const SHARED_PATTERN: &str = "*a**/b[0-9]?";

#[test]
fn scratch_capacity_is_reused_without_per_token_row_allocations() -> Result<(), PatternError> {
    let pattern = GlobPattern::compile_with_dialect("*a**/b[0-9]?", GlobDialect::Legacy)?;
    let mut scratch = GlobScratch::new();
    assert!(
        pattern.is_match_with("a/x/b7é", &mut scratch),
        "first match succeeds"
    );
    let capacities = (
        scratch.scalars.capacity(),
        scratch.previous.capacity(),
        scratch.next.capacity(),
    );
    for _ in 0..8 {
        assert!(
            pattern.is_match_with("a/x/b7é", &mut scratch),
            "reused match succeeds"
        );
        assert_eq!(
            (
                scratch.scalars.capacity(),
                scratch.previous.capacity(),
                scratch.next.capacity()
            ),
            capacities,
            "scalar and two-row capacities remain fixed after warm capacity"
        );
    }
    assert_eq!(
        pattern.tokens.len(),
        6,
        "compiled pattern storage is counted separately"
    );
    Ok(())
}
