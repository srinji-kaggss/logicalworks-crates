//! Simulation family: bounded replay and cross-tenant journal isolation.
//!
//! Two properties of the durable journal that a seeded sweep can pin exactly:
//!
//! | Family | Property it pins |
//! |---|---|
//! | `streaming_replay` | the bounded, frame-at-a-time replay yields exactly the acknowledged history, and the materialized view agrees |
//! | `tenant_isolation` | two journals writing the **same effect key** keep only their own events, and one file cannot be shared |
//!
//! Both drive the real [`FileJournal`] — its real storage-owner thread, frame
//! codec and chain verification. Nothing here reimplements a journal: a second
//! one written for the simulation would pass while the shipped one rotted.
//!
//! One seed controls how many attempts the run writes; every seed must satisfy
//! the same properties, and each family sweeps its band twice and requires the
//! two trace hashes to be identical.

mod sim;

#[path = "sim/bands.rs"]
mod band_family;

use std::error::Error;

use lgwks_bot::journal::{EffectEvent, EffectJournal, FileJournal, JournalError};

use sim::Band;

use sim::rig::{attempt_key, ladder};

type TestResult = Result<(), Box<dyn Error>>;

/// The bounded replay yields exactly the acknowledged history.
///
/// [`FileJournal::replay`] reads frame by frame from its own descriptor and
/// retains one event at a time; the handle's `events()` borrows the history
/// `open` materialized. The two views must be byte-for-byte the same sequence,
/// and a torn tail must end the stream without inventing an event — which is
/// what makes the streaming door a replay of the record rather than a second
/// opinion about it.
fn streaming_replay(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("stream")?;
        let path = sim.journal_path(&dir, 0);
        let attempts = u64::from(sim.rng().between(1, 12));

        let mut expected: Vec<EffectEvent> = Vec::new();
        {
            let mut journal = FileJournal::open(&path)?;
            for n in 1..=attempts {
                for event in ladder(n)? {
                    let tail = journal.tail();
                    journal.compare_and_append(tail, &event)?;
                    expected.push(event);
                }
            }
        }

        let reopened = FileJournal::open(&path)?;
        let materialized: Vec<EffectEvent> = reopened.events().copied().collect();
        assert_eq!(
            materialized, expected,
            "the materialized history is not what was appended"
        );
        let mut streamed: Vec<EffectEvent> = Vec::new();
        for item in reopened.replay()? {
            streamed.push(item?);
        }
        assert_eq!(
            streamed, materialized,
            "the streaming replay diverged from the materialized history"
        );

        // The stream is bounded by its own descriptor; a second pass is a fresh
        // cursor and must reproduce the same sequence rather than resume.
        let mut again: Vec<EffectEvent> = Vec::new();
        for item in reopened.replay()? {
            again.push(item?);
        }
        assert_eq!(again, materialized, "a second replay pass diverged");

        sim.record("streaming-replay-matches-history");
        sim.trace
            .record_count("stream-attempts", usize::try_from(attempts)?);
        sim.trace.record_count("stream-events", streamed.len());
        Ok(())
    })
}

/// Two journals writing the same effect key stay isolated.
///
/// The key carries no tenant, so two journals that share a store would alias
/// the same attempt. They do not: each keeps only its own events, and the
/// second opener of one path is refused, so a tenant cannot be handed another
/// tenant's file and continue its ladder.
fn tenant_isolation_same_key(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("tenants")?;
        let first = sim.journal_path(&dir, 0);
        let second = sim.journal_path(&dir, 1);
        let attempts = u64::from(sim.rng().between(1, 8));

        let mut left = FileJournal::open(&first)?;
        let mut right = FileJournal::open(&second)?;
        for n in 1..=attempts {
            let event = EffectEvent::IntentAdmitted {
                key: attempt_key(n)?,
            };
            let tail = left.tail();
            left.compare_and_append(tail, &event)?;
            let tail = right.tail();
            right.compare_and_append(tail, &event)?;
        }

        assert_eq!(
            left.events().count(),
            usize::try_from(attempts)?,
            "the left tenant's own history is not exactly what it wrote"
        );
        assert_eq!(
            right.events().count(),
            usize::try_from(attempts)?,
            "the right tenant's own history is not exactly what it wrote"
        );
        // The two histories are separate sequences over the same keys, not a
        // shared one: each reopened journal replays to its own count.
        drop(left);
        drop(right);
        let reopened_left = FileJournal::open(&first)?;
        let reopened_right = FileJournal::open(&second)?;
        assert_eq!(
            reopened_left.events().count(),
            usize::try_from(attempts)?,
            "the left tenant's reopened history changed"
        );
        assert_eq!(
            reopened_right.events().count(),
            usize::try_from(attempts)?,
            "the right tenant's reopened history changed"
        );

        // The second opener of a file that is already a tenant's is refused, so
        // two tenants cannot be handed one store through this constructor.
        match FileJournal::open(&first) {
            Err(JournalError::Locked { .. }) => {}
            Err(other) => {
                return Err(format!("expected a lock refusal, got {other}").into());
            }
            Ok(_) => {
                return Err("a second tenant opened an already-owned journal".into());
            }
        }

        sim.record("same-key-tenants-isolated");
        sim.trace
            .record_count("tenant-attempts", usize::try_from(attempts)?);
        Ok(())
    })
}

use band_family::band_family;

band_family!(
    streaming_replay_r00 => streaming_replay, 0;
    streaming_replay_r01 => streaming_replay, 1;
    streaming_replay_r02 => streaming_replay, 2;
    streaming_replay_r03 => streaming_replay, 3;
    streaming_replay_r04 => streaming_replay, 4;
    streaming_replay_r05 => streaming_replay, 5;
    streaming_replay_r06 => streaming_replay, 6;
    streaming_replay_r07 => streaming_replay, 7;
    streaming_replay_r08 => streaming_replay, 8;
    streaming_replay_r09 => streaming_replay, 9;
    streaming_replay_r10 => streaming_replay, 10;
    streaming_replay_r11 => streaming_replay, 11;
    streaming_replay_r12 => streaming_replay, 12;
    streaming_replay_r13 => streaming_replay, 13;
    streaming_replay_r14 => streaming_replay, 14;
    streaming_replay_r15 => streaming_replay, 15;

    tenant_isolation_same_key_r00 => tenant_isolation_same_key, 0;
    tenant_isolation_same_key_r01 => tenant_isolation_same_key, 1;
    tenant_isolation_same_key_r02 => tenant_isolation_same_key, 2;
    tenant_isolation_same_key_r03 => tenant_isolation_same_key, 3;
    tenant_isolation_same_key_r04 => tenant_isolation_same_key, 4;
    tenant_isolation_same_key_r05 => tenant_isolation_same_key, 5;
    tenant_isolation_same_key_r06 => tenant_isolation_same_key, 6;
    tenant_isolation_same_key_r07 => tenant_isolation_same_key, 7;
    tenant_isolation_same_key_r08 => tenant_isolation_same_key, 8;
    tenant_isolation_same_key_r09 => tenant_isolation_same_key, 9;
    tenant_isolation_same_key_r10 => tenant_isolation_same_key, 10;
    tenant_isolation_same_key_r11 => tenant_isolation_same_key, 11;
    tenant_isolation_same_key_r12 => tenant_isolation_same_key, 12;
    tenant_isolation_same_key_r13 => tenant_isolation_same_key, 13;
    tenant_isolation_same_key_r14 => tenant_isolation_same_key, 14;
    tenant_isolation_same_key_r15 => tenant_isolation_same_key, 15;
);
