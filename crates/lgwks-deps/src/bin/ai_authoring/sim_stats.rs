//! Simulated statistics: medians, intervals, ratios, the README tables and the
//! run summary are computed over generated trials and held to definitions that
//! share no code with them (order statistics by counting, published interval
//! values, group counts taken from the records themselves).
//!
//! Every number in the results table comes through here, so a wrong median is a
//! wrong claim about which arm wins.

use std::collections::{BTreeMap, BTreeSet};

use lgwks_std::json::{self, Value};

use super::bench::{Bench, Setting};
use super::plan::{Mode, Selection, plan};
use super::record::Record;
use super::replay::{Failure, replay_sweep};
use super::seed::Rng;
use super::sim_kit::{close, committed, count, ensure, job, one_of, record, shuffle};
use super::stats::{exact, mean_whole, median_real, median_whole, ratio, wide, wilson};
use super::summary::{protocol, render, summarize};
use super::table::render_tables;

/// Up to twenty whole numbers below a million, drawn from the sequence.
fn whole_values(rng: &mut Rng) -> Result<Vec<u32>, Failure> {
    let mut values = Vec::new();
    for _ in 0..count(rng, 0, 20)? {
        values.push(rng.below(1_000_000));
    }
    Ok(values)
}

/// The values as reals.
fn reals(values: &[u32]) -> Vec<f64> {
    values.iter().map(|found| f64::from(*found)).collect()
}

/// The values as wide whole numbers.
fn wides(values: &[u32]) -> Vec<u64> {
    values.iter().map(|found| u64::from(*found)).collect()
}

/// A JSON number as a real, for the whole numbers and halves a median is.
fn real_of(value: &Value) -> Option<f64> {
    match *value {
        Value::Number(ref number) => match number.as_u64() {
            Some(whole) => u32::try_from(whole).ok().map(f64::from),
            None => number.as_f64(),
        },
        Value::Null | Value::Bool(_) | Value::String(_) | Value::Array(_) | Value::Object(_) => None,
    }
}

/// Whether two optional reals agree to a millionth.
fn same(left: Option<f64>, right: Option<f64>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(one), Some(other)) => close(one, other),
        (Some(_), None) | (None, Some(_)) => false,
    }
}

/// The `rank`-th smallest of `values`, counting from one: the least value with at
/// least `rank` values at or below it. Defined by counting, with no sort.
fn order_statistic(values: &[f64], rank: usize) -> Option<f64> {
    values
        .iter()
        .copied()
        .filter(|candidate| values.iter().filter(|other| **other <= *candidate).count() >= rank)
        .min_by(f64::total_cmp)
}

/// The median as the order statistics define it: the middle one of an odd count,
/// the mean of the middle two of an even count, nothing for none.
fn modelled_median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let lower = order_statistic(values, values.len().div_ceil(2))?;
    if values.len().rem_euclid(2) == 1 {
        return Some(lower);
    }
    let upper = order_statistic(values, values.len().div_ceil(2).saturating_add(1))?;
    Some((lower + upper) / 2.0)
}

/// Source: Python `statistics.median` (docs.python.org: the middle value of an odd
/// count, the mean of the two middle values of an even one) and the order
/// statistics of David, *Order Statistics*, ch. 1. Requirement: the whole-number
/// and real medians equal the middle order statistics found by counting, do not
/// depend on the order the values arrive in, and move with them when every value
/// moves by the same amount.
#[test]
fn medians_are_the_middle_order_statistics_whatever_the_order_or_offset() -> Result<(), Failure> {
    replay_sweep("medians", 600, |rng, trace| {
        let mut values = whole_values(rng)?;
        let wanted = modelled_median(&reals(&values));
        let found = median_whole(&wides(&values)).as_ref().and_then(real_of);
        ensure(same(found, wanted), || format!("whole median of {values:?} was {found:?}, expected {wanted:?}"))?;
        let eighths: Vec<f64> = reals(&values).iter().map(|item| item / 8.0).collect();
        ensure(
            same(median_real(&eighths), modelled_median(&eighths)),
            || format!("real median of {eighths:?} was {:?}", median_real(&eighths)),
        )?;
        shuffle(rng, &mut values)?;
        let shuffled = median_whole(&wides(&values)).as_ref().and_then(real_of);
        ensure(same(shuffled, found), || format!("the order of {values:?} moved the median"))?;
        let offset = rng.below(1_000_000);
        let moved: Vec<u64> = wides(&values).iter().map(|item| item.saturating_add(u64::from(offset))).collect();
        let moved_found = median_whole(&moved).as_ref().and_then(real_of);
        ensure(
            same(moved_found, found.map(|middle| middle + f64::from(offset))),
            || format!("moving {values:?} by {offset} did not move the median by {offset}"),
        )?;
        trace.record_number("size", values.len());
        trace.record(&format!("{found:?}"));
        Ok(())
    })
}

/// Published 95% Wilson intervals for n = 10: successes, low, high (Brown, Cai and
/// DasGupta, "Interval Estimation for a Binomial Proportion", *Statistical
/// Science* 16(2), 2001, Table 1 with z = 1.96).
const PUBLISHED: [(u64, f64, f64); 3] = [(5, 0.2366, 0.7634), (8, 0.4902, 0.9433), (0, 0.0, 0.2775)];

/// Source: Wilson 1927, "Probable inference, the law of succession, and
/// statistical inference", JASA 22. Requirement: the interval agrees with the
/// published values, brackets the observed share inside [0, 1], rises with the
/// number of successes, mirrors itself when successes and failures swap, and
/// narrows as the same share is seen over four times the trials.
#[test]
fn the_wilson_interval_brackets_the_share_and_is_monotone_symmetric_and_narrowing() -> Result<(), Failure> {
    replay_sweep("wilson", 500, |rng, trace| {
        for row in PUBLISHED {
            let (low, high) = wilson(row.0, 10);
            ensure((low - row.1).abs() < 5e-4 && (high - row.2).abs() < 5e-4, || {
                format!("{}/10 gave [{low}, {high}], published [{}, {}]", row.0, row.1, row.2)
            })?;
        }
        let total = u64::from(rng.between(1, 300));
        let passed = u64::from(rng.below(u32::try_from(total)?.saturating_add(1)));
        let (low, high) = wilson(passed, total);
        let share = exact(passed) / exact(total);
        ensure(
            (0.0..=share + 1e-9).contains(&low) && (share - 1e-9..=1.0).contains(&high) && low < high,
            || format!("{passed}/{total} gave [{low}, {high}] around {share}"),
        )?;
        if passed < total {
            let (next_low, next_high) = wilson(passed.saturating_add(1), total);
            ensure(next_low >= low - 1e-9 && next_high >= high - 1e-9, || {
                format!("one more success of {total} lowered the interval at {passed}")
            })?;
        }
        let (mirror_low, mirror_high) = wilson(total.saturating_sub(passed), total);
        ensure(close(low, 1.0 - mirror_high) && close(high, 1.0 - mirror_low), || {
            format!("{passed}/{total} is not the mirror of its failures")
        })?;
        let (wide_low, wide_high) = wilson(passed.saturating_mul(4), total.saturating_mul(4));
        ensure(wide_high - wide_low < high - low, || {
            format!("four times the trials of {passed}/{total} did not narrow the interval")
        })?;
        trace.record_number("low", low);
        trace.record_number("high", high);
        Ok(())
    })
}

/// Requirement: counts reach floats exactly (a whole number below 2^53 keeps every
/// bit and keeps its order), a ratio has no value without a denominator and does
/// not change when numerator and denominator scale together, and a mean is the sum
/// over the count.
#[test]
fn counts_reach_floats_exactly_and_ratios_and_means_are_what_they_are_defined_as() -> Result<(), Failure> {
    replay_sweep("counts", 600, |rng, trace| {
        let high = rng.below(2_097_152);
        let low = rng.below(u32::MAX);
        let value = u64::from(high)
            .saturating_mul(4_294_967_296)
            .saturating_add(u64::from(low));
        let wanted = f64::from(high) * 4_294_967_296.0 + f64::from(low);
        ensure(close(exact(value), wanted) && exact(value) < exact(value.saturating_add(1)), || {
            format!("{value} did not reach a float exactly and in order")
        })?;
        ensure(wide(usize::try_from(low)?) == u64::from(low), || "a count changed value".to_owned())?;
        let numerator = u64::from(rng.below(1000));
        let denominator = u64::from(rng.between(1, 1000));
        let factor = u64::from(rng.between(1, 1000));
        ensure(ratio(numerator, 0).is_none(), || "a ratio had a value over zero".to_owned())?;
        ensure(
            same(
                ratio(numerator, denominator),
                ratio(numerator.saturating_mul(factor), denominator.saturating_mul(factor)),
            ),
            || format!("{numerator}/{denominator} changed under a factor of {factor}"),
        )?;
        let values = whole_values(rng)?;
        let sum: u32 = values.iter().fold(0_u32, |total, found| total.saturating_add(*found));
        let wanted_mean = if values.is_empty() {
            None
        } else {
            Some(f64::from(sum) / f64::from(u32::try_from(values.len())?))
        };
        ensure(same(mean_whole(&wides(&values)), wanted_mean), || {
            format!("the mean of {values:?} was {:?}", mean_whole(&wides(&values)))
        })?;
        trace.record_number("value", value);
        trace.record_number("sum", sum);
        Ok(())
    })
}

/// Records of up to forty trials over the committed benchmark.
fn records_of(rng: &mut Rng, bench: &Bench) -> Result<Vec<Record>, Failure> {
    let mut records = Vec::new();
    for _ in 0..count(rng, 1, 40)? {
        let drawn = job(rng, bench, Mode::Models)?;
        records.push(record(rng, bench, &drawn)?);
    }
    Ok(records)
}

/// Requirement: the tables are a function of the set of trials, never of the order
/// they were logged in; each (arm, profile) row carries the count of its trials and
/// how many compiled; each (task, arm) cell carries its full passes over its
/// trials, counted from the records themselves.
#[test]
fn the_tables_do_not_depend_on_trial_order_and_count_each_cell_from_the_records() -> Result<(), Failure> {
    let bench = committed()?;
    replay_sweep("tables", 300, |rng, trace| {
        let records = records_of(rng, &bench)?;
        let tables = render_tables(&records);
        let mut reordered = records.clone();
        shuffle(rng, &mut reordered)?;
        ensure(render_tables(&reordered) == tables, || "the order of the trials changed the tables".to_owned())?;
        let lines: Vec<&str> = tables.lines().collect();
        let mut profiles: BTreeMap<(String, String), Vec<&Record>> = BTreeMap::new();
        let mut cells: BTreeMap<(String, String), Vec<&Record>> = BTreeMap::new();
        for row in &records {
            profiles.entry((row.arm.clone(), row.profile.clone())).or_default().push(row);
            cells.entry((row.task.clone(), row.arm.clone())).or_default().push(row);
        }
        for (key, rows) in &profiles {
            let compiled = rows.iter().filter(|row| row.compiled).count();
            let prefix = format!("| {} | {} | {} | {compiled}/{} | ", key.0, key.1, rows.len(), rows.len());
            ensure(lines.iter().any(|line| line.starts_with(&prefix)), || {
                format!("no row starts `{prefix}`")
            })?;
        }
        for (key, rows) in &cells {
            let passes = rows.iter().filter(|row| row.is_full_pass()).count();
            let wanted = format!("{passes}/{} [", rows.len());
            let lead = format!("| {} | ", key.0);
            ensure(
                lines.iter().any(|line| line.starts_with(&lead) && line.contains(&wanted)),
                || format!("no `{}` row holds `{wanted}`", key.0),
            )?;
        }
        trace.record(&tables);
        Ok(())
    })
}

/// A text field of a summary cell.
fn text_of<'cell>(cell: &'cell Value, name: &str) -> Option<&'cell str> {
    cell.get(name).and_then(Value::as_str)
}

/// A whole-number field of a summary cell.
fn number_of(cell: &Value, name: &str) -> Option<u64> {
    cell.get(name).and_then(Value::as_u64)
}

/// Requirement: the summary has one cell per (model, arm, task, profile) in key
/// order, each cell counts its trials and its compiles exactly, lists every trial
/// in trial order, and no trial is counted twice or lost.
#[test]
fn the_summary_holds_one_cell_per_key_counting_every_trial_once() -> Result<(), Failure> {
    let bench = committed()?;
    replay_sweep("summary", 300, |rng, trace| {
        let records = records_of(rng, &bench)?;
        let keys: BTreeSet<(String, String, String, String)> = records
            .iter()
            .map(|row| (row.model.clone(), row.arm.clone(), row.task.clone(), row.profile.clone()))
            .collect();
        let cells = summarize(&records);
        ensure(cells.len() == keys.len(), || {
            format!("{} cells for {} keys", cells.len(), keys.len())
        })?;
        let mut seen = 0_u64;
        for (cell, key) in cells.iter().zip(&keys) {
            let rows: Vec<&Record> = records
                .iter()
                .filter(|row| row.model == key.0 && row.arm == key.1 && row.task == key.2 && row.profile == key.3)
                .collect();
            ensure(
                text_of(cell, "model") == Some(key.0.as_str())
                    && text_of(cell, "api") == Some(key.1.as_str())
                    && text_of(cell, "task") == Some(key.2.as_str())
                    && text_of(cell, "profile") == Some(key.3.as_str()),
                || format!("the cell for {key:?} is under another key or out of order"),
            )?;
            let compiled = rows.iter().filter(|row| row.compiled).count();
            ensure(
                number_of(cell, "n") == Some(wide(rows.len())) && number_of(cell, "compiled") == Some(wide(compiled)),
                || format!("the cell for {key:?} miscounts its trials or compiles"),
            )?;
            let trials: Vec<u64> = match cell.get("oracle_per_trial").and_then(Value::as_array) {
                Some(listed) => listed.iter().filter_map(|item| number_of(item, "trial")).collect(),
                None => return Err(format!("the cell for {key:?} lists no trials").into()),
            };
            ensure(trials.len() == rows.len() && trials.is_sorted(), || {
                format!("the cell for {key:?} lists {trials:?} for {} trials", rows.len())
            })?;
            seen = seen.saturating_add(wide(rows.len()));
        }
        ensure(seen == wide(records.len()), || "a trial was counted twice or lost".to_owned())?;
        trace.record_number("cells", cells.len());
        trace.record_number("trials", seen);
        Ok(())
    })
}

/// Requirement: the summary file is one valid JSON document, one top-level key per
/// line and one group per line (so a 33-trial run stays under the file law and
/// diffs one group at a time), and an empty list is written `[]`.
#[test]
fn the_summary_file_is_valid_json_with_one_group_per_line() -> Result<(), Failure> {
    let bench = committed()?;
    replay_sweep("render", 200, |rng, trace| {
        let records = records_of(rng, &bench)?;
        let cells = summarize(&records);
        let entries = [
            ("protocol", Value::from("held".to_owned())),
            ("cells", Value::Array(cells.clone())),
            ("empty", Value::Array(Vec::new())),
        ];
        let text = render(&entries);
        let parsed = json::from_str::<Value>(&text)
            .map_err(|error| format!("the summary is not JSON: {error}"))?;
        let listed = match parsed.get("cells").and_then(Value::as_array) {
            Some(items) => items.len(),
            None => return Err("the summary lost its cells".into()),
        };
        ensure(listed == cells.len(), || format!("{listed} cells survived of {}", cells.len()))?;
        ensure(parsed.get("empty") == Some(&Value::Array(Vec::new())), || {
            "an empty list did not survive".to_owned()
        })?;
        ensure(
            text.starts_with("{\n") && text.ends_with("\n}\n") && text.contains("\"empty\": []"),
            || format!("the summary is shaped wrongly: {text:?}"),
        )?;
        ensure(text.lines().count() == cells.len().saturating_add(6), || {
            format!("{} lines for {} cells", text.lines().count(), cells.len())
        })?;
        trace.record(&text);
        Ok(())
    })
}

/// Requirement: the protocol a run is held to carries every statement the manifest
/// makes, the run's repair budget, the profiles it used and every cell it skipped
/// with the reason, so a reader of `summary.json` alone knows the rules of the run.
#[test]
fn the_protocol_carries_the_manifests_statements_the_budget_the_profiles_and_the_skips() -> Result<(), Failure> {
    let bench = committed()?;
    replay_sweep("protocol", 100, |rng, trace| {
        let task = one_of(rng, "tasks", &bench.tasks)?;
        let chosen = Selection {
            models: Vec::new(),
            arms: Vec::new(),
            tasks: vec![task.name.clone()],
            profiles: Vec::new(),
            trials: 1,
        };
        let skipped = plan(&bench, Mode::Dry, &chosen)?.skipped;
        let kept = count(rng, 1, u32::try_from(bench.profiles.len())?)?;
        let names: Vec<String> = bench.profiles.iter().take(kept).map(|found| found.name.clone()).collect();
        let budget = rng.between(1, 20);
        let table = protocol(&bench, &names, budget, &skipped);
        for statement in &bench.protocol {
            let wanted = match statement.1 {
                Setting::Words(ref words) => Value::from(words.clone()),
                Setting::Flag(flag) => Value::from(flag),
            };
            ensure(table.get(statement.0.as_str()) == Some(&wanted), || {
                format!("the statement {} was lost or changed", statement.0)
            })?;
        }
        ensure(table.get("repair_budget") == Some(&Value::from(budget)), || "the budget was lost".to_owned())?;
        ensure(table.get("profiles") == Some(&Value::from(names)), || "the profiles were lost".to_owned())?;
        let listed = match table.get("skipped_cells").and_then(Value::as_array) {
            Some(items) => items.clone(),
            None => return Err("the skipped cells were lost".into()),
        };
        ensure(listed.len() == skipped.len(), || format!("{} skips of {}", listed.len(), skipped.len()))?;
        for (entry, cell) in listed.iter().zip(&skipped) {
            ensure(
                text_of(entry, "api") == Some(cell.arm.as_str())
                    && text_of(entry, "task") == Some(cell.task.as_str())
                    && text_of(entry, "why") == Some(cell.why.as_str()),
                || format!("the skip of {}/{} changed", cell.arm, cell.task),
            )?;
        }
        trace.record_number("skips", skipped.len());
        trace.record_number("budget", budget);
        Ok(())
    })
}
