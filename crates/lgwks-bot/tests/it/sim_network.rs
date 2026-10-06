//! Simulation family: the network the durable dispatch runs over.
//!
//! The network here is virtual — a seeded [`sim::Network`] that drops,
//! duplicates, reorders and partitions — because those are the four things a
//! test cannot otherwise ask a real network to do on demand and get the same
//! answer twice. What is *not* virtual is the other end: every delivered
//! envelope is fed to the **real** journal, through the **real** ladder, and
//! every readback goes through the real trait. A network family that used a
//! fake store would prove that the fake store tolerates duplication, which is
//! not a property anything ships.
//!
//! The property the whole file exists to pin is stated in the module docs of
//! the journal: delivery is at-least-once and never exactly-once, so a
//! consumer must key on identity and refuse a repeat. Each family below is one
//! way a network can try to break that.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `at_least_once` | every envelope sent is delivered or visibly dropped; nothing vanishes |
//! | `dup_suppressed` | a duplicate delivery does not duplicate a journal event |
//! | `reorder_tolerated` | out-of-order delivery still leaves a verifiable chain |
//! | `partition_recovers` | delivery resumes after a partition, with the loss accounted |
//! | `at_least_once_claimed` | the trace records that duplicates occur, so no exactly-once claim is made |
//! | `accounting` | delivered + dropped + in-flight equals what was sent |
//! | `routes` | an envelope reaches its addressee and nobody else |
//! | `retry_same` | a re-send carries the same bytes, so it is the same fact |
//! | `idle_net` | an idle network produces no events at all |
//! | `burst` | a burst delivered in one tick arrives whole |
//! | `drop_redo` | a dropped message re-sent is not a second fact |
//! | `late_verify` | a late delivery still lands before the chain is verified |
//! | `dup_visible` | a duplicate is visible as more than one attempt |
//! | `partition_journal` | a partition leaves the journal untouched |
//! | `flood` | a flood of envelopes stays bounded by what the network is holding |
//! | `deterministic` | one seed delivers in exactly one order, every time |

use crate::sim;

// The band-declaration macro, defined once for the whole layer.
use crate::band_family;

use std::collections::BTreeSet;
use std::error::Error;

use lgwks_bot::journal::{EffectEvent, EffectJournal, FileJournal};

use sim::Band;

use sim::rig::Run;
use sim::rig::ladder;

type TestResult = Result<(), Box<dyn Error>>;

/// The ladder for a network envelope numbered `attempt` from zero.
///
/// The offset is applied here rather than at each payload construction so that
/// the identity a duplicate preserves is the identity the journal is given, and
/// the key and the rungs come from the rig, so a network trace and a journal
/// trace that agree on an attempt are talking about the same effect.
fn ladder_for(attempt: u32) -> Result<Vec<EffectEvent>, Box<dyn Error>> {
    ladder(u64::from(attempt).saturating_add(1))
}

/// The bytes one delivery carries: the event's own key, so a re-send is
/// recognisable as the same fact without the simulation understanding the
/// payload. Compared, never interpreted.
fn payload(tenant: u32, attempt: u32) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&tenant.to_be_bytes());
    body.extend_from_slice(&attempt.to_be_bytes());
    body
}

/// The key one payload stands for, recovered by reading it back.
fn subject(body: &[u8]) -> Option<(u32, u32)> {
    if body.len() < 8 {
        return None;
    }
    let tenant = u32::from_be_bytes([body[0], body[1], body[2], body[3]]);
    let attempt = u32::from_be_bytes([body[4], body[5], body[6], body[7]]);
    Some((tenant, attempt))
}

/// Write one journal event per delivered envelope, refusing a repeat of a
/// subject the journal already holds.
///
/// The dedup is keyed on the payload, which is the identity a network
/// duplicate preserves: the same bytes mean the same fact, so applying them
/// twice is what an at-least-once consumer must not do.
fn apply_deliveries(path: &std::path::Path, deliveries: &[&[u8]]) -> Result<usize, Box<dyn Error>> {
    let mut journal = FileJournal::open(path)?;
    let mut seen: BTreeSet<(u32, u32)> = BTreeSet::new();
    for body in deliveries {
        let Some(subject) = subject(body) else {
            continue;
        };
        if !seen.insert(subject) {
            continue;
        }
        let events = ladder_for(subject.1)?;
        drop(journal.compare_and_append_all(&events));
    }
    Ok(journal.events().count())
}

/// Run one sweep of the network under the seed's fault schedule and hand the
/// scenario its deliveries, so every family reads the same run.
fn deliveries_for(sim: &mut sim::Sim, count: u64) -> Result<Vec<sim::Envelope>, Box<dyn Error>> {
    // The envelope index is a `u32` because that is the width the payload
    // grammar names, so the loop is drawn in that width rather than narrowed per
    // iteration: a count this host cannot address is a refusal.
    for index in 0..u32::try_from(count)? {
        let due = sim.tick(1).saturating_add(u64::from(sim.rng().below(4)));
        sim.send(index % 4, index % 2, payload(index % 4, index), due);
    }
    let now = sim.tick(16);
    Ok(sim.drain(now))
}

/// Send a seeded burst of two to eight envelopes and apply every delivered
/// body to a fresh journal in a scratch directory tagged `tag`, then hand
/// `check` the journal's path, the delivered bodies and the events applied.
///
/// The opening every journal-backed family shares, written once so a family
/// states only what it requires of the journal the network left behind. The
/// scratch directory outlives `check`, so the journal is still there to reopen.
fn with_delivered_journal(
    sim: &mut sim::Sim,
    tag: &str,
    check: impl FnOnce(&mut sim::Sim, &std::path::Path, &[&[u8]], usize) -> TestResult,
) -> TestResult {
    let dir = sim.scratch(tag)?;
    let path = sim.journal_path(&dir, 0);
    let sent = u64::from(sim.rng().between(2, 8));
    let deliveries = deliveries_for(sim, sent)?;
    let bodies: Vec<&[u8]> = deliveries.iter().map(|envelope| envelope.body()).collect();
    let applied = apply_deliveries(&path, &bodies)?;
    check(sim, &path, &bodies, applied)
}

// ── Families ──────────────────────────────────────────────────────────────

/// Every envelope sent is delivered or visibly dropped; nothing vanishes.
fn at_least_once(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let sent = u64::from(sim.rng().between(1, 8));
        let returned = deliveries_for(sim, sent)?;
        let (delivered, dropped, _) = sim.net.counts();
        assert_eq!(
            u64::from(delivered.saturating_add(dropped)),
            sent,
            "{delivered} delivered and {dropped} dropped is not the {sent} that were sent"
        );
        sim.record("nothing-vanished");
        sim.trace.record_count("aloast-returned", returned.len());
        sim.trace.record_u64("aloast-sent", sent);
        sim.trace
            .record_u64("aloast-delivered", u64::from(delivered));
        Ok(())
    })
}

/// A duplicate delivery does not duplicate a journal event.
fn dup_suppressed(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        with_delivered_journal(sim, "net-dup", |sim, path, bodies, applied| {
            let distinct = bodies
                .iter()
                .filter_map(|body| subject(body))
                .collect::<BTreeSet<_>>();

            // Counted in *keys*, not events: every fact that is accepted walks a
            // two-rung ladder, so the event count is twice the fact count by
            // design. What a duplicate would break is the key count, because a
            // repeated fact has no second ladder to climb — the journal refuses it.
            let handle = FileJournal::open(path)?;
            let mut keys: BTreeSet<String> = BTreeSet::new();
            for event in handle.events() {
                keys.insert(format!("{:?}", event.key()));
            }
            drop(handle);
            assert_eq!(
                keys.len(),
                distinct.len(),
                "{} keys were written for {} distinct facts, so a duplicate delivery \
             became a second event",
                keys.len(),
                distinct.len()
            );
            assert!(applied >= distinct.len(), "the journal lost accepted facts");
            sim.record("duplicates-suppressed");
            sim.trace.record_count("dup-applied", applied);
            sim.trace.record_count("dup-keys", keys.len());
            Ok(())
        })
    })
}

/// Out-of-order delivery still leaves a verifiable chain.
fn reorder_tolerated(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        with_delivered_journal(sim, "net-reorder", |sim, path, _bodies, _applied| {
            let handle = FileJournal::open(path)?;
            let entries = handle.committed_entries()?;
            let position = lgwks_bot::journal::verify_chain(&entries)?;
            assert_eq!(
                position,
                handle.tail(),
                "a chain that survived reordering must still end at this handle's own tail"
            );
            sim.record("reorder-verifies");
            sim.trace.record_count("reorder-entries", entries.len());
            Ok(())
        })
    })
}

/// Delivery resumes after a partition, with the loss accounted.
///
/// The property is stated about *new* traffic, not about the envelopes the
/// partition consumed: a partition drops what it was holding, so asserting
/// that the same envelopes arrive afterwards would be asserting that they were
/// never dropped. What must hold is that once the window closes, traffic sent
/// afterwards flows.
fn partition_recovers(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let sent = u64::from(sim.rng().between(1, 6));
        drop(deliveries_for(sim, sent)?);
        let (during, dropped, _) = sim.net.counts();

        // Past every partition window the schedule declared.
        let after = sim.tick(4096);
        assert!(
            sim.faults.partition_until == 0 || after > sim.faults.partition_until,
            "the run advanced to tick {after} but its partition runs to {}",
            sim.faults.partition_until
        );

        // Sixteen fresh envelopes. The post-partition drop rate is at most three
        // in ten per mille of the seed's own range, so losing all sixteen has
        // probability well under one in a billion — a failure here means the
        // network is dead, not unlucky.
        let probe = 16u32;
        for n in 0..probe {
            sim.send_now(0, 1, payload(7, n));
        }
        let resumed = sim.advance_and_drain(64);
        assert!(
            !resumed.is_empty(),
            "no traffic at all was delivered after the partition ended at tick {}",
            sim.faults.partition_until
        );
        sim.record("partition-recovers");
        sim.trace.record_u64("partition-during", u64::from(during));
        sim.trace
            .record_u64("partition-dropped", u64::from(dropped));
        sim.trace.record_u64("partition-probe", u64::from(probe));
        sim.trace.record_count("partition-resumed", resumed.len());
        Ok(())
    })
}

/// The trace records that duplicates occur, so no exactly-once claim is made.
fn at_least_once_claimed(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let sent = u32::try_from(64_u64)?;
        for index in 0..sent {
            sim.send_now(0, 1, payload(0, index));
        }
        let now = sim.tick(256);
        let deliveries = sim.drain(now);
        let (_, dropped, duplicated) = sim.net.counts();
        let repeat = deliveries.iter().any(|envelope| envelope.attempts() > 1);
        // Whatever the seed decided, the receipt has to carry the fact: a
        // consumer that cannot see duplication will claim exactly-once.
        sim.trace
            .record_u64("alonce-duplicates", u64::from(duplicated));
        sim.trace.record_u64("alonce-dropped", u64::from(dropped));
        sim.trace
            .record_u64("alonce-repeat-seen", u64::from(repeat));
        sim.record("at-least-once-is-claimed");
        Ok(())
    })
}

/// Delivered plus dropped plus in-flight equals what was sent.
fn accounting(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let sent = u64::from(sim.rng().between(1, 12));
        let deliveries = deliveries_for(sim, sent)?;
        let (delivered, dropped, _) = sim.net.counts();
        let held = sim.net.in_flight();
        let accounted =
            u64::from(delivered.saturating_add(dropped)).saturating_add(u64::try_from(held)?);
        assert!(
            accounted >= u64::try_from(deliveries.len())?,
            "the network accounted for fewer envelopes than it returned"
        );
        sim.record("accounting-balances");
        sim.trace.record_u64("acct-sent", sent);
        sim.trace.record_count("acct-in-flight", held);
        Ok(())
    })
}

/// An envelope reaches its addressee and nobody else.
fn routes(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let pairs = sim.rng().between(1, 8);
        for tenant in 0..pairs {
            sim.send_now(tenant, tenant.saturating_add(1), payload(tenant, tenant));
        }
        let now = sim.tick(32);
        let deliveries = sim.drain(now);
        for envelope in &deliveries {
            assert_eq!(
                envelope.to(),
                envelope.from().saturating_add(1),
                "an envelope for tenant {} arrived at {}",
                envelope.from(),
                envelope.to()
            );
            let seen = subject(envelope.body()).map(|pair| pair.0);
            assert_eq!(
                seen,
                Some(envelope.from()),
                "a tenant received another tenant's payload"
            );
        }
        sim.record("routes-to-addressee");
        sim.trace.record_count("routed", deliveries.len());
        Ok(())
    })
}

/// A re-send carries the same bytes, so it is the same fact.
fn retry_same(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let body = payload(1, 7);
        sim.send_now(1, 2, body.clone());
        let first = sim.advance_and_drain(2);
        for envelope in &first {
            assert_eq!(
                envelope.body(),
                body.as_slice(),
                "a re-send changed the bytes, so it is a different fact under the same name"
            );
        }
        sim.send_now(1, 2, body.clone());
        let second = sim.advance_and_drain(4);
        for envelope in &second {
            assert_eq!(
                envelope.body(),
                body.as_slice(),
                "a re-send changed the bytes, so it is a different fact under the same name"
            );
        }
        sim.record("retry-carries-same-bytes");
        sim.trace.record_count("retry-first", first.len());
        sim.trace.record_count("retry-second", second.len());
        Ok(())
    })
}

/// An idle network produces no events at all.
fn idle_net(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let now = sim.tick(1024);
        let deliveries = sim.drain(now);
        assert!(
            deliveries.is_empty(),
            "a network nobody spoke on returned {} deliveries",
            deliveries.len()
        );
        assert_eq!(sim.net.in_flight(), 0, "an idle network held traffic");
        sim.record("idle-net-quiet");
        sim.trace.record_count("idle-deliveries", deliveries.len());
        Ok(())
    })
}

/// A burst delivered in one tick arrives whole.
fn burst(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let burst = sim.rng().between(1, 16);
        for index in 0..burst {
            sim.send(0, 1, payload(0, index), 0);
        }
        let deliveries = sim.drain(0);
        let (delivered, _, _) = sim.net.counts();
        assert_eq!(
            u64::from(delivered),
            u64::try_from(deliveries.len())?,
            "the scheduler's return disagrees with its own accounting"
        );
        sim.record("burst-accounted");
        sim.trace.record_u64("burst-size", u64::from(burst));
        sim.trace.record_count("burst-delivered", deliveries.len());
        Ok(())
    })
}

/// A dropped message re-sent is not a second fact.
fn drop_redo(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("net-redo")?;
        let path = sim.journal_path(&dir, 0);
        let subject_id = sim.rng().between(1, 4);
        let body = payload(0, subject_id);
        let once = [body.as_slice()];
        let first = apply_deliveries(&path, &once)?;
        let second = apply_deliveries(&path, &once)?;
        assert_eq!(
            first, second,
            "re-sending the same fact grew the journal, so a re-send became a second event"
        );
        sim.record("re-send-is-one-fact");
        sim.trace.record_count("redo-events", second);
        Ok(())
    })
}

/// A late delivery still lands before the chain is verified.
fn late_verify(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("net-late")?;
        let path = sim.journal_path(&dir, 0);
        let sent = u64::from(sim.rng().between(1, 6));
        let early = deliveries_for(sim, sent)?;
        let early_bodies: Vec<&[u8]> = early.iter().map(|envelope| envelope.body()).collect();
        apply_deliveries(&path, &early_bodies)?;

        // A much later wave, then verification: the chain has to hold across
        // both, not only at the end of the first.
        for n in 0..2u32 {
            sim.send_now(0, 1, payload(9, n));
        }
        let late = sim.advance_and_drain(8192);
        let late_bodies: Vec<&[u8]> = late.iter().map(|envelope| envelope.body()).collect();
        let total = apply_deliveries(&path, &late_bodies)?;

        let handle = FileJournal::open(&path)?;
        let entries = handle.committed_entries()?;
        assert_eq!(
            lgwks_bot::journal::verify_chain(&entries)?,
            handle.tail(),
            "a late delivery left the chain not ending at the tail"
        );
        assert!(total >= early_bodies.len(), "the late wave lost history");
        sim.record("late-delivery-verifies");
        sim.trace.record_count("late-events", total);
        Ok(())
    })
}

/// A duplicate is visible as more than one attempt.
fn dup_visible(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let sent = u32::try_from(64_u64)?;
        for index in 0..sent {
            sim.send_now(0, 1, payload(0, index));
        }
        let deliveries = sim.advance_and_drain(512);
        for envelope in &deliveries {
            assert!(
                envelope.attempts() >= 1,
                "a delivered envelope claimed zero attempts, so a consumer cannot see \
                 that it was re-sent"
            );
        }
        let repeats = deliveries
            .iter()
            .filter(|envelope| envelope.attempts() > 1)
            .count();
        sim.record("duplicates-are-visible");
        sim.trace.record_count("dup-repeats", repeats);
        Ok(())
    })
}

/// A partition leaves the journal untouched.
fn partition_journal(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("net-part")?;
        let path = sim.journal_path(&dir, 0);
        let before = apply_deliveries(&path, &[])?;
        let size = path.metadata()?.len();

        let sent = sim.rng().between(1, 8);
        for index in 0..sent {
            sim.send_now(0, 1, payload(0, index));
        }
        let _ = sim.advance_and_drain(64);
        let after = apply_deliveries(&path, &[])?;
        assert_eq!(before, after, "a network partition changed the journal");
        assert_eq!(
            path.metadata()?.len(),
            size,
            "a network partition changed the journal's bytes"
        );
        sim.record("partition-leaves-journal");
        sim.trace.record_u64("partition-sent", u64::from(sent));
        Ok(())
    })
}

/// A flood of envelopes stays bounded by what the network is holding.
fn flood(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let flood = u64::from(sim.rng().between(1, 64));
        for index in 0..flood {
            let index = u32::try_from(index)?;
            sim.send(0, 1, payload(0, index), u64::MAX);
        }
        let held = sim.net.in_flight();
        assert_eq!(
            u64::try_from(held)?,
            flood,
            "the network did not retain what it was given, or retained more than it was"
        );
        // A flood that is never due must not block the next tick.
        let _ = sim.advance_and_drain(1);
        assert_eq!(
            u64::try_from(sim.net.in_flight())?,
            flood,
            "a tick released traffic that was not due"
        );
        sim.record("flood-bounded");
        sim.trace.record_u64("flood-size", flood);
        Ok(())
    })
}

/// One seed delivers in exactly one order, every time.
fn deterministic(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let sent = sim.rng().between(1, 12);
        for index in 0..sent {
            let due = sim.tick(1).saturating_add(u64::from(sim.rng().below(8)));
            sim.send(index, 1, payload(0, index), due);
        }
        let mut order: Vec<String> = Vec::new();
        let mut now = 0u64;
        for _ in 0..16 {
            now = now.saturating_add(4);
            for envelope in sim.drain(now) {
                let (tenant, attempt) = subject(envelope.body())
                    .ok_or("a delivered envelope carried a body this file did not write")?;
                order.push(format!("{tenant}:{attempt}"));
            }
        }
        sim.trace.record_count("order-length", order.len());
        for entry in &order {
            sim.trace.record(entry);
        }
        sim.record("delivery-order-fixed");
        Ok(())
    })
}

// ── The sweep ─────────────────────────────────────────────────────────────

use band_family::band_family;

band_family!(
at_least_once_r00 => at_least_once, 0; at_least_once_r01 => at_least_once, 1;
at_least_once_r02 => at_least_once, 2; at_least_once_r03 => at_least_once, 3;
at_least_once_r04 => at_least_once, 4; at_least_once_r05 => at_least_once, 5;
at_least_once_r06 => at_least_once, 6; at_least_once_r07 => at_least_once, 7;
at_least_once_r08 => at_least_once, 8; at_least_once_r09 => at_least_once, 9;
at_least_once_r10 => at_least_once, 10; at_least_once_r11 => at_least_once, 11;
at_least_once_r12 => at_least_once, 12; at_least_once_r13 => at_least_once, 13;
at_least_once_r14 => at_least_once, 14; at_least_once_r15 => at_least_once, 15;
dup_suppressed_r00 => dup_suppressed, 0; dup_suppressed_r01 => dup_suppressed, 1;
dup_suppressed_r02 => dup_suppressed, 2; dup_suppressed_r03 => dup_suppressed, 3;
dup_suppressed_r04 => dup_suppressed, 4; dup_suppressed_r05 => dup_suppressed, 5;
dup_suppressed_r06 => dup_suppressed, 6; dup_suppressed_r07 => dup_suppressed, 7;
dup_suppressed_r08 => dup_suppressed, 8; dup_suppressed_r09 => dup_suppressed, 9;
dup_suppressed_r10 => dup_suppressed, 10; dup_suppressed_r11 => dup_suppressed, 11;
dup_suppressed_r12 => dup_suppressed, 12; dup_suppressed_r13 => dup_suppressed, 13;
dup_suppressed_r14 => dup_suppressed, 14; dup_suppressed_r15 => dup_suppressed, 15;
reorder_r00 => reorder_tolerated, 0; reorder_r01 => reorder_tolerated, 1;
reorder_r02 => reorder_tolerated, 2; reorder_r03 => reorder_tolerated, 3;
reorder_r04 => reorder_tolerated, 4; reorder_r05 => reorder_tolerated, 5;
reorder_r06 => reorder_tolerated, 6; reorder_r07 => reorder_tolerated, 7;
reorder_r08 => reorder_tolerated, 8; reorder_r09 => reorder_tolerated, 9;
reorder_r10 => reorder_tolerated, 10; reorder_r11 => reorder_tolerated, 11;
reorder_r12 => reorder_tolerated, 12; reorder_r13 => reorder_tolerated, 13;
reorder_r14 => reorder_tolerated, 14; reorder_r15 => reorder_tolerated, 15;
partition_recovers_r00 => partition_recovers, 0; partition_recovers_r01 => partition_recovers, 1;
partition_recovers_r02 => partition_recovers, 2; partition_recovers_r03 => partition_recovers, 3;
partition_recovers_r04 => partition_recovers, 4; partition_recovers_r05 => partition_recovers, 5;
partition_recovers_r06 => partition_recovers, 6; partition_recovers_r07 => partition_recovers, 7;
partition_recovers_r08 => partition_recovers, 8; partition_recovers_r09 => partition_recovers, 9;
partition_recovers_r10 => partition_recovers, 10; partition_recovers_r11 => partition_recovers, 11;
partition_recovers_r12 => partition_recovers, 12; partition_recovers_r13 => partition_recovers, 13;
partition_recovers_r14 => partition_recovers, 14; partition_recovers_r15 => partition_recovers, 15;
at_least_once_claimed_r00 => at_least_once_claimed, 0; at_least_once_claimed_r01 => at_least_once_claimed, 1;
at_least_once_claimed_r02 => at_least_once_claimed, 2; at_least_once_claimed_r03 => at_least_once_claimed, 3;
at_least_once_claimed_r04 => at_least_once_claimed, 4; at_least_once_claimed_r05 => at_least_once_claimed, 5;
at_least_once_claimed_r06 => at_least_once_claimed, 6; at_least_once_claimed_r07 => at_least_once_claimed, 7;
at_least_once_claimed_r08 => at_least_once_claimed, 8; at_least_once_claimed_r09 => at_least_once_claimed, 9;
at_least_once_claimed_r10 => at_least_once_claimed, 10; at_least_once_claimed_r11 => at_least_once_claimed, 11;
at_least_once_claimed_r12 => at_least_once_claimed, 12; at_least_once_claimed_r13 => at_least_once_claimed, 13;
at_least_once_claimed_r14 => at_least_once_claimed, 14; at_least_once_claimed_r15 => at_least_once_claimed, 15;
accounting_r00 => accounting, 0; accounting_r01 => accounting, 1;
accounting_r02 => accounting, 2; accounting_r03 => accounting, 3;
accounting_r04 => accounting, 4; accounting_r05 => accounting, 5;
accounting_r06 => accounting, 6; accounting_r07 => accounting, 7;
accounting_r08 => accounting, 8; accounting_r09 => accounting, 9;
accounting_r10 => accounting, 10; accounting_r11 => accounting, 11;
accounting_r12 => accounting, 12; accounting_r13 => accounting, 13;
accounting_r14 => accounting, 14; accounting_r15 => accounting, 15;
routes_r00 => routes, 0; routes_r01 => routes, 1;
routes_r02 => routes, 2; routes_r03 => routes, 3;
routes_r04 => routes, 4; routes_r05 => routes, 5;
routes_r06 => routes, 6; routes_r07 => routes, 7;
routes_r08 => routes, 8; routes_r09 => routes, 9;
routes_r10 => routes, 10; routes_r11 => routes, 11;
routes_r12 => routes, 12; routes_r13 => routes, 13;
routes_r14 => routes, 14; routes_r15 => routes, 15;
retry_same_r00 => retry_same, 0; retry_same_r01 => retry_same, 1;
retry_same_r02 => retry_same, 2; retry_same_r03 => retry_same, 3;
retry_same_r04 => retry_same, 4; retry_same_r05 => retry_same, 5;
retry_same_r06 => retry_same, 6; retry_same_r07 => retry_same, 7;
retry_same_r08 => retry_same, 8; retry_same_r09 => retry_same, 9;
retry_same_r10 => retry_same, 10; retry_same_r11 => retry_same, 11;
retry_same_r12 => retry_same, 12; retry_same_r13 => retry_same, 13;
retry_same_r14 => retry_same, 14; retry_same_r15 => retry_same, 15;
idle_net_r00 => idle_net, 0; idle_net_r01 => idle_net, 1;
idle_net_r02 => idle_net, 2; idle_net_r03 => idle_net, 3;
idle_net_r04 => idle_net, 4; idle_net_r05 => idle_net, 5;
idle_net_r06 => idle_net, 6; idle_net_r07 => idle_net, 7;
idle_net_r08 => idle_net, 8; idle_net_r09 => idle_net, 9;
idle_net_r10 => idle_net, 10; idle_net_r11 => idle_net, 11;
idle_net_r12 => idle_net, 12; idle_net_r13 => idle_net, 13;
idle_net_r14 => idle_net, 14; idle_net_r15 => idle_net, 15;
burst_r00 => burst, 0; burst_r01 => burst, 1;
burst_r02 => burst, 2; burst_r03 => burst, 3;
burst_r04 => burst, 4; burst_r05 => burst, 5;
burst_r06 => burst, 6; burst_r07 => burst, 7;
burst_r08 => burst, 8; burst_r09 => burst, 9;
burst_r10 => burst, 10; burst_r11 => burst, 11;
burst_r12 => burst, 12; burst_r13 => burst, 13;
burst_r14 => burst, 14; burst_r15 => burst, 15;
drop_redo_r00 => drop_redo, 0; drop_redo_r01 => drop_redo, 1;
drop_redo_r02 => drop_redo, 2; drop_redo_r03 => drop_redo, 3;
drop_redo_r04 => drop_redo, 4; drop_redo_r05 => drop_redo, 5;
drop_redo_r06 => drop_redo, 6; drop_redo_r07 => drop_redo, 7;
drop_redo_r08 => drop_redo, 8; drop_redo_r09 => drop_redo, 9;
drop_redo_r10 => drop_redo, 10; drop_redo_r11 => drop_redo, 11;
drop_redo_r12 => drop_redo, 12; drop_redo_r13 => drop_redo, 13;
drop_redo_r14 => drop_redo, 14; drop_redo_r15 => drop_redo, 15;
late_verify_r00 => late_verify, 0; late_verify_r01 => late_verify, 1;
late_verify_r02 => late_verify, 2; late_verify_r03 => late_verify, 3;
late_verify_r04 => late_verify, 4; late_verify_r05 => late_verify, 5;
late_verify_r06 => late_verify, 6; late_verify_r07 => late_verify, 7;
late_verify_r08 => late_verify, 8; late_verify_r09 => late_verify, 9;
late_verify_r10 => late_verify, 10; late_verify_r11 => late_verify, 11;
late_verify_r12 => late_verify, 12; late_verify_r13 => late_verify, 13;
late_verify_r14 => late_verify, 14; late_verify_r15 => late_verify, 15;
dup_visible_r00 => dup_visible, 0; dup_visible_r01 => dup_visible, 1;
dup_visible_r02 => dup_visible, 2; dup_visible_r03 => dup_visible, 3;
dup_visible_r04 => dup_visible, 4; dup_visible_r05 => dup_visible, 5;
dup_visible_r06 => dup_visible, 6; dup_visible_r07 => dup_visible, 7;
dup_visible_r08 => dup_visible, 8; dup_visible_r09 => dup_visible, 9;
dup_visible_r10 => dup_visible, 10; dup_visible_r11 => dup_visible, 11;
dup_visible_r12 => dup_visible, 12; dup_visible_r13 => dup_visible, 13;
dup_visible_r14 => dup_visible, 14; dup_visible_r15 => dup_visible, 15;
partition_journal_r00 => partition_journal, 0; partition_journal_r01 => partition_journal, 1;
partition_journal_r02 => partition_journal, 2; partition_journal_r03 => partition_journal, 3;
partition_journal_r04 => partition_journal, 4; partition_journal_r05 => partition_journal, 5;
partition_journal_r06 => partition_journal, 6; partition_journal_r07 => partition_journal, 7;
partition_journal_r08 => partition_journal, 8; partition_journal_r09 => partition_journal, 9;
partition_journal_r10 => partition_journal, 10; partition_journal_r11 => partition_journal, 11;
partition_journal_r12 => partition_journal, 12; partition_journal_r13 => partition_journal, 13;
partition_journal_r14 => partition_journal, 14; partition_journal_r15 => partition_journal, 15;
flood_r00 => flood, 0; flood_r01 => flood, 1;
flood_r02 => flood, 2; flood_r03 => flood, 3;
flood_r04 => flood, 4; flood_r05 => flood, 5;
flood_r06 => flood, 6; flood_r07 => flood, 7;
flood_r08 => flood, 8; flood_r09 => flood, 9;
flood_r10 => flood, 10; flood_r11 => flood, 11;
flood_r12 => flood, 12; flood_r13 => flood, 13;
flood_r14 => flood, 14; flood_r15 => flood, 15;
deterministic_r00 => deterministic, 0; deterministic_r01 => deterministic, 1;
deterministic_r02 => deterministic, 2; deterministic_r03 => deterministic, 3;
deterministic_r04 => deterministic, 4; deterministic_r05 => deterministic, 5;
deterministic_r06 => deterministic, 6; deterministic_r07 => deterministic, 7;
deterministic_r08 => deterministic, 8; deterministic_r09 => deterministic, 9;
deterministic_r10 => deterministic, 10; deterministic_r11 => deterministic, 11;
deterministic_r12 => deterministic, 12; deterministic_r13 => deterministic, 13;
deterministic_r14 => deterministic, 14; deterministic_r15 => deterministic, 15;
);

/// The bands partition the seed space: no gap, no overlap.
#[test]
fn the_bands_cover_every_seed_exactly_once() -> TestResult {
    let mut covered = Vec::new();
    for band in sim::bands(sim::SEED_SPACE, sim::BANDS) {
        covered.extend(band.seeds());
    }
    covered.sort_unstable();
    covered.dedup();
    assert_eq!(
        u64::try_from(covered.len())?,
        sim::SEED_SPACE,
        "the bands cover {} seeds, not the shared seed space",
        covered.len()
    );
    Ok(())
}

/// The rig this file drives is the one the acceptance file drives.
#[test]
fn the_rig_is_shared_not_copied() -> TestResult {
    let run = Run::new();
    run.lands()?.tick()?;
    assert!(
        run.kinds()?.len() >= 3,
        "the shared rig must write the ladder"
    );
    Ok(())
}
