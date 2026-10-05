//! The seeded simulation family for the open-loop driver.
//!
//! # What this family is for
//!
//! The live driver in `main.rs` measures the real `Supervisor` and real raw
//! Tokio on real time, so it cannot answer two questions that a reader of its
//! table has to be able to ask:
//!
//! 1. **Is the schedule arithmetic the arithmetic the claim rests on?** Latency
//!    measured from the intended start is the whole correction for coordinated
//!    omission, and a closed-loop model of the same workload reports a p99 that
//!    barely moves past the knee. The model here is driven by a logical clock, so
//!    the two figures can be read side by side on the same world.
//! 2. **Does the run replay?** A saturation curve is a set of numbers produced by
//!    a generator; a generator whose numbers cannot be reproduced is a number with
//!    no provenance. Every test here is a pure function of one seed, and the
//!    replay assertion is on a trace hash that folds every scheduled event.
//!
//! # The shape of the family
//!
//! One seed drives arrival jitter and body length; two seeds of the same world
//! must produce identical traces (same trace hash) and different seeds must
//! diverge. Bounds swept are 1, 4 and 64 — the model's admission scan is linear in
//! the bound, and the property under test is the *arithmetic*, not the
//! throughput, which the live rig measures.
//!
//! Each test prints its seed in the assertion message, so a failure names the
//! world that failed and re-running it is one argument.

use crate::openloop::{
    Histogram, Recorder, RecorderSet, SimSpec, intended_arrival, period_nanos, simulate, slot_of,
    slot_range, slot_value,
};

/// The body costs the live sweep's ladder is checked at, in microseconds.
///
/// Three, because one is a point rather than a family: the sweep's own declared default,
/// a body ten times cheaper (a ceiling whose declared capacity is already generator-capped
/// even at the narrowest bound), and a body ten times dearer (a ceiling so wide that every
/// rung but the first is clamped). A ladder checked at one body cost would only prove the
/// ladder is right where it was looked at.
const BODY_COSTS: [u64; 3] = [500, 5_000, 50_000];

/// The bounds the live sweep declares, at which the ladder arithmetic is checked.
///
/// Every declared bound, not a convenient subset: the ladder is exactly the arithmetic
/// whose wide-bound behaviour is the thing a reader has to be told about, so checking it
/// at 64 alone would prove it where it is least interesting.
const SWEEP_BOUNDS: [usize; 6] = [64, 1_024, 10_000, 16_384, 100_000, 131_072];

/// The bounds the model can sweep exhaustively, because its admission is a linear scan.
const MODEL_BOUNDS: [usize; 3] = [1, 4, 64];

/// The seeds this family sweeps.
const SEEDS: [u64; 4] = [
    0x0000_0000_0000_0001,
    0x5EED_0000_0000_0001,
    0xDEAD_BEEF_0000_0001,
    0xA5A5_A5A5_1234_5678,
];

/// What one permit of this body's length can sustain, in arrivals per second.
///
/// The mean body is 5 µs, so a permit admits about 200,000 arrivals a second. Every
/// rate below is derived from this rather than typed in, because a hard-coded rate that
/// saturates one bound and not another is the defect this family exists to rule out.
const PERMITS_PER_SECOND: u64 = 200_000;

/// A world far below its knee: a tenth of what the ceiling can sustain, so nothing
/// queues at any bound and every arrival completes.
fn quiet(bound: usize, seed: u64) -> SimSpec {
    SimSpec {
        offered_rate: u64::try_from(bound)
            .unwrap_or(1)
            .saturating_mul(PERMITS_PER_SECOND / 10)
            .max(1),
        arrivals: 512,
        bound,
        service_min_nanos: 2_000,
        service_max_nanos: 8_000,
        jitter: true,
        refuse_at_bound: false,
        seed,
    }
}

/// A world driven past its knee, at **every** bound the family sweeps.
///
/// The offered rate is five times the ceiling's capacity and the run is four times the
/// bound in arrivals, so the backlog is guaranteed to build at bound 1 and at bound 64
/// alike. A saturation arm that only saturates its smallest bound would prove nothing
/// about the others, which is exactly how a reader would take it.
fn saturated(bound: usize, seed: u64) -> SimSpec {
    SimSpec {
        offered_rate: u64::try_from(bound)
            .unwrap_or(1)
            .saturating_mul(PERMITS_PER_SECOND * 5),
        arrivals: u64::try_from(bound).unwrap_or(1).saturating_mul(4),
        bound,
        service_min_nanos: 2_000,
        service_max_nanos: 8_000,
        jitter: true,
        refuse_at_bound: false,
        seed,
    }
}

/// The same saturated world on the refusal door: the arrival is dropped and
/// counted rather than queued.
fn saturated_refusing(bound: usize, seed: u64) -> SimSpec {
    SimSpec {
        refuse_at_bound: true,
        ..saturated(bound, seed)
    }
}

/// One world with every field stated, for the tests that need two worlds differing in
/// exactly one field.
///
/// `quiet` and `saturated` derive the offered rate *from the bound*, which is right for
/// the saturation families and wrong for a comparison: a comparison that wants the same
/// work on both sides has to name the same rate on both, or it is comparing two
/// different offers.
fn world(
    bound: usize,
    offered_rate: u64,
    arrivals: u64,
    jitter: bool,
    refuse: bool,
    seed: u64,
) -> SimSpec {
    SimSpec {
        offered_rate,
        arrivals,
        bound,
        service_min_nanos: 2_000,
        service_max_nanos: 8_000,
        jitter,
        refuse_at_bound: refuse,
        seed,
    }
}

/// Every `(seed, bound)` pair the family sweeps.
fn sweep(spec: fn(usize, u64) -> SimSpec) -> Vec<(u64, usize, crate::openloop::SimTrace)> {
    let mut runs = Vec::new();
    for seed in SEEDS {
        for bound in [1_usize, 4, 64] {
            runs.push((seed, bound, simulate(&spec(bound, seed))));
        }
    }
    runs
}

/// One world whose bodies all cost exactly `body_micros`, jitter-free.
///
/// The live rig's declared capacity is `bound * 1e6 / body_micros`, so a model of the same
/// ceiling has to charge each body exactly that and nothing else — a jittered body would
/// put the model's own mean capacity somewhere the ladder had no rung for, and the
/// comparison between the ladder and the model it is read against would be a comparison of
/// two different workloads.
fn constant_service(
    bound: usize,
    body_micros: u64,
    offered_rate: u64,
    bodies_per_bound: u64,
    seed: u64,
) -> SimSpec {
    let service_nanos = body_micros.saturating_mul(1_000);
    SimSpec {
        offered_rate,
        arrivals: u64::try_from(bound)
            .unwrap_or(1)
            .saturating_mul(bodies_per_bound),
        bound,
        service_min_nanos: service_nanos,
        service_max_nanos: service_nanos,
        jitter: false,
        refuse_at_bound: false,
        seed,
    }
}

#[test]
fn sim_the_live_ladder_brackets_the_knee_or_declares_itself_capped() {
    // The knee table is read as "the highest rung that refused nothing and stayed inside
    // the budget", which is only a knee if the ladder actually reaches one. Two ways it
    // can fail, and both are defects rather than findings: a ladder whose rungs all clamp
    // to the generator's own ceiling prints one rate many times and reads as a curve, and
    // a ladder whose top rung sits below the declared capacity never offered past the knee
    // at all, so the "knee" it declares is the top of the ladder rather than a property of
    // the engine.
    for bound in SWEEP_BOUNDS {
        for body_micros in BODY_COSTS {
            let ladder = crate::sweep_ladder(bound, body_micros);
            assert!(
                ladder.len() <= crate::SWEEP_MULTIPLIERS.len(),
                "bound {bound} at {body_micros}us produced {} rungs for {} multipliers: the \
                 ladder was never deduplicated, so one rate is printed several times",
                ladder.len(),
                crate::SWEEP_MULTIPLIERS.len()
            );
            assert!(
                ladder.windows(2).all(|pair| pair[0] < pair[1]),
                "bound {bound} at {body_micros}us produced a non-increasing ladder {ladder:?}: \
                 two adjacent rungs resolved to the same rate, so the deduplication did not run"
            );
            assert!(
                ladder
                    .iter()
                    .all(|rate| *rate >= 1 && *rate <= crate::MAX_OFFERED_RATE),
                "bound {bound} at {body_micros}us produced {ladder:?}: a rate outside \
                 [1, MAX_OFFERED_RATE] is one the generator cannot express"
            );

            let capacity = crate::capacity_per_second(bound, body_micros);
            let top = ladder.last().copied().unwrap_or(0);
            if crate::ladder_clamped(bound, body_micros) {
                assert_eq!(
                    top, crate::MAX_OFFERED_RATE,
                    "bound {bound} at {body_micros}us declares itself clamped and yet its top \
                     rung is {top}, not the generator's own ceiling"
                );
            } else {
                assert!(
                    top >= capacity,
                    "bound {bound} at {body_micros}us declares a capacity of {capacity} \
                     arrivals/s and its top rung is {top}: the ladder never offered past the \
                     knee, so the knee it declares is the end of the ladder"
                );
                assert!(
                    ladder.first().copied().unwrap_or(0) < capacity,
                    "bound {bound} at {body_micros}us starts its ladder at {:?}, at or above its \
                     own declared capacity of {capacity}: there is no rung below the knee to \
                     read it from",
                    ladder.first()
                );
            }
        }
    }
}

#[test]
fn sim_both_sides_are_offered_the_same_arrivals() {
    // The fairness gate refused a window-bounded run naming `placed`, because a faster side
    // places more arrivals inside the same second and the offered work stops being equal.
    // The count both sides are given is therefore derived from the rate and the window, and
    // this pins the three properties that makes it: it is the same number whoever asks for
    // it, it is the rate times the window, and a zero-second window cannot silently offer
    // nothing.
    let mut drawn: Vec<u64> = Vec::new();
    for seed in SEEDS {
        let rate = crate::async_stats::Rng::new(seed).next_u64() % crate::MAX_OFFERED_RATE + 1;
        drawn.push(rate);
        assert_eq!(
            crate::async_stats::Rng::new(seed).next_u64() % crate::MAX_OFFERED_RATE + 1,
            rate,
            "seed {seed}: the offered rate this world drew did not replay, so the count under \
             test is not a function of the seed alone"
        );
        for window in [
            std::time::Duration::ZERO,
            std::time::Duration::from_secs(1),
            std::time::Duration::from_secs(3),
        ] {
            let offered = crate::sweep_arrivals(rate, window);
            assert_eq!(
                offered,
                crate::sweep_arrivals(rate, window),
                "seed {seed}: {rate}/s for {window:?} produced two different arrival counts — \
                 the count is a function of the rate and the window and of nothing else"
            );
            assert_eq!(
                offered,
                rate.saturating_mul(window.as_secs().max(1)),
                "seed {seed}: {rate}/s for {window:?} offered {offered} arrivals, which is not \
                 the rate times the window floored at one second"
            );
            assert!(
                offered > 0,
                "seed {seed}: {rate}/s for {window:?} offered nothing at all: a zero-arrival \
                 point is a measurement of nothing rather than a short one"
            );
        }
    }
    let unique = drawn.iter().collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        unique.len(),
        drawn.len(),
        "two seeds drew the same offered rate, so the seeds are not driving this family"
    );
}

#[test]
fn sim_the_live_ladder_reads_the_knee_the_model_measures() {
    // The same ladder the live sweep walks, run through the model at the bounds the model
    // can sweep exhaustively: the bottom rung admits and completes every arrival, and the
    // top rung — four times the ceiling's declared capacity once it is past it — queues
    // behind the ceiling rather than passing through it. A ladder whose rungs do not land
    // in those two regimes would declare a knee against worlds where the knee is not there.
    for bound in MODEL_BOUNDS {
        for body_micros in BODY_COSTS {
            let ladder = crate::sweep_ladder(bound, body_micros);
            let bottom = ladder.first().copied().unwrap_or(0);
            let top = ladder.last().copied().unwrap_or(0);
            assert!(
                ladder.len() >= 2,
                "bound {bound} at {body_micros}us produced a {}-rung ladder {ladder:?}: the knee \
                 table reads the top rung, and a ladder of one rung has no top to read",
                ladder.len()
            );
            for seed in SEEDS {
                let quiet = simulate(&constant_service(bound, body_micros, bottom, 8, seed));
                assert_eq!(
                    quiet.refused, 0,
                    "seed {seed} at bound {bound} and {body_micros}us: the ladder's bottom rung \
                     {bottom}/s is a quarter of the declared capacity and the model refused \
                     {} arrivals on it",
                    quiet.refused
                );
                assert_eq!(
                    quiet.completed, quiet.offered,
                    "seed {seed} at bound {bound} and {body_micros}us: the ladder's bottom rung \
                     completed {} of {} arrivals",
                    quiet.completed,
                    quiet.offered
                );

                if top <= crate::capacity_per_second(bound, body_micros) {
                    continue;
                }
                let deep = simulate(&constant_service(bound, body_micros, top, 8, seed));
                assert!(
                    deep.peak_queue > bound,
                    "seed {seed} at bound {bound} and {body_micros}us: the ladder's top rung \
                     {top}/s is past the declared capacity and the model still queued no \
                     deeper than its ceiling (peak queue {})",
                    deep.peak_queue
                );
                assert!(
                    deep.p99_intended_nanos > deep.p99_actual_nanos,
                    "seed {seed} at bound {bound} and {body_micros}us: past the knee the \
                     intended-start p99 {}ns did not exceed the actual-start p99 {}ns, so the \
                     queue the saturation built is invisible to the measurement",
                    deep.p99_intended_nanos,
                    deep.p99_actual_nanos
                );
            }
        }
    }
}

#[test]
fn sim_the_derived_body_puts_every_ceiling_at_the_declared_target() {
    // The scaling the whole sweep rests on: the body cost is derived per bound so every
    // ceiling's declared capacity lands on the same target, because a constant body cost
    // puts a wide ceiling's capacity above what an in-process generator can offer and every
    // rung above the first then measures the generator instead of the ceiling. Three
    // properties, and the middle one is the one that makes the other two true: the rounding
    // is upwards, so the derived capacity is at or just below the target and never above it.
    for bound in SWEEP_BOUNDS {
        for target in [1_000_u64, 20_000, 250_000] {
            let body = crate::body_for_bound(bound, target, 0);
            let capacity = crate::capacity_per_second(bound, body);
            assert!(
                capacity <= target,
                "bound {bound} derived a {body}us body at a target of {target}/s and declares \
                 {capacity}/s: the rounding went the wrong way, so the ceiling is under more \
                 load than the ladder's middle rung expects"
            );
            assert!(
                capacity * 100 >= target * 90,
                "bound {bound} derived a {body}us body at a target of {target}/s and declares \
                 only {capacity}/s: a tenth of the load is missing, so the ladder's knee rung \
                 is below the ceiling's own knee"
            );
            let ladder = crate::sweep_ladder(bound, body);
            assert!(
                ladder.windows(2).all(|pair| pair[0] < pair[1]),
                "bound {bound} at a {target}/s target produced a non-increasing ladder \
                 {ladder:?} from a derived {body}us body"
            );
            // Four times the capacity is the rung that has to exist for the knee to be
            // inside the ladder rather than at its end. A target above 125,000/s clamps
            // that rung at the generator's own ceiling and still leaves eight times the
            // capacity, which is why the assertion is four and not sixteen.
            assert!(
                ladder.last().copied().unwrap_or(0) >= capacity.saturating_mul(4),
                "bound {bound} at a {target}/s target produced {ladder:?} against a declared \
                 capacity of {capacity}/s: the ladder does not reach four times the capacity, \
                 so the knee it declares is the end of the ladder"
            );
        // Every ceiling the family derives has to sit below what this generator can
            // actually offer, or the sweep's knee is the generator's. The measured floor on
            // the reference host over four sweep points was 449,482 arrivals a second for
            // the facade; the constant is a round number under it, and the run prints its own
            // `achieved/s` beside every row so a reader sees the margin rather than trusting
            // this line. The second target in this list is the rig's own
            // `DEFAULT_CAPACITY_TARGET`, so the default sweep is inside the assertion.
            assert!(
                capacity <= crate::GENERATOR_PLACEMENT_CEILING,
                "bound {bound} at a {target}/s target declares {capacity}/s, above the measured \
                 generator placement ceiling {}: this sweep's knee would be the generator's",
                crate::GENERATOR_PLACEMENT_CEILING
            );
        }
    }
}

#[test]
fn sim_the_knee_budget_scales_with_the_body_and_still_bites() {
    // The sweep derives a different body cost per bound, so a budget in milliseconds alone
    // cannot mean the same thing at every ceiling: at a bound of 131,072 the body is 6.55 s
    // and a 50 ms floor is smaller than one service time, which would declare "no knee" at
    // every rung and say something about the budget rather than about the engine. The rule
    // under test is `max(floor, one more service time)`, and both halves matter: the floor
    // has to hold where the body is cheap, and the body has to hold where it is dear.
    for bound in MODEL_BOUNDS {
        for body_micros in BODY_COSTS {
            let body_nanos = body_micros.saturating_mul(1_000);
            let budget = crate::SLO_P99_NANOS.max(body_nanos.saturating_mul(2));
            assert!(
                budget >= crate::SLO_P99_NANOS,
                "bound {bound} at {body_micros}us: the budget {budget}ns is below the declared \\
                 {}-ns floor, so a cheap body could not be held to the estate's budget",
                crate::SLO_P99_NANOS
            );
            assert!(
                budget >= body_nanos,
                "bound {bound} at {body_micros}us: the budget {budget}ns is below one service \
                 time ({body_nanos}ns), so every arrival would read as out of budget however \
                 little queueing there was"
            );
            let capacity = crate::capacity_per_second(bound, body_micros);
            for seed in SEEDS {
                // Enough bodies that a world driven sixteen times past its capacity builds a
                // backlog deeper than the budget rather than a shallow one: at a 500 us body
                // and eight arrivals per permit the deepest wait is seven service times,
                // which is *inside* a 50 ms floor, and the test would then be asserting that
                // a saturated world reads as healthy.
                let below = simulate(&constant_service(bound, body_micros, capacity, 256, seed));
                assert!(
                    below.p99_intended_nanos <= budget,
                    "seed {seed} at bound {bound} and {body_micros}us: a world offered at its \\
                     own declared capacity reported p99 {}ns against a budget of {budget}ns",
                    below.p99_intended_nanos
                );
                let past = simulate(&constant_service(
                    bound,
                    body_micros,
                    capacity.saturating_mul(16),
                    256,
                    seed,
                ));
                assert!(
                    past.p99_intended_nanos > budget,
                    "seed {seed} at bound {bound} and {body_micros}us: a world offered at sixteen \\
                     times its declared capacity reported p99 {}ns against a budget of \\
                     {budget}ns, so the budget cannot tell the two regimes apart",
                    past.p99_intended_nanos
                );
            }
        }
    }
}

#[test]
fn sim_an_explicit_body_cost_overrides_the_derived_one() {
    // `--body-micros` pins the cost at every bound, which is the other question a reader
    // may ask ("at a fixed service time, where does each ceiling's knee sit"). The override
    // has to win outright, and the run that uses it has to say on its face that the capacity
    // target does not apply — a silently ignored override would publish a scaled curve under
    // the name of a fixed one.
    for bound in SWEEP_BOUNDS {
        for pinned in [1_u64, 500, 5_000, 50_000] {
            assert_eq!(
                crate::body_for_bound(bound, 20_000, pinned),
                pinned,
                "bound {bound} with an explicit {pinned}us body: the override was not applied, \\
                 so the run measured a derived service time under the name of a pinned one"
            );
        }
        assert_eq!(
            crate::body_for_bound(bound, 20_000, 0),
            crate::body_for_bound(bound, 20_000, 0),
            "bound {bound}: the derived body cost did not replay, so the ladder it produces is \
             not a function of the bound and the target alone"
        );
    }
}

#[test]
fn sim_the_same_seed_replays_the_same_trace_hash() {
    for (seed, bound, first) in sweep(quiet) {
        let second = simulate(&quiet(bound, seed));
        assert_eq!(
            first, second,
            "seed {seed} at bound {bound} did not replay: the trace hash and every \
             reported figure must be a function of the seed alone"
        );
        assert_eq!(
            first.trace_hash, second.trace_hash,
            "seed {seed} at bound {bound} produced two different trace hashes"
        );
    }
}

#[test]
fn sim_distinct_seeds_diverge_in_their_trace() {
    // Across *seeds* at one fixed bound: two different seeds must never produce the same
    // trace, on either a quiet world or a saturated one.
    for bound in [1_usize, 64] {
        let mut hashes = std::collections::BTreeSet::new();
        for seed in SEEDS {
            for trace in [
                simulate(&quiet(bound, seed)),
                simulate(&saturated(bound, seed)),
            ] {
                assert!(
                    hashes.insert(trace.trace_hash),
                    "seed {seed} at bound {bound} replayed an earlier world's trace hash: \
                     the schedule is not seed-driven"
                );
            }
        }
        assert_eq!(
            hashes.len(),
            SEEDS.len() * 2,
            "every seeded world at bound {bound} must be distinct from every other one"
        );
    }
}

#[test]
fn sim_a_ceiling_changes_the_trace_where_it_binds_and_not_where_it_does_not() {
    // Two facts that are easy to confuse, and that a hash over only the admissions would
    // have hidden. Both worlds below offer *the same* rate and the same arrivals at two
    // ceilings: a ceiling that never binds leaves the schedule identical (same arrivals,
    // same starts, same ends — so the same hash), while a ceiling that does bind leaves a
    // different trace. A hash that could not tell those apart would report "the trace
    // records the ceiling" while in fact recording nothing at all.
    const OFFER: u64 = 20_000;
    const ARRIVALS: u64 = 256;
    for seed in SEEDS {
        let quiet_one = simulate(&world(1, OFFER, ARRIVALS, false, false, seed));
        let quiet_sixty_four = simulate(&world(64, OFFER, ARRIVALS, false, false, seed));
        assert_eq!(
            quiet_one.max_generator_lag_nanos, 0,
            "seed {seed}: the premise world waited for a slot at {OFFER}/s against a ceiling \
             of 1, so the ceiling bound it and the identical-trace assertion below would \
             prove nothing. The premise is jitter-free on purpose: a jittered schedule can \
             place two arrivals a nanosecond apart, so even an unbinding ceiling makes the \
             generator wait, and this test is about the ceiling rather than about jitter"
        );
        assert_eq!(
            quiet_one.peak_in_flight, 1,
            "seed {seed}: the premise world ran {} bodies at once against a ceiling of 1",
            quiet_one.peak_in_flight
        );
        assert_eq!(
            quiet_one.trace_hash, quiet_sixty_four.trace_hash,
            "seed {seed}: a world the ceiling never bound traced differently at bound 1 and \
             bound 64 — the trace records something other than the work"
        );

        // The same offer against a ceiling it cannot sustain: the narrow world queues and
        // the wide one does not, and the trace must record the difference.
        let bound_rate = u64::try_from(SEEDS.len() + 1).unwrap_or(1) * PERMITS_PER_SECOND * 10;
        let narrow = simulate(&world(1, bound_rate, ARRIVALS, false, false, seed));
        let wide = simulate(&world(64, bound_rate, ARRIVALS, false, false, seed));
        assert!(
            narrow.max_generator_lag_nanos > 0,
            "seed {seed}: the narrow premise world never waited for a slot at {bound_rate}/s, \
             so its ceiling did not bind and the differing-trace assertion below would \
             prove nothing"
        );
        assert_ne!(
            narrow.trace_hash, wide.trace_hash,
            "seed {seed}: two ceilings bound the same offer identically, so the trace does \
             not record which ceiling bound it"
        );
    }
}

#[test]
fn sim_latency_is_measured_from_the_intended_start_and_not_the_actual_one() {
    // The coordinated-omission property, on the same world read two ways. A
    // measurement from the actual start *excludes* the queueing delay by
    // construction: a saturated server is one the client stopped offering to, so
    // the queue never enters its samples. Past the knee the intended-start p99
    // must be materially larger, and that gap is the correction.
    for (seed, bound, trace) in sweep(saturated) {
        assert!(
            trace.p99_intended_nanos > trace.p99_actual_nanos,
            "seed {seed} at bound {bound}: intended-start p99 {}ns did not exceed \
             actual-start p99 {}ns, so the queue the saturation created is invisible \
             — the measurement is closed-loop under another name",
            trace.p99_intended_nanos,
            trace.p99_actual_nanos
        );
    }
}

#[test]
fn sim_a_schedule_below_the_knee_queues_nothing_and_completes_every_arrival() {
    for (seed, bound, trace) in sweep(quiet) {
        assert_eq!(
            trace.refused, 0,
            "seed {seed} at bound {bound} refused {} arrivals below its knee",
            trace.refused
        );
        assert_eq!(
            trace.completed, trace.offered,
            "seed {seed} at bound {bound}: {} of {} arrivals completed",
            trace.completed, trace.offered
        );
        assert_eq!(
            trace.admitted, trace.offered,
            "seed {seed} at bound {bound}: {} of {} arrivals were admitted",
            trace.admitted, trace.offered
        );
    }
}

#[test]
fn sim_past_the_knee_the_queue_never_exceeds_the_declared_bound() {
    for (seed, bound, trace) in sweep(saturated) {
        assert!(
            trace.peak_in_flight <= bound,
            "seed {seed} at bound {bound} ran {} bodies at once, above the ceiling: \
             the admission bound is not a bound",
            trace.peak_in_flight
        );
        assert!(
            trace.peak_queue > bound,
            "seed {seed} at bound {bound} never queued past its ceiling (peak queue \
             {}): the offered rate never reached the knee, so the saturation arm \
             proved nothing",
            trace.peak_queue
        );
    }
}

#[test]
fn sim_past_the_knee_the_refusal_door_refuses_instead_of_growing() {
    for (seed, bound, trace) in sweep(saturated_refusing) {
        assert!(
            trace.refused > 0,
            "seed {seed} at bound {bound} refused nothing on the refusal door: a \
             saturated world must drop arrivals rather than hold them"
        );
        assert_eq!(
            trace.admitted + trace.refused,
            trace.offered,
            "seed {seed} at bound {bound}: {} admitted + {} refused is not the \
             {} arrivals offered — work was lost or duplicated",
            trace.admitted,
            trace.refused,
            trace.offered
        );
        assert_eq!(
            trace.completed, trace.admitted,
            "seed {seed} at bound {bound}: {} of {} admitted bodies completed",
            trace.completed, trace.admitted
        );
    }
}

#[test]
fn sim_a_higher_bound_moves_the_knee_not_the_arithmetic() {
    // The same offer and the same bodies at two ceilings: the ceiling may change how far
    // behind the generator falls and how deep the queue gets, and may change nothing
    // else. If it changed the arrival count or the completions, the ceiling would be
    // fabricating work rather than bounding it.
    const OFFER: u64 = 10 * 4 * PERMITS_PER_SECOND;
    const ARRIVALS: u64 = 512;
    for seed in SEEDS {
        let narrow = simulate(&world(4, OFFER, ARRIVALS, false, false, seed));
        let wide = simulate(&world(64, OFFER, ARRIVALS, false, false, seed));
        assert_eq!(
            narrow.offered, wide.offered,
            "seed {seed}: the offered arrival count changed with the ceiling"
        );
        assert_eq!(
            narrow.completed, wide.completed,
            "seed {seed}: the completed count changed with the ceiling"
        );
        assert!(
            narrow.peak_in_flight <= 4,
            "seed {seed}: {} bodies ran at once against a ceiling of 4",
            narrow.peak_in_flight
        );
        assert!(
            wide.peak_in_flight <= 64,
            "seed {seed}: {} bodies ran at once against a ceiling of 64",
            wide.peak_in_flight
        );
        assert!(
            narrow.peak_queue > wide.peak_queue,
            "seed {seed}: the narrower ceiling queued no deeper ({} against {}) — a ceiling \
             that does not bind cannot be the only thing that moved",
            narrow.peak_queue,
            wide.peak_queue
        );
    }
}

#[test]
fn sim_a_recorder_reports_the_percentiles_it_was_given() {
    // The recorder's own contract: every value it is handed must come back inside
    // its bucket, and the quantiles must be ordered. The tolerance is the
    // documented `1 / SUB_BUCKETS` quantization bound plus one nanosecond, not a
    // number picked to make this pass.
    let recorder = Recorder::new();
    let mut values: Vec<u64> = Vec::new();
    for index in 0..2_000_u64 {
        let value = 40_u64
            .saturating_add(index.saturating_mul(977))
            .saturating_add((index % 7) * 13);
        values.push(value);
        recorder.record(value);
    }
    let histogram = Histogram::of_one(&recorder);
    assert_eq!(histogram.count(), 2_000, "every recorded sample is counted");
    for value in &values {
        let slot = slot_of(*value);
        let (low, high) = slot_range(slot);
        assert!(
            *value >= low && *value <= high,
            "{value}ns landed in the bucket for [{low}, {high}]"
        );
        let reported = slot_value(slot);
        let error = reported.abs_diff(*value);
        assert!(
            error <= 1 + value / crate::openloop::SUB_BUCKETS as u64,
            "{value}ns reported as {reported}ns: the quantization bound is \
             1/SUB_BUCKETS plus a nanosecond, got an error of {error}ns"
        );
    }
    let (p50, p95, p99) = histogram.percentiles_nanos();
    assert!(
        p50 <= p95 && p95 <= p99,
        "quantiles are not ordered: {p50}, {p95}, {p99}"
    );
    assert!(
        p50 >= histogram.min_nanos() && p99 <= histogram.max_nanos(),
        "the quantiles fell outside the observed range: min {}ns, p50 {p50}ns, \
         p99 {p99}ns, max {}ns",
        histogram.min_nanos(),
        histogram.max_nanos()
    );
}

#[test]
fn sim_a_sharded_recorder_equals_an_unsharded_one() {
    // The live driver records through 16 shards from every worker thread. A merge
    // that lost or double-counted a bucket would quietly move a p99, so the
    // sharded path is compared against the single-shard path on the same values.
    let set = RecorderSet::new();
    let mut sorted: Vec<u64> = Vec::new();
    for index in 0..4_096_u64 {
        let value = 1_u64.saturating_add((index % 97) * 1_003);
        set.take().record(value);
        sorted.push(value);
    }
    sorted.sort_unstable();
    let merged = Recorder::merged(set.shards());
    assert_eq!(
        merged.count(),
        4_096,
        "the merge lost or duplicated samples"
    );
    let want_median = sorted[sorted.len() / 2];
    let got_median = merged.quantile_nanos(0.50);
    assert!(
        got_median.abs_diff(want_median) <= want_median / crate::openloop::SUB_BUCKETS as u64 + 1,
        "the merged median {got_median}ns is not within the quantization bound of \
         the sorted median {want_median}ns"
    );
    assert_eq!(
        merged.min_nanos(),
        sorted[0],
        "the merge must report the exact minimum, not a bucketed one"
    );
    assert_eq!(
        merged.max_nanos(),
        sorted[sorted.len() - 1],
        "the merge must report the exact maximum, not a bucketed one"
    );
}

#[test]
fn sim_the_schedule_is_the_schedule_the_claim_names() {
    // The intended-arrival function is shared by the live driver and the model,
    // so this pins the arithmetic the whole open-loop correction rests on: at
    // 1,000/s the arrivals are exactly 1 ms apart, and a jittered arrival never
    // crosses into the next slot.
    let period = period_nanos(1_000);
    assert_eq!(
        period, 1_000_000,
        "1,000/s must be 1 ms apart, not {period}ns"
    );
    for index in 0..64_u64 {
        let uniform = intended_arrival(index, period, 0);
        assert_eq!(
            uniform,
            std::time::Duration::from_millis(index),
            "arrival {index} is not {index} ms after t0"
        );
        let jittered = intended_arrival(index, period, period - 1);
        assert!(
            jittered < intended_arrival(index + 1, period, 0),
            "a jittered arrival {index} ({jittered:?}) landed at or after the next \
             slot: a jitter that reorders arrivals is not jitter"
        );
    }
    assert_eq!(
        period_nanos(0),
        0,
        "a rate of zero must not divide by zero; the driver reads it as 'as fast as \
         this process can place work'"
    );
}
