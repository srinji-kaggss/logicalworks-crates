//! The in-flight tiers: that many tasks concurrently admitted, so the bound is
//! the tier and the observed peak in flight is the tier.
//!
//! Moved from `main.rs` for #639: the behaviour, printed lines, numbers and
//! bounds are unchanged.

use super::*;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use lgwks_bot::rt::supervise::Supervisor;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::driver::{Arrival, recorded_body_unit, resident_bytes};
use crate::engine::Side;
use crate::gate::work_units_match;
use crate::openloop::{Histogram, Recorder, RecorderSet};
use crate::support::{
    completed_target, drain_supervisor, micros, print_columns, print_row, widen,
    worker_and_core_cells,
};
// ── In-flight tiers ──────────────────────────────────────────────────────────

/// How long one tier may take to admit everything it asked for before the tier is
/// reported as not reached.
const TIER_ADMIT_LIMIT: Duration = Duration::from_secs(180);

/// A gate every admitted body waits on, so a tier's in-flight count is *proved* by the
/// bodies themselves rather than asserted from a counter.
#[derive(Debug)]
pub(crate) struct Gate {
    pub(crate) entered: AtomicU64,
    pub(crate) open: AtomicBool,
    pub(crate) notify: tokio::sync::Notify,
}

impl Gate {
    /// A closed gate.
    fn new() -> Self {
        Self {
            entered: AtomicU64::new(0),
            open: AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
        }
    }

    /// Record one arrival at the gate and wait for it to open.
    async fn enter(&self) {
        self.entered.fetch_add(1, Ordering::SeqCst);
        loop {
            // The future is created before the flag is read, so an open landing
            // between the two cannot be missed — the lost-wakeup window a
            // check-then-wait ordering has.
            let notified = self.notify.notified();
            if self.open.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }

    /// Wait until `tier` bodies are parked here, and report how many got in.
    ///
    /// Bounded by [`TIER_ADMIT_LIMIT`]: a tier that cannot be admitted in that time
    /// did not saturate, it hung, and the caller reports the level reached rather than
    /// waiting for ever (INV-BOT-16's rule — requested, reached and ceiling together,
    /// so no reader is told a concurrency number nobody ran).
    async fn await_tier(&self, tier: u64) -> u64 {
        let started = Instant::now();
        while self.entered() < tier {
            if started.elapsed() > TIER_ADMIT_LIMIT {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        self.entered()
    }

    /// How many arrivals are parked at the gate.
    fn entered(&self) -> u64 {
        self.entered.load(Ordering::SeqCst)
    }

    /// Open the gate and release every waiter.
    fn release(&self) {
        self.open.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }
}

/// One tier body's two handles: the gate it parks on, and the arrival it runs after.
///
/// The facade's and the baseline's tiers differ in where the body is placed, not in what
/// it captures, so the captures are built here once: a body that was offered at a
/// different instant on one side than the other would make the tier's latency figures a
/// comparison of two harnesses.
pub(crate) struct TierBody {
    pub(crate) arrival: Arrival,
    pub(crate) gate: Arc<Gate>,
}

impl TierBody {
    /// Mint the handles for one body against a gate.
    ///
    /// The tier's bound is its own ceiling, so its bodies are the declared service cost
    /// and nothing else: a tier measures how much memory `tier` simultaneous tasks cost,
    /// and a service time inside that figure would be the measurement's own weight.
    fn new(work: &Arc<AtomicU64>, recorders: &RecorderSet, gate: &Arc<Gate>) -> Self {
        Self {
            arrival: Arrival::mint(work, recorders, Instant::now(), 0),
            gate: Arc::clone(gate),
        }
    }
}

/// Wait for the whole tier to be admitted, read the resident set at that peak, release
/// it, and report what was reached.
///
/// One function for both sides, because the order matters and the order *is* the claim:
/// the resident set is read **between** the admission and the release, which is the only
/// moment at which the tier's worth of tasks are simultaneously alive. Read before the
/// admission it would report a smaller process; read after the release it would report a
/// draining one, and the per-task figure the issue asks for would be measured at the wrong
/// moment on both sides.
///
/// Returns the level reached beside the reading because both are facts about the same
/// instant, and a caller that reported them from two places could pair a peak from one with
/// an admission count from another.
async fn admit_tier(gate: &Arc<Gate>, tier: usize) -> (u64, Option<u64>, &'static str) {
    let reached = gate.await_tier(widen(tier)).await;
    let (rss_bytes, rss_source) = resident_bytes();
    gate.release();
    (reached, rss_bytes, rss_source)
}

/// The body an in-flight tier runs: park at the gate, then the same recorded work unit
/// every open-loop arrival runs.
///
/// The park is what makes the tier a concurrency measurement: without it a body would
/// finish before its neighbour was admitted and the peak in-flight count would be an
/// artefact of scheduling rather than of the ceiling.
async fn gated_body_unit(body: TierBody) {
    body.gate.enter().await;
    let (counter, shard, intended) = body.arrival.into_parts();
    recorded_body_unit(Arrival {
        counter,
        shard,
        intended,
        body_micros: 0,
    })
    .await;
}

/// One in-flight tier's measurement.
pub(crate) struct TierRun {
    pub(crate) side: Side,
    /// The tier the caller asked for.
    pub(crate) requested: usize,
    /// How many were concurrently admitted, as the bodies themselves observed.
    pub(crate) reached: u64,
    /// The engine's own maximum permit count.
    pub(crate) ceiling: usize,
    pub(crate) placed: u64,
    pub(crate) completed: u64,
    pub(crate) aborted: u64,
    pub(crate) work_units: u64,
    pub(crate) in_flight_at_end: u64,
    pub(crate) histogram: Histogram,
    pub(crate) rss_bytes: Option<u64>,
    pub(crate) rss_source: &'static str,
    pub(crate) wall: Duration,
}

impl TierRun {
    /// p50, p95 and p99, in nanoseconds.
    pub(crate) fn percentiles(&self) -> (u64, u64, u64) {
        self.histogram.percentiles_nanos()
    }

    /// Resident bytes per concurrently admitted task, or `None` where the host cannot
    /// report RSS at all.
    pub(crate) fn rss_per_task(&self) -> Option<f64> {
        self.rss_bytes
            .map(|bytes| bytes as f64 / self.reached.max(1) as f64)
    }

    /// The RSS cell for a table: bytes, or an explicit `null`.
    pub(crate) fn rss_cell(&self) -> String {
        match self.rss_bytes {
            Some(bytes) => format!("{bytes} B"),
            None => "null".to_string(),
        }
    }

    /// The RSS-per-task cell for a table: bytes per task, or an explicit `null`.
    pub(crate) fn rss_per_task_cell(&self) -> String {
        match self.rss_per_task() {
            Some(bytes) => format!("{bytes:.0} B"),
            None => "null".to_string(),
        }
    }
}

/// Admit `tier` tasks at once through the facade, then drain them.
async fn inflight_tier_facade(tier: usize) -> Result<TierRun, String> {
    let work = Arc::new(AtomicU64::new(0));
    let recorders = RecorderSet::new();
    let gate = Arc::new(Gate::new());
    let mut supervisor = Supervisor::new(tier);
    let t0 = Instant::now();
    for _ in 0..tier {
        let body = TierBody::new(&work, &recorders, &gate);
        supervisor
            .spawn(move |_token| async move {
                gated_body_unit(body).await;
            })
            .await;
    }
    let (reached, rss_bytes, rss_source) = admit_tier(&gate, tier).await;
    let drain_started = Instant::now();
    drain_supervisor(&mut supervisor, completed_target(tier)).await;
    let stats = supervisor.stats();
    Ok(TierRun {
        side: Side::Facade,
        requested: tier,
        reached,
        ceiling: Semaphore::MAX_PERMITS,
        placed: widen(tier),
        completed: stats.succeeded,
        aborted: stats.aborted,
        work_units: work.load(Ordering::SeqCst),
        in_flight_at_end: stats.in_flight(),
        histogram: Recorder::merged(recorders.shards()),
        rss_bytes,
        rss_source,
        wall: t0.elapsed() + drain_started.elapsed(),
    })
}

/// Admit `tier` tasks at once through the raw baseline, then drain them.
///
/// The same gate, the same recorder and the same drain as the facade tier; the engine
/// surface is the only difference.
async fn inflight_tier_baseline(tier: usize) -> Result<TierRun, String> {
    let work = Arc::new(AtomicU64::new(0));
    let recorders = RecorderSet::new();
    let permits = Arc::new(Semaphore::new(tier));
    let gate = Arc::new(Gate::new());
    let mut set: JoinSet<()> = JoinSet::new();
    let t0 = Instant::now();
    for _ in 0..tier {
        let Some(permit) = Arc::clone(&permits).try_acquire_owned().ok() else {
            break;
        };
        let body = TierBody::new(&work, &recorders, &gate);
        set.spawn(async move {
            let _held = permit;
            gated_body_unit(body).await;
        });
    }
    let (reached, rss_bytes, rss_source) = admit_tier(&gate, tier).await;
    let drain_started = Instant::now();
    let mut completed = 0_u64;
    let mut aborted = 0_u64;
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(()) => completed = completed.saturating_add(1),
            Err(_) => aborted = aborted.saturating_add(1),
        }
    }
    Ok(TierRun {
        side: Side::Baseline,
        requested: tier,
        reached,
        ceiling: Semaphore::MAX_PERMITS,
        placed: completed.saturating_add(aborted),
        completed,
        aborted,
        work_units: work.load(Ordering::SeqCst),
        in_flight_at_end: 0,
        histogram: Recorder::merged(recorders.shards()),
        rss_bytes,
        rss_source,
        wall: t0.elapsed() + drain_started.elapsed(),
    })
}

/// The in-flight tiers, each the ceiling it admits at.
///
/// The distinction from the concurrency ladder is the point of this row: the ladder runs
/// *many tasks through* a fixed bound of 64, so its peak in-flight count is 64 however
/// large the task count grows. A tier here runs *that many tasks concurrently
/// admitted*, so the bound is the tier and the peak is the tier.
const INFLIGHT_TIERS: [usize; 5] = [100, 1_000, 10_000, 100_000, 1_048_576];

/// The columns of the in-flight tier table.
const TIER_COLUMNS: [&str; 9] = [
    "tier",
    "side",
    "reached",
    "placed",
    "completed",
    "p50 us",
    "p99 us",
    "peak RSS",
    "RSS/task",
];

/// Drive one in-flight tier on one side and gate it.
///
/// Every gate here is about the *claim* rather than about execution: a tier is only
/// evidence about concurrency if the tier's worth of tasks really were alive at once,
/// and if every one of them finished.
async fn in_flight_tier(tier: usize, side: Side) -> Result<TierRun, Box<dyn std::error::Error>> {
    let run = match side {
        Side::Facade => inflight_tier_facade(tier).await,
        Side::Baseline => inflight_tier_baseline(tier).await,
    }
    .map_err(|reason| -> Box<dyn std::error::Error> { reason.into() })?;
    let label = format!("in-flight tier {tier} on the {}", side.as_str());
    let requested = widen(tier);
    if run.reached != requested {
        let refusal = Err(format!(
            "{label}: only {} of {tier} tasks were concurrently admitted within {} s — \
             requested {tier}, reached {}, engine ceiling {}",
            run.reached,
            TIER_ADMIT_LIMIT.as_secs(),
            run.reached,
            run.ceiling
        )
        .into());
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "in_flight_tier: returning an error to the caller");
        return refusal;
    }
    if run.completed.saturating_add(run.aborted) != run.placed {
        let refusal = Err(format!(
            "{label}: {} of {} admitted tasks reached a terminal state",
            run.completed.saturating_add(run.aborted),
            run.placed
        )
        .into());
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "in_flight_tier: returning an error to the caller");
        return refusal;
    }
    work_units_match(&label, run.completed, run.work_units, "tasks")
        .map_err(|reason| -> Box<dyn std::error::Error> { reason.into() })?;
    if run.in_flight_at_end != 0 {
        let refusal = Err(format!(
            "{label}: {} tasks still in flight after the drain",
            run.in_flight_at_end
        )
        .into());
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "in_flight_tier: returning an error to the caller");
        return refusal;
    }
    Ok(run)
}

/// One tier's row in the results file.
///
/// **Numbers, not the rendered cells.** The table's `rss_cell()` and
/// `rss_per_task_cell()` carry their units for a human reader, and using them here wrote
/// `"peak_rss_bytes":3407872 B` into the record — which is not JSON, so every row of
/// `inflight.json` was unparseable. A results file that only parses for some readers is a
/// record nobody can rely on, and this one did not parse at all.
fn tier_json(run: &TierRun) -> String {
    let (p50, _, p99) = run.percentiles();
    let per_task = match run.rss_per_task() {
        Some(bytes) => bytes.to_string(),
        None => "null".to_string(),
    };
    let rss = match run.rss_bytes {
        Some(bytes) => bytes.to_string(),
        None => "null".to_string(),
    };
    format!(
        "{{\"tier\":{},\"side\":\"{}\",\"reached\":{},\"ceiling\":{},\"placed\":{},\
          \"completed\":{},\"aborted\":{},\"work_units\":{},\"p50_nanos\":{p50},\
          \"p99_nanos\":{p99},\"peak_rss_bytes\":{rss},\
          \"rss_bytes_per_in_flight_task\":{per_task},\
          \"rss_source\":\"{}\",\"wall_millis\":{}}}",
        run.requested,
        run.side.as_str(),
        run.reached,
        run.ceiling,
        run.placed,
        run.completed,
        run.aborted,
        run.work_units,
        run.rss_source,
        run.wall.as_millis()
    )
}

/// Drive every in-flight tier on both sides and report the memory each cost.
pub(crate) async fn in_flight_tiers(
    only: Option<usize>,
    workers: Option<usize>,
    json: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("in-flight tiers — that many tasks CONCURRENTLY ADMITTED, each tier its own ceiling");
    info!("every body parks on a gate until the whole tier is admitted, so the peak");
    info!("in-flight count is what the bodies saw and not a counter's opinion\n");
    print_columns(&TIER_COLUMNS);

    let mut json_rows: Vec<String> = Vec::new();
    for tier in INFLIGHT_TIERS {
        if only.is_some_and(|wanted| wanted != tier) {
            continue;
        }
        for side in Side::ALL {
            let run = in_flight_tier(tier, side).await?;
            let (p50, _, p99) = run.percentiles();
            print_row(&[
                tier.to_string(),
                side.as_str().to_string(),
                run.reached.to_string(),
                run.placed.to_string(),
                run.completed.to_string(),
                micros(p50),
                micros(p99),
                run.rss_cell(),
                run.rss_per_task_cell(),
            ]);
            json_rows.push(tier_json(&run));
        }
        info!("");
    }

    if let Some(path) = json {
        let (workers, cores) = worker_and_core_cells(workers);
        let body = format!(
            "{{\"tool\":\"lgwks-bench-async-inflight\",\"note\":\"each tier's ceiling is the \
              tier; every body parks on a gate until the whole tier is admitted, so the peak \
              in-flight count is observed rather than inferred\",\"runtime_workers\":{},\
              \"host_cores_visible\":{},\"tiers\":[\n{}\n]}}",
            workers,
            cores,
            json_rows.join(",\n")
        );
        std::fs::write(path, body)?;
        info!("wrote {path}");
    }
    Ok(())
}
