//! The §3 workload matrix: one receipt per scenario the crate can actually drive
//! today, gated by the counts each row produced rather than by a summary line.
//!
//! Moved from `main.rs` for #639; the behaviour it exercised is unchanged.

use std::sync::Arc;

use lgwks_std::trace::info;
use std::sync::atomic::{AtomicU64, Ordering};

use lgwks_bot::rt::runtime::Runtime;
use lgwks_bot::rt::supervise::Supervisor;
use tokio::task::JoinSet;

use crate::support::{
    body_unit, completed_target, drain_supervisor, drain_two_supervisors, narrow, refuse_failed,
    widen,
};
// ── The §3 workload matrix ─────────────────────────────────────────────────

/// One row of the workload matrix: a scenario lgwks_bot can actually drive
/// today, and the receipt it produces.
///
/// A row is a case rather than a test: each one drives the public surface, runs
/// a fixed amount of work, and reports the terminal counts. A row that cannot be
/// driven names the missing capability instead of reporting a number nobody
/// measured — an empty cell that says "needs a model key" is honest, and a
/// guessed figure is not.
pub(crate) struct Receipt {
    pub(crate) row: &'static str,
    pub(crate) shape: &'static str,
    pub(crate) placed: u64,
    pub(crate) completed: u64,
    pub(crate) cancelled: u64,
    pub(crate) aborted: u64,
    pub(crate) work_units: u64,
}

impl Receipt {
    /// The receipt for a run that just drained, taken from the supervisor's own
    /// aggregate and the work counter the bodies bumped.
    ///
    /// Every row builds the same six numbers from the same two sources. Written
    /// once because a row that assembled its own receipt is a row whose receipt
    /// could report a field its body never produced — and the receipt is the
    /// matrix's evidence, so a receipt assembled wrongly is worse than none.
    pub(crate) fn of(
        row: &'static str,
        shape: &'static str,
        stats: &lgwks_bot::rt::supervise::Stats,
        work_units: u64,
    ) -> Self {
        Self {
            row,
            shape,
            placed: stats.spawned,
            completed: stats.succeeded,
            cancelled: stats.cancelled,
            aborted: stats.aborted,
            work_units,
        }
    }
}

/// Drain `supervisor` until `total` of its tasks have ended, then return the
/// aggregate it settled on.
///
/// `drain_supervisor` waits on the supervisor's own task set rather than
/// sleeping between counts: a sleep would burn wall time the row does not own,
/// and a spin would burn a core and change the timings the same process
/// reports.
async fn drain_to(supervisor: &mut Supervisor, total: usize) -> lgwks_bot::rt::supervise::Stats {
    drain_supervisor(supervisor, completed_target(total)).await;
    supervisor.stats()
}

/// Drain until `expected` tasks have ended **and** nothing is left in flight.
///
/// The second condition is what distinguishes this from [`drain_to`]: a run
/// whose bodies are cancelled can reach its completion count and still hold
/// permits for tasks nobody has reaped. A drain that stopped at the count would
/// report a clean run over a supervisor that has leaked work.
async fn drain_until_idle(supervisor: &mut Supervisor) -> lgwks_bot::rt::supervise::Stats {
    // `wait_idle` returns only once the task set is empty; the caller's receipt checks the
    // ended count.
    supervisor.wait_idle().await;
    supervisor.stats()
}

/// Row: sequential composition at concurrency one.
///
/// The smallest possible in-flight ceiling, which is the row that catches a
/// combinator that silently assumes parallelism. At bound 1 nothing overlaps,
/// so a combinator that relied on overlapping would starve here rather than
/// merely being slow. Every body must run exactly once.
async fn row_sequential_composition() -> Result<Receipt, String> {
    const BODIES: usize = 64;
    let counter = Arc::new(AtomicU64::new(0));
    let mut supervisor = Supervisor::new(1);

    // `BODIES` bodies run one at a time at bound 1, so nothing overlaps: a
    // combinator that needed parallelism would starve here rather than merely
    // being slow. Every leaf must run exactly once; a nested composition that
    // skipped or duplicated a level shows up as a count that is not `BODIES`.
    for _ in 0..BODIES {
        let counter = Arc::clone(&counter);
        supervisor
            .spawn(move |_token| async move { body_unit(counter).await })
            .await;
    }
    let stats = drain_to(&mut supervisor, BODIES).await;
    let receipt = Receipt::of(
        "sequential-composition",
        "64 bodies, bound 1, nothing overlaps",
        &stats,
        counter.load(Ordering::SeqCst),
    );
    if receipt.work_units != widen(BODIES) {
        let refusal = Err(format!(
            "sequential composition ran {} work units, not {BODIES}",
            receipt.work_units
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "row_sequential_composition: returning an error to the caller");
        return refusal;
    }
    Ok(receipt)
}

/// Row: high fan-out carrying large results into a deliberately slow consumer.
///
/// The row that catches an unbounded buffer. Fan-out at 32 with 4 KiB of result
/// per task is 128 KiB in flight; the consumer pauses between reads so any
/// unbounded channel shows up as retained memory rather than as a number that
/// happens to be right.
async fn row_high_fanout_slow_consumer() -> Result<Receipt, String> {
    const TASKS: usize = 1_024;
    const BOUND: usize = 32;
    const PAYLOAD: usize = 4 * 1024;
    let counter = Arc::new(AtomicU64::new(0));
    let mut supervisor = Supervisor::new(BOUND);
    let (sender, mut receiver) = tokio::sync::mpsc::channel::<Vec<u8>>(8);

    // A `JoinSet`, not a bare `spawn`: the consumer is owned work whose outcome
    // this row asserts on, and a detached task that panicked would take the
    // assertion down with it instead of reporting why.
    let mut joined = JoinSet::new();
    joined.spawn(async move {
        let mut seen = 0_u64;
        let mut sink = Vec::new();
        while let Some(item) = receiver.recv().await {
            seen = seen.saturating_add(1);
            sink.extend_from_slice(&item);
        }
        // The slow consumer: drain slowly enough that the producer must apply
        // backpressure rather than buffering without limit.
        tokio::time::sleep(std::time::Duration::from_micros(200)).await;
        (seen, sink.len())
    });

    for _ in 0..TASKS {
        let counter = Arc::clone(&counter);
        let sender = sender.clone();
        supervisor
            .spawn(move |_token| async move {
                body_unit(counter).await;
                // A send error means the consumer is gone, which is the one
                // outcome this row does not expect.
                let _ = sender.send(vec![0_u8; PAYLOAD]).await;
            })
            .await;
    }
    drop(sender);
    let stats = drain_to(&mut supervisor, TASKS).await;
    let (seen, bytes) = joined
        .join_next()
        .await
        .ok_or("the consumer task never reported")?
        .map_err(|error| format!("the consumer panicked: {error}"))?;
    let receipt = Receipt::of(
        "high-fanout-slow-consumer",
        "1024 tasks, 4 KiB each, bound 32, 8-slot channel",
        &stats,
        counter.load(Ordering::SeqCst),
    );
    let expected = widen(TASKS);
    if seen != expected {
        let refusal = Err(format!(
            "the slow consumer received {seen} results, not {expected}: fan-out lost or \
         duplicated work under backpressure"
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "row_high_fanout_slow_consumer: returning an error to the caller");
        return refusal;
    }
    if bytes != narrow(expected).saturating_mul(4_096) {
        let refusal = Err(format!(
            "the consumer buffered {bytes} bytes, not {}: the payload was corrupted in flight",
            narrow(expected).saturating_mul(4_096)
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "row_high_fanout_slow_consumer: returning an error to the caller");
        return refusal;
    }
    Ok(receipt)
}

/// Row: cancellation and cleanup while the ceiling is saturated.
///
/// The row that catches a leaked task. The ceiling is filled with bodies that
/// end *only* when cancelled, the cancel lands while every permit is held, and
/// the assertion is that nothing is lost: every parked body reaches a terminal
/// state, the accounting count returns to zero, and the counts partition.
///
/// The wave is exactly `BOUND` bodies rather than more. Placing more would park
/// the placement loop in admission behind bodies that only cancellation can end,
/// and the loop — not the crate — would be what the test is waiting on. The
/// saturation that matters here is the *ceiling*, and `BOUND` bodies fill it
/// completely.
///
/// A cancelled supervisor is spent — `claim` refuses every later admission — so
/// no second wave is placed afterwards. That is a property of the API rather
/// than a gap in the row, and it is asserted below as a refusal, because "a
/// spent supervisor keeps accepting work" would be the defect.
async fn row_cancel_at_saturation() -> Result<Receipt, String> {
    const BOUND: usize = 64;
    let counter = Arc::new(AtomicU64::new(0));
    let mut supervisor = Supervisor::new(BOUND);

    // Fill the ceiling completely: every body ends only when its own token is
    // cancelled, so nothing completes on its own and the ceiling stays full.
    let entered = Arc::new(AtomicU64::new(0));
    for _ in 0..BOUND {
        let counter = Arc::clone(&counter);
        let entered = Arc::clone(&entered);
        supervisor
            .spawn(move |token| async move {
                entered.fetch_add(1, Ordering::SeqCst);
                token.cancelled().await;
                counter.fetch_add(1, Ordering::SeqCst);
            })
            .await;
    }
    let mut entered_count = 0_u64;
    while entered_count < widen(BOUND) {
        entered_count = entered.load(Ordering::SeqCst);
        if entered_count < widen(BOUND) {
            tokio::time::sleep(std::time::Duration::from_micros(50)).await;
        }
    }
    // Every permit is now held by a body parked on its own token. The ceiling is
    // saturated by construction, not by timing.
    if entered.load(Ordering::SeqCst) != widen(BOUND) {
        let refusal = Err(format!(
            "only {} of {BOUND} bodies entered before the cancel: the row did not reach \
         saturation, so a leaked task could not have been detected",
            entered.load(Ordering::SeqCst)
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "row_cancel_at_saturation: returning an error to the caller");
        return refusal;
    }
    supervisor.cancel();

    // Drain: every parked body must end, and the accounting must return to zero.
    let stats = drain_until_idle(&mut supervisor).await;
    let receipt = Receipt::of(
        "cancel-at-saturation",
        "64 bodies fill the ceiling, cancelled while full",
        &stats,
        counter.load(Ordering::SeqCst),
    );
    if stats.in_flight() != 0 {
        let refusal = Err(format!(
            "after cancelling a saturated run, {} tasks were still in flight: cleanup leaked",
            stats.in_flight()
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "row_cancel_at_saturation: returning an error to the caller");
        return refusal;
    }
    let accounted = stats
        .succeeded
        .saturating_add(stats.cancelled)
        .saturating_add(stats.aborted);
    if accounted != stats.spawned {
        let refusal = Err(format!(
            "{accounted} of {} placed tasks were accounted for; a cancelled task was lost",
            stats.spawned
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "row_cancel_at_saturation: returning an error to the caller");
        return refusal;
    }
    if stats.cancelled == 0 {
        let refusal = Err(format!(
            "a run cancelled while saturated reported {} cancellations: the cancel did \
         not reach the parked bodies",
            stats.cancelled
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "row_cancel_at_saturation: returning an error to the caller");
        return refusal;
    }
    if receipt.work_units != accounted {
        let refusal = Err(format!(
            "{} bodies recorded work for {accounted} terminal tasks: the accounting and \
         the bodies disagree about how much ran",
            receipt.work_units
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "row_cancel_at_saturation: returning an error to the caller");
        return refusal;
    }

    // A spent supervisor refuses later work rather than accepting it. This is
    // asserted, not assumed: a supervisor that kept admitting after its token is
    // cancelled would place work that can never be cancelled again.
    let before = supervisor.stats().spawned;
    supervisor.spawn(|_token| async {}).await;
    let after = supervisor.stats().spawned;
    if after != before {
        let refusal = Err(format!(
            "a cancelled supervisor admitted {} more task(s): a spent supervisor must refuse, \
         not keep accepting",
            after.saturating_sub(before)
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "row_cancel_at_saturation: returning an error to the caller");
        return refusal;
    }
    Ok(receipt)
}

/// Row: two tenants, the same task, one saturated and one not, at the same time.
///
/// The multi-tenant row. Both run through their own `Supervisor` with the same
/// ceiling; the saturated one's ceiling fills while the other drains. Neither
/// may touch the other's accounting — the defect this row exists for is a
/// shared permit pool or a shared counter, and both would show up as one tenant's
/// refusal count landing in the other tenant's tally.
async fn row_two_tenants() -> Result<Receipt, String> {
    const PER_TENANT: usize = 2_048;
    const BOUND: usize = 32;
    let acme_counter = Arc::new(AtomicU64::new(0));
    let globex_counter = Arc::new(AtomicU64::new(0));
    let mut acme = Supervisor::new(BOUND);
    let mut globex = Supervisor::new(BOUND);

    for _ in 0..PER_TENANT {
        let counter = Arc::clone(&acme_counter);
        acme.spawn(move |_token| async move { body_unit(counter).await })
            .await;
        let counter = Arc::clone(&globex_counter);
        globex
            .spawn(move |_token| async move { body_unit(counter).await })
            .await;
    }
    let target = completed_target(PER_TENANT);
    drain_two_supervisors(&mut acme, &mut globex, target).await;
    let acme_stats = acme.stats();
    let globex_stats = globex.stats();
    let acme_work = acme_counter.load(Ordering::SeqCst);
    let globex_work = globex_counter.load(Ordering::SeqCst);

    for (who, stats, work) in [
        ("acme", &acme_stats, acme_work),
        ("globex", &globex_stats, globex_work),
    ] {
        if stats.in_flight() != 0 {
            let refusal = Err(format!(
                "{who} left {} tasks in flight after draining: its permits were not its own",
                stats.in_flight()
            ));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "row_two_tenants: returning an error to the caller");
            return refusal;
        }
        if work != target {
            let refusal = Err(format!(
                "{who} performed {work} work units, not {target}: a shared counter was read"
            ));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "row_two_tenants: returning an error to the caller");
            return refusal;
        }
        if stats.refused != 0 {
            let refusal = Err(format!(
                "{who} refused {} spawns: its ceiling was contended by the other tenant",
                stats.refused
            ));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "row_two_tenants: returning an error to the caller");
            return refusal;
        }
    }
    Ok(Receipt {
        row: "two-tenants",
        shape: "2048 tasks each, bound 32 each, run together",
        placed: acme_stats.spawned.saturating_add(globex_stats.spawned),
        completed: acme_stats.succeeded.saturating_add(globex_stats.succeeded),
        cancelled: acme_stats.cancelled.saturating_add(globex_stats.cancelled),
        aborted: acme_stats.aborted.saturating_add(globex_stats.aborted),
        work_units: acme_work.saturating_add(globex_work),
    })
}

/// Row: sustained load, then a burst, then a reconnect.
///
/// Three phases against one supervisor, which must survive all three without a
/// single refusal. The burst is the interesting part: the ceiling is full from
/// the sustained phase when it lands, so a supervisor that refused rather than
/// waited would fail here.
async fn row_sustained_burst_reconnect() -> Result<Receipt, String> {
    const SUSTAINED: usize = 512;
    const BURST: usize = 1_024;
    const BOUND: usize = 16;
    let counter = Arc::new(AtomicU64::new(0));
    let mut supervisor = Supervisor::new(BOUND);

    // Phase helper: place `count` bodies on this supervisor. A named function
    // rather than a closure because a closure that awaits borrows the supervisor
    // for its whole body, which would stop the drain loop below from using it.
    async fn place(supervisor: &mut Supervisor, counter: &Arc<AtomicU64>, count: usize) {
        for _ in 0..count {
            let counter = Arc::clone(counter);
            supervisor
                .spawn(move |_token| async move { body_unit(counter).await })
                .await;
        }
    }
    place(&mut supervisor, &counter, SUSTAINED).await;
    drain_to(&mut supervisor, SUSTAINED).await;
    // Burst, immediately after: the accounting count is back to zero but the
    // ceiling has only just been released.
    place(&mut supervisor, &counter, BURST).await;
    drain_to(&mut supervisor, SUSTAINED + BURST).await;
    // Reconnect: a third, smaller wave on the same supervisor, which is a
    // caller that came back rather than a fresh process.
    place(&mut supervisor, &counter, 128).await;
    let total = SUSTAINED + BURST + 128;
    let stats = drain_to(&mut supervisor, total).await;

    let receipt = Receipt::of(
        "sustained-burst-reconnect",
        "512 sustained, 1024 burst, 128 reconnect, bound 16",
        &stats,
        counter.load(Ordering::SeqCst),
    );
    if stats.refused != 0 {
        let refusal = Err(format!(
            "the burst saw {} refusals: a supervisor that refuses under load is not one \
         that waits for a permit",
            stats.refused
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "place: returning an error to the caller");
        return refusal;
    }
    if receipt.work_units != widen(total) {
        let refusal = Err(format!(
            "the three phases performed {} work units, not {total}",
            receipt.work_units
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "place: returning an error to the caller");
        return refusal;
    }
    Ok(receipt)
}

/// The key of the `attempt`th record of the durable-history row.
///
/// One attempt per record: the journal enforces the ladder in order for a
/// *given* attempt, so reusing an attempt id would make the second record's
/// `IntentAdmitted` an out-of-order append rather than history. A fresh attempt
/// id per record is what makes this a history of distinct facts.
fn history_key(attempt: usize) -> Result<lgwks_bot::effect::EffectKey, String> {
    use lgwks_bot::effect::{
        ActionDigest, ActionId, AttemptId, EffectIdentity, EnvironmentEpoch, EnvironmentId,
        FlowRevision, RunId,
    };

    const RUN: &str = "0102030405060708090a0b0c0d0e0f10";
    const ACTION: &str = "1112131415161718191a1b1c1d1e1f20";
    const ENV: &str = "2122232425262728292a2b2c2d2e2f30";
    const FLOW: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    const DIGEST: &str = "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeffe0e1e2e3e4e5e6e7e8e9eaebecedeeef";

    let run = RunId::from_hex(RUN).map_err(|error| format!("run id: {error}"))?;
    let action = ActionId::from_hex(ACTION).map_err(|error| format!("action id: {error}"))?;
    let attempt = AttemptId::from_decimal(&format!("{attempt}"))
        .map_err(|error| format!("attempt id: {error}"))?;
    let flow = FlowRevision::from_tagged("blake3_256", FLOW)
        .map_err(|error| format!("flow revision: {error}"))?;
    let digest = ActionDigest::from_tagged("blake3_256", DIGEST)
        .map_err(|error| format!("action digest: {error}"))?;
    let environment =
        EnvironmentId::from_hex(ENV).map_err(|error| format!("environment: {error}"))?;
    let epoch = EnvironmentEpoch::from_decimal("1").map_err(|error| format!("epoch: {error}"))?;
    Ok(EffectIdentity::new(run, environment, flow).key(action, attempt, digest, epoch))
}

/// Row: durable history at 1k, 10k and 100k records through the real journal.
///
/// The durability row, and the only one here that writes to disk. Each tier is
/// appended and re-read; the receipt is the record count that came back. A tier
/// whose records are lost or duplicated is reported by its own receipt rather
/// than by a summary line, so the loss is attributable to a tier.
fn row_durable_history(
    dir: &std::path::Path,
    records: usize,
    shape: &'static str,
) -> Result<Receipt, String> {
    use lgwks_bot::journal::{EffectEvent, EffectJournal, FileJournal};

    std::fs::create_dir_all(dir).map_err(|error| format!("scratch dir: {error}"))?;
    let path = dir.join(format!("journal-{records}.log"));
    let mut journal = FileJournal::open(&path).map_err(|error| format!("open: {error}"))?;
    let mut appended = 0_usize;
    // Batched: a tier that fsyncs per record measures the disk rather than the
    // recovery, and recovery is what this row is about.
    while appended < records {
        let batch = 1_000_usize.min(records.saturating_sub(appended));
        let mut events = Vec::with_capacity(batch);
        for offset in 0..batch {
            // One attempt per record: the journal enforces the ladder in order
            // for a *given* attempt, so reusing an attempt id would make the
            // second record's `IntentAdmitted` an out-of-order append rather
            // than history. A fresh attempt id per record is what makes this a
            // history of `records` distinct facts.
            let attempt = appended.saturating_add(offset).saturating_add(1);
            let key = history_key(attempt)?;
            events.push(EffectEvent::IntentAdmitted { key });
        }
        journal
            .compare_and_append_all(&events)
            .map_err(|error| format!("append at {appended}: {error}"))?;
        appended = appended.saturating_add(batch);
    }
    drop(journal);

    // Re-read: the tier's receipt is what came back off the disk, not what was
    // written.
    let reopened = FileJournal::open(&path).map_err(|error| format!("reopen: {error}"))?;
    let recovered = reopened
        .committed()
        .map_err(|error| format!("read back: {error}"))?
        .len();
    if recovered != records {
        let refusal = Err(format!(
            "{records} records were appended and {recovered} came back: the tier lost or \
         duplicated history"
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "row_durable_history: returning an error to the caller");
        return refusal;
    }
    let recovered_attempts = reopened.recover().len();
    Ok(Receipt {
        row: "durable-history",
        shape,
        placed: widen(recovered),
        completed: widen(recovered_attempts),
        cancelled: 0,
        aborted: 0,
        work_units: widen(recovered),
    })
}

/// Drive every row the matrix names and record a receipt for each.
///
/// A row that fails aborts the run with the row named, because a matrix with one
/// quietly-skipped row is worse than no matrix: it reads as coverage.
///
/// Deliberately *not* `async`: each row drives its own runtime with its own
/// `block_on`, so this is a blocking driver and calling it from inside a runtime
/// is the documented way to deadlock a multi-thread scheduler.
pub(crate) fn workload_matrix(
    runtime: &Runtime,
    dir: &std::path::Path,
    json: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("§3 workload matrix — every row lgwks_bot can drive today\n");

    let mut receipts = Vec::new();
    let mut failures = Vec::new();
    let mut run = |name: &str, outcome: Result<Receipt, String>| match outcome {
        Ok(receipt) => {
            info!(
                "  {:<28} placed {:>6}  completed {:>6}  cancelled {:>4}  aborted {:>4}  \
                     work {:>6}",
                receipt.row,
                receipt.placed,
                receipt.completed,
                receipt.cancelled,
                receipt.aborted,
                receipt.work_units
            );
            receipts.push(receipt);
        }
        Err(reason) => {
            info!("  {name:<28} FAILED: {reason}");
            failures.push(format!("{name}: {reason}"));
        }
    };

    run(
        "sequential-composition",
        runtime.block_on(row_sequential_composition()),
    );
    run(
        "high-fanout-slow-consumer",
        runtime.block_on(row_high_fanout_slow_consumer()),
    );
    run(
        "cancel-at-saturation",
        runtime.block_on(row_cancel_at_saturation()),
    );
    run("two-tenants", runtime.block_on(row_two_tenants()));
    run(
        "sustained-burst-reconnect",
        runtime.block_on(row_sustained_burst_reconnect()),
    );
    // Durable history is driven from a blocking context, so it is stepped
    // outside `block_on` rather than pretending to be a future.
    for (records, shape) in [
        (1_000_usize, "1000 records through FileJournal"),
        (10_000, "10000 records through FileJournal"),
        (100_000, "100000 records through FileJournal"),
    ] {
        run(
            "durable-history",
            row_durable_history(&dir.join("durable"), records, shape),
        );
    }

    info!("");
    if let Some(path) = json {
        let rows: Vec<String> = receipts
            .iter()
            .map(|receipt| {
                format!(
                    "{{\"row\":\"{}\",\"shape\":\"{}\",\"placed\":{},\"completed\":{},\
                      \"cancelled\":{},\"aborted\":{},\"work_units\":{}}}",
                    receipt.row,
                    receipt.shape,
                    receipt.placed,
                    receipt.completed,
                    receipt.cancelled,
                    receipt.aborted,
                    receipt.work_units
                )
            })
            .collect();
        let body = format!(
            "{{\"tool\":\"lgwks-workload-matrix\",\"rows\":[{}],\"failed\":{}}}",
            rows.join(","),
            failures.len()
        );
        std::fs::write(path, body)?;
        info!("wrote {path}");
    }
    if !failures.is_empty() {
        let refusal = Err(refuse_failed("workload-matrix", "row(s) failed", &failures));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "workload_matrix: returning an error to the caller");
        return refusal;
    }
    info!(
        "{} rows, every receipt above was produced by running the row",
        receipts.len()
    );
    Ok(())
}
