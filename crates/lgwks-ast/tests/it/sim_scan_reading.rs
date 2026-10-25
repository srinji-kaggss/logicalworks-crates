//! Deterministic simulation of how the scan reads what it is shown: the
//! arguments of a log, the form of a name inside them, and the key path and
//! comment of a manifest line (#699).
//!
//! Each family draws its shape from a seed and checks the verdict against a
//! statement of the rule, never against the reader. A label is not the value it
//! labels; a macro that only ends like a log macro is not one; a format string
//! captures what its braces name and nothing its prose says; a `[..]` after a
//! name is an index and a `(..)` a call; whitespace and comments inside a log
//! change nothing; a log reaches every later statement of its block and into the
//! blocks nested under it, and never a sibling branch, a function, or a return it
//! follows. For a manifest, a `scan-approved:` marker approves only as a comment,
//! the table path and not its spelling decides what is a lint, and the comment
//! block above a declaration ends at a blank line.
//!
//! Sources: the field and argument syntax of `tracing::event!` and
//! `tracing::error!` (tokio-rs/tracing, `tracing::field`); implicit named
//! arguments, `{{` and the `{:w$}` width argument (Rust RFC 2795, `std::fmt`);
//! TOML v1.0.0 (comments, quoted keys, dotted keys); and the Cargo book,
//! "Manifest: the `[lints]` section".
//!
//! | Family | Property it pins |
//! |---|---|
//! | `a_label_is_never_the_value_it_labels` | `error = "boom"` names nothing, `error = ?cause` names `cause` |
//! | `a_macro_that_only_ends_like_a_log_is_not_one` | a log is a whole path segment, not a suffix |
//! | `a_format_capture_is_read_exactly` | captures and width arguments count; prose and `{{` do not |
//! | `the_form_of_a_name_decides_whether_it_is_a_value` | `name[0]` is a value; `name(0)`, `name!(0)`, `Level::name` and `Level.name` are not |
//! | `a_log_reaches_past_any_gap_and_never_back_to_a_return_before_it` | order, not distance, decides |
//! | `whitespace_and_comments_inside_a_log_change_nothing` | the verdict reads tokens, not text |
//! | `a_log_in_an_enclosing_block_reaches_in_and_a_sibling_branch_does_not` | evidence follows the path |
//! | `a_nested_function_inherits_no_log` | a function body starts with none |
//! | `a_marker_inside_a_value_approves_nothing` | only a comment approves |
//! | `the_table_path_and_not_its_spelling_makes_a_lint` | spacing, quoting and dotting do not change the table |
//! | `the_comment_block_above_a_declaration_ends_at_a_blank_line` | an approval is read where it sits |

#![cfg(feature = "scan")]

use std::error::Error;

use lgwks_ast::scan::{Rule, scan_manifest, scan_source_with};

use crate::sim::Rng;
use crate::sim_scan_manifest::{LINTS, REASONS, header};

/// The odd multiplier that spreads consecutive indices across the seed space.
const SPREAD: u64 = 0x9e37_79b9_7f4a_7c15;

/// The line a `returning` function returns on.
const RETURN_LINE: usize = 4;

/// Labels a log argument may carry, dotted ones included.
const LABELS: [&str; 5] = ["e", "error", "cause", "error.message", "err.kind"];

/// Values an error may carry.
const NAMES: [&str; 3] = ["e", "cause", "err"];

/// Macro paths that are log emissions: a level by name through any path, and any
/// macro of a logging crate.
const LOGGING: [&str; 11] = [
    "info",
    "warn",
    "error",
    "debug",
    "trace",
    "event",
    "eprintln",
    "eprint",
    "tracing::span",
    "log::anything",
    "lgwks_std::trace::warn",
];

/// Macro paths that only look like a log: a longer word that ends in a level, a
/// macro of another crate, and a print that is not a log.
const LOOKALIKE: [&str; 9] = [
    "my_info",
    "info_all",
    "catalog::emit",
    "metrics::counter",
    "logger::emit",
    "tracing_ext::emit",
    "trace_id",
    "println",
    "debug_assert",
];

/// Format strings and whether each captures `e`.
const CAPTURES: [(&str, bool); 14] = [
    ("{e}", true),
    ("{e:?}", true),
    ("{e:#?}", true),
    ("{e:>8}", true),
    ("{:e$}", true),
    ("{x:e$}", true),
    ("{x:?} {e}", true),
    ("{{e}}", false),
    ("{}", false),
    ("{0}", false),
    ("{ee}", false),
    ("{e2}", false),
    ("{x}", false),
    ("{e", false),
];

/// Prose that may sit around a format string; the word `e` in it names nothing.
const PROSE: [&str; 3] = ["", "request failed: ", "an e occurred "];

/// Bindings an argument may name by form.
const BINDINGS: [&str; 3] = ["header", "frame", "cause"];

/// Statements that are not logs, of several shapes.
const FILLERS: [&str; 5] = [
    "step();",
    "let n = 1_u32;",
    "if flag { step(); }",
    "while flag { step(); }",
    "{ step(); }",
];

/// What may sit between two tokens of a statement.
const SEPARATORS: [&str; 6] = [" ", "\n", "\t", "   ", " /* note */ ", " // note\n"];

/// The tokens of a log that names the error.
const NAMING: [&str; 11] = [
    "tracing",
    "::",
    "error",
    "!",
    "(",
    "%",
    "e",
    ",",
    "\"failed\"",
    ")",
    ";",
];

/// The tokens of a log that names nothing.
const SILENT: [&str; 6] = ["eprintln", "!", "(", "\"zero\"", ")", ";"];

/// Tables that are lint tables, and tables that only have a lint-shaped name.
const LINT_TABLES: [&str; 4] = [
    "lints",
    "lints.rust",
    "workspace.lints",
    "workspace.lints.clippy",
];

/// Tables that hold a `lints` word and are no lint table.
const OTHER_TABLES: [&str; 4] = [
    "package.metadata.lints",
    "profile.dev.lints",
    "badges.lints",
    "features",
];

/// The seed of the family's `index`-th scenario.
const fn seed_of(index: u64) -> u64 {
    0x5ca6_0000_0000_0000 ^ index.wrapping_mul(SPREAD)
}

/// The lines the unlogged-error rule reports on `text`, with no other rule
/// running.
fn unlogged(text: &str) -> Result<Vec<usize>, Box<dyn Error>> {
    let hits = scan_source_with(text, "sim.rs", &[Rule::UnloggedError])?;
    Ok(hits.iter().map(|hit| hit.line()).collect())
}

/// A function that binds `name`, runs `statement` and returns `Err(name)` on
/// [`RETURN_LINE`].
fn returning(statement: &str, name: &str) -> String {
    format!(
        "fn f() -> Result<(), E> {{\n    let {name} = E::Flat;\n    {statement}\n    return Err({name});\n}}\n"
    )
}

/// The findings of a manifest by line and evidence.
fn manifest_findings(text: &str) -> Vec<(usize, String)> {
    scan_manifest(text)
        .iter()
        .map(|hit| (hit.line(), hit.snippet().to_owned()))
        .collect()
}

/// A label never excuses the value it labels: the same label over a literal
/// names nothing, and over the carried value names it.
#[test]
fn a_label_is_never_the_value_it_labels() -> Result<(), Box<dyn Error>> {
    for index in 0..256_u64 {
        let mut rng = Rng::new(seed_of(index ^ 0x1abe));
        let label = *rng.pick_named("labels", &LABELS)?;
        let name = *rng.pick_named("names", &NAMES)?;
        let literal = returning(
            &format!("tracing::warn!({label} = \"boom\", \"refused\");"),
            name,
        );
        assert_eq!(unlogged(&literal)?, vec![RETURN_LINE], "{literal}");
        let valued = returning(
            &format!("tracing::warn!({label} = ?{name}, \"refused\");"),
            name,
        );
        assert_eq!(unlogged(&valued)?, Vec::<usize>::new(), "{valued}");
    }
    Ok(())
}

/// A macro is a log by its whole last segment or its logging crate, so one that
/// only ends in a level, or lives in another crate, excuses nothing.
#[test]
fn a_macro_that_only_ends_like_a_log_is_not_one() -> Result<(), Box<dyn Error>> {
    let mut logs = 0_usize;
    let mut lookalikes = 0_usize;
    for index in 0..256_u64 {
        let mut rng = Rng::new(seed_of(index ^ 0x100c));
        let is_log = rng.coin();
        let path = if is_log {
            logs = logs.saturating_add(1);
            *rng.pick_named("logging", &LOGGING)?
        } else {
            lookalikes = lookalikes.saturating_add(1);
            *rng.pick_named("lookalikes", &LOOKALIKE)?
        };
        // The return carries no value, so any log on the path is the signal; it
        // sits on line 3 of a function with no binding.
        let source = format!(
            "fn f() -> Result<(), E> {{\n    {path}!(\"x\");\n    return Err(E::Flat);\n}}\n"
        );
        let expected = if is_log { Vec::new() } else { vec![3] };
        assert_eq!(unlogged(&source)?, expected, "{source}");
    }
    assert!(
        logs > 0 && lookalikes > 0,
        "the sweep reached both kinds of macro"
    );
    Ok(())
}

/// A format string names what its braces capture, a width argument included, and
/// nothing its prose or a doubled brace says.
#[test]
fn a_format_capture_is_read_exactly() -> Result<(), Box<dyn Error>> {
    for index in 0..256_u64 {
        let mut rng = Rng::new(seed_of(index ^ 0xca97));
        let &(format, captures) = rng.pick_named("formats", &CAPTURES)?;
        let before = *rng.pick_named("prose", &PROSE)?;
        let after = *rng.pick_named("prose", &PROSE)?;
        let source = returning(&format!("eprintln!(\"{before}{format}{after}\");"), "e");
        let expected = if captures {
            Vec::new()
        } else {
            vec![RETURN_LINE]
        };
        assert_eq!(unlogged(&source)?, expected, "{source}");
    }
    Ok(())
}

/// The form a name is written in decides whether the log names a value: a bare
/// name and an indexed one do, a callee, a macro, a path segment and a field do
/// not.
#[test]
fn the_form_of_a_name_decides_whether_it_is_a_value() -> Result<(), Box<dyn Error>> {
    for index in 0..256_u64 {
        let mut rng = Rng::new(seed_of(index ^ 0x4f0a));
        let name = *rng.pick_named("bindings", &BINDINGS)?;
        let (form, value) = match rng.below(6) {
            0 => (name.to_owned(), true),
            1 => (format!("{name}[0]"), true),
            2 => (format!("{name}(0)"), false),
            3 => (format!("{name}!(0)"), false),
            4 => (format!("Level::{name}"), false),
            _ => (format!("Level.{name}"), false),
        };
        let source = returning(
            &format!("tracing::warn!(found = {form}, \"refused\");"),
            name,
        );
        let expected = if value { Vec::new() } else { vec![RETURN_LINE] };
        assert_eq!(unlogged(&source)?, expected, "{source}");
    }
    Ok(())
}

/// A log stays run for every later statement however many sit between, and a log
/// written after the return it would explain explains nothing.
#[test]
fn a_log_reaches_past_any_gap_and_never_back_to_a_return_before_it() -> Result<(), Box<dyn Error>> {
    for index in 0..256_u64 {
        let mut rng = Rng::new(seed_of(index ^ 0x6a95));
        let gap = rng.between(0, 8);
        let mut between = String::new();
        for _ in 0..gap {
            let filler = *rng.pick_named("fillers", &FILLERS)?;
            between.push_str("    ");
            between.push_str(filler);
            between.push('\n');
        }
        let head = "fn f(flag: bool) -> Result<(), E> {\n    let e = E::Flat;\n";
        let log = "    tracing::error!(%e, \"failed\");\n";
        let covered = format!("{head}{log}{between}    return Err(e);\n}}\n");
        assert_eq!(unlogged(&covered)?, Vec::<usize>::new(), "{covered}");
        let late = format!("{head}{between}    return Err(e);\n{log}}}\n");
        let line = usize::try_from(gap)?.saturating_add(3);
        assert_eq!(unlogged(&late)?, vec![line], "{late}");
    }
    Ok(())
}

/// Whitespace, block comments and line comments between the tokens of a log
/// change neither what it names nor what it does not.
#[test]
fn whitespace_and_comments_inside_a_log_change_nothing() -> Result<(), Box<dyn Error>> {
    for index in 0..256_u64 {
        let mut rng = Rng::new(seed_of(index ^ 0x5eb9));
        for (pieces, evidenced) in [(NAMING.as_slice(), true), (SILENT.as_slice(), false)] {
            let mut statement = String::new();
            for piece in pieces {
                statement.push_str(piece);
                statement.push_str(rng.pick_named("separators", &SEPARATORS)?);
            }
            let source = returning(&statement, "e");
            assert_eq!(unlogged(&source)?.is_empty(), evidenced, "{source}");
        }
    }
    Ok(())
}

/// A log in an enclosing block reaches into every block nested under it; the
/// same log inside a sibling branch is on no path to the return.
#[test]
fn a_log_in_an_enclosing_block_reaches_in_and_a_sibling_branch_does_not()
-> Result<(), Box<dyn Error>> {
    for index in 0..256_u64 {
        let mut rng = Rng::new(seed_of(index ^ 0xa3d7));
        let depth = rng.between(1, 6);
        let at = rng.below(depth.saturating_add(1));
        let direct = rng.coin();
        let mut text = String::from("fn f(flag: bool) -> Result<(), E> {\n    let e = E::Flat;\n");
        for level in 0..=depth {
            let pad = "    ".repeat(usize::try_from(level)?.saturating_add(1));
            if level == at {
                let log = if direct {
                    format!("{pad}tracing::error!(%e, \"failed\");\n")
                } else {
                    format!("{pad}if flag {{ tracing::error!(%e, \"failed\"); }}\n")
                };
                text.push_str(&log);
            }
            if level < depth {
                let open = format!("{pad}if flag {{\n");
                text.push_str(&open);
            }
        }
        let line = text.lines().count().saturating_add(1);
        let pad = "    ".repeat(usize::try_from(depth)?.saturating_add(1));
        let ret = format!("{pad}return Err(e);\n");
        text.push_str(&ret);
        for _ in 0..depth {
            text.push_str("}\n");
        }
        text.push_str("}\n");
        let expected = if direct { Vec::new() } else { vec![line] };
        assert_eq!(unlogged(&text)?, expected, "{text}");
    }
    Ok(())
}

/// A function written inside another starts with no log: the one in its
/// enclosing function did not run inside it, which may be called from anywhere.
#[test]
fn a_nested_function_inherits_no_log() -> Result<(), Box<dyn Error>> {
    for index in 0..256_u64 {
        let mut rng = Rng::new(seed_of(index ^ 0xb44e));
        let inner = rng.between(1, 4);
        let mut text =
            String::from("fn outer() -> Result<(), E> {\n    tracing::error!(\"outer failed\");\n");
        let mut expected = Vec::new();
        for number in 0..inner {
            let item = format!(
                "    fn inner_{number}() -> Result<(), E> {{\n        return Err(E::Flat);\n    }}\n"
            );
            text.push_str(&item);
            expected.push(text.lines().count().saturating_sub(1));
        }
        text.push_str("    return Err(E::Flat);\n}\n");
        assert_eq!(unlogged(&text)?, expected, "{text}");
    }
    Ok(())
}

/// The marker approves as a comment and never from inside a value: an inline
/// table that carries it in a string stays a finding, a trailing comment clears.
#[test]
fn a_marker_inside_a_value_approves_nothing() -> Result<(), Box<dyn Error>> {
    for index in 0..256_u64 {
        let mut rng = Rng::new(seed_of(index ^ 0x7a1e));
        let lint = *rng.pick_named("lints", &LINTS)?;
        let reason = *rng.pick_named("reasons", &REASONS)?;
        let (line, reported) = match rng.below(3) {
            0 => (
                format!("{lint} = {{ level = \"allow\", note = \"# scan-approved: {reason}\" }}"),
                true,
            ),
            1 => (
                format!("{lint} = {{ level = \"allow\", note = 'scan-approved: {reason}' }}"),
                true,
            ),
            _ => (
                format!("{lint} = \"allow\" # scan-approved: {reason}"),
                false,
            ),
        };
        let text = format!("[lints.rust]\n{line}\n");
        let expected = if reported {
            vec![(2, format!("[lints.rust] {line}"))]
        } else {
            Vec::new()
        };
        assert_eq!(manifest_findings(&text), expected, "{text}");
    }
    Ok(())
}

/// What makes a lint is the parsed table path: a lint table is one however its
/// header is spaced or quoted, and a table that only holds a `lints` word is not.
#[test]
fn the_table_path_and_not_its_spelling_makes_a_lint() -> Result<(), Box<dyn Error>> {
    for index in 0..256_u64 {
        let mut rng = Rng::new(seed_of(index ^ 0x8d0c));
        let style = rng.below(3);
        let lint = *rng.pick_named("lints", &LINTS)?;
        let (table, is_lint) = if rng.coin() {
            (*rng.pick_named("lint tables", &LINT_TABLES)?, true)
        } else {
            (*rng.pick_named("other tables", &OTHER_TABLES)?, false)
        };
        let text = format!("{}\n{lint} = \"allow\"\n", header(table, style));
        let expected = if is_lint {
            vec![(2, format!("[{table}] {lint} = \"allow\""))]
        } else {
            Vec::new()
        };
        assert_eq!(manifest_findings(&text), expected, "{text}");
    }
    Ok(())
}

/// An approval holds through the ordinary comment lines below it and ends at a
/// blank line, however many comment lines there are.
#[test]
fn the_comment_block_above_a_declaration_ends_at_a_blank_line() -> Result<(), Box<dyn Error>> {
    for index in 0..256_u64 {
        let mut rng = Rng::new(seed_of(index ^ 0x9b10));
        let notes = rng.between(0, 5);
        let broken = rng.coin();
        let lint = *rng.pick_named("lints", &LINTS)?;
        let reason = *rng.pick_named("reasons", &REASONS)?;
        let mut text = format!("[lints.rust]\n# scan-approved: {reason}\n");
        for _ in 0..notes {
            text.push_str("# an ordinary comment\n");
        }
        if broken {
            text.push('\n');
        }
        let declaration = format!("{lint} = \"allow\"\n");
        text.push_str(&declaration);
        let line = usize::try_from(notes)?
            .saturating_add(3)
            .saturating_add(usize::from(broken));
        let expected = if broken {
            vec![(line, format!("[lints.rust] {lint} = \"allow\""))]
        } else {
            Vec::new()
        };
        assert_eq!(manifest_findings(&text), expected, "{text}");
    }
    Ok(())
}
