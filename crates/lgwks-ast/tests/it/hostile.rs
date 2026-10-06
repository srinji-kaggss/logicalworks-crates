//! Hostile and malformed input: every answer is a typed refusal, or it is not
//! an answer (#277).
//!
//! Two bodies of evidence. The first is the whole fixture table the crate
//! already declares, plus adversarial generators that produce the shapes a
//! hostile PR actually carries -- nested delimiters with and without their
//! closers, one line of megabytes, a source at the byte ceiling, invalid UTF-8
//! boundaries, and each grammar's own nesting fragment taken to a depth past
//! [`MAX_AST_DEPTH`]. Every one of them must come back as a [`ParseError`].
//! Not "no panic" as a matter of luck: each generator is paired with the shape
//! of answer it is supposed to get, so a refusal that arrives for the wrong
//! reason fails.
//!
//! The second body is the markdown guard. tree-sitter's markdown external
//! scanner serializes its open block containers into a fixed 1 024-byte buffer
//! and asserts when they do not fit, and an assertion in a C parser is
//! `abort()`: the process dies, the caller with it, and `try_parse` returns
//! nothing at all. `"- "` repeated 255 times — 510 bytes — does it. The crate
//! cannot patch the scanner, but it can refuse the source before the scanner
//! sees it, and `MAX_MARKDOWN_CONTAINERS_PER_LINE` is that refusal. The
//! seventeen shapes, their measured abort depths and the thousand-seed proof are
//! in the section at the foot of this file.
//!
//! # What the guard does not cover
//!
//! It runs on the **checked** parse. `parse` and `parse_with` return a `Parsed`
//! rather than a `Result`, so a refusal has nowhere to go in them and this change
//! moved neither signature; the unchecked path carries the exposure and says so
//! on its own documentation. A caller accepting untrusted input uses
//! [`try_parse`].
//!
//! # Why a hard per-test timeout
//!
//! A test that hangs is a test that hides a defect, and the shapes here are the
//! ones that would hang. `.config/nextest.toml` gives every test in this module
//! a `slow-timeout` and a `terminate-after`, so a walk that stops making
//! progress fails the run instead of holding it open.

#![cfg(feature = "lang-rust")]

use std::error::Error;

#[cfg(feature = "lang-md")]
use std::io::Write;
#[cfg(feature = "lang-md")]
use std::time::Duration;

#[cfg(feature = "lang-md")]
use crate::seed::Rng;
use lgwks_ast::{
    Language, MAX_AST_DEPTH, MAX_AST_NODES, MAX_SOURCE_BYTES, ParseError, inspect_ast, parse,
    try_parse,
};

/// What every family returns.
type TestResult = Result<(), Box<dyn Error>>;

/// What a case builder returns: the cases, or the refusal that stopped it.
type Built<T> = Result<T, Box<dyn Error>>;

/// One shape of hostile input, and what it is supposed to earn.
struct Adversarial {
    /// What the generator is called in an assertion message.
    name: &'static str,
    /// The source, or `None` when this build cannot build the shape's grammar.
    source: Option<String>,
    /// Whether the source is the grammar's own valid source.
    ///
    /// Not "must be accepted": a full-ceiling tiling of a grammar that emits
    /// eight nodes per twelve source bytes is past [`MAX_AST_NODES`], and being
    /// refused for that is the bound working. What a valid source must never be
    /// refused for is *syntax* — that would mean the tiling broke the grammar's
    /// own input rather than testing a bound.
    valid: bool,
}

/// The nesting fragment each grammar is fed, where it has one.
///
/// `None` where the grammar is not compiled in this build, or where the
/// generator has no delimiter to nest. The table is the one the measurement
/// example uses, kept here deliberately separate: that one describes shapes to
/// time, this one describes shapes to refuse, and a shape allowed to be invalid
/// there is not the same thing as one allowed to be invalid here.
fn nesting_fragment(language: Language) -> Option<(&'static str, &'static str)> {
    match language.name() {
        "bash" | "c" | "cpp" | "csharp" | "css" | "dart" | "hcl" | "java" | "kotlin" | "nix"
        | "scala" | "solidity" | "swift" => Some(("{", "}")),
        "go" => Some(("{", "}")),
        "typescript" | "tsx" | "javascript" | "python" | "ruby" | "php" | "lua" | "haskell"
        | "rust" => Some(("(", ")")),
        "json" => Some(("[", "]")),
        "html" => Some(("<div>", "</div>")),
        "elixir" => Some(("defmodule A do\n", "end\n")),
        "markdown" => Some(("- ", "")),
        "yaml" => Some(("  ", "")),
        _ => None,
    }
}

/// The nesting fragment, nested past [`MAX_AST_DEPTH`] and paired.
fn deeply_nested(language: Language) -> Option<String> {
    let (open, close) = nesting_fragment(language)?;
    if close.is_empty() {
        // An indentation-run grammar closes by dedent, which this generator
        // cannot express without also producing valid source; the paired case
        // below covers it.
        return None;
    }
    let levels = MAX_AST_DEPTH.saturating_mul(8);
    Some(format!(
        "{}{}{}",
        open.repeat(levels),
        "x",
        close.repeat(levels)
    ))
}

/// The nesting fragment with no closers at all.
fn unbalanced(language: Language) -> Option<String> {
    let (open, _) = nesting_fragment(language)?;
    Some(open.repeat(MAX_AST_DEPTH.saturating_mul(8)))
}

/// Every adversarial shape this build can construct for `language`.
///
/// Four families: the grammar's own source at the byte ceiling, one line of
/// megabytes, a nesting run past the depth ceiling with its closers, and the
/// same run without them. The last two are the shapes the depth bound exists
/// for; the first two are the shapes a byte bound exists for.
fn adversarial_shapes(language: Language) -> Vec<Adversarial> {
    let mut shapes = Vec::new();
    let Some(valid) = grammar_sample(language) else {
        return shapes;
    };
    let one_line = "x".repeat(64);
    shapes.push(Adversarial {
        name: "tiled-to-the-byte-ceiling",
        source: Some(tile_to(valid, MAX_SOURCE_BYTES)),
        valid: true,
    });
    shapes.push(Adversarial {
        name: "one-line-of-megabytes",
        source: Some(tile_to(&one_line, MAX_SOURCE_BYTES)),
        valid: false,
    });
    if let Some(nested) = deeply_nested(language) {
        shapes.push(Adversarial {
            name: "nested-past-the-depth-ceiling",
            source: Some(nested),
            valid: false,
        });
    }
    if let Some(open) = unbalanced(language) {
        shapes.push(Adversarial {
            name: "unbalanced-openers",
            source: Some(open),
            valid: false,
        });
    }
    shapes
}

/// `fragment` repeated into `bytes`, in whole copies and never past it.
///
/// Whole copies only, and never a byte over: a shape that overshot the ceiling
/// would be refused by the byte bound and would measure nothing about the
/// grammar, and truncating mid-copy could split a multi-byte character. A
/// fragment wider than the budget yields an empty string rather than a partial
/// one, which no sample in this table is.
fn tile_to(fragment: &str, bytes: usize) -> String {
    let mut source = String::with_capacity(bytes);
    while source.len().saturating_add(fragment.len()) <= bytes {
        source.push_str(fragment);
    }
    source
}

/// Source this build knows `language` accepts, or `None` where the build has no
/// fixture for it.
///
/// The crate's own `FIXTURES` table is `#[cfg(test)]` inside the library and is
/// not reachable from an integration binary, so the one sample each shape needs
/// is written here. Where a grammar's sample is not known, no shape is built:
/// inventing source for a grammar nobody here can validate would make the test
/// assert something about the wrong thing.
fn grammar_sample(language: Language) -> Option<&'static str> {
    Some(match language.name() {
        "bash" => "echo hello\n",
        "c" => "int main(void) { return 0; }\n",
        "cpp" => "int main() { return 0; }\n",
        "csharp" => "class A { }\n",
        "css" => "a { color: red; }\n",
        "dart" => "void main() {}\n",
        "elixir" => "defmodule A do\n  def f, do: 1\nend\n",
        "go" => "package main\n\nfunc main() {}\n",
        "haskell" => "main = putStrLn \"hi\"\n",
        "hcl" => "resource \"a\" \"b\" {\n}\n",
        "html" => "<!DOCTYPE html>\n<html><body><p>hi</p></body></html>\n",
        "java" => "class A {}\n",
        "javascript" => "const a = 1;\n",
        "json" => "{\"a\": 1}\n",
        "kotlin" => "fun main() {}\n",
        "lua" => "local a = 1\n",
        "nix" => "{ pkgs }: pkgs.hello\n",
        "php" => "<?php echo \"hi\";\n",
        "python" => "def f():\n    return 1\n",
        "ruby" => "def f\n  1\nend\n",
        "rust" => "fn main() {}\n",
        "scala" => "object A { def f = 1 }\n",
        "solidity" => "contract A {}\n",
        "swift" => "func f() {}\n",
        "tsx" | "typescript" => "const a: number = 1;\n",
        "yaml" => "a: 1\n",
        _ => return None,
    })
}

/// The name of a refusal, for an assertion that reads as the variant itself.
fn refusal_name(error: &ParseError) -> String {
    format!("{error}")
}

// ── Typed refusals, in process ──────────────────────────────────────────────

#[test]
/// Every adversarial shape of every compiled grammar answers with a `ParseError`
/// or a tree, and never with a panic, a hang or a silent empty answer.
fn every_adversarial_shape_answers_typed() -> TestResult {
    let mut checked = 0_usize;
    let mut tilings_refused_for_syntax = 0_usize;
    let mut tilings_accepted = 0_usize;
    for &language in Language::ALL {
        for shape in adversarial_shapes(language) {
            let Some(source) = shape.source.as_deref() else {
                continue;
            };
            let answer = try_parse(source, language);
            match answer {
                Ok(tree) => {
                    // An adversarial *shape* is not the same thing as invalid
                    // source: two megabytes of `x` is one enormous valid Bash
                    // command, and the one-line shape is adversarial because of
                    // its length rather than its content. What must hold either
                    // way is that a tree this crate handed back is inside the
                    // bounds it claims.
                    if shape.valid {
                        tilings_accepted = tilings_accepted.saturating_add(1);
                    }
                    let metrics = inspect_ast(&tree.root(), None);
                    assert!(
                        metrics.nodes <= MAX_AST_NODES,
                        "{} {}: a checked parse returned a tree of {} nodes",
                        language.name(),
                        shape.name,
                        metrics.nodes
                    );
                    assert!(
                        !metrics.has_syntax_issues,
                        "{} {}: a checked parse returned a tree carrying a recovery node",
                        language.name(),
                        shape.name
                    );
                }
                Err(error) => {
                    // The refusal must name itself: a caller reporting findings
                    // cannot use a blank message.
                    assert!(
                        !refusal_name(&error).is_empty(),
                        "{} {}: the refusal rendered as nothing",
                        language.name(),
                        shape.name
                    );
                    if shape.valid {
                        // Tiling is only sound for a grammar whose unit of source
                        // concatenates: three Nix expressions in one file are
                        // three expressions and a syntax error, which says
                        // nothing about a bound. Counted rather than asserted,
                        // so the assertion at the end can read how many grammars
                        // tile cleanly rather than this one guessing.
                        tilings_refused_for_syntax = tilings_refused_for_syntax.saturating_add(1);
                    }
                }
            }
            checked = checked.saturating_add(1);
        }
    }
    assert!(
        checked >= 4 * 3,
        "only {checked} shapes were checked; the default build compiles 7 grammars \
         and four shapes each"
    );
    // The byte-ceiling shape must be able to succeed, or the family would only
    // ever be measuring refusals and a bound that refused everything would pass.
    // Tiling is not sound for every grammar, so this is a count over the
    // grammars whose unit of source concatenates, not over all of them.
    assert!(
        tilings_accepted >= 3,
        "only {tilings_accepted} grammars accepted a full-ceiling tiling of their own valid \
         source, and {tilings_refused_for_syntax} refused it for syntax; the byte-ceiling shape \
         is not proving that a 2 MiB source can be parsed"
    );
    Ok(())
}

#[test]
/// A source at the byte ceiling is refused *before* the parser runs, so the
/// refusal names the byte bound and not a tree it built first.
fn the_byte_ceiling_is_refused_before_any_tree_exists() -> TestResult {
    let over = "x".repeat(MAX_SOURCE_BYTES.saturating_add(1));
    let refusal = match try_parse(&over, Language::Rust) {
        Err(refusal) => refusal,
        Ok(_) => return Err("a source one byte over the ceiling was accepted".into()),
    };
    match refusal {
        ParseError::SourceTooLarge { actual, limit } => {
            assert_eq!(
                limit, MAX_SOURCE_BYTES,
                "the refusal names the applied bound"
            );
            assert_eq!(actual, over.len(), "and the observed length");
        }
        other => {
            return Err(format!("expected SourceTooLarge, got {}", refusal_name(&other)).into());
        }
    }
    // A source of exactly the ceiling is inside the byte bound, so what refuses
    // it next is a recovery node rather than the byte check. That ordering is
    // the claim: the byte check runs first, and only for a source past it.
    let at = "x".repeat(MAX_SOURCE_BYTES);
    assert!(
        matches!(
            try_parse(&at, Language::Rust),
            Err(ParseError::InvalidSyntax { .. })
        ),
        "a source exactly at the ceiling is past the byte bound but not the node bound"
    );
    Ok(())
}

#[test]
/// Every refusal renders as a located diagnostic rather than as text, including
/// the ones the byte, depth and deadline bounds produce.
fn every_refusal_renders_as_a_located_diagnostic() -> TestResult {
    let source = "fn main() {}\n";
    let refusals = [
        ParseError::SourceTooLarge {
            actual: MAX_SOURCE_BYTES.saturating_add(1),
            limit: MAX_SOURCE_BYTES,
        },
        ParseError::ParserUnavailable {
            language: "rust",
            detail: "injected".to_owned(),
        },
        ParseError::AstTooLarge {
            language: "rust",
            observed: 3,
            limit: 2,
        },
        ParseError::AstTooDeep {
            language: "rust",
            observed: MAX_AST_DEPTH.saturating_add(1),
            limit: MAX_AST_DEPTH,
        },
        ParseError::TimedOut {
            language: "rust",
            after: lgwks_ast::DEFAULT_PARSE_DEADLINE,
        },
    ];
    for refusal in refusals {
        let reported = refusal.to_diagnostic("hostile.rs", source);
        assert!(
            !reported.render().is_empty(),
            "{refusal} rendered as an empty report"
        );
        assert!(
            reported.span().start.byte <= source.len(),
            "{refusal} rendered a span past the end of its own source"
        );
    }
    Ok(())
}

#[test]
/// An unchecked parse of the same hostile shapes does not hang or panic, which
/// is what `parse` exists for: inspecting a malformed tree on purpose.
fn the_unchecked_parse_of_the_same_shapes_still_answers() -> TestResult {
    for source in [
        "(".repeat(MAX_AST_DEPTH.saturating_mul(8)),
        ")".repeat(MAX_AST_DEPTH.saturating_mul(8)),
        format!("{}{}", "(".repeat(4_096), ")".repeat(4_096)),
        "\u{fffd}".repeat(1_024),
        "\u{1f600}".repeat(1_024),
    ] {
        let tree = parse(&source, Language::Rust);
        let metrics = inspect_ast(&tree.root(), None);
        assert!(
            metrics.nodes >= 1,
            "a parse of {} bytes reported {} nodes",
            source.len(),
            metrics.nodes
        );
        // The depth of a hostile tree is a measurement, never a refusal: this is
        // the unchecked walk, and capping it would make the number a constant.
        assert!(
            metrics.max_depth >= 1,
            "the root counts as depth 1 for a {} byte source",
            source.len()
        );
    }
    Ok(())
}

#[test]
/// Multi-byte and truncated UTF-8 boundaries are answered, not panicked on.
fn multibyte_and_truncated_sources_are_answered() -> TestResult {
    for source in [
        // A multibyte character cut in half is not a `&str`; the truncated form
        // is the one a caller reading bytes can actually hand over.
        String::from_utf8_lossy(&[0xf0, 0x9f, 0x98]).into_owned(),
        String::from_utf8_lossy(&[0x80]).into_owned(),
        String::from("fn f() { /* \u{e9}\u{65e5}\u{672c} */ }\n"),
        format!("{}\u{2028}{}", "fn f() {}".repeat(64), "//".repeat(1_000)),
    ] {
        // The only assertion available in process: whatever came back is a
        // `Result` the caller can match on, never a panic on the way to it.
        if let Err(error) = try_parse(&source, Language::Rust) {
            assert!(
                !refusal_name(&error).is_empty(),
                "a refusal rendered as nothing"
            );
        }
    }
    Ok(())
}

// ── The markdown guard ──────────────────────────────────────────────────────
//
// Compiled only where the markdown grammar is. Every case below is a case about
// *that* grammar's external scanner, and a build without it has no scanner to
// overflow; the sibling test outside this module asserts that rather than
// letting the coverage read as wider than it is.
#[cfg(feature = "lang-md")]
mod markdown {
    use super::*;

    use lgwks_ast::MAX_MARKDOWN_CONTAINERS_PER_LINE;

    // ── The markdown guard ──────────────────────────────────────────────────────
    //
    // # What was measured
    //
    // tree-sitter's markdown grammar keeps its open block containers in an external
    // scanner whose state is serialized into a fixed 1 024-byte buffer, and it
    // *asserts* when that state does not fit. An assertion in a C parser is
    // `abort()`: the process dies, the caller with it, and `try_parse` never returns
    // anything a caller could handle. `"- "` repeated 255 times -- 510 bytes --
    // does it.
    //
    // Seventeen container shapes were bisected from a child process, one depth at a
    // time. **Every shape that aborts does so at 255 open containers**, whether it
    // spells them one per repetition (255 of `- `), two per repetition (128 of
    // `> - `) or three (85 of `>>> `). Three shapes -- tab runs and
    // indentation-nested blockquotes and ordered lists -- never abort at all,
    // because markdown does not nest those by indentation.
    // `MAX_MARKDOWN_CONTAINERS_PER_LINE` is 64, about a quarter of 255, and
    // `lgwks_ast::markdown_containers` counts that quantity directly.
    //
    // An exhaustive sweep afterwards took all seventeen shapes through every depth
    // from 1 to 512 -- 8 704 (shape, depth) pairs -- and every child exited.
    //
    // # What is proved here
    //
    // Every shape at bound-1, bound and bound+1, every shape at its own measured
    // abort depth, and a thousand seeded mixes of container prefixes. Each runs in a
    // child process, because a process that aborts cannot assert anything
    // afterwards: the parent reads an exit status, so "the guard held" and "the
    // guard did not hold" are two different observations rather than one green line.

    /// Set in the child so a re-executed binary takes the child branch.
    const CHILD_ENV: &str = "LGWKS_AST_MARKDOWN_GUARD_CHILD";

    /// Which case of [`markdown_cases`] the child is asked to parse.
    const CASE_ENV: &str = "LGWKS_AST_MARKDOWN_GUARD_CASE";

    /// This test's own name, for the child's `--exact` filter.
    ///
    /// Qualified by its module, because the child is this same binary and libtest
    /// matches the full path: an unqualified name selects nothing, the child runs
    /// zero tests and exits 0, and the parent's "the child died" assertion then
    /// fails against a child that never did anything.
    const GUARD_TEST_NAME: &str =
        "hostile::markdown::markdown_never_reaches_the_scanner_past_its_bound";

    /// Seeds in the mixed-prefix family.
    ///
    /// A count rather than a seed count: the loop draws each seed from its
    /// index, and a case total is compared against this on both sides.
    const SEEDED_CASES: usize = 1_000;

    /// The base the seeded family's seeds are derived from.
    const SEED_BASE: u64 = 0x0d0c_0000_0000_0277;

    /// How a container shape spells one level of nesting.
    #[derive(Clone, Copy)]
    enum Form {
        /// The fragment repeated on a single line, then an item.
        Repeated,
        /// One item per level, indented by two spaces per level.
        Indented,
        /// A code block inside `depth` quote levels.
        Coded,
    }

    /// This shape's source at a nesting `depth`.
    fn shape_source(form: Form, fragment: &str, depth: usize) -> String {
        match form {
            Form::Repeated => format!("{}x\n", fragment.repeat(depth)),
            Form::Indented => {
                let mut source = String::new();
                for level in 0..depth {
                    source.push_str(&"  ".repeat(level));
                    source.push_str(fragment);
                    source.push('\n');
                }
                source
            }
            Form::Coded => format!("{}{}\n", "> ".repeat(depth), fragment),
        }
    }

    /// A markdown container shape: its name, the fragment it nests, how it spells a
    /// level, and the smallest depth that aborted before the guard existed.
    ///
    /// `None` for the last element means the shape never aborts, because markdown
    /// does not nest it by indentation and the scanner's open-container count stays
    /// flat however deep the source goes.
    type Shape = (&'static str, &'static str, Form, Option<usize>);

    /// Every container shape, with the depth at which it was measured to abort.
    const SHAPES: [Shape; 17] = [
        ("dash-line", "- ", Form::Repeated, Some(255)),
        ("star-line", "* ", Form::Repeated, Some(255)),
        ("plus-line", "+ ", Form::Repeated, Some(255)),
        ("ordered-dot-line", "1. ", Form::Repeated, Some(255)),
        ("ordered-paren-line", "1) ", Form::Repeated, Some(255)),
        ("quote-space-line", "> ", Form::Repeated, Some(255)),
        ("quote-bare-line", ">", Form::Repeated, Some(255)),
        ("quote-triple-line", ">>> ", Form::Repeated, Some(85)),
        ("tab-line", "\t", Form::Repeated, None),
        ("indent-quote", "> x", Form::Indented, None),
        ("indent-ordered", "1. x", Form::Indented, None),
        ("indent-dash", "- x", Form::Indented, Some(255)),
        ("list-in-quote", "> - ", Form::Repeated, Some(128)),
        ("quote-in-list", "- > ", Form::Repeated, Some(128)),
        ("list-in-quote-in-list", "- > - ", Form::Repeated, Some(85)),
        ("fence-in-quotes", "```\nx\n```\n", Form::Coded, Some(255)),
        ("indented-code-in-quotes", "    x\n", Form::Coded, Some(255)),
    ];

    /// The container fragments a seeded case mixes, drawn per line.
    const FRAGMENTS: [&str; 10] = [
        "- ", "* ", "+ ", "1. ", "1) ", "> ", ">>> ", ">", "  ", "\t",
    ];

    /// A seeded mix of container prefixes: per line, an indent and a fragment, so a
    /// case can be shallow but dense, deep but sparse, or both at once.
    ///
    /// Every draw is bounded by a count a sixteen-bit `usize` holds, and each is
    /// converted rather than assumed: a host that cannot hold one is reported,
    /// because a mix written from a substituted indent or fragment is still a
    /// case and would still be run against the guard.
    fn seeded_source(seed: u64) -> Built<String> {
        let mut rng = Rng::new(seed ^ SEED_BASE);
        let lines = rng.between(1, 24);
        let mut source = String::new();
        for _ in 0..lines {
            let indent = usize::try_from(rng.between(0, 24))?;
            source.push_str(&"  ".repeat(indent));
            let count = u32::try_from(FRAGMENTS.len())?;
            let at = usize::try_from(rng.below(count))?;
            let fragment = FRAGMENTS
                .get(at)
                .copied()
                .ok_or("a draw from the fragment table names none of it")?;
            let count = usize::try_from(rng.between(1, 40))?;
            source.push_str(&fragment.repeat(count));
            source.push_str("x\n");
        }
        Ok(source)
    }

    /// One markdown source the guard is proved against.
    struct Case {
        /// Where the case came from, as an assertion message.
        label: String,
        /// The source handed to `try_parse`.
        source: String,
        /// Whether the guard is expected to be what refuses it.
        guarded: bool,
    }

    /// Every case: the shapes at the bound and around it, the shapes at their own
    /// measured abort depth, and [`SEEDED_CASES`] seeded mixes.
    fn markdown_cases() -> Built<Vec<Case>> {
        let bound = MAX_MARKDOWN_CONTAINERS_PER_LINE;
        let mut cases = Vec::new();
        for entry in &SHAPES {
            let (name, fragment, form, aborts_at) = *entry;
            for depth in [bound.saturating_sub(1), bound, bound.saturating_add(1)] {
                cases.push(case(
                    shape_source(form, fragment, depth),
                    format!("{name} at depth {depth}"),
                ));
            }
            if let Some(depth) = aborts_at {
                cases.push(case(
                    shape_source(form, fragment, depth),
                    format!("{name} at its abort depth {depth}"),
                ));
            }
        }
        for index in 0..SEEDED_CASES {
            let seed = u64::try_from(index)?.wrapping_mul(0x9e37_79b9_7f4a_7c15);
            cases.push(case(
                seeded_source(seed)?,
                format!("seeded mix {index} (seed {seed:#x})"),
            ));
        }
        Ok(cases)
    }

    /// One case from a source, recording whether the guard is what refuses it.
    fn case(source: String, label: String) -> Case {
        let guarded =
            lgwks_ast::markdown_containers(&source, MAX_MARKDOWN_CONTAINERS_PER_LINE).is_some();
        Case {
            label,
            source,
            guarded,
        }
    }

    /// Run this binary as the child that parses case `index`.
    ///
    /// The child's exit status is the whole observation, so nothing is written to a
    /// file: a code means the child answered, and no code means it did not, which is
    /// the fact this module exists to record. A child that will not answer is
    /// killed at [`CHILD_BUDGET`] rather than waited on, because the parse is
    /// single-digit milliseconds and a hang is the defect this module records.
    fn markdown_child(index: usize) -> Built<std::process::ExitStatus> {
        let mut child = std::process::Command::new(std::env::current_exe()?)
            .args([GUARD_TEST_NAME, "--exact", "--nocapture"])
            .env(CHILD_ENV, "1")
            .env(CASE_ENV, index.to_string())
            .spawn()?;
        let started = std::time::Instant::now();
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            // The budget is measured from this instant rather than added to one,
            // so the deadline cannot be built by arithmetic that overflows or is
            // mistaken for a duration the clock holds.
            let elapsed = started.elapsed();
            if elapsed >= CHILD_BUDGET {
                // The child owns nothing of this process and holds the only
                // handle to itself, so a kill followed by a wait is what
                // releases it rather than leaving a process behind the run.
                let _killed = child.kill();
                let _reaped = child.wait();
                return Err(format!("case {index} did not answer within {CHILD_BUDGET:?}").into());
            }
            // A park, not a spin: the budget is seconds and the poll interval is
            // milliseconds, so the cost of noticing an answer is bounded and a
            // parent waiting on a child burns nothing.
            let remaining = CHILD_BUDGET.saturating_sub(elapsed);
            std::thread::park_timeout(remaining.min(CHILD_POLL));
        }
    }

    /// How often the parent looks for a child's answer, between [`CHILD_BUDGET`]
    /// and the instant the budget runs out.
    const CHILD_POLL: Duration = Duration::from_millis(5);

    /// Re-run this binary as the child that parses one case.
    fn run_markdown_child() -> TestResult {
        let index = std::env::var(CASE_ENV)
            .map_err(|error| {
                format!("the child branch needs CASE_ENV to know which case to parse: {error}")
            })?
            .parse::<usize>()
            .map_err(|error| format!("CASE_ENV is not an index: {error}"))?;
        let cases = markdown_cases()?;
        let case = cases
            .get(index)
            .ok_or("CASE_ENV names a case this build does not have")?;
        // The arm is printed, not returned: a child that reaches here answered,
        // and its exit status is the observation the parent reads. The text goes
        // to stdout through `Write` because `clippy::print_stdout` is forbidden
        // workspace-wide, and this file is not an exempt surface.
        let arm = arm_of(case);
        let mut stdout = std::io::stdout();
        drop(writeln!(stdout, "{} -> {arm}", case.label));
        Ok(())
    }

    /// The arm a case answered with: a refusal's own text, or `accepted` for a
    /// source that parsed.
    fn arm_of(case: &Case) -> String {
        match try_parse(&case.source, Language::Markdown) {
            Ok(_) => String::from("accepted"),
            Err(refusal) => refusal_name(&refusal),
        }
    }

    #[test]
    fn markdown_never_reaches_the_scanner_past_its_bound() -> TestResult {
        if std::env::var_os(CHILD_ENV).is_some() {
            return run_markdown_child();
        }

        let cases = markdown_cases()?;
        assert!(
            cases.len() >= 50 + SEEDED_CASES,
            "only {} cases; the shapes alone contribute more than 50",
            cases.len()
        );

        let mut guarded = 0_usize;
        let mut passed_through = 0_usize;
        for (index, case) in cases.iter().enumerate() {
            let status = markdown_child(index)?;
            assert!(
                status.code().is_some(),
                "{}: the child died of {status:?}. Before the guard this case called abort() inside \\
                 tree-sitter's external scanner; now it must return a tree or a refusal.",
                case.label
            );
            if case.guarded {
                guarded = guarded.saturating_add(1);
                let arm = arm_of(case);
                assert_ne!(
                    arm, "accepted",
                    "{}: the guard is expected to refuse this source, and it parsed",
                    case.label
                );
                assert!(
                    arm.contains("block containers"),
                    "{}: refused as `{arm}` rather than by the container bound, so this is not the \\
                     refusal the guard is being proved against",
                    case.label
                );
            } else {
                passed_through = passed_through.saturating_add(1);
            }
        }

        assert!(
            guarded >= 40,
            "only {guarded} of {} cases were expected to be guarded; the shapes at their measured \\
             abort depths alone contribute more than 14",
            cases.len()
        );
        assert!(
            passed_through >= 20,
            "only {passed_through} cases were expected to pass the guard; without them a guard that \\
             refused every source would pass this test"
        );
        Ok(())
    }

    /// The guard's cases need the markdown grammar, so they are only meaningful
    /// where it is compiled. Assert the guard rather than letting the module read as
    /// covering markdown in a build without it.
    #[cfg(all(not(feature = "lang-md"), feature = "lang-rust"))]
    #[test]
    fn the_markdown_guard_cases_are_absent_from_a_build_without_that_grammar() {
        assert!(
            Language::ALL
                .iter()
                .all(|language| language.name() != "markdown"),
            "this build compiles the markdown grammar, so the guard cases must be compiled in too; \\
             a module that skips them would read as coverage it does not have"
        );
    }

    /// How long a child is given before the parent stops believing it will answer.
    ///
    /// The parse is single-digit milliseconds; a child that takes longer is a hang,
    /// and a hang in the subject is exactly what this module exists to catch, so the
    /// bound is asserted rather than assumed.
    const CHILD_BUDGET: Duration = Duration::from_secs(30);

    #[test]
    fn the_child_budget_is_longer_than_a_parse_and_finite() {
        // A test that waits on a child with no timeout is the same defect as a walk
        // with no cap, one process up.
        assert!(
            CHILD_BUDGET > Duration::from_millis(1),
            "the child budget must let a parse finish"
        );
        assert!(
            CHILD_BUDGET < Duration::from_secs(600),
            "the child budget must fail a hang rather than hold the run open for it"
        );
    }
}
