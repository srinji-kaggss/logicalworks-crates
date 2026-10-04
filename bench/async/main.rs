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
    use lgwks_bot::effect::{
        ActionDigest, ActionId, AttemptId, EffectKey, EnvironmentEpoch, EnvironmentId,
        FlowRevision, RunId,
    };
    use lgwks_bot::journal::{EffectEvent, EffectJournal, FileJournal};

    const RUN: &str = "0102030405060708090a0b0c0d0e0f10";
    const ACTION: &str = "1112131415161718191a1b1c1d1e1f20";
    const ENV: &str = "2122232425262728292a2b2c2d2e2f30";
    const FLOW: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    const DIGEST: &str = "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeffe0e1e2e3e4e5e6e7e8e9eaebecedeeef";

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
            let attempt = appended.saturating_add(offset) + 1;
            let key = EffectKey::new(
                RunId::from_hex(RUN).map_err(|error| format!("run id: {error}"))?,
                ActionId::from_hex(ACTION).map_err(|error| format!("action id: {error}"))?,
                AttemptId::from_decimal(&format!("{attempt}"))
                    .map_err(|error| format!("attempt id: {error}"))?,
                FlowRevision::from_tagged("blake3_256", FLOW)
                    .map_err(|error| format!("flow revision: {error}"))?,
                ActionDigest::from_tagged("blake3_256", DIGEST)
                    .map_err(|error| format!("action digest: {error}"))?,
                EnvironmentId::from_hex(ENV).map_err(|error| format!("environment: {error}"))?,
                EnvironmentEpoch::from_decimal("1").map_err(|error| format!("epoch: {error}"))?,
            );
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut rounds: usize = 15;
    let mut json: Option<String> = None;
    let mut alloc_report = false;
    let mut mutant = false;
    let mut tiers = false;
    let mut matrix = false;
    for arg in std::env::args().skip(1) {
        if let Some(value) = arg.strip_prefix("--rounds=") {
            rounds = value.parse().map_err(|_| "rounds must be a number")?;
        } else if let Some(value) = arg.strip_prefix("--json=") {
            json = Some(value.to_string());
        } else if arg == "--alloc-report" {
            alloc_report = true;
        } else if arg == "--mutant-check" {
            mutant = true;
        } else if arg == "--tiers" {
            tiers = true;
        } else if arg == "--matrix" {
            matrix = true;
        }
    }

    if matrix {
        // The matrix runs against a scratch directory of its own, under the
        // host's own temp root rather than a path baked into the source, so it
        // is portable and leaves nothing in the working tree.
        let dir = std::env::temp_dir().join(format!("lgwks-matrix-{}", std::process::id()));
        // `workload_matrix` drives each row with its own `block_on`, so it is
        // called *outside* a runtime rather than from inside one: nesting the
        // two is the documented way to deadlock a multi-thread scheduler.
        let outcome = workload_matrix(&Runtime::new()?, &dir, json.as_deref());
        drop(std::fs::remove_dir_all(&dir));
        return outcome;
    }

    if mutant {
        // The negative control runs alone and exits: a run that also produced a
        // table would leave a reader unable to tell which number the refusal
        // referred to.
        return mutant_check(&Runtime::new()?);
    }

    if tiers {
        return tier_ladder(&Runtime::new()?, json.as_deref());
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
