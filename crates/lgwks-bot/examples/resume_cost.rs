//! Matched-workload measurement: what a durable step costs against the alternatives.
//!
//! Three measurements over **the same payload size**, because a comparison across
//! different payloads measures the payloads:
//!
//! 1. `plain` — a step body with no store: the floor, what a run costs when it
//!    claims nothing.
//! 2. `stored` — the same body through `remember` with a real file-backed store,
//!    which is the per-step cost the durability claim is made against.
//! 3. `journal` — one `FileJournal` append at the same payload size, which is the
//!    credible alternative: an existing estate capability that also writes and
//!    `fsync`s a chain-framed record. Comparing (2) against (3) is what says
//!    whether `remember` is a re-implementation or a different commitment over a
//!    shared mechanism; both now go through `journal::frame` and the same
//!    storage-owner thread.
//!
//! This is a measurement harness, not a pass/fail gate: wall time varies with the
//! machine, the shape of the distribution does not.
//!
//! ```text
//! cargo run -p lgwks_bot --features script,ephemeral --release --example resume_cost
//! ```
//!
//! The output is written through `std::io::Write` rather than `println!` because
//! `clippy::print_stdout` is forbidden workspace-wide with no example-only
//! carve-out; an example that writes explicitly says the same thing fallibly.

use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use lgwks_bot::journal::{EffectEvent, EffectJournal, FileJournal, JournalPosition};
use lgwks_bot::script::{FlowError, Scope, Tenant, remember};
use lgwks_bot::task::{Host, task};

#[path = "support/effect_key.rs"]
mod effect_key;
#[path = "../tests/support/lock.rs"]
mod lock;
#[path = "support/measure.rs"]
mod measure;
#[path = "support/scratch.rs"]
mod scratch;

use effect_key::key;
use lock::take_unpoisoned;
use scratch::Scratch;

/// How many steps each measurement performs.
const STEPS: usize = 2_000;

/// The payload every measurement frames, so the comparison is of mechanisms rather
/// than of sizes.
///
/// A few hundred bytes: what a step returning a short string archives to, and what
/// an effect event with a key and a verdict archives to. The two are close enough
/// that a difference in the numbers is a difference in what each mechanism does,
/// not in how much it wrote.
const PAYLOAD_BYTES: usize = 512;

/// Report one measurement.
fn report(label: &str, summary: &measure::Summary) {
    let mut out = std::io::stdout().lock();
    let _written = writeln!(out, "{}", summary.line(label));
}

/// The named future a one-step body's task returns.
type StepFuture = std::pin::Pin<Box<dyn std::future::Future<Output = Result<u32, FlowError>>>>;

/// The task type the two run-store measurements share.
type OneStep = lgwks_bot::task::Task<fn(Scope, (u32, u32)) -> StepFuture>;

/// A task whose whole body is one `remember` call.
fn one_step_task() -> Result<OneStep, FlowError> {
    task("one", boxed_body)
}

/// The one-step body behind its nameable return type.
///
/// The input is a pair rather than a scalar because the concurrent sweep files
/// each lane's appends under distinct steps: one lane's first append must not be a
/// repeat of its own second, or the store would answer it from the index and never
/// reach the device at all.
fn boxed_body(scope: Scope, value: (u32, u32)) -> StepFuture {
    Box::pin(async move {
        remember(&scope, "v", || async move {
            Ok::<_, FlowError>(value.0.saturating_add(value.1))
        })
        .await
    })
}

/// The journal's tail, so every append chains from the one before it rather than
/// being refused at the tail fence — the same thing `FileJournal::tail` gives, and
/// read from the journal rather than recomputed here.
fn tail_of(journal: &FileJournal) -> JournalPosition {
    journal.tail()
}

/// The concurrency tiers the sweep drives.
const TIERS: [u32; 4] = [1, 16, 256, 1_024];

/// How many appends each lane of a concurrent tier performs.
const LANE_APPENDS: u32 = 16;

/// `fsyncs / records` as hundredths, or an explicit marker when nothing was staged.
///
/// Integer arithmetic only, so the number a report carries is exactly the number a
/// reader can compare against another run's rather than one that lost a digit to
/// floating point on the way out.
fn per_record(flushes: u64, records: u64) -> Option<String> {
    let hundredths = flushes.checked_mul(100)?.checked_div(records)?;
    Some(format!("{hundredths}/100"))
}

/// The `fsyncs_per_record` clause every tier line and the stored summary carry.
///
/// One definition, because the ratio is what says whether a mechanism batches,
/// and two spellings of it would be free to drift. An absent ratio is printed as
/// `none` with the two counts beside it, never as a zero.
fn flush_clause(flushes: u64, records: u64) -> String {
    match per_record(flushes, records) {
        Some(ratio) => format!("fsyncs_per_record={ratio}"),
        None => format!("fsyncs_per_record=none (flushes={flushes} records={records})"),
    }
}

/// Report one tier's cost at `concurrency` lanes of [`LANE_APPENDS`] appends.
///
/// Each lane is its own OS thread driving its own runtime, because that is what
/// makes the appends *concurrent* from the storage owner's point of view: the owner
/// sees them arrive together or not at all, and how many share one flush is the
/// measurement.
///
/// # Errors
///
/// Whatever the host, a lane or the filesystem reports. A lane's refusal is an error
/// rather than a dropped sample, because a tier that silently lost appends would
/// report a throughput nobody achieved.
fn report_concurrent(
    scratch: &std::path::Path,
    concurrency: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    let tier_dir = scratch.join(format!("tier-{concurrency}"));
    std::fs::create_dir_all(&tier_dir)?;
    let host = Host::builder("bench")?.run_store(&tier_dir)?.build()?;
    let store = host
        .run_store()
        .ok_or("a host built with a store installed keeps one")?
        .clone();
    let samples: std::sync::Arc<std::sync::Mutex<Vec<u128>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

    let started = Instant::now();
    let lane_count = usize::try_from(concurrency).map_err(|error| {
        format!("a lane count that does not fit this host's address space: {error}")
    })?;
    let mut lanes = Vec::with_capacity(lane_count);
    let per_lane = usize::try_from(LANE_APPENDS).map_err(|error| {
        format!("a per-lane append count that does not fit this host's address space: {error}")
    })?;
    for lane in 0..concurrency {
        let host = host.clone();
        let samples = std::sync::Arc::clone(&samples);
        lanes.push(
            std::thread::Builder::new()
                .name(format!("cost-{lane}"))
                .spawn(move || -> Result<(), String> {
                    let mut local = Vec::with_capacity(per_lane);
                    let work = one_step_task().map_err(|cause| cause.to_string())?;
                    for index in 0..LANE_APPENDS {
                        let at = Instant::now();
                        let result =
                            lgwks_bot::rt::runtime::block_on(host.run(&work, (lane, index)));
                        local.push(at.elapsed().as_micros());
                        if !result.disposition().is_success() {
                            { let refusal = Err(format!(
                                "lane {lane} append {index}: {:?}",
                                result.error()
                            )); lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "report_concurrent: returning an error to the caller"); return refusal; };
                        }
                    }
                    take_unpoisoned(&samples).extend(local);
                    Ok(())
                })?,
        );
    }
    for lane in lanes {
        lane.join()
            .map_err(|panic| format!("a benchmark lane panicked: {panic:?}"))??;
    }
    let elapsed = started.elapsed();
    let acknowledged = LANE_APPENDS.saturating_mul(concurrency);
    let (flushes, staged) = store.flush_counts();
    let mut sorted = take_unpoisoned(&samples).clone();
    let summary = measure::Summary::of(&mut sorted);
    report(&format!("tier c={concurrency} n={acknowledged}"), &summary);
    let mut out = std::io::stdout().lock();
    let _written = writeln!(
        out,
        "tier c={concurrency}: elapsed={elapsed:?} fsyncs={flushes} records={staged} {}",
        flush_clause(flushes, staged)
    );
    drop(host);
    drop(std::fs::remove_dir_all(&tier_dir));
    Ok(())
}

/// Report the per-step cost of each mechanism over the same payload size.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The scratch root is owned by a guard: a refusal half way through the sweep
    // leaves no directory behind for the next run to inherit.
    let scratch = Scratch::new("resume-bench")?;

    // Every mechanism keys its record by a `u32` ordinal, so the sample count is
    // drawn in that width once here instead of being narrowed per step: a count
    // this host cannot address is a refusal, not a wrapped index.
    let steps = u32::try_from(STEPS)?;
    let step_slots = usize::try_from(steps)?;

    let mut out = std::io::stdout().lock();
    let _written = writeln!(
        out,
        "matched payload: one step returning a {PAYLOAD_BYTES}-byte value; \
         {STEPS} samples per mechanism"
    );

    // Measurement 1: no store. A hand-built scope, so the "no store" run is the
    // same body rather than a different one that happens to be cheaper.
    let scope = Scope::root(Tenant::new("bench")?);
    let mut plain = Vec::with_capacity(step_slots);
    for index in 0..steps {
        let started = Instant::now();
        let outcome = lgwks_bot::block_on(remember(&scope, "plain", || async move {
            Ok::<_, FlowError>(index)
        }));
        plain.push(started.elapsed().as_micros());
        if outcome.is_err() {
            let refusal = Err("the unstored measurement step failed".into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "main: returning an error to the caller");
            return refusal;
        }
    }
    report("plain  (no store)", &measure::Summary::of(&mut plain));

    // Measurement 2: the same count through a real file-backed store, each step
    // under its own run so no step replays another step's record.
    let store_dir: PathBuf = scratch.path().join("store");
    let host = Host::builder("bench")?.run_store(&store_dir)?.build()?;
    let mut stored = Vec::with_capacity(step_slots);
    for index in 0..steps {
        let started = Instant::now();
        let report = lgwks_bot::block_on(host.run(&one_step_task()?, (0, index)));
        stored.push(started.elapsed().as_micros());
        if !report.disposition().is_success() {
            {
                let refusal =
                    Err(format!("the stored measurement step failed: {:?}", report.error()).into());
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "main: returning an error to the caller");
                return refusal;
            };
        }
    }
    report("stored (remember)", &measure::Summary::of(&mut stored));

    // What that cost in flushes. One `sync_all` per record is the whole of the
    // defect #152 group commit removed, so the count is stated beside the latency
    // rather than left to be inferred from it.
    let (flushes, staged) = host
        .run_store()
        .ok_or("a host built with a store installed keeps one")?
        .flush_counts();
    let _written = writeln!(
        out,
        "stored (remember): fsyncs={flushes} records={staged} {}",
        flush_clause(flushes, staged)
    );

    // Measurement 2b: the same body at concurrency, which is where a batch forms.
    // Sequential runs pay one flush each — correctly, because there is nothing to
    // batch with. The sweep below is what says whether the flush rate is the
    // mechanism's ceiling or the device's.
    for tier in TIERS {
        report_concurrent(scratch.path(), tier)?;
    }

    // Measurement 3: the estate's existing durable append, at the same payload
    // size. This is the frontier comparison: if `remember` cost more than a
    // journal append for the same bytes, it would be paying for the same mechanism
    // twice.
    let mut journal = FileJournal::open(scratch.path().join("journal.bin"))?;
    let mut journalled = Vec::with_capacity(step_slots);
    for index in 0..steps {
        let event = EffectEvent::IntentAdmitted { key: key(index)? };
        let started = Instant::now();
        let appended = journal.compare_and_append(tail_of(&journal), &event);
        journalled.push(started.elapsed().as_micros());
        if appended.is_err() {
            {
                let refusal =
                    Err(format!("journal append {index} failed: {:?}", appended.err()).into());
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "main: returning an error to the caller");
                return refusal;
            };
        }
    }
    report(
        "journal (FileJournal)",
        &measure::Summary::of(&mut journalled),
    );

    let _written = writeln!(
        out,
        "both durable mechanisms frame through journal::frame and write through \
         journal::owner, so the difference above is what a step record costs over \
         an effect record, not two implementations of the same thing"
    );
    Ok(())
}
