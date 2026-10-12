//! The scan under hostile input: a file the checked parse will not admit is a
//! typed refusal, never an abort and never a pass (INV-SCAN-ZERO, ADR 0001,
//! #408).
//!
//! The reader that scanned before the port recursed without a bound: a
//! 10,000-deep expression overflowed an 8 MiB stack and killed the gate with
//! exit 134 (`docs/adr/0001-one-rust-reader.md`, "depth 10,000, 100,000 and
//! 1,000,000"). The checked parse bounds depth at [`MAX_AST_DEPTH`] before a
//! detector sees the tree, so these rows are the same depths, read through the
//! scan, answered as [`ParseError::AstTooDeep`].

#![cfg(feature = "scan")]

use std::error::Error;
use std::thread;

use lgwks_ast::scan::{ScanError, scan_source};
use lgwks_ast::{MAX_AST_DEPTH, MAX_SOURCE_BYTES, ParseError};

type TestResult = Result<(), Box<dyn Error>>;

/// One function whose body is a balanced parenthesised expression `depth` levels
/// deep: the hostile row of `examples/rust_readers.rs nest`.
fn nested(depth: usize) -> String {
    format!(
        "fn f() {{ let _ = {}1{}; }}\n",
        "(".repeat(depth),
        ")".repeat(depth)
    )
}

#[test]
fn nesting_past_the_bound_is_a_typed_refusal_at_every_depth_the_adr_measured() -> TestResult {
    for depth in [10_000_usize, 100_000, 1_000_000] {
        let source = nested(depth);
        assert!(
            source.len() <= MAX_SOURCE_BYTES,
            "the {depth}-deep row fits the byte ceiling, so the depth bound is what refuses it"
        );
        match scan_source(&source, "deep.rs") {
            Err(ScanError::Refused {
                path,
                refusal:
                    ParseError::AstTooDeep {
                        observed, limit, ..
                    },
            }) => {
                assert_eq!(path, "deep.rs", "the refusal names the file");
                assert_eq!(limit, MAX_AST_DEPTH, "the refusal names the bound");
                assert!(
                    observed > limit,
                    "{observed} is past {limit} at depth {depth}"
                );
            }
            other => return Err(format!("depth {depth} answered {other:?}").into()),
        }
    }
    Ok(())
}

#[test]
fn a_refusal_is_not_a_clean_scan_and_names_its_bound() -> TestResult {
    let refusal = scan_source(&nested(10_000), "deep.rs");
    let rendered = match refusal {
        Err(error) => error.to_string(),
        Ok(hits) => return Err(format!("a refused file scanned clean: {hits:?}").into()),
    };
    assert!(
        rendered.starts_with("deep.rs: refused before any detector ran:")
            && rendered.contains("nests deeper than 512 levels"),
        "the message says what bound refused it: {rendered}"
    );
    Ok(())
}

#[test]
fn a_source_past_the_byte_ceiling_is_refused_before_it_is_parsed() -> TestResult {
    let source = "//".repeat(MAX_SOURCE_BYTES);
    match scan_source(&source, "big.rs") {
        Err(ScanError::Refused {
            refusal: ParseError::SourceTooLarge { actual, limit },
            ..
        }) => {
            assert_eq!(actual, source.len(), "the refusal reports the size it saw");
            assert_eq!(limit, MAX_SOURCE_BYTES, "and the bound it applied");
            Ok(())
        }
        other => Err(format!("an oversized file answered {other:?}").into()),
    }
}

#[test]
fn bad_syntax_is_unparseable_and_names_the_line_of_the_first_damage() -> TestResult {
    match scan_source("fn ok() {}\nfn torn(\n", "torn.rs") {
        Err(ScanError::Unparseable { path, cause }) => {
            assert_eq!(path, "torn.rs", "the refusal names the file");
            assert!(
                cause.contains("line 2"),
                "and the line of the damage: {cause}"
            );
            Ok(())
        }
        other => Err(format!("a torn file answered {other:?}").into()),
    }
}

/// One way of nesting, with the depth that lands just inside the bound.
struct Shape {
    /// What this shape nests.
    name: &'static str,
    /// Opens one level, on one line.
    open: &'static str,
    /// Closes it.
    close: &'static str,
    /// The repetitions that land the tree a few levels inside [`MAX_AST_DEPTH`]:
    /// a statement, the construct and its block make two to five tree levels each.
    levels: usize,
}

const SHAPES: [Shape; 4] = [
    Shape {
        name: "blocks",
        open: "{\n",
        close: "}\n",
        levels: 248,
    },
    Shape {
        name: "ifs",
        open: "if c {\n",
        close: "}\n",
        levels: 165,
    },
    Shape {
        name: "closures",
        open: "let g = || {\n",
        close: "};\n",
        levels: 165,
    },
    Shape {
        name: "matches",
        open: "match c { _ => {\n",
        close: "} }\n",
        levels: 99,
    },
];

/// The source of `shape` nested `levels` deep, with one allowance written at the
/// innermost point so a scan that reaches it reports it.
fn nest_shape(shape: &Shape, levels: usize) -> String {
    format!(
        "fn f(c: bool) {{\n{}#[allow(clippy::all)]\nlet x = 1;\n{}}}\n",
        shape.open.repeat(levels),
        shape.close.repeat(levels)
    )
}

#[test]
fn a_tree_at_the_depth_bound_is_scanned_on_a_small_stack_in_every_shape() -> TestResult {
    // The detectors recurse over the tree, so what bounds their stack is the
    // parse's depth bound. A thread of 512 KiB, a quarter of a test thread's
    // default and a sixteenth of the main thread's, reads each deepest shape.
    for shape in &SHAPES {
        let source = nest_shape(shape, shape.levels);
        let found = thread::scope(|scope| {
            thread::Builder::new()
                .stack_size(512 * 1024)
                .spawn_scoped(scope, || scan_source(&source, "deepest.rs"))
                .map_err(|error| format!("{}: no thread: {error}", shape.name))?
                .join()
                .map_err(|_| format!("{}: the scan thread died", shape.name))
        })?;
        let hits = found.map_err(|error| format!("{}: {error}", shape.name))?;
        let reported: Vec<(usize, &str)> =
            hits.iter().map(|hit| (hit.line(), hit.rule())).collect();
        assert_eq!(
            reported,
            vec![(shape.levels.saturating_add(2), "ALLOW-SILENCE")],
            "{}: the allowance at the bottom of the nest is the one finding",
            shape.name
        );
    }
    Ok(())
}

#[test]
fn a_few_levels_past_each_shape_is_the_depth_refusal() -> TestResult {
    // The sandwich: the levels above are inside the bound, and these are not, so
    // the previous test reads trees that are close to the bound and not far from it.
    for shape in &SHAPES {
        let source = nest_shape(shape, shape.levels.saturating_add(12));
        match scan_source(&source, "past.rs") {
            Err(ScanError::Refused {
                refusal: ParseError::AstTooDeep { .. },
                ..
            }) => {}
            other => return Err(format!("{}: answered {other:?}", shape.name).into()),
        }
    }
    Ok(())
}
