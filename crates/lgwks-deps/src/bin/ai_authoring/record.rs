//! A trial's record: what one trial measured, as the JSON line a run keeps and
//! reads back.
//!
//! A run appends each record to a durable log the moment its trial ends, so an
//! interruption keeps every finished trial, and resuming reads that log back.
//! A line the interruption tore in half does not parse, and its trial runs again.

use std::collections::BTreeMap;

use lgwks_std::json::{self, Map, Value};

use super::grade::Verdict;
use super::plan::Job;
use super::{BenchError, fail};

/// The trial id a crashed trial is filed under; resuming runs those again.
pub(super) const CRASHED: &str = "crashed";

/// What one trial measured.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Record {
    /// The model, or a stand-in label for a dry or mutant run.
    pub(super) model: String,
    /// The arm; the record calls it `api`.
    pub(super) arm: String,
    /// The task.
    pub(super) task: String,
    /// The profile; empty for a mutant.
    pub(super) profile: String,
    /// Which trial of its cell.
    pub(super) trial: u32,
    /// The trial's directory name.
    pub(super) trial_id: String,
    /// `dry`, `mutant` or `model`.
    pub(super) mode: String,
    /// Whether a candidate compiled.
    pub(super) compiled: bool,
    /// How many repairs it took.
    pub(super) repairs: u32,
    /// Whether the task's drop clause passed.
    pub(super) cleanup_ok: bool,
    /// Each oracle clause's verdict.
    pub(super) oracle: BTreeMap<String, Verdict>,
    /// The oracle run's wall time, in milliseconds.
    pub(super) oracle_wall_ms: f64,
    /// The oracle run's peak resident set, when the host reports one.
    pub(super) oracle_peak_rss_bytes: Option<u64>,
    /// Tokens the model read.
    pub(super) tokens_in: u64,
    /// Tokens the model wrote.
    pub(super) tokens_out: u64,
    /// The model's own wall time, in milliseconds.
    pub(super) model_wall_ms: u64,
    /// Time spent compiling, in milliseconds.
    pub(super) compile_wall_ms: f64,
    /// Non-blank, non-comment lines of the solution.
    pub(super) consumer_lines: usize,
    /// Author-burden tokens in the solution.
    pub(super) orchestration_sites: usize,
    /// How often each token occurs.
    pub(super) orchestration_site_tokens: BTreeMap<String, usize>,
    /// The tokens counted.
    pub(super) orchestration_token_list: Vec<String>,
    /// What the record says about the lock the trial built from.
    pub(super) lockfile: String,
    /// The commit the trial ran at.
    pub(super) lgwks_bot_sha: String,
    /// The solution.
    pub(super) lib_rs: String,
    /// Why the trial crashed, when it did.
    pub(super) error: Option<String>,
}

/// `value` to three decimals, as the committed records carry it.
pub(super) fn round_millis(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

/// A time in milliseconds as a JSON number to three decimals.
fn millis(value: f64) -> Value {
    Value::from(round_millis(value))
}

/// A count as a JSON number.
fn number<T: TryInto<u64>>(value: T) -> Value {
    match value.try_into() {
        Ok(whole) => Value::from(whole),
        Err(_) => Value::Null,
    }
}

/// The string under `key`.
fn text_of(object: &Map<String, Value>, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// The whole number under `key`.
fn whole_of(object: &Map<String, Value>, key: &str) -> Option<u64> {
    object.get(key).and_then(Value::as_u64)
}

/// The whole number under `key`, when it fits 32 bits.
fn small_of(object: &Map<String, Value>, key: &str) -> Option<u32> {
    whole_of(object, key).and_then(|found| u32::try_from(found).ok())
}

/// The whole number under `key`, as a size.
fn size_of(object: &Map<String, Value>, key: &str) -> Option<usize> {
    whole_of(object, key).and_then(|found| usize::try_from(found).ok())
}

/// The oracle's clause verdicts, when every one names a known verdict.
fn verdicts_of(object: &Map<String, Value>) -> Option<BTreeMap<String, Verdict>> {
    let clauses = object.get("oracle")?.as_object()?;
    let mut found = BTreeMap::new();
    for (name, label) in clauses {
        found.insert(name.clone(), Verdict::from_label(label.as_str()?)?);
    }
    Some(found)
}

/// How often each author-burden token occurs.
fn sites_of(object: &Map<String, Value>) -> Option<BTreeMap<String, usize>> {
    let counted = object.get("orchestration_site_tokens")?.as_object()?;
    let mut found = BTreeMap::new();
    for (token, seen) in counted {
        found.insert(token.clone(), usize::try_from(seen.as_u64()?).ok()?);
    }
    Some(found)
}

/// The strings of the array under `key`.
fn words_of(object: &Map<String, Value>, key: &str) -> Option<Vec<String>> {
    object
        .get(key)?
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_owned))
        .collect()
}

impl Record {
    /// The record of a trial that could not be run, so a crash is a recorded trial.
    pub(super) fn crashed(job: &Job, commit: &str, tokens: &[String], error: String) -> Self {
        Self {
            model: job.model.clone(),
            arm: job.arm.clone(),
            task: job.task.clone(),
            profile: job.profile.clone(),
            trial: job.trial,
            trial_id: CRASHED.to_owned(),
            mode: job.mode.record_label().to_owned(),
            compiled: false,
            repairs: 0,
            cleanup_ok: false,
            oracle: BTreeMap::new(),
            oracle_wall_ms: 0.0,
            oracle_peak_rss_bytes: None,
            tokens_in: 0,
            tokens_out: 0,
            model_wall_ms: 0,
            compile_wall_ms: 0.0,
            consumer_lines: 0,
            orchestration_sites: 0,
            orchestration_site_tokens: BTreeMap::new(),
            orchestration_token_list: tokens.to_vec(),
            lockfile: String::new(),
            lgwks_bot_sha: commit.to_owned(),
            lib_rs: String::new(),
            error: Some(error),
        }
    }

    /// How many clauses passed.
    pub(super) fn pass_count(&self) -> usize {
        self.oracle
            .values()
            .filter(|verdict| **verdict == Verdict::Pass)
            .count()
    }

    /// How many clauses the oracle has.
    pub(super) fn total(&self) -> usize {
        self.oracle.len()
    }

    /// Whether it compiled and every clause passed.
    pub(super) fn is_full_pass(&self) -> bool {
        self.compiled && self.total() > 0 && self.pass_count() == self.total()
    }

    /// The record as a JSON value.
    pub(super) fn to_value(&self) -> Value {
        let mut object = Map::new();
        let mut put = |key: &str, value: Value| {
            object.insert(key.to_owned(), value);
        };
        put("model", Value::from(self.model.clone()));
        put("api", Value::from(self.arm.clone()));
        put("task", Value::from(self.task.clone()));
        put("profile", Value::from(self.profile.clone()));
        put("trial", Value::from(self.trial));
        put("trial_id", Value::from(self.trial_id.clone()));
        put("mode", Value::from(self.mode.clone()));
        put("compiled", Value::from(self.compiled));
        put("repairs", Value::from(self.repairs));
        put(
            "oracle",
            Value::Object(
                self.oracle
                    .iter()
                    .map(|(name, verdict)| (name.clone(), Value::from(verdict.label())))
                    .collect(),
            ),
        );
        put("oracle_pass_count", number(self.pass_count()));
        put("oracle_total", number(self.total()));
        put("cleanup_ok", Value::from(self.cleanup_ok));
        put("oracle_wall_ms", millis(self.oracle_wall_ms));
        put(
            "oracle_peak_rss_bytes",
            match self.oracle_peak_rss_bytes {
                Some(bytes) => Value::from(bytes),
                None => Value::Null,
            },
        );
        put("tokens_in", Value::from(self.tokens_in));
        put("tokens_out", Value::from(self.tokens_out));
        put("model_wall_ms", Value::from(self.model_wall_ms));
        put("compile_wall_ms", millis(self.compile_wall_ms));
        put("consumer_lines", number(self.consumer_lines));
        put("orchestration_sites", number(self.orchestration_sites));
        put(
            "orchestration_site_tokens",
            Value::Object(
                self.orchestration_site_tokens
                    .iter()
                    .map(|(token, seen)| (token.clone(), number(*seen)))
                    .collect(),
            ),
        );
        put(
            "orchestration_token_list",
            Value::Array(
                self.orchestration_token_list
                    .iter()
                    .map(|token| Value::from(token.clone()))
                    .collect(),
            ),
        );
        put("lockfile", Value::from(self.lockfile.clone()));
        put("lgwks_bot_sha", Value::from(self.lgwks_bot_sha.clone()));
        put("lib_rs", Value::from(self.lib_rs.clone()));
        if let Some(ref why) = self.error {
            put("error", Value::from(why.clone()));
        }
        Value::Object(object)
    }

    /// The record a JSON value holds, or `None` when it is not a whole record.
    pub(super) fn from_value(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        Some(Self {
            model: text_of(object, "model")?,
            arm: text_of(object, "api")?,
            task: text_of(object, "task")?,
            profile: text_of(object, "profile")?,
            trial: small_of(object, "trial")?,
            trial_id: text_of(object, "trial_id")?,
            mode: text_of(object, "mode")?,
            compiled: object.get("compiled")?.as_bool()?,
            repairs: small_of(object, "repairs")?,
            cleanup_ok: object.get("cleanup_ok")?.as_bool()?,
            oracle: verdicts_of(object)?,
            oracle_wall_ms: object.get("oracle_wall_ms")?.as_f64()?,
            oracle_peak_rss_bytes: object
                .get("oracle_peak_rss_bytes")
                .and_then(Value::as_u64),
            tokens_in: whole_of(object, "tokens_in")?,
            tokens_out: whole_of(object, "tokens_out")?,
            model_wall_ms: whole_of(object, "model_wall_ms")?,
            compile_wall_ms: object.get("compile_wall_ms")?.as_f64()?,
            consumer_lines: size_of(object, "consumer_lines")?,
            orchestration_sites: size_of(object, "orchestration_sites")?,
            orchestration_site_tokens: sites_of(object)?,
            orchestration_token_list: words_of(object, "orchestration_token_list")?,
            lockfile: text_of(object, "lockfile")?,
            lgwks_bot_sha: text_of(object, "lgwks_bot_sha")?,
            lib_rs: text_of(object, "lib_rs")?,
            error: text_of(object, "error"),
        })
    }

    /// The record as one line of canonical JSON, without its newline.
    pub(super) fn to_line(&self) -> String {
        canonical(&self.to_value())
    }
}

/// `value` as JSON with every object's keys in sorted order and `, ` and `: `
/// between entries, so one run's records diff against another's key by key and
/// the same trial always writes the same bytes.
pub(super) fn canonical(value: &Value) -> String {
    match *value {
        Value::Object(ref map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let entries: Vec<String> = keys
                .into_iter()
                .filter_map(|key| {
                    map.get(key.as_str()).map(|inner| {
                        format!("{}: {}", canonical(&Value::from(key.clone())), canonical(inner))
                    })
                })
                .collect();
            format!("{{{}}}", entries.join(", "))
        }
        Value::Array(ref items) => {
            let rendered: Vec<String> = items.iter().map(canonical).collect();
            format!("[{}]", rendered.join(", "))
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            match json::to_string(value) {
                Ok(scalar) => scalar,
                Err(error) => {
                    lgwks_std::trace::debug!(%error, "a scalar could not be written as JSON");
                    "null".to_owned()
                }
            }
        }
    }
}

/// Every record of a finished run's `results.jsonl`, in file order.
///
/// Strict where [`read_log`] is forgiving: a finished run has no torn line, so a
/// line that is not a whole record is a corrupt file, named by its line number.
pub(super) fn read_results(text: &str) -> Result<Vec<Record>, BenchError> {
    let mut records = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let parsed = json::from_str::<Value>(line)
            .ok()
            .and_then(|value| Record::from_value(&value));
        match parsed {
            Some(record) => records.push(record),
            None => {
                return fail(
                    BenchError::Manifest(format!(
                        "results line {} is not a whole trial record",
                        index.saturating_add(1)
                    )),
                    "read_results",
                );
            }
        }
    }
    Ok(records)
}

/// The finished trials a log holds, by trial id, and how many lines were not whole records.
///
/// A crashed trial is not finished, and a trial id that appears twice keeps its
/// last record.
pub(super) fn read_log(log: &str) -> (BTreeMap<String, Record>, usize) {
    let mut done = BTreeMap::new();
    let mut torn = 0_usize;
    for line in log.lines().filter(|line| !line.trim().is_empty()) {
        let parsed = json::from_str::<Value>(line)
            .ok()
            .and_then(|value| Record::from_value(&value));
        match parsed {
            Some(record) if record.trial_id != CRASHED => {
                done.insert(record.trial_id.clone(), record);
            }
            Some(_) => {}
            None => torn = torn.saturating_add(1),
        }
    }
    (done, torn)
}
