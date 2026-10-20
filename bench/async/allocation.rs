//! Allocation attribution: each operation counted in its own process-wide window,
//! never mixed with a timed round, with the unit named on every row.
//!
//! Moved from `main.rs` for #639; the behaviour it exercised is unchanged.

use std::future::Future;

use lgwks_std::trace::info;

use lgwks_bot::rt::supervise::Supervisor;
use tokio::task::JoinSet;

use crate::alloc_count;
use crate::sides::{baseline_side, facade_side};
// ── Allocation attribution ───────────────────────────────────────────────────

/// One measured allocation window: what it exercised, what it cost, and in what unit.
///
/// `units_per_iteration` is the field the previous report was missing. Three of these
/// windows run a **wave** of 1,024 tasks per iteration and two run **one** operation per
/// iteration, so dividing every window's total by its iteration count reported a wave of
/// 1,024 tasks as "per call" beside a single `child_token` measured the same way — and
/// the headline sentence below the table read the wave's whole cost as a per-task figure.
/// Every row now states its unit, and the headline is derived per task.
pub(crate) struct AllocWindow {
    /// The operation the window timed and counted.
    pub(crate) what: &'static str,
    /// How many times it ran inside the window.
    pub(crate) iterations: u64,
    /// Tasks the operation inside one iteration creates, or one for a
    /// single-operation window.
    pub(crate) units_per_iteration: u64,
    /// What one unit is, named so the table needs no arithmetic to be read.
    pub(crate) unit: &'static str,
    pub(crate) allocs: u64,
    pub(crate) bytes: u64,
}

impl AllocWindow {
    /// Allocations per iteration.
    fn per_iteration(&self) -> f64 {
        if self.iterations == 0 {
            return 0.0;
        }
        (self.allocs as f64) / (self.iterations as f64)
    }

    /// Allocations per task, or per unit of whatever this window's unit is.
    fn per_unit(&self) -> f64 {
        let units = (self.iterations as f64) * (self.units_per_iteration.max(1) as f64);
        if units <= 0.0 {
            return 0.0;
        }
        (self.allocs as f64) / units
    }

    /// One printed line.
    fn line(&self) -> String {
        format!(
            "{:<52} {:>7} x {:<6} {:>11} allocs  {:>8.2} per {}",
            self.what,
            self.iterations,
            self.units_per_iteration,
            self.allocs,
            self.per_unit(),
            self.unit
        )
    }

    /// One results-file row.
    fn json(&self) -> String {
        format!(
            "{{\"operation\":\"{}\",\"iterations\":{},\"units_per_iteration\":{},\
              \"unit\":\"{}\",\"allocations\":{},\"bytes\":{},\
              \"allocations_per_iteration\":{},\"allocations_per_unit\":{}}}",
            self.what,
            self.iterations,
            self.units_per_iteration,
            self.unit,
            self.allocs,
            self.bytes,
            self.per_iteration(),
            self.per_unit()
        )
    }
}

/// Count the allocations one window costs.
///
/// Counting runs in its own process-wide window, strictly around the operation and
/// never mixed with a timed round: the counter is a relaxed atomic add on the
/// allocation path, so counting during a timing run would attribute the counter's own
/// cost to the code under test — which is how a measurement instrument ends up
/// reporting its overhead as a result.
async fn count_allocations<F, Fut>(
    iterations: u64,
    units_per_iteration: u64,
    what: &'static str,
    unit: &'static str,
    body: F,
) -> AllocWindow
where
    F: Fn() -> Fut,
    Fut: Future<Output = ()>,
{
    alloc_count::reset();
    alloc_count::start();
    for _ in 0..iterations {
        body().await;
    }
    alloc_count::stop();
    let (allocs, bytes) = alloc_count::snapshot();
    AllocWindow {
        what,
        iterations,
        units_per_iteration,
        unit,
        allocs,
        bytes,
    }
}

/// Attribute the facade's per-task allocations to the operations that make them up.
///
/// The issue asks for each allocation with its source line, and a source line is a
/// claim a reader can check by reading. This measures the same thing without trusting
/// the reading: each window counts one operation in isolation, so the difference
/// between the whole facade and the baseline is decomposed into the parts that were
/// measured individually rather than asserted.
///
/// The windows are chosen to be *subtractive*: the facade's spawn wave at an
/// uncontended ceiling and the same wave at a contended one, against a raw-Tokio wave
/// of the same shape, so the difference between them is the facade's admission cost
/// and the difference between contended and uncontended is the cost of waiting for a
/// slot.
pub(crate) async fn allocation_attribution(
    json: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    const TASKS: u64 = 1_024;
    const UNCONTENDED: usize = 4_096;
    const CONTENDED: usize = 8;

    info!("allocation attribution — every window counted separately from every timed round");
    info!("one wave is {TASKS} tasks, so a wave row is read per task and a single-operation");
    info!("row is read per operation; the units are named on every row\n");

    let mut windows = Vec::new();

    // ── The two ends: the whole facade wave, and the raw wave it is compared against ──
    windows.push(
        count_allocations(
            TASKS,
            TASKS,
            "facade spawn wave, contended",
            "task",
            || async {
                let _wave = facade_side(TASKS as usize, CONTENDED).await;
            },
        )
        .await,
    );
    windows.push(
        count_allocations(
            TASKS,
            TASKS,
            "raw tokio wave, contended",
            "task",
            || async {
                let _wave = baseline_side(TASKS as usize, CONTENDED).await;
            },
        )
        .await,
    );
    // The same wave at a ceiling the wave never reaches: the same work with no waiting,
    // so the difference between the two facade rows is the cost of the wait for a slot.
    windows.push(
        count_allocations(
            TASKS,
            TASKS,
            "facade spawn wave, uncontended (bound 4096)",
            "task",
            || async {
                let _wave = facade_side(TASKS as usize, UNCONTENDED).await;
            },
        )
        .await,
    );

    // ── The pieces, each counted on its own ──
    //
    // A wave is not a black box: it is a tokio spawn plus this module's own per-task
    // work, and the only way to know which half the difference is is to price each half.
    windows.push(
        count_allocations(
            TASKS,
            TASKS,
            "one raw tokio spawn (JoinSet::spawn, no body)",
            "task",
            || async {
                let mut set: JoinSet<()> = JoinSet::new();
                set.spawn(async {});
                while set.join_next().await.is_some() {}
            },
        )
        .await,
    );
    windows.push(
        count_allocations(
            TASKS,
            1,
            "one Supervisor::new(4096)",
            "supervisor",
            || async {
                let _supervisor = Supervisor::new(UNCONTENDED);
            },
        )
        .await,
    );
    windows.push(
        count_allocations(
            TASKS,
            1,
            "one CancellationToken::new (a root token)",
            "token",
            || async {
                let _token = lgwks_bot::rt::sync::CancellationToken::new();
            },
        )
        .await,
    );
    // A child cannot exist without a parent, so this window *is* the spawn path's whole
    // token cost and the child's own share of it is the difference from the root window
    // above. Two windows for it would have measured the same operation twice.
    windows.push(
        count_allocations(
            TASKS,
            1,
            "a root token plus one child (the spawn path)",
            "child token",
            || async {
                let root = lgwks_bot::rt::sync::CancellationToken::new();
                let _child = root.child_token();
            },
        )
        .await,
    );
    windows.push(
        count_allocations(TASKS, 1, "one CancellationToken clone", "clone", || async {
            let root = lgwks_bot::rt::sync::CancellationToken::new();
            let _kept = root.clone();
        })
        .await,
    );
    windows.push(
        count_allocations(
            TASKS,
            1,
            "one cancellation race (run_until_cancelled)",
            "race",
            || async {
                let root = lgwks_bot::rt::sync::CancellationToken::new();
                let raced = root.run_until_cancelled(async {});
                let _done = raced.await;
            },
        )
        .await,
    );

    for window in &windows {
        info!("  {}", window.line());
    }

    // Read by name rather than by index. An index is a second list to keep in step with
    // the first, and it silently read the wrong window the moment a duplicate was removed
    // from the middle — which is how `root_only` came to report 5 allocations for a token
    // that costs 1, and the headline below it a *negative* child token.
    let named = |needle: &str| match windows.iter().find(|window| window.what.contains(needle)) {
        Some(window) => window.per_unit(),
        None => 0.0,
    };
    let facade_wave = named("facade spawn wave, contended");
    let raw_wave = named("raw tokio wave");
    let raw_spawn = named("one raw tokio spawn");
    let root_plus_child = named("root token plus one child");
    let root_only = named("(a root token)");
    info!("\ndecomposition, all per task, from the rows above rather than asserted:");
    info!("  the facade's whole wave                    {facade_wave:>8.2} allocs per task");
    info!("  the raw wave doing the same work            {raw_wave:>8.2} allocs per task");
    info!(
        "  the facade's excess over raw               {:>8.2} allocs per task",
        facade_wave - raw_wave
    );
    info!("  a raw tokio spawn on its own                {raw_spawn:>8.2} allocs per task");
    info!("  a root token plus one child token           {root_plus_child:>8.2} allocs per token");
    info!("  a root token on its own                     {root_only:>8.2} allocs per token");
    info!(
        "  so the child token the spawn path mints     {:>8.2} allocs per task",
        root_plus_child - root_only
    );
    info!(
        "  and the facade's residual over a raw spawn  {:>8.2} allocs per task",
        facade_wave - raw_spawn
    );

    if let Some(path) = json {
        let body = format!(
            "{{\"tool\":\"lgwks-bench-async-alloc-attribution\",\"note\":\"every window is \
              counted separately from every timed round; a wave row is a per-TASK figure and a \
              single-operation row is per operation, and the unit is named on every row\",\
              \"decomposition\":{{\"facade_wave\":{facade_wave},\"raw_wave\":{raw_wave},\
              \"facade_excess_over_raw\":{},\"raw_spawn_alone\":{raw_spawn},\
              \"root_plus_child_token\":{root_plus_child},\"root_token_alone\":{root_only},\
              \"child_token_alone\":{},\"facade_residual_over_a_raw_spawn\":{}}},\
              \"windows\":[\n{}\n]}}",
            facade_wave - raw_wave,
            root_plus_child - root_only,
            facade_wave - raw_spawn,
            windows
                .iter()
                .map(AllocWindow::json)
                .collect::<Vec<_>>()
                .join(",\n")
        );
        std::fs::write(path, body)?;
        info!("wrote {path}");
    }
    Ok(())
}
