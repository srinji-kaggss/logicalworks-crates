//! The record store's verb adapter under disk faults and simulated time.
//!
//! `sim_cas_disk` drives [`RecordStore`] directly; this family drives the same
//! store through [`CasWrite`], the [`Execute`] adapter a bot actually calls, so
//! what is under test is the mapping a caller sees: a lost write is
//! [`BotError::Conflict`] and never retried blindly, a persist the disk refuses
//! is a refused [`BotError::DomainError`] that applied nothing, a proof that
//! does not cover the store's capability writes nothing, and a credential is
//! honoured exactly until its lifetime runs out on a virtual [`Clock`].
//!
//! One seed drives every draw, and every scenario replays twice per seed to
//! the same trace hash ([`sweep`]). The clock is virtual, so a lifetime of a
//! second costs no wall time.

use std::sync::Arc;
use std::time::Duration;

use lgwks_bot::cap::{Auth, Cap};
use lgwks_bot::cas::{CasInput, CasWrite, RecordStore};
use lgwks_bot::clock::Clock;
use lgwks_bot::error::{BotError, DispatchCertainty, RetryClass};
use lgwks_bot::gate::GrantSet;
use lgwks_bot::verb::Execute;

use crate::scratch::Scratch;
use crate::sim::{Rng, Trace};
use crate::sim_cas_disk::{Model, SEEDS, TestResult, agrees, store_path, sweep};
use crate::sweep_fixtures::refuse;

/// A proof covering the store's one capability, with no lifetime.
fn fs_auth() -> Result<Auth, BotError> {
    GrantSet::empty().grant(Cap::fs()).issue(&[Cap::fs()])
}

/// One seeded write through the adapter, checked against the model.
fn execute_step(
    rng: &mut Rng,
    adapter: &CasWrite,
    auth: &Auth,
    model: &mut Model,
    records: u32,
    trace: &mut Trace,
) -> TestResult {
    let name = format!("record-{}", rng.below(records));
    let current = model.version(&name);
    let fresh = rng.chance(700);
    let expected = if fresh {
        current
    } else {
        current.saturating_add(1)
    };
    let value = format!("v{}", rng.below(1_000_000));
    let input = CasInput::new(name.clone(), expected, value.clone());
    match lgwks_std::task::block_on(adapter.execute_action((auth.clone(), &input))) {
        Ok(output) if fresh => {
            if output.version() != current.saturating_add(1) || output.record() != name {
                return refuse(format!(
                    "the adapter reported {} at {} for a write to {name} naming {current}",
                    output.record(),
                    output.version()
                ));
            }
            model.applied(name, output.version(), value);
            trace.record_number("applied", output.version());
        }
        Ok(output) => {
            return refuse(format!(
                "a stale write to {name} installed {} through the adapter",
                output.version()
            ));
        }
        Err(error) if !fresh => {
            let BotError::Conflict {
                ref domain,
                ref record,
                expected: named,
                found,
                ..
            } = error
            else {
                return refuse(format!(
                    "a stale write failed as {error}, not as a conflict"
                ));
            };
            if domain != "cas::store" || *record != name || named != expected || found != current {
                return refuse(format!("the conflict named the wrong facts: {error}"));
            }
            if error.retry_class() != RetryClass::Never {
                return refuse("a conflict must never be retried blindly");
            }
            model.conflicted();
            trace.record_number("conflict", found);
        }
        Err(error) => return refuse(format!("a fresh write to {name} failed: {error}")),
    }
    Ok(())
}

/// Seeded writes through the adapter agree with the model, live and after a
/// reopen.
#[test]
fn execute_writes_match_the_model_across_reopens() -> TestResult {
    sweep("execute-model", SEEDS, |seed, trace| {
        let mut rng = Rng::new(seed);
        let dir = Scratch::new("sim-cas-exec")?;
        let path = store_path(dir.path());
        let auth = fs_auth()?;
        let records = rng.between(1, 5);
        let mut model = Model::default();
        let mut adapter = CasWrite::new(Arc::new(RecordStore::open(&path)?));
        for _ in 0..rng.between(4, 24) {
            if rng.chance(150) {
                drop(adapter);
                adapter = CasWrite::new(Arc::new(RecordStore::open(&path)?));
                trace.record("reopened");
            }
            execute_step(&mut rng, &adapter, &auth, &mut model, records, trace)?;
        }
        agrees(&adapter.store().state(), &model)?;
        drop(adapter);
        agrees(&RecordStore::open(&path)?.state(), &model)
    })
}

/// A persist the disk refuses reaches the caller as a refused domain error,
/// and the store holds what it held before.
#[test]
fn a_refused_persist_through_the_adapter_applies_nothing() -> TestResult {
    sweep("execute-persist", SEEDS, |seed, trace| {
        let mut rng = Rng::new(seed);
        let dir = Scratch::new("sim-cas-exec-io")?;
        let sub = dir.path().join("store");
        std::fs::create_dir_all(&sub)?;
        let auth = fs_auth()?;
        let records = rng.between(1, 4);
        let mut model = Model::default();
        let adapter = CasWrite::new(Arc::new(RecordStore::open(store_path(&sub))?));
        for _ in 0..rng.between(0, 10) {
            execute_step(&mut rng, &adapter, &auth, &mut model, records, trace)?;
        }
        std::fs::remove_dir_all(&sub)?;
        let name = format!("record-{}", rng.below(records));
        let expected = if rng.chance(500) {
            model.version(&name)
        } else {
            model.version(&name).saturating_add(1)
        };
        let input = CasInput::new(name, expected, "never persisted");
        match lgwks_std::task::block_on(adapter.execute_action((auth, &input))) {
            Err(BotError::DomainError {
                ref domain,
                certainty: DispatchCertainty::Refused,
                ..
            }) if domain == "cas::store" => trace.record("refused"),
            other => return refuse(format!("a refused persist reached the caller as {other:?}")),
        }
        agrees(&adapter.store().state(), &model)
    })
}

/// A proof that does not cover the store's capability is denied before the
/// store is touched: the file's bytes and the state are unchanged.
#[test]
fn a_proof_without_the_fs_capability_writes_nothing() -> TestResult {
    sweep("execute-denied", SEEDS, |seed, trace| {
        let mut rng = Rng::new(seed);
        let dir = Scratch::new("sim-cas-exec-cap")?;
        let path = store_path(dir.path());
        let records = rng.between(1, 4);
        let mut model = Model::default();
        let adapter = CasWrite::new(Arc::new(RecordStore::open(&path)?));
        let granted = fs_auth()?;
        for _ in 0..rng.between(1, 8) {
            execute_step(&mut rng, &adapter, &granted, &mut model, records, trace)?;
        }
        let before = std::fs::read(&path).ok();
        let bare = GrantSet::empty().issue(&[])?;
        for _ in 0..rng.between(1, 6) {
            let name = format!("record-{}", rng.below(records));
            let input = CasInput::new(name.clone(), model.version(&name), "denied");
            match lgwks_std::task::block_on(adapter.execute_action((bare.clone(), &input))) {
                Err(BotError::CapabilityDenied { .. }) => trace.record("denied"),
                other => return refuse(format!("an uncovered proof reached {other:?}")),
            }
        }
        if std::fs::read(&path).ok() != before {
            return refuse("a denied write changed the store file");
        }
        agrees(&adapter.store().state(), &model)
    })
}

/// A credential with a lifetime writes while the virtual clock is short of
/// its expiry and is refused, writing nothing, from the expiry on.
#[test]
fn a_credential_writes_until_its_lifetime_runs_out() -> TestResult {
    sweep("execute-lifetime", SEEDS, |seed, trace| {
        let mut rng = Rng::new(seed);
        let dir = Scratch::new("sim-cas-exec-ttl")?;
        let path = store_path(dir.path());
        let clock = Clock::virtual_at(Duration::ZERO);
        let ttl = Duration::from_millis(u64::from(rng.between(4, 1_000)));
        let auth = GrantSet::empty()
            .grant_expiring(Cap::fs(), ttl)
            .issue_at(&[Cap::fs()], &clock)?;
        if auth.expires_at() != Some(ttl) {
            return refuse(format!(
                "a proof minted at zero with lifetime {ttl:?} expires at {:?}",
                auth.expires_at()
            ));
        }
        let adapter = CasWrite::new(Arc::new(RecordStore::open(&path)?));
        let step = ttl.checked_div(4).ok_or("a quarter of the lifetime")?;
        let mut model = Model::default();
        let mut written = 0_u32;
        for _ in 0..rng.between(4, 16) {
            let jump = Duration::from_millis(u64::from(
                rng.below(u32::try_from(step.as_millis())?.saturating_add(1)),
            ));
            let now = clock.advance(jump)?;
            let name = format!("record-{written}");
            let input = CasInput::new(name.clone(), model.version(&name), "timed");
            let outcome = lgwks_std::task::block_on(adapter.execute_action((auth.clone(), &input)));
            match (now < ttl, outcome) {
                (true, Ok(output)) => {
                    model.applied(name, output.version(), "timed".to_owned());
                    written = written.saturating_add(1);
                    trace.record_number("written at", now.as_millis());
                }
                (
                    false,
                    Err(BotError::CredentialExpired {
                        expired_at,
                        now: seen,
                        ..
                    }),
                ) => {
                    if expired_at != ttl || seen != now {
                        return refuse(format!(
                            "the expiry named {expired_at:?} at {seen:?}, the clock says {ttl:?} at {now:?}"
                        ));
                    }
                    trace.record_number("expired at", now.as_millis());
                }
                (live, other) => {
                    return refuse(format!(
                        "at {now:?} of {ttl:?} (live: {live}) the adapter returned {other:?}"
                    ));
                }
            }
        }
        agrees(&adapter.store().state(), &model)?;
        drop(adapter);
        agrees(&RecordStore::open(&path)?.state(), &model)
    })
}

/// Threads bumping records through one adapter lose no update.
#[test]
fn threads_through_one_adapter_lose_no_update() -> TestResult {
    sweep("execute-threads", 64, |seed, trace| {
        let mut rng = Rng::new(seed);
        let dir = Scratch::new("sim-cas-exec-threads")?;
        let path = store_path(dir.path());
        let adapter = CasWrite::new(Arc::new(RecordStore::open(&path)?));
        let auth = fs_auth()?;
        let records = rng.between(1, 3);
        let mut plan = Vec::new();
        for _ in 0..rng.between(2, 8) {
            plan.push((rng.below(records), rng.between(1, 5)));
        }
        let outcomes: Vec<Result<(), String>> = std::thread::scope(|scope| {
            // Every writer is spawned before any is joined, so they contend.
            let mut handles = Vec::with_capacity(plan.len());
            for &(record, bumps) in &plan {
                let (adapter, auth) = (&adapter, &auth);
                handles.push(scope.spawn(move || bump(adapter, auth, record, bumps)));
            }
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
        let state = adapter.store().state();
        for record in 0..records {
            let aimed: u64 = plan
                .iter()
                .filter(|&&(target, _)| target == record)
                .map(|&(_, bumps)| u64::from(bumps))
                .sum();
            let held = match state.records().get(&format!("record-{record}")) {
                Some(entry) => entry.version(),
                None => 0,
            };
            if held != aimed {
                return refuse(format!(
                    "record-{record} ended at {held} after {aimed} bumps"
                ));
            }
            trace.record_number("record", held);
        }
        Ok(())
    })
}

/// Bump `record-{record}` `bumps` times through the adapter, re-reading after
/// every conflict.
fn bump(adapter: &CasWrite, auth: &Auth, record: u32, bumps: u32) -> Result<(), String> {
    let name = format!("record-{record}");
    let mut left = bumps;
    let mut attempts = 0_u32;
    while left > 0 {
        attempts = attempts.saturating_add(1);
        if attempts > 100_000 {
            return give_up(format!("{name}: no progress after 100,000 attempts"));
        }
        let seen = match adapter.store().state().records().get(&name) {
            Some(entry) => entry.version(),
            None => 0,
        };
        let input = CasInput::new(name.clone(), seen, "bumped");
        match lgwks_std::task::block_on(adapter.execute_action((auth.clone(), &input))) {
            Ok(_) => left = left.saturating_sub(1),
            Err(BotError::Conflict { .. }) => {}
            Err(error) => return give_up(format!("{name}: {error}")),
        }
    }
    Ok(())
}

/// A writer thread's refusal, emitted before it is returned.
fn give_up(cause: String) -> Result<(), String> {
    let refusal = Err(cause);
    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "bump: a writer thread gave up");
    refusal
}
