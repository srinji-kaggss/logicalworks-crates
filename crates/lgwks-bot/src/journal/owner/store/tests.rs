//! The failed covering flush, driven on the shipped store.
//!
//! `tests/sim_group_commit.rs` drives the same group commit through the front
//! door, but the flush-failure switch is a `#[cfg(test)]` seam and no seeded
//! simulation can reach it, so every batch a sim stages flushes. This is the
//! test that injects the failure the sim family cannot: it queues `MEMBERS`
//! requests into one batch, refuses that batch's one `sync_all`, and checks
//! what the contract promises a failed group commit does.
//!
//! Beside it, the awaited answer's lost-wakeup probe (INV-BOT-140), which needs
//! no task feature: it drives the owner directly.

use super::{Stage, StorageOwner};
use crate::journal::owner::{lock, wait_timeout_while};
use lgwks_std::task::spawn_blocking;
use std::fs::File;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[cfg(feature = "script")]
use std::error::Error;

#[cfg(feature = "script")]
use crate::effect::RunId;
#[cfg(feature = "script")]
use crate::script::run_store::RunRecords;
#[cfg(feature = "script")]
use crate::script::{Scope, Tenant};
#[cfg(feature = "script")]
use crate::task::RunStore;

/// The most members the one batch this test stages holds.
///
/// Three is the smallest count that is more than one *and* more than two, so a
/// failure that answered only the first request, or paired requests, is caught
/// rather than mistaken for the batch-wide answer.
#[cfg(feature = "script")]
const MEMBERS: usize = 3;

/// One distinct run id per member, in fixed hex.
///
/// Fixed rather than minted so the scenario needs no entropy, and distinct so
/// each record is its own to fold — a fold any member missed would be visible
/// as a missing run in the store's index.
#[cfg(feature = "script")]
const RUN_HEX: [&str; MEMBERS] = [
    "01000000000000000000000000000000",
    "02000000000000000000000000000000",
    "03000000000000000000000000000000",
];

/// One distinct payload per member, so no two records are duplicates of each
/// other and the duplicate path cannot fold one as another's answer.
#[cfg(feature = "script")]
const PAYLOAD: [u8; MEMBERS] = [1, 2, 3];

/// A failed batch's flush answers every member, folds none, and leaves the file
/// as the only authority.
///
/// # Errors
///
/// Whatever the store, the scope or the filesystem reports.
#[cfg(feature = "script")]
#[test]
fn a_failed_batch_flush_acknowledges_nobody_and_folds_nothing() -> Result<(), Box<dyn Error>> {
    let dir = crate::journal::file::tests::scratch("owner-flush")?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("store");

    // A device parked before it takes any request, so every submission below is
    // queued when it is released and the owner drains all of them into one
    // batch — rather than a race for how many happen to be queued together.
    let store = RunStore::open_with_stalled_device(&path)?;
    let tenant = "flush-tenant";
    let step_scope = Scope::root(Tenant::new(tenant)?).enter("step")?;
    let key = step_scope.key();
    let step_definition = crate::script::run_store::definition_of(&step_scope, "step");
    let late_definition = crate::script::run_store::definition_of(&step_scope, "late");

    // One distinct record per member, built through the store's own staging
    // door so the record is exactly what `remember` would commit.
    let mut runs = Vec::with_capacity(MEMBERS);
    let mut outstanding = Vec::with_capacity(MEMBERS);
    for index in 0..MEMBERS {
        let run = RunId::from_hex(RUN_HEX[index])?;
        runs.push(run);
        let record = RunRecords::stage(
            &store,
            tenant,
            run,
            key,
            "step",
            &step_definition,
            vec![PAYLOAD[index]],
        );
        outstanding.push(RunRecords::append_async(&store, record));
    }

    // Poll each future once so its request reaches the owner's ring, and no
    // further: a device that is parked cannot answer, so a request completing
    // here would mean a release this test did not make. Polling every future
    // before the release is what queues them into *one* batch rather than a race
    // for how many happen to be queued together.
    {
        let waker = std::task::Waker::noop();
        let mut context = std::task::Context::from_waker(waker);
        for future in &mut outstanding {
            if future.as_mut().poll(&mut context).is_ready() {
                return Err("a parked device answered a request before it was released".into());
            }
        }
    }

    // Refuse the batch's one covering flush, then release the parked device.
    store.fail_next_flush();
    store.release_device();

    // (1) Every member receives the failure: none of the batch is acknowledged.
    for (index, pending) in outstanding.into_iter().enumerate() {
        let answered = crate::rt::runtime::block_on(pending);
        assert!(
            answered.is_err(),
            "member {index} of a batch whose flush failed was acknowledged: {answered:?}"
        );
    }

    // (2) No member's fold ran: the handle's index names none of them, so the
    // reader cannot see a record whose bytes the device never took.
    for (index, run) in runs.iter().enumerate() {
        assert!(
            !store.knows_run(*run),
            "member {index}'s run is in the handle's index after a failed flush"
        );
        assert_eq!(
            store.record_count(*run),
            0,
            "member {index}'s record was folded into the handle's index after a failed flush"
        );
    }

    // (3) One poison is latched for the whole handle: every later submission is
    // refused with the same message rather than written, and the index still
    // holds nothing.
    for run in runs.iter().take(2) {
        let refusal =
            RunRecords::append(&store, tenant, *run, key, "late", &late_definition, vec![9])
                .err()
                .map(|error| error.to_string());
        assert!(
            refusal
                .as_deref()
                .is_some_and(|message| message.contains("previous append")),
            "a later append must be refused as poisoned, not accepted: {refusal:?}"
        );
    }

    // (4) A reopen reads the file, not the handle. The batch's bytes were
    // written before the flush failed, so the complete frames are exactly what a
    // fresh handle replays and consumes — none invented, none dropped. The
    // handle's own index, which folded nothing, is not consulted.
    let staged = store.committed_bytes();
    drop(store);
    let reopened = RunStore::open(&path)?;
    assert_eq!(
        reopened.committed_bytes(),
        std::fs::metadata(&path)?.len(),
        "a reopen must consume the whole file, so it replays exactly its durable frames"
    );
    assert_eq!(
        reopened.committed_bytes(),
        staged,
        "the failed batch's complete frames must be the file's own bytes"
    );
    for (index, run) in runs.iter().enumerate() {
        assert_eq!(
            reopened.record_count(*run),
            1,
            "member {index}'s complete frame is on the file and a reopen reads it, \
             though the handle acknowledged none of the batch"
        );
    }
    drop(reopened);
    drop(std::fs::remove_dir_all(&dir));
    Ok(())
}

/// How many awaited round trips the race gets to land in.
const ROUND_TRIPS: u64 = 200_000;

/// How long one round trip may make no progress before it is called a hang.
///
/// A bound on progress, not on the sweep: a lost wakeup stops the counter for
/// ever, while a slow host only slows it. The sweep takes a few seconds on an
/// idle machine and took more than 60 s beside ten CI jobs on the same cores
/// (run 37623747480: 73,968 round trips done, every one answered), and a
/// bound on the whole sweep reported that as a parked poll.
const PATIENCE: Duration = Duration::from_secs(60);

/// An answer the owner publishes while the awaiting poll is between "the slot
/// is empty" and "my waker is registered" still wakes that poll.
///
/// Each round trip is one `submit_async` driven by a bare `block_on`, so the
/// owner thread publishes concurrently with the poll that checks for its
/// answer. If the poll checked the slot before registering its waker, a
/// publish landing between the two would find no waker to fire and leave an
/// answer nobody reads: `block_on` parks for ever. That is the hang CI showed
/// as `sim_repair saturation_applies_each_ticket_once` parked past 600 s, and
/// the watchdog here turns it into a failure naming the round trip that
/// parked, rather than a test that never returns.
#[test]
fn an_answer_published_during_registration_still_wakes_the_poll()
-> Result<(), Box<dyn std::error::Error>> {
    let path = crate::journal::file::tests::scratch("owner-wake")?;
    let owner = StorageOwner::<(), u64>::spawn(File::create(&path)?, (), false)?;
    let progress = Arc::new((Mutex::new((0_u64, false)), Condvar::new()));
    let reported = Arc::clone(&progress);
    let driver = spawn_blocking(move || -> Result<(), String> {
        let (ref state, ref changed) = *reported;
        let mut outcome = Ok(());
        for trip in 0..ROUND_TRIPS {
            match lgwks_std::task::block_on(
                owner.submit_async(move |_, _| Ok(Stage::Settled(Ok(trip)))),
            ) {
                Ok(answer) if answer == trip => {}
                other => {
                    outcome = Err(format!("round trip {trip} answered {other:?}"));
                    break;
                }
            }
            lock(state).0 = trip + 1;
            changed.notify_all();
        }
        // Finished or failed, the watchdog is told, so a refusal is reported
        // as itself rather than as a hang.
        lock(state).1 = true;
        changed.notify_all();
        outcome
    });

    let (ref state, ref changed) = *progress;
    let mut seen = 0_u64;
    loop {
        let (held, waited) = wait_timeout_while(changed, lock(state), PATIENCE, |progress| {
            !progress.1 && progress.0 == seen
        });
        let (completed, done) = *held;
        drop(held);
        if done {
            break;
        }
        if waited.timed_out() && completed == seen {
            return Err(format!(
                "round trip {completed} of {ROUND_TRIPS} parked for {PATIENCE:?}: the owner \
                 published an answer whose poll had not yet registered a waker, so nothing \
                 woke it"
            )
            .into());
        }
        seen = completed;
    }
    lgwks_std::task::block_on(driver)?;
    std::fs::remove_file(&path)?;
    Ok(())
}
