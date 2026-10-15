//! The shipped record store under disk faults, as a deterministic simulation.
//!
//! Issue #278 row 5 asks for concurrent editors of shared records with no lost
//! update; `sim_cas` covers the schedules. This family covers what the disk
//! does to the same store: a crash between writes, a temp file torn by a crash
//! before its rename, a deleted store, a directory that vanishes under a write,
//! a hot record that overruns the conflict log, many tenants, and many threads.
//!
//! One seed drives every draw: the record count, the operations, which writes
//! are stale, where the crash lands and how much of a torn file survives. The
//! store is the real [`RecordStore`] on a real temp file, and a plain model —
//! a map of versions and values plus a conflict count — says what the store
//! must hold after every step. Every scenario runs twice per seed and the two
//! traces must hash alike, so a run that depends on anything but its seed
//! fails even when its assertions pass. Conflict stamps are wall-clock
//! evidence, so they never enter a trace.
//!
//! Seeds per scenario: [`SEEDS`], fewer for the scenarios whose every step is
//! a contended file rewrite, each named at its test.

use std::collections::BTreeMap;
use std::error::Error;
use std::path::{Path, PathBuf};

use lgwks_bot::cas::{CasError, MAX_CONFLICTS, NO_VERSION, RecordStore, StoreState};

use crate::scratch::Scratch;
use crate::sim::{Rng, Trace};
use crate::sweep_fixtures::{FIRST_SEED, refuse};

pub(crate) type TestResult = Result<(), Box<dyn Error>>;

/// Seeds per scenario: every operation is an atomic file rewrite, so the
/// sweep is sized to stay a few seconds per scenario on a laptop disk.
pub(crate) const SEEDS: u64 = 256;

/// Run `body` twice per seed and require the two traces to hash alike.
pub(crate) fn sweep(
    name: &str,
    seeds: u64,
    mut body: impl FnMut(u64, &mut Trace) -> TestResult,
) -> TestResult {
    for index in 0..seeds {
        let seed = FIRST_SEED.saturating_add(index);
        let mut first = Trace::new();
        first.record_number("seed", seed);
        body(seed, &mut first).map_err(|error| format!("{name}: seed {seed}: {error}"))?;
        let mut again = Trace::new();
        again.record_number("seed", seed);
        body(seed, &mut again).map_err(|error| format!("{name}: seed {seed}: {error}"))?;
        if first.hash() != again.hash() {
            return refuse(format!(
                "{name}: seed {seed} did not replay to the same trace hash"
            ));
        }
    }
    Ok(())
}

/// What the store must hold: each record's version and value, and how many
/// conflicts were recorded in all (retained plus evicted).
#[derive(Debug, Default, Clone)]
pub(crate) struct Model {
    records: BTreeMap<String, (u64, String)>,
    conflicts: u64,
}

impl Model {
    /// The version the model holds for `name`; a record never written is at
    /// [`NO_VERSION`].
    pub(crate) fn version(&self, name: &str) -> u64 {
        match self.records.get(name) {
            Some(&(version, _)) => version,
            None => NO_VERSION,
        }
    }

    /// Fold in a write the store reported applied.
    pub(crate) fn applied(&mut self, name: String, version: u64, value: String) {
        self.records.insert(name, (version, value));
    }

    /// Fold in a conflict the store reported recorded.
    pub(crate) fn conflicted(&mut self) {
        self.conflicts = self.conflicts.saturating_add(1);
    }
}

/// The store file inside `dir`.
pub(crate) fn store_path(dir: &Path) -> PathBuf {
    dir.join("store.json")
}

/// Refuse unless `state` holds exactly what `model` says.
pub(crate) fn agrees(state: &StoreState, model: &Model) -> TestResult {
    if state.records().len() != model.records.len() {
        return refuse(format!(
            "the store holds {} records, the model {}",
            state.records().len(),
            model.records.len()
        ));
    }
    for (name, &(version, ref value)) in &model.records {
        let Some(record) = state.records().get(name) else {
            return refuse(format!("record {name} is missing from the store"));
        };
        if record.version() != version || record.value() != value.as_str() {
            return refuse(format!(
                "record {name} is at {} = {:?}, the model says {version} = {value:?}",
                record.version(),
                record.value()
            ));
        }
    }
    let held = u64::try_from(state.conflicts().len())?.saturating_add(state.evicted());
    if held != model.conflicts {
        return refuse(format!(
            "the store accounts for {held} conflicts, the model recorded {}",
            model.conflicts
        ));
    }
    Ok(())
}

/// One seeded write, checked against the model and folded into it.
///
/// `fresh_per_mille` is the chance the write names the current version; the
/// rest name a version the record does not hold and must conflict.
pub(crate) fn step(
    rng: &mut Rng,
    store: &RecordStore,
    model: &mut Model,
    records: u32,
    fresh_per_mille: u32,
    trace: &mut Trace,
) -> TestResult {
    let name = format!("record-{}", rng.below(records));
    let current = model.version(&name);
    let fresh = rng.chance(fresh_per_mille);
    let expected = if fresh {
        current
    } else {
        current
            .saturating_add(1)
            .saturating_add(u64::from(rng.below(3)))
    };
    let value = format!("v{}", rng.below(1_000_000));
    match store.write(&name, expected, &value) {
        Ok(version) if fresh => {
            if version != current.saturating_add(1) {
                return refuse(format!(
                    "a write to {name} naming {current} installed {version}"
                ));
            }
            model.applied(name, version, value);
            trace.record_number("applied", version);
        }
        Ok(version) => {
            return refuse(format!(
                "a stale write to {name} naming {expected} against {current} installed {version}"
            ));
        }
        Err(CasError::Conflict {
            expected: named,
            found,
            ..
        }) if !fresh => {
            if named != expected || found != current {
                return refuse(format!(
                    "a conflict on {name} named {named}/{found}, the write saw {expected} against {current}"
                ));
            }
            model.conflicted();
            trace.record_number("conflict", found);
        }
        Err(error) => {
            return refuse(format!(
                "a write to {name} (fresh: {fresh}) failed: {error}"
            ));
        }
    }
    Ok(())
}

/// A seeded store left on disk: at least one record, then a seeded mix of
/// fresh and stale writes. Returns the directory, the store path and the
/// model of what the file holds.
pub(crate) fn seeded_store(
    rng: &mut Rng,
    tag: &str,
    trace: &mut Trace,
) -> Result<(Scratch, PathBuf, Model, u32), Box<dyn Error>> {
    let dir = Scratch::new(tag)?;
    let path = store_path(dir.path());
    let records = rng.between(1, 5);
    let mut model = Model::default();
    let store = RecordStore::open(&path)?;
    step(rng, &store, &mut model, records, 1000, trace)?;
    for _ in 0..rng.between(0, 16) {
        step(rng, &store, &mut model, records, 650, trace)?;
    }
    drop(store);
    unstamp(&path)?;
    Ok((dir, path, model, records))
}

/// Replace every conflict's wall-clock stamp in the file with one fixed
/// value, so the file's bytes are a function of the seed alone.
///
/// The store writes the instant each conflict lost, and a damage offset drawn
/// against a length that moves with the clock would not replay. Decoding
/// ignores the stamp (it reopens as the epoch), so the state is unchanged.
fn unstamp(path: &Path) -> TestResult {
    let mut document: lgwks_std::json::Value =
        lgwks_std::json::from_str(&std::fs::read_to_string(path)?)?;
    let conflicts = document
        .get_mut("conflicts")
        .and_then(lgwks_std::json::Value::as_array_mut)
        .ok_or("the store holds a conflicts array")?;
    for entry in conflicts {
        let entry = entry.as_object_mut().ok_or("every conflict is an object")?;
        entry.insert("stamped".to_owned(), lgwks_std::json::Value::from("epoch"));
    }
    std::fs::write(path, lgwks_std::json::to_string_pretty(&document)?)?;
    Ok(())
}

/// A seeded permutation, drawn from the seed rather than the scheduler.
fn shuffle<T>(rng: &mut Rng, items: &mut [T]) -> TestResult {
    for index in (1..items.len()).rev() {
        let drawn = usize::try_from(rng.below(u32::try_from(index.saturating_add(1))?))?;
        items.swap(index, drawn);
    }
    Ok(())
}

/// The retained conflict log as plain facts, oldest first.
fn conflict_log(state: &StoreState) -> Vec<(String, u64, u64)> {
    state
        .conflicts()
        .iter()
        .map(|entry| (entry.record().to_owned(), entry.expected(), entry.found()))
        .collect()
}

/// Seeded writes with crashes between them: after every reopen the file
/// holds exactly what every reported outcome said.
#[test]
fn random_operations_match_the_model_across_reopens() -> TestResult {
    sweep("model", SEEDS, |seed, trace| {
        let mut rng = Rng::new(seed);
        let dir = Scratch::new("sim-cas-disk")?;
        let path = store_path(dir.path());
        let records = rng.between(1, 6);
        let mut model = Model::default();
        let mut store = RecordStore::open(&path)?;
        for _ in 0..rng.between(4, 24) {
            if rng.chance(150) {
                drop(store);
                store = RecordStore::open(&path)?;
                agrees(&store.state(), &model)?;
                trace.record("reopened");
            }
            step(&mut rng, &store, &mut model, records, 700, trace)?;
        }
        agrees(&store.state(), &model)?;
        drop(store);
        agrees(&RecordStore::open(&path)?.state(), &model)
    })
}

/// A crash between writing the temp file and renaming it leaves a torn temp
/// file beside a complete store; the store, not the temp file, is read back.
#[test]
fn a_torn_temp_file_leaves_the_previous_state_readable() -> TestResult {
    sweep("torn-read", SEEDS, |seed, trace| {
        let mut rng = Rng::new(seed);
        let (_dir, path, model, _records) = seeded_store(&mut rng, "sim-cas-torn", trace)?;
        tear(&mut rng, &path, trace)?;
        agrees(&RecordStore::open(&path)?.state(), &model)
    })
}

/// The write after a torn temp file replaces it, and the store it leaves
/// reopens to the model.
#[test]
fn the_next_write_replaces_a_torn_temp_file() -> TestResult {
    sweep("torn-replace", SEEDS, |seed, trace| {
        let mut rng = Rng::new(seed);
        let (_dir, path, mut model, records) = seeded_store(&mut rng, "sim-cas-torn", trace)?;
        tear(&mut rng, &path, trace)?;
        let store = RecordStore::open(&path)?;
        step(&mut rng, &store, &mut model, records, 1000, trace)?;
        if path.with_extension("tmp").exists() {
            return refuse("the write after a torn temp file left it in place");
        }
        drop(store);
        agrees(&RecordStore::open(&path)?.state(), &model)
    })
}

/// Leave a torn temp file beside `path`: a seeded prefix of the store's own
/// bytes, as a crash before the rename would.
fn tear(rng: &mut Rng, path: &Path, trace: &mut Trace) -> TestResult {
    let whole = std::fs::read(path)?;
    let cut = usize::try_from(rng.below(u32::try_from(whole.len())?))?;
    let torn = whole
        .get(..cut)
        .ok_or("a cut below the length is in range")?;
    std::fs::write(path.with_extension("tmp"), torn)?;
    trace.record_number("torn at", cut);
    Ok(())
}

/// A deleted store is absence, not corruption: it reopens empty and a first
/// write creates at version one.
#[test]
fn a_deleted_store_reopens_empty() -> TestResult {
    sweep("deleted", SEEDS, |seed, trace| {
        let mut rng = Rng::new(seed);
        let (_dir, path, _model, _records) = seeded_store(&mut rng, "sim-cas-deleted", trace)?;
        std::fs::remove_file(&path)?;
        let store = RecordStore::open(&path)?;
        agrees(&store.state(), &Model::default())?;
        let version = store.write("record-0", NO_VERSION, "again")?;
        if version != 1 {
            return refuse(format!("a first write after deletion installed {version}"));
        }
        trace.record_number("recreated", version);
        Ok(())
    })
}

/// A write whose persist fails applies nothing: the store's state is what it
/// was, and the next write names the version the failed one named.
#[test]
fn a_failed_persist_applies_nothing() -> TestResult {
    sweep("failed-persist", SEEDS, |seed, trace| {
        failed_persist(seed, 1000, trace)
    })
}

/// A conflict whose persist fails records nothing: no conflict entry is held
/// that the file never received.
#[test]
fn a_failed_conflict_persist_records_nothing() -> TestResult {
    sweep("failed-conflict", SEEDS, |seed, trace| {
        failed_persist(seed, 0, trace)
    })
}

/// Remove the store's directory, attempt one seeded write, and require an I/O
/// refusal that leaves the state as the model holds it.
fn failed_persist(seed: u64, fresh_per_mille: u32, trace: &mut Trace) -> TestResult {
    let mut rng = Rng::new(seed);
    let dir = Scratch::new("sim-cas-persist")?;
    let sub = dir.path().join("store");
    std::fs::create_dir_all(&sub)?;
    let path = store_path(&sub);
    let store = RecordStore::open(&path)?;
    let records = rng.between(1, 4);
    let mut model = Model::default();
    for _ in 0..rng.between(0, 12) {
        step(&mut rng, &store, &mut model, records, 700, trace)?;
    }
    std::fs::remove_dir_all(&sub)?;
    let name = format!("record-{}", rng.below(records));
    let current = model.version(&name);
    let expected = if rng.chance(fresh_per_mille) {
        current
    } else {
        current.saturating_add(1)
    };
    match store.write(&name, expected, "never persisted") {
        Err(CasError::Io { .. }) => trace.record("persist refused"),
        other => {
            return refuse(format!(
                "a write with no directory to persist into returned {other:?}"
            ));
        }
    }
    agrees(&store.state(), &model)?;
    std::fs::create_dir_all(&sub)?;
    step(&mut rng, &store, &mut model, records, 1000, trace)?;
    drop(store);
    agrees(&RecordStore::open(&path)?.state(), &model)
}

/// A hot record overruns the conflict log: the log holds exactly the cap, the
/// eviction count says how many were discarded, and both survive a reopen.
/// Sixteen seeds: every loser is a whole-file rewrite.
#[test]
fn the_conflict_log_is_bounded_and_counts_evictions() -> TestResult {
    sweep("bound", 16, |seed, trace| {
        let mut rng = Rng::new(seed);
        let dir = Scratch::new("sim-cas-bound")?;
        let path = store_path(dir.path());
        let store = RecordStore::open(&path)?;
        store.write("hot", NO_VERSION, "winner")?;
        let cap = u32::try_from(MAX_CONFLICTS)?;
        let losers = rng.between(cap.saturating_add(1), cap.saturating_mul(2));
        for _ in 0..losers {
            match store.write("hot", NO_VERSION, "loser") {
                Err(CasError::Conflict { .. }) => {}
                other => {
                    return refuse(format!(
                        "a stale write to the hot record returned {other:?}"
                    ));
                }
            }
        }
        let evicted = u64::from(losers.saturating_sub(cap));
        let state = store.state();
        drop(store);
        for (when, held) in [
            ("live", state),
            ("reopened", RecordStore::open(&path)?.state()),
        ] {
            if held.conflicts().len() != MAX_CONFLICTS || held.evicted() != evicted {
                return refuse(format!(
                    "{when}: {} conflicts retained and {} evicted, expected {MAX_CONFLICTS} and {evicted}",
                    held.conflicts().len(),
                    held.evicted()
                ));
            }
        }
        trace.record_number("evicted", evicted);
        Ok(())
    })
}

/// The retained conflicts reopen as the same facts in the same order.
#[test]
fn retained_conflicts_reopen_in_order() -> TestResult {
    sweep("conflict-order", SEEDS, |seed, trace| {
        let mut rng = Rng::new(seed);
        let dir = Scratch::new("sim-cas-order")?;
        let path = store_path(dir.path());
        let store = RecordStore::open(&path)?;
        let records = rng.between(1, 3);
        let mut model = Model::default();
        for _ in 0..rng.between(4, 40) {
            step(&mut rng, &store, &mut model, records, 300, trace)?;
        }
        let live = conflict_log(&store.state());
        drop(store);
        let reopened = conflict_log(&RecordStore::open(&path)?.state());
        if live != reopened {
            return refuse("the reopened conflict log differs from the live one");
        }
        trace.record_number("conflicts", live.len());
        Ok(())
    })
}

/// One state persists as one byte sequence, whatever order its records were
/// written in.
#[test]
fn one_state_persists_as_one_byte_sequence() -> TestResult {
    sweep("bytes", SEEDS, |seed, trace| {
        let mut rng = Rng::new(seed);
        let dir = Scratch::new("sim-cas-bytes")?;
        let mut entries = Vec::new();
        for index in 0..rng.between(1, 8) {
            entries.push((
                format!("record-{index}"),
                format!("v{}", rng.below(1_000_000)),
            ));
        }
        let mut copies = Vec::new();
        for copy in 0..2 {
            shuffle(&mut rng, &mut entries)?;
            let path = dir.path().join(format!("copy-{copy}.json"));
            let store = RecordStore::open(&path)?;
            for entry in &entries {
                store.write(&entry.0, NO_VERSION, &entry.1)?;
            }
            drop(store);
            copies.push(std::fs::read(&path)?);
        }
        let (Some(first), Some(second)) = (copies.first(), copies.get(1)) else {
            return refuse("two copies were written");
        };
        if first != second {
            return refuse("one state persisted as two different byte sequences");
        }
        trace.record_number("bytes", first.len());
        Ok(())
    })
}

/// Tenants on separate stores use the same record names and never see each
/// other's writes, through every reopen.
#[test]
fn tenants_on_separate_stores_share_no_record() -> TestResult {
    sweep("tenants", SEEDS, |seed, trace| {
        let mut rng = Rng::new(seed);
        let dir = Scratch::new("sim-cas-tenants")?;
        let tenants = rng.between(2, 6);
        let tenant_path = |tenant: u32| dir.path().join(format!("tenant-{tenant}.json"));
        let mut stores = Vec::new();
        let mut models = Vec::new();
        for tenant in 0..tenants {
            stores.push(RecordStore::open(tenant_path(tenant))?);
            models.push(Model::default());
        }
        let records = rng.between(1, 4);
        for _ in 0..rng.between(8, 32) {
            let tenant = rng.below(tenants);
            let index = usize::try_from(tenant)?;
            let (Some(store), Some(model)) = (stores.get(index), models.get_mut(index)) else {
                return refuse(format!("tenant {tenant} is out of range"));
            };
            trace.record_number("tenant", tenant);
            step(&mut rng, store, model, records, 700, trace)?;
        }
        drop(stores);
        for (tenant, model) in (0..tenants).zip(&models) {
            agrees(&RecordStore::open(tenant_path(tenant))?.state(), model)?;
        }
        Ok(())
    })
}

/// Threads bumping shared records through one store lose no update: each
/// record ends at exactly the number of bumps aimed at it. Which writes
/// conflict depends on the scheduler, so only the outcome is traced.
#[test]
fn threads_on_one_store_lose_no_update() -> TestResult {
    sweep("threads", 64, |seed, trace| {
        let mut rng = Rng::new(seed);
        let dir = Scratch::new("sim-cas-threads")?;
        let path = store_path(dir.path());
        let store = RecordStore::open(&path)?;
        let records = rng.between(1, 3);
        let mut plan = Vec::new();
        for _ in 0..rng.between(2, 8) {
            plan.push((format!("record-{}", rng.below(records)), rng.between(1, 6)));
        }
        let outcomes: Vec<Result<(), String>> = std::thread::scope(|scope| {
            let handles: Vec<_> = plan
                .iter()
                .map(|&(ref name, bumps)| {
                    let store = &store;
                    scope.spawn(move || bump(store, name, bumps))
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| match handle.join() {
                    Ok(outcome) => outcome,
                    Err(_) => Err("a writer thread panicked".to_owned()),
                })
                .collect()
        });
        for outcome in outcomes {
            outcome?;
        }
        let mut aimed: BTreeMap<&str, u64> = BTreeMap::new();
        for &(ref name, bumps) in &plan {
            let total = aimed.entry(name.as_str()).or_default();
            *total = total.saturating_add(u64::from(bumps));
        }
        let state = store.state();
        if state.conflicts().len() > MAX_CONFLICTS {
            return refuse("the conflict log outgrew its cap under threads");
        }
        for (name, total) in aimed {
            let held = match state.records().get(name) {
                Some(record) => record.version(),
                None => NO_VERSION,
            };
            if held != total {
                return refuse(format!("{name} ended at {held} after {total} bumps"));
            }
            trace.record_number(name, held);
        }
        drop(store);
        let reopened = RecordStore::open(&path)?.state();
        if reopened.records() != state.records() {
            return refuse("the reopened records differ from the live ones");
        }
        Ok(())
    })
}

/// Bump `name` `bumps` times, re-reading after every conflict.
fn bump(store: &RecordStore, name: &str, bumps: u32) -> Result<(), String> {
    let mut left = bumps;
    let mut attempts = 0_u32;
    while left > 0 {
        attempts = attempts.saturating_add(1);
        if attempts > 100_000 {
            return give_up(format!("{name}: no progress after 100,000 attempts"));
        }
        let seen = match store.state().records().get(name) {
            Some(record) => record.version(),
            None => NO_VERSION,
        };
        match store.write(name, seen, "bumped") {
            Ok(_) => left = left.saturating_sub(1),
            Err(CasError::Conflict { .. }) => {}
            Err(error) => return give_up(format!("{name}: {error}")),
        }
    }
    Ok(())
}

/// A writer thread's refusal, emitted before it is returned: a `String`
/// because a thread's result must be `Send`.
fn give_up(cause: String) -> Result<(), String> {
    let refusal = Err(cause);
    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "bump: a writer thread gave up");
    refusal
}
