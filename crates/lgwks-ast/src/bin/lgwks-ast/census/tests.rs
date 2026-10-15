//! The census's shapes and its table, from scripts as a user writes them.
//!
//! The shape cases are the ones the census was specified with (SL-7, #389):
//! two lines that differ only in names and literals are one shape, and a line
//! that differs in a keyword, a type, a called name or its punctuation is not.

use std::error::Error;
use std::path::PathBuf;

use lgwks_ast::script::read_source;

use super::{MAX_TOP, count, percent, render, rows, shape_of, tokens, write};
use crate::FileReading;

type TestResult = Result<(), Box<dyn Error>>;

/// Two flows whose plain Rust shares one shape, and one line that does not.
const SOURCE: &str = "script! {
    flow first(site: &Site) -> usize:
        let seen = site.get(1)
        let again = page.get(2)
        site.note(seen).await?
        give back seen
    flow second(site: &Site) -> usize:
        let third = site.get(3)
        give back third
}
";

/// What `source` reads as when it is the file `name`.
fn reading(name: &str, source: &str) -> Result<FileReading, String> {
    let invocations = read_source(source).map_err(|refusal| refusal.to_string())?;
    Ok(FileReading {
        path: PathBuf::from(name),
        outcome: Ok(invocations),
    })
}

#[test]
fn a_shape_blanks_names_and_literals_and_keeps_structure() {
    let cases = [
        ("let a = site.get(id)", "let _ = _.get(_)"),
        ("let b = page.get(n)", "let _ = _.get(_)"),
        ("site.note(\"busy\", 3).await?", "_.note(_, 0).await?"),
        (
            "let o: u16 = Duration::from_millis(5)",
            "let _: _ = Duration::from_millis(0)",
        ),
        (
            "let left = site.pair(\n    1,\n    items.len())",
            "let _ = _.pair(0, _.len())",
        ),
        ("assert!(x > 2)", "assert!(_ > 0)"),
        ("let same = a != b", "let _ = _ != _"),
        ("let n = r#type.len()", "let _ = _.len()"),
    ];
    for (text, want) in cases {
        assert_eq!(shape_of(text), want, "the shape of {text:?}");
    }
}

#[test]
fn a_lifetime_a_character_a_string_and_a_number_are_each_one_token() {
    let read = tokens("f::<&'static str>(b\"x\", 'c', 1_000.5e-3f64, r#loop)");
    let want = [
        "f",
        ":",
        ":",
        "<",
        "&",
        "'static",
        "str",
        ">",
        "(",
        "b\"x\"",
        ",",
        "'c'",
        ",",
        "1_000.5e-3f64",
        ",",
        "r#loop",
        ")",
    ];
    assert_eq!(read, want, "the line's tokens, in order");
}

#[test]
fn an_unterminated_quote_is_one_character_and_the_rest_still_reads() {
    assert_eq!(
        tokens("let s = \"open"),
        ["let", "s", "=", "\"", "open"],
        "a quote that never closes is a stray character, not the rest of the line"
    );
}

#[test]
fn the_share_is_rounded_to_a_tenth_and_n_a_without_a_whole() {
    for (part, whole, want) in [
        (1, 3, "33.3%"),
        (2, 3, "66.7%"),
        (5, 5, "100.0%"),
        (0, 4, "0.0%"),
        (0, 0, "n/a"),
    ] {
        assert_eq!(percent(part, whole), want, "{part} of {whole}");
    }
}

#[test]
fn a_row_count_is_a_whole_number_in_bounds() {
    assert_eq!(rows("1"), Ok(1), "the least row count");
    assert_eq!(rows(&MAX_TOP.to_string()), Ok(MAX_TOP), "the most");
    let past = MAX_TOP.saturating_add(1).to_string();
    for bad in ["0", "-1", "x", "", "2.5", past.as_str()] {
        let refused = match rows(bad) {
            Err(message) => message,
            Ok(accepted) => format!("accepted as {accepted}"),
        };
        assert!(
            refused.contains("--top takes a row count from 1 to"),
            "`{bad}` is refused with the bounds, got {refused:?}"
        );
    }
}

#[test]
fn recurring_lines_group_by_shape_most_recurrent_first_with_their_sites() -> TestResult {
    let census = count(&[reading("flows.rs", SOURCE)?]);
    let lines = render(&census, 0, 20);
    assert_eq!(
        lines.first().map(String::as_str),
        Some(
            "1 `script!` in 1 files, 2 flows; 2 word steps and 4 passthrough lines \
             (passthrough share 66.7%); 0 refused or unreadable and not read; 2 distinct shapes."
        ),
        "the totals read from the parsed scripts: each flow's `give back` is a word step"
    );
    assert_eq!(
        lines.get(4).map(String::as_str),
        Some("| 1 | 3 | 1 | `let _ = _.get(0)` | `flows.rs:3`, `flows.rs:4`, `flows.rs:8` |"),
        "three lines are one shape, named at their own lines in read order"
    );
    assert_eq!(
        lines.get(5).map(String::as_str),
        Some("| 2 | 1 | 1 | `_.note(_).await?` | `flows.rs:5` |"),
        "the one line of another shape follows"
    );
    assert_eq!(
        lines.last().map(String::as_str),
        Some("- `flows.rs`"),
        "the files read close the report"
    );
    Ok(())
}

#[test]
fn the_table_stops_at_the_rows_asked_for() -> TestResult {
    let census = count(&[reading("flows.rs", SOURCE)?]);
    let lines = render(&census, 0, 1);
    let rows_printed = lines
        .iter()
        .filter(|line| line.starts_with("| ") && line.ends_with(" |"))
        .count();
    assert_eq!(
        rows_printed, 2,
        "the header and one row, and not the second shape"
    );
    Ok(())
}

#[test]
fn a_pipe_in_a_shape_does_not_end_its_table_cell() -> TestResult {
    let source = "script! {
    flow pick(items: Vec<u32>) -> usize:
        let ids = items.iter().map(|item| item.id).collect()
        give back ids
}
";
    let census = count(&[reading("pick.rs", source)?]);
    let lines = render(&census, 0, 20);
    assert!(
        lines
            .iter()
            .any(|line| line.contains("`let _ = _.iter().map(\\| _ \\| _._).collect()`")),
        "each `|` in the closure is escaped, got {lines:?}"
    );
    Ok(())
}

#[test]
fn a_file_the_language_refused_is_counted_and_never_read() -> TestResult {
    let refused = "script! {\n    flow stop():\n        std::process::exit(1)\n}\n";
    let readings = [
        reading("good.rs", SOURCE)?,
        reading("bad.rs", refused)?,
        FileReading {
            path: PathBuf::from("gone.rs"),
            outcome: Err("cannot read gone.rs".to_owned()),
        },
    ];
    let mut out = Vec::new();
    write(&readings, 20, &mut out)?;
    let printed = String::from_utf8(out)?;
    assert!(
        printed.starts_with("2 `script!` in 1 files, 2 flows;"),
        "both invocations are counted and only the accepted one is read:\n{printed}"
    );
    assert!(
        printed.contains("2 refused or unreadable and not read"),
        "the refusal and the unreadable file are both named, not skipped:\n{printed}"
    );
    Ok(())
}
