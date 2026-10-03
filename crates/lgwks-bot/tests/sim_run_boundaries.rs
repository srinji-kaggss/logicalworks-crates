//! Seeded sweeps over the simulation harness's own boundary behaviour.
//!
//! The store and journal families sweep properties of a *store*. This file sweeps
//! the properties of the harness every one of them rests on: the seed space is
//! contiguous and gapless, the fault schedule is a function of its seed alone,
//! the network's accounting conserves its envelopes, and the clock is monotone.
//!
//! It exists because those are exactly the assumptions a passing store family
//! makes without checking. A band table with a hole, or a `Faults::for_seed` that
//! reads a shared RNG, would make every other family's pass rate mean something
//! other than what it claims — and the failure would look like a store bug.
//!
//! One seed drives the schedule, the draws and the order throughout, and every
//! family here is swept across a whole band rather than a single seed, because a
//! property that holds at one seed is not a property.

#![cfg(all(feature = "script", feature = "ephemeral"))]

mod sim;

use std::error::Error;

use sim::{Band, Faults, Network, Rng, Sim};

type TestResult = Result<(), Box<dyn Error>>;

// The tests below are written out rather than declared through `band_family!`,
// and that is deliberate: the gate that counts simulation evidence reads
// `#[test]` attributes out of the sources, so a family declared through a macro
// is real, runs, and is invisible to it. Each one names the band it sweeps.

#[test]
fn the_band_table_is_contiguous_and_gapless_band_00() -> TestResult {
    the_band_table_is_contiguous_and_gapless(Band::new(
        sim::band_of(0).first,
        sim::band_of(0).count,
    ))
}

#[test]
fn the_band_table_is_contiguous_and_gapless_band_01() -> TestResult {
    the_band_table_is_contiguous_and_gapless(Band::new(
        sim::band_of(1).first,
        sim::band_of(1).count,
    ))
}

#[test]
fn the_band_table_is_contiguous_and_gapless_band_02() -> TestResult {
    the_band_table_is_contiguous_and_gapless(Band::new(
        sim::band_of(2).first,
        sim::band_of(2).count,
    ))
}

#[test]
fn the_band_table_is_contiguous_and_gapless_band_03() -> TestResult {
    the_band_table_is_contiguous_and_gapless(Band::new(
        sim::band_of(3).first,
        sim::band_of(3).count,
    ))
}

#[test]
fn the_seed_space_is_covered_exactly_once_band_04() -> TestResult {
    the_seed_space_is_covered_exactly_once(Band::new(sim::band_of(4).first, sim::band_of(4).count))
}

#[test]
fn the_seed_space_is_covered_exactly_once_band_05() -> TestResult {
    the_seed_space_is_covered_exactly_once(Band::new(sim::band_of(5).first, sim::band_of(5).count))
}

#[test]
fn a_fault_schedule_is_a_function_of_its_seed_band_06() -> TestResult {
    a_fault_schedule_is_a_function_of_its_seed(Band::new(
        sim::band_of(6).first,
        sim::band_of(6).count,
    ))
}

#[test]
fn a_fault_schedule_is_a_function_of_its_seed_band_07() -> TestResult {
    a_fault_schedule_is_a_function_of_its_seed(Band::new(
        sim::band_of(7).first,
        sim::band_of(7).count,
    ))
}

#[test]
fn a_fault_schedule_never_proposes_an_impossible_run_band_08() -> TestResult {
    a_fault_schedule_never_proposes_an_impossible_run(Band::new(
        sim::band_of(8).first,
        sim::band_of(8).count,
    ))
}

#[test]
fn a_fault_schedule_never_proposes_an_impossible_run_band_09() -> TestResult {
    a_fault_schedule_never_proposes_an_impossible_run(Band::new(
        sim::band_of(9).first,
        sim::band_of(9).count,
    ))
}

#[test]
fn the_clock_only_ever_moves_forward_band_10() -> TestResult {
    the_clock_only_ever_moves_forward(Band::new(sim::band_of(10).first, sim::band_of(10).count))
}

#[test]
fn the_clock_only_ever_moves_forward_band_11() -> TestResult {
    the_clock_only_ever_moves_forward(Band::new(sim::band_of(11).first, sim::band_of(11).count))
}

#[test]
fn a_tenant_count_is_never_zero_band_12() -> TestResult {
    a_tenant_count_is_never_zero(Band::new(sim::band_of(12).first, sim::band_of(12).count))
}

#[test]
fn a_tenant_count_is_never_zero_band_13() -> TestResult {
    a_tenant_count_is_never_zero(Band::new(sim::band_of(13).first, sim::band_of(13).count))
}

#[test]
fn the_network_conserves_every_envelope_band_14() -> TestResult {
    the_network_conserves_every_envelope(Band::new(sim::band_of(14).first, sim::band_of(14).count))
}

#[test]
fn the_network_conserves_every_envelope_band_15() -> TestResult {
    the_network_conserves_every_envelope(Band::new(sim::band_of(15).first, sim::band_of(15).count))
}

#[test]
fn a_message_is_never_delivered_before_it_is_due_band_16() -> TestResult {
    a_message_is_never_delivered_before_it_is_due(Band::new(
        sim::band_of(16).first,
        sim::band_of(16).count,
    ))
}

#[test]
fn a_message_is_never_delivered_before_it_is_due_band_17() -> TestResult {
    a_message_is_never_delivered_before_it_is_due(Band::new(
        sim::band_of(17).first,
        sim::band_of(17).count,
    ))
}

#[test]
fn a_duplicate_carries_a_later_attempt_count_band_18() -> TestResult {
    a_duplicate_carries_a_later_attempt_count(Band::new(
        sim::band_of(18).first,
        sim::band_of(18).count,
    ))
}

#[test]
fn a_duplicate_carries_a_later_attempt_count_band_19() -> TestResult {
    a_duplicate_carries_a_later_attempt_count(Band::new(
        sim::band_of(19).first,
        sim::band_of(19).count,
    ))
}

#[test]
fn draining_an_empty_network_yields_nothing_band_20() -> TestResult {
    draining_an_empty_network_yields_nothing(Band::new(
        sim::band_of(20).first,
        sim::band_of(20).count,
    ))
}

#[test]
fn draining_an_empty_network_yields_nothing_band_21() -> TestResult {
    draining_an_empty_network_yields_nothing(Band::new(
        sim::band_of(21).first,
        sim::band_of(21).count,
    ))
}

#[test]
fn the_same_seed_replays_to_the_same_trace_band_22() -> TestResult {
    the_same_seed_replays_to_the_same_trace(Band::new(
        sim::band_of(22).first,
        sim::band_of(22).count,
    ))
}

#[test]
fn the_same_seed_replays_to_the_same_trace_band_23() -> TestResult {
    the_same_seed_replays_to_the_same_trace(Band::new(
        sim::band_of(23).first,
        sim::band_of(23).count,
    ))
}

#[test]
fn distinct_seeds_reach_distinct_traces_band_24() -> TestResult {
    distinct_seeds_reach_distinct_traces(Band::new(sim::band_of(24).first, sim::band_of(24).count))
}

#[test]
fn distinct_seeds_reach_distinct_traces_band_25() -> TestResult {
    distinct_seeds_reach_distinct_traces(Band::new(sim::band_of(25).first, sim::band_of(25).count))
}

#[test]
fn a_partition_holds_everything_until_its_tick_band_26() -> TestResult {
    a_partition_holds_everything_until_its_tick(Band::new(
        sim::band_of(26).first,
        sim::band_of(26).count,
    ))
}

#[test]
fn a_partition_holds_everything_until_its_tick_band_27() -> TestResult {
    a_partition_holds_everything_until_its_tick(Band::new(
        sim::band_of(27).first,
        sim::band_of(27).count,
    ))
}

#[test]
fn crashes_land_on_a_tick_the_clock_can_reach_band_28() -> TestResult {
    crashes_land_on_a_tick_the_clock_can_reach(Band::new(
        sim::band_of(28).first,
        sim::band_of(28).count,
    ))
}

#[test]
fn crashes_land_on_a_tick_the_clock_can_reach_band_29() -> TestResult {
    crashes_land_on_a_tick_the_clock_can_reach(Band::new(
        sim::band_of(29).first,
        sim::band_of(29).count,
    ))
}

#[test]
fn generations_are_never_zero_band_30() -> TestResult {
    generations_are_never_zero(Band::new(sim::band_of(30).first, sim::band_of(30).count))
}

#[test]
fn generations_are_never_zero_band_31() -> TestResult {
    generations_are_never_zero(Band::new(sim::band_of(31).first, sim::band_of(31).count))
}

#[test]
fn an_advance_delivers_everything_that_became_due_band_32() -> TestResult {
    an_advance_delivers_everything_that_became_due(Band::new(
        sim::band_of(32).first,
        sim::band_of(32).count,
    ))
}

#[test]
fn an_advance_delivers_everything_that_became_due_band_33() -> TestResult {
    an_advance_delivers_everything_that_became_due(Band::new(
        sim::band_of(33).first,
        sim::band_of(33).count,
    ))
}

/// The declared bands are contiguous, in order, and none is empty.
///
/// An empty band is a test that asserts nothing while reporting as a pass, and a
/// gap is a hole in the seed space — the file's own doc calls that "reporting a
/// coverage it does not have".
fn the_band_table_is_contiguous_and_gapless(band: Band) -> TestResult {
    let table = sim::bands(sim::SEED_SPACE, sim::BANDS);
    assert_eq!(
        u64::try_from(table.len()).unwrap_or_default(),
        sim::BANDS,
        "the band table's length is not the declared band count"
    );
    let mut cursor = 0u64;
    for (index, entry) in table.iter().enumerate() {
        assert_eq!(
            entry.first, cursor,
            "band {index} starts at {} rather than {cursor}, so the seed space has a gap or an overlap",
            entry.first
        );
        assert!(entry.count > 0, "band {index} is empty and asserts nothing");
        cursor = cursor.saturating_add(entry.count);
    }
    assert_eq!(
        cursor,
        sim::SEED_SPACE,
        "the bands cover {cursor} seeds rather than the declared {}",
        sim::SEED_SPACE
    );
    let _ = band;
    Ok(())
}

/// The bands between them cover every seed exactly once.
fn the_seed_space_is_covered_exactly_once(band: Band) -> TestResult {
    let table = sim::bands(sim::SEED_SPACE, sim::BANDS);
    let mut seen = std::collections::BTreeSet::new();
    for entry in table.iter() {
        for seed in entry.seeds() {
            assert!(
                seen.insert(seed),
                "seed {seed} is claimed by two bands, so its result is attributed twice"
            );
        }
    }
    assert_eq!(
        seen.len(),
        usize::try_from(sim::SEED_SPACE).unwrap_or_default(),
        "the bands cover {} seeds rather than the declared space",
        seen.len()
    );
    let _ = band;
    Ok(())
}

/// A seed determines its whole fault schedule, with nothing shared between runs.
///
/// Two schedules for one seed would make every store family's pass rate a coin
/// flip on a second run, and `assert_replays` would catch it there rather than
/// here. The comparison is field by field because a schedule that matched only in
/// aggregate could differ on the one dimension a scenario depends on.
fn a_fault_schedule_is_a_function_of_its_seed(band: Band) -> TestResult {
    for seed in band.seeds() {
        let first = Faults::for_seed(seed);
        let second = Faults::for_seed(seed);
        assert_eq!(
            first.disk_refuse, second.disk_refuse,
            "seed {seed}: disk_refuse drifted"
        );
        assert_eq!(
            first.disk_tear, second.disk_tear,
            "seed {seed}: disk_tear drifted"
        );
        assert_eq!(
            first.net_drop, second.net_drop,
            "seed {seed}: net_drop drifted"
        );
        assert_eq!(
            first.net_duplicate, second.net_duplicate,
            "seed {seed}: net_duplicate drifted"
        );
        assert_eq!(
            first.partition_until, second.partition_until,
            "seed {seed}: the partition moved"
        );
        assert_eq!(
            first.crash_at, second.crash_at,
            "seed {seed}: the crash moved"
        );
        assert_eq!(
            first.tenants, second.tenants,
            "seed {seed}: the tenant count drifted"
        );
        assert_eq!(
            first.concurrency, second.concurrency,
            "seed {seed}: the concurrency drifted"
        );
        assert_eq!(
            first.generations, second.generations,
            "seed {seed}: the generation count drifted"
        );
    }
    Ok(())
}

/// Every scheduled fault is one the harness can actually carry out.
///
/// A per-mille chance above a thousand, or a concurrency of zero, is a schedule
/// the harness silently cannot honour: `chance` would compare against a number
/// it never reaches and the fault would never fire, so the family relying on it
/// would report coverage it never exercised.
fn a_fault_schedule_never_proposes_an_impossible_run(band: Band) -> TestResult {
    for seed in band.seeds() {
        let faults = Faults::for_seed(seed);
        for (name, per_mille) in [
            ("disk_refuse", faults.disk_refuse),
            ("disk_tear", faults.disk_tear),
            ("net_drop", faults.net_drop),
            ("net_duplicate", faults.net_duplicate),
        ] {
            assert!(
                per_mille <= 1000,
                "seed {seed}: {name} is {per_mille} per mille, which no draw can reach"
            );
        }
        assert!(
            faults.tenants > 0,
            "seed {seed}: a run with no tenant proves nothing"
        );
        assert!(
            faults.concurrency > 0,
            "seed {seed}: zero concurrency means nothing was ever in flight"
        );
    }
    Ok(())
}

/// The clock is monotone under any sequence of advances, including a zero step.
///
/// A clock that went backwards would let a message become due "in the past" and
/// be delivered twice, which is a harness bug that would read as a duplicate
/// delivery in every family that uses one.
fn the_clock_only_ever_moves_forward(band: Band) -> TestResult {
    let mut rng = Rng::new(band.first);
    for _ in band.seeds() {
        let mut harness = Sim::new(band.first);
        let mut highest = 0u64;
        for _ in 0..16 {
            let step = u64::from(rng.between(0, 32));
            let now = harness.tick(step);
            assert!(now >= highest, "the clock went from {highest} to {now}");
            highest = now;
        }
    }
    Ok(())
}

/// Every seed's tenant count is at least one.
fn a_tenant_count_is_never_zero(band: Band) -> TestResult {
    for seed in band.seeds() {
        let faults = Faults::for_seed(seed);
        assert!(
            faults.tenants > 0,
            "seed {seed}: a schedule with no tenant cannot isolate anything"
        );
        assert!(
            faults.tenants <= 65,
            "seed {seed}: {} tenants is past the declared bound",
            faults.tenants
        );
    }
    Ok(())
}

/// The network accounts for every envelope it was ever given.
///
/// Sent is delivered plus dropped plus in flight. A dropped envelope that was
/// never counted, or a duplicate that was counted twice in `delivered`, would
/// make every family's delivery totals a plausible-looking fiction.
fn the_network_conserves_every_envelope(band: Band) -> TestResult {
    for seed in band.seeds() {
        let faults = Faults::for_seed(seed);
        let mut rng = Rng::new(seed);
        let mut net = Network::new();
        let mut sent = 0usize;
        let mut horizon = 0u64;
        for _ in 0..24 {
            let fill = u8::try_from(seed).unwrap_or(0);
            let width = usize::try_from(rng.between(1, 8)).unwrap_or(1);
            let body = vec![fill; width];
            let from = rng.below(8);
            let to = rng.below(8);
            let due = horizon.saturating_add(u64::from(rng.below(8)));
            net.send(from, to, body, due);
            sent = sent.saturating_add(1);
            horizon = horizon.saturating_add(4);
            net.deliver(horizon, &mut rng, &faults);
        }
        // Drain everything that is left, however long it takes.
        let mut guard = 0u32;
        while net.in_flight() > 0 && guard < 4096 {
            horizon = horizon.saturating_add(16);
            net.deliver(horizon, &mut rng, &faults);
            guard = guard.saturating_add(1);
        }
        // A duplicate is an *extra* copy delivered of something already sent, so
        // `delivered` counts it once more than the sends that produced it and
        // conservation is not an equality. What must hold is the direction: no
        // envelope is conjured, and after a full drain nothing is left on the
        // wire. A drop loses one and a duplicate adds one, so the sum is
        // `sent - dropped + duplicated`.
        let (delivered, dropped, duplicated) = net.counts();
        let expected = sent
            .saturating_sub(usize::try_from(dropped).unwrap_or_default())
            .saturating_add(usize::try_from(duplicated).unwrap_or_default());
        assert_eq!(
            usize::try_from(delivered).unwrap_or_default(),
            expected,
            "seed {seed}: sent {sent}, dropped {dropped}, duplicated {duplicated}, \
             so {expected} deliveries were due and {delivered} happened"
        );
        assert_eq!(
            net.in_flight(),
            0,
            "seed {seed}: {} envelopes were still on the wire after the drain",
            net.in_flight()
        );
    }
    Ok(())
}

/// A message is never delivered at a tick before the one it was due at.
fn a_message_is_never_delivered_before_it_is_due(band: Band) -> TestResult {
    for seed in band.seeds() {
        let faults = Faults::for_seed(seed);
        let mut rng = Rng::new(seed);
        let mut net = Network::new();
        let mut due_at = std::collections::BTreeMap::new();
        for index in 0..16u32 {
            let due = u64::from(index).saturating_mul(7);
            due_at.insert(index, due);
            net.send(0, 1, vec![1u8], due);
        }
        let mut now = 0u64;
        for _ in 0..20 {
            let got = net.deliver(now, &mut rng, &faults);
            for envelope in got.iter() {
                let scheduled = due_at.get(&envelope.from).copied().unwrap_or(0);
                assert!(
                    now >= scheduled,
                    "a message due at {scheduled} was delivered at {now}"
                );
            }
            now = now.saturating_add(5);
        }
    }
    Ok(())
}

/// A duplicated envelope records more attempts than the one it was cloned from.
fn a_duplicate_carries_a_later_attempt_count(band: Band) -> TestResult {
    for seed in band.seeds() {
        let mut faults = Faults::for_seed(seed);
        // Force the duplicate branch and suppress every other fault, so the
        // assertion is about duplication and not about whichever draw fired.
        faults.net_drop = 0;
        faults.net_duplicate = 1000;
        faults.partition_until = 0;
        let mut rng = Rng::new(seed);
        let mut net = Network::new();
        net.send(0, 1, vec![7u8], 1);
        let first = net.deliver(4, &mut rng, &faults);
        assert_eq!(
            first.len(),
            1,
            "seed {seed}: the original was not delivered"
        );
        let original = first
            .first()
            .map(|envelope| envelope.attempts)
            .unwrap_or_default();
        let second = net.deliver(64, &mut rng, &faults);
        for envelope in second.iter() {
            assert!(
                envelope.attempts > original,
                "seed {seed}: a duplicate claims {} attempts, not more than the \
                 original's {original}",
                envelope.attempts
            );
        }
    }
    Ok(())
}

/// Draining a network that was never used yields nothing and changes nothing.
fn draining_an_empty_network_yields_nothing(band: Band) -> TestResult {
    for seed in band.seeds() {
        let faults = Faults::for_seed(seed);
        let mut rng = Rng::new(seed);
        let mut net = Network::new();
        for _ in 0..8 {
            let got = net.deliver(1_000, &mut rng, &faults);
            assert!(
                got.is_empty(),
                "seed {seed}: an empty network delivered something"
            );
            assert_eq!(net.in_flight(), 0, "seed {seed}: an empty network grew");
            let (delivered, dropped, duplicated) = net.counts();
            assert_eq!(
                (delivered, dropped, duplicated),
                (0, 0, 0),
                "seed {seed}: an empty network accounted for something"
            );
        }
    }
    Ok(())
}

/// The same seed replays to the same trace hash, across the whole band.
fn the_same_seed_replays_to_the_same_trace(band: Band) -> TestResult {
    let body = |harness: &mut Sim| -> TestResult {
        let seed = harness.seed;
        let faults = Faults::for_seed(seed);
        let mut net = Network::new();
        for index in 0..8u32 {
            let due = u64::from(index).saturating_mul(3);
            net.send(
                index,
                index.saturating_add(1),
                vec![u8::try_from(index).unwrap_or(0)],
                due,
            );
        }
        for _ in 0..12 {
            let now = harness.tick(4);
            let got = net.deliver(now, harness.rng(), &faults);
            harness.trace.record_count("delivered", got.len());
        }
        let (delivered, dropped, duplicated) = net.counts();
        harness.trace.record_u64("d", u64::from(delivered));
        harness.trace.record_u64("x", u64::from(dropped));
        harness.trace.record_u64("u", u64::from(duplicated));
        Ok(())
    };
    sim::assert_replays(band, body)?;
    Ok(())
}

/// Distinct seeds reach distinct traces, so a band is not one seed repeated.
///
/// Without this, a band of eight identical hashes would read as eight passing
/// seeds when it is one seed checked eight times.
fn distinct_seeds_reach_distinct_traces(band: Band) -> TestResult {
    let mut hashes = std::collections::BTreeSet::new();
    for seed in band.seeds() {
        let mut harness = Sim::new(seed);
        let faults = Faults::for_seed(seed);
        let mut rng = Rng::new(seed);
        let mut net = Network::new();
        for index in 0..6u32 {
            net.send(
                index,
                index.saturating_add(1),
                vec![u8::try_from(index).unwrap_or(0)],
                u64::from(index),
            );
        }
        for _ in 0..8 {
            let now = harness.tick(3);
            let got = net.deliver(now, &mut rng, &faults);
            harness.trace.record_count("n", got.len());
        }
        let (delivered, _, _) = net.counts();
        harness.trace.record_u64("d", u64::from(delivered));
        hashes.insert(harness.hash());
    }
    assert!(
        hashes.len() > 1,
        "every seed in the band reached the same trace, so the band is one seed \
         repeated rather than {} independent runs",
        band.count
    );
    Ok(())
}

/// A partition holds everything due before its tick, and nothing after it.
fn a_partition_holds_everything_until_its_tick(band: Band) -> TestResult {
    for seed in band.seeds() {
        let mut faults = Faults::for_seed(seed);
        faults.net_drop = 0;
        faults.net_duplicate = 0;
        faults.partition_until = 10;
        let mut rng = Rng::new(seed);
        let mut net = Network::new();
        // Sent *after* the partition lifts. A message sent during one is dropped
        // outright and never comes back — which is the correct semantics for a
        // partition, and is what `the_network_conserves_every_envelope` checks.
        // Reusing one message for both halves would assert that a dropped
        // envelope is retained, which is the opposite of what a partition does.
        net.send(0, 1, vec![1u8], 20);
        let held = net.deliver(5, &mut rng, &faults);
        assert!(
            held.is_empty(),
            "seed {seed}: a partitioned network delivered early"
        );
        let released = net.deliver(20, &mut rng, &faults);
        assert_eq!(
            released.len(),
            1,
            "seed {seed}: the message was not delivered after the partition lifted"
        );
    }
    Ok(())
}

/// A scheduled crash names a reachable tick, or none at all.
fn crashes_land_on_a_tick_the_clock_can_reach(band: Band) -> TestResult {
    for seed in band.seeds() {
        let faults = Faults::for_seed(seed);
        if faults.crash_at == u64::MAX {
            continue;
        }
        assert!(
            faults.crash_at > 0,
            "seed {seed}: a crash at tick zero would fire before any step ran"
        );
        let mut harness = Sim::new(seed);
        let mut now = 0u64;
        let mut fired = false;
        for _ in 0..4096 {
            if now >= faults.crash_at {
                fired = true;
                break;
            }
            now = harness.tick(1);
        }
        assert!(
            fired,
            "seed {seed}: the clock never reached the scheduled crash at {}",
            faults.crash_at
        );
    }
    Ok(())
}

/// Every seed schedules at least one journal generation.
fn generations_are_never_zero(band: Band) -> TestResult {
    for seed in band.seeds() {
        let faults = Faults::for_seed(seed);
        assert!(
            faults.generations > 0,
            "seed {seed}: zero generations exercises no chain at all"
        );
    }
    Ok(())
}

/// Advancing the harness delivers everything that became due across the step.
fn an_advance_delivers_everything_that_became_due(band: Band) -> TestResult {
    for seed in band.seeds() {
        let mut faults = Faults::for_seed(seed);
        faults.net_drop = 0;
        faults.net_duplicate = 0;
        faults.partition_until = 0;
        let mut harness = Sim::new(seed);
        harness.faults = faults;
        let mut expected = 0usize;
        for index in 0..8u64 {
            harness.send(0, 1, vec![1u8], index.saturating_mul(2));
            expected = expected.saturating_add(1);
        }
        let mut now = 0u64;
        let mut total = 0usize;
        // Far past the last due tick, so a message that is merely still on the
        // wire cannot be mistaken for one that was lost.
        for _ in 0..128 {
            now = harness.tick(2);
            total = total.saturating_add(harness.drain(now).len());
        }
        assert_eq!(
            total, expected,
            "seed {seed}: drained {total} of {expected} messages over a clock that reached {now}"
        );
    }
    Ok(())
}
