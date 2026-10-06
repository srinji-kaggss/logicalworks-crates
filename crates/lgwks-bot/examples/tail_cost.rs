//! What an open pays to decide whether a cut frame was ever acknowledged (#262).
//!
//! A frame cut short by the end of the file is either an append that never finished,
//! which is trimmed, or an acknowledged frame whose length prefix was changed, which
//! is refused. Telling them apart reads the bytes behind the prefix and checks them
//! against the stored head. This measures that decision on the two stores that make
//! it, the effect journal and the run store, which share one door in
//! `journal::frame`:
//!
//! - `clean` — an open of an undamaged file: the floor the others are read against.
//! - `lengthened` — the final frame's prefix grown by a seeded 1 to 2048: refused,
//!   file untouched.
//! - `cut` — an append cut at a seeded byte of the final frame: repaired, which
//!   truncates and `fsync`s, so its cost is the device's more than the decision's.
//! - `noise` — a full-ceiling prefix over a ceiling-sized tail of noise: the worst
//!   tail the search can be asked to examine, trimmed.
//!
//! Every sample asserts the outcome it is named for, so a case that stopped doing
//! what its label says is an error rather than a flattering number. Peak RSS is
//! the process's, read with `/usr/bin/time -l` around the whole run.
//!
//! ```text
//! /usr/bin/time -l cargo run -p lgwks_bot --features script,ephemeral --release --example tail_cost
//! ```
//!
//! The output is written through `std::io::Write` rather than `println!` for the
//! reason `resume_cost` states.

use std::error::Error;
use std::io::Write;
use std::path::Path;
use std::time::Instant;

use lgwks_bot::journal::{EffectEvent, EffectJournal, FileJournal, JournalError};
use lgwks_bot::script::{FlowError, Scope, remember};
use lgwks_bot::task::{Host, MAX_RECORD_BYTES, RunStore, StoreError, Task, task};

#[path = "support/effect_key.rs"]
mod effect_key;
#[path = "support/measure.rs"]
mod measure;
#[path = "../tests/support/scratch.rs"]
mod scratch;

use scratch::Scratch;

/// Frames in the file every case starts from.
const FRAMES: u32 = 64;

/// Samples for the cases that only read.
const READ_SAMPLES: usize = 400;

/// Samples for the cases that repair, each of which pays an `fsync`.
const REPAIR_SAMPLES: usize = 60;

/// The effect journal's frame ceiling, which that store keeps private.
const JOURNAL_CEILING: usize = 64 * 1024;

/// The run store's header: the 16-byte magic whose last byte is the version.
const STORE_HEADER_BYTES: usize = 16;

/// What an open did with the file it was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The file opened, repaired if it needed to be.
    Opened,
    /// The open refused the file as corrupt.
    Refused,
    /// The open failed some other way, which no case expects.
    Failed,
}

/// One store under measurement: a file it wrote, and how to open one.
struct Subject {
    /// The label every line carries.
    name: &'static str,
    /// The bytes of the undamaged file.
    bytes: Vec<u8>,
    /// File header bytes before the first frame.
    header: usize,
    /// The store's frame ceiling.
    ceiling: usize,
    /// Open the file at a path and say what happened.
    open: fn(&Path) -> Outcome,
}

/// Open a journal file.
fn open_journal(path: &Path) -> Outcome {
    match FileJournal::open(path) {
        Ok(_) => Outcome::Opened,
        Err(JournalError::Corrupt(_)) => Outcome::Refused,
        Err(_) => Outcome::Failed,
    }
}

/// Open a run store file.
fn open_store(path: &Path) -> Outcome {
    match RunStore::open(path) {
        Ok(_) => Outcome::Opened,
        Err(StoreError::Corrupt { .. }) => Outcome::Refused,
        Err(_) => Outcome::Failed,
    }
}

/// The named future one step's task returns.
type TailFuture = std::pin::Pin<Box<dyn std::future::Future<Output = Result<u64, FlowError>>>>;

/// The task every stored frame is written by.
type TailTask = Task<fn(Scope, u32) -> TailFuture>;

/// A task whose body records one value under a step of its own.
fn tail_task() -> Result<TailTask, FlowError> {
    task("tail", tail_body)
}

/// The body, behind its nameable return type.
fn tail_body(scope: Scope, ordinal: u32) -> TailFuture {
    Box::pin(async move {
        remember(&scope, "tail", || async move {
            Ok::<_, FlowError>(u64::from(ordinal).saturating_mul(7))
        })
        .await
    })
}

/// A journal of [`FRAMES`] events, as the file it wrote.
fn journal_subject(dir: &Path) -> Result<Subject, Box<dyn Error>> {
    let path = dir.join("journal.bin");
    let mut journal = FileJournal::open(&path)?;
    for ordinal in 0..FRAMES {
        let event = EffectEvent::IntentAdmitted {
            key: effect_key::key(ordinal)?,
        };
        journal.compare_and_append(journal.tail(), &event)?;
    }
    drop(journal);
    Ok(Subject {
        name: "journal",
        bytes: std::fs::read(&path)?,
        header: 0,
        ceiling: JOURNAL_CEILING,
        open: open_journal,
    })
}

/// A run store of [`FRAMES`] records, as the file it wrote.
fn store_subject(dir: &Path) -> Result<Subject, Box<dyn Error>> {
    let host = Host::builder("bench")?.run_store(dir)?.build()?;
    let work = tail_task()?;
    for ordinal in 0..FRAMES {
        let report = lgwks_bot::block_on(host.run(&work, ordinal));
        report
            .disposition()
            .is_success()
            .then_some(())
            .ok_or_else(|| format!("store record {ordinal} failed: {:?}", report.error()))?;
    }
    let path = host
        .run_store()
        .ok_or("a host built with a store keeps one")?
        .path()
        .to_path_buf();
    drop(host);
    Ok(Subject {
        name: "store",
        bytes: std::fs::read(&path)?,
        header: STORE_HEADER_BYTES,
        ceiling: MAX_RECORD_BYTES,
        open: open_store,
    })
}

/// Where each frame begins, walked from the prefixes.
fn frame_starts(subject: &Subject) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut at = subject.header;
    while let Some(prefix) = subject.bytes.get(at..at.saturating_add(4)) {
        starts.push(at);
        let declared = prefix
            .iter()
            .fold(0usize, |whole, byte| (whole << 8) | usize::from(*byte));
        at = at.saturating_add(declared).saturating_add(36);
    }
    starts
}

/// A step of a small deterministic generator, so a run draws the same cuts.
fn next(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1);
    *state >> 33
}

/// `bytes` with the prefix at `at` replaced by `declared`.
fn with_prefix(bytes: &[u8], at: usize, declared: usize) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut out = bytes.to_vec();
    let prefix = u32::try_from(declared)?.to_be_bytes();
    for (slot, byte) in out.iter_mut().skip(at).zip(prefix) {
        *slot = byte;
    }
    Ok(out)
}

/// Time `samples` opens, each over the file `make` draws, and require each to end
/// as `want`.
fn measure_case(
    subject: &Subject,
    path: &Path,
    want: Outcome,
    samples: usize,
    mut make: impl FnMut(usize) -> Result<Vec<u8>, Box<dyn Error>>,
) -> Result<Vec<u128>, Box<dyn Error>> {
    let mut micros = Vec::with_capacity(samples);
    for sample in 0..samples {
        std::fs::write(path, make(sample)?)?;
        let started = Instant::now();
        let got = (subject.open)(path);
        micros.push(started.elapsed().as_micros());
        (got == want).then_some(()).ok_or_else(|| {
            format!(
                "{}: sample {sample} expected {want:?}, got {got:?}",
                subject.name
            )
        })?;
    }
    Ok(micros)
}

/// Report one case's percentiles.
fn report(subject: &Subject, case: &str, micros: &mut [u128]) {
    let line = measure::Summary::of(micros).line(&format!("{} {case}", subject.name));
    let mut out = std::io::stdout().lock();
    let _written = writeln!(out, "{line} n={}", micros.len());
}

/// Measure the four cases on one subject.
fn measure_subject(subject: &Subject, dir: &Path) -> Result<(), Box<dyn Error>> {
    let path = dir.join(format!("{}-case", subject.name));
    let starts = frame_starts(subject);
    let last = starts
        .last()
        .copied()
        .ok_or("the subject wrote no frames")?;
    let whole = subject.bytes.len().saturating_sub(last);
    let declared = whole.saturating_sub(36);
    let mut state = 0x9e37_79b9_7f4a_7c15u64;

    let mut clean = measure_case(subject, &path, Outcome::Opened, READ_SAMPLES, |_| {
        Ok(subject.bytes.clone())
    })?;
    report(subject, "clean", &mut clean);

    let mut lengthened = measure_case(subject, &path, Outcome::Refused, READ_SAMPLES, |_| {
        let extra = usize::try_from(next(&mut state) % 2048)?.saturating_add(1);
        with_prefix(&subject.bytes, last, declared.saturating_add(extra))
    })?;
    report(subject, "lengthened", &mut lengthened);

    let mut cut = measure_case(subject, &path, Outcome::Opened, REPAIR_SAMPLES, |_| {
        // The cut keeps one byte of the drawn offset through the end of the
        // frame, so a frame of fewer than two bytes has no cut to draw and the
        // case is refused rather than measured at an offset nobody chose.
        let span = whole
            .checked_sub(2)
            .ok_or("the final frame has no interior byte to cut on")?;
        let keep = usize::try_from(next(&mut state))?
            .checked_rem(span.saturating_add(1))
            .ok_or("a cut span of one byte admits no remainder to draw")?
            .saturating_add(1);
        let end = last.saturating_add(keep);
        Ok(subject
            .bytes
            .get(..end)
            .ok_or("a cut past the end")?
            .to_vec())
    })?;
    report(subject, "cut", &mut cut);

    let mut noise = measure_case(subject, &path, Outcome::Opened, 8, |_| {
        let mut bytes = subject.bytes.clone();
        bytes.extend_from_slice(&u32::try_from(subject.ceiling)?.to_be_bytes());
        for _ in 0..subject.ceiling.saturating_add(31) {
            bytes.push(next(&mut state).to_be_bytes()[0]);
        }
        Ok(bytes)
    })?;
    report(subject, "noise", &mut noise);
    Ok(())
}

/// Measure the journal and the run store on files of [`FRAMES`] frames.
fn main() -> Result<(), Box<dyn Error>> {
    // The scratch root is owned by a guard: a refusal half way through the sweep
    // leaves no directory behind for the next run to inherit.
    let scratch = Scratch::new("tail-bench")?;
    let journal = journal_subject(scratch.path())?;
    measure_subject(&journal, scratch.path())?;
    let store_dir = scratch.path().join("store");
    let store = store_subject(&store_dir)?;
    measure_subject(&store, scratch.path())?;
    Ok(())
}
