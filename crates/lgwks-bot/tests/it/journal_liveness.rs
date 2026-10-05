//! Slow-store liveness: a parked device must not stop the runtime.
//!
//! This is the acceptance for #122 item 2/4 and the #156 "Controller/storage
//! liveness" row, in the shape the review asked for: the instrument is an
//! **independent OS thread** — not the runtime that is awaiting the append, and
//! not the storage owner — and the two things it watches are that an unrelated
//! ready task keeps getting turns and that the release it performs is what
//! finally lets the append finish.
//!
//! The device is not merely slow, it is *parked*: [`FileJournal`] is opened with
//! its storage gate closed, so the flush provably cannot complete until the
//! watchdog thread releases it. A tree whose durable write ran on the thread
//! driving the future would never reach the release, the unrelated task would
//! never tick, and the watchdog would time out — which is why the assertion is
//! on the tick count and not on a duration.
#![cfg(feature = "rt")]

use std::error::Error;
use std::future::{Future, poll_fn};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::task::Poll;
use std::time::{Duration, Instant};

use lgwks_bot::journal::{DurabilityPromise, EffectEvent, EffectJournal, FileJournal};

use crate::journal_fixtures as fixtures;

use crate::resume_fixtures as shared;

use fixtures::{TempGuard, key, scratch};
use shared::{PROGRESS_TURNS, Parked, heartbeat};

type TestResult = Result<(), Box<dyn Error>>;

/// Consume an expression for its side effect without binding it.
fn ignore<T>(_: T) {}

/// A parked journal with the two instruments both liveness tests need.
///
/// One constructor rather than a copy per test, because the watchdog contract —
/// an independent thread that releases the device once the unrelated task has
/// progressed — is the thing under test, and a second copy is a second thing to
/// drift.
/// A parked device must not stop an unrelated ready task from getting turns.
///
/// The append is awaited on the runtime being driven; the unrelated task is
/// polled on the same driver each round. If the durable wait were on the
/// driver's thread, the unrelated task would never tick — so the assertion is
/// `ticks >= PROGRESS_TURNS`, which a blocking implementation fails by
/// construction.
#[test]
fn a_slow_store_lets_the_runtime_and_the_release_progress() -> TestResult {
    let path = scratch("progress");
    let _guard = TempGuard(path.clone());
    let mut journal = FileJournal::open_with_stalled_storage(&path)?;
    let parked = Parked::at(Some(journal.tail()), journal.storage_gate())?;
    let event = EffectEvent::IntentAdmitted { key: key(1)? };

    let mut heartbeat = Box::pin(heartbeat(Arc::clone(&parked.ticks())));
    let mut append = Box::pin(journal.compare_and_append_async(parked.tail()?, &event));
    let ack = lgwks_bot::rt::runtime::block_on(poll_fn(|cx| {
        // The unrelated task gets a turn before the append is polled, and its
        // wake keeps the driver moving; if the append blocked here, this line
        // would be reached at most once.
        ignore(heartbeat.as_mut().poll(cx));
        match append.as_mut().poll(cx) {
            Poll::Ready(answer) => Poll::Ready(answer),
            Poll::Pending => Poll::Pending,
        }
    }))?;
    drop(append);

    let observed = parked.observed()?;
    assert!(
        observed >= PROGRESS_TURNS,
        "the unrelated task ticked {observed} times while a parked flush was \
         outstanding: the durable wait is running on the driver's thread"
    );
    assert_eq!(
        ack.promise(),
        DurabilityPromise::ProcessCrash,
        "the acknowledgment still has to be earned, and a released device earns it"
    );

    drop(journal);
    let reopened = FileJournal::open(&path)?;
    assert_eq!(
        reopened.events().count(),
        1,
        "the released frame must be on the disk, not only in the handle"
    );
    Ok(())
}

/// A caller that walks away from a parked append must not wedge the runtime,
/// and the abandoned handle must refuse rather than carry on.
///
/// Phase one parks the append and abandons it. Phase two keeps driving the
/// unrelated task on the same thread until the **independent** watchdog has
/// released the device, so the tick count is what the runtime achieved while
/// the abandoned flush was outstanding. The handle must then refuse a further
/// append — the poison INV-BOT-15 requires once a waiter is gone.
#[test]
fn a_cancelled_append_leaves_the_runtime_and_the_handle_live() -> TestResult {
    let path = scratch("cancel");
    let _guard = TempGuard(path.clone());
    let mut journal = FileJournal::open_with_stalled_storage(&path)?;
    let parked = Parked::at(Some(journal.tail()), journal.storage_gate())?;
    let event = EffectEvent::IntentAdmitted { key: key(2)? };

    let mut heartbeat = Box::pin(heartbeat(Arc::clone(&parked.ticks())));
    let mut append = Box::pin(journal.compare_and_append_async(parked.tail()?, &event));
    // Poll the append exactly once so the owner has the request, letting the
    // unrelated task tick in the same round. Then abandon it: the append is
    // parked, so the drop cannot be waiting on the device.
    let first = lgwks_bot::rt::runtime::block_on(poll_fn(|cx| {
        ignore(heartbeat.as_mut().poll(cx));
        Poll::Ready(append.as_mut().poll(cx))
    }));
    assert!(
        first.is_pending(),
        "the append completed before it could be abandoned"
    );
    drop(append);

    // Phase two: the runtime stays live while the abandoned flush is still
    // outstanding, turning the unrelated task until the independent watchdog
    // releases the device.
    lgwks_bot::rt::runtime::block_on(poll_fn(|cx| {
        if parked.released().load(Ordering::Relaxed) {
            return Poll::Ready(());
        }
        ignore(heartbeat.as_mut().poll(cx));
        Poll::Pending
    }));

    let observed = parked.observed()?;
    assert!(
        observed >= PROGRESS_TURNS,
        "the unrelated task ticked {observed} times while a parked flush was \
         outstanding: the cancellation blocked the driver"
    );

    // The owner finished the write it was given and found no waiter, so the
    // handle must refuse the next append with the poison fence. Polled against
    // a deadline because the owner latches the poison only after its real
    // `sync_all`; in the window before that, the length fence can answer first,
    // and the test waits through it rather than accepting it as the poison.
    let deadline = Instant::now().checked_add(Duration::from_secs(10));
    loop {
        match journal.compare_and_append(journal.tail(), &event) {
            Err(error) if error.to_string().contains("previous append failed") => break,
            Ok(_) | Err(_) if deadline.is_some_and(|limit| Instant::now() >= limit) => {
                return Err("the abandoned append never poisoned the handle".into());
            }
            Ok(_) | Err(_) => std::thread::park_timeout(Duration::from_millis(1)),
        }
    }
    Ok(())
}
