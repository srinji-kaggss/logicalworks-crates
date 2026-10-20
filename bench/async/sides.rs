//! The two sides of every closed-loop comparison: the facade, the pinned raw
//! Tokio baseline, the bare floor, and the paired-round harness that gates them
//! on identical work before any ratio is printed.
//!
//! Moved verbatim from `main.rs` for #639; the items below are unchanged.

use super::*;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use lgwks_bot::rt::runtime::Runtime;
use lgwks_bot::rt::supervise::Supervisor;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::gate::fair;
use crate::support::{
    Drain, Tally, body_unit, completed_target, drain_supervisor, tokio_util_cancel, widen,
};
/// Run `total` bodies through the facade at a ceiling of `bound`.
///
/// The honest side of every comparison: same spawn loop, same body, same tally,
/// and the drain policy that waits for all of them.
pub(crate) async fn facade_side(total: usize, bound: usize) -> (f64, Tally) {
    facade_side_draining(total, bound, Drain::Complete).await
}

/// Run `total` bodies through the facade at a ceiling of `bound`, under
/// `drain`.
///
/// Reports the wall time of the whole drain and the tally the fairness gate
/// compares. Uses `Supervisor`'s bounded `spawn`, so the facade's admission
/// ceiling, its terminal outcomes and its cancellation all apply.
pub(crate) async fn facade_side_draining(total: usize, bound: usize, drain: Drain) -> (f64, Tally) {
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
    let target = completed_target(total);
    if drain == Drain::GiveUp {
        // The defect, in one line: reap whatever is ready and stop. A real
        // harness that made this mistake would report success for work it never
        // waited for.
        supervisor.reap();
    } else {
        drain_supervisor(&mut supervisor, target).await;
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
pub(crate) async fn baseline_side(total: usize, bound: usize) -> (f64, Tally) {
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
    tally.placed = widen(total);
    tally.work_units = counter.load(Ordering::SeqCst);
    tally.retained = 0;
    (elapsed, tally)
}

/// The bare synchronous floor: the same counter bump, no async at all.
///
/// Reported as its own line and never multiplied into the facade/baseline
/// ratio. It answers "what would this cost with no scheduler at all", which is
/// the floor the other two numbers are read against.
pub(crate) fn bare_floor(total: usize) -> (f64, u64) {
    let counter = Arc::new(AtomicU64::new(0));
    let started = Instant::now();
    for _ in 0..total {
        counter.fetch_add(1, Ordering::SeqCst);
    }
    let elapsed = started.elapsed().as_secs_f64();
    (elapsed, counter.load(Ordering::SeqCst))
}

/// The measured result of one scenario.
pub(crate) struct ScenarioResult {
    /// Human name.
    pub(crate) name: String,
    /// Tasks run per round.
    pub(crate) total: usize,
    /// In-flight ceiling both sides ran at.
    pub(crate) bound: usize,
    /// Facade wall seconds per round.
    pub(crate) facade: Vec<f64>,
    /// Baseline wall seconds per round.
    pub(crate) baseline: Vec<f64>,
    /// The tally both sides agreed on (checked every round).
    pub(crate) tally: Tally,
}

/// One scenario: paired rounds of facade and baseline, gated on fairness.
pub(crate) async fn measure(
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
pub(crate) fn mutant_check(runtime: &Runtime) -> Result<(), Box<dyn std::error::Error>> {
    const TASKS: usize = 512;
    const BOUND: usize = 8;
    info!("mutant baseline: a side that places every task and then stops draining");
    info!("the gate must refuse it, naming the diverging field.\n");

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
            info!("refused, as required: {reason}");
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
            info!("the refusal names a work-count field, so the gate discriminates on work");
            info!(
                "\nmutant tally:  placed {} completed {} cancelled {} aborted {} work_units {}",
                mutant.placed,
                mutant.completed,
                mutant.cancelled,
                mutant.aborted,
                mutant.work_units
            );
            info!(
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
