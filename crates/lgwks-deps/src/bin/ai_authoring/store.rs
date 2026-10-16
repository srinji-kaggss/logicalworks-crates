//! A run's directory on disk: its manifest, the durable log every finished trial
//! is appended to, and the sorted results that replace the log when the run ends.
//!
//! A run never reuses or overwrites another run's evidence: its results
//! directory and its work root are new, and an existing one is a refusal.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use lgwks_std::json::{self, Map, Value};

use super::plan::Mode;
use super::record::{Record, canonical, read_log};
use super::{BenchError, create_dir, create_dir_all, fail, read, read_lossy, write};

/// Where a run records its work root, mode and commit, so a resume finds all three.
const MANIFEST: &str = "run.json";

/// Every finished trial, appended and synced as it lands.
///
/// A long matrix that held its records in memory until the last trial would lose
/// every one to a crash, a kill or a dead battery at hour fourteen. This log is
/// what a resume reads; the sorted results replace it when the run ends.
const PARTIAL: &str = "results.partial.jsonl";

/// The sorted results of a finished run.
const RESULTS: &str = "results.jsonl";

/// The results as they are written, before they take their final name.
const STAGED: &str = "results.jsonl.staged";

/// The summary of a finished run.
const SUMMARY: &str = "summary.json";

/// What a resume finds in an interrupted run.
pub(super) struct Resumed {
    /// The run's work root.
    pub(super) work: PathBuf,
    /// The trials that finished, by id.
    pub(super) done: BTreeMap<String, Record>,
    /// Lines of the log that were not whole records.
    pub(super) torn: usize,
}

/// A path as the text the manifest holds.
fn shown(path: &Path) -> String {
    path.display().to_string()
}

/// Starts a run: creates its results directory and work root, both new, and
/// writes the manifest a resume reads.
pub(super) fn begin(dir: &Path, work: &Path, mode: Mode, commit: &str) -> Result<(), BenchError> {
    for fresh in [work, dir] {
        if let Some(parent) = fresh.parent() {
            create_dir_all(parent)?;
        }
        create_dir(fresh)?;
    }
    let mut manifest = Map::new();
    manifest.insert("work".to_owned(), Value::from(shown(work)));
    manifest.insert("mode".to_owned(), Value::from(mode.run_label()));
    manifest.insert("lgwks_bot_sha".to_owned(), Value::from(commit));
    write(
        &dir.join(MANIFEST),
        &format!("{}\n", canonical(&Value::Object(manifest))),
    )
}

/// One text field of the manifest.
fn field(manifest: &Value, key: &str, dir: &Path) -> Result<String, BenchError> {
    match manifest.get(key).and_then(Value::as_str) {
        Some(found) => Ok(found.to_owned()),
        None => fail(
            BenchError::Manifest(format!("{} has no {key} in {MANIFEST}", dir.display())),
            "field",
        ),
    }
}

/// Reopens an interrupted run.
///
/// Refused when the run finished, was another mode, or ran at another commit:
/// trials graded against two different crate trees are not one measurement. A
/// torn last line, where the interruption landed mid-append, is dropped and that
/// trial runs again.
pub(super) fn resume(dir: &Path, mode: Mode, commit: &str) -> Result<Resumed, BenchError> {
    let manifest = match json::from_str::<Value>(&read(&dir.join(MANIFEST))?) {
        Ok(found) => found,
        Err(cause) => {
            return fail(
                BenchError::Manifest(format!("{}/{MANIFEST}: {cause}", dir.display())),
                "resume",
            );
        }
    };
    if dir.join(RESULTS).exists() {
        return fail(
            BenchError::Refused(format!("{} finished; there is nothing to resume", dir.display())),
            "resume",
        );
    }
    let was = field(&manifest, "mode", dir)?;
    if was != mode.run_label() {
        return fail(
            BenchError::Refused(format!(
                "{} is a {was} run, not {}",
                dir.display(),
                mode.run_label()
            )),
            "resume",
        );
    }
    let ran_at = field(&manifest, "lgwks_bot_sha", dir)?;
    if ran_at != commit {
        return fail(
            BenchError::Refused(format!(
                "{} ran at {ran_at}, HEAD is {commit}; check that commit out to resume it",
                dir.display()
            )),
            "resume",
        );
    }
    let partial = dir.join(PARTIAL);
    let (done, torn) = if partial.exists() {
        read_log(&read_lossy(&partial)?)
    } else {
        (BTreeMap::new(), 0)
    };
    Ok(Resumed {
        work: PathBuf::from(field(&manifest, "work", dir)?),
        done,
        torn,
    })
}

/// Appends one record to the run's log and syncs it before returning.
pub(super) fn append(dir: &Path, record: &Record) -> Result<(), BenchError> {
    let path = dir.join(PARTIAL);
    let line = format!("{}\n", record.to_line());
    let written = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut log| {
            log.write_all(line.as_bytes())?;
            log.flush()?;
            log.sync_all()
        });
    match written {
        Ok(()) => Ok(()),
        Err(cause) => fail(BenchError::Io { path, cause }, "append"),
    }
}

/// Writes a finished run's sorted results and summary, and retires the log.
///
/// The sorted file takes its final name only once it is whole: written beside
/// that name and renamed over it, so an interruption here still leaves the log
/// every finished trial was appended to.
pub(super) fn finish(dir: &Path, records: &[Record], summary: &str) -> Result<(), BenchError> {
    let staged = dir.join(STAGED);
    let mut text = String::new();
    for record in records {
        text.push_str(&record.to_line());
        text.push('\n');
    }
    let wrote = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&staged)
        .and_then(|mut file| {
            file.write_all(text.as_bytes())?;
            file.sync_all()
        });
    if let Err(cause) = wrote {
        return fail(BenchError::Io { path: staged, cause }, "finish");
    }
    let results = dir.join(RESULTS);
    if let Err(cause) = fs::rename(&staged, &results) {
        return fail(BenchError::Io { path: results, cause }, "finish");
    }
    let partial = dir.join(PARTIAL);
    if partial.exists()
        && let Err(cause) = fs::remove_file(&partial)
    {
        return fail(BenchError::Io { path: partial, cause }, "finish");
    }
    write(&dir.join(SUMMARY), summary)
}

/// Renames each unfinished trial's directory so its rerun starts clean.
///
/// Renamed, not deleted: what an interrupted trial left is evidence of the
/// interruption, and a trial directory is never reused. Returns how many were set
/// aside.
pub(super) fn set_aside(work: &Path, ids: &[String]) -> Result<usize, BenchError> {
    let mut moved = 0_usize;
    for id in ids {
        let trial = work.join(id);
        if !trial.exists() {
            continue;
        }
        let mut attempt = 1_u32;
        let aside = loop {
            let candidate = work.join(format!("{id}.interrupted-{attempt}"));
            if !candidate.exists() {
                break candidate;
            }
            attempt = attempt.saturating_add(1);
        };
        if let Err(cause) = fs::rename(&trial, &aside) {
            return fail(BenchError::Io { path: trial, cause }, "set_aside");
        }
        moved = moved.saturating_add(1);
    }
    Ok(moved)
}
