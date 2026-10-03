//! The file journal's writer fence, observed across real processes.
//!
//! [`FileJournal`]'s replay, torn-tail repair and append are the owner's
//! alone: `open` takes the file's exclusive advisory lock before it reads a
//! byte, and a second opener is refused with [`JournalError::Locked`] instead
//! of scanning or repairing a file someone else is writing. What a unit test
//! cannot see is whether the fence holds across process boundaries and what
//! happens to it when the holder dies, so this file runs a real second
//! process: the test binary re-executing itself in probe mode, the pattern
//! `durable_crash_observation.rs` uses.
//!
//! The fence is the operating system's advisory file lock. It binds writers
//! that go through `FileJournal::open`; a writer that never asks for the lock
//! is outside its reach, and writers on other hosts need a lease, not a lock.
//! Both limits are stated on [`JournalError::Locked`] rather than hidden.

/// The scratch-path and cleanup-guard fixtures this file shares with the
/// journal liveness, scale and crash-observation families.
///
/// This file was the third copy of both, and the fence it observes is the same
/// device the others write to: a fourth reader of the disk must not be able to
/// assert a different cleanup discipline than the rest.
#[path = "support/journal.rs"]
mod shared;

use shared::{DIGEST_HEX, ProbeGuard, TempGuard, key_for, pause, scratch};

use lgwks_bot::effect::EffectKey;
use lgwks_bot::journal::{
    DurabilityPromise, EffectEvent, EffectJournal, FileJournal, JournalError,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Environment variable that turns this test binary into a probe child.
const PROBE_ENV: &str = "LGWKS_FENCE_PROBE";
/// Environment variables carrying the probe's orders.
const PROBE_JOURNAL: &str = "LGWKS_FENCE_JOURNAL";
const PROBE_MARKER: &str = "LGWKS_FENCE_MARKER";

/// The fence harness's key for attempt `attempt`, under the shared digest.
fn key(attempt: &str) -> Result<EffectKey, Box<dyn std::error::Error>> {
    key_for(attempt, DIGEST_HEX)
}

/// Run the probe body when this process is the child.
///
/// The child opens the journal — taking its fence — appends one event, writes
/// the marker, then parks with the lock held. Parking is what lets the parent
/// observe the fence while the holder is alive, and the kill is what moves
/// the fence back to the kernel.
fn probe_body() -> TestResult {
    let path = std::env::var_os(PROBE_JOURNAL).ok_or("probe started without a journal path")?;
    let marker = std::env::var_os(PROBE_MARKER).ok_or("probe started without a marker path")?;

    let mut journal = FileJournal::open(&path)?;
    journal.compare_and_append(
        journal.tail(),
        &EffectEvent::IntentAdmitted { key: key("2")? },
    )?;
    std::fs::write(&marker, b"held")?;
    pause(60_000);
    Ok(())
}

#[test]
fn a_writer_is_refused_while_the_fence_is_held_and_reacquired_after_the_holder_dies() -> TestResult
{
    if std::env::var_os(PROBE_ENV).is_some() {
        return probe_body();
    }

    let path = scratch("fence");
    let _guard = TempGuard(path.clone());

    // In one process: the open handle is the fence's owner, and a second
    // opener — even here, where both sides are trusted code — is refused
    // before it reads a byte. Dropping the owner releases the fence.
    let mut owner = FileJournal::open(&path)?;
    owner.compare_and_append(
        owner.tail(),
        &EffectEvent::IntentAdmitted { key: key("1")? },
    )?;
    match FileJournal::open(&path) {
        Err(JournalError::Locked { .. }) => {}
        Err(other) => {
            return Err(format!("expected a lock refusal, got {other}").into());
        }
        Ok(_) => return Err("a second open of a held journal was admitted".into()),
    }
    drop(owner);
    let owner = FileJournal::open(&path)?;
    assert_eq!(owner.committed()?.len(), 1, "release lost no events");
    drop(owner);

    // Across processes: a real child takes the fence, and while it holds it
    // this process is the one that must be refused — the refusal is what
    // keeps a second writer from scanning, repairing, or branching the chain
    // under a live appender. The child then dies holding the fence, and the
    // kernel hands it back: the reopen succeeds and the child's acknowledged
    // events are all still there.
    let marker = scratch("fence-marker");
    let _marker_guard = TempGuard(marker.clone());
    let child = {
        let exe = std::env::current_exe()?;
        std::process::Command::new(exe)
            .arg("journal_writer_fence")
            .arg("--exact")
            .arg("a_writer_is_refused_while_the_fence_is_held_and_reacquired_after_the_holder_dies")
            .env(PROBE_ENV, "1")
            .env(PROBE_JOURNAL, &path)
            .env(PROBE_MARKER, &marker)
            .spawn()?
    };
    let mut probe = ProbeGuard(Some(child));
    for _ in 0..200 {
        if marker.exists() {
            break;
        }
        pause(25);
    }
    assert!(
        marker.exists(),
        "the probe child never took the fence and wrote its marker"
    );

    match FileJournal::open(&path) {
        Err(JournalError::Locked { .. }) => {}
        Err(other) => {
            return Err(format!("expected a lock refusal against the child, got {other}").into());
        }
        Ok(_) => {
            return Err("a second process opened a journal another process holds".into());
        }
    }

    if let Some(mut held) = probe.take() {
        drop(held.kill());
        let status = held.wait()?;
        assert!(
            !status.success(),
            "the probe child should have died from the kill, not exited on its own"
        );
    }
    for _ in 0..100 {
        match FileJournal::open(&path) {
            Ok(_) => break,
            Err(JournalError::Locked { .. }) => pause(25),
            Err(other) => return Err(format!("reopen after the kill failed: {other}").into()),
        }
    }
    let reopened = FileJournal::open(&path)?;
    assert_eq!(
        reopened.committed()?.len(),
        2,
        "the fence released by death must hand back the holder's acknowledged events"
    );
    assert_eq!(
        reopened.durability(),
        DurabilityPromise::ProcessCrash,
        "the file-backed promise is unchanged by the fence"
    );
    Ok(())
}
