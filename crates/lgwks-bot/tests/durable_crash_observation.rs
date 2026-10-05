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

use lgwks_bot::effect::{EffectKey, Id128};
/// The scratch-path and cleanup-guard fixtures this file shares with the
/// journal liveness and scale families.
///
/// One definition of "a unique scratch path" and of "remove it when the test
/// ends", so this observation cannot drift into asserting a different cleanup
/// discipline than the families that also observe a real kill.
#[path = "support/journal.rs"]
mod shared;

use shared::{ProbeGuard, TempGuard, key_for as key, pause, scratch_dir};

use lgwks_bot::journal::{
    AttemptStatus, DurabilityPromise, EffectEvent, EffectEvidence, EffectJournal, FileJournal,
    JournalError, Verification, VerificationResult,
};

const DIGEST_A: &str = "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeffe0e1e2e3e4e5e6e7e8e9eaebecedeeef";
const DIGEST_B: &str = "efeeedecebeae9e8e7e6e5e4e3e2e1e0dfdedddcdbdad9d8d7d6d5d4d3d2d1d0";
const PREDICATE: &str = "4142434445464748494a4b4c4d4e4f50";

type TestResult = Result<(), Box<dyn std::error::Error>>;

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
/// Turns this test binary into a probe child that parks mid-append, with its
/// storage device held closed, and waits to be killed with nothing written.
const PROBE_STALLED: &str = "LGWKS_PROBE_STALLED";

/// Append the first `rungs` events of the standard ladder for `key`, each
/// through `compare_and_append` at the journal's own tail.
///
/// A prefix of the shared support module's ladder rather than a second copy of
/// it. Spelled out again here, the four rungs would be a second claim about what
/// an attempt looks like, and the two could come to disagree about which rung a
/// given attempt is on — which would make "the kill landed at this boundary" a
/// statement about the fixture rather than about the ladder.
fn walk_ladder(
    journal: &mut FileJournal,
    key: EffectKey,
    rungs: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    shared::walk_ladder(journal, key, PREDICATE, 1, rungs)
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
        .map_err(|error| format!("the probe child was started without an event count: {error}"))?
        .parse()
        .map_err(|error| format!("the probe event count was not a number: {error}"))?;
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

/// This test binary re-invoked as a named test with a fresh, empty environment.
///
/// One builder for every probe, so the executable and argument shape cannot
/// drift between the acknowledged-append probes and the stalled-append one.
fn probe_command(test_name: &str) -> Result<std::process::Command, Box<dyn std::error::Error>> {
    let executable = std::env::current_exe()?;
    let mut command = std::process::Command::new(executable);
    command.args([test_name, "--exact", "--nocapture"]);
    Ok(command)
}

/// Spawn this test binary as a probe child ordered to append `events` ladder
/// rungs and then park.
fn spawn_probe(
    test_name: &str,
    journal_path: &std::path::Path,
    marker_path: &std::path::Path,
    events: usize,
) -> Result<ProbeGuard, Box<dyn std::error::Error>> {
    Ok(ProbeGuard(Some(
        probe_command(test_name)?
            .env(PROBE_ENV, "1")
            .env(PROBE_JOURNAL, journal_path)
            .env(PROBE_EVENTS, events.to_string())
            .env(PROBE_MARKER, marker_path)
            .spawn()?,
    )))
}

/// Run the probe body instead of the test when this process is the child.
///
/// Every observation row is one function that is both the test and the probe it
/// spawns, which is what keeps a row's pre-kill appends and its parent's
/// post-restart reads in the same file. The dispatch is stated once here: a
/// row that forgot it would run its real body in the child, whose environment
/// names a journal it must instead park in, and the parent would hang waiting
/// for a marker no child ever writes.
fn dispatch_to_probe() -> Result<bool, Box<dyn std::error::Error>> {
    if std::env::var_os(PROBE_ENV).is_some() {
        probe_body()?;
        return Ok(true);
    }
    Ok(false)
}

/// One kill scenario's setup: a scratch directory, its guard, and the two paths
/// the probe child and its parent both need.
///
/// Three of the rows below are the same observation with a different rung count
/// and a different assertion after the restart, so the part before the
/// assertion is what they share. Naming it means the probe's contract — a
/// directory removed when the test ends, a journal under `journal.log`, a
/// liveness marker under `marker` — is stated once instead of being re-spelled
/// per row, and a row that forgets the guard fails the same way as the others
/// instead of silently leaking a directory.
///
/// The guard is returned rather than bound inside, because a `_guard` bound by
/// the caller is dropped at the end of that caller's scope, which is what makes
/// it live as long as the test.
fn kill_scenario(
    tag: &str,
    row: &str,
    events: usize,
) -> Result<(TempGuard, std::path::PathBuf), Box<dyn std::error::Error>> {
    let dir = scratch_dir(tag)?;
    let journal_path = dir.join("journal.log");
    let marker = dir.join("marker");
    let child = spawn_probe(row, &journal_path, &marker, events)?;
    kill_after_marker(child, &marker)?;
    Ok((TempGuard(dir), journal_path))
}

/// Spawn this test binary as a probe child that parks mid-append.
fn spawn_stalled_probe(
    test_name: &str,
    journal_path: &std::path::Path,
    marker_path: &std::path::Path,
) -> Result<ProbeGuard, Box<dyn std::error::Error>> {
    Ok(ProbeGuard(Some(
        probe_command(test_name)?
            .env(PROBE_STALLED, "1")
            .env(PROBE_JOURNAL, journal_path)
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
    guard.kill_after_marker(marker, "durable_crash_observation")
}

// ── T14: the four boundaries the ladder has, each against a real kill ────────
//
// The row names five crashes and one property: crash before intent ack, after
// ack before dispatch, after dispatch before response, after response before
// receipt, and during recovery; and no unknown effect is blindly replayed.
//
// The existing rows above cover two of them under a real kill. The three below
// are the remaining boundaries, and each is *modelled at the point the crash
// names* rather than by waiting for a window to open:
//
// - **after ack before dispatch** is the probe that acknowledges `IntentAdmitted`
//   and is killed before it appends the preparation. The window between those two
//   appends is where a controller decides whether to hand over, and the honest
//   recovered answer is `Prepared` — the intent is on the disk and nothing has
//   left the process, so the attempt is eligible for an attempt again and not
//   unknown.
// - **after response before receipt** is the probe that walks the whole ladder
//   and is killed after the outcome is on the disk but before anything verified
//   it. The outcome is the fact; `Verified` is the attestation. A kill between
//   them must recover the outcome, because the effect landed and a recovery path
//   that re-dispatched on the missing verification would duplicate it.
// - **during recovery** is the probe that is killed while it is *reading* a
//   journal a previous process wrote — the restart itself. Nothing new is
//   appended, so the file is unchanged, and the third restart reads the same
//   history: recovery that mutates is a recovery that can lose.
//
// Each row is one function that is both the test and the probe it spawns, the
// dispatch stated once at the top of the file.

/// What one probe child did before it was killed, for the parent to read.
///
/// A report file rather than a marker, because these rows assert *which* events
/// reached the disk and a marker can only say that the child got that far. The
/// child writes it after its last acknowledged append and before it parks, so
/// the report is itself evidence that those appends were acknowledged before the
/// kill.
const PROBE_REPORT: &str = "LGWKS_PROBE_REPORT";

/// What the probe child was told to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeRungs {
    /// Acknowledge `IntentAdmitted`, park, and be killed before the preparation.
    AckBeforeDispatch,
    /// Walk the whole ladder, park, and be killed before the verification.
    ResponseBeforeReceipt,
    /// Append nothing at all: park holding the journal, and be killed while a
    /// restart is reading it.
    DuringRecovery,
}

impl ProbeRungs {
    /// The environment-variable spelling the child reads.
    const fn as_str(self) -> &'static str {
        match self {
            Self::AckBeforeDispatch => "ack-before-dispatch",
            Self::ResponseBeforeReceipt => "response-before-receipt",
            Self::DuringRecovery => "during-recovery",
        }
    }

    /// The spelling this probe writes.
    fn from_str(text: &str) -> Result<Self, String> {
        match text {
            "ack-before-dispatch" => Ok(Self::AckBeforeDispatch),
            "response-before-receipt" => Ok(Self::ResponseBeforeReceipt),
            "during-recovery" => Ok(Self::DuringRecovery),
            other => Err(format!("{other:?} is not a probe rung this file knows")),
        }
    }
}

/// The probe child for the three boundary rows.
///
/// Each rung shape is the real thing: a `FileJournal` over a real file, appends
/// that are `fsync`-ed before their acknowledgments, and a real park the parent
/// decides when to end with a `SIGKILL`. The difference between the shapes is
/// only *where* in the ladder the kill lands, which is the thing each row is
/// about.
fn boundary_probe_body() -> TestResult {
    let path = std::env::var_os(PROBE_JOURNAL)
        .ok_or("the boundary probe was started without a journal path")?;
    let marker = std::env::var_os(PROBE_MARKER)
        .ok_or("the boundary probe was started without a marker path")?;
    let report = std::env::var_os(PROBE_REPORT)
        .ok_or("the boundary probe was started without a report path")?;
    let rung = ProbeRungs::from_str(
        &std::env::var("LGWKS_PROBE_RUNG")
            .map_err(|error| format!("the boundary probe was started without a rung: {error}"))?,
    )
    .map_err(std::io::Error::other)?;

    let key = key("1", DIGEST_A)?;
    let mut journal = FileJournal::open(&path)?;
    let handoff = journal.admit_external_handoff()?;
    assert_eq!(
        handoff,
        DurabilityPromise::ProcessCrash,
        "the file journal must grant exactly the promise its appends earn"
    );

    match rung {
        // Acknowledge the intent and stop. Nothing is prepared, so nothing has
        // left the process, and this is the boundary the row calls "after ack
        // before dispatch".
        ProbeRungs::AckBeforeDispatch => {
            journal.compare_and_append(journal.tail(), &EffectEvent::IntentAdmitted { key })?;
        }
        // Walk the ladder to the outcome and stop one rung short of the
        // verification: the response is recorded, its receipt is not.
        ProbeRungs::ResponseBeforeReceipt => {
            walk_ladder(&mut journal, key, RUNG_APPLIED)?;
        }
        // Fold a journal a previous process wrote and hold it. This probe
        // appends nothing at all, so the file is exactly what the parent left.
        ProbeRungs::DuringRecovery => {
            // A full recovery fold, not just a read: this is the restart doing
            // the work a restart does, and the parent's assertion is that the
            // fold moved nothing.
            let folded = journal.recover();
            assert!(
                !folded.is_empty(),
                "the journal this process was told to recover holds an attempt"
            );
        }
    }

    std::fs::write(&report, rung.as_str())?;
    std::fs::write(&marker, b"acked")?;

    // Park until the parent kills us. Bounded, so a parent that never kills
    // cannot leave a stray process behind.
    for _ in 0..600 {
        pause(100);
    }
    Err("the boundary probe parked for its whole bound and was never killed".into())
}

/// Run the boundary probe when this process is the child.
fn dispatch_to_boundary_probe() -> Result<bool, Box<dyn std::error::Error>> {
    if std::env::var_os("LGWKS_PROBE_RUNG").is_some() {
        boundary_probe_body()?;
        return Ok(true);
    }
    Ok(false)
}

/// Spawn one boundary probe and kill it once it has acknowledged its last append.
fn spawn_boundary_probe(
    test_name: &str,
    dir: &std::path::Path,
    rung: ProbeRungs,
) -> Result<(std::path::PathBuf, TempGuard), Box<dyn std::error::Error>> {
    let journal_path = dir.join("journal.log");
    let marker = dir.join("marker");
    let report = dir.join("report");
    let child = probe_command(test_name)?
        .env(PROBE_RUNG_ENV, rung.as_str())
        .env(PROBE_JOURNAL, &journal_path)
        .env(PROBE_MARKER, &marker)
        .env(PROBE_REPORT, &report)
        .spawn()?;
    kill_after_marker(ProbeGuard(Some(child)), &marker)?;
    Ok((journal_path, TempGuard(dir.to_path_buf())))
}

/// The environment variable naming which ladder boundary a probe is killed at.
const PROBE_RUNG_ENV: &str = "LGWKS_PROBE_RUNG";

// ── T14 row 1: killed after the intent ack, before the dispatch ─────────────

/// A kill after the intent is acknowledged and before anything is dispatched
/// recovers as `Prepared`, and the attempt may be dispatched because nothing has
/// left the process.
///
/// The half of the row that a "recover as unknown" answer would break. A kill
/// between the admission and the preparation has *not* handed anything over:
/// there is no `DispatchPrepared`, so no bytes are in flight, and a recovery path
/// that answered `OutcomeUnknown` here would make an effect that provably never
/// left the process sit behind a barrier forever. The control is the row below
/// this one, which kills the same child one rung later and gets the opposite
/// answer — so this is not a blanket "everything recovers as Prepared".
#[test]
fn a_kill_after_the_intent_ack_and_before_the_dispatch_recovers_as_prepared() -> TestResult {
    if dispatch_to_boundary_probe()? {
        return Ok(());
    }
    let dir = scratch_dir("ack-before-dispatch")?;
    let (journal_path, _guard) = spawn_boundary_probe(
        "a_kill_after_the_intent_ack_and_before_the_dispatch_recovers_as_prepared",
        &dir,
        ProbeRungs::AckBeforeDispatch,
    )?;

    let journal = FileJournal::open(&journal_path)?;
    let this_key = key("1", DIGEST_A)?;
    let committed = journal.committed()?;
    assert_eq!(
        committed.len(),
        1,
        "the acknowledged intent is the only event the kill could leave"
    );
    assert!(
        matches!(committed.first(), Some(EffectEvent::IntentAdmitted { .. })),
        "and it is the admission, not a preparation: {committed:?}"
    );

    let recovered = journal.recover();
    assert_eq!(
        recovered.status(this_key),
        Some(AttemptStatus::Prepared),
        "nothing was handed over, so the recovered answer is Prepared"
    );
    assert!(
        recovered.uncertain().is_empty(),
        "an attempt that was never dispatched is not an unknown one: {:?}",
        recovered.uncertain()
    );

    // And it is genuinely dispatchable: the next rung for the same key is the
    // preparation, and a recovery path that only *thought* it was prepared would
    // be refused here.
    let mut journal = journal;
    journal.compare_and_append(
        journal.tail(),
        &EffectEvent::DispatchPrepared { key: this_key },
    )?;
    assert_eq!(
        journal.recover().status(this_key),
        Some(AttemptStatus::OutcomeUnknown),
        "dispatching it moves the attempt to the barrier, which is the other \
         row's answer"
    );
    Ok(())
}

// ── T14 row 2: killed after the response, before the receipt ───────────────

/// A kill after the outcome is recorded and before it is verified recovers the
/// outcome, and the effect is not dispatched again.
///
/// The boundary that decides whether a verified-but-unrecorded effect is
/// duplicated. The outcome append is the *fact* — the bytes reached the world —
/// and the verification is an attestation about that fact made afterwards. A kill
/// between them must therefore recover `Applied` and leave nothing uncertain: a
/// recovery path that treated the missing verification as an unknown would offer
/// to dispatch the effect a second time, and the journal is what refuses it.
///
/// The ladder is the mechanism, and it is asserted rather than assumed: the
/// `Verified` rung for this key is now the only one left, so the duplicate
/// settlement the row forbids is not merely discouraged but unrepresentable.
#[test]
fn a_kill_after_the_response_and_before_the_receipt_recovers_the_outcome() -> TestResult {
    if dispatch_to_boundary_probe()? {
        return Ok(());
    }
    let dir = scratch_dir("response-before-receipt")?;
    let (journal_path, _guard) = spawn_boundary_probe(
        "a_kill_after_the_response_and_before_the_receipt_recovers_the_outcome",
        &dir,
        ProbeRungs::ResponseBeforeReceipt,
    )?;

    let mut journal = FileJournal::open(&journal_path)?;
    let this_key = key("1", DIGEST_A)?;
    assert_eq!(
        journal.committed()?.len(),
        RUNG_APPLIED,
        "the outcome is on the disk; the verification is not"
    );
    assert!(
        !journal.torn_tail_repaired(),
        "the kill lost no acknowledged frame, so there is nothing to repair"
    );

    let recovered = journal.recover();
    assert_eq!(
        recovered.status(this_key),
        Some(AttemptStatus::Applied),
        "the effect landed and its outcome was acknowledged; a kill before the \
         verification cannot un-apply it"
    );
    assert!(
        recovered.uncertain().is_empty(),
        "an acknowledged outcome is not an unknown, whatever happened to its \
         verification: {:?}",
        recovered.uncertain()
    );
    assert!(
        !recovered
            .status(this_key)
            .is_some_and(AttemptStatus::is_uncertain),
        "and the status an external system is asked about is the settled one"
    );

    // The duplicate the row forbids is not representable: the ladder for this
    // key has exactly one rung left, and it is the verification.
    match journal.compare_and_append(
        journal.tail(),
        &EffectEvent::OutcomeObserved {
            key: this_key,
            evidence: EffectEvidence::Applied,
        },
    ) {
        Err(JournalError::OutOfOrder {
            expected: Some(lgwks_bot::journal::EventKind::Verified),
            ..
        }) => {}
        Err(other) => return Err(format!("expected an out-of-order refusal, got {other}").into()),
        Ok(_) => return Err("a settled effect accepted a second settlement".into()),
    }

    // The verification is what the restart adds, and adding it changes the status
    // without changing the fact.
    journal.compare_and_append(
        journal.tail(),
        &EffectEvent::Verified {
            key: this_key,
            verification: Verification::new(
                Id128::from_hex(PREDICATE)?,
                1,
                lgwks_std::hash::blake3(b"the postcondition the restart observed"),
                VerificationResult::Satisfied,
            ),
        },
    )?;
    let verified = journal.recover();
    assert_eq!(
        verified.status(this_key),
        Some(AttemptStatus::Verified),
        "the restart's verification is what moves the attempt from settled to verified"
    );
    assert_eq!(
        verified.uncertain().len(),
        0,
        "and verifying never reintroduces uncertainty"
    );
    Ok(())
}

/// A scratch journal path with its cleanup guard, and the one key the verdict
/// tests climb, so each test states only what it appends and what it expects.
fn verdict_fixture(
    name: &str,
) -> Result<(TempGuard, std::path::PathBuf, EffectKey), Box<dyn std::error::Error>> {
    let dir = scratch_dir(name)?;
    let guard = TempGuard(dir.clone());
    Ok((guard, dir.join("journal.log"), key("1", DIGEST_A)?))
}

/// Append a `Verified` event for `key`, at the journal's own tail, saying that the
/// predicate at `version` over observations specific to that version `result`.
fn append_verdict(
    journal: &mut FileJournal,
    key: EffectKey,
    version: u64,
    result: VerificationResult,
) -> TestResult {
    journal.compare_and_append(
        journal.tail(),
        &EffectEvent::Verified {
            key,
            verification: Verification::new(
                Id128::from_hex(PREDICATE)?,
                version,
                lgwks_std::hash::blake3(format!("observed at version {version}").as_bytes()),
                result,
            ),
        },
    )?;
    Ok(())
}

// ── #257: a predicate that did not hold survives a reopen as its own state ──

/// A verification that did not hold is recovered from a real file, after the
/// handle that wrote it is gone, as a state of its own: not `Applied`, which an
/// attempt also reads as before any predicate ran, and not uncertainty, because
/// the effect is known to have landed. The answer comes from `Recovered`, with no
/// walk through the raw events.
#[test]
fn a_failed_verification_survives_a_reopen_as_its_own_state() -> TestResult {
    let (_guard, path, this_key) = verdict_fixture("failed-verification")?;
    {
        let mut journal = FileJournal::open(&path)?;
        walk_ladder(&mut journal, this_key, RUNG_APPLIED)?;
        append_verdict(&mut journal, this_key, 1, VerificationResult::NotSatisfied)?;
    }

    let reopened = FileJournal::open(&path)?.recover();
    assert_eq!(
        reopened.status(this_key),
        Some(AttemptStatus::VerificationFailed),
        "the failure is the recovered answer"
    );
    assert_ne!(reopened.status(this_key), Some(AttemptStatus::Applied));
    assert_eq!(
        reopened.uncertain().len(),
        0,
        "the effect landed, so a failed predicate is not an unknown outcome"
    );
    Ok(())
}

// ── #260: a verdict revised after a restart ────────────────────────────────

/// A `Verified` attempt is reopened after the world has moved, and a later
/// verification that no longer holds is recorded against the same key. The
/// recovered status follows it, names the predicate version it was decided at, and
/// keeps what it superseded, so an operator can read what changed after a restart.
#[test]
fn a_verdict_is_revised_by_a_later_verification_after_a_reopen() -> TestResult {
    let (_guard, path, this_key) = verdict_fixture("revised-verification")?;
    {
        let mut journal = FileJournal::open(&path)?;
        walk_ladder(&mut journal, this_key, RUNG_APPLIED)?;
        append_verdict(&mut journal, this_key, 1, VerificationResult::Satisfied)?;
    }
    {
        let mut journal = FileJournal::open(&path)?;
        assert_eq!(
            journal.recover().status(this_key),
            Some(AttemptStatus::Verified),
            "the first verdict survives the reopen"
        );
        append_verdict(&mut journal, this_key, 2, VerificationResult::NotSatisfied)?;
    }

    let recovered = FileJournal::open(&path)?.recover();
    assert_eq!(
        recovered.status(this_key),
        Some(AttemptStatus::VerificationFailed),
        "the later verdict is the recovered one"
    );
    assert_eq!(
        recovered
            .verification(this_key)
            .map(|found| found.predicate_version()),
        Some(2),
        "and it says which predicate version decided it"
    );
    let last = recovered
        .history(this_key)
        .last()
        .ok_or("a recovered attempt has a history")?;
    assert_eq!(
        (last.from(), last.to()),
        (
            Some(AttemptStatus::Verified),
            AttemptStatus::VerificationFailed
        ),
        "the history names what the verdict superseded"
    );
    assert_eq!(recovered.uncertain().len(), 0);
    Ok(())
}

// ── T14 row 3: killed while recovering ─────────────────────────────────────

/// A kill while a restart is reading the journal leaves the file exactly as the
/// process that wrote it left it, and the next restart reads the same history.
///
/// The boundary that is easy to skip and expensive to get wrong: recovery itself
/// is a window in which the crashing process can do damage. A replay that
/// repairs a torn tail is *writing*, and a reader that wrote would turn an
/// interrupted append — which was never anyone's answer — into a committed one.
///
/// Three opens in a row, with a real kill in the middle of the second, is what
/// makes this an observation rather than an assertion about one process: the
/// bytes after the kill are compared against the bytes before it, and the history
/// the third reader folds is the history the first wrote.
#[test]
fn a_kill_during_recovery_leaves_the_journal_exactly_as_it_was() -> TestResult {
    if dispatch_to_boundary_probe()? {
        return Ok(());
    }
    // The first process writes the ladder and dies; its bytes are the subject.
    // A recovery window needs something to recover, so the parent writes the
    // ladder itself and then hands the finished file to a child that only reads.
    let dir = scratch_dir("during-recovery")?;
    let journal_path = dir.join("journal.log");
    {
        let mut journal = FileJournal::open(&journal_path)?;
        walk_ladder(&mut journal, key("1", DIGEST_A)?, RUNG_APPLIED)?;
    }
    // The bytes this process wrote, kept so the assertion after the kill is a
    // comparison against a known subject rather than against whatever is left.
    let after_seed = std::fs::read(&journal_path)?;

    let (journal_path, _guard) = spawn_boundary_probe(
        "a_kill_during_recovery_leaves_the_journal_exactly_as_it_was",
        &dir,
        ProbeRungs::DuringRecovery,
    )?;

    // The reading child appended nothing and repaired nothing: the file holds
    // exactly the events the writer acknowledged, byte for byte.
    let before = std::fs::read(&journal_path)?;
    assert_eq!(
        before, after_seed,
        "a kill while a journal was being read must leave the file exactly as \
         the process that wrote it left it"
    );

    let reader = FileJournal::open(&journal_path)?;
    let history = reader.committed()?;
    assert_eq!(
        history.len(),
        RUNG_APPLIED,
        "a reader that appends nothing sees the ladder it was left"
    );
    assert!(
        !reader.torn_tail_repaired(),
        "a complete file is not torn, so a reader repaired nothing"
    );
    let this_key = key("1", DIGEST_A)?;
    assert_eq!(
        reader.recover().status(this_key),
        Some(AttemptStatus::Applied),
        "and the recovery it folds is the settled answer, not a barrier"
    );
    drop(reader);

    assert_eq!(
        std::fs::read(&journal_path)?,
        before,
        "reading a journal must not move a byte: the file before the recovery \
         and the file after it are the same bytes"
    );

    // The same history again, from a reader that never saw the first: recovery is
    // a fold over committed evidence, so it is repeatable.
    let again = FileJournal::open(&journal_path)?;
    assert_eq!(
        again.committed()?,
        history,
        "a second recovery reads the same history, not a repaired one"
    );
    assert_eq!(
        std::fs::read(&journal_path)?,
        before,
        "and still moves no byte"
    );
    Ok(())
}

#[test]
fn a_settlement_recorded_before_a_real_kill_is_the_recovered_answer() -> TestResult {
    if dispatch_to_probe()? {
        return Ok(());
    }
    let (_guard, journal_path) = kill_scenario(
        "settlement",
        "a_settlement_recorded_before_a_real_kill_is_the_recovered_answer",
        RUNG_APPLIED,
    )?;

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
    if dispatch_to_probe()? {
        return Ok(());
    }
    let (_guard, journal_path) = kill_scenario(
        "barrier",
        "a_kill_between_prepare_and_outcome_recovers_a_barrier_not_an_answer",
        RUNG_PREPARED,
    )?;

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
    if dispatch_to_probe()? {
        return Ok(());
    }
    let (_guard, journal_path) = kill_scenario(
        "identity",
        "a_restart_with_a_new_digest_cannot_fold_into_a_completed_attempt",
        RUNG_APPLIED,
    )?;

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
    if dispatch_to_probe()? {
        return Ok(());
    }
    let dir = scratch_dir("handoff")?;
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
    let dir = scratch_dir("recording")?;
    let _guard = TempGuard(dir.clone());
    let journal_path = dir.join("journal.log");

    // The writer's handle is scoped: the fence is exclusive, so the reopen
    // below must not race a live first handle.
    let this_key = key("1", DIGEST_A)?;
    {
        let mut journal = FileJournal::open(&journal_path)?;
        walk_ladder(&mut journal, this_key, RUNG_APPLIED)?;

        // The recording failure: the next append dies mid-write. On a real
        // store that is a torn final frame — bytes after the last
        // acknowledged record.
        tear_tail(&journal_path, &[0x00, 0x00, 0x00])?;
    }

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
    let dir = scratch_dir("torn")?;
    let _guard = TempGuard(dir.clone());
    let journal_path = dir.join("journal.log");

    // The writer's handle is scoped: the fence is exclusive, so the reopen
    // below must not race a live first handle.
    let this_key = key("1", DIGEST_A)?;
    let acked_len;
    {
        let mut journal = FileJournal::open(&journal_path)?;
        walk_ladder(&mut journal, this_key, RUNG_PREPARED)?;
        acked_len = std::fs::metadata(&journal_path)?.len();

        // A kill mid-append leaves a partial frame. Both shapes a torn write
        // produces: a truncated frame body, and a truncated length prefix.
        tear_tail(&journal_path, &[0x00, 0x00, 0x01, 0x9f, 0xde, 0xad])?;
    }

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
    let dir = scratch_dir("tamper")?;
    let _guard = TempGuard(dir.clone());
    let journal_path = dir.join("journal.log");

    // The writer's handle is scoped: the fence is exclusive, so the refused
    // reopen below must not race a live first handle.
    {
        let mut journal = FileJournal::open(&journal_path)?;
        let this_key = key("1", DIGEST_A)?;
        walk_ladder(&mut journal, this_key, RUNG_APPLIED)?;

        // Flip one payload byte inside the last frame. This is not an
        // interrupted append; it is committed bytes that no longer mean what
        // the chain says.
        let mut bytes = std::fs::read(&journal_path)?;
        let first_len = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        assert!(
            first_len > 16,
            "the first frame carries a real event, so the framing is real"
        );
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        std::fs::write(&journal_path, &bytes)?;
    }

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
    let dir = scratch_dir("parity")?;
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

// ── A real kill while an append is in flight, on a store that is not answering

/// The child that dies mid-append.
///
/// It opens a journal whose storage device is held closed, hands the owner one
/// append — which cannot complete until the device is released — writes the
/// marker, and waits to be killed. Nothing is on the disk when the kill lands,
/// so the reopen must show an empty, clean journal and the retry must land
/// exactly once: no duplicate, no lost receipt.
fn stalled_probe_body() -> TestResult {
    use std::task::{Context, Waker};

    let path = std::env::var_os(PROBE_JOURNAL)
        .ok_or("the stalled probe was started without a journal path")?;
    let marker = std::env::var_os(PROBE_MARKER)
        .ok_or("the stalled probe was started without a marker path")?;

    let mut journal = FileJournal::open_with_stalled_storage(&path)?;
    let event = EffectEvent::IntentAdmitted {
        key: key("1", DIGEST_A)?,
    };
    let tail = journal.tail();
    let mut append = Box::pin(journal.compare_and_append_async(tail, &event));
    // One poll: the owner is handed the request and parks on the closed device.
    // The append is now genuinely in flight, and no byte has reached the disk.
    // A single poll needs no runtime, so this kill test runs under every feature
    // set rather than only where `rt` is compiled in.
    let mut cx = Context::from_waker(Waker::noop());
    if append.as_mut().poll(&mut cx).is_ready() {
        let refusal = Err("the stalled append completed before the kill".into());
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "stalled_probe_body: returning an error to the caller");
        return refusal;
    }
    std::fs::write(&marker, b"in-flight")?;

    // Park until the parent kills us. Bounded, so a parent that never kills
    // cannot leave a stray process behind.
    for _ in 0..600 {
        pause(100);
    }
    Err("the stalled probe parked for its whole bound and was never killed".into())
}

#[test]
fn a_real_kill_mid_append_leaves_no_duplicate_and_no_lost_receipt() -> TestResult {
    if std::env::var_os(PROBE_STALLED).is_some() {
        return stalled_probe_body();
    }
    let dir = scratch_dir("midappend")?;
    let journal_path = dir.join("journal.log");
    let marker = dir.join("marker");
    let _guard = TempGuard(dir);
    let child = spawn_stalled_probe(
        "a_real_kill_mid_append_leaves_no_duplicate_and_no_lost_receipt",
        &journal_path,
        &marker,
    )?;
    kill_after_marker(child, &marker)?;

    // The append never completed: the owner was parked, so nothing reached the
    // disk. A clean, empty journal is the only honest answer — not a torn tail
    // and not a phantom committed event.
    let mut journal = FileJournal::open(&journal_path)?;
    assert!(
        !journal.torn_tail_repaired(),
        "an append that wrote nothing cannot leave a torn tail"
    );
    assert_eq!(
        journal.committed()?.len(),
        0,
        "an in-flight append killed before its device answered must not commit"
    );

    // The retry lands exactly once: there was no acknowledgment to lose, and
    // the fact on the disk is the first and only one for the attempt.
    let this_key = key("1", DIGEST_A)?;
    let ack = journal.compare_and_append(
        journal.tail(),
        &EffectEvent::IntentAdmitted { key: this_key },
    )?;
    assert_eq!(
        ack.promise(),
        DurabilityPromise::ProcessCrash,
        "the retry earns the same durable promise the killed append would have"
    );
    drop(journal);
    let reopened = FileJournal::open(&journal_path)?;
    assert_eq!(
        reopened.committed()?.len(),
        1,
        "the retry must land exactly once, never a duplicate"
    );
    assert_eq!(
        reopened.recover().status(this_key),
        Some(AttemptStatus::Prepared),
        "the retry's own admission is the recovered answer"
    );
    Ok(())
}
