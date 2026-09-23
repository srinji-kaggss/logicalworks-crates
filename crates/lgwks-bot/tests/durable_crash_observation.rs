//! External observations for issue #109's release-gate register.
//!
//! The register's rows were repaired in code and unit-tested against
//! `MemoryJournal`, whose whole storage is a `Vec` in the process that would
//! have to survive the crash. What every row still needed was the *external*
//! observation: a real store on a real disk, a real process kill, a restart,
//! and an assertion that the recovered answer is the one the design names.
//! This file runs those observations.
//!
//! The kill is a real `SIGKILL` of a real child process (the test binary
//! re-executing itself in probe mode, the pattern `ecs_tick.rs` uses for its
//! watchdog). The store is [`FileJournal`] over a real file. What makes the
//! observation discriminating is the order the child writes: it appends
//! through the journal — whose append is `fsync`-ed before the acknowledgment
//! is minted — and only then touches a marker file. A marker therefore proves
//! the appends were acknowledged, and a kill after the marker proves the
//! acknowledgments were either kept or were lies.
//!
//! Rows observed here: #100 (external handoff admits on a durable record, and
//! the external marker never precedes the ack), #101 (a restart with a new
//! digest cannot fold into a completed attempt), #102 (a settlement followed
//! by a failed recording append is still the settlement), #104 (an unknown
//! stays a barrier across the kill), #106 (a live settlement survives the
//! kill; a duplicate stays idempotent).
//!
//! Rows still unobserved after this file: #99 (needs the ECS poll path under
//! a real store), #107 T21/T22 (needs a real descendant tree), #108 (needs a
//! real frame). They are named so they cannot quietly count as done.

use std::process::Child;
use std::time::Duration;

/// Owns the probe child and kills it, reaping it, however the test ends.
///
/// A test that returns early, or panics, between the spawn and the kill must
/// not leave a live child behind: the guard's drop is the backstop the
/// harness itself can forget. `take` hands the child to the kill harness,
/// which then owns the kill and the reap, so the guard drops empty and no
/// path kills twice.
struct ProbeGuard(Option<Child>);

impl ProbeGuard {
    /// Hand the child to the kill harness.
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

use lgwks_bot::effect::{
    ActionDigest, ActionId, AttemptId, EffectKey, EnvironmentEpoch, EnvironmentId, FlowRevision,
    Id128, RunId,
};
use lgwks_bot::journal::{
    AttemptStatus, DurabilityPromise, EffectEvent, EffectEvidence, EffectJournal, FileJournal,
    JournalError, Verification, VerificationResult,
};

const RUN: &str = "0102030405060708090a0b0c0d0e0f10";
const ACTION: &str = "1112131415161718191a1b1c1d1e1f20";
const ENV: &str = "2122232425262728292a2b2c2d2e2f30";
const FLOW_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const DIGEST_A: &str = "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeffe0e1e2e3e4e5e6e7e8e9eaebecedeeef";
const DIGEST_B: &str = "efeeedecebeae9e8e7e6e5e4e3e2e1e0dfdedddcdbdad9d8d7d6d5d4d3d2d1d0";
const PREDICATE: &str = "4142434445464748494a4b4c4d4e4f50";

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Removes a test's scratch directory when the test ends, however it ends.
///
/// The guard is best effort: a scratch directory the system refuses to remove
/// is litter, not a failed observation, so the error is dropped rather than
/// allowed to mask the test's own verdict.
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

/// One ladder rung, counted from one, in the order the journal enforces.
const RUNG_PREPARED: usize = 2;
const RUNG_APPLIED: usize = 3;
const RUNG_VERIFIED: usize = 4;

/// Environment variable that turns this test binary into a probe child.
const PROBE_ENV: &str = "LGWKS_JOURNAL_PROBE";
/// Environment variables carrying the probe's orders.
const PROBE_JOURNAL: &str = "LGWKS_PROBE_JOURNAL";
const PROBE_EVENTS: &str = "LGWKS_PROBE_EVENTS";
const PROBE_MARKER: &str = "LGWKS_PROBE_MARKER";

/// A key for one attempt at the shared intent, under `digest`.
fn key(attempt: &str, digest: &str) -> Result<EffectKey, Box<dyn std::error::Error>> {
    Ok(EffectKey::new(
        RunId::from_hex(RUN)?,
        ActionId::from_hex(ACTION)?,
        AttemptId::from_decimal(attempt)?,
        FlowRevision::from_tagged("blake3_256", FLOW_HEX)?,
        ActionDigest::from_tagged("blake3_256", digest)?,
        EnvironmentId::from_hex(ENV)?,
        EnvironmentEpoch::from_decimal("1")?,
    ))
}

/// A counter that gives concurrent test runs distinct scratch names.
///
/// Nanos plus a counter, not a process id: the OS reuses both pids and
/// threads, and a reused id must never make two runs share a journal.
static SCRATCH_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A unique scratch directory for one test.
fn scratch(name: &str) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    use std::sync::atomic::Ordering;
    let dir = std::env::temp_dir().join(format!(
        "lgwks-obs-{name}-{}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos(),
        SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Append the first `rungs` events of the standard ladder for `key`, each
/// through `compare_and_append` at the journal's own tail.
fn walk_ladder(
    journal: &mut FileJournal,
    key: EffectKey,
    rungs: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let ladder: Vec<EffectEvent> = vec![
        EffectEvent::IntentAdmitted { key },
        EffectEvent::DispatchPrepared { key },
        EffectEvent::OutcomeObserved {
            key,
            evidence: EffectEvidence::Applied,
        },
        EffectEvent::Verified {
            key,
            verification: Verification::new(
                Id128::from_hex(PREDICATE)?,
                1,
                lgwks_std::hash::blake3(b"postcondition observed"),
                VerificationResult::Satisfied,
            ),
        },
    ];
    for event in ladder.into_iter().take(rungs) {
        journal.compare_and_append(journal.tail(), &event)?;
    }
    Ok(())
}

/// Append a partial frame to the journal's file, as an append interrupted
/// mid-write leaves it: bytes after the last acknowledged record.
fn tear_tail(
    journal_path: &std::path::Path,
    bytes: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(journal_path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

// ── The probe child ─────────────────────────────────────────────────────────

/// A plain-process pause, for both branches of the harness that have no
/// runtime: the probe child parking between its appends and its kill, and the
/// parent polling for the child's marker.
///
/// The workspace's ban on `std::thread::sleep` exists because blocking an
/// executor thread stalls every task on it. Neither branch here has an
/// executor: this file's tests drive the journal synchronously, the child is
/// this same binary running one sync body, and the wait — a child parked
/// while the parent decides when it dies — is the observation's subject
/// rather than its scaffolding.
#[expect(
    clippy::disallowed_methods,
    reason = "the kill harness is a plain process with no async runtime and no reactor to \
              stall; the parked child and the marker poll are the observation's shape, and \
              `rt::time::sleep` cannot be awaited here"
)]
fn pause(millis: u64) {
    std::thread::sleep(Duration::from_millis(millis));
}

/// Run the probe body when this process is the child.
///
/// The child appends `PROBE_EVENTS` ladder events for attempt `"1"` to the
/// journal at `PROBE_JOURNAL` — each append fsync-ed before its acknowledgment
/// — then writes `PROBE_MARKER`, then parks. Parking is what makes the kill
/// land mid-flight: the parent decides when the process dies, and the only
/// facts the child has produced are the ones already on the disk.
fn probe_body() -> TestResult {
    let path = std::env::var_os(PROBE_JOURNAL)
        .ok_or("the probe child was started without a journal path")?;
    let events: usize = std::env::var(PROBE_EVENTS)
        .map_err(|_| "the probe child was started without an event count")?
        .parse()
        .map_err(|_| "the probe event count was not a number")?;
    let marker = std::env::var_os(PROBE_MARKER)
        .ok_or("the probe child was started without a marker path")?;

    let mut journal = FileJournal::open(&path)?;
    // The handoff gate on a real store: the admission half of row #100.
    let granted = journal.admit_external_handoff()?;
    assert_eq!(
        granted,
        DurabilityPromise::ProcessCrash,
        "the file journal must grant exactly the promise its appends earn"
    );

    walk_ladder(&mut journal, key("1", DIGEST_A)?, events)?;
    // The marker is written only after every append was acknowledged, so its
    // existence is the parent's proof that the acknowledgments were minted
    // before the process died.
    std::fs::write(&marker, b"acked")?;

    // Park until the parent kills us. Bounded, so a parent that never kills
    // cannot leave a stray process behind: the loop exits and the probe fails
    // loudly instead.
    for _ in 0..600 {
        pause(100);
    }
    Err("the probe child parked for its whole bound and was never killed".into())
}

/// Spawn this test binary as a probe child ordered to append `events` ladder
/// rungs and then park.
fn spawn_probe(
    test_name: &str,
    journal_path: &std::path::Path,
    marker_path: &std::path::Path,
    events: usize,
) -> Result<ProbeGuard, Box<dyn std::error::Error>> {
    let executable = std::env::current_exe()?;
    Ok(ProbeGuard(Some(
        std::process::Command::new(executable)
            .args([test_name, "--exact", "--nocapture"])
            .env(PROBE_ENV, "1")
            .env(PROBE_JOURNAL, journal_path)
            .env(PROBE_EVENTS, events.to_string())
            .env(PROBE_MARKER, marker_path)
            .spawn()?,
    )))
}

/// Wait until the probe's marker exists — its proof that every append was
/// acknowledged — then kill the child with a real `SIGKILL` and reap it.
///
/// `Child::kill` sends `SIGKILL` on Unix: no cleanup, no destructors, no
/// flushing. Everything the child wrote that is not on the disk is gone, and
/// everything that claimed to be durable had better be there.
fn kill_after_marker(
    mut guard: ProbeGuard,
    marker: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(mut child) = guard.take() else {
        return Err("the probe child was already gone before the kill".into());
    };
    for _ in 0..2_000 {
        if marker.exists() {
            child.kill()?;
            child.wait()?;
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            return Err(
                format!("the probe child exited on its own before the kill: {status}").into(),
            );
        }
        pause(5);
    }
    child.kill()?;
    child.wait()?;
    Err("the probe child never reached its marker; the observation has no kill to test".into())
}

// ── Row #106: the settlement survives the kill ──────────────────────────────

#[test]
fn a_settlement_recorded_before_a_real_kill_is_the_recovered_answer() -> TestResult {
    if std::env::var_os(PROBE_ENV).is_some() {
        return probe_body();
    }
    let dir = scratch("settlement")?;
    let _guard = TempGuard(dir.clone());
    let journal_path = dir.join("journal.log");
    let marker = dir.join("marker");

    let child = spawn_probe(
        "a_settlement_recorded_before_a_real_kill_is_the_recovered_answer",
        &journal_path,
        &marker,
        RUNG_APPLIED,
    )?;
    kill_after_marker(child, &marker)?;

    // The restart: a fresh controller opens what is on the disk.
    let mut journal = FileJournal::open(&journal_path)?;
    let recovered = journal.recover();
    let settled = key("1", DIGEST_A)?;
    assert_eq!(
        recovered.status(settled),
        Some(AttemptStatus::Applied),
        "the kill happened after the outcome was acknowledged; \
         the recovered answer must be that outcome"
    );

    // The duplicate: a second OutcomeObserved for the settled key is not a
    // second settlement, it is a refused append. Idempotent by ladder.
    let duplicate = journal.compare_and_append(
        journal.tail(),
        &EffectEvent::OutcomeObserved {
            key: settled,
            evidence: EffectEvidence::Applied,
        },
    );
    match duplicate {
        Err(JournalError::OutOfOrder {
            expected: Some(lgwks_bot::journal::EventKind::Verified),
            ..
        }) => {}
        Err(other) => return Err(format!("expected an out-of-order refusal, got {other}").into()),
        Ok(_) => return Err("a duplicate settlement must not be appendable".into()),
    }

    // The journal is still a journal after the kill: a new key climbs fresh.
    let next = key("2", DIGEST_A)?;
    journal.compare_and_append(journal.tail(), &EffectEvent::IntentAdmitted { key: next })?;
    assert_eq!(journal.recover().len(), 2);
    Ok(())
}

// ── Rows #102 and #104: the unknown is a barrier across the kill ────────────

#[test]
fn a_kill_between_prepare_and_outcome_recovers_a_barrier_not_an_answer() -> TestResult {
    if std::env::var_os(PROBE_ENV).is_some() {
        return probe_body();
    }
    let dir = scratch("barrier")?;
    let _guard = TempGuard(dir.clone());
    let journal_path = dir.join("journal.log");
    let marker = dir.join("marker");

    let child = spawn_probe(
        "a_kill_between_prepare_and_outcome_recovers_a_barrier_not_an_answer",
        &journal_path,
        &marker,
        RUNG_PREPARED,
    )?;
    kill_after_marker(child, &marker)?;

    let journal = FileJournal::open(&journal_path)?;
    let this_key = key("1", DIGEST_A)?;
    let recovered = journal.recover();
    assert_eq!(
        recovered.status(this_key),
        Some(AttemptStatus::OutcomeUnknown),
        "the bytes may have left; the only honest answer is unknown"
    );
    assert_ne!(
        recovered.status(this_key),
        Some(AttemptStatus::NotApplied),
        "nothing established that the bytes did not arrive"
    );
    assert_eq!(
        recovered.uncertain(),
        vec![this_key],
        "the barrier is named"
    );

    // The barrier settles by evidence, appended — never by a resend. The
    // restart appends the outcome and the attempt leaves uncertainty.
    let mut journal = journal;
    journal.compare_and_append(
        journal.tail(),
        &EffectEvent::OutcomeObserved {
            key: this_key,
            evidence: EffectEvidence::Applied,
        },
    )?;
    let settled = journal.recover();
    assert_eq!(settled.status(this_key), Some(AttemptStatus::Applied));
    assert!(settled.uncertain().is_empty());
    Ok(())
}

// ── Row #101: a new digest after the restart is a new identity ──────────────

#[test]
fn a_restart_with_a_new_digest_cannot_fold_into_a_completed_attempt() -> TestResult {
    if std::env::var_os(PROBE_ENV).is_some() {
        return probe_body();
    }
    let dir = scratch("identity")?;
    let _guard = TempGuard(dir.clone());
    let journal_path = dir.join("journal.log");
    let marker = dir.join("marker");

    let child = spawn_probe(
        "a_restart_with_a_new_digest_cannot_fold_into_a_completed_attempt",
        &journal_path,
        &marker,
        RUNG_APPLIED,
    )?;
    kill_after_marker(child, &marker)?;

    // The restart re-admits the same logical action under a changed input:
    // attempt `"2"`, digest `B`. The binding is to the digest, so this is a
    // different attempt with its own fresh ladder, not a continuation of the
    // one the disk says already applied.
    let mut journal = FileJournal::open(&journal_path)?;
    let first = key("1", DIGEST_A)?;
    let rebound = key("2", DIGEST_B)?;
    assert_ne!(first, rebound);
    journal.compare_and_append(
        journal.tail(),
        &EffectEvent::IntentAdmitted { key: rebound },
    )?;

    let recovered = journal.recover();
    assert_eq!(recovered.len(), 2, "two identities, never one");
    assert_eq!(recovered.status(first), Some(AttemptStatus::Applied));
    assert_eq!(recovered.status(rebound), Some(AttemptStatus::Prepared));
    Ok(())
}

// ── Row #100: the external marker never precedes the durable ack ────────────

#[test]
fn an_external_marker_appears_only_after_the_durable_ack() -> TestResult {
    if std::env::var_os(PROBE_ENV).is_some() {
        return probe_body();
    }
    let dir = scratch("handoff")?;
    let _guard = TempGuard(dir.clone());
    let journal_path = dir.join("journal.log");
    // For this row the marker is not scaffolding: it is the external effect
    // the row is about — an artifact on the disk, outside the dying process.
    // The child writes it only after its appends were acknowledged by a
    // journal that promises to survive the kill, so its existence plus the
    // kill is the observation.
    let marker = dir.join("external-effect");

    let child = spawn_probe(
        "an_external_marker_appears_only_after_the_durable_ack",
        &journal_path,
        &marker,
        RUNG_PREPARED,
    )?;
    kill_after_marker(child, &marker)?;

    let journal = FileJournal::open(&journal_path)?;
    let granted = journal.admit_external_handoff()?;
    assert!(
        granted.survives_process_crash(),
        "a real file journal must be the record a handoff may leave behind"
    );
    let this_key = key("1", DIGEST_A)?;
    assert_eq!(
        journal.recover().status(this_key),
        Some(AttemptStatus::OutcomeUnknown)
    );

    // The ordering the observation pins: the child wrote its external marker
    // only after the DispatchPrepared append was acknowledged, so both the
    // marker and the prepared record are here, and the record is on the disk
    // the kill did not touch.
    let committed = journal.committed()?;
    assert_eq!(committed.len(), RUNG_PREPARED);
    assert!(matches!(
        committed.last(),
        Some(EffectEvent::DispatchPrepared { .. })
    ));
    Ok(())
}

// ── Row #102, recorded side: the failed append after a settlement ───────────

#[test]
fn a_settlement_followed_by_a_failed_recording_append_is_still_the_settlement() -> TestResult {
    let dir = scratch("recording")?;
    let _guard = TempGuard(dir.clone());
    let journal_path = dir.join("journal.log");

    let mut journal = FileJournal::open(&journal_path)?;
    let this_key = key("1", DIGEST_A)?;
    walk_ladder(&mut journal, this_key, RUNG_APPLIED)?;

    // The recording failure: the next append dies mid-write. On a real store
    // that is a torn final frame — bytes after the last acknowledged record.
    tear_tail(&journal_path, &[0x00, 0x00, 0x00])?;

    let reopened = FileJournal::open(&journal_path)?;
    assert!(
        reopened.torn_tail_repaired(),
        "the interrupted append was never acknowledged; opening repairs it"
    );
    assert_eq!(
        reopened.recover().status(this_key),
        Some(AttemptStatus::Applied),
        "the settlement stands; a recording failure after it is not a refusal \
         and not an unknown"
    );
    assert_eq!(reopened.committed()?.len(), RUNG_APPLIED);
    Ok(())
}

// ── The mid-write kill, modelled at the byte level ──────────────────────────

#[test]
fn a_torn_final_frame_is_repaired_and_never_replayed() -> TestResult {
    let dir = scratch("torn")?;
    let _guard = TempGuard(dir.clone());
    let journal_path = dir.join("journal.log");

    let mut journal = FileJournal::open(&journal_path)?;
    let this_key = key("1", DIGEST_A)?;
    walk_ladder(&mut journal, this_key, RUNG_PREPARED)?;
    let acked_len = std::fs::metadata(&journal_path)?.len();

    // A kill mid-append leaves a partial frame. Both shapes a torn write
    // produces: a truncated frame body, and a truncated length prefix.
    tear_tail(&journal_path, &[0x00, 0x00, 0x01, 0x9f, 0xde, 0xad])?;

    let mut reopened = FileJournal::open(&journal_path)?;
    assert!(reopened.torn_tail_repaired());
    assert_eq!(reopened.committed()?.len(), RUNG_PREPARED);
    assert_eq!(
        reopened.recover().status(this_key),
        Some(AttemptStatus::OutcomeUnknown),
        "the torn frame was never acknowledged, so it is not an answer"
    );
    assert_eq!(
        std::fs::metadata(&journal_path)?.len(),
        acked_len,
        "the repair truncates back to the acknowledged prefix"
    );
    // And the repaired journal still appends: the kill cost the un-acked
    // frame, nothing else. The continuation is the ladder's next rung for the
    // same key — the attempt it was for, not a new one.
    reopened.compare_and_append(
        reopened.tail(),
        &EffectEvent::OutcomeObserved {
            key: this_key,
            evidence: EffectEvidence::Applied,
        },
    )?;
    assert_eq!(reopened.committed()?.len(), RUNG_APPLIED);
    assert_eq!(
        reopened.recover().status(this_key),
        Some(AttemptStatus::Applied)
    );
    Ok(())
}

// ── Tamper: a well-framed but lying record is refused, never truncated ──────

#[test]
fn a_tampered_committed_frame_is_refused_rather_than_trimmed() -> TestResult {
    let dir = scratch("tamper")?;
    let _guard = TempGuard(dir.clone());
    let journal_path = dir.join("journal.log");

    let mut journal = FileJournal::open(&journal_path)?;
    let this_key = key("1", DIGEST_A)?;
    walk_ladder(&mut journal, this_key, RUNG_APPLIED)?;

    // Flip one payload byte inside the last frame. This is not an interrupted
    // append; it is committed bytes that no longer mean what the chain says.
    let mut bytes = std::fs::read(&journal_path)?;
    let first_len = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    assert!(
        first_len > 16,
        "the first frame carries a real event, so the framing is real"
    );
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;
    std::fs::write(&journal_path, &bytes)?;

    match FileJournal::open(&journal_path) {
        Err(JournalError::Corrupt(corruption)) => {
            // Three committed frames; the lying one is the third, named from
            // zero.
            assert_eq!(corruption.at(), 2, "the lying frame is the one named");
        }
        Err(other) => return Err(format!("expected a corruption refusal, got {other}").into()),
        Ok(_) => return Err("tampered committed bytes must not reopen as a journal".into()),
    }
    Ok(())
}

// ── Parity: the real store folds the same answers the in-memory one does ────

#[test]
fn the_file_journal_recovers_exactly_what_the_memory_journal_recovers() -> TestResult {
    let dir = scratch("parity")?;
    let _guard = TempGuard(dir.clone());
    let journal_path = dir.join("journal.log");

    let mut file_journal = FileJournal::open(&journal_path)?;
    let this_key = key("1", DIGEST_A)?;
    walk_ladder(&mut file_journal, this_key, RUNG_VERIFIED)?;

    let mut memory_journal = lgwks_bot::journal::MemoryJournal::new();
    for event in file_journal.committed()? {
        memory_journal.compare_and_append(memory_journal.tail(), &event)?;
    }

    assert_eq!(
        file_journal.recover(),
        memory_journal.recover(),
        "one fold, one answer, whichever store carried the events"
    );
    assert_eq!(
        file_journal.recover().status(this_key),
        Some(AttemptStatus::Verified)
    );
    Ok(())
}
