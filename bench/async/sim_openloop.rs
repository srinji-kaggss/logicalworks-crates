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
