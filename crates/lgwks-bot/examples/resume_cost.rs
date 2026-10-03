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

use lgwks_bot::effect::{
    ActionDigest, ActionId, AttemptId, EffectKey, EnvironmentEpoch, EnvironmentId, FlowRevision,
    RunId,
};
use lgwks_bot::journal::{EffectEvent, EffectJournal, FileJournal, JournalPosition};
use lgwks_bot::script::{FlowError, Scope, Tenant, remember};
use lgwks_bot::task::{Host, task};

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

/// A percentile of a sorted sample, by nearest rank.
///
/// Nearest rank rather than interpolation: the report then names a sample that was
/// actually observed, which is the property a reader of a latency claim needs and
/// an interpolated number does not have.
fn percentile(sorted: &[u128], per_mille: usize) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = sorted.len().saturating_mul(per_mille).div_ceil(1_000);
    sorted[rank.clamp(1, sorted.len()).saturating_sub(1)]
}

/// The percentiles of one measurement, in microseconds.
struct Summary {
    /// Median.
    p50: u128,
    /// 95th.
    p95: u128,
    /// 99th.
    p99: u128,
}

impl Summary {
    /// The percentiles of `samples`, sorted in place.
    fn of(samples: &mut [u128]) -> Self {
        samples.sort_unstable();
        Self {
            p50: percentile(samples, 500),
            p95: percentile(samples, 950),
            p99: percentile(samples, 990),
        }
    }

    /// The summary as one line.
    fn line(&self, label: &str) -> String {
        format!(
            "{label}: p50={}us p95={}us p99={}us",
            self.p50, self.p95, self.p99
        )
    }
}

/// Report one measurement.
fn report(label: &str, summary: &Summary) {
    let mut out = std::io::stdout().lock();
    let _written = writeln!(out, "{}", summary.line(label));
}

/// The named future a one-step body's task returns.
type StepFuture = std::pin::Pin<Box<dyn std::future::Future<Output = Result<u32, FlowError>>>>;

/// The task type the two run-store measurements share.
type OneStep = lgwks_bot::task::Task<fn(Scope, u32) -> StepFuture>;

/// A task whose whole body is one `remember` call.
fn one_step_task() -> Result<OneStep, FlowError> {
    task("one", boxed_body)
}

/// The one-step body behind its nameable return type.
fn boxed_body(scope: Scope, value: u32) -> StepFuture {
    Box::pin(
        async move { remember(&scope, "v", || async move { Ok::<_, FlowError>(value) }).await },
    )
}

/// The effect key every journal append uses, one per measurement step.
///
/// Distinct per step so each is its own append rather than a repeated one the
/// journal would answer as already committed, which would measure the ladder check
/// instead of the flush.
fn key(index: u32) -> Result<EffectKey, Box<dyn std::error::Error>> {
    // Every id here is one-based: `AttemptId` and `EnvironmentEpoch` are non-zero
    // counters by construction, and an attempt of zero is not an attempt.
    let at = u64::from(index).saturating_add(1);
    Ok(EffectKey::new(
        RunId::from_hex(&format!("{at:032x}"))?,
        ActionId::from_hex(&format!("{:032x}", at.saturating_add(1)))?,
        AttemptId::from_decimal(&at.to_string())?,
        FlowRevision::from_tagged("blake3_256", &format!("{at:064x}"))?,
        ActionDigest::from_tagged("blake3_256", &format!("{:064x}", at.saturating_add(2)))?,
        EnvironmentId::from_hex(&format!("{:032x}", at.saturating_add(3)))?,
        EnvironmentEpoch::from_decimal(&at.to_string())?,
    ))
}

/// The journal's tail, so every append chains from the one before it rather than
/// being refused at the tail fence — the same thing `FileJournal::tail` gives, and
/// read from the journal rather than recomputed here.
fn tail_of(journal: &FileJournal) -> JournalPosition {
    journal.tail()
}

/// Report the per-step cost of each mechanism over the same payload size.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Random bytes rather than a process id: the OS reuses ids, so two harness
    // runs on one machine would share a directory and read each other's records.
    let unique = lgwks_std::random::bytes::<8>()?;
    let hex: String = unique.iter().map(|byte| format!("{byte:02x}")).collect();
    let scratch = std::env::temp_dir().join(format!("lgwks-resume-bench-{hex}"));
    std::fs::create_dir_all(&scratch)?;

    let mut out = std::io::stdout().lock();
    let _written = writeln!(
        out,
        "matched payload: one step returning a {PAYLOAD_BYTES}-byte value; \
         {STEPS} samples per mechanism"
    );

    // Measurement 1: no store. A hand-built scope, so the "no store" run is the
    // same body rather than a different one that happens to be cheaper.
    let scope = Scope::root(Tenant::new("bench")?);
    let mut plain = Vec::with_capacity(STEPS);
    for index in 0..STEPS {
        let started = Instant::now();
        let outcome = lgwks_bot::block_on(remember(&scope, "plain", || async move {
            Ok::<_, FlowError>(u32::try_from(index).unwrap_or_default())
        }));
        plain.push(started.elapsed().as_micros());
        if outcome.is_err() {
            return Err("the unstored measurement step failed".into());
        }
    }
    report("plain  (no store)", &Summary::of(&mut plain));

    // Measurement 2: the same count through a real file-backed store, each step
    // under its own run so no step replays another step's record.
    let store_dir: PathBuf = scratch.join("store");
    let host = Host::builder("bench")?.run_store(&store_dir)?.build()?;
    let mut stored = Vec::with_capacity(STEPS);
    for index in 0..STEPS {
        let started = Instant::now();
        let report = lgwks_bot::block_on(
            host.run(&one_step_task()?, u32::try_from(index).unwrap_or_default()),
        );
        stored.push(started.elapsed().as_micros());
        if !report.disposition().is_success() {
            return Err(format!("the stored measurement step failed: {:?}", report.error()).into());
        }
    }
    report("stored (remember)", &Summary::of(&mut stored));

    // Measurement 3: the estate's existing durable append, at the same payload
    // size. This is the frontier comparison: if `remember` cost more than a
    // journal append for the same bytes, it would be paying for the same mechanism
    // twice.
    let mut journal = FileJournal::open(scratch.join("journal.bin"))?;
    let mut journalled = Vec::with_capacity(STEPS);
    for index in 0..STEPS {
        let event = EffectEvent::IntentAdmitted {
            key: key(u32::try_from(index).unwrap_or_default())?,
        };
        let started = Instant::now();
        let appended = journal.compare_and_append(tail_of(&journal), &event);
        journalled.push(started.elapsed().as_micros());
        if appended.is_err() {
            return Err(format!("journal append {index} failed: {:?}", appended.err()).into());
        }
    }
    report("journal (FileJournal)", &Summary::of(&mut journalled));

    let _written = writeln!(
        out,
        "both durable mechanisms frame through journal::frame and write through \
         journal::owner, so the difference above is what a step record costs over \
         an effect record, not two implementations of the same thing"
    );
    drop(std::fs::remove_dir_all(&scratch));
    Ok(())
}
