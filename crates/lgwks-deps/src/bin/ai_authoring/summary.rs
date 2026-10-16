//! A run's `summary.json`: the protocol the run held every trial to, and one
//! aggregate per (model, arm, task, profile) cell with every trial listed.
//!
//! Grouping by profile is what makes the profile axis readable at all: without
//! it, five trials that each behaved differently collapse into one row and the
//! profile comparison has to be rebuilt from `results.jsonl` by hand.

use std::collections::BTreeMap;

use lgwks_std::json::{Map, Value};

use super::bench::{Bench, Setting};
use super::plan::Skipped;
use super::record::{Record, canonical};
use super::stats::{mean_whole, median_real, median_whole, ratio, wide};

/// What a cell is keyed by.
type Key = (String, String, String, String);

/// A real number as JSON, or `null` when there is none.
fn real(value: Option<f64>) -> Value {
    match value {
        Some(found) => Value::from(found),
        None => Value::Null,
    }
}

/// A median of whole numbers as JSON, or `null` when there is none.
fn whole(values: &[u64]) -> Value {
    match median_whole(values) {
        Some(found) => found,
        None => Value::Null,
    }
}

/// One cell's aggregate.
fn group(key: &Key, rows: &[&Record]) -> Value {
    let repairs: Vec<u64> = rows.iter().map(|row| u64::from(row.repairs)).collect();
    let tokens_in: Vec<u64> = rows.iter().map(|row| row.tokens_in).collect();
    let tokens_out: Vec<u64> = rows.iter().map(|row| row.tokens_out).collect();
    let lines: Vec<u64> = rows.iter().map(|row| wide(row.consumer_lines)).collect();
    let sites: Vec<u64> = rows
        .iter()
        .map(|row| wide(row.orchestration_sites))
        .collect();
    let rss: Vec<u64> = rows
        .iter()
        .filter_map(|row| row.oracle_peak_rss_bytes)
        .collect();
    let walls: Vec<f64> = rows.iter().map(|row| row.oracle_wall_ms).collect();
    let passed = rows
        .iter()
        .fold(0_u64, |sum, row| sum.saturating_add(wide(row.pass_count())));
    let total = rows
        .iter()
        .fold(0_u64, |sum, row| sum.saturating_add(wide(row.total())));
    let mut ordered = rows.to_vec();
    ordered.sort_by_key(|row| row.trial);
    let per_trial: Vec<Value> = ordered
        .iter()
        .map(|row| {
            let mut trial = Map::new();
            trial.insert("trial".to_owned(), Value::from(row.trial));
            trial.insert("compiled".to_owned(), Value::from(row.compiled));
            trial.insert("cleanup_ok".to_owned(), Value::from(row.cleanup_ok));
            trial.insert(
                "oracle_pass_count".to_owned(),
                Value::from(wide(row.pass_count())),
            );
            trial.insert("oracle_total".to_owned(), Value::from(wide(row.total())));
            trial.insert(
                "oracle".to_owned(),
                match row.to_value().get("oracle") {
                    Some(clauses) => clauses.clone(),
                    None => Value::Null,
                },
            );
            Value::Object(trial)
        })
        .collect();
    let mut cell = Map::new();
    let mut put = |name: &str, value: Value| {
        cell.insert(name.to_owned(), value);
    };
    put("model", Value::from(key.0.clone()));
    put("api", Value::from(key.1.clone()));
    put("task", Value::from(key.2.clone()));
    put("profile", Value::from(key.3.clone()));
    put("n", Value::from(wide(rows.len())));
    put(
        "compiled",
        Value::from(wide(rows.iter().filter(|row| row.compiled).count())),
    );
    put(
        "cleanup_ok_all",
        Value::from(rows.iter().all(|row| row.cleanup_ok)),
    );
    put("repairs_mean", real(mean_whole(&repairs)));
    put("repairs_median", whole(&repairs));
    put("oracle_pass_rate", real(ratio(passed, total)));
    put("oracle_per_trial", Value::Array(per_trial));
    put("tokens_in_median", whole(&tokens_in));
    put("tokens_out_median", whole(&tokens_out));
    put("consumer_lines_median", whole(&lines));
    put("orchestration_sites_median", whole(&sites));
    put("oracle_wall_ms_median", real(median_real(&walls)));
    put("oracle_peak_rss_bytes_median", whole(&rss));
    Value::Object(cell)
}

/// The per-cell aggregates of `records`, in key order.
pub(super) fn summarize(records: &[Record]) -> Vec<Value> {
    let mut cells: BTreeMap<Key, Vec<&Record>> = BTreeMap::new();
    for record in records {
        cells
            .entry((
                record.model.clone(),
                record.arm.clone(),
                record.task.clone(),
                record.profile.clone(),
            ))
            .or_default()
            .push(record);
    }
    cells.iter().map(|(key, rows)| group(key, rows)).collect()
}

/// What the run held every trial to: the manifest's statements, and what this
/// run chose.
pub(super) fn protocol(
    bench: &Bench,
    profiles: &[String],
    max_repairs: u32,
    skipped: &[Skipped],
) -> Value {
    let mut table = Map::new();
    for statement in &bench.protocol {
        let value = match statement.1 {
            Setting::Words(ref words) => Value::from(words.clone()),
            Setting::Flag(flag) => Value::from(flag),
        };
        table.insert(statement.0.clone(), value);
    }
    table.insert("repair_budget".to_owned(), Value::from(max_repairs));
    table.insert(
        "profiles".to_owned(),
        Value::Array(
            profiles
                .iter()
                .map(|name| Value::from(name.clone()))
                .collect(),
        ),
    );
    table.insert(
        "skipped_cells".to_owned(),
        Value::Array(
            skipped
                .iter()
                .map(|cell| {
                    let mut entry = Map::new();
                    entry.insert("api".to_owned(), Value::from(cell.arm.clone()));
                    entry.insert("task".to_owned(), Value::from(cell.task.clone()));
                    entry.insert("why".to_owned(), Value::from(cell.why.clone()));
                    Value::Object(entry)
                })
                .collect(),
        ),
    );
    Value::Object(table)
}

/// The summary as JSON with one top-level key per line, and one line per element
/// of a list-valued key (one group per line).
///
/// Indenting every nested field put a 33-trial run past the repository's
/// 1,000-line file law; this shape is still one valid JSON document and still
/// diffs one group at a time.
pub(super) fn render(entries: &[(&str, Value)]) -> String {
    let lines: Vec<String> = entries
        .iter()
        .map(|entry| {
            let (key, value) = (entry.0, &entry.1);
            let body = match *value {
                Value::Array(ref items) if items.is_empty() => "[]".to_owned(),
                Value::Array(ref items) => {
                    let rows: Vec<String> = items
                        .iter()
                        .map(|item| format!("    {}", canonical(item)))
                        .collect();
                    format!("[\n{}\n  ]", rows.join(",\n"))
                }
                Value::Null
                | Value::Bool(_)
                | Value::Number(_)
                | Value::String(_)
                | Value::Object(_) => canonical(value),
            };
            format!("  {}: {body}", canonical(&Value::from(key.to_owned())))
        })
        .collect();
    format!("{{\n{}\n}}\n", lines.join(",\n"))
}
