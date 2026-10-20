//! The two renderings of an architecture diff (#655): a Markdown body a person
//! pastes into a pull request, and one JSON document an agent parses.
//!
//! The Markdown has a heading, the three classification tables and the reason
//! for every row. The JSON has one stable key per fact, so a CI step reads the
//! verdict from `unapproved_widening` and never from the prose.

use std::io::{self, Write};
use std::path::Path;

use lgwks_std::json::{Map, Value};

use super::{Change, Class, Diff};

/// Write the Markdown PR-comment body.
///
/// # Errors
///
/// The writer's own error.
pub(super) fn write_markdown(
    diff: &Diff,
    base: &Path,
    head: &Path,
    out: &mut impl Write,
) -> io::Result<()> {
    for line in markdown(diff, base, head) {
        writeln!(out, "{line}")?;
    }
    Ok(())
}

/// Write the one JSON document.
///
/// # Errors
///
/// The writer's own error.
pub(super) fn write_json(
    diff: &Diff,
    base: &Path,
    head: &Path,
    out: &mut impl Write,
) -> io::Result<()> {
    writeln!(out, "{}", document(diff, base, head))
}

/// The Markdown lines: heading, counts, approval state, then one table per
/// class.
fn markdown(diff: &Diff, base: &Path, head: &Path) -> Vec<String> {
    let mut lines = vec![
        "## Architecture diff".to_owned(),
        String::new(),
        format!("`{}` → `{}`", base.display(), head.display()),
        String::new(),
        format!(
            "**Widening {}, narrowing {}, neutral {}.**",
            diff.count(Class::Widening),
            diff.count(Class::Narrowing),
            diff.count(Class::Neutral),
        ),
        String::new(),
    ];
    let unapproved = diff.unapproved_widening();
    if unapproved > 0 {
        lines.push(format!(
            "**UNAPPROVED WIDENING ({unapproved})**: add \
             `// lgwks-architecture-approved: <reason>` to the head source and run this again."
        ));
        lines.push(String::new());
    } else if !diff.approvals.is_empty() && diff.count(Class::Widening) > 0 {
        for (file, reason) in &diff.approvals {
            lines.push(format!("Approved in `{file}`: {reason}"));
        }
        lines.push(String::new());
    }
    if diff.is_empty() {
        lines.push(
            "No structural change: every flow, word, subject and nesting path matches, and \
             source lines are not compared."
                .to_owned(),
        );
        return lines;
    }
    for class in [Class::Widening, Class::Narrowing, Class::Neutral] {
        section(&mut lines, diff, class);
    }
    lines
}

/// One classification table.
fn section(lines: &mut Vec<String>, diff: &Diff, class: Class) {
    let rows: Vec<&Change> = diff
        .changes
        .iter()
        .filter(|change| change.class == class)
        .collect();
    lines.push(format!("### {} ({})", class.title(), rows.len()));
    lines.push(String::new());
    if rows.is_empty() {
        lines.push("_None._".to_owned());
        lines.push(String::new());
        return;
    }
    lines.push("| Flow | Path | Kind | From | To | Why |".to_owned());
    lines.push("|---|---|---|---|---|---|".to_owned());
    for change in rows {
        lines.push(format!(
            "| {} | {} | {} | {} | {} | {} |",
            cell(&change.flow),
            cell(&change.path),
            cell(&change.kind),
            code(&change.from),
            code(&change.to),
            cell(&change.reason),
        ));
    }
    lines.push(String::new());
}

/// A table cell: a `|` would end the cell, and a line break would end the row.
fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

/// A value in code ticks, or an em dash when the revision has no value.
fn code(text: &str) -> String {
    if text.is_empty() {
        "—".to_owned()
    } else {
        format!("`{}`", cell(text))
    }
}

/// The JSON document. Every key here is stable: `verb`, `base`, `head`,
/// `widening`, `narrowing`, `neutral`, `unapproved_widening`, `approved`,
/// `approvals` and `changes`; each change is `class`, `file`, `flow`, `path`,
/// `kind`, `from`, `to`, `reason` and `approved`.
fn document(diff: &Diff, base: &Path, head: &Path) -> Value {
    let changes = diff.changes.iter().map(change_value).collect();
    let approvals = diff
        .approvals
        .iter()
        .map(|(file, reason)| {
            object(vec![
                ("file", Value::from(file.as_str())),
                ("reason", Value::from(reason.as_str())),
            ])
        })
        .collect();
    object(vec![
        ("verb", Value::from("diff")),
        ("base", Value::from(base.display().to_string())),
        ("head", Value::from(head.display().to_string())),
        ("widening", Value::from(diff.count(Class::Widening))),
        ("narrowing", Value::from(diff.count(Class::Narrowing))),
        ("neutral", Value::from(diff.count(Class::Neutral))),
        (
            "unapproved_widening",
            Value::from(diff.unapproved_widening()),
        ),
        ("approved", Value::from(diff.unapproved_widening() == 0)),
        ("approvals", Value::Array(approvals)),
        ("changes", Value::Array(changes)),
    ])
}

/// One change as a JSON object.
fn change_value(change: &Change) -> Value {
    object(vec![
        ("class", Value::from(change.class.word())),
        ("file", Value::from(change.file.as_str())),
        ("flow", Value::from(change.flow.as_str())),
        ("path", Value::from(change.path.as_str())),
        ("kind", Value::from(change.kind.as_str())),
        ("from", Value::from(change.from.as_str())),
        ("to", Value::from(change.to.as_str())),
        ("reason", Value::from(change.reason.as_str())),
        ("approved", Value::from(change.approved)),
    ])
}

/// A JSON object from its fields.
fn object(fields: Vec<(&str, Value)>) -> Value {
    Value::Object(
        fields
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect::<Map<String, Value>>(),
    )
}
