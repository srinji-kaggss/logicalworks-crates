//! Seeded deterministic replay of the #154 glob matcher contracts.
//!
//! One seed drives every pattern and path, a failure prints the seed, and the
//! same seed must produce the same trace hash. Matching results are compared
//! against an independent brute-force reference so the seeded sweep checks
//! semantics, not just self-consistency.
//!
//! The reference is deliberately naive: it enumerates every way the pattern's
//! tokens can consume the path, in both the strict and legacy dialects, and
//! computes the rolling reachability row for the whole path at each step. It is
//! `O(tokens * N²)` and shares no code with the shipped per-token scan, so a
//! match that both agree on is a real agreement.

use lgwks_std::glob::{GlobDialect, GlobPattern, GlobScratch, PatternError, matches};

use crate::seeded_sweep;

use seeded_sweep::{
    SWEEP_SEEDS, assert_distinct_seeds_diverge, assert_same_seed_replays, fold, initial_trace,
    next_seed,
};

/// The reference's token set, mirroring the documented dialect without sharing
/// the shipped code.
#[derive(Clone, Debug, PartialEq, Eq)]
enum RefToken {
    /// One exact scalar.
    Literal(char),
    /// Exactly one non-separator scalar.
    Question,
    /// Zero or more non-separator scalars.
    Star,
    /// Zero or more scalars, including separators.
    DoubleStar,
    /// A class: inclusive ranges plus a negation flag.
    Class {
        /// Sorted inclusive ranges.
        ranges: Vec<(char, char)>,
        /// Whether a scalar outside the ranges is accepted.
        negated: bool,
    },
}

/// Tokenizes a pattern under the reference's own rules.
fn reference_tokens(pattern: &str, dialect: GlobDialect) -> Vec<RefToken> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        match chars[index] {
            '*' if chars.get(index.saturating_add(1)) == Some(&'*') => {
                let mut run = index;
                while chars.get(run) == Some(&'*') {
                    run = run.saturating_add(1);
                }
                if dialect == GlobDialect::Strict
                    && (run.saturating_sub(index) != 2
                        || (index != 0 && chars[index.saturating_sub(1)] != '/')
                        || (run != chars.len() && chars[run] != '/'))
                {
                    return Vec::new();
                }
                tokens.push(RefToken::DoubleStar);
                index = run;
            }
            '*' => {
                tokens.push(RefToken::Star);
                index = index.saturating_add(1);
            }
            '?' => {
                tokens.push(RefToken::Question);
                index = index.saturating_add(1);
            }
            '[' => {
                let mut body = index.saturating_add(1);
                let mut negated = false;
                if matches!(chars.get(body), Some('!' | '^')) {
                    negated = true;
                    body = body.saturating_add(1);
                }
                let mut close = body;
                if chars.get(close) == Some(&']') {
                    close = close.saturating_add(1);
                }
                while close < chars.len() && chars[close] != ']' {
                    close = close.saturating_add(1);
                }
                if close >= chars.len() {
                    if dialect == GlobDialect::Strict {
                        return Vec::new();
                    }
                    tokens.push(RefToken::Literal('['));
                    index = index.saturating_add(1);
                    continue;
                }
                let mut ranges = Vec::new();
                let inner = &chars[body..close];
                let mut at = 0;
                while at < inner.len() {
                    if at.saturating_add(2) < inner.len() && inner[at.saturating_add(1)] == '-' {
                        let (start, end) = (inner[at], inner[at.saturating_add(2)]);
                        if start > end && dialect == GlobDialect::Strict {
                            return Vec::new();
                        }
                        if start <= end {
                            ranges.push((start, end));
                        }
                        at = at.saturating_add(3);
                    } else {
                        ranges.push((inner[at], inner[at]));
                        at = at.saturating_add(1);
                    }
                }
                tokens.push(RefToken::Class { ranges, negated });
                index = close.saturating_add(1);
            }
            other => {
                tokens.push(RefToken::Literal(other));
                index = index.saturating_add(1);
            }
        }
    }
    tokens
}

/// Whether the class accepts a scalar.
fn class_accepts(ranges: &[(char, char)], negated: bool, value: char) -> bool {
    let inside = ranges
        .iter()
        .any(|&(low, high)| low <= value && value <= high);
    inside != negated
}

/// Computes the reference verdict with a whole-path reachability sweep.
///
/// This is the shape the shipped per-token scan replaced, kept here so the
/// seeded sweep compares results rather than re-running the same algorithm.
fn reference_matches(tokens: &[RefToken], path: &[char]) -> bool {
    let mut reachable = vec![false; path.len().saturating_add(1)];
    reachable[0] = true;
    for token in tokens {
        let mut next = vec![false; path.len().saturating_add(1)];
        match *token {
            RefToken::Literal(literal) => {
                for (offset, value) in path.iter().enumerate() {
                    if reachable[offset] && *value == literal {
                        next[offset.saturating_add(1)] = true;
                    }
                }
            }
            RefToken::Question => {
                for (offset, value) in path.iter().enumerate() {
                    if reachable[offset] && *value != '/' {
                        next[offset.saturating_add(1)] = true;
                    }
                }
            }
            RefToken::Class {
                ref ranges,
                negated,
            } => {
                for (offset, value) in path.iter().enumerate() {
                    if reachable[offset] && *value != '/' && class_accepts(ranges, negated, *value)
                    {
                        next[offset.saturating_add(1)] = true;
                    }
                }
            }
            RefToken::Star => {
                for offset in 0..=path.len() {
                    let previous = offset.saturating_sub(1);
                    let extend = offset > 0 && next[previous] && path[previous] != '/';
                    next[offset] = reachable[offset] || extend;
                }
            }
            RefToken::DoubleStar => {
                let mut any = false;
                for offset in 0..=path.len() {
                    any |= reachable[offset];
                    next[offset] = any;
                }
            }
        }
        reachable = next;
    }
    reachable[path.len()]
}

/// Runs the seeded sweep and returns its deterministic trace.
fn run_seeded_sweep(seed: u64) -> u64 {
    let mut state = seed;
    let mut trace = initial_trace();

    let alphabets = ["ab/", "ab/?", "ab/éঈ😀", ".-_", "xyz/[]"];
    let path_fragments = ["a", "b", "x/y", "", "é", "😀", ".", "/", "a/b/c", "-"];

    for _ in 0..1_500 {
        let alphabet: Vec<char> = alphabets
            [usize::try_from(next_seed(&mut state).rem_euclid(5)).unwrap_or(0)]
        .chars()
        .collect();
        // The pattern is a seeded run of alphabet scalars.
        let pattern_length = usize::try_from(next_seed(&mut state).rem_euclid(8))
            .unwrap_or(1)
            .saturating_add(1);
        let mut pattern = String::new();
        for _ in 0..pattern_length {
            let pick = alphabet[usize::try_from(
                next_seed(&mut state).rem_euclid(u64::try_from(alphabet.len()).unwrap_or(1)),
            )
            .unwrap_or(0)];
            pattern.push(pick);
        }
        // Inject the wildcard and class metacharacters at a seeded position.
        let meta = ['*', '?', '[', ']', '/'];
        let inject_at = usize::try_from(
            next_seed(&mut state)
                .rem_euclid(u64::try_from(pattern.chars().count().saturating_add(1)).unwrap_or(1)),
        )
        .unwrap_or(0);
        let inject = meta[usize::try_from(next_seed(&mut state).rem_euclid(5)).unwrap_or(0)];
        let pattern: String = {
            let mut chars: Vec<char> = pattern.chars().collect();
            chars.insert(inject_at.min(chars.len()), inject);
            chars.into_iter().collect()
        };

        let dialect = if next_seed(&mut state).rem_euclid(2) == 0 {
            GlobDialect::Strict
        } else {
            GlobDialect::Legacy
        };
        let path: String = {
            let first =
                path_fragments[usize::try_from(next_seed(&mut state).rem_euclid(10)).unwrap_or(0)];
            let second =
                path_fragments[usize::try_from(next_seed(&mut state).rem_euclid(10)).unwrap_or(0)];
            format!("{first}{second}")
        };

        let compiled = GlobPattern::compile_with_dialect(&pattern, dialect);
        let reference_tokens = reference_tokens(&pattern, dialect);
        if reference_tokens.is_empty() {
            // The reference tokenized to nothing, which it also uses for a
            // strict refusal. Distinguish the two with the real compiler.
            assert!(
                compiled.is_err(),
                "seed {seed}: pattern {pattern:?} refused by the reference must \
                 also be refused by the strict compiler"
            );
            fold(&mut trace, u64::MAX);
            continue;
        }

        let compiled = match compiled {
            Ok(value) => value,
            Err(_) => {
                fold(&mut trace, u64::MAX);
                continue;
            }
        };

        let mut scratch = GlobScratch::new();
        let compiled_chars: Vec<char> = path.chars().collect();
        let expected = reference_matches(&reference_tokens, &compiled_chars);
        let observed = compiled.is_match_with(&path, &mut scratch);
        fold(&mut trace, u64::from(observed));
        assert_eq!(
            observed, expected,
            "seed {seed}: {pattern:?} against {path:?} under {dialect:?} must \
             agree with the independent reference"
        );

        // The one-call legacy entry and the compiled path must agree.
        if dialect == GlobDialect::Legacy {
            assert_eq!(
                matches(&pattern, &path),
                observed,
                "seed {seed}: the one-call entry must agree with the compiled path"
            );
        }
    }
    trace
}

/// Whether a single wildcard or class component consumes the separator.
///
/// A pattern that is exactly one `?`, one `*`, or one class must never match
/// the bare `/`. A pattern that spells `/` literally may, and a multi-token
/// pattern is not this property at all — `a/b` matches the separator by
/// matching the literal.
fn single_component_matches_slash(pattern: &str, dialect: GlobDialect) -> bool {
    let Ok(compiled) = GlobPattern::compile_with_dialect(pattern, dialect) else {
        return false;
    };
    // Only a single wildcard or class token, with no literal around it.
    let is_single_component =
        matches!(pattern, "?" | "*" | "**") || (pattern.starts_with('[') && pattern.ends_with(']'));
    is_single_component && compiled.is_match("/")
}

#[test]
fn the_same_seed_replays_to_the_same_trace() {
    for seed in SWEEP_SEEDS {
        assert_same_seed_replays(run_seeded_sweep, seed);
    }
}

#[test]
fn different_seeds_produce_different_traces() {
    assert_distinct_seeds_diverge(run_seeded_sweep, SWEEP_SEEDS[0], SWEEP_SEEDS[1]);
}

#[test]
fn the_separator_is_never_consumed_by_any_single_component() -> Result<(), PatternError> {
    // `/` is excluded from `?`, `*`, and every class including a negated one, in
    // both dialects and however the class is spelled. `**` is deliberately
    // absent: crossing the separator is what it exists to do.
    for pattern in [
        "?", "*", "[/]", "[!-#]", "[^/]", "[a-z]", "[!a-z]", "[--/]", "[!]", "*?", "?*",
    ] {
        assert!(
            !single_component_matches_slash(pattern, GlobDialect::Strict),
            "{pattern:?} must not match the bare separator under strict"
        );
        assert!(
            !single_component_matches_slash(pattern, GlobDialect::Legacy),
            "{pattern:?} must not match the bare separator under legacy"
        );
    }
    // A pattern that spells the separator literally does match it, which is
    // what keeps `a/**/b` against `a/b` working.
    assert!(
        !matches_slash("a/**/b", '/', GlobDialect::Strict),
        "a/**/b needs at least an `a` before the separator"
    );
    assert!(matches_slash("/", '/', GlobDialect::Strict));
    Ok(())
}

/// Whether a pattern matches the bare separator as its whole path.
fn matches_slash(pattern: &str, path: char, dialect: GlobDialect) -> bool {
    match GlobPattern::compile_with_dialect(pattern, dialect) {
        Ok(compiled) => compiled.is_match(&path.to_string()),
        Err(_) => false,
    }
}

#[test]
fn the_documented_directory_reach_cases_keep_their_exact_results() -> Result<(), PatternError> {
    // The migration-sensitive forms #154 names, retained exactly.
    for (pattern, path, expected) in [
        ("a/**/b", "a/b", true),
        ("a/**/b", "a/x/y/b", true),
        ("a/**/b", "a/b/c", false),
        ("**/b", "b", true),
        ("**/b", "a/b", true),
        ("a/**", "a", true),
        ("a/**", "a/x/y", true),
        ("a**b", "a/x/y/b", true),
        ("", "", true),
        ("", "a", false),
        ("[", "[", true),
        ("[abc", "[abc", true),
        ("*", "a/b", false),
        ("**", "a/b", true),
    ] {
        assert_eq!(
            matches(pattern, path),
            expected,
            "legacy dialect: {pattern:?} against {path:?}"
        );
        let compiled = GlobPattern::compile_with_dialect(pattern, GlobDialect::Legacy)?;
        assert_eq!(
            compiled.is_match(path),
            expected,
            "compiled legacy dialect: {pattern:?} against {path:?}"
        );
    }
    // The same cases under the strict dialect, where `a**b` is a typed refusal
    // rather than a silent no-match.
    assert!(GlobPattern::compile("a**b").is_err());
    assert!(GlobPattern::compile("[abc").is_err());
    assert!(GlobPattern::compile("[z-a]").is_err());
    Ok(())
}

#[test]
fn unicode_scalar_semantics_hold_across_the_declared_cases() -> Result<(), PatternError> {
    // One scalar matches `?`, whether it is one UTF-8 byte or four: the unit is
    // the scalar, not the byte. Two `?` do not match one scalar.
    assert!(GlobPattern::compile("?")?.is_match("é"));
    assert!(GlobPattern::compile("?")?.is_match("😀"));
    assert!(!GlobPattern::compile("??")?.is_match("é"));
    assert!(!GlobPattern::compile("??")?.is_match("😀"));
    // A decomposed sequence is two scalars and matches `??` without
    // normalization.
    assert!(GlobPattern::compile("??")?.is_match("e\u{301}"));
    assert!(!GlobPattern::compile("?")?.is_match("e\u{301}"));
    // Classes and ranges follow scalar ordering.
    assert!(GlobPattern::compile("[é]")?.is_match("é"));
    assert!(GlobPattern::compile("[অ-ঊ]")?.is_match("ঈ"));
    assert!(GlobPattern::compile("[😀-🙏]")?.is_match("😃"));
    assert!(GlobPattern::compile("[ঊ-অ]").is_err());
    Ok(())
}
