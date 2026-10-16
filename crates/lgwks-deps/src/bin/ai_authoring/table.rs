//! The tables the benchmark's README carries, read from a committed run.
//!
//! One definition, so the table on the page and any number quoted in a review
//! come from the same aggregation over `results.jsonl`. Read-only: it opens a
//! committed run and prints, and writes nothing, because a table generator that
//! rewrote the run it read would be a second source of truth in a directory whose
//! whole point is that one run's evidence is never edited.

use std::collections::{BTreeMap, BTreeSet};

use lgwks_std::json::Value;

use super::record::Record;
use super::stats::{median_real, median_whole, ratio, wide, wilson};

/// What a table cell shows when there is no number.
const NONE: &str = "—";

/// The numbers one group of trials contributes to a row.
struct Cell {
    /// Trials in the group.
    n: usize,
    /// Trials that compiled.
    compiled: usize,
    /// Clauses passed over clauses run.
    pass_rate: Option<f64>,
    /// Median repairs.
    repairs: Option<Value>,
    /// Median tokens, read plus written.
    tokens: Option<Value>,
    /// Median consumer lines.
    lines: Option<Value>,
    /// Median oracle wall time.
    wall: Option<f64>,
    /// Median peak resident set.
    rss: Option<Value>,
    /// Whether every trial's drop clause passed.
    cleanup_all: bool,
}

/// A number as a table cell: an integer as itself, a real to one decimal.
fn shown_value(value: Option<&Value>) -> String {
    let Some(found) = value else {
        return NONE.to_owned();
    };
    match *found {
        Value::Number(ref number) => match number.as_u64() {
            Some(whole) => whole.to_string(),
            None => match number.as_f64() {
                Some(real) => format!("{real:.1}"),
                None => NONE.to_owned(),
            },
        },
        Value::Null | Value::Bool(_) | Value::String(_) | Value::Array(_) | Value::Object(_) => {
            NONE.to_owned()
        }
    }
}

/// A real as a table cell.
fn shown_real(value: Option<f64>) -> String {
    match value {
        Some(real) => format!("{real:.1}"),
        None => NONE.to_owned(),
    }
}

/// The groups of `records` by the key `key_of` names, each record kept in its group.
fn grouped<Key: Ord>(
    records: &[Record],
    key_of: impl Fn(&Record) -> Key,
) -> BTreeMap<Key, Vec<&Record>> {
    let mut groups: BTreeMap<Key, Vec<&Record>> = BTreeMap::new();
    for record in records {
        groups.entry(key_of(record)).or_default().push(record);
    }
    groups
}

/// The sum of `count` over `rows`, saturating.
fn total_of(rows: &[&Record], count: impl Fn(&Record) -> usize) -> u64 {
    rows.iter()
        .fold(0_u64, |sum, row| sum.saturating_add(wide(count(row))))
}

/// The numbers of one group.
fn cell(rows: &[&Record]) -> Cell {
    let whole =
        |value: fn(&Record) -> u64| -> Vec<u64> { rows.iter().map(|row| value(row)).collect() };
    let wall: Vec<f64> = rows.iter().map(|row| row.oracle_wall_ms).collect();
    let rss: Vec<u64> = rows
        .iter()
        .filter_map(|row| row.oracle_peak_rss_bytes)
        .collect();
    Cell {
        n: rows.len(),
        compiled: rows.iter().filter(|row| row.compiled).count(),
        pass_rate: ratio(
            total_of(rows, Record::pass_count),
            total_of(rows, Record::total),
        ),
        repairs: median_whole(&whole(|row| u64::from(row.repairs))),
        tokens: median_whole(&whole(|row| row.tokens_in.saturating_add(row.tokens_out))),
        lines: median_whole(&whole(|row| wide(row.consumer_lines))),
        wall: median_real(&wall),
        rss: median_whole(&rss),
        cleanup_all: rows.iter().all(|row| row.cleanup_ok),
    }
}

/// `k/n [low, high]`: full passes and their 95% interval, in percent.
fn pass_cell(rows: &[&Record]) -> String {
    let passed = wide(rows.iter().filter(|row| row.is_full_pass()).count());
    let (low, high) = wilson(passed, wide(rows.len()));
    format!(
        "{passed}/{} [{:.0}, {:.0}]",
        rows.len(),
        low * 100.0,
        high * 100.0
    )
}

/// The per-(api, profile) table.
fn by_profile(records: &[Record], out: &mut Vec<String>) {
    out.push("## Per-(api, profile), from the committed run\n".to_owned());
    out.push(
        "| api | profile | n | compiled | oracle pass rate | repairs median | \
         tokens median | consumer lines median | oracle wall median (ms) | \
         peak RSS median (bytes) | cleanup |"
            .to_owned(),
    );
    out.push("|---|---|---|---|---|---|---|---|---|---|---|".to_owned());
    let groups = grouped(records, |row| (row.arm.clone(), row.profile.clone()));
    for (key, rows) in &groups {
        let numbers = cell(rows);
        out.push(format!(
            "| {} | {} | {} | {}/{} | {} | {} | {} | {} | {} | {} | {} |",
            key.0,
            key.1,
            numbers.n,
            numbers.compiled,
            numbers.n,
            shown_real(numbers.pass_rate),
            shown_value(numbers.repairs.as_ref()),
            shown_value(numbers.tokens.as_ref()),
            shown_value(numbers.lines.as_ref()),
            shown_real(numbers.wall),
            shown_value(numbers.rss.as_ref()),
            if numbers.cleanup_all { "yes" } else { "no" },
        ));
    }
}

/// The per-(task, api) table.
fn by_task(records: &[Record], out: &mut Vec<String>) {
    out.push("\n## Per-(task), same run\n".to_owned());
    out.push("| task | api | n | pass rate | repairs median | lines median |".to_owned());
    out.push("|---|---|---|---|---|---|".to_owned());
    let groups = grouped(records, |row| (row.task.clone(), row.arm.clone()));
    for (key, rows) in &groups {
        let numbers = cell(rows);
        out.push(format!(
            "| {} | {} | {} | {} | {} | {} |",
            key.0,
            key.1,
            numbers.n,
            shown_real(numbers.pass_rate),
            shown_value(numbers.repairs.as_ref()),
            shown_value(numbers.lines.as_ref()),
        ));
    }
}

/// The header of a table whose columns are the arms, after `lead` leading columns.
fn arm_header(lead: &[&str], arms: &BTreeSet<&str>, out: &mut Vec<String>) {
    let names: Vec<&str> = arms.iter().copied().collect();
    out.push(format!("| {} | {} |", lead.join(" | "), names.join(" | ")));
    out.push(format!(
        "|{}",
        "---|".repeat(lead.len().saturating_add(arms.len()))
    ));
}

/// The full-pass tables, with 95% Wilson intervals.
fn full_passes(records: &[Record], out: &mut Vec<String>) {
    let arms: BTreeSet<&str> = records.iter().map(|row| row.arm.as_str()).collect();
    let tasks: BTreeSet<&str> = records.iter().map(|row| row.task.as_str()).collect();
    let models: BTreeSet<&str> = records.iter().map(|row| row.model.as_str()).collect();
    out.push("\n## Full passes per (task, arm), all models, with 95% Wilson intervals\n".to_owned());
    arm_header(&["task"], &arms, out);
    let by_task_arm = grouped(records, |row| (row.task.clone(), row.arm.clone()));
    for task in &tasks {
        let cells: Vec<String> = arms
            .iter()
            .map(
                |arm| match by_task_arm.get(&((*task).to_owned(), (*arm).to_owned())) {
                    Some(rows) => pass_cell(rows),
                    None => NONE.to_owned(),
                },
            )
            .collect();
        out.push(format!("| {task} | {} |", cells.join(" | ")));
    }
    out.push("\n## Full passes per (model, task, arm), with 95% Wilson intervals\n".to_owned());
    arm_header(&["model", "task"], &arms, out);
    let by_cell = grouped(records, |row| {
        (row.model.clone(), row.task.clone(), row.arm.clone())
    });
    for model in &models {
        for task in &tasks {
            let cells: Vec<String> = arms
                .iter()
                .map(|arm| {
                    let key = ((*model).to_owned(), (*task).to_owned(), (*arm).to_owned());
                    match by_cell.get(&key) {
                        Some(rows) => pass_cell(rows),
                        None => NONE.to_owned(),
                    }
                })
                .collect();
            out.push(format!("| `{model}` | {task} | {} |", cells.join(" | ")));
        }
    }
}

/// Every table, as the Markdown the README carries.
pub(super) fn render_tables(records: &[Record]) -> String {
    let mut out = Vec::new();
    by_profile(records, &mut out);
    by_task(records, &mut out);
    full_passes(records, &mut out);
    format!("{}\n", out.join("\n"))
}
