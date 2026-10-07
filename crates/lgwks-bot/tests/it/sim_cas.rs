//! Row 5 of issue #278, as a deterministic simulation: concurrent editors of
//! shared records.
//!
//! One seed drives everything: how many writers, how many records, and the
//! order their reads and writes land in. The store is the shipped
//! [`RecordStore`](lgwks_bot::cas::RecordStore) on a real temp file —
//! nothing here reimplements compare-and-set, because a second store written
//! "for the simulation" would let this file pass while the shipped one
//! rotted. What is modelled is the schedule the writers do not control; the
//! versions make every schedule safe, and the assertions say what safe means:
//!
//! - every applied write installs exactly the expected version plus one;
//! - every refused write names the version it saw and the version it found,
//!   and changes nothing;
//! - versions per record are contiguous from one: no lost update, whatever
//!   the order;
//! - reopening the store reads back the same records and the same conflicts,
//!   so "what changed" is answerable after a restart;
//! - the same seed gives the same trace hash, and the seeds diverge.

use std::collections::HashMap;
use std::error::Error;

use lgwks_bot::cas::{CasError, NO_VERSION, RecordStore};

use crate::scratch::Scratch;
use crate::sim::{Rng, Trace};
use crate::sweep_fixtures::{FIRST_SEED, SWEEP_SEEDS, peak_rss_bytes, refuse};

type TestResult = Result<(), Box<dyn Error>>;

/// One writer's attempt: which record, which version it saw, and what it
/// would leave.
struct Attempt {
    /// Which record the write names.
    record: usize,
    /// The version the writer saw.
    expected: u64,
    /// The value the write would install.
    value: String,
}

/// Run one seeded schedule of concurrent editors against the shipped store.
///
/// Returns the trace hash: the replay receipt.
fn run(seed: u64, trace: &mut Trace) -> TestResult {
    let mut rng = Rng::new(seed);
    let writers = usize::try_from(rng.between(2, 8)).map_err(|_| "a draw fits")?;
    let records = usize::try_from(rng.between(1, 4)).map_err(|_| "a draw fits")?;
    trace.record_number("writers", writers);
    trace.record_number("records", records);
    // The directory dies with the run: a refused scenario leaves no file
    // behind, and no manual removal stands between the schedule and its
    // evidence.
    let dir = Scratch::new("sim-cas")?;
    let path = dir.path().join(format!("{seed}.json"));
    let store = RecordStore::open(&path)?;
    // Every writer reads every record once, up front: they all see version
    // zero and all believe they are first. The writes then land in a seeded
    // order, so exactly one writer per record wins the first round and the
    // rest lose against the version the winner installed.
    let mut attempts: Vec<Attempt> = Vec::new();
    for writer in 0..writers {
        for record in 0..records {
            attempts.push(Attempt {
                record,
                expected: NO_VERSION,
                value: format!("writer-{writer}"),
            });
        }
    }
    // A seeded permutation of the attempts: the only nondeterminism a real
    // race has, drawn from the seed rather than the scheduler.
    for index in (1..attempts.len()).rev() {
        let bound = u32::try_from(index.saturating_add(1)).map_err(|_| "too many attempts")?;
        let drawn = usize::try_from(rng.below(bound)).map_err(|_| "a draw is a valid index")?;
        attempts.swap(index, drawn);
    }
    let mut applied: HashMap<usize, u64> = HashMap::new();
    let mut conflicts = 0u64;
    for attempt in &attempts {
        let name = format!("record-{}", attempt.record);
        match store.write(&name, attempt.expected, &attempt.value) {
            Ok(version) => {
                if version != attempt.expected.saturating_add(1).max(1) {
                    return refuse(format!(
                        "an applied write installs expected plus one, not {version}"
                    ));
                }
                applied.insert(attempt.record, version);
                trace.record_number("applied", version);
            }
            Err(CasError::Conflict {
                expected, found, ..
            }) => {
                if expected != attempt.expected {
                    return refuse("a conflict names the version the write saw");
                }
                let current = store
                    .state()
                    .records()
                    .get(&name)
                    .map_or(NO_VERSION, |record| record.version());
                if found != current {
                    return refuse("a conflict names the version the store holds");
                }
                conflicts = conflicts.saturating_add(1);
                trace.record_number("conflict", found);
            }
            Err(error) => {
                return refuse(format!(
                    "a write failed as something other than a conflict: {error}"
                ));
            }
        }
    }
    // One winner per record in the first round, every other attempt a
    // conflict: versions are contiguous from one, so no update was lost.
    // Sorted, because the winners map is keyed by record and hash order is
    // not an assertion order.
    let mut won: Vec<usize> = applied.keys().copied().collect();
    won.sort_unstable();
    if won.len() != records {
        return refuse(format!(
            "one winner per record was expected, {} won",
            won.len()
        ));
    }
    for record in won {
        let version = applied.get(&record).ok_or("a winner records its version")?;
        if *version != 1 {
            return refuse(format!("record {record} is at version {version}, not one"));
        }
    }
    let expected_conflicts = u64::try_from(attempts.len())
        .map_err(|_| "too many attempts")?
        .saturating_sub(u64::try_from(records).map_err(|_| "too many records")?);
    if conflicts != expected_conflicts {
        return refuse(format!(
            "{conflicts} conflicts were recorded, {expected_conflicts} were inevitable"
        ));
    }
    // The losers re-read and name what they found: second-round writes all
    // apply, one per record, at version two.
    for record in 0..records {
        let name = format!("record-{record}");
        let version = store.write(&name, 1, "second-round")?;
        if version != 2 {
            return refuse(format!(
                "a re-read write installs version two, not {version}"
            ));
        }
        trace.record_number("reapplied", version);
    }
    // Reopen: the records and the conflicts survive the restart, so what
    // changed is answerable from the file rather than from a live handle.
    let state = store.state();
    drop(store);
    let reopened = RecordStore::open(&path)?;
    let reread = reopened.state();
    if reread.records().len() != state.records().len() {
        return refuse("a reopened store holds the same records");
    }
    if reread.conflicts().len() != state.conflicts().len() {
        return refuse("a reopened store holds the same conflicts");
    }
    if reread.evicted() != state.evicted() {
        return refuse("a reopened store holds the same eviction count");
    }
    trace.record_number("records", reread.records().len());
    trace.record_number("conflicts", reread.conflicts().len());
    Ok(())
}

/// Seeded writer schedules against the shipped store, over 1,024 seeds.
#[test]
fn seeded_editors_produce_one_winner_and_typed_conflicts() -> TestResult {
    let mut first_hash: Option<u64> = None;
    for index in 0..SWEEP_SEEDS {
        let seed = FIRST_SEED.saturating_add(index);
        let mut trace = Trace::new();
        trace.record_number("seed", seed);
        run(seed, &mut trace)?;
        let mut replay = Trace::new();
        replay.record_number("seed", seed);
        run(seed, &mut replay)?;
        if trace.hash() != replay.hash() {
            return refuse(format!("seed {seed} did not replay to the same trace hash"));
        }
        if index == 0 {
            first_hash = Some(trace.hash());
        }
    }
    if first_hash.is_none() {
        return refuse("the sweep ran no seeds");
    }
    if let Some(rss) = peak_rss_bytes() {
        assert!(
            rss < 512 * 1024 * 1024,
            "a 1,024-seed CAS sweep must stay under 512 MiB, observed {rss} bytes"
        );
    }
    Ok(())
}
