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

use std::process::Child;
use std::time::Duration;

use lgwks_bot::effect::{
    ActionDigest, ActionId, AttemptId, EffectKey, EnvironmentEpoch, EnvironmentId, FlowRevision,
    RunId,
};
use lgwks_bot::journal::{
    DurabilityPromise, EffectEvent, EffectJournal, FileJournal, JournalError,
};

const RUN: &str = "0102030405060708090a0b0c0d0e0f10";
const ACTION: &str = "1112131415161718191a1b1c1d1e1f20";
const ENV: &str = "2122232425262728292a2b2c2d2e2f30";
const FLOW_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const DIGEST_HEX: &str = "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeffe0e1e2e3e4e5e6e7e8e9eaebecedeeef";

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Environment variable that turns this test binary into a probe child.
const PROBE_ENV: &str = "LGWKS_FENCE_PROBE";
/// Environment variables carrying the probe's orders.
const PROBE_JOURNAL: &str = "LGWKS_FENCE_JOURNAL";
const PROBE_MARKER: &str = "LGWKS_FENCE_MARKER";

/// Owns the probe child and kills it, reaping it, however the test ends.
struct ProbeGuard(Option<Child>);

impl ProbeGuard {
    /// Hand the child to the kill harness, so no path kills twice.
    fn take(&mut self) -> Option<Child> {
        self.0.take()
    }
}

impl Drop for ProbeGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            drop(child.kill());
            drop(child.wait());
        }
    }
}

/// A counter that gives concurrent test runs distinct scratch names.
static SCRATCH_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A unique scratch path for one test.
fn scratch(name: &str) -> std::path::PathBuf {
    use std::sync::atomic::Ordering;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!(
        "lgwks-fence-{name}-{}-{}",
        nanos,
        SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

fn key(attempt: &str) -> Result<EffectKey, Box<dyn std::error::Error>> {
    Ok(EffectKey::new(
        RunId::from_hex(RUN)?,
        ActionId::from_hex(ACTION)?,
        AttemptId::from_decimal(attempt)?,
        FlowRevision::from_tagged("blake3_256", FLOW_HEX)?,
        ActionDigest::from_tagged("blake3_256", DIGEST_HEX)?,
        EnvironmentId::from_hex(ENV)?,
        EnvironmentEpoch::from_decimal("1")?,
    ))
}

/// The kill harness is a plain process with no async runtime and no reactor
/// to stall; the parked child and the marker poll are the observation's
/// shape, and `rt::time::sleep` cannot be awaited here.
#[expect(
    clippy::disallowed_methods,
    reason = "the probe child parks while the parent decides when it dies, and the parent \
              polls for the child's marker; neither side runs an executor that a blocked \
              thread would starve"
)]
fn pause(millis: u64) {
    std::thread::sleep(Duration::from_millis(millis));
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

/// Removes a test's scratch path when the test ends, however it ends.
struct TempGuard(std::path::PathBuf);

impl Drop for TempGuard {
    fn drop(&mut self) {
        if self.0.is_dir() {
            drop(std::fs::remove_dir_all(&self.0));
        } else {
            drop(std::fs::remove_file(&self.0));
        }
    }
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
