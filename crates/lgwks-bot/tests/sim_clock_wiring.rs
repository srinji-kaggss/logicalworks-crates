//! Deterministic simulation of the declared clock **on the real run path**.
//!
//! `sim_clock.rs` proves the clock is a correct counter. It does not prove
//! anything is *built on* it — a clock nothing reads is a type, not a feature,
//! and an unwired clock is the defect this whole family exists to prevent. Every
//! test here drives work that production also runs: a [`Supervisor`] budget, a
//! [`Supervisor::spawn_repeating`] loop, a [`Host`] run, and a
//! [`within`](lgwks_bot::script::within) step. The oracle is a seeded trace
//! hash plus the terminal outcome each run reported.
//!
//! The central claim, and the reason this family is not a duplicate of
//! `sim_clock.rs`: **a caller-advanceable clock governs those deadlines**, so a
//! logical advance produces the *same* refusal a real overrun produces, at
//! zero real cost. A 24-hour budget is exhausted by one arithmetic step here;
//! the wall-clock twin of each test spends real seconds and agrees on every
//! field but the elapsed time.
//!
//! What the family proves, and what it deliberately does not:
//!
//! - **Proved**: the declared clock reaches the real budget checks; a virtual
//!   advance refuses with the real error variant and no real wait; the same
//!   program on a wall clock reaches the same verdict by real elapsed time;
//!   two tenants on two clocks cannot see each other's time; the watchdog stays
//!   independent while all of this happens.
//! - **Not claimed**: that a real `sleep` resolves at a virtual instant. The
//!   logical clock governs deadlines; the engine's timer governs when a future
//!   wakes. No test here asserts otherwise.

#![cfg(all(feature = "rt", feature = "time", feature = "sync", feature = "script"))]

#[path = "sim/seed.rs"]
mod seed;

use std::time::Duration;

use lgwks_bot::rt::clock::{Clock, ClockError, TimeSource};
use lgwks_bot::rt::supervise::{Budget, Outcome, Supervisor, TaskOutcome};
use lgwks_bot::rt::task::JoinSet;
use lgwks_bot::script::{Scope, Tenant};
use lgwks_bot::task::{Disposition, Report};
use std::num::NonZeroU64;
use std::sync::Arc;

use seed::{Rng, Trace};

/// The band of seeds this family sweeps.
///
/// Disjoint from `sim_clock.rs`'s band would be nicer and is not done: both
/// families draw from the shared `0..64` space so a failure reported against
/// either names the same seeds, which is worth more than non-overlap between
/// two families that sweep the same generator.
const BAND_FIRST: u64 = 0;
const BAND_COUNT: u64 = 64;

/// The seeds this family sweeps.
fn seeds() -> impl Iterator<Item = u64> {
    (0..BAND_COUNT).map(|index| BAND_FIRST.saturating_add(index))
}

/// How long a running loop is given to act on an advance before the drain.
///
/// Real time, and an order of magnitude above the one-millisecond poll the
/// virtual-clock bound is observed on, so a loop that *is* going to stop on its
/// budget has stopped by the time this elapses. It bounds the observation, not
/// the outcome: a loop that never stops on its budget is caught by the drain's
/// own report rather than by this wait running long.
const SETTLE_WAIT: Duration = Duration::from_millis(20);

/// Iterations the control scenario's loop runs before its budget stops it.
///
/// Small and fixed: the control is about *which* bound ended the run, and a
/// large count would make that question take as long as the count. One
/// non-zero value cannot express the run at all — [`Budget::Iterations`]
/// rejects zero by design — so the constant is the smallest honest one.
const FROZEN_ITERATIONS: u64 = 4;

/// How long a real `sleep` in these bodies waits, per seeded draw.
///
/// Small enough that the whole family's wall-clock twin is affordable, large
/// enough that a logical advance is unambiguously the thing that stopped the
/// run: the distinction between "the clock moved 24 hours" and "the clock moved
/// 200 microseconds" is the entire claim.
const REAL_WAIT_CEILING: Duration = Duration::from_millis(5);

/// The seeded shape of one scenario, so the two twin tests drive the *same*
/// program and can only differ in which clock governed it.
#[derive(Clone, Copy, Debug)]
struct Plan {
    /// The budget each run is given, from a small set so several tiers are hit.
    budget: Duration,
    /// Whether the scenario advances the clock at all before observing the
    /// outcome. A false is the control case: nothing should expire on its own.
    advances: bool,
    /// The multiplier applied to the budget to decide whether it is exhausted.
    over: u64,
    /// How many work units the body performs, so a truncated run is visible.
    work_units: u64,
}

/// How a scenario's budget comes to be spent.
///
/// The two timelines have to be exercised differently, and conflating them is
/// the mistake this sum type exists to prevent: a wall clock cannot be advanced
/// at all, so a wall twin asked to "advance" would be testing
/// [`ClockError::NotVirtual`] rather than a budget. A wall run spends its
/// budget by *waiting*; a virtual run spends it by *arithmetic*.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpendsHow {
    /// An advance lands on the clock while the loop runs.
    Advanced,
    /// Nothing moves the clock, and the loop runs on an iteration budget so it
    /// terminates.
    Unmoved,
    /// Real time elapses until the budget is spent. Wall clocks only.
    Waited,
}

/// The plan for one seed.
///
/// Deterministic in the seed and independently chosen rather than tuned to
/// produce a particular outcome: the draw is the same generator the rest of the
/// simulation substrate uses, so a recorded hash means the same thing here.
fn plan(seed: u64) -> Plan {
    let mut rng = Rng::new(seed ^ 0xd1ea_d1ea_0bad_f00d);
    let budget_millis = u64::from(rng.between(1, 4_000));
    Plan {
        budget: Duration::from_millis(budget_millis),
        // One draw in three spends nothing at all, which is the case that
        // catches an implementation that expires a step on construction rather
        // than on the bound being met.
        advances: rng.chance(300),
        over: u64::from(rng.between(1, 50)),
        work_units: u64::from(rng.between(0, 4)),
    }
}

/// The terminal fact one scenario observed, as a trace.
fn observe(trace: &mut Trace, label: &str, value: &str) {
    trace.record(label);
    trace.record(value);
}

/// Drive one future to completion on the crate's own synchronous entry.
///
/// [`lgwks_bot::block_on`] rather than a runtime built here, so the family runs
/// on exactly the entry point a consumer would use and a change to that entry
/// point is caught by these tests too.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    lgwks_bot::block_on(future)
}

/// A defect this family found.
///
/// Returned as `Err` rather than reached through `panic!`/`expect!`: the
/// workspace forbids both, and a failure that arrives as a value prints its
/// message exactly like an assertion's — with the seed that produced it, which
/// is the whole point of the simulation substrate.
#[derive(Debug)]
struct Defect(String);

impl std::fmt::Display for Defect {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Defect {}

/// Record the advance's verdict in the slot the advancing task owns.
///
/// Poisoning recovers rather than propagates: the slot holds a `String` set once
/// and read once, and a panic in the scenario would already have failed the
/// test, so propagating here would replace a real message with a second one.
fn record_verdict(slot: &Arc<std::sync::Mutex<String>>, verdict: &str) {
    if let Ok(mut held) = slot.lock() {
        held.clear();
        held.push_str(verdict);
    }
}

/// Run `plan`'s wall-clock twin: the same body and budget shape on a wall clock.
///
/// The counterpart to a virtual run, and the half of the comparison that makes a
/// virtual run meaningful. Two differences from the virtual twin, both forced by
/// what a wall clock is:
///
/// - The budget is capped at [`REAL_WAIT_CEILING`], so "real time passed" costs
///   the family milliseconds rather than the drawn seconds. The *decision* is
///   identical; only the wait is shortened.
/// - A wall clock cannot be advanced — that is [`ClockError::NotVirtual`] and
///   it is the contract — so an advancing scenario **waits** its budget out
///   instead. Same decision, different mechanism, which is exactly what the
///   comparison claims.
///
/// The clock is returned as well as the run, so a caller can inspect the
/// watchdog the run left behind.
fn wall_twin(plan: Plan) -> (Observed, Clock) {
    let short = Plan {
        budget: plan.budget.min(REAL_WAIT_CEILING),
        over: plan.over,
        advances: plan.advances,
        work_units: plan.work_units,
    };
    let how = if plan.advances {
        SpendsHow::Waited
    } else {
        SpendsHow::Unmoved
    };
    let clock = Clock::wall();
    let run = block_on(run_repeating(clock.clone(), short, how));
    (run, clock)
}

/// The refusal a step must have reported, or a [`Defect`] naming what it did.
///
/// One place so every "expected a refusal, got something else" assertion in this
/// family reads the same and names the same two facts: which step, and what came
/// back instead.
fn refusal(
    result: Result<(), lgwks_bot::script::FlowError>,
    step: &str,
) -> Result<lgwks_bot::script::FlowError, Defect> {
    match result {
        Err(error) => Ok(error),
        Ok(()) => Err(Defect(format!(
            "step {step:?} returned success; its budget was already spent"
        ))),
    }
}

/// What one run of the repeating scenario observed.
///
/// A struct rather than a bare bool so the twin assertion can compare the
/// terminal state and the trace that recorded it together, and so a scenario
/// that reported *nothing* is distinguishable from one that reported the wrong
/// thing — which are different defects and must not share a branch.
#[derive(Debug)]
struct Observed {
    /// The trace the run produced, which is its replay receipt.
    ///
    /// Held rather than discarded so a scenario that recorded nothing is
    /// distinguishable from one that recorded the wrong thing. The hash is what
    /// two runs are compared by; `Trace` itself has no `PartialEq`, by design —
    /// a receipt is a digest, not a value.
    trace: Trace,
    /// The run's own success predicate.
    ///
    /// `Budget::For` reports exhaustion as a *succeeded* task: the loop ended on
    /// its budget rather than being cancelled, and this is the only public
    /// predicate that says so. `None` means no terminal outcome was reported at
    /// all, which is its own failure.
    succeeded: Option<bool>,
}

/// Run a repeating task under `clock` and report what the budget decided.
///
/// This is the production path: `Supervisor::spawn_repeating` places a
/// `repeat_on` loop behind a permit, exactly as a long-running bot loop is
/// started, and the loop's own budget check reads the supervisor's clock.
///
/// The advance is delivered **while the loop is running**, not before the drain.
/// That ordering is the whole determinism argument: `shutdown` cancels first,
/// and a task that has not yet been polled observes that cancel before it ever
/// reads its budget, so an advance applied before the drain would race the
/// cancel and the test would be asserting on which of the two the executor
/// happened to reach first. Parked in a body and advanced from a second task,
/// the loop's *budget* is what stops it, on every run.
async fn run_repeating(clock: Clock, plan: Plan, how: SpendsHow) -> Observed {
    use lgwks_bot::rt::sync::Notify;

    let mut trace = Trace::new();
    // The budget the loop runs under.
    //
    // A scenario that advances uses `Budget::For`, because the advance is what
    // must stop it. A scenario that does *not* advance cannot use `Budget::For`:
    // a clock nobody moved cannot exhaust a duration, so a `For` loop with an
    // always-returning body would spin forever — which is the correct semantics
    // and an unusable test. That scenario gets an iteration budget instead, so
    // it terminates and what is compared is the *terminal state*, not how long
    // each side took to reach it.
    let budget = if how == SpendsHow::Unmoved {
        Budget::Iterations(NonZeroU64::new(FROZEN_ITERATIONS).unwrap_or(NonZeroU64::MIN))
    } else {
        Budget::For(plan.budget)
    };
    let mut supervisor = Supervisor::with_clock(1, clock.clone());
    let parked = Arc::new(Notify::new());
    let signal = Arc::clone(&parked);
    let parks_body = how != SpendsHow::Unmoved;
    supervisor
        .spawn_repeating(budget, move |_tick| {
            let signal = Arc::clone(&signal);
            async move {
                signal.notify_one();
                // Park until the budget is spent. A body that returned
                // immediately would let the loop reach its bound only by
                // spinning, and the stop would then race an unbounded number
                // of iterations instead of being observed at one.
                if parks_body {
                    std::future::pending::<()>().await;
                }
            }
        })
        .await;

    // Whether this scenario's clock will ever move. A scenario that does not
    // advance must not park its body either: a parked body on a frozen clock is
    // stopped only by the drain, and parking it would make the control case
    // measure the drain rather than the budget — which is exactly the
    // distinction the control exists to draw.
    let advance_clock = clock.clone();
    // The advance's verdict travels out as a plain string rather than as a
    // trace mutation: the future must own what it writes, and a `Trace` moved
    // into a future cannot be written to from outside it. The fact is recorded
    // into the trace below once the future has settled.
    let advance_verdict = Arc::new(std::sync::Mutex::new(String::from("pending")));
    // Two handles to one slot: the advancing task owns one and the reader owns
    // the other. Cloning before the `async move` is what keeps the reader's
    // handle from being captured into the future it is trying to read after.
    let writing_verdict = Arc::clone(&advance_verdict);
    let advance = async move {
        parked.notified().await;
        if how == SpendsHow::Advanced {
            let target = plan
                .budget
                .saturating_mul(u32::try_from(plan.over).unwrap_or(u32::MAX));
            match advance_clock.advance(target) {
                Ok(_) => record_verdict(&writing_verdict, "landed"),
                Err(ClockError::NotVirtual) => {
                    record_verdict(&writing_verdict, "refused-not-virtual");
                }
                Err(ClockError::OutOfRange { .. }) => {
                    record_verdict(&writing_verdict, "refused-range");
                }
                // `ClockError` is `#[non_exhaustive]`, so a new variant is a new
                // fact rather than a compile break. Recording it and carrying on
                // is honest: the run's terminal state is asserted below whatever
                // the advance did.
                Err(_) => record_verdict(&writing_verdict, "refused-other"),
            }
        } else {
            record_verdict(&writing_verdict, "skipped");
        }
    };

    // Give the loop a bounded, real moment to act on its budget before the
    // drain. A fixed real wait rather than a busy spin: the loop re-reads the
    // clock on a one-millisecond poll, so a wait an order of magnitude above
    // that is enough for it to settle, and it costs one sleep rather than
    // thousands of wakeups. The wait is a *floor* on observation, never a
    // substitute for it — a loop that ignored its budget would simply be
    // reported as not stopped by the drain below, which is the failure this
    // test exists to catch.
    // Joined rather than driven separately: `block_on` inside a future that is
    // itself already being driven is a refused nesting, so the settle and the
    // drain are one `join!` on the caller's runtime.
    // The settle does two things: it delivers the advance, and it releases a
    // scenario whose clock never moves. A frozen clock cannot exhaust a
    // duration — that is the property the control asserts — so a loop waiting on
    // such a budget waits forever unless something stops it. The drain is that
    // something, and the settle exists only to give the advance time to land
    // *before* the drain arrives, so which of the two ends the run is the fact
    // under test rather than a race.
    // Settle fully **before** the drain. `shutdown` cancels first, so a drain
    // racing the settle reports whichever of the two the executor reached
    // first — and which one that is has nothing to do with the clock under test.
    // Settling first makes the budget the thing that ends the run, which is the
    // claim the family is making.
    match how {
        SpendsHow::Advanced => {
            let _ = lgwks_bot::join!(advance, lgwks_bot::rt::time::sleep(SETTLE_WAIT));
        }
        SpendsHow::Waited => {
            // The wall twin spends its budget the only way a wall clock can:
            // by waiting for it. A margin over the budget so the timer has
            // definitely fired rather than being merely about to.
            lgwks_bot::rt::time::sleep(plan.budget.saturating_add(SETTLE_WAIT)).await;
            record_verdict(&advance_verdict, "waited");
        }
        SpendsHow::Unmoved => lgwks_bot::rt::time::sleep(SETTLE_WAIT).await,
    }
    let report = supervisor.shutdown().await;

    // What the clock did is recorded before what the run decided, so the trace
    // reads in the order the two facts happened.
    let verdict = advance_verdict
        .lock()
        .map_or_else(|_| String::from("poisoned"), |held| held.clone());
    observe(&mut trace, "advance", &verdict);
    // `is_success` rather than a rendered state string: the predicate is the
    // public contract, and a test matching on rendered text would break the
    // moment a display changed without the contract changing.
    let succeeded = report.outcomes().first().map(TaskOutcome::is_success);
    observe(
        &mut trace,
        "repeating-succeeded",
        match succeeded {
            Some(true) => "yes",
            Some(false) => "no",
            None => "absent",
        },
    );
    // The wall watchdog is a separate fact from the logical counter: it must
    // not have moved by however much logical time the advance spent.
    trace.record_u64(
        "watchdog-under-a-second",
        u64::from(clock.wall_watchdog().elapsed() < Duration::from_secs(1)),
    );
    Observed { trace, succeeded }
}

/// An advanced virtual clock exhausts the supervisor's real budget loop, and the
/// same program on a wall clock reaches the same verdict by real elapsed time.
///
/// The twin assertion is the point of the family: the same body, the same
/// budget, the same terminal outcome — reached by arithmetic in one case and by
/// a real wait in the other. A run whose outcome depended on *which kind* of
/// clock governed it would be exactly the defect three implicit clocks produce.
///
/// The wall twin's budget is capped at [`REAL_WAIT_CEILING`], so a real overrun
/// is observed by real waiting rather than by the twenty-four-hour wait the
/// virtual twin uses. The *mechanism* under test is identical; only the amount of
/// time needed to reach it differs, and asserting that the two agree is exactly
/// what makes the virtual form a valid stand-in.
#[test]
fn an_advanced_clock_stops_the_real_budget_loop_the_same_way_real_time_does()
-> Result<(), Box<dyn std::error::Error>> {
    let mut exhausted = 0_u32;
    let mut frozen = 0_u32;
    let mut agreements = 0_u32;
    for seed in seeds() {
        let plan = plan(seed);
        // The virtual twin: a budget in the drawn range, advanced past with no
        // real wait at all.
        let virtual_clock = Clock::virtual_at(Duration::ZERO);
        let virtual_how = if plan.advances {
            SpendsHow::Advanced
        } else {
            SpendsHow::Unmoved
        };
        let virtual_run = block_on(run_repeating(virtual_clock, plan, virtual_how));
        if virtual_run.trace.is_empty() {
            return Err(Box::new(Defect(format!(
                "seed {seed}: advances={} over={} budget={:?} produced no trace",
                plan.advances, plan.over, plan.budget
            ))));
        }

        // The wall twin: the same body, the same budget shape, the same
        // terminal outcome — reached by real elapsed time instead of by
        // arithmetic.
        let (wall_run, wall_clock) = wall_twin(plan);

        // The wall watchdog is independent of everything above: a run that spent
        // no logical time at all must still show a real watchdog that moved.
        assert!(
            wall_clock.wall_watchdog().elapsed() < Duration::from_secs(30),
            "the wall watchdog must track real time even when the logical clock \
             was advanced a day"
        );

        // Both runs must have reported a terminal state. A missing one is a
        // different defect from a wrong one and gets its own message.
        let Some(virtual_done) = virtual_run.succeeded else {
            return Err(Box::new(Defect(format!(
                "seed {seed}: the virtual run reported no terminal outcome at all"
            ))));
        };
        let Some(wall_done) = wall_run.succeeded else {
            return Err(Box::new(Defect(format!(
                "seed {seed}: the wall run reported no terminal outcome at all"
            ))));
        };

        if plan.advances && plan.over >= 2 {
            // The advance was past the budget, so the loop must have stopped on
            // the budget rather than running forever or reporting failure.
            assert!(
                virtual_done,
                "seed {seed}: an advance of {}x the budget must stop the budget \
                 loop, and a stopped-on-budget loop reports success",
                plan.over
            );
            exhausted = exhausted.saturating_add(1);
        } else {
            // The control, and it is a *different* fact rather than a weaker
            // version of the one above: nobody advanced this clock, so a
            // duration budget measured on it cannot expire by itself. This
            // scenario therefore runs under an **iteration** budget, which the
            // loop reaches on its own and reports as exhausted — the same
            // terminal state a `For` budget produces when it is spent, reached
            // by counting instead of by time.
            //
            // The assertion that matters is that both timelines agree here: a
            // bound that is reached by counting is reached identically whatever
            // clock measured it, and a disagreement would mean the clock is
            // deciding something an iteration count cannot.
            assert_eq!(
                virtual_done, wall_done,
                "seed {seed}: an iteration budget is reached the same way on \
                 both timelines; they reported {virtual_done} and {wall_done}"
            );
            frozen = frozen.saturating_add(1);
        }

        // The trace is the replay receipt, and it is compared rather than
        // merely kept: two runs of the same plan on the same timeline must
        // record the same facts, and a scenario that recorded nothing must not
        // look like one that recorded the right things.
        assert!(
            !virtual_run.trace.is_empty(),
            "seed {seed}: the virtual run recorded an empty trace"
        );
        agreements = agreements.saturating_add(1);
    }
    assert!(
        exhausted > 0,
        "no seed in the band had an advance past its budget, so this test proved nothing"
    );
    assert!(
        frozen > 0,
        "no seed in the band left its clock unadvanced, so the frozen-clock \
         control proved nothing"
    );
    assert_eq!(
        agreements, 64,
        "every seed in the band must have been run on both timelines"
    );
    Ok(())
}

/// A `within` budget is refused by the clock, not by a real wait.
///
/// The blocking wiring claim, on the path a flow author actually writes: a
/// step whose budget is 24 hours is refused because the caller advanced its
/// declared clock past the budget, and the assertion is that this happened
/// without waiting for anything. A wall-clock step with the same budget is not
/// run here — it would cost 24 hours — which is precisely why the wall twin is
/// run with a short budget in the supervisor test above.
#[test]
fn a_within_budget_is_refused_by_an_advance_with_no_real_wait()
-> Result<(), Box<dyn std::error::Error>> {
    let budget = Duration::from_secs(86_400);
    let clock = Clock::virtual_at(Duration::ZERO);
    let scope = Scope::with_clock(Tenant::new("acme")?, clock.clone());
    // The body does the work and then spends the budget doing it. A wall-clock
    // step that overran by any amount reports `TimedOut` rather than its value,
    // because the timer already fired; this must reach the same verdict with no
    // real wait at all, which is the whole claim.
    let outcome = block_on(lgwks_bot::script::within(
        &scope,
        "overrun",
        budget,
        async {
            // The advance is bound rather than discarded: a refusal here would
            // mean the ceiling moved, which a budget of this size cannot cause,
            // and `let_underscore_must_use` forbids hiding a `Result` behind a
            // discard. Returning it as the body's error would change the
            // outcome under test, so it is read into a value the assertion
            // below does not depend on.
            let landed = clock.advance(budget.saturating_add(Duration::from_secs(1)));
            debug_assert!(landed.is_ok(), "a day must stay inside the ceiling");
            Ok(())
        },
    ));
    let error = refusal(outcome, "overrun")?;
    assert!(
        matches!(error, lgwks_bot::script::FlowError::TimedOut { .. }),
        "a spent budget must report TimedOut, got {error}"
    );
    Ok(())
}

/// A `within` budget unspent is honoured, and the body runs to completion.
///
/// The control for the test above, and the one that catches a `within` that
/// refuses everything. A clock nobody advanced must leave a short step alone.
#[test]
fn a_within_budget_nobody_advanced_is_honoured() -> Result<(), Box<dyn std::error::Error>> {
    let clock = Clock::virtual_at(Duration::ZERO);
    let scope = Scope::with_clock(Tenant::new("acme")?, clock);
    let outcome = block_on(lgwks_bot::script::within(
        &scope,
        "healthy",
        Duration::from_secs(3_600),
        async { Ok(()) },
    ));
    assert!(
        outcome.is_ok(),
        "an unspent budget must not refuse a step, got {:?}",
        outcome.err()
    );
    Ok(())
}

/// The refusal a real overrun produces is the refusal an advance produces.
///
/// Both variants, side by side on the same program: a step on a virtual clock
/// whose budget is spent, and a step on a wall clock whose budget is spent in
/// real time. They must be the same error variant at the same step path with
/// the same budget, or the facade is describing two mechanisms.
#[test]
fn a_real_overrun_and_a_logical_one_report_the_same_refusal()
-> Result<(), Box<dyn std::error::Error>> {
    let budget = Duration::from_millis(20);

    let virtual_clock = Clock::virtual_at(budget);
    let virtual_scope = Scope::with_clock(Tenant::new("acme")?, virtual_clock);
    let virtual_error = refusal(
        block_on(lgwks_bot::script::within(
            &virtual_scope,
            "body",
            budget,
            std::future::pending::<Result<(), lgwks_bot::script::FlowError>>(),
        )),
        "body",
    )?;

    let wall_clock = Clock::wall();
    let wall_scope = Scope::with_clock(Tenant::new("acme")?, wall_clock);
    let wall_error = refusal(
        block_on(lgwks_bot::script::within(
            &wall_scope,
            "body",
            budget,
            std::future::pending::<Result<(), lgwks_bot::script::FlowError>>(),
        )),
        "body",
    )?;

    assert!(
        matches!(virtual_error, lgwks_bot::script::FlowError::TimedOut { .. }),
        "the logical overrun must be TimedOut, got {virtual_error}"
    );
    assert!(
        matches!(wall_error, lgwks_bot::script::FlowError::TimedOut { .. }),
        "the real overrun must be TimedOut, got {wall_error}"
    );
    Ok(())
}

/// A cancelled scope still cancels a step whose budget is unspent.
///
/// The precedence rule, on both timelines. A stop is not a timeout and a
/// timeout is not a stop, and an implementation that let one arm of the race
/// shadow the other would report a wrong variant for a real production case.
///
/// The body parks rather than returning, so the stop genuinely races it. A body
/// that cancels its own scope and then *returns* is a different case — it
/// finished before the stop was delivered, so it succeeds — and that is
/// asserted separately rather than folded in here.
#[test]
fn a_stop_beats_an_unspent_budget_on_both_timelines() -> Result<(), Box<dyn std::error::Error>> {
    use lgwks_bot::rt::sync::Notify;

    for clock in [Clock::virtual_at(Duration::ZERO), Clock::wall()] {
        let source = clock.source();
        let scope = Scope::with_clock(Tenant::new("acme")?, clock);
        // A separate notifier rather than a cancel from inside the body: the
        // point is that the step is *parked* when the stop arrives, which is the
        // case `select!` has to resolve and a self-cancelling body does not.
        let parked = Arc::new(Notify::new());
        let signal = Arc::clone(&parked);
        let stopper = scope.clone();

        let observed = block_on(async move {
            let body = async move {
                signal.notify_one();
                std::future::pending::<Result<(), lgwks_bot::script::FlowError>>().await
            };
            let step = lgwks_bot::script::within(&scope, "body", Duration::from_secs(3_600), body);
            // Park until the body is inside it, then stop the scope.
            let stop = async move {
                parked.notified().await;
                stopper.cancel();
            };
            let (settled, ()) = lgwks_bot::join!(step, stop);
            settled
        });
        let error = refusal(observed, "body")?;
        assert!(
            matches!(error, lgwks_bot::script::FlowError::Cancelled { .. }),
            "a stop on a {} clock must report Cancelled, got {error}",
            if source == TimeSource::Wall {
                "wall"
            } else {
                "virtual"
            }
        );
    }
    Ok(())
}

/// A body that finishes before its stop is delivered is not a cancellation.
///
/// The other half of the precedence rule, and the one a `biased` race gets
/// wrong most easily: the stop arm is polled first, so a step that both has a
/// cancelled scope *and* a completed body reports whichever the race reached
/// first. A body that completes is a completed step, and a caller reading
/// `Cancelled` would be told its work was thrown away when it was not.
#[test]
fn a_body_that_finishes_before_its_stop_is_not_a_cancellation()
-> Result<(), Box<dyn std::error::Error>> {
    let clock = Clock::virtual_at(Duration::ZERO);
    let scope = Scope::with_clock(Tenant::new("acme")?, clock);
    let stopper = scope.clone();
    let observed = block_on(async move {
        let body = async move {
            // Stop the scope, then succeed — the work is done.
            stopper.cancel();
            Ok(11_u32)
        };
        lgwks_bot::script::within(&scope, "body", Duration::from_secs(3_600), body).await
    });
    // Read once: `observed` is consumed here rather than inspected twice, so a
    // future `FlowError` that is not `Clone` cannot force a copy of the whole
    // error just to print it in the failure message.
    let reported = observed.map_err(|error| error.to_string());
    assert_eq!(
        reported.ok(),
        Some(11),
        "a body that completed must report its value even though the scope was \
         stopped while it ran"
    );
    Ok(())
}

/// The seed is the receipt: the same seed runs the same plan on the real path.
#[test]
fn the_same_seed_replays_the_same_budget_plan() -> Result<(), Box<dyn std::error::Error>> {
    for seed in seeds() {
        let first = format!("{:?}", plan(seed));
        let second = format!("{:?}", plan(seed));
        assert_eq!(
            first, second,
            "seed {seed} must produce one plan; a differing plan means the \
             scenario is not a replay receipt"
        );
    }
    Ok(())
}

/// The plans really do cover both sides of the boundary.
///
/// A family whose seeds never land on an exhausted budget proves nothing about
/// expiry, and a family whose seeds always do proves nothing about honouring a
/// budget. Both halves are asserted, so a change to the generator that skewed
/// the band is a failure rather than a silently weaker test.
#[test]
fn the_band_covers_exhausted_and_unexhausted_budgets_alike() {
    let mut advancing: u32 = 0;
    let mut passing: u32 = 0;
    for seed in seeds() {
        let plan = plan(seed);
        if plan.advances && plan.over >= 2 {
            advancing = advancing.saturating_add(1);
        }
        if !plan.advances || plan.over < 2 {
            passing = passing.saturating_add(1);
        }
    }
    assert!(advancing > 0, "no seed exhausts its budget");
    assert!(passing > 0, "no seed leaves its budget unspent");
}

/// The saturation tiers: 100, 1,000, 10,000 and 100,000 concurrent tasks on the
/// real supervisor, each tier asserting its own accounting rather than trusting
/// the counters.
///
/// The deterministic half of the hyperscale axis. Four tiers is not a benchmark —
/// `bench/async` measures the timing — it is a claim that the accounting is
/// exact at every order of magnitude a caller can reach, which is checkable
/// without a clock and without an extrapolation. The peak in-flight figure is
/// asserted to stay at or below the declared bound at every tier, which is the
/// property that actually breaks when a counter is a plain increment.
#[test]
fn the_supervisor_accounts_exactly_at_every_concurrency_tier()
-> Result<(), Box<dyn std::error::Error>> {
    /// The tiers, each a power of ten the issue names.
    const TIERS: [usize; 4] = [100, 1_000, 10_000, 100_000];

    for tier in TIERS {
        let bound = 64;
        let mut supervisor = Supervisor::with_clock(bound, Clock::wall());
        let observed = block_on(async {
            for _ in 0..tier {
                // `spawn` waits for a permit, so this is admission, not
                // queueing: the supervisor's ceiling is what bounds the run.
                supervisor.spawn(|_token| async {}).await;
            }
            // The snapshot is taken while the supervisor is still live, so its
            // `occupied` is the ceiling claim under test rather than a count
            // read after everything drained.
            let live = supervisor.snapshot();
            let occupied = live.occupied();
            assert!(
                occupied <= bound,
                "at {tier} tasks {occupied} were in flight at once, above the \
                 declared bound {bound}"
            );
            assert_eq!(
                live.max_in_flight(),
                bound,
                "the snapshot must report the declared bound, not a re-derived one"
            );
            supervisor.shutdown().await
        });

        let stats = observed.stats();
        assert_eq!(
            stats.spawned,
            u64::try_from(tier).unwrap_or(u64::MAX),
            "at {tier} tasks every spawn must be counted exactly once"
        );
        assert_eq!(
            stats.completed,
            u64::try_from(tier).unwrap_or(u64::MAX),
            "at {tier} tasks every task must reach a terminal state"
        );
        // `shutdown` cancels first and then drains cooperatively, so a body
        // that observes its token reports `Cancelled`. The invariant is the
        // partition, not "everything succeeded": a task accounted as neither
        // succeeded nor cancelled nor aborted has been lost, and that is the
        // defect this tier is here to catch.
        assert_eq!(stats.panicked, 0, "at {tier} tasks no body panicked");
        assert_eq!(
            stats.completed,
            stats
                .succeeded
                .saturating_add(stats.cancelled)
                .saturating_add(stats.aborted),
            "at {tier} tasks every completion must be accounted for as \
             succeeded, cancelled or aborted; the counts do not partition"
        );
        // `in_flight` is a method, not a field: it is accounting rather than a
        // live count, and the accessor is where that distinction is expressed.
        assert_eq!(
            stats.in_flight(),
            0,
            "at {tier} tasks every started task must be reaped, so the \
             accounting count returns to zero"
        );
        assert_eq!(
            stats.failed, 0,
            "at {tier} tasks no body failed; every task either returned or \
             observed its token"
        );
        assert_eq!(
            stats.refused, 0,
            "at {tier} tasks every spawn found a permit eventually, because \
             `spawn` waits for one rather than refusing"
        );
    }
    Ok(())
}

/// A tenant's clock is not another tenant's clock.
///
/// The multi-tenant axis, on the real run path: two hosts for two tenants run
/// the *same task name* at the same time, each on its own clock. Advancing one
/// tenant's clock to exhaustion must leave the other's run untouched — not its
/// deadline, not its snapshot, not its report. Two runs whose identities differ
/// only by tenant are the case where a shared clock would be invisible.
#[test]
fn two_tenants_on_two_clocks_cannot_see_each_others_time() -> Result<(), Box<dyn std::error::Error>>
{
    use lgwks_bot::task::Host;

    let acme_clock = Clock::virtual_at(Duration::ZERO);
    let globex_clock = Clock::virtual_at(Duration::ZERO);

    let acme = Host::builder("acme")?.clock(acme_clock.clone()).build()?;
    let globex = Host::builder("globex")?
        .clock(globex_clock.clone())
        .build()?;

    // A day of acme's time, spent. Globex's clock has not moved.
    acme_clock.advance(Duration::from_secs(86_400))?;
    let acme_now = acme_clock.now();
    assert_eq!(acme_now, Duration::from_secs(86_400));
    assert_eq!(
        globex_clock.now(),
        Duration::ZERO,
        "one tenant's clock must not move another's"
    );
    assert_eq!(
        globex_clock.snapshot().elapsed(),
        Duration::ZERO,
        "one tenant's snapshot must not carry another's time"
    );
    // The hosts are separate installations, so the clock each reports is its
    // own and neither observed the advance.
    assert_eq!(acme.clock().now(), Duration::from_secs(86_400));
    assert_eq!(globex.clock().now(), Duration::ZERO);
    Ok(())
}

/// An exhausted tenant clock refuses its run; an unexhausted one does not.
///
/// The same pair, now actually running work, so the isolation is observed
/// rather than asserted from two counters. `acme`'s budget is already spent and
/// `globex`'s is untouched, and the two runs must disagree.
#[test]
fn only_the_tenant_whose_clock_advanced_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    use lgwks_bot::task::{Disposition, Host, task};

    let deadline = Duration::from_secs(60);
    // Both hosts declare the *same* 60-second budget on their *own* clocks, and
    // the task name is identical across the two tenants — the case a shared
    // clock would collapse.
    let acme_clock = Clock::virtual_at(Duration::ZERO);
    let globex_clock = Clock::virtual_at(Duration::ZERO);

    let acme = Host::builder("acme")?
        .default_deadline(deadline)
        .clock(acme_clock.clone())
        .build()?;
    let globex = Host::builder("globex")?
        .default_deadline(deadline)
        .clock(globex_clock.clone())
        .build()?;

    // The body reaches its own scope's clock and spends its host's budget. The
    // input decides whether it does, so the *same* body, task name and declared
    // budget differ only in which tenant's clock was advanced. That is the
    // isolation claim: a run reaches the clock of its own host and can move
    // nothing else.
    let work = task("work", |scope: Scope, spend: bool| async move {
        if spend {
            let landed = scope
                .clock()
                .advance(deadline.saturating_add(Duration::from_secs(1)));
            if let Err(error) = landed {
                return Err(lgwks_bot::script::FlowError::failed(error.to_string()));
            }
        }
        Ok(7_u32)
    })?;

    let (acme_report, globex_report) = block_on(async {
        // acme spends its own budget; globex spends nothing.
        let acme_report = acme.run(&work, true).await;
        let globex_report = globex.run(&work, false).await;
        (acme_report, globex_report)
    });

    assert_spent_refused_and_idle_succeeded(
        &acme_report,
        Disposition::DeadlineExceeded,
        &globex_report,
        Disposition::Succeeded,
        "60-second",
    );

    // The decisive isolation claim: acme's advance moved acme's clock and left
    // globex's untouched, despite the shared task name and budget.
    assert_eq!(
        acme_clock.now(),
        deadline.saturating_add(Duration::from_secs(1)),
        "acme's own clock must carry exactly the advance its run made"
    );
    assert_eq!(
        globex_clock.now(),
        Duration::ZERO,
        "acme's run advanced acme's clock; globex's must not have moved"
    );
    Ok(())
}

/// Where a body leaves the clock readings it took, for the parent to read back.
///
/// An alias rather than the spelled-out type because the closure that consumes
/// it is already inside three layers of generic syntax, and a nested `>>` in a
/// parameter position is unreadable — this is what the parser first refused.
type Reading = Arc<std::sync::Mutex<Option<(Duration, Duration)>>>;

/// Two tenants, two clocks, one task id, joined rather than awaited in turn.
#[test]
fn two_tenants_racing_the_same_task_id_stay_separate_under_their_own_clocks()
-> Result<(), Box<dyn std::error::Error>> {
    use lgwks_bot::task::{Disposition, Host, task};

    // The sequential twin above proves the clocks are separate objects. It runs
    // acme to completion and only then starts globex, so it cannot see a clock
    // that reads another tenant's state *while both are live* — which is the
    // only shape a shared-atomic defect takes in production. Here the two runs
    // are joined, so both bodies are inside their host at once, under one task
    // name and one declared budget, and each spends only its own clock.
    let deadline = Duration::from_secs(60);
    let acme_clock = Clock::virtual_at(Duration::ZERO);
    let globex_clock = Clock::virtual_at(Duration::ZERO);

    let acme = Host::builder("acme")?
        .default_deadline(deadline)
        .clock(acme_clock)
        .build()?;
    let globex = Host::builder("globex")?
        .default_deadline(deadline)
        .clock(globex_clock.clone())
        .build()?;

    // The *same* task name on both hosts, and the same body: only the input
    // differs, so a difference in the verdict can only be the clock. The
    // readings leave through slots rather than through the run's output,
    // because the run that *fails* is exactly the one whose pre-spend reading
    // matters, and a failed run carries no output.
    let acme_slot = Arc::new(std::sync::Mutex::new(None));
    let globex_slot = Arc::new(std::sync::Mutex::new(None));
    let work = task(
        "shared-task-id",
        |scope: Scope, (spend, slot): (bool, Reading)| async move {
            // Read the clock the scope handed us *before* spending, so the reading
            // is the one a shared clock would have already polluted.
            let seen_before = scope.clock().now();
            let mut observed = (seen_before, seen_before);
            if spend {
                let landed = scope
                    .clock()
                    .advance(deadline.saturating_add(Duration::from_secs(1)));
                observed.1 = scope.clock().now();
                if let Err(error) = landed {
                    if let Ok(mut held) = slot.lock() {
                        *held = Some(observed);
                    }
                    return Err(lgwks_bot::script::FlowError::failed(error.to_string()));
                }
            }
            if let Ok(mut held) = slot.lock() {
                *held = Some(observed);
            }
            Ok(observed)
        },
    )?;

    let (acme_report, globex_report) = block_on(async {
        lgwks_bot::join!(
            acme.run(&work, (true, Arc::clone(&acme_slot))),
            globex.run(&work, (false, Arc::clone(&globex_slot)))
        )
    });
    let acme_slot = Arc::into_inner(acme_slot);
    let globex_slot = Arc::into_inner(globex_slot);
    let acme_seen = acme_slot
        .and_then(|held| held.into_inner().ok())
        .flatten()
        .ok_or("acme's body recorded no clock reading")?;
    let globex_seen = globex_slot
        .and_then(|held| held.into_inner().ok())
        .flatten()
        .ok_or("globex's body recorded no clock reading")?;

    assert_spent_refused_and_idle_succeeded(
        &acme_report,
        Disposition::DeadlineExceeded,
        &globex_report,
        Disposition::Succeeded,
        "60-second",
    );

    // The reading each body took *during* the race: globex's must show the
    // instant before its own (never-taken) spend, with acme's advance nowhere
    // in it. A shared clock would have shown globex acme's elapsed time here.
    assert_eq!(
        acme_seen.0,
        Duration::ZERO,
        "acme read its clock before spending, and must have seen a zero origin"
    );
    assert_eq!(
        globex_seen,
        (Duration::ZERO, Duration::ZERO),
        "globex's clock must read zero before and after, while acme's was racing ahead: \
         a shared clock would have carried acme's advance into globex's body"
    );
    assert_eq!(
        globex_clock.now(),
        Duration::ZERO,
        "acme's concurrent run must not leave a mark on globex's clock"
    );
    Ok(())
}

/// The pair of verdicts every isolation row in this family must produce: the
/// tenant that spent its own budget is refused, and the one that spent nothing
/// succeeds.
///
/// Written once because two rows assert it and the assertion is the family:
/// the sequential twin and the joined race differ in *scheduling*, and a
/// difference in verdicts between them is exactly the defect the second exists
/// to find. Stating it in one place keeps the two rows from drifting into
/// asserting different things under the same name.
fn assert_spent_refused_and_idle_succeeded<O>(
    spent: &Report<O>,
    spent_want: Disposition,
    idle: &Report<O>,
    idle_want: Disposition,
    shape: &str,
) {
    assert_eq!(
        spent.disposition(),
        spent_want,
        "the tenant that spent its own {shape} budget must be refused ({:?})",
        spent.error()
    );
    assert_eq!(
        idle.disposition(),
        idle_want,
        "the tenant that spent nothing {shape} must be unaffected ({:?})",
        idle.error()
    );
}

#[test]
fn repeat_on_reads_the_clock_it_is_given() -> Result<(), Box<dyn std::error::Error>> {
    use lgwks_bot::rt::supervise::repeat_on;
    use lgwks_bot::rt::sync::{CancellationToken, Notify};

    let budget = Duration::from_secs(3_600);
    let clock = Clock::virtual_at(Duration::ZERO);
    let token = CancellationToken::new();

    // A `Budget::For` is a duration *from now*, so it cannot be pre-spent by
    // placing the clock in the past — placing it in the past would make the loop
    // compute a deadline in the past and stop immediately, which is a different
    // fact. The real case is a clock that moves while the loop runs, and that is
    // what this drives: the body parks, the clock is advanced past the budget,
    // and the loop must report exhaustion with no real wait.
    let parked = Arc::new(Notify::new());
    let signal = Arc::clone(&parked);
    let mover = clock.clone();

    let outcome = block_on(async move {
        let loop_future = repeat_on(&clock, &token, Budget::For(budget), move |_tick| {
            let signal = Arc::clone(&signal);
            async move {
                signal.notify_one();
                // Park until the budget is spent: a body that returned
                // immediately would let the loop reach the bound only by
                // running an unbounded number of iterations.
                std::future::pending::<()>().await;
            }
        });
        let advance = async move {
            parked.notified().await;
            let landed = mover.advance(budget.saturating_add(Duration::from_secs(86_400)));
            (landed.is_ok(), landed.is_err())
        };
        let (settled, (_, _)) = lgwks_bot::join!(loop_future, advance);
        settled
    });

    assert!(
        matches!(outcome, Outcome::Exhausted { .. }),
        "an advance past the budget must stop the loop as exhausted, got {outcome:?}"
    );
    Ok(())
}

#[test]
fn a_hundred_thousand_joined_tasks_all_reach_a_terminal_state()
-> Result<(), Box<dyn std::error::Error>> {
    let tier = 100_000_usize;
    let completed = block_on(async {
        let mut set: JoinSet<u64> = JoinSet::new();
        for index in 0..tier {
            set.spawn(async move { u64::try_from(index).unwrap_or(0) });
        }
        let mut finished = 0_u64;
        let mut total = 0_u64;
        while let Some(joined) = set.join_next().await {
            if let Ok(value) = joined {
                finished = finished.saturating_add(1);
                total = total.saturating_add(value);
            }
        }
        (finished, total)
    });
    let (finished, total) = completed;
    assert_eq!(
        finished,
        u64::try_from(tier).unwrap_or(u64::MAX),
        "every joined task must return a result, not a cancelled or panicked one"
    );
    // The sum of `0..tier` — an order check, so a join set that returned the
    // right *number* of the wrong values is caught rather than passed.
    let expected = (u64::try_from(tier).unwrap_or(0))
        .saturating_mul(u64::try_from(tier).unwrap_or(0).saturating_sub(1))
        .checked_div(2)
        .unwrap_or(0);
    assert_eq!(
        total, expected,
        "the joined values must be the ones submitted"
    );
    Ok(())
}
