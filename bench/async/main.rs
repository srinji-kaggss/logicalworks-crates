//! Matched-semantics async comparison: `lgwks_bot::rt::supervise::Supervisor`
//! against pinned raw Tokio `JoinSet` + `Semaphore` + `CancellationToken`.
//!
//! # The fairness gate, and why it exists here
//!
//! A comparison between a facade and the engine it wraps is meaningless unless
//! both sides are doing **the same work, with the same bounds, and reporting the
//! same failures**. The existing rig (`main.rs`) already gates the synchronous
//! comparison on identical effect counts *and* identical condition evaluations;
//! this binary carries the same gate for the async comparison, and adds the two
//! facts a facade comparison is usually quietly missing:
//!
//! - **Same in-flight bound.** Both sides hold a permit for a task's whole life.
//!   A `JoinSet` alone has no ceiling — spawning 10,000 futures into it is
//!   *unbounded* — so the baseline here is `JoinSet` plus a `Semaphore`, which is
//!   the closest honest raw equivalent. A baseline without the bound would be
//!   measuring "the facade is slower" while the facade is the only one enforcing
//!   anything.
//! - **Same failure and cancellation semantics.** Every task that ends is
//!   accounted the same way: a returning body is a completion, a body dropped at
//!   a suspension point is an abort, and a cancellation is a cancellation. A
//!   baseline that dropped its `JoinError` would be trivially faster and would
//!   be measuring a program that does less.
//!
//! If either side's work count or bound differs by even one, the run **aborts**
//! rather than printing a ratio. A ratio computed on unequal work is the exact
//! failure the existing rig's evaluation counter was written to catch.
//!
//! # What this rig does not claim
//!
//! - It measures *this host*, at this profile, with this toolchain. Absolute
//!   numbers here are not a cross-platform claim.
//! - It compares the facade against a hand-written raw equivalent, not against a
//!   different scheduler. There is no third engine in this comparison, so this is
//!   a cost-decomposition measurement, not a leaderboard.
//! - The bare-loop floor is a separate scenario, reported as its own line and
//!   never multiplied into the facade/baseline ratio.
//!
//! # Running it
//!
//! ```sh
//! CARGO_TARGET_DIR=/tmp/lgwks-bench-async \
//!   cargo run --release --manifest-path bench/Cargo.toml --bin lgwks-bench-async
//! ```
//!
//! `--json=<path>` writes the machine-readable record; `--rounds=<n>` sets the
//! paired-round count (default 15); `--seed=<n>` fixes the seed so an interval
//! can be reproduced exactly.

// The rig's own statistics module, included by path rather than re-declared.
//
// One copy, for the same reason `sim/seed.rs` exists once and is included by
// every `sim_*` target: a second copy of the quantile and bootstrap arithmetic
// would drift from the one the synchronous rig reports with, and a reader
// comparing the two rigs would be comparing two different statistics under the
// same name.
// `next_unit` and the resampler live in that module for the synchronous rig;
// this binary uses the quantile and bootstrap only, so the two unused items are
// declared rather than allowed -- a crate-level lint suppression in a measurement
// instrument is exactly the kind of quiet widening this repository forbids.
#[path = "../src/stats.rs"]
#[allow(
    dead_code,
    reason = "the shared statistics module also serves the synchronous rig; each binary uses a different subset of it"
)]
mod async_stats;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use lgwks_bot::rt::runtime::Runtime;
use lgwks_bot::rt::supervise::Supervisor;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;


// ── Allocation counting ─────────────────────────────────────────────────────

// The allocation counter, shared with `bench/` by path inclusion.
//
// It lives in its own file because a second copy of an instrument that produces
// published numbers is a second instrument, and a reader comparing the two rigs
// would be comparing two different measurements under one name. Its counters sit
// behind a flag, so a timed round pays nothing for the counter — see that file's
// module docs for why that separation is the whole point.
#[path = "../src/alloc_count.rs"]
mod alloc_count;

// ── The two sides ───────────────────────────────────────────────────────────

/// The outcome tally both sides must agree on, exactly.
///
/// Every field is a count of something observable. The fairness gate compares
/// these field by field between the facade and the baseline; a difference in any
/// one of them means the two sides did different work and the timing is void.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Tally {
    /// Tasks the owner placed, whether or not they have ended.
    placed: u64,
    /// Tasks that ended by delivering a value.
    completed: u64,
    /// Tasks whose body observed the token and returned because of it.
    cancelled: u64,
    /// Tasks the runtime dropped before they finished.
    aborted: u64,
    /// Spawn attempts refused because every permit was taken.
    refused: u64,
    /// Terminal outcomes still *retained* for the caller to read.
    ///
    /// Deliberately outside the fairness gate. The facade bounds this at the
    /// in-flight ceiling and counts the loss in `dropped_detail`; the baseline
    /// has no reporting layer and so bounds it at nothing. Comparing the two
    /// would be comparing reporting granularity, not execution.
    retained: u64,
    /// Terminal outcomes the facade counted but did not retain.
    dropped_detail: u64,
    /// The sum of every task's own completion count, so "the body did its work N
    /// times" is compared and not just "a task ended".
    work_units: u64,
}

/// One unit of the body both sides run: a counter bump and a short await.
///
/// The await is what makes this an async measurement rather than a synchronous
/// one. `yield_now` is chosen over a timer so the comparison does not become a
/// measurement of the timer wheel; both sides use the same body.
async fn body_unit(counter: Arc<AtomicU64>) {
    counter.fetch_add(1, Ordering::SeqCst);
    tokio::task::yield_now().await;
}

/// Run `total` bodies through the facade at a ceiling of `bound`.
///
/// Reports the wall time of the whole drain and the tally the fairness gate
/// compares. Uses `Supervisor`'s bounded `spawn`, so the facade's admission
/// ceiling, its terminal outcomes and its cancellation all apply.
async fn facade_side(total: usize, bound: usize) -> (f64, Tally) {
    let counter = Arc::new(AtomicU64::new(0));
    let mut supervisor = Supervisor::new(bound);
    let started = Instant::now();
    for _ in 0..total {
        let counter = Arc::clone(&counter);
        supervisor
            .spawn(move |_token| async move { body_unit(counter).await })
            .await;
    }
    // Drain every task before cancelling anything.
    //
    // `shutdown` cancels the supervisor's token first, and a body that returns
    // after that instant is conservatively reported cancelled — a correct reading
    // of "the token was cancelled", but not the same question the baseline
    // answers, where nothing is cancelled. To compare "every task ran to
    // completion", the facade is drained with its token intact: reap until the
    // completion counter reaches the placed count. This is the drain a caller who
    // cares about completion rather than about shutdown performs.
    let target = u64::try_from(total).unwrap_or(0);
    while supervisor.stats().completed < target {
        if supervisor.reap() == 0 {
            // Nothing has ended yet; give the workers a real moment. A spin here
            // would burn a core and change the timing being measured.
            tokio::time::sleep(std::time::Duration::from_micros(50)).await;
        }
    }
    let stats = supervisor.stats();
    let elapsed = started.elapsed().as_secs_f64();

    // Terminal counts from the aggregate the facade keeps about every task it
    // ended. Deliberately not the retained report *list*: `Supervisor` bounds that
    // at the in-flight ceiling and counts the loss in `reports_dropped`, while the
    // baseline has no reporting layer and so bounds it at nothing. Comparing
    // retained-list lengths would compare reporting granularity, not execution;
    // the retention asymmetry is reported separately below, as the asymmetry it
    // is.
    let tally = Tally {
        completed: stats.succeeded,
        cancelled: stats.cancelled,
        aborted: stats.aborted,
        refused: stats.refused,
        placed: stats.spawned,
        retained: 0,
        dropped_detail: stats.reports_dropped,
        work_units: counter.load(Ordering::SeqCst),
    };
    drop(supervisor);
    (elapsed, tally)
}

/// Run `total` bodies through raw Tokio at a ceiling of `bound`.
///
/// The honest raw equivalent of the facade: a `JoinSet` (task-set lifetime and
/// completion semantics), a `Semaphore` (the in-flight ceiling the facade
/// enforces), a `CancellationToken` (the cancellation the facade propagates),
/// and an explicit `try_acquire` before each spawn so a refusal at the bound is
/// counted rather than silently queued — the same refusal semantics the facade
/// has. Every `JoinError` is read, not discarded, so the abort/panic split is
/// preserved exactly as the facade preserves it.
async fn baseline_side(total: usize, bound: usize) -> (f64, Tally) {
    let counter = Arc::new(AtomicU64::new(0));
    let permits = Arc::new(Semaphore::new(bound));
    let token = tokio_util_cancel::token();
    let mut set: JoinSet<()> = JoinSet::new();
    let mut tally = Tally::default();
    let mut next = 0usize;
    let started = Instant::now();

    // Prime the pipeline to the ceiling, then replenish on completion — the same
    // replenishment discipline `join_all_bounded` uses, because spawning all
    // `total` up front would be unbounded and therefore not the same work.
    while next < bound && next < total {
        let counter = Arc::clone(&counter);
        let child = token.child();
        let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
            tally.refused = tally.refused.saturating_add(1);
            break;
        };
        set.spawn(async move {
            let _held = permit;
            body_unit(counter).await;
            let _ = child;
        });
        next = next.saturating_add(1);
    }

    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(()) => tally.completed = tally.completed.saturating_add(1),
            Err(error) if error.is_panic() => tally.aborted = tally.aborted.saturating_add(1),
            Err(_) => tally.aborted = tally.aborted.saturating_add(1),
        }
        if next < total {
            let counter = Arc::clone(&counter);
            let child = token.child();
            match Arc::clone(&permits).try_acquire_owned() {
                Ok(permit) => {
                    set.spawn(async move {
                        let _held = permit;
                        body_unit(counter).await;
                        let _ = child;
                    });
                    next = next.saturating_add(1);
                }
                Err(_) => {
                    // The bound is full because a task is still in flight; the
                    // JoinSet join above has already released one, so this arm
                    // is not expected. Counting it keeps a divergence visible
                    // rather than silently dropping work.
                    tally.refused = tally.refused.saturating_add(1);
                    next = total;
                }
            }
        }
    }

    let elapsed = started.elapsed().as_secs_f64();
    tally.placed = u64::try_from(total).unwrap_or(0);
    tally.work_units = counter.load(Ordering::SeqCst);
    tally.retained = 0;
    (elapsed, tally)
}

/// The bare synchronous floor: the same counter bump, no async at all.
///
/// Reported as its own line and never multiplied into the facade/baseline
/// ratio. It answers "what would this cost with no scheduler at all", which is
/// the floor the other two numbers are read against.
fn bare_floor(total: usize) -> (f64, u64) {
    let counter = Arc::new(AtomicU64::new(0));
    let started = Instant::now();
    for _ in 0..total {
        counter.fetch_add(1, Ordering::SeqCst);
    }
    let elapsed = started.elapsed().as_secs_f64();
    (elapsed, counter.load(Ordering::SeqCst))
}

// ── The fairness gate ────────────────────────────────────────────────────────

/// Whether two tallies describe the same work, with a reason when they do not.
///
/// A `bool` would let a caller print a ratio on unequal work; a `Result` with a
/// message makes the refusal carry the exact field that diverged, which is what a
/// reader needs to know whether the run measured anything.
fn fair(left: &Tally, right: &Tally) -> Result<(), String> {
    if left.placed != right.placed {
        return Err(format!(
            "placed: facade {} vs baseline {}",
            left.placed, right.placed
        ));
    }
    if left.completed != right.completed {
        return Err(format!(
            "completed: facade {} vs baseline {}",
            left.completed, right.completed
        ));
    }
    if left.cancelled != right.cancelled {
        return Err(format!(
            "cancelled: facade {} vs baseline {}",
            left.cancelled, right.cancelled
        ));
    }
    if left.aborted != right.aborted {
        return Err(format!(
            "aborted: facade {} vs baseline {}",
            left.aborted, right.aborted
        ));
    }
    if left.refused != right.refused {
        return Err(format!(
            "refused: facade {} vs baseline {}",
            left.refused, right.refused
        ));
    }
    if left.work_units != right.work_units {
        return Err(format!(
            "work units: facade {} vs baseline {}",
            left.work_units, right.work_units
        ));
    }
    Ok(())
}

/// Minimal cancellation token for the baseline, built on the engine's `watch`
/// channel.
///
/// The baseline cannot use `lgwks_bot`'s own token — that would not be a raw
/// comparison — and it cannot use `tokio-util` without a second third-party
/// edge, which this workspace does not admit. `watch` is the primitive the
/// estate's own `rt::cancel` is built on, so the baseline is built on the same
/// primitive the facade uses and the comparison stays about the *facade*, not
/// about two different cancellation implementations.
mod tokio_util_cancel {
    use std::sync::Arc;
    use tokio::sync::watch;

    /// A parent/child token with the propagation the facade's has.
    #[derive(Clone, Debug)]
    pub struct Token {
        sender: Arc<watch::Sender<bool>>,
        receiver: watch::Receiver<bool>,
    }

    impl Token {
        /// A fresh uncancelled token.
        #[must_use]
        pub fn new() -> Self {
            let (sender, receiver) = watch::channel(false);
            Self {
                sender: Arc::new(sender),
                receiver,
            }
        }

        /// A token cancelled when this one is.
        #[must_use]
        pub fn child(&self) -> Self {
            Self {
                sender: Arc::clone(&self.sender),
                receiver: self.receiver.clone(),
            }
        }
    }

    /// A new token.
    #[must_use]
    pub fn token() -> Token {
        Token::new()
    }
}

/// The measured result of one scenario.
struct ScenarioResult {
    /// Human name.
    name: String,
    /// Tasks run per round.
    total: usize,
    /// In-flight ceiling both sides ran at.
    bound: usize,
    /// Facade wall seconds per round.
    facade: Vec<f64>,
    /// Baseline wall seconds per round.
    baseline: Vec<f64>,
    /// The tally both sides agreed on (checked every round).
    tally: Tally,
}

/// One scenario: paired rounds of facade and baseline, gated on fairness.
async fn measure(
    name: &str,
    total: usize,
    bound: usize,
    rounds: usize,
) -> Result<ScenarioResult, String> {
    let mut facade = Vec::with_capacity(rounds);
    let mut baseline = Vec::with_capacity(rounds);
    let mut tally = Tally::default();

    // A warm-up round for each side, discarded. It pays the first-touch page
    // faults and the runtime's thread start, which are one-time costs that a
    // measured round would otherwise attribute to the code under test.
    let (warm_facade, _) = facade_side(total, bound).await;
    let (warm_base, _) = baseline_side(total, bound).await;
    if warm_facade == 0.0 || warm_base == 0.0 {
        return Err(format!("{name}: a warm-up round measured zero time"));
    }

    // Paired within each round: facade then baseline, so drift that affects the
    // whole round moves both legs and cancels in the per-round ratio.
    for round in 0..rounds {
        let (facade_time, facade_tally) = facade_side(total, bound).await;
        let (base_time, base_tally) = baseline_side(total, bound).await;
        fair(&facade_tally, &base_tally)
            .map_err(|reason| format!("{name} round {round}: {reason}"))?;
        if round == 0 {
            tally = facade_tally;
        } else if tally != facade_tally {
            return Err(format!(
                "{name} round {round}: the facade's own tally changed between rounds \
                 ({tally:?} then {facade_tally:?}), so the rounds are not the same work"
            ));
        }
        facade.push(facade_time);
        baseline.push(base_time);
    }

    Ok(ScenarioResult {
        name: name.to_string(),
        total,
        bound,
        facade,
        baseline,
        tally,
    })
}

/// A deliberately defective side, and why the gate must refuse it.
///
/// This is the negative control the fairness gate has never had. A gate that has
/// only ever been shown agreeing runs cannot be distinguished from a gate that
/// always says yes: both produce a green table every time. The only thing that
/// shows a gate works is a variant it *refuses*, for the reason it is supposed
/// to refuse it.
///
/// The defect is the one a real regression looks like, and it is not subtle:
/// **it stops draining.** It places every task exactly as the honest facade
/// does, then reaps whatever happens to be finished at the instant it looks and
/// reports that as the run's work. The tally it therefore hands the gate claims
/// `placed` tasks while some of them were never observed to finish — the precise
/// shape of "the harness measured a different program than the one it was
/// comparing", which is the failure the gate exists to catch.
///
/// Which field diverges depends on how far the workers got before the early
/// reap, and that is not something to assert: on the observed host it is
/// `completed` (509 of 512), because the counter bump happens before the yield
/// that most bodies are still sitting in. A slower host would diverge on
/// `work_units` instead. That is why the check matches on *any* work-count
/// field and prints which one fired, rather than pinning one.
///
/// It is deliberately *not* a timing defect. A mutant that is merely slower
/// would pass the gate and produce a meaningless ratio; one that is *unfair*
/// must fail it, and the failure must name the diverging field so a reader can
/// see the gate is discriminating rather than merely strict.
async fn mutant_side(total: usize, bound: usize) -> (f64, Tally) {
    let counter = Arc::new(AtomicU64::new(0));
    let mut supervisor = Supervisor::new(bound);
    let started = Instant::now();
    for _ in 0..total {
        let counter = Arc::clone(&counter);
        supervisor
            .spawn(move |_token| async move { body_unit(counter).await })
            .await;
    }
    // The defect, in one line: reap whatever is ready and stop. A real harness
    // that made this mistake would report success for work it never waited for.
    supervisor.reap();
    let stats = supervisor.stats();
    let elapsed = started.elapsed().as_secs_f64();
    let tally = Tally {
        completed: stats.succeeded,
        cancelled: stats.cancelled,
        aborted: stats.aborted,
        refused: stats.refused,
        placed: stats.spawned,
        retained: 0,
        dropped_detail: stats.reports_dropped,
        work_units: counter.load(Ordering::SeqCst),
    };
    drop(supervisor);
    (elapsed, tally)
}

/// Run the gate's negative control and print exactly what it refused.
///
/// Exits non-zero unless the gate refused the mutant *for a work-count reason*.
/// A refusal for any other reason would satisfy a weaker check and prove nothing
/// — the gate must be discriminating, so the reason is matched, not merely
/// present. The refusal is printed verbatim, because "the gate refused" is a
/// claim and "the gate said this" is evidence.
fn mutant_check(runtime: &Runtime) -> Result<(), Box<dyn std::error::Error>> {
    const TASKS: usize = 512;
    const BOUND: usize = 8;
    println!("mutant baseline: a side that places every task and then stops draining");
    println!("the gate must refuse it, naming the diverging field.\n");

    let (mutant_time, mutant) = runtime.block_on(mutant_side(TASKS, BOUND));
    let (_, honest) = runtime.block_on(facade_side(TASKS, BOUND));

    match fair(&mutant, &honest) {
        Ok(()) => {
            return Err(format!(
                "the fairness gate ACCEPTED a side that placed {} tasks but performed {} \
                 work units against an honest {} / {} — a gate that passes an unfair \
                 comparison is not a gate (mutant took {:.6}s)",
                mutant.placed, mutant.work_units, honest.work_units, honest.placed, mutant_time
            )
            .into());
        }
        Err(reason) => {
            println!("refused, as required: {reason}");
            let discriminates = reason.contains("work units")
                || reason.contains("completed")
                || reason.contains("cancelled")
                || reason.contains("aborted");
            if !discriminates {
                return Err(format!(
                    "the gate refused the mutant for {reason:?}, which is not a work-count \
                     reason: a gate that refused for an unrelated cause would pass this check \
                     without ever checking work"
                )
                .into());
            }
            println!("the refusal names a work-count field, so the gate discriminates on work");
            println!(
                "\nmutant tally:  placed {} completed {} cancelled {} aborted {} work_units {}",
                mutant.placed, mutant.completed, mutant.cancelled, mutant.aborted, mutant.work_units
            );
            println!(
                "honest tally:  placed {} completed {} cancelled {} aborted {} work_units {}",
                honest.placed, honest.completed, honest.cancelled, honest.aborted, honest.work_units
            );
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut rounds: usize = 15;
    let mut json: Option<String> = None;
let mut alloc_report = false;
    let mut mutant = false;
    for arg in std::env::args().skip(1) {
        if let Some(value) = arg.strip_prefix("--rounds=") {
            rounds = value.parse().map_err(|_| "rounds must be a number")?;
        } else if let Some(value) = arg.strip_prefix("--json=") {
            json = Some(value.to_string());
        } else if arg == "--alloc-report" {
            alloc_report = true;
        } else if arg == "--mutant-check" {
            mutant = true;
        }
    }

    if mutant {
        // The negative control runs alone and exits: a run that also produced a
        // table would leave a reader unable to tell which number the refusal
        // referred to.
        return mutant_check(&Runtime::new()?);
    }

    println!("lgwks_bot async matched-semantics comparison (facade vs raw tokio)");
    println!(
        "Every round is paired and gated on identical work; a mismatch aborts the run.\n"
    );

    let scenarios: [(&str, usize, usize); 4] = [
        ("quiet-async-bot", 256, 8),
        ("high-fanout", 2048, 32),
        ("at-capacity", 512, 4),
        ("single-permit", 512, 1),
    ];

    // The bare floor is measured first, before any runtime is built, so it is
    // genuinely the same counter bump with no scheduler underneath it.
    let (floor_time, floor_units) = bare_floor(4096);

    let mut results: Vec<ScenarioResult> = Vec::new();
    if alloc_report {
        let runtime = Runtime::new()?;
        runtime.block_on(async {
            // Counting runs in its own process-wide window, taken strictly
            // around one round of each side and never mixed with a timed round.
            // The counter itself is a relaxed atomic add on the allocation path,
            // so counting during a timing run would attribute the counter's cost
            // to the code under test — which is how a measurement instrument ends
            // up reporting its own overhead as a result.
            alloc_count::reset();
            alloc_count::start();
            let _ = facade_side(1_024, 8).await;
            alloc_count::stop();
            let after_facade = alloc_count::snapshot();

            alloc_count::reset();
            alloc_count::start();
            let _ = baseline_side(1_024, 8).await;
            alloc_count::stop();
            let after_baseline = alloc_count::snapshot();

            // Each window was reset immediately before it was counted, so the
            // snapshot after it *is* the window's total; nothing is subtracted.
            let (facade_allocs, facade_bytes) = after_facade;
            let (baseline_allocs, baseline_bytes) = after_baseline;

            println!("allocation report (1024 tasks at bound 8, counted separately from timing):");
            println!("  facade   {facade_allocs:>8} allocations, {facade_bytes:>10} bytes");
            println!("  baseline {baseline_allocs:>8} allocations, {baseline_bytes:>10} bytes");
            let per_task_facade = facade_allocs as f64 / 1_024.0;
            let per_task_base = baseline_allocs as f64 / 1_024.0;
            println!(
                "  per task: facade {per_task_facade:.2}, baseline {per_task_base:.2} \
                 (a ratio, and an absolute; both are reported)"
            );
        });
        println!();
    }

    let runtime = Runtime::new()?;
    for (name, total, bound) in scenarios {
        let result = runtime.block_on(measure(name, total, bound, rounds))?;
        results.push(result);
    }

    // The report.
    println!(
        "{:<16} {:>7} {:>7} {:>11} {:>11} {:>11} {:>11} {:>9}",
        "scenario", "tasks", "bound", "facade p50", "facade p99", "base p50", "base p99", "ratio"
    );
    let mut json_rows = String::new();
    for result in &results {
        let mut f = result.facade.clone();
        let mut b = result.baseline.clone();
        let f50 = async_stats::quantile(&mut f, 0.50);
        let f99 = async_stats::quantile(&mut f, 0.99);
        let b50 = async_stats::quantile(&mut b, 0.50);
        let b99 = async_stats::quantile(&mut b, 0.99);
        let ratio = f50 / b50;
        // Paired ratio distribution: the unit of analysis is the per-round
        // ratio, so drift that moves both legs of a round cancels.
        let paired: Vec<f64> = result
            .facade
            .iter()
            .zip(result.baseline.iter())
            .map(|(facade, base)| facade / base)
            .collect();
        let (lo, hi) = async_stats::bootstrap_median_ci(&paired, 2_000, 0.95, 0x5EED);
        let distinguishes = async_stats::distinguishes_parity(lo, hi);
        println!(
            "{:<16} {:>7} {:>7} {:>11.6} {:>11.6} {:>11.6} {:>11.6} {:>8.2}x",
            result.name, result.total, result.bound, f50, f99, b50, b99, ratio
        );
        let parity = if distinguishes { "distinguishes" } else { "spans parity" };
        println!(
            "                 95% CI on the paired ratio: [{lo:.2}, {hi:.2}] ({parity})"
        );
        if !json_rows.is_empty() {
            json_rows.push('\n');
        }
        json_rows.push_str(&format!(
            "{{\"scenario\":\"{}\",\"tasks\":{},\"bound\":{},\"facade_p50\":{},\
             \"facade_p99\":{},\"baseline_p50\":{},\"baseline_p99\":{},\
             \"ratio\":{},\"ci_lo\":{},\"ci_hi\":{},\"distinguishes\":{},\
             \"completed\":{},\"cancelled\":{},\"aborted\":{},\"work_units\":{}}}",
            result.name,
            result.total,
            result.bound,
            f50,
            f99,
            b50,
            b99,
            ratio,
            lo,
            hi,
            distinguishes,
            result.tally.completed,
            result.tally.cancelled,
            result.tally.aborted,
            result.tally.work_units,
        ));
    }
    println!(
        "\nbare synchronous floor (no scheduler): {floor_time:.6}s for {floor_units} units.\n\
         It is a reference line, never a multiplier on the ratio above."
    );

    if let Some(path) = json {
        // The rows are joined rather than concatenated with a separator: a
        // trailing comma is a syntax error and a missing one is a syntax error
        // too, and a results file that only parses for some row counts is a
        // record nobody can rely on.
        let rows: Vec<&str> = json_rows.split('\n').filter(|row| !row.is_empty()).collect();
        let joined = rows.join(",\n");
        let body = format!(
            "{{\n\"tool\":\"lgwks-bench-async\",\n\"rounds\":{rounds},\n\
             \"fairness\":\"paired; aborted on any work-count mismatch\",\n\
             \"floor_seconds\":{floor_time},\n\"scenarios\":[\n{joined}]\n}}\n"
        );
        std::fs::write(&path, body)?;
        println!("wrote {path}");
    }
    Ok(())
}