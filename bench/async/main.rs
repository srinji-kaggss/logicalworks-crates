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

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use lgwks_bot::rt::runtime::{Builder as RuntimeBuilder, Runtime};
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

// ── The open-loop half ───────────────────────────────────────────────────────

// The open-loop generator, the log-bucketed recorder and the seeded model they
// are both checked against. One module, because the measurement and the model of
// the measurement share the schedule arithmetic (`intended_arrival`) and a second
// copy of that function is a second definition of what "the intended start" is.
mod openloop;

#[cfg(test)]
mod sim_openloop;

use openloop::{
    Histogram, Recorder, RecorderSet, SimSpec, intended_arrival, period_nanos, simulate,
};

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
/// How a facade run ends: drain everything, or stop early and report anyway.
///
/// The drain policy is the only thing that separates an honest facade run from
/// the mutant, and it is a parameter rather than a copied function so the two
/// cannot drift on anything *else* — the spawn loop, the tally, the body and the
/// ceiling are one piece of code, and the negative control differs from the
/// honest run in exactly one named decision.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Drain {
    /// Reap until every placed task has reached a terminal state.
    Complete,
    /// Reap whatever is ready at this instant and stop. The defect.
    GiveUp,
}

/// Run `total` bodies through the facade at a ceiling of `bound`.
///
/// The honest side of every comparison: same spawn loop, same body, same tally,
/// and the drain policy that waits for all of them.
async fn facade_side(total: usize, bound: usize) -> (f64, Tally) {
    facade_side_draining(total, bound, Drain::Complete).await
}

/// Run `total` bodies through the facade at a ceiling of `bound`, under
/// `drain`.
///
/// Reports the wall time of the whole drain and the tally the fairness gate
/// compares. Uses `Supervisor`'s bounded `spawn`, so the facade's admission
/// ceiling, its terminal outcomes and its cancellation all apply.
async fn facade_side_draining(total: usize, bound: usize, drain: Drain) -> (f64, Tally) {
    let counter = Arc::new(AtomicU64::new(0));
    let mut supervisor = Supervisor::new(bound);
    // The mutant's tail: its last `bound` bodies wait on a gate that opens only
    // after the tally is read. Without it the defect is a race the host decides —
    // a fast runner finishes the last in-flight bodies before the one `reap`, the
    // mutant reports every unit complete, and the gate is told it accepted an
    // unfair side when the side was in fact fair. Holding exactly `bound` bodies
    // fills the ceiling and no more, so the spawn loop still places every task.
    let held = (drain == Drain::GiveUp).then(|| Arc::new(Semaphore::new(0)));
    let tail_from = total.saturating_sub(bound);
    let started = Instant::now();
    for index in 0..total {
        let counter = Arc::clone(&counter);
        let hold = held.clone().filter(|_| index >= tail_from);
        supervisor
            .spawn(move |_token| async move {
                if let Some(hold) = hold {
                    let _released = hold.acquire().await;
                }
                body_unit(counter).await;
            })
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
    if drain == Drain::GiveUp {
        // The defect, in one line: reap whatever is ready and stop. A real
        // harness that made this mistake would report success for work it never
        // waited for.
        supervisor.reap();
    } else {
        while supervisor.stats().completed < target {
            if supervisor.reap() == 0 {
                // Nothing has ended yet; give the workers a real moment. A spin
                // here would burn a core and change the timing being measured.
                tokio::time::sleep(std::time::Duration::from_micros(50)).await;
            }
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
    if let Some(held) = held {
        held.add_permits(bound);
    }
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
        let refusal = Err(format!(
            "placed: facade {} vs baseline {}",
            left.placed, right.placed
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "fair: returning an error to the caller");
        return refusal;
    }
    if left.completed != right.completed {
        let refusal = Err(format!(
            "completed: facade {} vs baseline {}",
            left.completed, right.completed
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "fair: returning an error to the caller");
        return refusal;
    }
    if left.cancelled != right.cancelled {
        let refusal = Err(format!(
            "cancelled: facade {} vs baseline {}",
            left.cancelled, right.cancelled
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "fair: returning an error to the caller");
        return refusal;
    }
    if left.aborted != right.aborted {
        let refusal = Err(format!(
            "aborted: facade {} vs baseline {}",
            left.aborted, right.aborted
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "fair: returning an error to the caller");
        return refusal;
    }
    if left.refused != right.refused {
        let refusal = Err(format!(
            "refused: facade {} vs baseline {}",
            left.refused, right.refused
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "fair: returning an error to the caller");
        return refusal;
    }
    if left.work_units != right.work_units {
        let refusal = Err(format!(
            "work units: facade {} vs baseline {}",
            left.work_units, right.work_units
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "fair: returning an error to the caller");
        return refusal;
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

    /// Mint a fresh cancellation token, unrelated to any other, so one benchmark lane's cancel never reaches another lane.
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
        let refusal = Err(format!("{name}: a warm-up round measured zero time"));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "measure: returning an error to the caller");
        return refusal;
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
            let refusal = Err(format!(
                "{name} round {round}: the facade's own tally changed between rounds \
             ({tally:?} then {facade_tally:?}), so the rounds are not the same work"
            ));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "measure: returning an error to the caller");
            return refusal;
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
///
/// Because it is the same body as the honest run with one parameter flipped, a
/// reader can check the defect by reading the parameter rather than by
/// diffing two functions.
async fn mutant_side(total: usize, bound: usize) -> (f64, Tally) {
    facade_side_draining(total, bound, Drain::GiveUp).await
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
            let refusal = Err(format!(
                "the fairness gate ACCEPTED a side that placed {} tasks but performed {} \
             work units against an honest {} / {} — a gate that passes an unfair \
             comparison is not a gate (mutant took {:.6}s)",
                mutant.placed, mutant.work_units, honest.work_units, honest.placed, mutant_time
            )
            .into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "mutant_check: returning an error to the caller");
            return refusal;
        }
        Err(reason) => {
            println!("refused, as required: {reason}");
            let discriminates = reason.contains("work units")
                || reason.contains("completed")
                || reason.contains("cancelled")
                || reason.contains("aborted");
            if !discriminates {
                let refusal = Err(format!(
                    "the gate refused the mutant for {reason:?}, which is not a work-count \
                 reason: a gate that refused for an unrelated cause would pass this check \
                 without ever checking work"
                )
                .into());
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "mutant_check: returning an error to the caller");
                return refusal;
            }
            println!("the refusal names a work-count field, so the gate discriminates on work");
            println!(
                "\nmutant tally:  placed {} completed {} cancelled {} aborted {} work_units {}",
                mutant.placed,
                mutant.completed,
                mutant.cancelled,
                mutant.aborted,
                mutant.work_units
            );
            println!(
                "honest tally:  placed {} completed {} cancelled {} aborted {} work_units {}",
                honest.placed,
                honest.completed,
                honest.cancelled,
                honest.aborted,
                honest.work_units
            );
        }
    }
    Ok(())
}

// ── The concurrency ladder: both sides, at every tier the contract names ─────

/// The tiers, each a power of ten the issue names.
const TIERS: [usize; 4] = [100, 1_000, 10_000, 100_000];

/// The in-flight ceiling every tier ran at.
///
/// Fixed rather than scaled: the point of a tier is to change the *number of
/// tasks*, and a ceiling that moved with it would measure two variables at once
/// and neither one cleanly.
const TIER_BOUND: usize = 64;

/// Paired rounds per tier. Five is enough for a p99 that means something on a
/// distribution this short while keeping the whole ladder inside a gate lane;
/// the paired ratio's interval, not the raw p99, is what the table leans on.
const TIER_ROUNDS: usize = 5;

/// The process's peak resident set size in bytes, or `None` where the host does
/// not expose it.
///
/// Linux reads its own `/proc/self/status`, which is a syscall the process
/// already has. macOS exposes no in-process equivalent without a `getrusage`
/// edge, and adding one to a measurement instrument is not worth a single
/// column — so macOS is reported as **absent** here and measured by the
/// documented `/usr/bin/time -l` wrapper in the README instead.
///
/// It is *optional* rather than required, and the reason is stated rather than
/// hidden: a peak-RSS column a host cannot fill is worse than none, because an
/// empty cell in a results file reads as "small". Where it is unavailable the
/// field is `null` and the report says so, and no figure is carried over from
/// another platform.
fn peak_rss_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in status.lines() {
            if let Some(value) = line.strip_prefix("VmHWM:") {
                let kib: u64 = value.split_whitespace().next()?.parse().ok()?;
                return Some(kib.saturating_mul(1024));
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// One tier's measured numbers, for the report and the JSON record.
struct TierResult {
    tasks: usize,
    facade: Vec<f64>,
    baseline: Vec<f64>,
    facade_work_units: u64,
    baseline_work_units: u64,
}

/// Measure both sides at one tier and return the paired samples.
///
/// Fairness is gated here too, for the same reason it is gated in `measure`: a
/// timing ratio computed on unequal work is not a slower engine, it is a
/// different program. The gate runs per round rather than once at the end, so a
/// tier that drifts mid-run is caught at the round it drifted rather than at the
/// end.
async fn measure_tier(tasks: usize) -> Result<TierResult, String> {
    let mut facade = Vec::with_capacity(TIER_ROUNDS);
    let mut baseline = Vec::with_capacity(TIER_ROUNDS);
    let mut facade_work_units = 0;
    let mut baseline_work_units = 0;

    // A warm-up per side, discarded: the first-touch page faults and thread
    // start are one-time costs a measured round would otherwise charge to the
    // code under test.
    let _ = facade_side(tasks, TIER_BOUND).await;
    let _ = baseline_side(tasks, TIER_BOUND).await;

    for round in 0..TIER_ROUNDS {
        let (facade_time, facade_tally) = facade_side(tasks, TIER_BOUND).await;
        let (base_time, base_tally) = baseline_side(tasks, TIER_BOUND).await;
        fair(&facade_tally, &base_tally)
            .map_err(|reason| format!("{tasks} tasks round {round}: {reason}"))?;
        facade.push(facade_time);
        baseline.push(base_time);
        facade_work_units = facade_tally.work_units;
        baseline_work_units = base_tally.work_units;
    }
    Ok(TierResult {
        tasks,
        facade,
        baseline,
        facade_work_units,
        baseline_work_units,
    })
}

/// The whole ladder, reported per tier with both sides' distribution.
fn tier_ladder(runtime: &Runtime, json: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    println!("concurrency ladder: both sides at every tier the contract names");
    println!("bound {TIER_BOUND} on every tier, {TIER_ROUNDS} paired rounds each");
    println!("peak RSS is the process high-water mark, read once at the end\n");

    let mut rows = Vec::new();
    for tasks in TIERS {
        let result = runtime.block_on(measure_tier(tasks))?;
        rows.push(result);
    }
    let peak = peak_rss_bytes();

    println!(
        "{:>9} {:>11} {:>11} {:>11} {:>11} {:>11} {:>11}",
        "tasks", "f p50", "f p95", "f p99", "b p50", "b p95", "b p99"
    );
    let mut json_rows = String::new();
    for row in &rows {
        let mut f = row.facade.clone();
        let mut b = row.baseline.clone();
        let percentiles = [
            async_stats::quantile(&mut f, 0.50),
            async_stats::quantile(&mut f, 0.95),
            async_stats::quantile(&mut f, 0.99),
            async_stats::quantile(&mut b, 0.50),
            async_stats::quantile(&mut b, 0.95),
            async_stats::quantile(&mut b, 0.99),
        ];
        println!(
            "{:>9} {:>11.6} {:>11.6} {:>11.6} {:>11.6} {:>11.6} {:>11.6}",
            row.tasks,
            percentiles[0],
            percentiles[1],
            percentiles[2],
            percentiles[3],
            percentiles[4],
            percentiles[5]
        );
        if !json_rows.is_empty() {
            json_rows.push('\n');
        }
        json_rows.push_str(&format!(
            "{{\"tier\":{},\"bound\":{},\"facade_p50\":{},\"facade_p95\":{},\
             \"facade_p99\":{},\"baseline_p50\":{},\"baseline_p95\":{},\"baseline_p99\":{},\
             \"facade_work_units\":{},\"baseline_work_units\":{}}}",
            row.tasks,
            TIER_BOUND,
            percentiles[0],
            percentiles[1],
            percentiles[2],
            percentiles[3],
            percentiles[4],
            percentiles[5],
            row.facade_work_units,
            row.baseline_work_units,
        ));
    }
    match peak {
        Some(bytes) => println!("\npeak RSS for the whole process: {bytes} bytes"),
        None => println!(
            "\npeak RSS: NOT AVAILABLE on this host — the field below is `null`, and no \
             value is extrapolated from another platform"
        ),
    }
    println!(
        "the ladder measures this host at this profile; the numbers are not a \
         cross-platform claim"
    );

    if let Some(path) = json {
        let peak_field = match peak {
            Some(bytes) => format!("\"peak_rss_bytes\":{bytes}"),
            None => String::from("\"peak_rss_bytes\":null"),
        };
        let body = format!(
            "{{\"tool\":\"lgwks-bench-async-ladder\",\"bound\":{TIER_BOUND},\
              \"rounds\":{TIER_ROUNDS},{peak_field},\"tiers\":[\n{json_rows}\n]}}"
        );
        std::fs::write(path, body)?;
        println!("\nwrote {path}");
    }
    Ok(())
}

// ── The §3 workload matrix ─────────────────────────────────────────────────

/// One row of the workload matrix: a scenario lgwks_bot can actually drive
/// today, and the receipt it produces.
///
/// A row is a case rather than a test: each one drives the public surface, runs
/// a fixed amount of work, and reports the terminal counts. A row that cannot be
/// driven names the missing capability instead of reporting a number nobody
/// measured — an empty cell that says "needs a model key" is honest, and a
/// guessed figure is not.
struct Receipt {
    row: &'static str,
    shape: &'static str,
    placed: u64,
    completed: u64,
    cancelled: u64,
    aborted: u64,
    work_units: u64,
}

impl Receipt {
    /// The receipt for a run that just drained, taken from the supervisor's own
    /// aggregate and the work counter the bodies bumped.
    ///
    /// Every row builds the same six numbers from the same two sources. Written
    /// once because a row that assembled its own receipt is a row whose receipt
    /// could report a field its body never produced — and the receipt is the
    /// matrix's evidence, so a receipt assembled wrongly is worse than none.
    fn of(
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
/// A short sleep rather than a spin: a spin would burn a core and change the
/// timings the same process reports, and a row that measures while it waits is a
/// row measuring its own harness.
async fn drain_to(supervisor: &mut Supervisor, total: usize) -> lgwks_bot::rt::supervise::Stats {
    let target = u64::try_from(total).unwrap_or(u64::MAX);
    while supervisor.stats().completed < target {
        if supervisor.reap() == 0 {
            tokio::time::sleep(std::time::Duration::from_micros(50)).await;
        }
    }
    supervisor.stats()
}

/// Drain until `expected` tasks have ended **and** nothing is left in flight.
///
/// The second condition is what distinguishes this from [`drain_to`]: a run
/// whose bodies are cancelled can reach its completion count and still hold
/// permits for tasks nobody has reaped. A drain that stopped at the count would
/// report a clean run over a supervisor that has leaked work.
async fn drain_until_idle(
    supervisor: &mut Supervisor,
    expected: usize,
) -> lgwks_bot::rt::supervise::Stats {
    let target = u64::try_from(expected).unwrap_or(u64::MAX);
    loop {
        supervisor.reap();
        let stats = supervisor.stats();
        let ended = stats
            .succeeded
            .saturating_add(stats.cancelled)
            .saturating_add(stats.aborted);
        if ended >= target && stats.in_flight() == 0 {
            return stats;
        }
        tokio::time::sleep(std::time::Duration::from_micros(50)).await;
    }
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
    if receipt.work_units != u64::try_from(BODIES).unwrap_or(u64::MAX) {
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
    let expected = u64::try_from(TASKS).unwrap_or(u64::MAX);
    if seen != expected {
        let refusal = Err(format!(
            "the slow consumer received {seen} results, not {expected}: fan-out lost or \
         duplicated work under backpressure"
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "row_high_fanout_slow_consumer: returning an error to the caller");
        return refusal;
    }
    if bytes
        != usize::try_from(expected)
            .unwrap_or(usize::MAX)
            .saturating_mul(4_096)
    {
        let refusal = Err(format!(
            "the consumer buffered {bytes} bytes, not {}: the payload was corrupted in flight",
            usize::try_from(expected)
                .unwrap_or(usize::MAX)
                .saturating_mul(4_096)
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
    while entered_count < u64::try_from(BOUND).unwrap_or(u64::MAX) {
        entered_count = entered.load(Ordering::SeqCst);
        if entered_count < u64::try_from(BOUND).unwrap_or(u64::MAX) {
            tokio::time::sleep(std::time::Duration::from_micros(50)).await;
        }
    }
    // Every permit is now held by a body parked on its own token. The ceiling is
    // saturated by construction, not by timing.
    if entered.load(Ordering::SeqCst) != u64::try_from(BOUND).unwrap_or(u64::MAX) {
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
    let stats = drain_until_idle(&mut supervisor, BOUND).await;
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
    let target = u64::try_from(PER_TENANT).unwrap_or(u64::MAX);
    while acme.stats().completed < target || globex.stats().completed < target {
        acme.reap();
        globex.reap();
        tokio::time::sleep(std::time::Duration::from_micros(50)).await;
    }
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
    if receipt.work_units != u64::try_from(total).unwrap_or(u64::MAX) {
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
        ActionDigest, ActionId, AttemptId, EffectKey, EnvironmentEpoch, EnvironmentId,
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
    Ok(EffectKey::new(
        run,
        action,
        attempt,
        flow,
        digest,
        environment,
        epoch,
    ))
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
        placed: u64::try_from(recovered).unwrap_or(u64::MAX),
        completed: u64::try_from(recovered_attempts).unwrap_or(u64::MAX),
        cancelled: 0,
        aborted: 0,
        work_units: u64::try_from(recovered).unwrap_or(u64::MAX),
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
fn workload_matrix(
    runtime: &Runtime,
    dir: &std::path::Path,
    json: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("§3 workload matrix — every row lgwks_bot can drive today\n");

    let mut receipts = Vec::new();
    let mut failures = Vec::new();
    let mut run = |name: &str, outcome: Result<Receipt, String>| match outcome {
        Ok(receipt) => {
            println!(
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
            println!("  {name:<28} FAILED: {reason}");
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

    println!();
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
        println!("wrote {path}");
    }
    if !failures.is_empty() {
        let refusal = Err(format!(
            "{} workload-matrix row(s) failed: {}",
            failures.len(),
            failures.join("; ")
        )
        .into());
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "workload_matrix: returning an error to the caller");
        return refusal;
    }
    println!(
        "{} rows, every receipt above was produced by running the row",
        receipts.len()
    );
    Ok(())
}

// ── The open-loop driver ─────────────────────────────────────────────────────

/// Which admission door an arrival is offered through.
///
/// The two doors are the facade's own, and they fail differently, which is why a
/// saturation curve needs both: `spawn` awaits a slot, so past the knee the
/// caller's own loop becomes the queue and the backlog shows up as latency measured
/// from the intended start; `try_spawn` refuses at the bound, so past the knee the
/// arrival is dropped and counted and memory does not grow.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Admission {
    /// Await a free slot: the caller's own loop is the queue.
    Backpressure,
    /// Refuse at the bound and count the refusal.
    Refuse,
}

impl Admission {
    /// The name the results file carries.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Backpressure => "backpressure",
            Self::Refuse => "refuse",
        }
    }
}

/// Which side of the comparison an open-loop run drove.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    /// `lgwks_bot::rt::supervise::Supervisor`.
    Facade,
    /// Pinned raw Tokio: `JoinSet` + `Semaphore` + a `watch`-based token.
    Baseline,
}

impl Side {
    /// The name the results file carries.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Facade => "facade",
            Self::Baseline => "baseline",
        }
    }

    /// Every side, in the order a paired run drives them.
    const ALL: [Side; 2] = [Side::Facade, Side::Baseline];
}

/// The floor of the p99 budget a knee is declared against.
///
/// A declared number rather than a discovered one, because "the knee" is only meaningful
/// against a service level: the same curve read against a 10 ms budget and a 1 s budget has
/// two different knees. 50 ms is the estate's standing budget for a request-shaped
/// interaction, and it is named on every table that uses it, so a reader can re-declare the
/// knee against another budget from the same rows.
///
/// It is a **floor**, not the whole rule: [`SideRun::p99_budget_nanos`] also allows one
/// further body's worth of queueing, because the sweep scales the body cost per bound and a
/// 50 ms ceiling would otherwise be smaller than one service time at the wide bounds.
const SLO_P99_NANOS: u64 = 50_000_000;

/// Extra service times of queueing the p99 budget allows on top of the floor.
///
/// One, and it is the standard queueing reading: below the knee an arrival waits less than
/// one body's time, above it the wait grows without bound. Stated as a multiple rather than
/// folded into the floor because it has to be re-readable at a different body cost — the
/// whole point of scaling the body per bound is that a budget in milliseconds alone cannot
/// mean the same thing at a 3.2 ms body and a 6.55 s one.
const BUDGET_EXTRA_BODIES: u64 = 1;

/// Queue-depth samples kept per open-loop run.
///
/// Bounded, because a run is a measurement and a measurement's own storage is a cost
/// it must not impose on what it measures. 65,536 samples covers a 65-second run at
/// one sample per millisecond; samples past the cap are counted, not kept.
const DEPTH_SAMPLE_CAP: usize = 65_536;

/// One open-loop run: an offered rate, a window, and a ceiling.
#[derive(Clone, Copy, Debug)]
struct OpenLoopSpec {
    /// How long each body takes, in microseconds, on top of its work unit.
    ///
    /// A declared parameter rather than an accident of the workload, because the knee is
    /// a joint property of the offered rate *and* the body's cost, and a curve that only
    /// varies one of them measures the other by accident. At zero the body is a counter
    /// bump and a yield — about a microsecond — and a ceiling of 64 then admits roughly
    /// 64 million arrivals a second, which an in-process generator cannot offer, so the
    /// ceiling never binds and the "refuse" door never refuses. Any figure about refusal,
    /// drain or recovery is therefore meaningless unless the body is declared.
    body_micros: u64,
    /// Arrivals offered per second.
    offered_rate: u64,
    /// How long arrivals are offered for.
    ///
    /// A ceiling on the run, never the run's definition: the *arrival count* below is.
    /// Offering "for one second" lets a faster side place more arrivals than a slower
    /// one, and the difference would be a property of the clock rather than of the
    /// engine — which is exactly what the fairness gate exists to refuse, and it did
    /// refuse it, naming `placed`. So a run offers the arrivals the rate and the window
    /// define and stops on the count, and the window only bounds how long a slow side may
    /// take to offer them.
    window: Duration,
    /// Arrivals to offer, or `None` to offer until the window closes.
    arrivals: Option<u64>,
    /// The in-flight ceiling.
    bound: usize,
    /// Report every `drop_every`-th arrival as placed without placing it.
    ///
    /// Zero on every honest run. This is the negative control the conservation gate
    /// has to catch: a driver that counts an admission it never made reports a
    /// latency for work that never ran, the same class of defect `--mutant-check`
    /// guards for the closed-loop rig.
    drop_every: u64,
}

/// A bounded queue-depth trace, plus the two facts a reader needs from it.
#[derive(Debug, Default)]
struct DepthTrace {
    samples: Vec<u32>,
    dropped: u64,
    peak: usize,
}

impl DepthTrace {
    /// A trace with room for a first window's worth of samples.
    fn new() -> Self {
        Self {
            samples: Vec::with_capacity(4_096),
            dropped: 0,
            peak: 0,
        }
    }

    /// Record one queue depth, in arrivals.
    fn push(&mut self, depth: usize) {
        self.peak = self.peak.max(depth);
        let depth = u32::try_from(depth).unwrap_or(u32::MAX);
        if self.samples.len() < DEPTH_SAMPLE_CAP {
            self.samples.push(depth);
        } else {
            self.dropped = self.dropped.saturating_add(1);
        }
    }

    /// Mean depth over the samples kept.
    fn mean(&self) -> f64 {
        if self.samples.is_empty() {
            return 0.0;
        }
        let total: u64 = self.samples.iter().map(|depth| u64::from(*depth)).sum();
        (total as f64) / (self.samples.len() as f64)
    }
}

/// The four operations an open-loop run needs from an engine.
///
/// This is why the generator exists once rather than twice. A comparison between a
/// facade and the engine it wraps is only a comparison if everything except the
/// engine surface is *the same code*: two driver loops, however carefully matched,
/// are two programs, and a difference between them is indistinguishable from a
/// difference between the engines. So the generator, the latency accounting, the
/// queue sampling and the conservation gate are one piece of code parameterised by
/// these calls, and an engine contributes only what is genuinely its own.
trait Engine {
    /// Offer one arrival at the ceiling, reporting whether it was admitted.
    ///
    /// Blocks on the backpressure door and refuses on the other, which is the whole
    /// difference between the two overload behaviours.
    fn admit(&mut self, arrival: Arrival) -> impl Future<Output = bool>;

    /// Join whatever has finished, without waiting.
    fn reap(&mut self);

    /// Tasks in flight at this instant.
    fn in_flight(&self) -> u64;

    /// Join everything, blocking until nothing is left.
    fn drain(&mut self) -> impl Future<Output = ()>;

    /// Terminal counters after the drain: completed, cancelled, aborted.
    ///
    /// The engine's own counters, never a second tally this rig maintains beside
    /// them: a comparison that reads its numbers from a different place than the
    /// engine writes them is the second-ledger defect INV-BOT-31 exists to refuse.
    fn terminals(&self) -> (u64, u64, u64);
}

/// The facade, as an [`Engine`].
///
/// Every call is the facade's own door: `spawn` awaits a permit, `try_spawn` refuses
/// at the bound and counts the refusal, `reap` joins what finished, and the terminal
/// counters are `Supervisor::stats`. Nothing here re-implements any of it.
struct FacadeEngine {
    supervisor: Supervisor,
    admission: Admission,
}

impl FacadeEngine {
    /// A facade engine with a ceiling of `bound` at the given door.
    fn new(bound: usize, admission: Admission) -> Self {
        Self {
            supervisor: Supervisor::new(bound),
            admission,
        }
    }
}

impl Engine for FacadeEngine {
    fn admit(&mut self, arrival: Arrival) -> impl Future<Output = bool> {
        let admission = self.admission;
        let supervisor = &mut self.supervisor;
        async move {
            match admission {
                Admission::Backpressure => {
                    supervisor.spawn(move |_token| recorded_body_unit(arrival)).await;
                    true
                }
                Admission::Refuse => {
                    let attempt = supervisor.try_spawn(move |_token| recorded_body_unit(arrival));
                    if attempt.is_err() {
                        lgwks_std::trace::debug!(error = ?attempt.as_ref().err(), "FacadeEngine: the ceiling refused an arrival");
                    }
                    attempt.is_ok()
                }
            }
        }
    }

    fn reap(&mut self) {
        // Reaping is what keeps the retained set at the live bound rather than the
        // lifetime total, and `Supervisor` performs it on its own call.
        while self.supervisor.reap() > 0 {}
    }

    fn in_flight(&self) -> u64 {
        self.supervisor.stats().in_flight()
    }

    fn drain(&mut self) -> impl Future<Output = ()> {
        // Naming the target before the block is what lets the loop read a plain local
        // instead of re-deriving the spawn count on every pass.
        let target = self.supervisor.stats().spawned;
        async move {
            while self.supervisor.stats().completed < target {
                if self.supervisor.reap() == 0 {
                    tokio::time::sleep(Duration::from_micros(200)).await;
                }
            }
        }
    }

    fn terminals(&self) -> (u64, u64, u64) {
        let stats = self.supervisor.stats();
        (stats.succeeded, stats.cancelled, stats.aborted)
    }
}

/// Pinned raw Tokio, as an [`Engine`].
///
/// `JoinSet` for task-set lifetime, `Semaphore` for the ceiling the facade enforces,
/// and an explicit `try_acquire` before each spawn so a refusal at the bound is
/// counted rather than silently queued — the same refusal semantics the facade has.
/// Every `JoinError` is read, not discarded, so the abort/panic split survives
/// exactly as the facade preserves it.
struct BaselineEngine {
    set: JoinSet<()>,
    permits: Arc<Semaphore>,
    token: tokio_util_cancel::Token,
    admission: Admission,
    completed: u64,
    aborted: u64,
}

impl BaselineEngine {
    /// A baseline engine with a ceiling of `bound` at the given door.
    fn new(bound: usize, admission: Admission) -> Self {
        Self {
            set: JoinSet::new(),
            permits: Arc::new(Semaphore::new(bound)),
            token: tokio_util_cancel::token(),
            admission,
            completed: 0,
            aborted: 0,
        }
    }

    /// Spawn one admitted body, holding its permit for the body's whole life.
    fn spawn_body(&mut self, permit: tokio::sync::OwnedSemaphorePermit, arrival: Arrival) {
        let child = self.token.child();
        self.set.spawn(async move {
            let _held = permit;
            recorded_body_unit(arrival).await;
            let _ = child;
        });
    }

    /// Count one join result.
    ///
    /// `JoinError` is read rather than discarded: an aborted task and a panicked one
    /// are different facts, and the facade keeps them apart, so a baseline that
    /// collapsed them would be doing less work for a better number.
    fn count(&mut self, joined: Result<(), tokio::task::JoinError>) {
        match joined {
            Ok(()) => self.completed = self.completed.saturating_add(1),
            Err(_) => self.aborted = self.aborted.saturating_add(1),
        }
    }
}

impl Engine for BaselineEngine {
    fn admit(&mut self, arrival: Arrival) -> impl Future<Output = bool> {
        let admission = self.admission;
        let permits = Arc::clone(&self.permits);
        async move {
            let permit = match admission {
                Admission::Backpressure => permits.acquire_owned().await.ok(),
                Admission::Refuse => permits.try_acquire_owned().ok(),
            };
            let Some(permit) = permit else {
                return false;
            };
            self.spawn_body(permit, arrival);
            true
        }
    }

    fn reap(&mut self) {
        while let Some(joined) = self.set.try_join_next() {
            self.count(joined);
        }
    }

    fn in_flight(&self) -> u64 {
        // `JoinSet::len` is the set's own count, which is the same fact as "tasks
        // placed and not yet joined" — read from the engine rather than from a
        // second counter this rig would then have to keep in step with it.
        u64::try_from(self.set.len()).unwrap_or(u64::MAX)
    }

    /// An explicit block rather than an `async fn`: the trait declares
    /// `-> impl Future`, the only shape that keeps the `&mut self` borrow across the
    /// await without boxing the future.
    #[expect(
        clippy::manual_async_fn,
        reason = "the trait's RPITIT signature keeps the `&mut self` borrow, which an                   `async fn` in the impl would have to box to match"
    )]
    fn drain(&mut self) -> impl Future<Output = ()> {
        async move {
            while let Some(joined) = self.set.join_next().await {
                self.count(joined);
            }
        }
    }

    fn terminals(&self) -> (u64, u64, u64) {
        // Nothing cancels a baseline run, so `cancelled` is structurally zero. It is
        // returned rather than omitted so both sides' reports carry the same fields
        // and a reader does not have to remember which is which.
        (self.completed, 0, self.aborted)
    }
}

/// Wait until `intended`, unless it has already passed.
///
/// A generator that sleeps unconditionally pays a timer round-trip per arrival, and
/// at a million arrivals per second that timer *is* the offered rate. Past saturation
/// the generator is supposed to fall behind, and falling behind means offering as fast
/// as it can — so the wait is skipped exactly when it is already late, and the lag is
/// charged to the latency instead.
async fn wait_until(intended: Instant) {
    let now = Instant::now();
    if now < intended {
        tokio::time::sleep_until(tokio::time::Instant::from_std(intended)).await;
    }
}

/// Sample the queue after one arrival and return the generator's lag.
///
/// The queue depth is "bodies running now, plus arrivals that are due but not yet
/// running". The second term is the generator's own lag divided by the arrival period,
/// so it reads as an arrival count rather than as a duration, and it is what a
/// backpressure door accumulates when it falls behind: the caller's own loop *is* the
/// queue, and this is its depth.
fn sample_queue(
    now: Instant,
    next_intended: Instant,
    period: u64,
    in_flight: u64,
    depth: &mut DepthTrace,
) -> Duration {
    let lag = now.saturating_duration_since(next_intended);
    let waiting = u64::try_from(lag.as_nanos() / u128::from(period.max(1))).unwrap_or(0);
    depth.push(usize::try_from(waiting.saturating_add(in_flight)).unwrap_or(usize::MAX));
    lag
}

/// Record one arrival's admission outcome.
fn count_arrival(placed: bool, admitted: &mut u64, refused: &mut u64) {
    if placed {
        *admitted = admitted.saturating_add(1);
    } else {
        *refused = refused.saturating_add(1);
    }
}

/// Nanoseconds from `intended` to now.
///
/// A `u128` from `elapsed()` into a `u64` cannot overflow in practice — a stalled run
/// of 584 years would — and the recorder clamps past its own ceiling, so the
/// conversion is stated rather than assumed.
fn latency_nanos(intended: Instant) -> u64 {
    u64::try_from(intended.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// The intended instant for arrival `index` of a schedule that began at `t0`.
fn intended_at(t0: Instant, index: u64, period: u64) -> Instant {
    t0 + intended_arrival(index, period, 0)
}

/// One arrival's identity: what its body runs on, and where its latency starts.
///
/// Bundled rather than threaded as four parameters, because the same four travelled
/// through the [`Engine`] trait, both of its implementations, the baseline's spawn helper
/// and the generator — six signatures that were each an identical list of four names.
/// Six copies of a list is six chances for the two engines to end up with two different
/// definitions of what an arrival is, and the whole comparison rests on them sharing one.
#[derive(Clone, Debug)]
struct Arrival {
    /// The counter this body's work unit bumps.
    counter: Arc<AtomicU64>,
    /// The recorder shard this body's one latency lands in.
    ///
    /// Taken once, here, and moved into the body — so the shard a body's single sample
    /// lands in cannot differ between two samples of the same body, which is what makes
    /// the merge across shards an addition rather than a puzzle.
    shard: Arc<Recorder>,
    /// The instant this arrival was **intended** to start, not the instant it was placed.
    intended: Instant,
    /// This body's own service cost, in microseconds, on top of its work unit.
    body_micros: u64,
}

impl Arrival {
    /// Mint an arrival intended for `intended`, recording into `shard`.
    fn mint(
        work: &Arc<AtomicU64>,
        recorders: &RecorderSet,
        intended: Instant,
        body_micros: u64,
    ) -> Self {
        Self {
            counter: Arc::clone(work),
            shard: recorders.take(),
            intended,
            body_micros,
        }
    }

    /// Destructure into the counter, the shard and the intended instant.
    ///
    /// One destructuring for every caller that has to add its own handle to an arrival
    /// (the in-flight tiers' gate), so the three names are named once rather than matched
    /// by three separate patterns that could drift apart.
    fn into_parts(self) -> (Arc<AtomicU64>, Arc<Recorder>, Instant) {
        (self.counter, self.shard, self.intended)
    }
}

/// The body an open-loop arrival runs: the same work unit every side runs, then its
/// latency from the **intended** start.
///
/// Measuring from the intended instant rather than from the body's own first poll is
/// the whole coordinated-omission correction: a generator that fell behind reports the
/// queue it created rather than the service it managed to reach.
async fn recorded_body_unit(arrival: Arrival) {
    let Arrival {
        counter,
        shard,
        intended,
        body_micros,
    } = arrival;
    body_unit(counter).await;
    if body_micros > 0 {
        tokio::time::sleep(Duration::from_micros(body_micros)).await;
    }
    shard.record(latency_nanos(intended));
}

/// What one side did under one offered rate.
#[derive(Debug)]
struct SideRun {
    side: Side,
    bound: usize,
    offered_rate: u64,
    /// This run's declared service cost per body, in microseconds.
    ///
    /// Carried on the run rather than passed to the reporter because the capacity a
    /// bound's knee is read against is `bound / body_micros`: a record that printed
    /// p99s without the body cost would leave a reader unable to tell which curve the
    /// latency belonged to.
    body_micros: u64,
    admission: Admission,
    /// Arrivals the generator offered.
    offered: u64,
    /// Arrivals admitted to a slot.
    admitted: u64,
    /// Arrivals the ceiling refused.
    refused: u64,
    /// Arrivals whose body ran to completion.
    completed: u64,
    /// Arrivals that ended by observing cancellation.
    cancelled: u64,
    /// Arrivals the runtime dropped.
    aborted: u64,
    /// Work units the bodies recorded.
    work_units: u64,
    /// Tasks still in flight after the drain.
    in_flight_at_end: u64,
    /// The queue-depth trace; its peak is the peak queue.
    depth: DepthTrace,
    /// The generator's worst lag behind its own schedule.
    max_lag: Duration,
    /// Latencies from the intended start.
    histogram: Histogram,
    /// Wall time over which arrivals were offered.
    wall: Duration,
    /// Time spent draining after the window closed.
    drain: Duration,
    /// Resident set size sampled at the run's peak.
    rss_bytes: Option<u64>,
    /// How the RSS figure was read.
    rss_source: &'static str,
}

impl SideRun {
    /// Fraction of offered arrivals the ceiling refused.
    fn refusal_rate(&self) -> f64 {
        if self.offered == 0 {
            return 0.0;
        }
        (self.refused as f64) / (self.offered as f64)
    }

    /// Completions per second over the window and the drain together.
    fn throughput_per_second(&self) -> f64 {
        let seconds = self.wall.as_secs_f64() + self.drain.as_secs_f64();
        if seconds <= 0.0 {
            return 0.0;
        }
        (self.completed as f64) / seconds
    }

    /// The p99 budget this run's knee is read against, in nanoseconds.
    ///
    /// `max(SLO_P99_NANOS, (1 + BUDGET_EXTRA_BODIES) x body)`. Both halves are named on
    /// every run that prints one, because a knee declared against a budget the reader cannot
    /// see is a knee against an unpublished rule.
    fn p99_budget_nanos(&self) -> u64 {
        let by_bodies = self
            .body_micros
            .saturating_mul(1_000)
            .saturating_mul(1 + BUDGET_EXTRA_BODIES);
        SLO_P99_NANOS.max(by_bodies)
    }

    /// Whether this run refused nothing and stayed inside its declared p99 budget.
    ///
    /// One expression for both doors, and the refusal door is the interesting half: its
    /// declared overflow past the knee *is* the answer, so what bounds the knee there is the
    /// rate at which the ceiling first refuses rather than a queueing blowup. A run that
    /// refused anything is out of budget on either door, because the refusal is the last
    /// rung at which the engine was still inside its own declared envelope.
    fn within_slo(&self) -> bool {
        self.refused == 0 && self.histogram.quantile_nanos(0.99) <= self.p99_budget_nanos()
    }
}

/// The resident set size of this process, and how it was read.
///
/// Linux reads its own `/proc/self/status`, which is a file the process already has
/// and a high-water mark rather than a sample. macOS has no in-process equivalent
/// without a `getrusage` edge, so the reading is a `ps` sample of the process's
/// **current** RSS, taken at the moment the run is at its peak. The two are different
/// measurements and each is labelled: a peak figure reported under the name of the
/// other is a false claim, and an unavailable one reported as a small one is worse
/// than an absent one.
fn resident_bytes() -> (Option<u64>, &'static str) {
    #[cfg(target_os = "linux")]
    {
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            for line in status.lines() {
                if let Some(value) = line.strip_prefix("VmHWM:")
                    && let Some(kib) = value.split_whitespace().next()
                    && let Ok(kib) = kib.parse::<u64>()
                {
                    return (
                        Some(kib.saturating_mul(1024)),
                        "/proc/self/status VmHWM (peak)",
                    );
                }
            }
        }
        (None, "/proc/self/status VmHWM unreadable")
    }
    #[cfg(not(target_os = "linux"))]
    {
        // `ps` rather than `getrusage`: the first is a system utility this instrument
        // may run, the second is a C edge no crate here may author.
        let sample = std::process::Command::new("/bin/ps")
            .args(["-o", "rss=", "-p"])
            .arg(std::process::id().to_string())
            .output();
        let read = match sample {
            Ok(output) => String::from_utf8_lossy(&output.stdout)
                .split_whitespace()
                .next()
                .and_then(|kib| kib.parse::<u64>().ok())
                .map(|kib| kib.saturating_mul(1024)),
            Err(_) => None,
        };
        match read {
            Some(bytes) => (Some(bytes), "/bin/ps -o rss= (current, at peak)"),
            None => (None, "/bin/ps -o rss= unreadable"),
        }
    }
}

/// Offer `spec.offered_rate` for `spec.window` to one engine, and report what it did.
///
/// The generator is one loop: join what has finished, sleep to the arrival's intended
/// instant, offer it through the engine's door, then sample the queue depth. The
/// latency is measured inside the body from the **intended** instant, which is what
/// makes the p99 comparable across offered rates and across engines.
async fn open_loop_run<E: Engine>(
    engine: &mut E,
    side: Side,
    admission: Admission,
    spec: &OpenLoopSpec,
) -> Result<SideRun, String> {
    let work = Arc::new(AtomicU64::new(0));
    let recorders = RecorderSet::new();
    let period = period_nanos(spec.offered_rate);
    let t0 = Instant::now();
    let mut offered = 0_u64;
    let mut admitted = 0_u64;
    let mut refused = 0_u64;
    let mut depth = DepthTrace::new();
    let mut max_lag = Duration::ZERO;

    loop {
        if let Some(limit) = spec.arrivals
            && offered >= limit
        {
            break;
        }
        if spec.arrivals.is_none() && t0.elapsed() >= spec.window {
            break;
        }
        engine.reap();
        let intended = intended_at(t0, offered, period);
        wait_until(intended).await;
        offered = offered.saturating_add(1);
        // The negative control's decision, made once per arrival so an honest run
        // pays one predictable branch.
        let place = spec.drop_every == 0 || !offered.is_multiple_of(spec.drop_every);
        let placed = if place {
            engine
                .admit(Arrival::mint(
                    &work,
                    &recorders,
                    intended,
                    spec.body_micros,
                ))
                .await
        } else {
            // The control's defect in one line: the arrival is counted as offered and
            // reported as admitted, and nothing runs.
            true
        };
        count_arrival(placed, &mut admitted, &mut refused);
        let next_intended = intended_at(t0, offered, period);
        max_lag = max_lag.max(sample_queue(
            Instant::now(),
            next_intended,
            period,
            engine.in_flight(),
            &mut depth,
        ));
    }

    let offered_at_end = Instant::now();
    let (rss_bytes, rss_source) = resident_bytes();
    let drain_started = Instant::now();
    engine.drain().await;
    let drain = drain_started.elapsed();
    let (completed, cancelled, aborted) = engine.terminals();
    Ok(SideRun {
        side,
        bound: spec.bound,
        offered_rate: spec.offered_rate,
        body_micros: spec.body_micros,
        admission,
        offered,
        admitted,
        refused,
        completed,
        cancelled,
        aborted,
        work_units: work.load(Ordering::SeqCst),
        in_flight_at_end: engine.in_flight(),
        depth,
        max_lag,
        histogram: Recorder::merged(recorders.shards()),
        wall: offered_at_end.duration_since(t0),
        drain,
        rss_bytes,
        rss_source,
    })
}

/// Build the engine for one side at one door and run the generator against it.
async fn open_loop_side(
    side: Side,
    admission: Admission,
    spec: &OpenLoopSpec,
) -> Result<SideRun, String> {
    match side {
        Side::Facade => {
            let mut engine = FacadeEngine::new(spec.bound, admission);
            open_loop_run(&mut engine, side, admission, spec).await
        }
        Side::Baseline => {
            let mut engine = BaselineEngine::new(spec.bound, admission);
            open_loop_run(&mut engine, side, admission, spec).await
        }
    }
}

/// The one accounting check both open-loop gates share: every completed body recorded
/// exactly its own work unit.
///
/// The same rule at both levels — an open-loop run and an in-flight tier — because it
/// is the same rule. Two copies of it is one place where the two gates can disagree
/// about what "the bodies and the accounting agree" means, and a gate that disagrees
/// with itself is a gate whose verdict depends on which level asked.
fn work_units_match(
    label: &str,
    completed: u64,
    work_units: u64,
    noun: &str,
) -> Result<(), String> {
    if work_units == completed {
        return Ok(());
    }
    Err(format!(
        "{label}: {work_units} work units for {completed} completed {noun} — the bodies and \
         the accounting disagree about how much ran"
    ))
}

/// The conservation gate: what every open-loop run must satisfy, whatever it measured.
///
/// This is the fairness gate's job in a regime where the two sides are *expected* to
/// diverge: past the knee one side admits what the other refuses, and that difference
/// is the result. What may never differ is whether a side did the work it reported —
/// every arrival offered was admitted or refused, every admitted body completed, every
/// completed body recorded its work, and nothing was left in flight. A percentile
/// computed on a run that violates one of these is a number about a different program,
/// so the run aborts and names the field.
fn open_loop_conserved(label: &str, run: &SideRun) -> Result<(), String> {
    let accounted = run.admitted.saturating_add(run.refused);
    if accounted != run.offered {
        return Err(format!(
            "{label}: offered {}, admitted {} plus refused {} is {accounted} — an arrival was \
             lost or counted twice",
            run.offered, run.admitted, run.refused
        ));
    }
    if run.completed != run.admitted {
        return Err(format!(
            "{label}: {} of {} admitted bodies completed — work was lost or duplicated",
            run.completed, run.admitted
        ));
    }
    let terminal = run
        .completed
        .saturating_add(run.cancelled)
        .saturating_add(run.aborted);
    if terminal != run.admitted {
        return Err(format!(
            "{label}: {terminal} of {} admitted arrivals reached a terminal state",
            run.admitted
        ));
    }
    work_units_match(label, run.completed, run.work_units, "bodies")?;
    if run.in_flight_at_end != 0 {
        return Err(format!(
            "{label}: {} tasks still in flight after the drain",
            run.in_flight_at_end
        ));
    }
    Ok(())
}

/// The cross-side gate for a pair that admitted everything it was offered.
///
/// Below both knees the two sides did the same work, so the closed-loop gate applies in
/// full. Past a knee the sides legitimately differ — one refused what the other
/// admitted — and refusing the run for that would refuse the measurement, so the pair
/// is reported as a saturation point, with each side's own conservation gate already
/// enforced.
///
/// Returns whether the pair was gated as equal work, so a reader can tell a gated pair
/// from a saturation point without re-deriving it.
fn open_loop_pair_fair(label: &str, facade: &SideRun, baseline: &SideRun) -> Result<bool, String> {
    if facade.refused != 0 || baseline.refused != 0 {
        return Ok(false);
    }
    let tally = |run: &SideRun| Tally {
        placed: run.admitted,
        completed: run.completed,
        cancelled: run.cancelled,
        aborted: run.aborted,
        refused: run.refused,
        retained: 0,
        dropped_detail: 0,
        work_units: run.work_units,
    };
    fair(&tally(facade), &tally(baseline)).map_err(|reason| format!("{label}: {reason}"))?;
    Ok(true)
}

/// Print one row of a table.
///
/// One printer for every table in this file: a column width chosen once, so the tables
/// cannot drift apart, and a cell's padding decided in one place rather than by each
/// table's own format string.
fn print_row(cells: &[String]) {
    println!("  {}", cells.join("  "));
}

/// Print a table's column names.
fn print_columns(columns: &[&str]) {
    print_row(
        &columns
            .iter()
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>(),
    );
}

/// A latency in nanoseconds, rendered in microseconds to one decimal.
///
/// One renderer for every latency this file prints, so no table reports the same
/// measurement in two scales and lets a reader compare them by eye.
fn micros(nanos: u64) -> String {
    format!("{:.1}", nanos as f64 / 1_000.0)
}

// ── The saturation sweep ─────────────────────────────────────────────────────

/// The in-flight bounds the saturation sweep runs at.
///
/// Six, because the issue's sweep list (`64`, `1,024`, `16,384`, `131,072`) and the
/// delivery brief's list (`64`, `1,024`, `10,000`, `100,000`) name different decades
/// and neither is a subset of the other. Both are here rather than one chosen for the
/// reader's convenience, because a sweep that quietly drops a tier reads as coverage
/// of the tiers it kept.
const SWEEP_BOUNDS: [usize; 6] = [64, 1_024, 10_000, 16_384, 100_000, 131_072];

/// The offered-rate ladder, as multiples of a quarter of the ceiling's capacity.
///
/// Expressed against **capacity** rather than against the bound, because the knee is
/// where the offered rate reaches what the ceiling can sustain, and a ladder of
/// `bound x k` walks straight past that point at a wide bound and never reaches it at a
/// narrow one. Capacity is `bound / body_seconds`, so a ceiling of 64 admitting a 3.2 ms
/// body sustains 20,000 arrivals a second and its ladder is 5,000 / 20,000 / 80,000 /
/// 320,000 — the knee at 20,000 is the second rung rather than beyond the ladder's end.
const SWEEP_MULTIPLIERS: [u64; 4] = [1, 4, 16, 64];

/// The declared capacity every swept ceiling is scaled to, in arrivals per second.
///
/// **One target for every bound, with the body cost derived per bound**, because a single
/// constant body cost cannot produce a curve at more than one ceiling width. A 5 ms body
/// puts a ceiling of 64 at 12,800 arrivals a second and a ceiling of 131,072 at 26
/// million; an in-process generator offers about 450,000 a second on this host, so at the
/// wide ceiling every rung above the first measures the *generator's* backlog — its own
/// `max lag` reached 3.4 s against a ceiling that had admitted every arrival immediately —
/// and the p99 on those rows is a property of the instrument rather than of the runtime.
/// Scaling the body instead (`bound * 1e6 / target`) puts every ceiling under exactly this
/// load at the same four offered rates, so the knee is comparable across bounds and is a
/// property of the engine.
///
/// 20,000 arrivals a second is a declared figure and is printed on every run: it is well
/// inside the generator's own placement ceiling (measured at 450,000–700,000 a second on
/// the reference host, and printed beside it as `achieved/s`), so a knee declared against
/// it is a knee of the engine rather than of the loop that offers the work.
const DEFAULT_CAPACITY_TARGET: u64 = 20_000;

/// The floor this generator was measured placing arrivals at, in arrivals per second.
///
/// A measured number, not a specification of this host: a calibration sweep on an Apple
/// M5 Pro with 15 cores placed 449,482 arrivals a second on the facade and 701,013 on the
/// baseline at a bound wide enough (100,000) that the ceiling was never the limit, and the
/// lower of the two is what a paired point can rely on. It is rounded down so a run whose
/// own `achieved/s` sits above it is not contradicted, and every sweep row prints its
/// achieved rate beside the offered one so a reader sees the margin rather than taking this
/// line's word for it.
///
/// A host that places slower than this gets a smaller usable range, and the rig says so
/// rather than quietly reporting the generator's backlog as the ceiling's knee: the
/// `max lag ms` column is the tell, and a row whose lag is comparable to its p99 is
/// measuring the generator.
const GENERATOR_PLACEMENT_CEILING: u64 = 400_000;

/// The service cost every body runs at one bound, in microseconds.
///
/// `bound * 1e6 / capacity_target` when no override is given, which is the scaling
/// [`DEFAULT_CAPACITY_TARGET`] exists for. An override pins the cost at every bound
/// instead, which is the other question a reader may want answered — "at a fixed 5 ms
/// service time, where does each ceiling's knee sit" — and the two are reported separately
/// because they answer different questions and neither answers the other.
fn body_for_bound(bound: usize, capacity_target: u64, override_micros: u64) -> u64 {
    if override_micros > 0 {
        return override_micros;
    }
    let permits = u64::try_from(bound).unwrap_or(u64::MAX);
    // Rounded up, so a ceiling is never declared to sustain more than the target by
    // truncation: `63 * 1e6 / 20_000` is 3,150 us exactly and `bound * 50` is 3,150 us,
    // while a bound the division rounds down would silently get a body cheaper than the
    // target asks for.
    permits.saturating_mul(1_000_000).div_ceil(capacity_target.max(1)).max(1)
}

/// The highest offered rate this rig will generate, in arrivals per second.
///
/// A cap on the *generator*, not on either engine, and declared because it is what
/// ends the ladder at a wide ceiling: an in-process generator places arrivals one at a
/// time through an engine's own spawn door, so a ceiling of 131,072 admitting a 5 ms body
/// could sustain 26 million a second and this rig cannot reach it. A ladder whose top
/// rung was clamped by the generator is reported as clamped, because a reader who read
/// the top row as the knee would be reading the instrument's ceiling as the runtime's.
const MAX_OFFERED_RATE: u64 = 2_000_000;

/// The offered rates this rig will actually drive at one bound, deduplicated.
///
/// [`SWEEP_MULTIPLIERS`] walked upward and every rung above [`MAX_OFFERED_RATE`] landed
/// on the same number, so a wide bound printed four identical rows and a reader counting
/// them would read four measurements where there was one. The ladder is deduplicated
/// instead, and [`ladder_clamped`] is what says whether the last rung was the
/// instrument's ceiling rather than the runtime's.
///
/// The deduplication is load-bearing at a zero body cost rather than cosmetic: at zero the
/// body is a counter bump and a yield — about a microsecond — so a ceiling of 64 declares
/// a capacity of 64 million arrivals a second and *every* rung of *every* ladder clamps to
/// [`MAX_OFFERED_RATE`]. Without it the sweep would print one rate six times per bound and
/// a reader would count six measurements where there was one.
fn sweep_ladder(bound: usize, body_micros: u64) -> Vec<u64> {
    let mut ladder: Vec<u64> = SWEEP_MULTIPLIERS
        .iter()
        .map(|rung| sweep_rate(bound, body_micros, *rung))
        .collect();
    // The rates rise with the rung, so every duplicate is adjacent and `dedup` removes
    // all of them rather than the runs of them.
    ladder.dedup();
    ladder
}

/// The arrival count one sweep point offers on each side.
///
/// A **count**, derived from the rate and the window, because the window alone let the
/// faster side place more arrivals than the slower one and the fairness gate refused
/// exactly that, naming `placed`: offering "for one second" makes the offered work a
/// property of each engine's speed. Both sides are therefore offered this many arrivals
/// and the difference between what they placed is a difference in the engines.
///
/// The window is floored at one second: a zero-second window would offer no arrivals at
/// all, which is a measurement of nothing rather than a short one.
fn sweep_arrivals(rate: u64, window: Duration) -> u64 {
    rate.saturating_mul(window.as_secs().max(1))
}

/// What one ceiling can sustain at one body cost, in arrivals per second.
///
/// Saturating throughout: a body of zero microseconds would claim an unbounded capacity,
/// and an unbounded figure is exactly the sort of number this rig exists not to publish.
fn capacity_per_second(bound: usize, body_micros: u64) -> u64 {
    let permits = u64::try_from(bound).unwrap_or(u64::MAX);
    permits.saturating_mul(1_000_000) / body_micros.max(1)
}

/// The offered rate for the `rung`-th multiple of a quarter of the ceiling's capacity.
fn sweep_rate(bound: usize, body_micros: u64, rung: u64) -> u64 {
    let capacity = capacity_per_second(bound, body_micros);
    capacity
        .saturating_mul(rung)
        .saturating_div(4)
        .clamp(1, MAX_OFFERED_RATE)
}

/// Whether the ladder's top rung was clamped by [`MAX_OFFERED_RATE`] rather than by the
/// ceiling, which is the fact a reader needs to read the knee table correctly.
fn ladder_clamped(bound: usize, body_micros: u64) -> bool {
    let declared = capacity_per_second(bound, body_micros)
        .saturating_mul(SWEEP_MULTIPLIERS[SWEEP_MULTIPLIERS.len() - 1])
        .saturating_div(4);
    declared > MAX_OFFERED_RATE
}

/// A rate rendered as a whole number of arrivals per second.
///
/// One renderer for the offered and the achieved columns, so the two are comparable by
/// eye — the pair is how a reader sees that an offered rate above this rig's own
/// placement ceiling was a rate the *generator* could not express.
fn rate_cell(per_second: f64) -> String {
    format!("{per_second:.0}")
}

/// The columns of the saturation table.
///
/// `offered/s` beside `achieved/s` is the pair that makes the table readable: past the
/// generator's own placement ceiling the two diverge, and the divergence is the fact a
/// reader needs before reading any latency on that row.
const SWEEP_COLUMNS: [&str; 12] = [
    "offered/s",
    "side",
    "offered",
    "admitted",
    "refused",
    "achieved/s",
    "p50 us",
    "p95 us",
    "p99 us",
    "peak queue",
    "mean queue",
    "max lag ms",
];

/// The columns of the knee table.
const KNEE_COLUMNS: [&str; 6] = [
    "bound",
    "side",
    "knee offered/s",
    "achieved/s",
    "p99 at knee",
    "refused at top",
];

/// Both sides' runs at one offered rate, gathered so the knee table can read either.
///
/// A `Vec` of two rather than a named pair, because the sweep walks both sides in the
/// same loop and the only thing that differs is the engine — so the ladder is a list of
/// points and each point holds the runs it measured.
struct SweepPair {
    bound: usize,
    offered_rate: u64,
    facade: SideRun,
    baseline: SideRun,
}

impl SweepPair {
    /// This pair's run on `side`.
    fn run(&self, side: Side) -> &SideRun {
        match side {
            Side::Facade => &self.facade,
            Side::Baseline => &self.baseline,
        }
    }
}

/// Measure one side at one offered rate and gate it, for the table and the record.
async fn measure_point(
    side: Side,
    admission: Admission,
    spec: &OpenLoopSpec,
) -> Result<SideRun, Box<dyn std::error::Error>> {
    let run = open_loop_side(side, admission, spec)
        .await
        .map_err(|reason| -> Box<dyn std::error::Error> { reason.into() })?;
    let label = format!(
        "bound {} at {}/s on the {}",
        spec.bound,
        spec.offered_rate,
        side.as_str()
    );
    open_loop_conserved(&label, &run)
        .map_err(|reason| -> Box<dyn std::error::Error> { reason.into() })?;
    Ok(run)
}

/// Print one sweep row.
fn print_sweep_row(run: &SideRun) {
    let (p50, p95, p99) = run.histogram.percentiles_nanos();
    print_row(&[
        run.offered_rate.to_string(),
        run.side.as_str().to_string(),
        run.offered.to_string(),
        run.admitted.to_string(),
        run.refused.to_string(),
        rate_cell(run.throughput_per_second()),
        micros(p50),
        micros(p95),
        micros(p99),
        run.depth.peak.to_string(),
        format!("{:.1}", run.depth.mean()),
        run.max_lag.as_millis().to_string(),
    ]);
}

/// Sweep offered rate past the knee at every bound, on one admission door.
///
/// For each bound the ladder is walked on both sides, paired within the point, and
/// gated. The knee for a row is the highest ladder rate that refused nothing and
/// stayed inside [`SLO_P99_NANOS`]: a rate past it either refused arrivals or blew the
/// budget, and either is a refusal rather than a served request.
async fn saturation_sweep(
    admission: Admission,
    window: Duration,
    capacity_target: u64,
    body_override: u64,
    workers: Option<usize>,
    json: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("saturation sweep — offered rate against p50/p95/p99 and the ceiling");
    println!(
        "door: {}   window: {} s per point   SLO: p99 <= {} ms",
        admission.as_str(),
        window.as_secs(),
        SLO_P99_NANOS / 1_000_000
    );
    if body_override > 0 {
        println!(
            "body cost pinned at {body_override} us at every bound, so each ceiling's declared \
             capacity is bound / body and the wide bounds are generator-capped by \
             construction; the capacity target {capacity_target}/s does not apply to this run"
        );
    } else {
        println!(
            "body cost derived per bound so every ceiling's declared capacity is \
             {capacity_target}/s, which is inside this generator's placement ceiling \
             (declared {}); a constant body cost would make the wide bounds measure the \
             generator",
            GENERATOR_PLACEMENT_CEILING
        );
    }
    println!("latency is measured from each arrival's INTENDED start\n");

    let mut points: Vec<SweepPair> = Vec::new();
    let mut json_rows: Vec<String> = Vec::new();
    let mut json_bodies: Vec<String> = Vec::new();

    for bound in SWEEP_BOUNDS {
        let body_micros = body_for_bound(bound, capacity_target, body_override);
        // One warm-up per side per bound, discarded: the first-touch page faults and
        // the runtime's worker start are one-time costs a measured point would
        // otherwise charge to the code under test.
        let warm = OpenLoopSpec {
            body_micros,
            offered_rate: sweep_rate(bound, body_micros, 1),
            window: Duration::from_millis(300),
            arrivals: None,
            bound,
            drop_every: 0,
        };
        for side in Side::ALL {
            open_loop_side(side, admission, &warm).await?;
        }

        let budget = SLO_P99_NANOS.max(
            body_micros
                .saturating_mul(1_000)
                .saturating_mul(1 + BUDGET_EXTRA_BODIES),
        );
        println!(
            "bound {bound}  (declared capacity {} arrivals/s at a {} us body; knee budget \
             {:.1} ms = the {SLO_P99_NANOS} ms floor or {BUDGET_EXTRA_BODIES} further service \
             time{}, whichever is larger)",
            capacity_per_second(bound, body_micros),
            body_micros,
            budget as f64 / 1_000_000.0,
            if ladder_clamped(bound, body_micros) {
                "; ladder top clamped by the generator's own rate"
            } else {
                ""
            }
        );
        print_columns(&SWEEP_COLUMNS);
        for rate in sweep_ladder(bound, body_micros) {
            let spec = OpenLoopSpec {
                body_micros,
                offered_rate: rate,
                window,
                // The count the rate and the window define: both sides are offered the
                // same work, so a difference in what they placed is a difference in the
                // engines rather than in how long each had.
                arrivals: Some(sweep_arrivals(rate, window)),
                bound,
                drop_every: 0,
            };
            let mut runs = Vec::new();
            for side in Side::ALL {
                let run = measure_point(side, admission, &spec).await?;
                print_sweep_row(&run);
                if let Some(rss) = run.rss_bytes {
                    println!(
                        "           peak RSS {rss} bytes at the run's peak ({})",
                        run.rss_source
                    );
                }
                json_rows.push(sweep_json(&run));
                runs.push(run);
            }
            let label = format!("bound {bound} at {}/s", spec.offered_rate);
            if !open_loop_pair_fair(&label, &runs[0], &runs[1])? {
                println!(
                    "           a saturation point: facade refused {}, baseline refused {} — \
                     each side's own conservation gate is what gates it",
                    runs[0].refused, runs[1].refused
                );
            }
            points.push(SweepPair {
                bound,
                offered_rate: spec.offered_rate,
                facade: runs.swap_remove(0),
                baseline: runs.swap_remove(0),
            });
        }
        json_bodies.push(format!(
            "{{\"bound\":{bound},\"body_micros\":{body_micros},\"declared_capacity_per_second\":{}}}",
            capacity_per_second(bound, body_micros)
        ));
        println!();
    }

    declare_knees(&points, capacity_target, body_override);
    write_sweep_json(
        json,
        admission,
        window,
        capacity_target,
        body_override,
        workers,
        &json_bodies,
        &json_rows,
    );
    Ok(())
}

/// The knee per bound and per side, declared against [`SLO_P99_NANOS`].
///
/// The knee is the highest ladder rate that refused nothing and stayed inside the
/// budget. It is declared rather than eyeballed because a curve has no knee of its
/// own: the same table read against a 10 ms SLO has a different one, and the SLO is
/// named on the same line so the two cannot be separated.
fn declare_knees(points: &[SweepPair], capacity_target: u64, body_override: u64) {
    println!(
        "knee — the highest offered rate with no refusal and p99 inside the bound's budget \
         ({SLO_P99_NANOS} ms or {BUDGET_EXTRA_BODIES} further service time, whichever is \
         larger)"
    );
    print_columns(&KNEE_COLUMNS);
    for bound in SWEEP_BOUNDS {
        let ladder = sweep_ladder(bound, body_for_bound(bound, capacity_target, body_override));
        let top_rate = ladder.last().copied().unwrap_or(0);
        for side in Side::ALL {
            let mut knee_rate = 0_u64;
            let mut knee_achieved = 0.0;
            let mut knee_p99 = 0_u64;
            let mut refused_at_top = 0_u64;
            for point in points.iter().filter(|point| point.bound == bound) {
                let run = point.run(side);
                if run.within_slo() {
                    knee_rate = point.offered_rate;
                    knee_achieved = run.throughput_per_second();
                    knee_p99 = run.histogram.quantile_nanos(0.99);
                }
                if point.offered_rate == top_rate {
                    refused_at_top = run.refused;
                }
            }
            let clamped = if ladder_clamped(bound, body_for_bound(bound, capacity_target, body_override))
                && knee_rate == top_rate
            {
                " (generator-capped)"
            } else {
                ""
            };
            // A knee of zero is not a knee of nothing: it is a ladder on which every rung
            // was past the budget, and saying so is the fact a reader needs. A bare `0`
            // in this column reads as "not measured".
            let knee = if knee_rate == 0 {
                format!("none on this ladder{clamped}")
            } else {
                format!("{knee_rate}{clamped}")
            };
            print_row(&[
                bound.to_string(),
                side.as_str().to_string(),
                knee,
                rate_cell(knee_achieved),
                if knee_rate == 0 {
                    "-".to_string()
                } else {
                    format!("{:.1} ms", knee_p99 as f64 / 1_000_000.0)
                },
                refused_at_top.to_string(),
            ]);
        }
    }
    println!();
}

/// Write the sweep's results rows, if a path was given.
///
/// A write failure is reported rather than propagated: the measurement has already
/// been made and printed, and refusing the whole run because the record could not be
/// filed would throw away the numbers in order to complain about the filing.
#[expect(
    clippy::too_many_arguments,
    reason = "the record's provenance is one fact per parameter; a struct here would be a \
              second place where a reader has to learn which field is which"
)]
fn write_sweep_json(
    json: Option<&str>,
    admission: Admission,
    window: Duration,
    capacity_target: u64,
    body_override: u64,
    workers: Option<usize>,
    bounds: &[String],
    rows: &[String],
) {
    let Some(path) = json else {
        return;
    };
    let body = format!(
        "{{\"tool\":\"lgwks-bench-async-saturation\",\"door\":\"{}\",\
          \"window_seconds\":{},\"capacity_target_per_second\":{capacity_target},\
          \"body_micros_override\":{body_override},\"max_offered_rate\":{MAX_OFFERED_RATE},\
          \"runtime_workers\":{},\"host_cores_visible\":{},\
          \"declared_capacity_note\":\"a point's declared capacity is bound * 1000000 / \
          body_micros arrivals per second, and the body cost is derived per bound to reach \
          capacity_target_per_second; the measured knee is read from the points, not from \
          this arithmetic\",\"slo_p99_floor_nanos\":{SLO_P99_NANOS},\
          \"slo_extra_bodies\":{BUDGET_EXTRA_BODIES},\
          \"bounds\":[\n{}\n],\"points\":[\n{}\n]}}",
        admission.as_str(),
        window.as_secs(),
        workers.map_or_else(|| "null".to_string(), |count| count.to_string()),
        std::thread::available_parallelism().map_or(0, |cores| cores.get()),
        bounds.join(",\n"),
        rows.join(",\n")
    );
    match std::fs::write(path, body) {
        Ok(()) => println!("wrote {path}"),
        Err(error) => println!("could not write {path}: {error}"),
    }
}

/// One sweep point as a results-file row.
fn sweep_json(run: &SideRun) -> String {
    let (p50, p95, p99) = run.histogram.percentiles_nanos();
    let rss = run
        .rss_bytes
        .map_or_else(|| "null".to_string(), |bytes| bytes.to_string());
    format!(
        "{{\"bound\":{},\"side\":\"{}\",\"door\":\"{}\",\"offered_rate\":{},\"body_micros\":{},\
          \"offered\":{},\
          \"admitted\":{},\"refused\":{},\"completed\":{},\"cancelled\":{},\"aborted\":{},\
          \"work_units\":{},\"refusal_rate\":{},\"p50_nanos\":{p50},\"p95_nanos\":{p95},\
          \"p99_nanos\":{p99},\"max_nanos\":{},\"mean_nanos\":{},\"peak_queue\":{},\
          \"mean_queue\":{},\"max_lag_millis\":{},\"drain_millis\":{},\
          \"throughput_per_second\":{},\"p99_budget_nanos\":{},\"within_slo\":{},\"min_nanos\":{},\
          \"clamped_samples\":{},\"rss_bytes\":{rss},\"rss_source\":\"{}\"}}",
        run.bound,
        run.side.as_str(),
        run.admission.as_str(),
        run.offered_rate,
        run.body_micros,
        run.offered,
        run.admitted,
        run.refused,
        run.completed,
        run.cancelled,
        run.aborted,
        run.work_units,
        run.refusal_rate(),
        run.histogram.max_nanos(),
        run.histogram.mean_nanos(),
        run.depth.peak,
        run.depth.mean(),
        run.max_lag.as_millis(),
        run.drain.as_millis(),
        run.throughput_per_second(),
        run.p99_budget_nanos(),
        run.within_slo(),
        run.histogram.min_nanos(),
        run.histogram.clamped(),
        run.rss_source
    )
}

// ── In-flight tiers ──────────────────────────────────────────────────────────

/// How long one tier may take to admit everything it asked for before the tier is
/// reported as not reached.
const TIER_ADMIT_LIMIT: Duration = Duration::from_secs(180);

/// A gate every admitted body waits on, so a tier's in-flight count is *proved* by the
/// bodies themselves rather than asserted from a counter.
#[derive(Debug)]
struct Gate {
    entered: AtomicU64,
    open: AtomicBool,
    notify: tokio::sync::Notify,
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
struct TierBody {
    arrival: Arrival,
    gate: Arc<Gate>,
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
    let reached = gate
        .await_tier(u64::try_from(tier).unwrap_or(u64::MAX))
        .await;
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
struct TierRun {
    side: Side,
    /// The tier the caller asked for.
    requested: usize,
    /// How many were concurrently admitted, as the bodies themselves observed.
    reached: u64,
    /// The engine's own maximum permit count.
    ceiling: usize,
    placed: u64,
    completed: u64,
    aborted: u64,
    work_units: u64,
    in_flight_at_end: u64,
    histogram: Histogram,
    rss_bytes: Option<u64>,
    rss_source: &'static str,
    wall: Duration,
}

impl TierRun {
    /// p50, p95 and p99, in nanoseconds.
    fn percentiles(&self) -> (u64, u64, u64) {
        self.histogram.percentiles_nanos()
    }

    /// Resident bytes per concurrently admitted task, or `None` where the host cannot
    /// report RSS at all.
    fn rss_per_task(&self) -> Option<f64> {
        self.rss_bytes
            .map(|bytes| bytes as f64 / self.reached.max(1) as f64)
    }

    /// The RSS cell for a table: bytes, or an explicit `null`.
    fn rss_cell(&self) -> String {
        self.rss_bytes
            .map_or_else(|| "null".to_string(), |bytes| format!("{bytes} B"))
    }

    /// The RSS-per-task cell for a table: bytes per task, or an explicit `null`.
    fn rss_per_task_cell(&self) -> String {
        self.rss_per_task()
            .map_or_else(|| "null".to_string(), |bytes| format!("{bytes:.0} B"))
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
    while supervisor.stats().completed < u64::try_from(tier).unwrap_or(u64::MAX) {
        if supervisor.reap() == 0 {
            tokio::time::sleep(Duration::from_micros(200)).await;
        }
    }
    let stats = supervisor.stats();
    Ok(TierRun {
        side: Side::Facade,
        requested: tier,
        reached,
        ceiling: Semaphore::MAX_PERMITS,
        placed: u64::try_from(tier).unwrap_or(u64::MAX),
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
    let requested = u64::try_from(tier).unwrap_or(u64::MAX);
    if run.reached != requested {
        return Err(format!(
            "{label}: only {} of {tier} tasks were concurrently admitted within {} s — \
             requested {tier}, reached {}, engine ceiling {}",
            run.reached,
            TIER_ADMIT_LIMIT.as_secs(),
            run.reached,
            run.ceiling
        )
        .into());
    }
    if run.completed.saturating_add(run.aborted) != run.placed {
        return Err(format!(
            "{label}: {} of {} admitted tasks reached a terminal state",
            run.completed.saturating_add(run.aborted),
            run.placed
        )
        .into());
    }
    work_units_match(&label, run.completed, run.work_units, "tasks")
        .map_err(|reason| -> Box<dyn std::error::Error> { reason.into() })?;
    if run.in_flight_at_end != 0 {
        return Err(format!(
            "{label}: {} tasks still in flight after the drain",
            run.in_flight_at_end
        )
        .into());
    }
    Ok(run)
}

/// One tier's row in the results file.
fn tier_json(run: &TierRun) -> String {
    let (p50, _, p99) = run.percentiles();
    let per_task = run
        .rss_per_task()
        .map_or_else(|| "null".to_string(), |bytes| bytes.to_string());
    format!(
        "{{\"tier\":{},\"side\":\"{}\",\"reached\":{},\"ceiling\":{},\"placed\":{},\
          \"completed\":{},\"aborted\":{},\"work_units\":{},\"p50_nanos\":{p50},\
          \"p99_nanos\":{p99},\"peak_rss_bytes\":{},\"rss_bytes_per_in_flight_task\":{per_task},\
          \"rss_source\":\"{}\",\"wall_millis\":{}}}",
        run.requested,
        run.side.as_str(),
        run.reached,
        run.ceiling,
        run.placed,
        run.completed,
        run.aborted,
        run.work_units,
        run.rss_cell(),
        run.rss_source,
        run.wall.as_millis()
    )
}

/// Drive every in-flight tier on both sides and report the memory each cost.
async fn in_flight_tiers(
    only: Option<usize>,
    workers: Option<usize>,
    json: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("in-flight tiers — that many tasks CONCURRENTLY ADMITTED, each tier its own ceiling");
    println!("every body parks on a gate until the whole tier is admitted, so the peak");
    println!("in-flight count is what the bodies saw and not a counter's opinion\n");
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
        println!();
    }

    if let Some(path) = json {
        let body = format!(
            "{{\"tool\":\"lgwks-bench-async-inflight\",\"note\":\"each tier's ceiling is the \
              tier; every body parks on a gate until the whole tier is admitted, so the peak \
              in-flight count is observed rather than inferred\",\"runtime_workers\":{},\
              \"host_cores_visible\":{},\"tiers\":[\n{}\n]}}",
            workers.map_or_else(|| "null".to_string(), |count| count.to_string()),
            std::thread::available_parallelism().map_or(0, |cores| cores.get()),
            json_rows.join(",\n")
        );
        std::fs::write(path, body)?;
        println!("wrote {path}");
    }
    Ok(())
}

// ── Overload and recovery ────────────────────────────────────────────────────

/// Which phase of the overload run is offering load.
///
/// An enum rather than a name compared at each use: a phase whose identity is a
/// string is a phase whose identity a typo can change, and the recovery reading
/// depends on telling the recovery phase from the other two.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PhaseLabel {
    /// Steady load below the knee: the p99 the run must come back to.
    Baseline,
    /// Twice the knee: the load that builds the queue.
    Overload,
    /// Back down below the knee: the drain and the return are measured here.
    Recovery,
}

impl PhaseLabel {
    /// The name the record carries.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Overload => "overload",
            Self::Recovery => "recovery",
        }
    }
}

/// One phase of the overload run: an offered rate and how long it holds.
#[derive(Clone, Copy)]
struct Phase {
    /// What this phase is.
    label: PhaseLabel,
    offered_rate: u64,
    window: Duration,
}

/// What one overload run measured.
#[derive(Debug)]
struct RecoveryRun {
    side: Side,
    bound: usize,
    /// The p99 the side served at 0.5× the knee, before any overload.
    baseline_p99_nanos: u64,
    /// The offered rate the overload ran at.
    overload_rate: u64,
    /// Deepest queue the overload reached, in arrivals.
    overload_peak_queue: usize,
    /// Wall time from the overload's last arrival until nothing was in flight.
    drain: Duration,
    /// Wall time from the overload's last arrival until a whole window's p99 was back
    /// inside the baseline's.
    recovered: Duration,
    /// The p99 of the first window that was back inside the baseline.
    recovered_p99_nanos: u64,
    /// The conservation counters of the whole run.
    offered: u64,
    admitted: u64,
    refused: u64,
    completed: u64,
    work_units: u64,
}

/// How long one recovery window offers load before its p99 is read.
///
/// A window has to be long enough to hold enough samples for a p99 to mean anything
/// and short enough that "back to baseline" is a time a reader can act on. 500 ms at
/// the recovery rate is several thousand samples on this host and is bounded above by
/// the run itself rather than by a timer.
const RECOVERY_WINDOW: Duration = Duration::from_millis(500);

/// The fewest samples a recovery window must hold before its p99 is read.
///
/// Below this a window's p99 is the maximum of a handful of samples, and a run that
/// reported it as a recovery would be reporting noise as a measurement.
const RECOVERY_MIN_SAMPLES: u64 = 64;

/// How long the recovery phase may run before the run reports it did not recover.
///
/// The overload left a queue behind; draining it is bounded work, and an unbounded
/// wait for a p99 that never returns would turn a slow host into a hung rig.
const RECOVERY_LIMIT: Duration = Duration::from_secs(120);

/// Drive one side through a baseline phase, an overload phase and a recovery phase,
/// and report what it took to come back.
///
/// The phases are the issue's: a steady baseline at 0.5× the knee, then 2× the knee
/// long enough to build a queue, then back down to 0.5×. Two readings come out: how
/// long the drain took, and how long until a whole window's p99 was back inside the
/// baseline's. The conservation counters come from the same gate as every other run,
/// so "nothing was lost or duplicated while it recovered" is checked rather than
/// assumed.
async fn overload_run(
    side: Side,
    bound: usize,
    knee_rate: u64,
    body_micros: u64,
    baseline_window: Duration,
    overload_window: Duration,
) -> Result<RecoveryRun, Box<dyn std::error::Error>> {
    let half = (knee_rate / 2).max(1);
    let twice = knee_rate.saturating_mul(2);
    let recovery_window = baseline_window;
    let phases = [
        Phase {
            label: PhaseLabel::Baseline,
            offered_rate: half,
            window: baseline_window,
        },
        Phase {
            label: PhaseLabel::Overload,
            offered_rate: twice,
            window: overload_window,
        },
        Phase {
            label: PhaseLabel::Recovery,
            offered_rate: half,
            window: recovery_window,
        },
    ];

    println!(
        "\n{} at bound {bound}: baseline {} s at {half}/s, overload {} s at {twice}/s, \
         recovery {} s at {half}/s",
        side.as_str(),
        baseline_window.as_secs(),
        overload_window.as_secs(),
        recovery_window.as_secs()
    );

    let mut engine = match side {
        Side::Facade => EngineBox::Facade(FacadeEngine::new(bound, Admission::Backpressure)),
        Side::Baseline => EngineBox::Baseline(BaselineEngine::new(bound, Admission::Backpressure)),
    };
    let work = Arc::new(AtomicU64::new(0));
    let recorders = RecorderSet::new();
    let mut depth = DepthTrace::new();
    let mut offered = 0_u64;
    let mut admitted = 0_u64;
    let mut refused = 0_u64;
    let mut baseline_p99 = 0_u64;
    // The instant the overload stopped offering: every recovery reading is measured
    // from here, so "back to baseline" includes the drain rather than starting after
    // it.
    let mut overload_stopped = Instant::now();
    let mut overload_peak_queue = 0_usize;
    let mut drain = Duration::ZERO;
    let mut recovered = Duration::ZERO;
    let mut recovered_p99 = 0_u64;

    for phase in phases {
        let period = period_nanos(phase.offered_rate);
        let t0 = Instant::now();
        while t0.elapsed() < phase.window {
            engine.reap();
            let intended = intended_at(t0, offered, period);
            wait_until(intended).await;
            offered = offered.saturating_add(1);
            let arrival = Arrival::mint(&work, &recorders, intended, body_micros);
            let placed = engine.admit(arrival).await;
            count_arrival(placed, &mut admitted, &mut refused);
            let next_intended = intended_at(t0, offered, period);
            sample_queue(
                Instant::now(),
                next_intended,
                period,
                engine.in_flight(),
                &mut depth,
            );
            let queue = depth.peak;
            match phase.label {
                PhaseLabel::Overload => {
                    overload_peak_queue = overload_peak_queue.max(queue);
                }
                // The baseline phase has nothing to watch for: it is the reading the
                // other two are compared against.
                PhaseLabel::Baseline => {}
                // The recovery phase watches for the two facts it is measuring: the
                // queue is empty, and a whole window has served a p99 inside the
                // baseline's. The window is bounded by `RECOVERY_LIMIT` so a host
                // that never recovers reports that rather than hanging.
                PhaseLabel::Recovery => {
                    if recovered_p99 == 0
                        && engine.in_flight() == 0
                        && t0.elapsed() >= RECOVERY_WINDOW
                    {
                        let window = Recorder::merged(recorders.shards());
                        // A window below `RECOVERY_MIN_SAMPLES` has a p99 that is the
                        // maximum of a handful of samples; reading it as "recovered"
                        // would report noise as a measurement, so the window is
                        // discarded and the next one is read instead.
                        let window_p99 = if window.count() >= RECOVERY_MIN_SAMPLES {
                            window.quantile_nanos(0.99)
                        } else {
                            0
                        };
                        if window_p99 != 0 && window_p99 <= baseline_p99 {
                            recovered_p99 = window_p99;
                            recovered = Instant::now().saturating_duration_since(overload_stopped);
                        }
                        recorders.reset();
                    }
                }
            }
        }

        let histogram = Recorder::merged(recorders.shards());
        let p99 = histogram.quantile_nanos(0.99);
        match phase.label {
            PhaseLabel::Baseline => {
                baseline_p99 = p99;
                println!(
                    "  baseline  {} offered  {} admitted  p99 {:.3} ms",
                    offered,
                    admitted,
                    p99 as f64 / 1_000_000.0
                );
            }
            PhaseLabel::Overload => {
                println!(
                    "  overload  {} offered  {} admitted  {} refused  p99 {:.3} ms  peak \
                     queue {overload_peak_queue}",
                    offered,
                    admitted,
                    refused,
                    p99 as f64 / 1_000_000.0
                );
                // Drain from here, timed: the overload's queue is what has to go.
                overload_stopped = Instant::now();
                let drain_started = overload_stopped;
                engine.drain().await;
                drain = drain_started.elapsed();
            }
            PhaseLabel::Recovery => {
                if recovered_p99 == 0 {
                    println!(
                        "  recovery  drained in {:.3} ms  p99 did NOT return to the baseline \
                         {} ms within the {} s phase — reported as not recovered rather \
                         than as a number",
                        drain.as_secs_f64() * 1_000.0,
                        RECOVERY_LIMIT.as_secs(),
                        phase.window.as_secs()
                    );
                } else {
                    println!(
                        "  recovery  drained in {:.3} ms  p99 back to baseline after {:.3} ms \
                         ({} ms inside)",
                        drain.as_secs_f64() * 1_000.0,
                        recovered.as_secs_f64() * 1_000.0,
                        recovered_p99 as f64 / 1_000_000.0
                    );
                }
            }
        }
        if phase.label != PhaseLabel::Recovery {
            recorders.reset();
        }
    }

    engine.drain().await;
    let (completed, cancelled, aborted) = engine.terminals();
    if cancelled != 0 || aborted != 0 {
        return Err(format!(
            "{} overload run ended with {cancelled} cancelled and {aborted} aborted tasks: a \
             recovery that drops work is not a recovery",
            side.as_str()
        )
        .into());
    }
    let work_units = work.load(Ordering::SeqCst);
    let run = RecoveryRun {
        side,
        bound,
        baseline_p99_nanos: baseline_p99,
        overload_rate: twice,
        overload_peak_queue,
        drain,
        recovered,
        recovered_p99_nanos: recovered_p99,
        offered,
        admitted,
        refused,
        completed,
        work_units,
    };
    // The same conservation gate as every other run, on the same counters.
    let conserved = RecoveryRun::conserved(&run);
    conserved.map_err(|reason| -> Box<dyn std::error::Error> { reason.into() })?;
    Ok(run)
}

impl RecoveryRun {
    /// Every arrival was admitted or refused, every admitted body completed, and the
    /// bodies' own work counter agrees — the property "nothing was lost or duplicated
    /// while it recovered" is a check on, not a claim about, the run.
    fn conserved(run: &RecoveryRun) -> Result<(), String> {
        let accounted = run.admitted.saturating_add(run.refused);
        if accounted != run.offered {
            return Err(format!(
                "{}: offered {}, admitted {} plus refused {} is {accounted} — an arrival was \
                 lost or counted twice",
                run.side.as_str(),
                run.offered,
                run.admitted,
                run.refused
            ));
        }
        if run.completed != run.admitted {
            return Err(format!(
                "{}: {} of {} admitted bodies completed — work was lost or duplicated",
                run.side.as_str(),
                run.completed,
                run.admitted
            ));
        }
        work_units_match(
            "overload and recovery",
            run.completed,
            run.work_units,
            "bodies",
        )
    }
}

/// Either engine, as one value, so a multi-phase run is written once.
///
/// The phases of an overload run are the same three on either side, and an `enum`
/// with the two `Engine` implementations under it is what lets the phase loop call
/// `reap`/`admit`/`drain` without knowing which engine it is driving. Two phase loops
/// would be two programs again — the same objection the `Engine` trait exists to
/// answer.
enum EngineBox {
    Facade(FacadeEngine),
    Baseline(BaselineEngine),
}

impl EngineBox {
    /// Join whatever has finished, without waiting.
    fn reap(&mut self) {
        match self {
            Self::Facade(engine) => engine.reap(),
            Self::Baseline(engine) => engine.reap(),
        }
    }

    /// Offer one arrival at the ceiling, reporting whether it was admitted.
    async fn admit(&mut self, arrival: Arrival) -> bool {
        match self {
            Self::Facade(engine) => engine.admit(arrival).await,
            Self::Baseline(engine) => engine.admit(arrival).await,
        }
    }

    /// Tasks in flight at this instant.
    fn in_flight(&self) -> u64 {
        match self {
            Self::Facade(engine) => engine.in_flight(),
            Self::Baseline(engine) => engine.in_flight(),
        }
    }

    /// Join everything, blocking until nothing is left.
    async fn drain(&mut self) {
        match self {
            Self::Facade(engine) => engine.drain().await,
            Self::Baseline(engine) => engine.drain().await,
        }
    }

    /// Terminal counters after the drain: completed, cancelled, aborted.
    fn terminals(&self) -> (u64, u64, u64) {
        match self {
            Self::Facade(engine) => engine.terminals(),
            Self::Baseline(engine) => engine.terminals(),
        }
    }
}

/// Drive the overload-and-recovery run on both sides at one bound.
///
/// The bound is a parameter rather than a constant because the recovery time is a
/// property of the ceiling a reader chose, not of the runtime: the same recovery at
/// bound 64 and at bound 1,024 are different claims.
async fn overload_and_recovery(
    bound: usize,
    knee_rate: u64,
    body_micros: u64,
    baseline_window: Duration,
    overload_window: Duration,
    json: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "overload and recovery — {baseline_window:?} baseline, {overload_window:?} at 2x the knee, then back down"
    );
    println!("drain time and time-to-baseline-p99 are both measured, and the conservation gate");
    println!("checks that nothing was lost or duplicated on either side of the transition\n");

    let mut rows = Vec::new();
    for side in Side::ALL {
        let run = overload_run(
            side,
            bound,
            knee_rate,
            body_micros,
            baseline_window,
            overload_window,
        )
        .await?;
        print_row(&[
            side.as_str().to_string(),
            bound.to_string(),
            format!("{:.3} ms", run.baseline_p99_nanos as f64 / 1_000_000.0),
            run.overload_rate.to_string(),
            run.overload_peak_queue.to_string(),
            format!("{:.1} ms", run.drain.as_secs_f64() * 1_000.0),
            format!("{:.1} ms", run.recovered.as_secs_f64() * 1_000.0),
            run.offered.to_string(),
            run.admitted.to_string(),
            run.completed.to_string(),
        ]);
        rows.push(run);
    }

    if let Some(path) = json {
        let body = format!(
            "{{\"tool\":\"lgwks-bench-async-overload\",\"bound\":{bound},\
              \"knee_offered_rate\":{knee_rate},\"baseline_window_ms\":{},\
              \"overload_window_ms\":{},\"runs\":[\n{}\n]}}",
            baseline_window.as_millis(),
            overload_window.as_millis(),
            rows.iter()
                .map(recovery_json)
                .collect::<Vec<_>>()
                .join(",\n")
        );
        std::fs::write(path, body)?;
        println!("wrote {path}");
    }
    Ok(())
}

/// One overload run as a results-file row.
fn recovery_json(run: &RecoveryRun) -> String {
    format!(
        "{{\"side\":\"{}\",\"bound\":{},\"baseline_p99_nanos\":{},\"overload_offered_rate\":{},\
          \"overload_peak_queue\":{},\"drain_millis\":{},\"recovered_millis\":{},\"phase\":\"{}\",\
          \"recovered_p99_nanos\":{},\"offered\":{},\"admitted\":{},\"refused\":{},\
          \"completed\":{},\"work_units\":{}}}",
        run.side.as_str(),
        run.bound,
        run.baseline_p99_nanos,
        run.overload_rate,
        run.overload_peak_queue,
        run.drain.as_millis(),
        run.recovered.as_millis(),
        PhaseLabel::Recovery.as_str(),
        run.recovered_p99_nanos,
        run.offered,
        run.admitted,
        run.refused,
        run.completed,
        run.work_units
    )
}

// ── Allocation attribution ───────────────────────────────────────────────────

/// One measured allocation window: what it exercised and what it cost.
struct AllocWindow {
    /// The operation the window timed and counted.
    what: &'static str,
    /// How many times it ran inside the window.
    iterations: u64,
    allocs: u64,
    bytes: u64,
}

impl AllocWindow {
    /// Allocations per iteration.
    fn per_iteration(&self) -> f64 {
        if self.iterations == 0 {
            return 0.0;
        }
        (self.allocs as f64) / (self.iterations as f64)
    }

    /// One printed line.
    fn line(&self) -> String {
        format!(
            "{:<46} {:>8} iterations {:>9} allocs {:>10.2} per call",
            self.what,
            self.iterations,
            self.allocs,
            self.per_iteration()
        )
    }

    /// One results-file row.
    fn json(&self) -> String {
        format!(
            "{{\"operation\":\"{}\",\"iterations\":{},\"allocations\":{},\"bytes\":{},\
              \"allocations_per_iteration\":{}}}",
            self.what,
            self.iterations,
            self.allocs,
            self.bytes,
            self.per_iteration()
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
async fn count_allocations<F, Fut>(iterations: u64, what: &'static str, body: F) -> AllocWindow
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
async fn allocation_attribution(json: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    const TASKS: u64 = 1_024;
    const UNCONTENDED: usize = 4_096;
    const CONTENDED: usize = 8;

    println!("allocation attribution — every window counted separately from every timed round");
    println!("{TASKS} tasks per wave unless stated otherwise\n");

    let mut windows = Vec::new();

    // The whole facade wave at a contended ceiling: the shipped figure.
    windows.push(
        count_allocations(TASKS, "facade spawn wave, contended (bound 8)", || async {
            let _wave = facade_side(TASKS as usize, CONTENDED).await;
        })
        .await,
    );
    // The same wave at a ceiling the wave never reaches: the same work with no waiting.
    windows.push(
        count_allocations(
            TASKS,
            "facade spawn wave, uncontended (bound 4096)",
            || async {
                let _wave = facade_side(TASKS as usize, UNCONTENDED).await;
            },
        )
        .await,
    );
    // The raw equivalent at the same contended ceiling: the baseline.
    windows.push(
        count_allocations(TASKS, "raw tokio wave, contended (bound 8)", || async {
            let _wave = baseline_side(TASKS as usize, CONTENDED).await;
        })
        .await,
    );
    // One child token, on its own: the per-task cancellation token the facade mints.
    windows.push(
        count_allocations(
            TASKS,
            "one child token (CancellationToken::child_token)",
            || async {
                let root = lgwks_bot::rt::sync::CancellationToken::new();
                let child = root.child_token();
                let _kept = child;
            },
        )
        .await,
    );
    // One raced wait, on its own: the cancellation race the admission path performs
    // per contended spawn.
    windows.push(
        count_allocations(
            TASKS,
            "one cancellation race (run_until_cancelled)",
            || async {
                let root = lgwks_bot::rt::sync::CancellationToken::new();
                let raced = root.run_until_cancelled(async {});
                let _done = raced.await;
            },
        )
        .await,
    );

    for window in &windows {
        println!("  {}", window.line());
    }
    println!(
        "\nthe facade's excess over raw tokio is {} allocs per task at the contended \
         ceiling, and {} of those are the wait for a slot rather than the spawn",
        windows[0].per_iteration() - windows[2].per_iteration(),
        windows[0].per_iteration() - windows[1].per_iteration()
    );

    if let Some(path) = json {
        let body = format!(
            "{{\"tool\":\"lgwks-bench-async-alloc-attribution\",\"note\":\"each window is \
              counted separately; the facade's excess is the difference between the whole \
              wave and the raw wave at the same ceiling\",\"windows\":[\n{}\n]}}",
            windows
                .iter()
                .map(AllocWindow::json)
                .collect::<Vec<_>>()
                .join(",\n")
        );
        std::fs::write(path, body)?;
        println!("wrote {path}");
    }
    Ok(())
}

/// Run one open-loop mode on a runtime of its own.
///
/// The open-loop modes drive a generator across every worker thread and are written
/// as one async body each, so each gets its own `block_on` here rather than being
/// nested inside a runtime the caller already owns: calling `block_on` from inside a
/// runtime is the documented way to deadlock a multi-threaded scheduler, and a
/// measurement rig that hangs is worse than one that refuses.
///
/// `workers` pins the scheduler's thread count and `None` takes the discovered default.
/// That is the closest this host comes to the estate's 1–2 vCPU profile, and the label
/// is deliberately narrower than that profile: see [`build_runtime`].
fn block_on_mode<F, Fut>(
    workers: Option<usize>,
    body: F,
) -> Result<(), Box<dyn std::error::Error>>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<(), Box<dyn std::error::Error>>>,
{
    build_runtime(workers)?.block_on(body())
}

/// The runtime this rig measures on, and what a pinned worker count does and does not mean.
///
/// **What it is:** the scheduler pinned to `workers` threads, which bounds the parallelism
/// the *work* under test can reach — the generator, every arrival's body, the drain, and
/// Tokio's timer wheel all run on those threads and nowhere else.
///
/// **What it is not, and the reason the label matters:** this is not the estate's
/// 1–2 vCPU / 1–2 GB VPS profile. A thread count bounds how much of this host runs at
/// once; a vCPU count is a property of a machine whose cores, cache, memory bandwidth and
/// scheduler are all smaller. macOS exposes no cgroup, no `taskset`, no `taskpolicy` CPU
/// set and no `cpulimit` — all four were checked on the reference host and all four are
/// absent — so no run on this host can be presented as that profile's numbers. A run with
/// `workers = 2` is reported as *two worker threads on an Apple M5 Pro*, and the README
/// says so where the figure is published.
fn build_runtime(workers: Option<usize>) -> std::io::Result<Runtime> {
    match workers {
        Some(count) => {
            let Some(count) = std::num::NonZeroUsize::new(count) else {
                let refusal = Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "lgwks-bench-async: --workers=0 would build a runtime with no worker, \
                     which cannot make progress",
                ));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build_runtime: refusing a zero worker count");
                return refusal;
            };
            RuntimeBuilder::new()
                .worker_threads(Some(count))
                .build()
        }
        None => Runtime::new(),
    }
}

/// The seeded open-loop model, run over the same bounds the sweep uses at every bound
/// small enough to sweep exhaustively.
///
/// Every world is simulated twice from its own seed, and the run exits non-zero unless
/// the two agree on the trace hash *and* on every reported figure. That is the same
/// property `tests/it/sim_*` families assert in-process, checked here on the path a
/// reader runs: a generator whose numbers cannot be reproduced from a seed are numbers
/// with no provenance.
fn open_loop_simulation() -> Result<(), Box<dyn std::error::Error>> {
    println!("seeded open-loop simulation — the model the live driver is checked against");
    println!("one seed drives arrival jitter and body length; every world is replayed\n");
    print_columns(&[
        "seed",
        "bound",
        "world",
        "offered/s",
        "admitted",
        "refused",
        "p50 intended us",
        "p99 intended us",
        "p99 actual us",
        "trace hash",
    ]);

    // A permit of a 5 µs body sustains about 200,000 arrivals a second, so each
    // bound's ladder is derived from its own ceiling: a tenth of capacity (quiet), five
    // times capacity (past the knee), and the same five times on the refusal door. The
    // rates are derived rather than typed so a bound is never swept at a rate that does
    // not reach its knee — which would print a saturation arm that saturated nothing.
    const PERMITS_PER_SECOND: u64 = 200_000;
    let mut failures = Vec::new();
    for seed in [
        0x0000_0000_0000_0001_u64,
        0x5EED_0000_0000_0001,
        0xDEAD_BEEF_0000_0001,
    ] {
        for bound in [1_usize, 4, 64] {
            let permits = u64::try_from(bound).unwrap_or(1);
            let ladder = [
                (
                    "quiet",
                    permits.saturating_mul(PERMITS_PER_SECOND / 10).max(1),
                    false,
                ),
                (
                    "past the knee",
                    permits.saturating_mul(PERMITS_PER_SECOND * 5),
                    false,
                ),
                (
                    "refusal door",
                    permits.saturating_mul(PERMITS_PER_SECOND * 5),
                    true,
                ),
            ];
            for (what, offered_rate, refuse) in ladder {
                let spec = SimSpec {
                    offered_rate,
                    arrivals: permits.saturating_mul(8),
                    bound,
                    service_min_nanos: 2_000,
                    service_max_nanos: 8_000,
                    jitter: true,
                    refuse_at_bound: refuse,
                    seed,
                };
                let first = simulate(&spec);
                let second = simulate(&spec);
                let replayed = first.trace_hash == second.trace_hash && first == second;
                if !replayed {
                    failures.push(format!(
                        "seed {seed} at bound {bound} at {offered_rate}/s did not replay: \
                         hash {} then {}",
                        first.trace_hash, second.trace_hash
                    ));
                }
                print_row(&[
                    format!("{seed:#x}"),
                    bound.to_string(),
                    what.to_string(),
                    offered_rate.to_string(),
                    first.admitted.to_string(),
                    first.refused.to_string(),
                    micros(first.p50_intended_nanos),
                    micros(first.p99_intended_nanos),
                    micros(first.p99_actual_nanos),
                    format!("{:#018x}", first.trace_hash),
                ]);
            }
        }
    }
    println!();

    if !failures.is_empty() {
        let refusal = Err(format!(
            "{} seeded world(s) did not replay: {}",
            failures.len(),
            failures.join("; ")
        )
        .into());
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "open_loop_simulation: refusing a simulation that did not replay");
        return refusal;
    }
    println!(
        "every seeded world replayed to the same trace hash; the p99-intended and \
         p99-actual columns are the coordinated-omission gap, measured on one world read \
         two ways"
    );
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut rounds: usize = 15;
    let mut json: Option<String> = None;
    let mut alloc_report = false;
    let mut mutant = false;
    let mut tiers = false;
    let mut matrix = false;
    let mut saturation = false;
    let mut refusal = false;
    let mut inflight = false;
    let mut overload = false;
    let mut attribution = false;
    let mut simulation = false;
    let mut window_seconds: u64 = 2;
    let mut tier: Option<usize> = None;
    let mut knee: u64 = 32_768;
    let mut baseline_seconds: u64 = 4;
    let mut overload_seconds: u64 = 30;
    let mut bound: Option<usize> = None;
    let mut body_micros: u64 = 0;
    let mut capacity_target: u64 = DEFAULT_CAPACITY_TARGET;
    let mut workers: Option<usize> = None;
    for arg in std::env::args().skip(1) {
        if let Some(value) = arg.strip_prefix("--rounds=") {
            rounds = value
                .parse()
                .map_err(|error| format!("rounds must be a number: {error}"))?;
        } else if let Some(value) = arg.strip_prefix("--json=") {
            json = Some(value.to_string());
        } else if let Some(value) = arg.strip_prefix("--window=") {
            window_seconds = value
                .parse()
                .map_err(|error| format!("window must be a number of seconds: {error}"))?;
        } else if let Some(value) = arg.strip_prefix("--tier=") {
            tier = Some(
                value
                    .parse()
                    .map_err(|error| format!("tier must be a number: {error}"))?,
            );
        } else if let Some(value) = arg.strip_prefix("--knee=") {
            knee = value
                .parse()
                .map_err(|error| format!("knee must be an offered rate: {error}"))?;
        } else if let Some(value) = arg.strip_prefix("--baseline-seconds=") {
            baseline_seconds = value
                .parse()
                .map_err(|error| format!("baseline-seconds must be a number: {error}"))?;
        } else if let Some(value) = arg.strip_prefix("--overload-seconds=") {
            overload_seconds = value
                .parse()
                .map_err(|error| format!("overload-seconds must be a number: {error}"))?;
        } else if let Some(value) = arg.strip_prefix("--body-micros=") {
            body_micros = value
                .parse()
                .map_err(|error| format!("body-micros must be a number: {error}"))?;
        } else if let Some(value) = arg.strip_prefix("--capacity-target=") {
            capacity_target = value
                .parse()
                .map_err(|error| format!("capacity-target must be a rate: {error}"))?;
        } else if let Some(value) = arg.strip_prefix("--bound=") {
            bound = Some(
                value
                    .parse()
                    .map_err(|error| format!("bound must be a number: {error}"))?,
            );
        } else if let Some(value) = arg.strip_prefix("--workers=") {
            workers = Some(
                value
                    .parse()
                    .map_err(|error| format!("workers must be a number: {error}"))?,
            );
        } else if let Some(arg) = arg.strip_prefix("--") {
            match arg {
                "alloc-report" => alloc_report = true,
                "mutant-check" => mutant = true,
                "tiers" => tiers = true,
                "matrix" => matrix = true,
                "saturation" => saturation = true,
                "refusal" => refusal = true,
                "inflight" => inflight = true,
                "overload" => overload = true,
                "alloc-attribution" => attribution = true,
                "sim-open-loop" => simulation = true,
                // An unknown flag is refused rather than ignored: a mistyped
                // `--saturaton` that silently ran the default suite would report a
                // number for a run nobody asked for.
                other => {
                    let refusal = Err(format!("unknown flag --{other}").into());
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "main: refusing an unknown flag");
                    return refusal;
                }
            }
        }
    }

    // The provenance every mode's numbers need, printed before the mode's own table so a
    // pasted excerpt carries it. `available_parallelism` is what the runtime would
    // otherwise discover for itself, so the discovered case is printed as the same
    // number the scheduler would have used rather than as the word "default".
    println!(
        "host: {} logical cores visible, runtime workers: {}",
        std::thread::available_parallelism().map_or(0, |cores| cores.get()),
        workers.map_or_else(
            || "discovered (available_parallelism)".to_string(),
            |count| count.to_string()
        )
    );

    if simulation {
        // The seeded model runs alone and exits non-zero when a replay diverges: a
        // simulation whose traces do not reproduce is a simulation whose numbers
        // cannot be attributed to a seed, and the gate on it has to be as loud as
        // the fairness gate's.
        return open_loop_simulation();
    }

    if saturation {
        return block_on_mode(workers, || {
            saturation_sweep(
                Admission::Backpressure,
                Duration::from_secs(window_seconds),
                capacity_target,
                body_micros,
                workers,
                json.as_deref(),
            )
        });
    }

    if refusal {
        return block_on_mode(workers, || {
            saturation_sweep(
                Admission::Refuse,
                Duration::from_secs(window_seconds),
                capacity_target,
                body_micros,
                workers,
                json.as_deref(),
            )
        });
    }

    if inflight {
        return block_on_mode(workers, || in_flight_tiers(tier, workers, json.as_deref()));
    }

    if overload {
        let at_bound = bound.unwrap_or(1_024);
        // The overload run names its own bound, so the body cost is resolved here rather
        // than derived by the sweep: the same `body_for_bound` arithmetic, applied once.
        let at_body = body_for_bound(at_bound, capacity_target, body_micros);
        return block_on_mode(workers, || {
            overload_and_recovery(
                at_bound,
                knee,
                at_body,
                Duration::from_secs(baseline_seconds),
                Duration::from_secs(overload_seconds),
                json.as_deref(),
            )
        });
    }

    if attribution {
        return block_on_mode(workers, || allocation_attribution(json.as_deref()));
    }

    if matrix {
        // The matrix runs against a scratch directory of its own, under the
        // host's own temp root rather than a path baked into the source, so it
        // is portable and leaves nothing in the working tree.
        let dir = std::env::temp_dir().join(format!("lgwks-matrix-{}", std::process::id()));
        // `workload_matrix` drives each row with its own `block_on`, so it is
        // called *outside* a runtime rather than from inside one: nesting the
        // two is the documented way to deadlock a multi-thread scheduler.
        let outcome = workload_matrix(&build_runtime(workers)?, &dir, json.as_deref());
        drop(std::fs::remove_dir_all(&dir));
        return outcome;
    }

    if mutant {
        // The negative control runs alone and exits: a run that also produced a
        // table would leave a reader unable to tell which number the refusal
        // referred to.
        return mutant_check(&build_runtime(workers)?);
    }

    if tiers {
        return tier_ladder(&build_runtime(workers)?, json.as_deref());
    }

    println!("lgwks_bot async matched-semantics comparison (facade vs raw tokio)");
    println!("Every round is paired and gated on identical work; a mismatch aborts the run.\n");

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
        let runtime = build_runtime(workers)?;
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

    let runtime = build_runtime(workers)?;
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
        let parity = if distinguishes {
            "distinguishes"
        } else {
            "spans parity"
        };
        println!("                 95% CI on the paired ratio: [{lo:.2}, {hi:.2}] ({parity})");
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
        let rows: Vec<&str> = json_rows
            .split('\n')
            .filter(|row| !row.is_empty())
            .collect();
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
