//! Row 5 of issue #278 against the real public surface: concurrent editors
//! of one record.
//!
//! The simulation (`sim_cas`) sweeps the schedules; this file races real
//! threads against the shipped [`RecordStore`](lgwks_bot::cas::RecordStore)
//! on a real file, drives the [`CasWrite`](lgwks_bot::cas::CasWrite)
//! [`Execute`](lgwks_bot::verb::Execute) adapter through its public verbs,
//! and reopens the store afterwards.
//!
//! The families:
//!
//! - N threads racing one record produce exactly one winner per round and
//!   N-1 typed conflicts, with versions contiguous from one — no lost update;
//! - every conflict is [`BotError::Conflict`](lgwks_bot::error::BotError)
//!   naming `expected` and `found`, and its
//!   [`RetryClass`](lgwks_bot::error::RetryClass) is `Never`: never retried
//!   blindly;
//! - the conflicts are in the reopened file, so what changed is answerable
//!   after a restart.

use std::error::Error;
use std::sync::Arc;

use lgwks_bot::cap::Cap;
use lgwks_bot::cas::{CasInput, CasWrite, NO_VERSION, RecordStore};
use lgwks_bot::error::{BotError, RetryClass};
use lgwks_bot::gate::GrantSet;
use lgwks_bot::verb::Execute;

use crate::scratch::Scratch;
use crate::sweep_fixtures::refuse;

type TestResult = Result<(), Box<dyn Error>>;

/// Writers racing one record in the thread burst.
const WRITERS: usize = 16;

/// An `Auth` covering the store's capability.
fn fs_auth() -> Result<lgwks_bot::cap::Auth, BotError> {
    GrantSet::empty().grant(Cap::fs()).issue(&[Cap::fs()])
}

/// Sixteen threads racing one record: one winner, fifteen typed conflicts,
/// versions contiguous from one.
#[test]
fn racing_writers_produce_one_winner_and_typed_conflicts() -> TestResult {
    // The directory dies with the test: every assertion has already been
    // made, and no manual removal stands between the race and its evidence.
    let dir = Scratch::new("cas-race")?;
    let path = dir.path().join("store.json");
    let store = Arc::new(RecordStore::open(&path)?);
    // One joined handle per writer: the outcomes are collected by joining in
    // order, so no shared collection and no lock stand between the race and
    // its evidence.
    let handles: Vec<std::thread::JoinHandle<Result<(), String>>> = (0..WRITERS)
        .map(|writer| {
            let store = Arc::clone(&store);
            std::thread::Builder::new()
                .name(format!("cas-racer-{writer}"))
                .spawn(move || {
                    // Every writer read version zero before the race: they all
                    // believe they are first, so all but one must lose.
                    store
                        .write("shared", NO_VERSION, &format!("writer-{writer}"))
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                })
                .map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, String>>()
        .map_err(|error| -> Box<dyn Error> { error.into() })?;
    let mut outcomes = Vec::with_capacity(handles.len());
    for handle in handles {
        outcomes.push(handle.join().map_err(|_| "a racing thread panicked")?);
    }
    let wins = outcomes.iter().filter(|outcome| outcome.is_ok()).count();
    let losses = outcomes.iter().filter(|outcome| outcome.is_err()).count();
    assert_eq!(wins, 1, "exactly one writer wins the first round");
    assert_eq!(
        losses,
        WRITERS - 1,
        "every other writer loses as a typed conflict"
    );
    let state = store.state();
    assert_eq!(
        state.records()["shared"].version(),
        1,
        "the winner installs version one"
    );
    assert_eq!(
        state.conflicts().len(),
        WRITERS - 1,
        "every loss is recorded in the conflict log"
    );
    for entry in state.conflicts().iter() {
        assert_eq!(entry.expected(), 0, "each loser saw version zero");
        assert_eq!(entry.found(), 1, "each loser found the winner's version");
    }
    Ok(())
}

/// The verb path reports the same typed conflict, refuses to retry it, and
/// the reopened file still holds it.
#[test]
fn the_execute_path_reports_a_typed_conflict_that_survives_reopen() -> TestResult {
    let dir = Scratch::new("cas-verb")?;
    let path = dir.path().join("store.json");
    let store = Arc::new(RecordStore::open(&path)?);
    let adapter = CasWrite::new(Arc::clone(&store));
    let auth = fs_auth()?;
    let won = lgwks_std::task::block_on(
        adapter.execute_action((auth.clone(), &CasInput::new("shared", NO_VERSION, "winner"))),
    )?;
    assert_eq!(won.version(), 1, "the first write installs version one");
    let Err(BotError::Conflict {
        expected,
        found,
        record,
        ..
    }) = lgwks_std::task::block_on(
        adapter.execute_action((auth, &CasInput::new("shared", NO_VERSION, "loser"))),
    )
    else {
        return refuse("a write against a moved record must conflict");
    };
    assert_eq!(record, "shared", "the conflict names its record");
    assert_eq!(expected, 0, "the conflict names the version the write saw");
    assert_eq!(found, 1, "the conflict names the version the store holds");
    let conflict = BotError::Conflict {
        domain: "cas::store".to_owned(),
        record: "shared".to_owned(),
        expected,
        found,
    };
    assert_eq!(
        conflict.retry_class(),
        RetryClass::Never,
        "a conflict is never retried blindly"
    );
    drop(adapter);
    drop(store);
    let reopened = RecordStore::open(&path)?;
    let state = reopened.state();
    assert_eq!(
        state.records()["shared"].value(),
        "winner",
        "the winner survives the restart"
    );
    assert_eq!(
        state.conflicts().len(),
        1,
        "the conflict survives the restart"
    );
    assert_eq!(
        state.conflicts()[0].found(),
        1,
        "the reopened conflict still names what it found"
    );
    Ok(())
}
