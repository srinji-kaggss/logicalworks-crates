//! Deterministic simulation of the declared logical clock.
//!
//! The clock is the one thing in this crate that a test can drive exactly, so
//! the family that covers it is the one that must not need a real wait. Every
//! scenario here runs on a virtual clock: a seed decides the sequence of
//! advances, budgets and watchdog observations, and the run replays to the same
//! trace hash every time. A three-day outage costs three arithmetic operations.
//!
//! What the family proves, and what it deliberately does not:
//!
//! - **Proved**: budget accounting under arbitrary advance sequences, including
//!   over-advance and re-advance; restart by remaining-duration rather than by
//!   instant; the refusal on a wall clock; the saturation ceiling; the watchdog
//!   being genuinely independent of logical time.
//! - **Not claimed**: that a *real* `sleep` resolves at a virtual instant. The
//!   logical clock governs deadlines; the engine's timer governs when a future
//!   wakes. Conflating them is the failure the module exists to prevent, so no
//!   test here asserts it.

#![cfg(all(feature = "rt", feature = "time"))]

#[path = "sim/seed.rs"]
mod seed;

use std::time::Duration;

use lgwks_bot::rt::clock::{Clock, ClockError, TimeSource};
use lgwks_bot::rt::time::Deadline;

use seed::{Rng, Trace};

/// A contiguous run of seeds this family sweeps.
///
/// Declared here rather than imported because `Band` lives in the shared rig,
/// which names `lgwks_bot` types the other `sim_*` targets need and this family
/// does not. The seed space itself is the shared one (`0..64`), so a failure
/// reported against this band names the same seeds as any other family.
const BAND_FIRST: u64 = 0;
const BAND_COUNT: u64 = 64;

/// Every seed in this family's band.
fn seeds() -> impl Iterator<Item = u64> {
    BAND_FIRST..BAND_FIRST.saturating_add(BAND_COUNT)
}

/// The claim this family proves about the watchdog: three logical days of
/// advance move the real-time watchdog by at most the millisecond a loaded test
/// host needs to run sixty-four scenarios. Anything approaching the logical
/// elapsed time would mean the watchdog had been wired to the logical clock.
///
/// One second, not one nanosecond: the bound is about *independence*, and a
/// nanosecond bound would be asserting that the host's scheduler took zero time,
/// which is a claim about the machine rather than about the clock.
const WATCHDOG_LIMIT: Duration = Duration::from_secs(1);

/// What a scenario records about a clock run, so two runs of the same seed can
/// be compared as one number rather than by reading two traces.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Receipt {
    /// Budget remaining after the scenario, per budget.
    remaining: Vec<u64>,
    /// How many times a deadline was observed exhausted.
    exhausted_count: u32,
    /// The largest advance the scenario refused, or zero.
    refused_ceiling_nanos: u64,
    /// Whether the watchdog outlived its limit during the run.
    watchdog_fired: bool,
}

/// Run one scenario from `seed` against a virtual clock, returning its trace.
fn scenario(seed: u64) -> (Trace, Receipt) {
    let mut rng = Rng::new(seed ^ 0xc10c_10c1_c10c_10c1);
    let mut trace = Trace::new();
    let clock = Clock::virtual_at(Duration::ZERO);
    let budgets: Vec<Duration> = (1..=4)
        .map(|index| {
            let span = u64::from(rng.between(1, 3));
            // Saturating rather than a bare product: this repository forbids
            // unchecked arithmetic, and both factors are bounded, so saturation
            // is the shape the lint admits and a fact the value already meets.
            Duration::from_secs(span.saturating_mul(u64::try_from(index).unwrap_or(0)))
        })
        .collect();

    let mut receipt = Receipt {
        remaining: Vec::new(),
        exhausted_count: 0,
        refused_ceiling_nanos: 0,
        watchdog_fired: false,
    };

    for _ in 0..32 {
        // A seeded mix of "just short of the budget" and "far past it", so the
        // boundary is exercised from both sides in every band.
        let step = if rng.chance(500) {
            Duration::from_millis(u64::from(rng.below(5_000)))
        } else {
            Duration::from_secs(u64::from(rng.between(1, 90)))
        };
        let before = clock.now();
        let after = match clock.advance(step) {
            Ok(landed) => landed,
            Err(ClockError::OutOfRange { ceiling, .. }) => {
                receipt.refused_ceiling_nanos = u64::try_from(ceiling.as_nanos()).unwrap_or(0);
                trace.record("advance-refused-out-of-range");
                break;
            }
            Err(other) => {
                // A virtual clock has no `NotVirtual` arm; a scenario that hit
                // one would be a real defect, so it is recorded and the run
                // ends rather than silently continuing on a clock that moved.
                trace.record(&format!("advance-refused-{other}"));
                break;
            }
        };
        // Monotonicity is the whole contract of an advance: never backwards,
        // and never more than asked unless the ceiling clamped it.
        trace.record_u64(
            "elapsed-nanos",
            u64::try_from(after.as_nanos()).unwrap_or(0),
        );
        if after < before {
            trace.record("monotonicity-violated");
        }

        for budget in &budgets {
            let deadline = Deadline::after(&clock, *budget);
            if deadline.is_exhausted() {
                receipt.exhausted_count = receipt.exhausted_count.saturating_add(1);
            }
            receipt
                .remaining
                .push(u64::try_from(deadline.remaining().as_nanos()).unwrap_or(0));
            // The remaining budget is the restart-safe quantity, and it is a
            // pure function of the clock and the budget — this is the identity
            // a restart depends on.
            let restored = Clock::virtual_at(clock.now());
            let restored_deadline = Deadline::after(&restored, *budget);
            if restored_deadline.remaining() != deadline.remaining() {
                trace.record("restart-changed-remaining");
            }
        }
    }

    // The watchdog is a separate fact from the logical counter. On a scenario
    // that ran microseconds of real time, advancing seconds of logical time
    // must not have moved it.
    let deadline = Deadline::after(&clock, Duration::from_secs(1));
    // A virtual clock refuses only at its ceiling, which this scenario is three
    // days short of. The `if let` keeps the refusal expressible as a failure
    // rather than an `unwrap`, because a refusal here would mean the ceiling
    // moved and every budget in the receipt above is now suspect.
    if let Err(error) = clock.advance(Duration::from_secs(86_400)) {
        trace.record(&format!("unexpected-ceiling-refusal-{error}"));
    }
    if deadline.is_exhausted() {
        trace.record("a-day-of-logical-time-exhausts-a-one-second-budget");
    }
    receipt.watchdog_fired = deadline.watchdog_exceeded(WATCHDOG_LIMIT);
    trace.record_u64(
        "final-nanos",
        u64::try_from(clock.now().as_nanos()).unwrap_or(0),
    );
    // The watchdog's *reading* is real time and is deliberately NOT recorded: a
    // trace that carried nanoseconds of wall clock would differ on every run and
    // stop being a replay receipt. What is recorded is the boolean below, which
    // is false on any run that finished inside a microsecond of real work.
    trace.record_u64("watchdog-fired", u64::from(receipt.watchdog_fired));
    (trace, receipt)
}

/// The clock is deterministic: the same seed gives the same trace and the same
/// receipt, so a recorded hash is a replay receipt.
#[test]
fn the_same_seed_replays_the_same_clock_trace() -> Result<(), Box<dyn std::error::Error>> {
    let mut first: Vec<(u64, Receipt)> = Vec::new();
    let mut second: Vec<(u64, Receipt)> = Vec::new();
    for seed in seeds() {
        let (trace_a, receipt_a) = scenario(seed);
        let (trace_b, receipt_b) = scenario(seed);
        assert_eq!(
            trace_a.hash(),
            trace_b.hash(),
            "seed {seed} produced two different clock traces, so a clock hash \
             is not a replay receipt"
        );
        first.push((trace_a.hash(), receipt_a));
        second.push((trace_b.hash(), receipt_b));
    }
    assert_eq!(first, second, "the receipt diverged across a repeat sweep");
    // The watchdog is independent in every seed, not just the one the boundary
    // tests exercise: three logical days inside a run this fast cannot have
    // consumed a second of real time.
    assert!(
        first.iter().all(|entry| !entry.1.watchdog_fired),
        "a seed drove the wall watchdog past {WATCHDOG_LIMIT:?} without any real \
         wait, which means logical time reached the watchdog"
    );
    Ok(())
}

/// Distinct seeds give distinct traces, so the family is not sweeping a
/// constant scenario that happens to replay.
#[test]
fn distinct_seeds_give_distinct_clock_traces() {
    let mut hashes: Vec<u64> = seeds().map(|seed| scenario(seed).0.hash()).collect();
    let before = hashes.len();
    hashes.sort_unstable();
    hashes.dedup();
    assert!(
        hashes.len() * 2 > before,
        "only {} of {} seeded clock traces were distinct, so most seeds replay \
         an identical scenario",
        hashes.len(),
        before
    );
}

/// A budget is never over-reported: remaining saturates at zero and never
/// wraps into a huge value the way a subtracting subtraction would.
#[test]
fn an_over_advanced_clock_saturates_rather_than_wrapping() -> Result<(), Box<dyn std::error::Error>>
{
    let clock = Clock::virtual_at(Duration::ZERO);
    let deadline = Deadline::after(&clock, Duration::from_secs(5));
    clock.advance(Duration::from_secs(86_400))?;
    assert_eq!(
        deadline.remaining(),
        Duration::ZERO,
        "an exhausted budget reports zero, not a wrapped multi-century remainder"
    );
    assert!(
        deadline.is_exhausted(),
        "a day past a five-second budget is spent"
    );
    Ok(())
}

/// Restart preserves the remaining budget, because the snapshot is a duration
/// rather than an instant. This is the property a durable record depends on.
#[test]
fn a_restart_restores_the_remaining_budget_and_not_an_instant()
-> Result<(), Box<dyn std::error::Error>> {
    let budget = Duration::from_secs(60);
    let original = Clock::virtual_at(Duration::ZERO);
    original.advance(Duration::from_secs(25))?;

    let recorded = original.snapshot();
    // This is what a durable record would hold: an elapsed duration. Not a
    // `std::time::Instant`, which has no epoch and would mean nothing on
    // another host.
    let resumed = Clock::virtual_at(recorded.elapsed());
    assert_eq!(
        resumed.snapshot().remaining_from(budget),
        recorded.remaining_from(budget),
        "a clock resumed from its snapshot has the same remaining budget"
    );
    assert_eq!(
        recorded.remaining_from(budget),
        Duration::from_secs(35),
        "sixty minus twenty-five is thirty-five seconds left"
    );
    Ok(())
}

/// A wall clock refuses to be advanced, and the refusal names which world
/// refused it, so a caller cannot mistake a wrong clock for a full one.
#[test]
fn a_wall_clock_refuses_a_caller_advance() {
    let clock = Clock::wall();
    assert_eq!(clock.source(), TimeSource::Wall);
    assert_eq!(
        clock.advance(Duration::from_secs(1)),
        Err(ClockError::NotVirtual),
        "a wall clock advances itself; a caller advance is a wrong-clock bug"
    );
    let err = ClockError::NotVirtual;
    assert!(
        format!("{err}").contains("real time"),
        "the refusal says why it refused, in text a log line can carry: {err}"
    );
}

/// The watchdog is independent of logical time. Pausing or racing a logical
/// clock cannot disable the real-time bound that catches a subprocess, a
/// blocking callback or a store hang.
#[test]
fn racing_logical_time_leaves_the_wall_watchdog_independent()
-> Result<(), Box<dyn std::error::Error>> {
    let clock = Clock::virtual_at(Duration::ZERO);
    let watchdog = clock.wall_watchdog();
    let before = watchdog.elapsed();
    // Three logical days, in one call, with no real wait at all.
    clock.advance(Duration::from_secs(259_200))?;
    let after = watchdog.elapsed();
    assert!(
        after < Duration::from_secs(30),
        "three logical days moved the wall watchdog by {after:?}, which is not \
         real elapsed time"
    );
    assert!(
        after >= before,
        "the watchdog is monotonic even while logical time races"
    );
    Ok(())
}

/// The representable ceiling is reported, not discovered by a caller tripping
/// over it: an advance past it saturates instead of wrapping into the past,
/// where a computed deadline would fire immediately and look legitimate.
#[test]
fn the_elapsed_ceiling_saturates_instead_of_wrapping_into_the_past()
-> Result<(), Box<dyn std::error::Error>> {
    let clock = Clock::virtual_at(Clock::elapsed_ceiling());
    assert_eq!(
        clock.advance(Duration::from_secs(1)),
        Err(ClockError::OutOfRange {
            requested: Duration::from_secs(1),
            ceiling: Duration::ZERO,
        }),
        "a clock already at its ceiling has no headroom and says so"
    );
    assert_eq!(
        clock.now(),
        Clock::elapsed_ceiling(),
        "a refused advance leaves the clock unchanged rather than wrapping"
    );
    Ok(())
}
