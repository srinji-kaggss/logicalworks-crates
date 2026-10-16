//! Simulated records: a record is written as one canonical JSON line and read
//! back, cut at every byte, repeated, corrupted, and crashed, one seed per
//! scenario.
//!
//! The line is the unit of durability, so these scenarios hold what the log
//! relies on: a line read back is the record written, a line cut anywhere is
//! never mistaken for a whole record, and the same record always writes the same
//! bytes.

use std::collections::BTreeSet;

use lgwks_std::json::{self, Map, Value};

use super::bench::Bench;
use super::plan::{Mode, trial_id};
use super::record::{CRASHED, Record, canonical, read_log, read_results};
use super::replay::{Failure, replay_sweep};
use super::seed::{Rng, Trace};
use super::sim_kit::{committed, count, ensure, job, one_of, record};

/// Every mode a record can be filed under.
const MODES: [Mode; 3] = [Mode::Dry, Mode::Mutants, Mode::Models];

/// A record of a trial of a random mode, drawn from the sequence.
fn drawn(rng: &mut Rng, bench: &Bench) -> Result<Record, Failure> {
    let mode = *one_of(rng, "modes", &MODES)?;
    let one = job(rng, bench, mode)?;
    record(rng, bench, &one)
}

/// The record a line holds, or `None` when it is not a whole one.
fn read_line(line: &str) -> Option<Record> {
    json::from_str::<Value>(line)
        .ok()
        .and_then(|value| Record::from_value(&value))
}

/// Requirement: what the log holds is what the trial measured. Every field
/// survives a write and a read, including the JSON escapes a solution's source
/// carries (RFC 8259 section 7: quotation mark, reverse solidus and the control
/// characters below U+0020 are escaped).
#[test]
fn a_record_is_read_back_exactly_as_its_line_wrote_it() -> Result<(), Failure> {
    let bench = committed()?;
    replay_sweep("round trip", 400, |rng, trace| {
        let written = drawn(rng, &bench)?;
        let line = written.to_line();
        ensure(!line.contains('\n') && !line.contains('\r'), || {
            "a record line must be one line".to_owned()
        })?;
        let read = read_line(&line);
        ensure(read.as_ref() == Some(&written), || {
            format!("the line read back as {read:?}, not the record written")
        })?;
        trace.record(&line);
        Ok(())
    })
}

/// Requirement: the same trial always writes the same bytes, whatever order the
/// fields were put together in, so one run's records diff against another's key
/// by key (the retired runner's `json.dumps(sort_keys=True)` contract, and the
/// canonical-JSON discipline of RFC 8785).
#[test]
fn a_line_has_sorted_keys_whatever_order_its_fields_were_assembled_in() -> Result<(), Failure> {
    let bench = committed()?;
    replay_sweep("canonical", 200, |rng, trace| {
        let written = drawn(rng, &bench)?;
        let Value::Object(whole) = written.to_value() else {
            return Err("a record is not a JSON object".into());
        };
        let mut keys: Vec<String> = whole.keys().cloned().collect();
        for top in (1..keys.len()).rev() {
            let other = count(rng, 0, u32::try_from(top)?)?;
            keys.swap(top, other);
        }
        let mut shuffled = Map::new();
        for key in &keys {
            if let Some(value) = whole.get(key) {
                shuffled.insert(key.clone(), value.clone());
            }
        }
        let line = canonical(&Value::Object(shuffled));
        ensure(line == written.to_line(), || {
            "the line depends on the order its fields were inserted in".to_owned()
        })?;
        ensure(line.starts_with("{\"api\": "), || {
            "the first key of a sorted record is `api`".to_owned()
        })?;
        trace.record_number("keys", keys.len());
        trace.record(&line);
        Ok(())
    })
}

/// Source: the same torn-write reasoning as SQLite's and every write-ahead log's:
/// a record is whole or it is not there. Requirement: a line cut at any byte,
/// including inside a multi-byte character, is counted torn and never read as a
/// record, and the whole line is.
#[test]
fn a_line_cut_at_any_byte_is_never_read_as_a_record() -> Result<(), Failure> {
    let bench = committed()?;
    replay_sweep("cut lines", 40, |rng, trace| {
        let written = drawn(rng, &bench)?;
        let line = written.to_line();
        let bytes = line.as_bytes();
        for cut in 0..bytes.len() {
            let prefix = String::from_utf8_lossy(bytes.get(..cut).ok_or("a cut past the line")?);
            let (done, torn) = read_log(&prefix);
            ensure(done.is_empty() && torn == usize::from(cut > 0), || {
                format!("a line cut at byte {cut} of {} was read as {} records, {torn} torn", bytes.len(), done.len())
            })?;
        }
        let (done, torn) = read_log(&line);
        ensure(done.get(&written.trial_id) == Some(&written) && torn == 0, || {
            "the whole line was not read as its record".to_owned()
        })?;
        trace.record_number("length", bytes.len());
        trace.record(&line);
        Ok(())
    })
}

/// Requirement: a trial id that appears twice keeps its last record (a trial
/// rerun after a resume supersedes the one it replaces), blank lines are
/// ignored, and every line that is not a whole record is counted.
#[test]
fn a_log_keeps_the_last_record_of_a_repeated_trial_and_counts_what_it_cannot_read() -> Result<(), Failure> {
    let bench = committed()?;
    replay_sweep("log reading", 200, |rng, trace| {
        let mut first = drawn(rng, &bench)?;
        let mut second = drawn(rng, &bench)?;
        second.trial_id.clone_from(&first.trial_id);
        first.trial_id.push_str("-first");
        let junk = count(rng, 0, 3)?;
        let text = format!(
            "{}\n\n{}{}\n   \n",
            first.to_line(),
            "{\"half\": \n".repeat(junk),
            second.to_line()
        );
        let (done, torn) = read_log(&text);
        ensure(torn == junk, || format!("{torn} lines counted torn, {junk} were"))?;
        ensure(
            done.len() == 2
                && done.get(&first.trial_id) == Some(&first)
                && done.get(&second.trial_id) == Some(&second),
            || "the log did not hold both trials".to_owned(),
        )?;
        let again = format!("{}\n{}\n", second.to_line(), {
            let mut later = second.clone();
            later.repairs = later.repairs.saturating_add(1);
            later.to_line()
        });
        let (kept, _) = read_log(&again);
        ensure(
            kept.len() == 1 && kept.get(&second.trial_id).map(|found| found.repairs) == Some(second.repairs.saturating_add(1)),
            || "a repeated trial id must keep its last record".to_owned(),
        )?;
        trace.record_number("junk", junk);
        trace.record(&text);
        Ok(())
    })
}

/// Requirement: a finished run has no torn line, so a corrupt results file is
/// named by its line number, blank lines included in the count, and nothing
/// partial is returned.
#[test]
fn a_corrupt_results_file_names_the_line_it_cannot_read() -> Result<(), Failure> {
    let bench = committed()?;
    replay_sweep("results reading", 200, |rng, trace| {
        let made = count(rng, 1, 5)?;
        let mut lines = Vec::new();
        for _ in 0..made {
            lines.push(drawn(rng, &bench)?.to_line());
        }
        let clean = format!("{}\n", lines.join("\n"));
        ensure(read_results(&clean)?.len() == made, || "a clean file lost records".to_owned())?;
        let at = count(rng, 0, u32::try_from(made)?)?;
        lines.insert(at, "{\"half\": ".to_owned());
        let broken = format!("{}\n", lines.join("\n"));
        let said = match read_results(&broken) {
            Ok(_) => return Err("a corrupt file was read".into()),
            Err(error) => error.to_string(),
        };
        let wanted = format!("results line {} ", at.saturating_add(1));
        ensure(said.contains(&wanted), || format!("the refusal said `{said}`, not `{wanted}`"))?;
        trace.record_number("at", at);
        Ok(())
    })
}

/// Requirement: a crash is a recorded trial (the retired runner's `crashed`
/// record): it names its job, carries the reason and no measurement, is filed
/// under an id that is never finished, and survives the line it is kept in.
#[test]
fn a_crashed_record_names_its_job_and_carries_no_measurement() -> Result<(), Failure> {
    let bench = committed()?;
    replay_sweep("crash records", 200, |rng, trace| {
        let mode = *one_of(rng, "modes", &MODES)?;
        let one = job(rng, &bench, mode)?;
        let reason = format!("the compiler could not start ({})", rng.next_u64());
        let crashed = Record::crashed(&one, "abc123", &bench.tokens, reason.clone());
        ensure(
            crashed.error.as_deref() == Some(reason.as_str())
                && crashed.trial_id == CRASHED
                && !crashed.compiled
                && !crashed.cleanup_ok
                && crashed.oracle.is_empty()
                && crashed.lib_rs.is_empty(),
            || "a crash carries its reason and no result".to_owned(),
        )?;
        ensure(
            crashed.model == one.model
                && crashed.arm == one.arm
                && crashed.task == one.task
                && crashed.profile == one.profile
                && crashed.trial == one.trial
                && crashed.mode == one.mode.record_label()
                && crashed.orchestration_token_list == bench.tokens,
            || "a crash names the trial it was".to_owned(),
        )?;
        ensure(trial_id(&one) != CRASHED, || "a real trial id collides with the crash id".to_owned())?;
        ensure(read_line(&crashed.to_line()).as_ref() == Some(&crashed), || {
            "a crash did not survive its line".to_owned()
        })?;
        let (done, torn) = read_log(&crashed.to_line());
        ensure(done.is_empty() && torn == 0, || "a crash was counted finished".to_owned())?;
        trace.record(&crashed.to_line());
        Ok(())
    })
}

/// Requirement: the record-drawing generator itself is not degenerate, or every
/// sweep above would be a sweep of one record.
#[test]
fn the_generator_draws_records_that_differ() -> Result<(), Failure> {
    let bench = committed()?;
    let mut lines = BTreeSet::new();
    for seed in 1..=100_u64 {
        lines.insert(drawn(&mut Rng::new(seed), &bench)?.to_line());
    }
    ensure(lines.len() > 95, || format!("100 seeds drew only {} different records", lines.len()))?;
    let mut trace = Trace::new();
    trace.record_number("distinct", lines.len());
    ensure(!trace.is_empty(), || "a trace that recorded something is empty".to_owned())
}
