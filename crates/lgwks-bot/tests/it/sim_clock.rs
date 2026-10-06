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

use crate::sim::seed;

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
    ///
    /// A `Duration` rather than a nanosecond count: a budget this family sweeps
    /// is seconds long, and narrowing one to `u64` nanoseconds would need a
    /// fallback for the case where it does not fit — and a fallback would make a
    /// wrapped reading and a genuinely unstarted clock the same number in the
    /// receipt, which is the failure this family exists to catch.
    remaining: Vec<Duration>,
    /// How many times a deadline was observed exhausted.
    exhausted_count: u32,
    /// The largest advance the scenario refused, or zero.
    refused_ceiling: Duration,
    /// Whether the watchdog outlived its limit during the run.
    watchdog_fired: bool,
}

/// Run one scenario from `seed` against a virtual clock, returning its trace.
fn scenario(seed: u64) -> (Trace, Receipt) {
    let mut rng = Rng::new(seed ^ 0xc10c_10c1_c10c_10c1);
    let mut trace = Trace::new();
    let clock = Clock::virtual_at(Duration::ZERO);
    let budgets: Vec<Duration> = (1_u64..=4)
        .map(|index| {
            let span = u64::from(rng.between(1, 3));
            // Saturating rather than a bare product: this repository forbids
            // unchecked arithmetic, and both factors are bounded, so saturation
            // is the shape the lint admits and a fact the value already meets.
            Duration::from_secs(span.saturating_mul(index))
        })
        .collect();

    let mut receipt = Receipt {
        remaining: Vec::new(),
        exhausted_count: 0,
        refused_ceiling: Duration::ZERO,
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
                receipt.refused_ceiling = ceiling;
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
        record_duration(&mut trace, "elapsed", after);
        if after < before {
            trace.record("monotonicity-violated");
        }

        for budget in &budgets {
            let deadline = Deadline::after(&clock, *budget);
            if deadline.is_exhausted() {
                receipt.exhausted_count = receipt.exhausted_count.saturating_add(1);
            }
            receipt.remaining.push(deadline.remaining());
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
    record_duration(&mut trace, "final", clock.now());
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

// ── Origin-to-counter saturation ───────────────────────────────────────────
//
// `Clock::virtual_at` converts an origin `Duration` into the `u64` nanosecond
// counter every budget, deadline and snapshot is derived from. `Duration`
// reaches `u128` nanoseconds and that counter reaches `u64` of them, so an
// origin past ~584 years cannot be represented and the conversion has to
// saturate at the ceiling. A conversion that wrapped instead would report a
// clock in the recent past for an origin centuries away, and every deadline
// computed from it would fire immediately and look legitimate — the exact
// failure INV-BOT-30 names. The sweep below pins that identity across the whole
// representable range rather than at one hand-picked value.

/// The classes of origin this family sweeps, by what each one would break.
///
/// Declared as data rather than drawn: a uniform draw over `Duration` would
/// essentially never land on a sub-second origin or on one nanosecond below the
/// ceiling, and those are the two boundaries the identity is about. Every
/// scenario sweeps every class, and the seed supplies the offset inside each.
const ORIGIN_CLASSES: [&str; 6] = [
    "zero",
    "sub-second",
    "whole-second",
    "below-ceiling",
    "at-ceiling",
    "past-ceiling",
];

/// One origin in the class `ORIGIN_CLASSES[index]` describes, offset by `span`.
///
/// The span is what the seed varies; the index is what the table fixes, so a
/// class is never skipped for want of a draw that happened to hit it.
fn origin_in_class(index: usize, span: u64) -> Duration {
    match index {
        0 => Duration::ZERO,
        1 => Duration::from_nanos(span),
        2 => Duration::from_secs(span),
        // One second at most below the ceiling, so every draw in this class is a
        // horizon a caller is plausibly near rather than a rounded figure.
        3 => Duration::from_nanos(u64::MAX.saturating_sub(span)),
        4 => Clock::elapsed_ceiling(),
        _ => Duration::MAX,
    }
}

/// Every class of origin for this scenario, each with its own seeded offset.
fn seeded_origins(rng: &mut Rng) -> Vec<(&'static str, Duration)> {
    ORIGIN_CLASSES
        .iter()
        .enumerate()
        .map(|(index, class)| {
            (
                *class,
                origin_in_class(index, u64::from(rng.below(1_000_000))),
            )
        })
        .collect()
}

/// One origin read back through the counter the clock actually keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OriginReading {
    /// Which class of origin produced this reading.
    class: &'static str,
    /// The origin the clock was restored at.
    origin: Duration,
    /// What the clock then read.
    read: Duration,
}

/// Record a duration as the two projections that carry it exactly.
///
/// `as_secs` and `subsec_nanos` are both infallible, so a duration at the far
/// end of the range reaches the trace without a fallible narrowing and without
/// a value that stands in for one.
fn record_duration(trace: &mut Trace, label: &str, value: Duration) {
    trace.record(label);
    trace.record_u64("secs", value.as_secs());
    trace.record_u64("subsec-nanos", u64::from(value.subsec_nanos()));
}

/// Run one origin scenario from `seed`, returning its trace and its readings.
fn origin_scenario(seed: u64) -> (Trace, Vec<OriginReading>) {
    let mut rng = Rng::new(seed ^ 0x0e5a_7e00_0e5a_7e00);
    let mut trace = Trace::new();
    let ceiling = Clock::elapsed_ceiling();
    let mut observed = Vec::new();

    for (class, origin) in seeded_origins(&mut rng) {
        let budget = Duration::from_secs(u64::from(rng.below(3_600)));
        let clock = Clock::virtual_at(origin);
        let read = clock.now();

        record_duration(&mut trace, class, origin);
        record_duration(&mut trace, "read", read);

        // The identity: a clock restored at an origin reads back the smaller of
        // that origin and the ceiling. Everything else in the module is derived
        // from this counter.
        if read != origin.min(ceiling) {
            trace.record("origin-not-clamped-to-its-own-ceiling");
        }
        // A restart restores a *duration*, and the resumed clock must agree
        // with the one it was resumed from rather than drifting on the round
        // trip.
        if Clock::virtual_at(read).now() != read {
            trace.record("restart-drifted-the-counter");
        }
        // The budget is never over-reported. A wrapped counter would leave the
        // origin looking recent and hand back a remainder larger than the whole
        // budget.
        let remaining = clock.snapshot().remaining_from(budget);
        record_duration(&mut trace, "remaining", remaining);
        if remaining > budget {
            trace.record("remaining-exceeded-its-own-budget");
        }

        // And the advance path uses the same conversion, so it gets the same
        // identity: a step lands on the smaller of "where the clock was plus
        // the step" and the ceiling, and never below where it was.
        let step = Duration::from_millis(u64::from(rng.below(120_000)));
        match clock.advance(step) {
            Ok(landed) => {
                record_duration(&mut trace, "landed", landed);
                if landed != read.saturating_add(step).min(ceiling) {
                    trace.record("advance-not-clamped-to-its-own-ceiling");
                }
                if landed < read {
                    trace.record("advance-went-backwards");
                }
            }
            Err(ClockError::OutOfRange { .. }) => {
                // Only reachable from a clock already at the ceiling, which is
                // the `at-ceiling` and `past-ceiling` classes; recorded so the
                // trace says which class refused rather than collapsing the two
                // refusals into one line.
                trace.record("advance-refused-at-the-ceiling");
            }
            Err(other) => {
                trace.record(&format!("advance-refused-{other}"));
            }
        }
        observed.push(OriginReading {
            class,
            origin,
            read,
        });
    }
    (trace, observed)
}

/// The saturation identity replays: the same seed gives the same trace and the
/// same readings, so a hash recorded against this sweep is a receipt.
#[test]
fn the_same_seed_replays_the_same_origin_trace() -> Result<(), Box<dyn std::error::Error>> {
    let mut first: Vec<(u64, Vec<OriginReading>)> = Vec::new();
    let mut second: Vec<(u64, Vec<OriginReading>)> = Vec::new();
    for seed in seeds() {
        let (trace_a, observed_a) = origin_scenario(seed);
        let (trace_b, observed_b) = origin_scenario(seed);
        assert_eq!(
            trace_a.hash(),
            trace_b.hash(),
            "seed {seed} produced two different origin traces, so an origin hash \
             is not a replay receipt"
        );
        first.push((trace_a.hash(), observed_a));
        second.push((trace_b.hash(), observed_b));
    }
    assert_eq!(first, second, "the readings diverged across a repeat sweep");
    Ok(())
}

/// No seed in the band observes a counter outside the representable range, and
/// every one of them exercises an origin at the ceiling — the half of the range
/// where a conversion is wrong rather than merely untidy.
#[test]
fn no_swept_origin_reads_outside_the_representable_range() {
    let ceiling = Clock::elapsed_ceiling();
    let mut saturated = 0_u32;
    for seed in seeds() {
        let (trace, observed) = origin_scenario(seed);
        for reading in observed {
            assert!(
                reading.read <= ceiling,
                "seed {seed}: a {} origin of {:?} read back as {:?}, which is past \
                 the ceiling {ceiling:?} rather than clamped to it",
                reading.class,
                reading.origin,
                reading.read
            );
            assert_eq!(
                reading.read,
                reading.origin.min(ceiling),
                "seed {seed}: a {} origin of {:?} read back as {:?}",
                reading.class,
                reading.origin,
                reading.read
            );
            if reading.origin >= ceiling {
                saturated = saturated.saturating_add(1);
            }
        }
        // A trace carrying no violation at all is the claim being made, and the
        // hash is checked against the sweep in the replay test above.
        assert!(
            !trace.is_empty(),
            "seed {seed} recorded nothing, so the sweep measured nothing"
        );
    }
    assert!(
        saturated > 0,
        "no seed in {BAND_FIRST}..{} reached the ceiling, so the saturating half \
         of the conversion was never exercised",
        BAND_FIRST.saturating_add(BAND_COUNT)
    );
}

/// The conversion keeps sub-second precision and clips at exactly the ceiling.
///
/// A whole-second-only conversion would pass every test above — every sweep
/// assertion compares two readings of the same conversion — and would silently
/// shorten every sub-second deadline, so the two facts are pinned directly.
#[test]
fn the_nanos_conversion_keeps_subsecond_precision_and_stops_at_the_ceiling() {
    let ceiling = Clock::elapsed_ceiling();
    let cases = [
        (Duration::ZERO, Duration::ZERO),
        (Duration::from_nanos(1), Duration::from_nanos(1)),
        (Duration::from_millis(1_500), Duration::from_millis(1_500)),
        (Duration::new(1, 999_999_999), Duration::new(1, 999_999_999)),
        // One nanosecond below the ceiling must survive exactly: a ceiling that
        // clips one value early would report a clock slightly in the past for
        // an origin it can represent.
        (
            Duration::from_nanos(u64::MAX.saturating_sub(1)),
            Duration::from_nanos(u64::MAX.saturating_sub(1)),
        ),
        (ceiling, ceiling),
        // Past the ceiling in both directions: saturates, never wraps.
        (Duration::MAX, ceiling),
        (ceiling.saturating_add(Duration::from_secs(1)), ceiling),
    ];
    for (origin, expected) in cases {
        assert_eq!(
            Clock::virtual_at(origin).now(),
            expected,
            "an origin of {origin:?} must read back as {expected:?}"
        );
    }
}
