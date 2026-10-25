//! The freshness report: one row per locked package, as a table or as JSON.
//!
//! A package whose lookup failed carries an error and is not stale: a network
//! fault is not evidence that a dependency is out of date, and reporting it as
//! stale would fail a build for a reason the lock cannot support. The exit code
//! says whether the check completed; the rows say what was found.

use std::io;

use lgwks_std::json::{Map, Value};

use super::index::Newest;

/// One registry package's freshness verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FreshnessResult {
    /// Package name as it appears in `Cargo.lock`.
    name: String,
    /// Version the lock file resolved.
    resolved: String,
    /// The newest version the lock could move to, or `None` when the registry
    /// offers none.
    latest: Option<String>,
    /// The upstream repository the cached manifest names, or `None` when Cargo
    /// has not unpacked this version on this machine.
    repository: Option<String>,
    /// Whether `latest` is ahead of `resolved` in precedence.
    stale: bool,
    /// The direct dependencies this package comes through.
    via: Vec<String>,
    /// Why the lookup failed, when it did.
    error: Option<String>,
}

impl FreshnessResult {
    /// A successful lookup.
    ///
    /// A registry that offers no version cannot be compared, so nothing is
    /// claimed: `stale` is only ever decided between two versions the registry
    /// and the lock both named.
    pub(super) fn found(
        name: &str,
        resolved: &str,
        newest: Option<Newest>,
        repository: Option<String>,
        via: &[String],
    ) -> Self {
        let (latest, stale) = match newest {
            Some(newest) => (Some(newest.text), newest.stale),
            None => (None, false),
        };
        Self {
            name: name.to_owned(),
            resolved: resolved.to_owned(),
            latest,
            repository,
            stale,
            via: via.to_vec(),
            error: None,
        }
    }

    /// A failed lookup, which is reported but never counted as stale.
    pub(super) fn failed(name: &str, resolved: &str, via: &[String], error: String) -> Self {
        Self {
            name: name.to_owned(),
            resolved: resolved.to_owned(),
            latest: None,
            repository: None,
            stale: false,
            via: via.to_vec(),
            error: Some(error),
        }
    }

    /// Whether this package is behind the newest version on offer.
    pub(super) fn is_stale(&self) -> bool {
        self.stale
    }

    /// Whether this package's lookup failed.
    pub(super) fn is_error(&self) -> bool {
        self.error.is_some()
    }
}

/// How many of `results` are stale.
pub(super) fn stale_count(results: &[FreshnessResult]) -> usize {
    results.iter().filter(|result| result.is_stale()).count()
}

/// How many of `results` failed to look up.
pub(super) fn error_count(results: &[FreshnessResult]) -> usize {
    results.iter().filter(|result| result.is_error()).count()
}

/// The one-line totals: printed under the table, and on stderr beside the JSON
/// so a caller that reads only the array still learns why the exit code is not
/// zero.
pub(super) fn summary_line(results: &[FreshnessResult], skipped: usize) -> String {
    let base = format!(
        "{} checked, {} stale, {} errors",
        results.len(),
        stale_count(results),
        error_count(results)
    );
    if skipped == 0 {
        base
    } else {
        format!("{base}, {skipped} skipped (not from the queried index)")
    }
}

/// The table cell for a field the registry may not have published.
///
/// `?` is the marker a failed lookup already prints, and a version the registry
/// never offered is the same fact to a reader: the value is not known. One
/// spelling for "not known" instead of an empty column that reads as a value the
/// registry sent.
fn or_unknown(value: Option<&str>) -> &str {
    match value {
        Some(value) => value,
        None => "?",
    }
}

/// Characters the `via` column holds before the next column starts.
const VIA_WIDTH: usize = 24;

/// The `via` column: the direct dependencies, or `-` for none.
///
/// The first name is always shown whole, because half a crate name names no
/// crate. More names follow while they fit the column, and the rest are counted
/// (`+3`), so one package reached through forty dependencies cannot push the
/// repository out of its column. The JSON report keeps every name.
fn via_cell(via: &[String]) -> String {
    let mut cell = String::new();
    let mut shown = 0_usize;
    for name in via {
        let grown = cell.len().saturating_add(2).saturating_add(name.len());
        if shown > 0 && grown > VIA_WIDTH {
            break;
        }
        if shown > 0 {
            cell.push_str(", ");
        }
        cell.push_str(name);
        shown = shown.saturating_add(1);
    }
    let hidden = via.len().saturating_sub(shown);
    if shown == 0 {
        "-".to_owned()
    } else if hidden == 0 {
        cell
    } else {
        format!("{cell} +{hidden}")
    }
}

/// Prints the human-readable freshness table.
///
/// A failed lookup is shown as `?` / `err` rather than as a version, so an
/// unreachable registry cannot be misread as an up-to-date dependency.
pub(super) fn print_table(
    results: &[FreshnessResult],
    skipped: usize,
    out: &mut impl io::Write,
) -> io::Result<()> {
    writeln!(
        out,
        "{:<30} {:<12} {:<12} {:<5} {:<24} repository",
        "crate", "resolved", "latest", "stale", "via"
    )?;
    writeln!(out, "{}", "-".repeat(120))?;
    for result in results {
        match result.error.as_deref() {
            Some(failure) => writeln!(
                out,
                "{:<30} {:<12} {:<12} {:<5} {:<24} {}",
                result.name,
                result.resolved,
                "?",
                "err",
                via_cell(&result.via),
                failure
            )?,
            None => writeln!(
                out,
                "{:<30} {:<12} {:<12} {:<5} {:<24} {}",
                result.name,
                result.resolved,
                or_unknown(result.latest.as_deref()),
                if result.stale { "YES" } else { "" },
                via_cell(&result.via),
                or_unknown(result.repository.as_deref())
            )?,
        }
    }
    writeln!(out, "\n{}", summary_line(results, skipped))
}

/// Prints the freshness results as a JSON array of objects.
///
/// The shape is contractual: one object per checked package, in lock order,
/// with every key always present. `latest`, `repository` and `error` are `null`
/// when there is nothing to say, so a consumer reads a fixed set of keys rather
/// than testing for the existence of each; `via` is an array, empty for a
/// package no workspace member reaches. The aggregate is the process exit code.
///
/// Built through `lgwks_std::json`, like `check --json`: a string with a
/// backslash or a quote in it is escaped by the codec, not by concatenation.
pub(super) fn print_json(results: &[FreshnessResult], out: &mut impl io::Write) -> io::Result<()> {
    let mut rows = Vec::with_capacity(results.len());
    for result in results {
        let mut row = Map::new();
        row.insert("name".to_owned(), Value::String(result.name.clone()));
        row.insert(
            "resolved".to_owned(),
            Value::String(result.resolved.clone()),
        );
        row.insert("latest".to_owned(), nullable(result.latest.as_ref()));
        row.insert("stale".to_owned(), Value::Bool(result.stale));
        row.insert(
            "repository".to_owned(),
            nullable(result.repository.as_ref()),
        );
        row.insert(
            "via".to_owned(),
            Value::Array(result.via.iter().cloned().map(Value::String).collect()),
        );
        row.insert("error".to_owned(), nullable(result.error.as_ref()));
        rows.push(Value::Object(row));
    }
    let rendered =
        lgwks_std::json::to_string_pretty(&Value::Array(rows)).map_err(io::Error::other)?;
    writeln!(out, "{rendered}")
}

/// A string, or `null` when there is none.
fn nullable(value: Option<&String>) -> Value {
    match value {
        Some(text) => Value::String(text.clone()),
        None => Value::Null,
    }
}

#[cfg(test)]
mod sim_report;
