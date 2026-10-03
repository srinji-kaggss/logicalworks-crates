//! Seeded deterministic replay of the `pattern` safety contracts.
//!
//! One seed draws every haystack, a failure prints the seed that produced it,
//! and the same seed must produce the same trace hash. Every probe drives the
//! public API of the shipped crate.
//!
//! INV-PATTERN-SAFE makes two promises that pull against each other, and this
//! file is built around the tension between them. A checked pattern enforces
//! ceilings on the input bytes and on the replacement output, so a hostile
//! haystack cannot make the matcher allocate without bound. And the bounded
//! replacement expands **exactly** like the engine's own, so the ceiling costs
//! a refusal rather than a different answer. A family that checked only the
//! first would pass a replacement that silently truncated; one that checked only
//! the second would pass one with no ceiling at all.
//!
//! The module is gated `feature = "pattern"` because that is what gates
//! `lgwks_std::pattern` (INV-DEP-6).

#![cfg(feature = "pattern")]

#[path = "support/seeded_bytes.rs"]
mod seeded_bytes;
#[path = "support/seeded_sweep.rs"]
mod seeded_sweep;

use std::borrow::Cow;

use lgwks_std::pattern::{
    BoundedRegex, PatternConfig, PatternError, PatternErrorKind, PatternRunError, Regex,
};

use seeded_bytes::{below, fold_bytes, next_byte, next_text};
use seeded_sweep::{
    SWEEP_SEEDS, assert_distinct_seeds_diverge, assert_same_seed_replays, fold, fold_usize,
    initial_trace,
};

/// The input ceiling the bounded families configure, and the size of the
/// haystacks they draw: a haystack at the ceiling is admitted, one byte over is
/// refused, which is the boundary the input family is stated at.
const INPUT_LIMIT: usize = 256;

/// The patterns the families draw from. Each is a different engine feature, so a
/// family that only ever used one literal pattern would be testing one grammar.
const PATTERNS: [&str; 6] = [r"\d+", r"[a-z]+", r"x", r"\w\s\w", r"(?<lead>a)b", r""];

/// A configuration with the declared input and output ceilings and otherwise
/// permissive compile limits, so a refusal in these families is about the input
/// or the output and never about compilation.
fn config(input_limit: usize, output_limit: usize) -> PatternConfig {
    PatternConfig::with_limits(64 * 1024, 1_000_000, 250, input_limit, output_limit)
}

/// A seeded haystack of `length` bytes drawn from a small alphabet, so
/// catastrophic backtracking has something to be catastrophic over.
fn draw_haystack(state: &mut u64, length: usize) -> String {
    let alphabet = "aaaab";
    (0..length)
        .map(|_| {
            let index = usize::from(next_byte(state).rem_euclid(5));
            char::from(alphabet.as_bytes()[index])
        })
        .collect()
}

/// Draws a (pattern, haystack) pair from one seed, for the families that
/// compare a bounded operation against the engine's.
///
/// The pair is drawn once here rather than in each family: four families want
/// the same shape of case, and a draw written four times is four draws that can
/// drift apart without anything noticing.
fn draw_case(state: &mut u64) -> (&'static str, String) {
    let pattern = PATTERNS[below(state, PATTERNS.len())];
    let length = below(state, 64);
    let haystack = draw_haystack(state, length);
    (pattern, haystack)
}

/// Compiles `pattern` under `config`, the one door the bounded type has.
///
/// `with_config` lives on `Regex` and returns the `BoundedRegex` it built, so
/// every family here names that constructor directly rather than through a
/// wrapper that would hide which type it produced.
fn bounded_regex(pattern: &str, config: PatternConfig) -> Result<BoundedRegex, PatternError> {
    Regex::with_config(pattern, config)
}

/// Runs the seeded bounded-versus-unbounded sweep and returns its trace.
fn pattern_trace(seed: u64) -> u64 {
    let mut state = seed;
    let mut trace = initial_trace();

    for _ in 0..32 {
        let pattern = PATTERNS[below(&mut state, PATTERNS.len())];
        let length = below(&mut state, INPUT_LIMIT);
        let haystack = draw_haystack(&mut state, length);

        let bounded = bounded_regex(pattern, config(INPUT_LIMIT, INPUT_LIMIT * 2)).ok();
        let expected = Regex::new(pattern)
            .map(|unbounded| unbounded.is_match(&haystack))
            .ok();

        if let Some((observed, expected)) = bounded.zip(expected).and_then(|(regex, expected)| {
            regex.is_match(&haystack).ok().map(|seen| (seen, expected))
        }) {
            assert_eq!(
                observed, expected,
                "seed {seed}: a {length}-byte haystack must match as the engine matches"
            );
            fold_usize(&mut trace, usize::from(observed));
        }
        fold_bytes(&mut trace, haystack.as_bytes());
        fold_usize(&mut trace, length);
    }
    trace
}

#[test]
/// The checked pattern preserves the engine's match semantics: for every seeded
/// haystack inside the ceiling, `is_match` agrees with the unbounded engine, so
/// the ceiling costs a refusal rather than a different answer.
fn a_bounded_match_agrees_with_the_unbounded_engine() -> Result<(), Box<dyn std::error::Error>> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..48 {
            let (pattern, haystack) = draw_case(&mut state);

            let expected = Regex::new(pattern)?.is_match(&haystack);
            let bounded = bounded_regex(pattern, config(INPUT_LIMIT, INPUT_LIMIT))?;
            assert_eq!(
                bounded.is_match(&haystack)?,
                expected,
                "seed {seed}: {pattern:?} on a {}-byte haystack must match as the engine does",
                haystack.len()
            );
            assert_eq!(
                bounded.is_match(&haystack)?,
                expected,
                "seed {seed}: the same call twice must give the same answer"
            );
        }
    }
    Ok(())
}

#[test]
/// INV-PATTERN-SAFE's input ceiling: a haystack past the declared limit is
/// refused *before* the engine sees it, naming the limit and the actual size —
/// and refused identically by every operation, so no door is the one that forgot.
fn an_input_past_the_ceiling_is_refused_by_every_operation()
-> Result<(), Box<dyn std::error::Error>> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..16 {
            let over = INPUT_LIMIT.saturating_add(1 + below(&mut state, 64));
            let haystack = draw_haystack(&mut state, over);
            let bounded = bounded_regex("a", config(INPUT_LIMIT, INPUT_LIMIT * 2))?;
            let refused = PatternRunError::InputTooLarge {
                limit: INPUT_LIMIT,
                actual: over,
            };

            // Each operation's success arm has a different type, so the shared
            // fact — that every door refuses an over-ceiling input with the same
            // error — is asserted on the error arm alone.
            assert_eq!(
                bounded.is_match(&haystack),
                Err(refused),
                "seed {seed}: is_match"
            );
            assert_eq!(
                bounded.find(&haystack).err(),
                Some(refused),
                "seed {seed}: find must refuse the same way"
            );
            assert_eq!(
                bounded.captures(&haystack).err(),
                Some(refused),
                "seed {seed}: captures must refuse the same way"
            );
            assert_eq!(
                bounded.find_all(&haystack).err(),
                Some(refused),
                "seed {seed}: find_all must refuse the same way"
            );
            assert_eq!(
                bounded.split(&haystack).err(),
                Some(refused),
                "seed {seed}: split must refuse the same way"
            );
            assert_eq!(
                bounded.replace(&haystack, "x").err(),
                Some(refused),
                "seed {seed}: replace must refuse the same way"
            );
            assert_eq!(
                bounded.replace_all(&haystack, "x").err(),
                Some(refused),
                "seed {seed}: replace_all must refuse the same way"
            );
        }
    }
    Ok(())
}

#[test]
/// The input ceiling is exact at its boundary: a haystack of exactly the limit is
/// admitted and answered, and one byte more is refused. A limit that refused at
/// `limit` would make the declared number a lie.
fn the_input_ceiling_admits_its_own_boundary_and_refuses_one_byte_past()
-> Result<(), Box<dyn std::error::Error>> {
    let mut boundary_state = SWEEP_SEEDS[0];
    let at_limit = draw_haystack(&mut boundary_state, INPUT_LIMIT);
    let bounded = bounded_regex("a", config(INPUT_LIMIT, INPUT_LIMIT * 2))?;
    assert_eq!(
        at_limit.len(),
        INPUT_LIMIT,
        "the boundary haystack is at the limit"
    );
    assert!(
        bounded.is_match(&at_limit)?,
        "a haystack of exactly the limit must be answered"
    );

    let over_limit = format!("{at_limit}b");
    assert_eq!(
        bounded.is_match(&over_limit),
        Err(PatternRunError::InputTooLarge {
            limit: INPUT_LIMIT,
            actual: INPUT_LIMIT.saturating_add(1)
        }),
        "one byte past the limit must be refused"
    );
    Ok(())
}

#[test]
/// INV-PATTERN-SAFE's output ceiling: a replacement that would amplify past the
/// limit is refused, and the refusal names the size the append *would* have
/// produced rather than the size reached. The ceiling is charged before the
/// append, so the cost is refused rather than paid.
fn an_amplifying_replacement_is_refused_before_the_append() -> Result<(), Box<dyn std::error::Error>>
{
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..24 {
            let matches = below(&mut state, 16) + 1;
            let haystack = "a".repeat(matches);
            let per_match = below(&mut state, 8) + 2;
            let replacement = "x".repeat(per_match);
            let output_limit = matches.saturating_mul(2);
            let would_produce = matches.saturating_mul(per_match);

            let bounded = bounded_regex("a", config(INPUT_LIMIT, output_limit))?;
            if would_produce > output_limit {
                let observed = bounded.replace_all(&haystack, &replacement);
                assert!(
                    matches!(observed, Err(PatternRunError::OutputTooLarge { .. })),
                    "seed {seed}: {matches} matches expanding to {would_produce} bytes must be \
                     refused under a {output_limit}-byte ceiling, got {observed:?}"
                );
            } else {
                assert_eq!(
                    bounded.replace_all(&haystack, &replacement)?,
                    replacement.repeat(matches),
                    "seed {seed}: a replacement inside the ceiling is the complete replacement"
                );
            }
        }
    }
    Ok(())
}

#[test]
/// A refused replacement returns **no** prefix: the error arm carries no output
/// at all, so a caller cannot mistake a truncated replacement for a complete one
/// by looking at the returned value.
fn a_refused_replacement_returns_no_prefix() -> Result<(), Box<dyn std::error::Error>> {
    let bounded = bounded_regex("a", config(64, 4))?;
    match bounded.replace_all("aaaaaaaa", "xxxxxxxx") {
        Err(error @ PatternRunError::OutputTooLarge { .. }) => {
            assert!(
                error.to_string().contains('4'),
                "the refusal names its ceiling in {error}"
            );
            Ok(())
        }
        other => Err(format!("an amplifying replacement was not refused: {other:?}").into()),
    }
}

#[test]
/// INV-PATTERN-SAFE's semantic promise: the bounded replacement expands exactly
/// like the engine's own, byte for byte, at every seeded replacement template.
///
/// The bounded expander is a second implementation of the engine's `$name` and
/// `${name}` grammar, so this is the family that would catch it drifting — and
/// a bounded expander that drifted would be the worst kind of defect, because
/// the ceilings would still be enforced and the answers would still look
/// reasonable.
fn a_bounded_replacement_expands_exactly_like_the_engine() -> Result<(), Box<dyn std::error::Error>>
{
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..32 {
            let (pattern, haystack) = draw_case(&mut state);
            let template_len = below(&mut state, 6);
            let template = next_text(&mut state, template_len);

            let unbounded = Regex::new(pattern)?;
            let bounded = bounded_regex(pattern, config(INPUT_LIMIT, INPUT_LIMIT * 8))?;
            assert_eq!(
                bounded.replace_all(&haystack, &template)?,
                unbounded.replace_all(&haystack, &template),
                "seed {seed}: {pattern:?} with the template {template:?} must expand alike"
            );
            assert_eq!(
                bounded.replace(&haystack, &template)?,
                unbounded.replace(&haystack, &template),
                "seed {seed}: the single-match replacement must expand alike too"
            );
        }
    }
    Ok(())
}

#[test]
/// Every bounded match position agrees with the engine's: the spans of
/// `find_all` over a seeded haystack are the same list, in the same order, as the
/// engine's own non-overlapping iteration.
fn a_bounded_find_all_yields_the_engines_spans_in_order() -> Result<(), Box<dyn std::error::Error>>
{
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..32 {
            let (pattern, haystack) = draw_case(&mut state);

            let unbounded: Vec<(usize, usize)> = Regex::new(pattern)?
                .find_all(&haystack)
                .map(|found| (found.start, found.end))
                .collect();
            let bounded = bounded_regex(pattern, config(INPUT_LIMIT, INPUT_LIMIT * 8))?;
            let observed: Vec<(usize, usize)> = bounded
                .find_all(&haystack)?
                .map(|found| (found.start, found.end))
                .collect();

            assert_eq!(
                observed, unbounded,
                "seed {seed}: {pattern:?} must yield the engine's spans in the engine's order"
            );
            for window in observed.windows(2) {
                assert!(
                    window[0].1 <= window[1].0,
                    "seed {seed}: spans must not overlap and must stay in source order"
                );
            }
        }
    }
    Ok(())
}

#[test]
/// A split over a seeded haystack yields the same pieces as the engine's, so a
/// caller that splits under a ceiling gets the same fields a caller without one
/// would get.
fn a_bounded_split_yields_the_engines_pieces() -> Result<(), Box<dyn std::error::Error>> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..32 {
            let (pattern, haystack) = draw_case(&mut state);

            let unbounded: Vec<&str> = Regex::new(pattern)?.split(&haystack).collect();
            let bounded = bounded_regex(pattern, config(INPUT_LIMIT, INPUT_LIMIT * 8))?;
            let observed: Vec<&str> = bounded.split(&haystack)?.collect();
            assert_eq!(
                observed, unbounded,
                "seed {seed}: {pattern:?} must split as the engine splits"
            );
            assert_eq!(
                observed.concat(),
                unbounded.concat(),
                "seed {seed}: the pieces together carry every non-separator byte"
            );
        }
    }
    Ok(())
}

#[test]
/// Captures agree between the bounded and unbounded decoders, group by group,
/// including the distinction between a group that did not participate (`None`)
/// and one that matched empty (`Some("")`).
fn a_bounded_capture_agrees_with_the_engines_group_by_group()
-> Result<(), Box<dyn std::error::Error>> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for length in 0..8_usize {
            let haystack = draw_haystack(&mut state, length);
            let pattern = r"(a)?(b)?";
            let unbounded = Regex::new(pattern)?.captures(&haystack);
            let bounded = bounded_regex(pattern, config(INPUT_LIMIT, INPUT_LIMIT))?;
            assert_eq!(
                bounded.captures(&haystack)?,
                unbounded,
                "seed {seed}: the captures of {pattern:?} on {haystack:?} must agree"
            );
        }
    }
    Ok(())
}

#[test]
/// A replacement with no match borrows the input rather than copying it, and a
/// narrow output ceiling still refuses it — so "borrowed" never becomes "the
/// ceiling does not apply when nothing matched".
fn a_no_match_borrows_and_still_honours_the_output_ceiling()
-> Result<(), Box<dyn std::error::Error>> {
    let input = "nothing here matches this pattern at all";
    let borrowed = bounded_regex("zzzzz", config(64, 64))?;
    assert!(
        matches!(borrowed.replace_all(input, "x")?, Cow::Borrowed(value) if value == input),
        "a replacement with no match borrows the input"
    );

    let narrow = bounded_regex("zzzzz", config(64, 4))?;
    assert_eq!(
        narrow.replace_all(input, "x"),
        Err(PatternRunError::OutputTooLarge {
            limit: 4,
            attempted: input.len()
        }),
        "borrowing the input does not exempt it from the output ceiling"
    );
    Ok(())
}

#[test]
/// The compile-time ceilings are refused before the engine compiles anything: a
/// source pattern over its limit is refused with the limit and the actual size,
/// and the refusal carries only a bounded excerpt of the pattern rather than the
/// whole thing.
fn a_compile_ceiling_is_refused_with_the_limit_and_the_size() {
    let long = "a".repeat(1_000);
    let error = match Regex::with_config(&long, PatternConfig::with_limits(3, 1_000, 250, 16, 16)) {
        Err(error) => error,
        Ok(_) => return,
    };
    assert_eq!(
        error.kind(),
        PatternErrorKind::PatternTooLarge {
            limit: 3,
            actual: 1_000
        },
        "the refusal names the limit and the actual size"
    );
    assert_eq!(
        error.pattern().len(),
        64,
        "the refusal keeps a bounded excerpt, not the whole pattern"
    );
}

#[test]
/// A compile-size refusal and a nesting refusal are distinct arms from a syntax
/// refusal: a caller that has to decide whether to shorten the pattern, shrink
/// the engine, or fix the syntax needs three different answers.
fn the_compile_ceilings_and_a_syntax_error_are_three_distinct_refusals()
-> Result<(), Box<dyn std::error::Error>> {
    let syntax = match Regex::new("[unclosed") {
        Err(error) => error,
        Ok(_) => return Err("an unclosed class must not compile".into()),
    };
    assert_eq!(
        syntax.kind(),
        PatternErrorKind::Syntax,
        "an unclosed class is a syntax refusal"
    );

    let compiled =
        match Regex::with_config(r"\w", PatternConfig::with_limits(1_024, 1, 250, 16, 16)) {
            Err(error) => error,
            Ok(_) => return Err("a compiled-size ceiling must refuse".into()),
        };
    assert_eq!(
        compiled.kind(),
        PatternErrorKind::CompiledTooLarge,
        "a compiled-size ceiling is its own arm"
    );
    Ok(())
}

#[test]
/// A pattern's text is escaped in `Debug` and `Display`, so a pattern containing
/// a newline cannot forge a second line in whatever log interpolates the error.
fn a_refusals_escape_the_pattern_text() -> Result<(), Box<dyn std::error::Error>> {
    let error = match Regex::new("(\n") {
        Err(error) => error,
        Ok(_) => return Err("an unclosed group must not compile".into()),
    };
    let rendered = error.to_string();
    assert!(
        !rendered.contains('\n'),
        "a raw newline survived into the rendered refusal: {rendered:?}"
    );
    assert!(
        rendered.contains("invalid pattern"),
        "the refusal says what it is: {rendered}"
    );
    Ok(())
}

#[test]
/// A hostile haystack against a backtracking-shaped pattern is answered under a
/// ceiling, and the bounded answer is the engine's: `a*a*a*a*a*b` really does
/// match a run of `a`s followed by a `b`, and a pattern that must *not* match a
/// near-miss haystack still does not.
///
/// The engine has no backtracking limit — it is a finite automaton — so the
/// property here is not "the adversary is defeated" but "the adversary costs
/// bounded work and the answer is the engine's". Both directions are asserted,
/// because a matcher that answered `false` to a real match would pass a family
/// that only ever checked the negative.
fn a_hostile_haystack_is_answered_under_a_backtracking_pattern()
-> Result<(), Box<dyn std::error::Error>> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..8 {
            let length = below(&mut state, INPUT_LIMIT);
            // Many `a`s and one `b`: the shape that defeats a naive backtracker.
            let haystack = format!("{}b", "a".repeat(length));
            let bounded = bounded_regex(
                r"a*a*a*a*a*b",
                config(INPUT_LIMIT.saturating_add(1), INPUT_LIMIT * 2),
            )?;
            let unbounded = Regex::new(r"a*a*a*a*a*b")?;
            assert_eq!(
                bounded.is_match(&haystack)?,
                unbounded.is_match(&haystack),
                "seed {seed}: {length} `a`s then a `b` is answered as the engine answers"
            );

            // The near miss: no `b` at all, so the trailing class cannot be met.
            let near_miss = "a".repeat(length);
            assert!(
                !bounded.is_match(&near_miss)?,
                "seed {seed}: {length} `a`s and no `b` must not match a trailing `b`"
            );
        }
    }
    Ok(())
}

#[test]
/// An empty pattern matches everything and an empty haystack is matched by
/// everything: the two degenerate cases are stated rather than left to the draw,
/// because a matcher that mishandled either would still pass every other family.
fn the_empty_pattern_and_empty_haystack_are_exact_degenerate_cases()
-> Result<(), Box<dyn std::error::Error>> {
    let unbounded = Regex::new("")?;
    let bounded = bounded_regex("", config(8, 8))?;
    assert!(
        unbounded.is_match(""),
        "the empty pattern matches the empty haystack"
    );
    assert!(
        unbounded.is_match("abc"),
        "the empty pattern matches any haystack"
    );
    assert!(
        bounded.is_match("")?,
        "the bounded empty pattern matches the empty haystack"
    );
    assert!(
        bounded.is_match("abc")?,
        "the bounded empty pattern matches any haystack"
    );

    let literal = bounded_regex("abc", config(8, 8))?;
    assert!(
        !literal.is_match("")?,
        "a non-empty pattern does not match the empty haystack"
    );
    Ok(())
}

#[test]
/// A replacement ceiling is exact at its boundary: an output of exactly the
/// limit is produced, and one byte more is refused, so the declared number is a
/// real bound rather than a bound that fires early.
fn the_output_ceiling_is_exact_at_its_boundary() -> Result<(), Box<dyn std::error::Error>> {
    // Three matches, each replaced by two bytes: an output of exactly six.
    let at_limit = bounded_regex("a", config(16, 6))?;
    assert_eq!(
        at_limit.replace_all("aaa", "xx")?,
        "xxxxxx",
        "an output of exactly the limit is produced"
    );

    let one_over = bounded_regex("a", config(16, 6))?;
    assert_eq!(
        one_over.replace_all("aaaa", "xx"),
        Err(PatternRunError::OutputTooLarge {
            limit: 6,
            attempted: 8
        }),
        "one group past the limit is refused with the size the next append would produce"
    );
    Ok(())
}

#[test]
/// The replay oracle: one seed, one trace.
fn the_same_seed_replays_to_the_same_pattern_trace() {
    for seed in SWEEP_SEEDS {
        assert_same_seed_replays(pattern_trace, seed);
    }
}

#[test]
/// The divergence oracle: a sweep that ignored its seed would pass the replay.
fn distinct_pattern_seeds_diverge_in_their_trace() {
    assert_distinct_seeds_diverge(pattern_trace, SWEEP_SEEDS[0], SWEEP_SEEDS[1]);
}

#[test]
/// Two seeds draw two different haystacks, so the semantic-agreement family is
/// not one haystack checked repeatedly.
fn two_seeds_draw_two_different_haystacks() {
    let mut first = SWEEP_SEEDS[0];
    let mut second = SWEEP_SEEDS[1];
    let left = draw_haystack(&mut first, 64);
    let right = draw_haystack(&mut second, 64);
    assert_ne!(left, right, "the two sweep seeds drew the same haystack");
    let mut trace = initial_trace();
    fold_bytes(&mut trace, left.as_bytes());
    fold_bytes(&mut trace, right.as_bytes());
    fold(&mut trace, u64::try_from(left.len()).unwrap_or(0));
    assert_ne!(
        trace,
        initial_trace(),
        "the two haystacks must fold into distinct traces"
    );
}
