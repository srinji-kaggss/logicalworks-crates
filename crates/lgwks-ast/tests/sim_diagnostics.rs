//! Deterministic simulation of located diagnostics (INV-AST-2, #194).
//!
//! One seed writes one source file from clean and malformed fragments:
//! multi-byte comments and strings, CRLF and LF line ends, blank lines, and
//! enough broken items that some files carry more recovery nodes than the
//! checked parser retains. The shipped parser, walk and renderer run on it
//! through the public API, and every position they report is checked against
//! a model computed from the source text alone: a line is one more than the
//! newlines before the byte, a column is one more than the characters between
//! the line start and the byte.
//!
//! The model never consults the parser to decide where a position should be,
//! so a span resolved against the wrong text, a byte column reported as a
//! character column, or a 0-based number leaking into a 1-based report
//! disagrees with it for some seed.
//!
//! The seed substrate (`Rng`, `Trace`) is the one the `lgwks_bot` simulation
//! families run on, included by path, so a seed means the same draw sequence
//! in every suite.

#![cfg(feature = "lang-rust")]

#[path = "../../lgwks-bot/tests/sim/seed.rs"]
mod seed;

use std::collections::BTreeSet;
use std::error::Error;

use lgwks_ast::diagnostic::{diagnostics, end_of};
use lgwks_ast::{
    Diagnostic, Language, MAX_SOURCE_BYTES, MAX_SYNTAX_DIAGNOSTICS, ParseError, Severity,
    inspect_ast, parse, tree_diagnostics, tree_recovery_count, try_parse,
};
use seed::{Rng, Trace};

/// What every family returns: a refusal reports itself.
type TestResult = Result<(), Box<dyn Error>>;

/// Sources written per family.
const SEEDS: u64 = 64;

/// The base every family's seeds are derived from.
const BASE_SEED: u64 = 0xd1a6_0057_2026_0930;

/// Rust fragments that parse cleanly. `{n}` is replaced by the fragment's
/// index so names stay unique.
const CLEAN_RUST: [&str; 6] = [
    "fn f{n}() {}",
    "// \u{e9}t\u{e9} \u{65e5}\u{672c} comment {n}",
    "fn g{n}() { let s{n} = \"h\u{e9}llo \u{65e5}\u{672c}\"; }",
    "struct S{n} { field: u32 }",
    "const C{n}: u32 = {n};",
    "/* \u{3a9} block */ fn h{n}() -> u32 { {n} }",
];

/// Rust fragments that force a recovery node.
const BROKEN_RUST: [&str; 6] = [
    "fn broken{n}( {",
    "fn k{n}() { let = ; }",
    "struct Open{n} {",
    "}}",
    "fn r{n}() -> {}",
    "impl for {}",
];

/// Line endings, CRLF included: a carriage return is a character on its line,
/// so it moves the column of anything after it on that line.
const ENDINGS: [&str; 3] = ["\n", "\r\n", "\n\n"];

/// File labels, multi-byte and spaced ones included.
const LABELS: [&str; 4] = [
    "src/main.rs",
    "src/\u{e9}t\u{e9}.rs",
    "dir with space/lib.rs",
    "a.rs",
];

/// The seed for run `index` of `family`; families take disjoint streams.
fn seed_for(family: u64, index: u64) -> u64 {
    BASE_SEED
        ^ family.wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ index.wrapping_mul(0x2545_f491_4f6c_dd1d)
}

/// One element of `pool`, chosen by `rng`.
fn pick<'pool>(rng: &mut Rng, pool: &[&'pool str]) -> &'pool str {
    let count = u32::try_from(pool.len()).unwrap_or(1);
    pool.get(usize::try_from(rng.below(count)).unwrap_or(0))
        .copied()
        .unwrap_or("")
}

/// A seeded source of up to `max_fragments` fragments, broken ones drawn
/// with probability `broken_per_mille`.
fn write_source(
    rng: &mut Rng,
    clean: &[&str],
    broken: &[&str],
    broken_per_mille: u32,
    max_fragments: u32,
) -> String {
    let fragments = rng.between(1, max_fragments);
    let mut source = String::new();
    for index in 0..fragments {
        let pool = if rng.chance(broken_per_mille) {
            broken
        } else {
            clean
        };
        source.push_str(&pick(rng, pool).replace("{n}", &index.to_string()));
        source.push_str(pick(rng, &ENDINGS));
    }
    source
}

/// A seeded Rust source mixing clean and broken fragments.
fn mixed_rust(rng: &mut Rng) -> String {
    let broken_per_mille = rng.between(0, 700);
    write_source(rng, &CLEAN_RUST, &BROKEN_RUST, broken_per_mille, 80)
}

/// The model: the 1-based line and character column of `byte` in `source`.
fn model_position(source: &str, byte: usize) -> Option<(usize, usize)> {
    let before = source.get(..byte)?;
    let line = before.matches('\n').count().saturating_add(1);
    let line_start = before.rfind('\n').map_or(0, |at| at.saturating_add(1));
    let column = before.get(line_start..)?.chars().count().saturating_add(1);
    Some((line, column))
}

/// Runs `family` over [`SEEDS`] mixed sources.
fn sweep(family: u64, mut check: impl FnMut(u64, &mut Rng, &str) -> TestResult) -> TestResult {
    for index in 0..SEEDS {
        let seed = seed_for(family, index);
        let mut rng = Rng::new(seed);
        let source = mixed_rust(&mut rng);
        check(seed, &mut rng, &source).map_err(|error| format!("seed {seed:#x}: {error}"))?;
    }
    Ok(())
}

/// Every located finding in `source`, through the tree-owned entry point.
fn findings(source: &str) -> Vec<Diagnostic> {
    let tree = parse(source, Language::Rust);
    tree_diagnostics("sim.rs", &tree, Language::Rust.name())
}

// ── Positions ──────────────────────────────────────────────────────────────

#[test]
/// A finding's line is one more than the newlines before its byte.
fn a_finding_line_is_one_more_than_the_newlines_before_it() -> TestResult {
    sweep(1, |seed, _, source| {
        for found in findings(source) {
            for pos in [found.span().start, found.span().end] {
                let (line, _) = model_position(source, pos.byte).ok_or("byte off a boundary")?;
                assert_eq!(
                    pos.line, line,
                    "seed {seed:#x}: byte {} is on line {line}",
                    pos.byte
                );
            }
        }
        Ok(())
    })
}

#[test]
/// A finding's column counts characters, not bytes, from its line start.
fn a_finding_column_counts_characters_from_the_line_start() -> TestResult {
    sweep(2, |seed, _, source| {
        for found in findings(source) {
            for pos in [found.span().start, found.span().end] {
                let (_, column) = model_position(source, pos.byte).ok_or("byte off a boundary")?;
                assert_eq!(
                    pos.column, column,
                    "seed {seed:#x}: byte {} is column {column}",
                    pos.byte
                );
            }
        }
        Ok(())
    })
}

#[test]
/// Every span lies inside the source, ends ordered, on character boundaries.
fn every_span_is_inside_the_source_on_character_boundaries() -> TestResult {
    sweep(3, |seed, _, source| {
        for found in findings(source) {
            let range = found.span().byte_range();
            assert!(
                range.start <= range.end,
                "seed {seed:#x}: span {range:?} runs backwards"
            );
            assert!(
                range.end <= source.len(),
                "seed {seed:#x}: span {range:?} leaves the source"
            );
            assert!(
                source.is_char_boundary(range.start) && source.is_char_boundary(range.end),
                "seed {seed:#x}: span {range:?} splits a character"
            );
        }
        Ok(())
    })
}

#[test]
/// Findings are listed in source order.
fn findings_come_back_in_source_order() -> TestResult {
    sweep(4, |seed, _, source| {
        let ranges: Vec<(usize, usize)> = findings(source)
            .iter()
            .map(|found| (found.span().start.byte, found.span().end.byte))
            .collect();
        let mut sorted = ranges.clone();
        sorted.sort_unstable();
        assert_eq!(
            ranges, sorted,
            "seed {seed:#x}: findings are out of source order"
        );
        Ok(())
    })
}

#[test]
/// One finding per recovery node, every one a warning naming its grammar.
fn one_warning_per_recovery_node_naming_the_grammar() -> TestResult {
    sweep(5, |seed, _, source| {
        let tree = parse(source, Language::Rust);
        let found = tree_diagnostics("sim.rs", &tree, Language::Rust.name());
        assert_eq!(
            found.len(),
            tree_recovery_count(&tree),
            "seed {seed:#x}: count mismatch"
        );
        for one in &found {
            assert_eq!(
                one.severity(),
                Severity::Warning,
                "seed {seed:#x}: a recovery is not a warning"
            );
            assert!(
                one.message().contains(Language::Rust.name()),
                "seed {seed:#x}: {} does not name the grammar",
                one.message()
            );
        }
        Ok(())
    })
}

#[test]
/// The tree-owned entry point and the free function agree when handed the
/// same text.
fn the_tree_owned_and_free_entry_points_agree() -> TestResult {
    sweep(6, |seed, _, source| {
        let tree = parse(source, Language::Rust);
        assert_eq!(
            tree_diagnostics("sim.rs", &tree, "rust"),
            diagnostics("sim.rs", &tree.root(), source, "rust"),
            "seed {seed:#x}: the two entry points disagree"
        );
        Ok(())
    })
}

// ── The checked parse ──────────────────────────────────────────────────────

#[test]
/// A source written only from clean fragments parses, checked, with nothing
/// to report.
fn a_clean_source_passes_the_checked_parse_with_no_findings() -> TestResult {
    for index in 0..SEEDS {
        let seed = seed_for(7, index);
        let mut rng = Rng::new(seed);
        let source = write_source(&mut rng, &CLEAN_RUST, &BROKEN_RUST, 0, 60);
        let tree = try_parse(&source, Language::Rust)
            .map_err(|error| format!("seed {seed:#x}: {error}"))?;
        assert!(
            tree_diagnostics("clean.rs", &tree, "rust").is_empty(),
            "seed {seed:#x}: a clean source reported findings"
        );
    }
    Ok(())
}

#[test]
/// The checked parse refuses exactly the sources that carry recovery nodes.
fn the_checked_parse_refuses_exactly_when_recovery_nodes_exist() -> TestResult {
    sweep(8, |seed, _, source| {
        let recoveries = tree_recovery_count(&parse(source, Language::Rust));
        match try_parse(source, Language::Rust) {
            Ok(_) => assert_eq!(
                recoveries, 0,
                "seed {seed:#x}: {recoveries} recoveries accepted"
            ),
            Err(ParseError::InvalidSyntax { .. }) => {
                assert!(recoveries > 0, "seed {seed:#x}: a clean tree was refused");
            }
            Err(other) => return Err(format!("seed {seed:#x}: unexpected refusal {other}").into()),
        }
        Ok(())
    })
}

/// A refused source's retained byte ranges, its truncation flag, and the
/// refusal itself.
type Refusal = (Vec<(usize, usize)>, bool, ParseError);

/// The retained diagnostics of a refused source, with its truncation flag.
fn refusal(source: &str) -> Option<Refusal> {
    let error = try_parse(source, Language::Rust).err()?;
    let ParseError::InvalidSyntax {
        ref diagnostics,
        diagnostics_truncated,
        ..
    } = error
    else {
        return None;
    };
    let ranges = diagnostics
        .iter()
        .map(|found| (found.start_byte, found.end_byte))
        .collect();
    Some((ranges, diagnostics_truncated, error))
}

#[test]
/// The refusal retains at most the ceiling and says when it dropped some.
fn the_refusal_retains_at_most_the_ceiling_and_reports_truncation() -> TestResult {
    let mut truncated_seen = false;
    sweep(9, |seed, _, source| {
        let recoveries = tree_recovery_count(&parse(source, Language::Rust));
        let Some((ranges, truncated, _)) = refusal(source) else {
            return Ok(());
        };
        assert!(
            ranges.len() <= MAX_SYNTAX_DIAGNOSTICS,
            "seed {seed:#x}: over the ceiling"
        );
        assert_eq!(
            truncated,
            recoveries > MAX_SYNTAX_DIAGNOSTICS,
            "seed {seed:#x}: {recoveries} recoveries, truncated={truncated}"
        );
        assert_eq!(
            ranges.len(),
            recoveries.min(MAX_SYNTAX_DIAGNOSTICS),
            "seed {seed:#x}: retained the wrong number"
        );
        truncated_seen |= truncated;
        Ok(())
    })?;
    assert!(
        truncated_seen,
        "no seed exceeded the ceiling, so truncation was never exercised"
    );
    Ok(())
}

#[test]
/// What the refusal retains is the earliest recovery nodes, in source order.
fn the_refusal_keeps_the_earliest_recovery_nodes_in_source_order() -> TestResult {
    sweep(10, |seed, _, source| {
        let Some((ranges, _, _)) = refusal(source) else {
            return Ok(());
        };
        let earliest: Vec<(usize, usize)> = findings(source)
            .iter()
            .take(MAX_SYNTAX_DIAGNOSTICS)
            .map(|found| (found.span().start.byte, found.span().end.byte))
            .collect();
        assert_eq!(
            ranges, earliest,
            "seed {seed:#x}: the retained set is not the earliest"
        );
        Ok(())
    })
}

#[test]
/// A rendered syntax refusal is an error at the earliest recovery node.
fn a_syntax_refusal_points_at_the_earliest_recovery_node() -> TestResult {
    sweep(11, |seed, rng, source| {
        let Some((_, _, error)) = refusal(source) else {
            return Ok(());
        };
        let label = pick(rng, &LABELS);
        let reported = error.to_diagnostic(label, source);
        let first = findings(source)
            .first()
            .map(|found| found.span().start)
            .ok_or("a refusal with no findings")?;
        assert_eq!(
            reported.severity(),
            Severity::Error,
            "seed {seed:#x}: a refusal is not an error"
        );
        assert_eq!(
            reported.span().start,
            first,
            "seed {seed:#x}: not the earliest node"
        );
        assert_eq!(
            reported.message(),
            error.to_string(),
            "seed {seed:#x}: the text is not the refusal's"
        );
        assert_eq!(
            reported.file(),
            std::path::Path::new(label),
            "seed {seed:#x}: wrong file"
        );
        Ok(())
    })
}

#[test]
/// A whole-file refusal is zero-width at the end of the source.
fn a_whole_file_refusal_is_zero_width_at_the_end() -> TestResult {
    sweep(12, |seed, rng, source| {
        let actual = usize::try_from(rng.next_u64()).unwrap_or(usize::MAX);
        let reported = ParseError::SourceTooLarge {
            actual,
            limit: MAX_SOURCE_BYTES,
        }
        .to_diagnostic("big.rs", source);
        let span = reported.span();
        assert!(
            span.is_empty(),
            "seed {seed:#x}: a whole-file condition has width"
        );
        assert_eq!(
            span.start.byte,
            source.len(),
            "seed {seed:#x}: not at the end"
        );
        let (line, column) = model_position(source, source.len()).ok_or("end off a boundary")?;
        assert_eq!(
            (span.start.line, span.start.column),
            (line, column),
            "seed {seed:#x}: wrong end position"
        );
        Ok(())
    })
}

#[test]
/// A source over the byte bound is refused before parsing, naming both sizes.
fn an_oversized_source_is_refused_naming_both_sizes() -> TestResult {
    for index in 0..4 {
        let seed = seed_for(13, index);
        let mut rng = Rng::new(seed);
        let unit = mixed_rust(&mut rng);
        let mut source = String::with_capacity(MAX_SOURCE_BYTES.saturating_add(unit.len()));
        while source.len() <= MAX_SOURCE_BYTES {
            source.push_str(&unit);
        }
        match try_parse(&source, Language::Rust) {
            Err(ParseError::SourceTooLarge { actual, limit }) => {
                assert_eq!(actual, source.len(), "seed {seed:#x}: wrong actual size");
                assert_eq!(limit, MAX_SOURCE_BYTES, "seed {seed:#x}: wrong limit");
            }
            Err(other) => return Err(format!("seed {seed:#x}: refused as {other}").into()),
            Ok(_) => return Err(format!("seed {seed:#x}: an oversized source parsed").into()),
        }
    }
    Ok(())
}

// ── Rendering ──────────────────────────────────────────────────────────────

#[test]
/// `end_of` is the model's position of the last byte, zero-width.
fn the_end_of_a_source_matches_the_model() -> TestResult {
    sweep(14, |seed, _, source| {
        let span = end_of(source);
        let (line, column) = model_position(source, source.len()).ok_or("end off a boundary")?;
        assert!(span.is_empty(), "seed {seed:#x}: end_of has width");
        assert_eq!(
            (span.start.line, span.start.column, span.start.byte),
            (line, column, source.len()),
            "seed {seed:#x}: end_of disagrees with the model"
        );
        Ok(())
    })
}

#[test]
/// A rendered finding is `path:line:column: W: message`, from its own fields.
fn a_rendered_finding_matches_its_fields() -> TestResult {
    sweep(15, |seed, rng, source| {
        let label = pick(rng, &LABELS);
        let tree = parse(source, Language::Rust);
        for found in tree_diagnostics(label, &tree, "rust") {
            let start = found.span().start;
            assert_eq!(
                found.render(),
                format!(
                    "{label}:{}:{}: W: {}",
                    start.line,
                    start.column,
                    found.message()
                ),
                "seed {seed:#x}: the render does not match the fields"
            );
        }
        Ok(())
    })
}

#[test]
/// An unlabelled finding renders a placeholder rather than a bare colon.
fn an_unlabelled_finding_renders_a_placeholder() -> TestResult {
    sweep(16, |seed, _, source| {
        let tree = parse(source, Language::Rust);
        for found in tree_diagnostics("", &tree, "rust") {
            assert!(
                found.render().starts_with("<source>:"),
                "seed {seed:#x}: {} reads as a truncated filename",
                found.render()
            );
        }
        Ok(())
    })
}

#[test]
/// Relabelling or escalating a finding keeps what it claims and where.
fn relabelling_a_finding_keeps_its_claim_and_place() -> TestResult {
    sweep(17, |seed, rng, source| {
        let label = pick(rng, &LABELS);
        for found in findings(source) {
            let moved = found.clone().in_file(label).with_severity(Severity::Error);
            assert_eq!(moved.span(), found.span(), "seed {seed:#x}: the span moved");
            assert_eq!(
                moved.message(),
                found.message(),
                "seed {seed:#x}: the message changed"
            );
            assert_eq!(
                moved.severity(),
                Severity::Error,
                "seed {seed:#x}: severity not applied"
            );
            assert!(
                moved.render().contains(": E: "),
                "seed {seed:#x}: {}",
                moved.render()
            );
        }
        Ok(())
    })
}

// ── Inspection ─────────────────────────────────────────────────────────────

#[test]
/// A bounded inspection is complete exactly when the tree fits its limit, and
/// an incomplete one stops one node past it.
fn a_bounded_inspection_is_complete_exactly_within_its_limit() -> TestResult {
    sweep(18, |seed, rng, source| {
        let tree = parse(source, Language::Rust);
        let nodes = inspect_ast(&tree.root(), None).nodes;
        let high = u32::try_from(nodes.saturating_add(2)).unwrap_or(u32::MAX);
        let limit = usize::try_from(rng.between(0, high)).unwrap_or(0);
        let bounded = inspect_ast(&tree.root(), Some(limit));
        assert_eq!(
            bounded.complete,
            nodes <= limit,
            "seed {seed:#x}: {nodes} nodes, limit {limit}"
        );
        assert_eq!(
            bounded.node_limit,
            Some(limit),
            "seed {seed:#x}: the limit is not reported"
        );
        if bounded.complete {
            assert_eq!(
                bounded.nodes, nodes,
                "seed {seed:#x}: a complete walk missed nodes"
            );
            assert!(
                bounded.stop_reason.is_none(),
                "seed {seed:#x}: complete but stopped"
            );
        } else {
            assert_eq!(
                bounded.nodes,
                limit.saturating_add(1),
                "seed {seed:#x}: stopped at the wrong node"
            );
            assert!(
                bounded.stop_reason.is_some(),
                "seed {seed:#x}: incomplete without a reason"
            );
        }
        Ok(())
    })
}

#[test]
/// Inspection flags recovery exactly when there are findings to report.
fn inspection_flags_recovery_exactly_when_there_are_findings() -> TestResult {
    sweep(19, |seed, _, source| {
        let tree = parse(source, Language::Rust);
        let metrics = inspect_ast(&tree.root(), None);
        assert!(
            metrics.complete,
            "seed {seed:#x}: an unbounded walk is incomplete"
        );
        assert_eq!(
            metrics.has_syntax_issues,
            !findings(source).is_empty(),
            "seed {seed:#x}: the metrics and the findings disagree"
        );
        Ok(())
    })
}

// ── A second grammar ───────────────────────────────────────────────────────

#[cfg(feature = "lang-python")]
#[test]
/// The same position model holds for another grammar's recovery nodes.
fn positions_hold_for_a_second_grammar() -> TestResult {
    /// Python fragments that parse cleanly.
    const CLEAN_PYTHON: [&str; 4] = [
        "def f{n}():\n    return {n}",
        "# \u{e9}t\u{e9} \u{65e5}\u{672c} {n}",
        "s{n} = \"h\u{e9}llo \u{65e5}\u{672c}\"",
        "class K{n}:\n    pass",
    ];
    /// Python fragments that force a recovery node.
    const BROKEN_PYTHON: [&str; 4] = ["def broken{n}(:", "x{n} = = 1", "class C{n}(", "return )"];
    for index in 0..SEEDS {
        let seed = seed_for(20, index);
        let mut rng = Rng::new(seed);
        let source = write_source(&mut rng, &CLEAN_PYTHON, &BROKEN_PYTHON, 500, 40);
        let tree = parse(&source, Language::Python);
        let found = tree_diagnostics("sim.py", &tree, Language::Python.name());
        assert_eq!(
            found.len(),
            tree_recovery_count(&tree),
            "seed {seed:#x}: count mismatch"
        );
        for one in found {
            let start = one.span().start;
            let model = model_position(&source, start.byte).ok_or("byte off a boundary")?;
            assert_eq!(
                (start.line, start.column),
                model,
                "seed {seed:#x}: python position is off"
            );
        }
    }
    Ok(())
}

// ── Replay ─────────────────────────────────────────────────────────────────

/// One seed's whole scenario: write, parse, locate, refuse, render.
fn scenario(seed: u64) -> Trace {
    let mut rng = Rng::new(seed);
    let source = mixed_rust(&mut rng);
    let mut trace = Trace::new();
    trace.record_count("bytes", source.len());
    for found in findings(&source) {
        trace.record(&found.render());
    }
    match try_parse(&source, Language::Rust) {
        Ok(_) => trace.record("accepted"),
        Err(error) => trace.record(&error.to_diagnostic("sim.rs", &source).render()),
    }
    trace
}

#[test]
/// The same seed replays to the same trace hash.
fn the_same_seed_replays_to_the_same_trace() {
    for index in 0..SEEDS {
        let seed = seed_for(21, index);
        let first = scenario(seed);
        assert!(
            !first.is_empty(),
            "seed {seed:#x}: the scenario recorded nothing"
        );
        assert_eq!(
            first.hash(),
            scenario(seed).hash(),
            "seed {seed:#x}: the replay diverged"
        );
    }
}

#[test]
/// Different seeds write different sources.
fn different_seeds_write_different_sources() {
    let hashes: BTreeSet<u64> = (0..SEEDS)
        .map(|index| scenario(seed_for(22, index)).hash())
        .collect();
    let distinct = u64::try_from(hashes.len()).unwrap_or(0);
    assert!(
        distinct.saturating_mul(4) >= SEEDS.saturating_mul(3),
        "only {distinct} distinct traces from {SEEDS} seeds"
    );
}
