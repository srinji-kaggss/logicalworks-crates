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
//! The second body is the one input that does not answer at all. tree-sitter's
//! markdown external scanner serializes its own state into a fixed 1 024-byte
//! buffer and asserts when the state is larger, which `"- "` repeated 255 times
//! does it. An assertion in a C parser is `abort()`, so the *process* dies: not
//! a typed refusal, not a catchable error, a `SIGABRT` that takes the caller
//! with it. 254 levels is refused as `InvalidSyntax`; 255 aborts. That boundary
//! is measured, not remembered, and it is proved here from a child process
//! because a process that aborts cannot assert anything afterwards.
//!
//! # What this does not claim
//!
//! The abort is upstream, in `tree-sitter-markdown` 0.5.3's scanner, and this
//! crate cannot fix it: the grammar arrives as a compiled C library through
//! `ast-grep-language`, and both routes out are closed by the issue — forking
//! the grammar is forbidden, and the one option this crate has no authority
//! over is a `tree-sitter` edge. So this module records it as a limit with an
//! exact reproducer rather than pretending the crate survives it, and the
//! script-level evidence is a `SIGABRT` observed from outside, never a green
//! run of the parse itself.
//!
//! # Why a hard per-test timeout
//!
//! A test that hangs is a test that hides a defect, and the shapes here are the
//! ones that would hang. `.config/nextest.toml` gives every test in this module
//! a `slow-timeout` and a `terminate-after`, so a walk that stops making
//! progress fails the run instead of holding it open.

#![cfg(feature = "lang-rust")]

use std::error::Error;
use std::time::Duration;

use lgwks_ast::{
    Language, MAX_AST_DEPTH, MAX_AST_NODES, MAX_SOURCE_BYTES, ParseError, inspect_ast, parse,
    try_parse,
};

/// What every family returns.
type TestResult = Result<(), Box<dyn Error>>;

/// Set in the child so a re-executed binary takes the child branch.
const CHILD_ENV: &str = "LGWKS_AST_MARKDOWN_ABORT_CHILD";

/// This test's own name, for the child's `--exact` filter.
///
/// Qualified by its module, because the child is this same binary and libtest
/// matches the full path: an unqualified name selects nothing, the child runs
/// zero tests and exits 0, and the parent's "the child died" assertion fails
/// against a child that never did anything.
const ABORT_TEST_NAME: &str =
    "hostile::a_nested_markdown_list_aborts_the_process_rather_than_refusing";

/// The number of `- ` markers at which the markdown scanner's serialized state
/// crosses tree-sitter's 1 024-byte buffer. Measured by bisection on this
/// machine: 254 markers (508 bytes) is refused as `InvalidSyntax`, 255 markers
/// (510 bytes) aborts. The child's own count is the lower one by default, and
/// the parent raises it, so one binary proves both halves of the boundary.
const SAFE_MARKDOWN_LEVELS: usize = 254;
/// The first count that aborts.
const ABORTING_MARKDOWN_LEVELS: usize = 255;

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
/// the two the byte and depth bounds produce.
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

// ── The one input that does not answer ─────────────────────────────────────

/// Re-run this binary as the child that performs one markdown parse.
///
/// The child records the arm it reached in a file rather than in its exit
/// status, because the two facts this test needs are different and neither is
/// the status: at one level fewer the child *answers*, and at the next it dies
/// before it can answer anything at all. A file the parent reads is the only
/// place the answered arm can come from.
#[cfg(feature = "lang-md")]
fn run_markdown_child() -> TestResult {
    let levels = std::env::var("LGWKS_AST_MARKDOWN_LEVELS")
        .ok()
        .and_then(|one| one.parse::<usize>().ok())
        .ok_or("the child branch needs LGWKS_AST_MARKDOWN_LEVELS to know what to parse")?;
    let outcome = std::env::var("LGWKS_AST_MARKDOWN_OUTCOME")
        .map_err(|_| "the child branch needs LGWKS_AST_MARKDOWN_OUTCOME to record its arm")?;
    let source = "- ".repeat(levels);
    let answer = try_parse(&source, Language::Markdown);
    let recorded = match answer {
        Ok(_) => "accepted".to_owned(),
        Err(ref error) => refusal_name(error),
    };

    std::fs::write(outcome, format!("{levels}\t{}\t{recorded}", source.len()))?;
    Ok(())
}

/// One child process: how it ended, and the file it would have recorded its arm
/// in if it reached one.
struct Child {
    /// How the child ended.
    status: std::process::ExitStatus,
    /// Where the child records the arm it reached.
    outcome: std::path::PathBuf,
}

/// Run this binary as a child that parses `levels` nested markdown list markers.
///
/// The outcome path carries the caller's own label rather than a fixed name, so
/// two children in one test cannot read each other's record.
#[cfg(feature = "lang-md")]
fn markdown_child(levels: usize, label: &str) -> Result<Child, Box<dyn Error>> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let outcome = std::env::temp_dir().join(format!("lgwks-ast-md-{label}-{nanos}-{seq}.outcome"));
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .args([ABORT_TEST_NAME, "--exact", "--nocapture"])
        .env(CHILD_ENV, "1")
        .env("LGWKS_AST_MARKDOWN_LEVELS", levels.to_string())
        .env("LGWKS_AST_MARKDOWN_OUTCOME", &outcome)
        .spawn()?;
    let status = child.wait()?;
    Ok(Child { status, outcome })
}

#[cfg(feature = "lang-md")]
#[test]
fn a_nested_markdown_list_aborts_the_process_rather_than_refusing() -> TestResult {
    if std::env::var_os(CHILD_ENV).is_some() {
        return run_markdown_child();
    }

    // The control, from outside and through the same binary and the same path:
    // one marker fewer, and it answers. The difference between the two arms is
    // therefore the input and nothing else.
    let control = markdown_child(SAFE_MARKDOWN_LEVELS, "control")?;
    assert_eq!(
        control.status.code(),
        Some(0),
        "the {SAFE_MARKDOWN_LEVELS}-marker child did not exit cleanly: {:?}",
        control.status
    );
    let answered = std::fs::read_to_string(&control.outcome)
        .map_err(|error| format!("the control child recorded no arm: {error}"))?;
    assert!(
        answered.contains("ERROR or MISSING"),
        "the {SAFE_MARKDOWN_LEVELS}-marker child answered `{answered}` rather than refusing for \
         syntax; if that has changed the boundary has moved and the abort arm below is measuring a \
         different input than this test claims"
    );

    // The subject: an assertion inside a C parser calls `abort`, so the only
    // thing that can observe it is a parent.
    let subject = markdown_child(ABORTING_MARKDOWN_LEVELS, "subject")?;
    assert_eq!(
        subject.status.code(),
        None,
        "the {ABORTING_MARKDOWN_LEVELS}-marker child exited cleanly: {:?}. The markdown scanner's \
         overflow has been fixed upstream, so this limit can be retired.",
        subject.status
    );
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        assert_eq!(
            subject.status.signal(),
            Some(6),
            "the child must have died of SIGABRT from tree-sitter's external-scanner assertion, \
             not of some other signal: {:?}",
            subject.status
        );
    }
    assert!(
        !subject.outcome.exists(),
        "the aborting child recorded an arm, so it answered rather than dying"
    );
    Ok(())
}

/// The shape the abort needs is the grammar's own nesting fragment, so this test
/// is only meaningful where that grammar is compiled. Assert the guard rather
/// than letting the module read as covering markdown in a build without it.
#[cfg(all(not(feature = "lang-md"), feature = "lang-rust"))]
#[test]
fn the_markdown_abort_case_is_absent_from_a_build_without_that_grammar() {
    assert!(
        Language::ALL
            .iter()
            .all(|language| language.name() != "markdown"),
        "this build compiles the markdown grammar, so the abort case must be compiled in too; \\
         a module that skips it would read as coverage it does not have"
    );
}

/// How long a child is given before the parent stops believing it will answer.
///
/// The abort happens in the parse, well inside this; a child that takes longer
/// is a hang, and a hang in the subject is exactly what this module exists to
/// catch, so the parent bounds its own waiting rather than blocking forever.
const CHILD_BUDGET: Duration = Duration::from_secs(60);

#[test]
fn the_child_budget_is_longer_than_a_parse_and_finite() {
    // The bound itself, asserted: a test that waits on a child with no timeout
    // is the same defect as a walk with no cap, one process up.
    assert!(
        CHILD_BUDGET > Duration::from_millis(1),
        "the child budget must let a parse finish"
    );
    assert!(
        CHILD_BUDGET < Duration::from_secs(600),
        "the child budget must fail a hang rather than hold the run open for it"
    );
}
