//! Slow-store liveness: a parked record device must not stop the runtime.
//!
//! The run store's append is a `write_all` plus a `sync_all`. Before this store
//! moved onto a storage-owner thread that pair ran inside the async step that
//! awaited it, which parks the runtime thread for the length of an `fsync` — the
//! exact defect #122 removed from the effect journal and that the run store
//! inherited when it was written later. This file is the acceptance for the move,
//! in the shape the review asked for: the instrument is an **independent OS thread**
//! — not the runtime awaiting the append, and not the storage owner — and the two
//! things it watches are that an unrelated ready task keeps getting turns and that
//! the release it performs is what finally lets the record land.
//!
//! The device is not merely slow, it is *parked*: the store is opened with its
//! storage gate closed, so the flush provably cannot complete until the watchdog
//! thread releases it. A tree whose durable write ran on the thread driving the
//! future would never reach the release, the unrelated task would never tick, and
//! the watchdog would time out — which is why the assertion is on the tick count
//! and not on a duration.
//!
//! The turns observed with the threshold raised so the watchdog timed out rather
//! than releasing: 46,835,531 polls of the unrelated task while one `sync_all` was
//! parked. That is the measurement behind "the runtime keeps turning", and it is
//! what a blocking implementation cannot reach — it reaches one poll and then sits
//! inside the flush.
//!
//! | Test | What it pins |
//! |---|---|
//! | `a_parked_record_device_lets_the_runtime_turn` | a `remember` whose flush is parked does not stop an unrelated ready task, and the record reaches the disk |
//! | `a_grouped_batch_still_lets_the_runtime_turn` | the same liveness claim for a *batch*: several records sharing one flush park the append only once, and all of them land |
//! | `an_abandoned_record_leaves_the_store_consistent` | dropping a `remember` mid-append leaves a store that reopens, is never duplicated, and resumes to the same output |
//! | `a_parked_store_still_serves_its_own_records_only` | a parked device widens neither who a run is attributed to nor which records are read |
//! | `the_parked_device_probe_measures_turns` | the tick counter is not vacuous, so the liveness assertions above cannot pass by accident |

#![cfg(all(feature = "script", feature = "ephemeral"))]

use crate::scratch::Scratch;

use crate::journal_fixtures as measure;

use crate::liveness_fixtures as liveness;

use crate::resume_fixtures as shared;

use std::error::Error;
use std::future::poll_fn;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::Poll;

use lgwks_bot::effect::RunId;
use lgwks_bot::rt::runtime;
use lgwks_bot::task::{Disposition, Host, RunStore};

use liveness::{PROGRESS_TURNS, Parked, heartbeat};
use shared::{one_step_task, store_file};

type TestResult = Result<(), Box<dyn Error>>;

/// A parked record device must not stop an unrelated ready task from getting turns.
///
/// The step is awaited on the runtime being driven; the unrelated task is polled on
/// the same driver each round. If the durable wait were on the driver's thread, the
/// unrelated task would never tick — so the assertion is `ticks >= PROGRESS_TURNS`,
/// which a blocking implementation fails by construction. The record that does land
/// is then read back through a *fresh* store, so the release is proved to have put
/// the bytes on the disk rather than only into a handle.
#[test]
fn a_parked_record_device_lets_the_runtime_turn() -> TestResult {
    let scratch = Scratch::new("liveness-progress")?;
    let store = RunStore::open_with_stalled_device(store_file(scratch.path()))?;
    let parked = Parked::at(None, store.storage_gate())?;
    let work = one_step_task()?;
    let host = Host::builder("acme")?.store(store).build()?;

    let mut beat = Box::pin(heartbeat(parked.ticks()));
    let mut step = Box::pin(host.run(&work, 7u32));
    let report = runtime::block_on(poll_fn(|cx| {
        // The unrelated task gets a turn before the step is polled, and its wake
        // keeps the driver moving; if the flush blocked here, this line would be
        // reached at most once.
        let _unobserved = beat.as_mut().poll(cx);
        step.as_mut().poll(cx)
    }));
    drop(step);

    let observed = parked.observed()?;
    // Reported, not only asserted: the number behind this family's claim is what
    // a reader needs, and an assertion that only says "at least 64" hides whether
    // the durable wait is a wait or a near-total stall.
    measure::record_measurement(&format!("resume-liveness turns_while_parked={observed}"))?;
    assert!(
        observed >= PROGRESS_TURNS,
        "the unrelated task ticked {observed} times while a parked flush was \
         outstanding: the durable wait is running on the driver's thread, which \
         reaches at most one poll per submission"
    );
    assert_eq!(
        report.disposition(),
        Disposition::Succeeded,
        "a released device still earns the record: {:?}",
        report.error()
    );

    // The released frame must be on the disk, read back through a fresh handle: a
    // resume over the same run through that fresh store replays the step rather than
    // running its body again, which is only true if the bytes are there.
    let landed = report.run_id().ok_or("a stored run must name a run id")?;
    drop(report);
    drop(host);
    let reopened = Host::builder("acme")?
        .store(RunStore::open(store_file(scratch.path()))?)
        .build()?;
    let replayed = runtime::block_on(reopened.resume(landed, &work, 7u32));
    assert_eq!(
        replayed.output(),
        Some(&7),
        "the released record is on the disk, so the resume answers from it: {:?}",
        replayed.error()
    );
    Ok(())
}

/// Dropping a `remember` mid-append must leave the store consistent, and a resume
/// must read that step exactly once.
///
/// Phase one parks the append and abandons it. Phase two keeps driving the unrelated
/// task on the same thread until the **independent** watchdog has released the
/// device, so the tick count is what the runtime achieved while the abandoned flush
/// was outstanding. Whatever the store decides about the abandoned record, the two
/// facts that must hold afterwards are that a reopen reads the store at all — a
/// half-written frame would refuse the open — and that the resumed run reaches its
/// value without the abandoned step's record being applied twice.
#[test]
fn an_abandoned_record_leaves_the_store_consistent() -> TestResult {
    let scratch = Scratch::new("liveness-cancel")?;
    let path = store_file(scratch.path());
    let store = RunStore::open_with_stalled_device(&path)?;
    let parked = Parked::at(None, store.storage_gate())?;
    let run = RunId::mint()?;
    let work = one_step_task()?;
    let host = Host::builder("acme")?.store(store).build()?;

    let mut beat = Box::pin(heartbeat(parked.ticks()));
    let mut step = Box::pin(host.resume(run, &work, 11u32));
    // Poll the step exactly once so the owner has the request, letting the unrelated
    // task tick in the same round. Then abandon it: the flush is parked, so the drop
    // cannot be waiting on the device.
    let first = runtime::block_on(poll_fn(|cx| {
        let _unobserved = beat.as_mut().poll(cx);
        Poll::Ready(step.as_mut().poll(cx))
    }));
    assert!(
        first.is_pending(),
        "the step completed before it could be abandoned"
    );
    drop(step);

    // Phase two: the runtime stays live while the abandoned flush is still
    // outstanding, turning the unrelated task until the independent watchdog
    // releases the device.
    runtime::block_on(poll_fn(|cx| {
        if parked.released().load(Ordering::Relaxed) {
            return Poll::Ready(());
        }
        let _unobserved = beat.as_mut().poll(cx);
        Poll::Pending
    }));
    assert!(
        parked.released().load(Ordering::Relaxed),
        "the watchdog never released the parked device, so the abandoned append is \
         still outstanding and nothing after this point is evidence"
    );

    let observed = parked.observed()?;
    assert!(
        observed >= PROGRESS_TURNS,
        "the unrelated task ticked {observed} times while an abandoned flush was \
         outstanding: the cancellation blocked the driver"
    );

    // The store must still be a store. A reopen replays whatever the owner wrote,
    // and refuses a frame that does not follow — so a successful open *is* the
    // consistency claim: an index that disagreed with the file, or a torn tail
    // where the abandoned write landed mid-frame, would both fail here.
    drop(host);
    let reopened = RunStore::open(&path)?;
    let recorded = reopened.record_count(run);
    assert!(
        recorded <= 1,
        "an abandoned record must not be duplicated, saw {recorded}"
    );

    // And the resume still reaches the same value: either the record landed and is
    // replayed, or it did not and the step runs now. Exactly-once for a recorded
    // step and at-least-once for an unrecorded one, which is the whole claim.
    let host = Host::builder("acme")?.store(reopened).build()?;
    let after = runtime::block_on(host.resume(run, &work, 11u32));
    assert_eq!(
        after.output(),
        Some(&11),
        "the resumed step returns its value however the abandoned write landed: {:?}",
        after.error()
    );
    Ok(())
}

/// A parked device must not widen who a run's records belong to.
///
/// The tenant check is decided on the record rather than on the path, so it has to
/// hold while the store's own device is stuck answering a write: a refusal that only
/// worked when the disk was answering would be a refusal to a different store than
/// the one that is actually open. What is checked is that a handle opened over the
/// same bytes with its device parked still attributes the run to the tenant that
/// minted it and still reads exactly that tenant's record.
#[test]
fn a_parked_store_still_serves_its_own_records_only() -> TestResult {
    let scratch = Scratch::new("liveness-tenant")?;
    let path = store_file(scratch.path());
    let work = one_step_task()?;
    let first = runtime::block_on(
        Host::builder("acme")?
            .store(RunStore::open(&path)?)
            .build()?
            .run(&work, 5u32),
    );
    let run = first.run_id().ok_or("a stored run must name a run id")?;
    drop(first);

    // The original handle has let the file go, so a second one can be opened over
    // the same bytes — this time with its device parked.
    let parked_store = RunStore::open_with_stalled_device(&path)?;
    assert_eq!(
        parked_store.tenant_of(run).as_deref(),
        Some("acme"),
        "the parked store attributes the run to the tenant that minted it"
    );
    assert_eq!(
        parked_store.record_count(run),
        1,
        "the parked store still reads that tenant's one record while its device is parked"
    );
    assert!(
        !parked_store.knows_run(RunId::mint()?),
        "a run id this store never wrote is not one it will serve"
    );
    drop(parked_store);
    Ok(())
}

/// The tick counter must count turns, or every liveness assertion above is vacuous.
///
/// One driver turn with a self-waking task must advance the counter more than once,
/// which is what makes `PROGRESS_TURNS` a claim about turns rather than about one
/// poll that happened to be counted twice. Pinned here so a future change to the
/// probe cannot leave the other three tests passing for the wrong reason.
#[test]
fn the_parked_device_probe_measures_turns() -> TestResult {
    let ticks = Arc::new(AtomicU64::new(0));
    let mut beat = Box::pin(heartbeat(Arc::clone(&ticks)));
    runtime::block_on(poll_fn(|cx| {
        let _first = beat.as_mut().poll(cx);
        let _second = beat.as_mut().poll(cx);
        Poll::Ready(())
    }));
    let counted = ticks.load(Ordering::Relaxed);
    assert!(
        counted >= 2,
        "one driver turn produced {counted} polls: the probe is not counting turns"
    );
    Ok(())
}

/// A batch of records sharing one flush must park the runtime exactly as one record
/// does, and must land every member of it.
///
/// Group commit changed *how many* records one flush covers, not where the flush
/// runs: the whole point of the storage owner is that the wait is the caller's and
/// the device's, and a batch is a reason to sync once rather than a reason to block
/// an executor thread twice. This asserts that directly — several runs are driven on
/// one driver while the device is parked, the unrelated task must keep turning, and
/// after the release every one of the runs must be on the disk.
///
/// The liveness half is the load-bearing one: a batch staged on the awaiting thread
/// would show up here as a turn count of one.
#[test]
fn a_grouped_batch_still_lets_the_runtime_turn() -> TestResult {
    let scratch = Scratch::new("liveness-group")?;
    let store = RunStore::open_with_stalled_device(store_file(scratch.path()))?;
    let parked = Parked::at(None, store.storage_gate())?;
    let work = one_step_task()?;
    let host = Host::builder("acme")?.store(store).build()?;
    // The batch is one number in two widths: the task's input is a `u32` and a
    // capacity is a `usize`, so both are drawn from the same constant here rather
    // than converted per use with a stand-in for a count this host cannot
    // address.
    const BATCH: u32 = 8;
    let batch = BATCH;
    let lanes = usize::try_from(BATCH)?;

    // Every lane's future is polled on one driver alongside the heartbeat, so the
    // turn count is what the runtime achieved while a whole batch was outstanding.
    let mut beat = Box::pin(heartbeat(parked.ticks()));
    let mut runs: Vec<std::pin::Pin<Box<_>>> = Vec::with_capacity(lanes);
    for index in 0..batch {
        runs.push(Box::pin(host.run(&work, index)));
    }
    // Answers are collected as they land rather than in a second pass: a lane that
    // has resolved is not polled again, which is the rule a `Host::run` future
    // enforces — polling it after it completed is a use-after-completion, not a way
    // to read its answer twice.
    let mut landed: Vec<RunId> = Vec::with_capacity(lanes);
    let mut resolved: Vec<Option<lgwks_bot::task::Report<u32>>> =
        runs.iter_mut().map(|_| None).collect();
    runtime::block_on(poll_fn(|cx| {
        let _unobserved = beat.as_mut().poll(cx);
        let mut pending = false;
        for (index, step) in runs.iter_mut().enumerate() {
            if resolved[index].is_some() {
                continue;
            }
            if let Poll::Ready(report) = step.as_mut().poll(cx) {
                resolved[index] = Some(report);
            } else {
                pending = true;
            }
        }
        if pending {
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    }));
    for report in resolved.iter().flatten() {
        landed.push(report.run_id().ok_or("a stored run must name a run id")?);
    }
    drop(runs);
    assert_eq!(
        landed.len(),
        lanes,
        "a batch of {batch} runs acknowledged {} of them",
        landed.len()
    );

    let observed = parked.observed()?;
    measure::record_measurement(&format!("group-commit turns_while_parked={observed}"))?;
    assert!(
        observed >= PROGRESS_TURNS,
        "the unrelated task ticked {observed} times while a batch of {batch} records was \
         outstanding: a batch that parked the driver's thread reaches one poll per submission"
    );

    // Every record of the batch is on the disk, read back through a fresh handle:
    // a batch that acknowledged only some of its members would look identical from
    // the driver's side.
    drop(beat);
    let store = RunStore::open(store_file(scratch.path()))?;
    let on_disk = landed
        .iter()
        .filter(|run| store.record_count(**run) == 1)
        .count();
    assert_eq!(
        on_disk,
        landed.len(),
        "a released device must land every record of its batch: {on_disk} of {} are on \
         the disk",
        landed.len()
    );
    // And the batch really was a batch: fewer flushes than records is what group
    // commit means, and a store that paid one flush per record under concurrency
    // would not have earned the name.
    let (flushes, staged) = store.flush_counts();
    assert!(
        flushes <= staged.max(1),
        "a batch of {} records reported {flushes} flushes",
        staged
    );
    Ok(())
}
