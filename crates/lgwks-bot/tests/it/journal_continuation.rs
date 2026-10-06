//! Continue-as-new: the journey, end to end, against a real file journal.
//!
//! Every arm here drives the **shipped** [`FileJournal`]: its real frame codec,
//! its real storage-owner thread, its real chain verification, and the real
//! [`recover`](lgwks_bot::journal::recover) fold. Nothing in this file
//! reimplements a journal or a checkpoint, because a second journal written "for
//! the test" would pass while the shipped one rotted.
//!
//! The arms, one property each:
//!
//! | Arm | Property it pins |
//! |---|---|
//! | `a_run_continues_at_its_watermark_and_the_predecessor_is_sealed_read_only` | the trigger fires, and the sealed file refuses every later append |
//! | `an_unresolved_attempt_stays_unknown_across_a_continuation_and_settles_without_a_resend` | nothing unresolved is dropped, and nothing is resent |
//! | `a_verification_digest_crosses_the_boundary` | the successor reports the verification a settled status is qualified by |
//! | `a_replayed_settled_attempt_is_refused_rather_than_admitted_again` | a re-presentation of a sealed attempt cannot duplicate an effect |
//! | `two_tenants_in_one_directory_continue_independently` | no key crosses tenants, and each tenant has its own chain of files |
//! | `a_reopen_follows_the_generation_chain_to_the_live_journal` | a restarting controller holding only the original path finds the live journal |
//! | `a_sealed_predecessor_refuses_itself_and_names_its_successor` | exactly one journal is authoritative, and the refusal names the other |
//! | `an_armed_continuation_refuses_at_the_boundary_it_reached` | every crash boundary is reachable and named |
//! | `durable_dispatch::the_controller_continues_its_own_journal_on_the_shipped_append_path` | the trigger is wired into `ecs`'s append path, not only into tests |
//! | `each_successor_is_the_next_generation_and_its_checkpoint_says_so` | no two files in one chain hold one generation |
//! | `a_seal_writes_exactly_its_own_frame_into_both_files` | a continuation is one frame in each file, and nothing else moves |

use std::error::Error;
use std::path::{Path, PathBuf};

use lgwks_bot::effect::{
    ActionDigest, ActionId, AttemptId, EffectIdentity, EffectKey, EnvironmentEpoch, EnvironmentId,
    FlowRevision, Id128, RunId,
};
use lgwks_bot::journal::{
    AttemptStatus, ContinuationPolicy, DurableAck, EffectEvent, EffectEvidence, EffectJournal,
    FileJournal, JournalError, JournalPosition, SealPause, Verification, VerificationResult,
};
use lgwks_std::hash::blake3;

use crate::journal_fixtures::{ACTION, DIGEST_HEX, ENV, FLOW_HEX, RUN, TempGuard, scratch_dir};

/// The predicate every ladder's verification names, as 32 hex characters.
const PREDICATE: &str = "42424242424242424242424242424242";

/// A test that needs a parsed value returns `Result` and propagates, because the
/// crate forbids a panicking path anywhere, tests included.
type TestResult = Result<(), Box<dyn Error>>;

/// The trigger this file's arms use: sixty-four events, and a byte trigger well
/// past what a journal that small can reach.
///
/// A declared policy rather than the shipped fraction, and the reason is the test's
/// own subject: the mechanism under test is "at the trigger, continue", and a test
/// that had to write eighty thousand events to reach the trigger would be a test
/// about the ceiling rather than about continuation.
/// `the_shipped_watermark_leaves_the_declared_headroom` pins the shipped numbers
/// themselves, and the 10-million-attempt run in `examples/journal_continuation.rs`
/// drives hundreds of real continuations.
const TEST_EVENT_WATERMARK: u64 = 64;
const TEST_BYTE_WATERMARK: u64 = 4 * 1024 * 1024;

/// The policy these arms open with.
fn policy() -> Result<ContinuationPolicy, Box<dyn Error>> {
    Ok(ContinuationPolicy::new(
        TEST_EVENT_WATERMARK,
        TEST_BYTE_WATERMARK,
    )?)
}

/// A key for `attempt` of `action`, under the shared run identity.
fn key_for_action(action: ActionId, attempt: u64) -> Result<EffectKey, Box<dyn Error>> {
    let run = RunId::from_hex(RUN)?;
    let environment = EnvironmentId::from_hex(ENV)?;
    let flow = FlowRevision::from_tagged("blake3_256", FLOW_HEX)?;
    let attempt = AttemptId::from_decimal(&attempt.to_string())?;
    let digest = ActionDigest::from_tagged("blake3_256", DIGEST_HEX)?;
    let epoch = EnvironmentEpoch::from_decimal("1")?;
    Ok(EffectIdentity::new(run, environment, flow).key(action, attempt, digest, epoch))
}

/// The action every attempt of one tenant belongs to.
///
/// One tenant per action, which is what "no key crosses tenants" means at the
/// journal's own level: a tenant's attempts are a different identity, so a fold
/// that mixed them would show up as an attempt under the wrong action.
fn tenant_action(tenant: u64) -> Result<ActionId, Box<dyn Error>> {
    let hex = format!("{tenant:032x}");
    Ok(ActionId::from_hex(&hex)?)
}

/// The shared action every attempt of the single-tenant arms belongs to.
fn shared_action() -> Result<ActionId, Box<dyn Error>> {
    Ok(ActionId::from_hex(ACTION)?)
}

/// A full four-rung ladder for `attempt` of the shared action.
fn ladder(attempt: u64) -> Result<Vec<EffectEvent>, Box<dyn Error>> {
    ladder_of(shared_action()?, attempt)
}

/// A full four-rung ladder for `attempt` of `action`.
///
/// The shared fixture's ladder, keyed. It is one definition of what a complete
/// attempt looks like for every harness in this crate, and a copy that walked
/// different rungs would be a second claim about the ladder rather than a shorter
/// spelling of this one.
fn ladder_of(action: ActionId, attempt: u64) -> Result<Vec<EffectEvent>, Box<dyn Error>> {
    crate::journal_fixtures::ladder(key_for_action(action, attempt)?, PREDICATE, 1)
}

/// Write `attempts` complete four-rung ladders, batched so the run costs one flush
/// per attempt rather than one per rung.
fn fill(journal: &mut FileJournal, from: u64, attempts: u64) -> Result<(), Box<dyn Error>> {
    for attempt in from..from.saturating_add(attempts) {
        journal.compare_and_append_all(&ladder(attempt)?)?;
    }
    Ok(())
}

/// Continue this handle and hand back the successor, refusing anything else.
///
/// One helper because "continue, then carry on with the successor" is the shape
/// every arm here runs, and a per-arm copy is a per-arm place to forget that the
/// predecessor is now read-only.
fn continue_once(journal: &mut FileJournal) -> Result<FileJournal, Box<dyn Error>> {
    journal
        .continue_as_file()?
        .ok_or_else(|| "a continuing journal must hand back a successor".into())
}

/// Append one event at the handle's own tail.
fn append(journal: &mut FileJournal, event: &EffectEvent) -> Result<DurableAck, JournalError> {
    journal.compare_and_append(journal.tail(), event)
}

/// A verification that did not hold, at `version`.
fn failed_verification(version: u64) -> Result<Verification, Box<dyn Error>> {
    Ok(Verification::new(
        Id128::from_hex(PREDICATE)?,
        version,
        blake3(b"the durable ladder's own predicate"),
        VerificationResult::NotSatisfied,
    ))
}

#[test]
fn a_run_continues_at_its_watermark_and_the_predecessor_is_sealed_read_only() -> TestResult {
    let dir = scratch_dir("seal")?;
    let _guard = TempGuard(dir.clone());
    let first = dir.join("run.jrnl");
    let mut journal = FileJournal::open_continuing_with(&first, policy()?)?;

    // Fifteen four-rung attempts, then the trigger.
    fill(&mut journal, 1, 15)?;
    assert!(
        !journal.continuation_watermark()?.is_due(),
        "sixty events must not have reached a sixty-four-event trigger"
    );
    let sealed_at = journal.tail();
    let sealed_path = journal.successor_path();

    let mut successor = continue_once(&mut journal)?;
    assert_eq!(
        journal.sealed_by(),
        Some(sealed_path.as_path()),
        "the handle reports where it sealed to, which is where it sealed"
    );
    assert_eq!(
        journal.committed()?.len(),
        60,
        "the seal frame is not an event, so the predecessor keeps its sixty events"
    );
    let refused = append(
        &mut journal,
        &EffectEvent::IntentAdmitted {
            key: key_for_action(shared_action()?, 1)?,
        },
    );
    assert!(
        matches!(refused, Err(JournalError::Superseded { .. })),
        "a sealed predecessor is read-only, and says why: {refused:?}"
    );

    // The successor's own chain starts at the predecessor's tail and its first
    // frame is the seal frame, so it is exactly one past.
    assert_eq!(
        successor.tail().sequence(),
        sealed_at.sequence().saturating_add(1),
        "the successor's first frame is the predecessor's seal, one past its tail"
    );
    assert_eq!(
        successor.base(),
        sealed_at,
        "and the successor names the predecessor's chain head as its own base"
    );
    assert!(
        successor.committed()?.is_empty(),
        "a successor's events are only what has been appended since its checkpoint"
    );
    assert!(
        append(
            &mut successor,
            &EffectEvent::IntentAdmitted {
                key: key_for_action(shared_action()?, 16)?
            }
        )
        .is_ok(),
        "and the successor admits work the sealed file refuses"
    );
    Ok(())
}

#[test]
fn an_unresolved_attempt_stays_unknown_across_a_continuation_and_settles_without_a_resend()
-> TestResult {
    let dir = scratch_dir("carry")?;
    let _guard = TempGuard(dir.clone());
    let first = dir.join("run.jrnl");
    let action = ActionId::from_hex(&"22".repeat(16))?;
    let mut journal = FileJournal::open_continuing_with(&first, policy()?)?;

    // One attempt handed over and nothing settled, then enough settled work to
    // reach the trigger.
    let stranded = key_for_action(action, 1)?;
    let two = ladder_of(action, 1)?;
    journal.compare_and_append_all(&two[..2])?;
    assert_eq!(
        journal.recover().status(stranded),
        Some(AttemptStatus::OutcomeUnknown),
        "the predecessor holds the attempt as unknown before the continuation"
    );
    fill(&mut journal, 2, 15)?;

    let mut successor = continue_once(&mut journal)?;
    drop(journal);

    assert_eq!(
        successor.recover().status(stranded),
        Some(AttemptStatus::OutcomeUnknown),
        "the successor holds the same attempt as unknown, never as absent"
    );
    assert!(
        successor.recover().uncertain().contains(&stranded),
        "and it is named in the uncertain list a recovery path must settle"
    );
    let checkpoint = successor
        .checkpoint()
        .ok_or("a successor carries a checkpoint")?;
    assert!(
        checkpoint
            .unresolved()
            .iter()
            .any(|carried| carried.key() == stranded),
        "the carried list names the unresolved attempt by full identity"
    );

    // A resend is refused: the ladder knows this key reached DispatchPrepared.
    let resend = successor.compare_and_append(
        successor.tail(),
        &EffectEvent::IntentAdmitted { key: stranded },
    );
    assert!(
        resend.is_err(),
        "an unknown attempt must not be re-admitted, which is how a duplicate \
         non-idempotent effect gets sent: {resend:?}"
    );

    // Evidence settles it, in the successor, with no resend and no second rung.
    let settled = append(
        &mut successor,
        &EffectEvent::OutcomeObserved {
            key: stranded,
            evidence: EffectEvidence::Applied,
        },
    )?;
    assert_eq!(
        successor.recover().status(stranded),
        Some(AttemptStatus::Applied),
        "the appended evidence settled the carried attempt at sequence {}",
        settled.position().sequence()
    );
    Ok(())
}

#[test]
fn a_verification_digest_crosses_the_boundary() -> TestResult {
    let dir = scratch_dir("verification")?;
    let _guard = TempGuard(dir.clone());
    let first = dir.join("run.jrnl");
    let action = ActionId::from_hex(&"33".repeat(16))?;
    let mut journal = FileJournal::open_continuing_with(&first, policy()?)?;

    // A ladder whose verification did not hold: the status a caller must not
    // confuse with an unverified success, and the digest it is qualified by.
    let verified = key_for_action(action, 1)?;
    let three = ladder_of(action, 1)?;
    journal.compare_and_append_all(&three[..3])?;
    append(
        &mut journal,
        &EffectEvent::Verified {
            key: verified,
            verification: failed_verification(9)?,
        },
    )?;
    // Sixteen settled attempts of a *different* action. Were they the same action
    // they would fold over this one, which is the compaction working: a successor
    // keeps the latest attempt per action, so this arm gives the failed verdict an
    // action of its own to be the latest of.
    let other = ActionId::from_hex(&"55".repeat(16))?;
    for attempt in 2..=16 {
        journal.compare_and_append_all(&ladder_of(other, attempt)?)?;
    }

    let successor = continue_once(&mut journal)?;
    drop(journal);

    assert_eq!(
        successor.recover().status(verified),
        Some(AttemptStatus::VerificationFailed),
        "the failed verification is carried as itself, not as an applied effect"
    );
    assert_eq!(
        successor
            .recover()
            .verification(verified)
            .ok_or("the verification digest must cross the boundary")?
            .predicate_version(),
        9,
        "and it is the revision the verdict was decided at"
    );
    Ok(())
}

#[test]
fn a_replayed_settled_attempt_is_refused_rather_than_admitted_again() -> TestResult {
    let dir = scratch_dir("replay")?;
    let _guard = TempGuard(dir.clone());
    let first = dir.join("run.jrnl");
    let mut journal = FileJournal::open_continuing_with(&first, policy()?)?;

    fill(&mut journal, 1, 3)?;
    let older = key_for_action(shared_action()?, 1)?;
    let latest = key_for_action(shared_action()?, 3)?;
    let successor = continue_once(&mut journal)?;
    drop(journal);

    // The successor does not carry attempt 1 by key, because `AttemptId` is
    // monotonic per action; it carries the fold that answers for it.
    let mut successor = successor;
    // Two arms, and the difference is the point. The **older** attempt is not in
    // the successor's ladder at all: it is refused by the folded record, which is
    // the only thing that remembers it. The **latest** attempt is in the ladder,
    // because the checkpoint seeded it, so the ladder refuses it and the fold never
    // has to be consulted. Both are refusals; neither is a duplicate.
    for (replay, by_the_fold) in [(older, true), (latest, false)] {
        let refused = successor.compare_and_append(
            successor.tail(),
            &EffectEvent::IntentAdmitted { key: replay },
        );
        let matched = match by_the_fold {
            true => matches!(refused, Err(JournalError::AttemptAlreadyWalked { .. })),
            false => matches!(refused, Err(JournalError::OutOfOrder { .. })),
        };
        assert!(
            matched,
            "a re-presentation of a sealed attempt is refused, by the fold for an \
             older attempt and by the ladder for the folded key itself: {refused:?}"
        );
    }
    let fresh = key_for_action(shared_action()?, 4)?;
    assert!(
        successor
            .compare_and_append(
                successor.tail(),
                &EffectEvent::IntentAdmitted { key: fresh }
            )
            .is_ok(),
        "and the next attempt of the same action is new work, so it is admitted"
    );
    Ok(())
}

#[test]
fn two_tenants_in_one_directory_continue_independently() -> TestResult {
    let dir = scratch_dir("tenants")?;
    let _guard = TempGuard(dir.clone());
    let mut live: Vec<PathBuf> = Vec::new();

    for tenant in 1_u64..=2 {
        let path = dir.join(format!("tenant-{tenant}.jrnl"));
        let action = tenant_action(tenant)?;
        let mut journal = FileJournal::open_continuing_with(&path, policy()?)?;
        let mut continuations = 0_u64;
        for attempt in 1..=90 {
            let events = tenant_ladder(action, attempt)?;
            journal.compare_and_append_all(&events)?;
            if journal.continuation_watermark()?.is_due() {
                journal = continue_once(&mut journal)?;
                continuations = continuations.saturating_add(1);
            }
        }
        assert!(
            continuations > 1,
            "tenant {tenant} continued {continuations} times, which is not enough \
             to say a chain of files works"
        );
        live.push(journal.path().to_path_buf());
        for attempt in journal.recover().attempts() {
            assert_eq!(
                attempt.key().action(),
                action,
                "tenant {tenant} recovered an attempt belonging to another tenant"
            );
        }
        // The ninety attempts are accounted for as *refused*, not as remembered:
        // the checkpoint folds them into one record per action, so what a caller
        // can see is small and what it cannot do is replay any of them.
        let mut replay = journal;
        for attempt in 1..=90 {
            let refused = replay.compare_and_append(
                replay.tail(),
                &EffectEvent::IntentAdmitted {
                    key: key_for_action(action, attempt)?,
                },
            );
            // Two typed answers are both refusals and both correct: an attempt the
            // live suffix or the folded key still holds is refused by the ladder,
            // and one only the folded record knows about is refused by the fold.
            // Neither admits it, which is the whole property.
            assert!(
                matches!(
                    refused,
                    Err(JournalError::AttemptAlreadyWalked { .. } | JournalError::OutOfOrder { .. })
                ),
                "tenant {tenant}'s attempt {attempt} was already walked, and a \
                 re-presentation of it must be refused: {refused:?}"
            );
        }
    }

    assert_ne!(
        live[0], live[1],
        "two tenants sharing one directory must not share a journal"
    );
    for (index, path) in live.iter().enumerate() {
        assert!(
            path.to_string_lossy().starts_with(
                &dir.join(format!("tenant-{}.jrnl", index + 1))
                    .display()
                    .to_string()
            ),
            "each tenant's live journal is its own file plus the shared successor \
             rule applied: {}",
            path.display()
        );
    }
    Ok(())
}

/// One tenant's three-rung ladder under its own action.
fn tenant_ladder(action: ActionId, attempt: u64) -> Result<Vec<EffectEvent>, Box<dyn Error>> {
    let key = key_for_action(action, attempt)?;
    Ok(vec![
        EffectEvent::IntentAdmitted { key },
        EffectEvent::DispatchPrepared { key },
        EffectEvent::OutcomeObserved {
            key,
            evidence: EffectEvidence::Applied,
        },
    ])
}

#[test]
fn a_reopen_follows_the_generation_chain_to_the_live_journal() -> TestResult {
    let dir = scratch_dir("walk")?;
    let _guard = TempGuard(dir.clone());
    let first = dir.join("run.jrnl");
    let mut journal = FileJournal::open_continuing_with(&first, policy()?)?;
    for round in 0..3_u64 {
        let from = round.saturating_mul(30).saturating_add(1);
        fill(&mut journal, from, 30)?;
        if journal.continuation_watermark()?.is_due() {
            journal = continue_once(&mut journal)?;
        }
    }
    let live = journal.path().to_path_buf();
    let generation = journal.generation();
    drop(journal);

    let reopened = FileJournal::open_active(&first)?;
    assert_eq!(
        reopened.path(),
        live.as_path(),
        "a caller holding only the original path reaches the live journal"
    );
    assert_eq!(reopened.generation(), generation);
    Ok(())
}

/// Every file in a chain is its own generation, and the checkpoint it was opened
/// from names that same generation.
///
/// The base is generation one, so its first successor, `000001`, is generation
/// two. Read from the digits alone it was generation one, and the checkpoint its
/// own successor carried claimed generation two a second time: two files in one
/// chain under one number.
#[test]
fn each_successor_is_the_next_generation_and_its_checkpoint_says_so() -> TestResult {
    let dir = scratch_dir("generations")?;
    let _guard = TempGuard(dir.clone());
    let mut journal = FileJournal::open_continuing_with(dir.join("run.jrnl"), policy()?)?;
    assert_eq!(
        journal.generation(),
        1,
        "the base journal is generation one"
    );
    assert!(journal.checkpoint().is_none(), "and carries no checkpoint");
    for expected in 2..=4_u64 {
        fill(&mut journal, expected.saturating_mul(100), 3)?;
        journal = continue_once(&mut journal)?;
        let carried = journal
            .checkpoint()
            .ok_or("a successor is opened from its predecessor's checkpoint")?
            .generation();
        assert_eq!(
            (journal.generation(), carried),
            (expected, expected),
            "the file and the checkpoint it was opened from name one generation"
        );
    }
    Ok(())
}

#[test]
fn a_sealed_predecessor_refuses_itself_and_names_its_successor() -> TestResult {
    let dir = scratch_dir("superseded")?;
    let _guard = TempGuard(dir.clone());
    let first = dir.join("run.jrnl");
    let mut journal = FileJournal::open_continuing_with(&first, policy()?)?;
    fill(&mut journal, 1, 20)?;
    let successor_path = continue_once(&mut journal)?.path().to_path_buf();
    drop(journal);

    match FileJournal::open(&first) {
        Err(JournalError::Superseded { path }) => {
            assert_eq!(path, successor_path.display().to_string());
        }
        other => {
            return Err(format!(
                "a sealed predecessor must refuse itself and name its successor, got {other:?}"
            )
            .into());
        }
    }
    Ok(())
}

#[test]
fn an_armed_continuation_refuses_at_the_boundary_it_reached() -> TestResult {
    for boundary in SealPause::all() {
        let dir = scratch_dir("pause")?;
        let _guard = TempGuard(dir.clone());
        let first = dir.join("run.jrnl");
        let mut journal = FileJournal::open_continuing_with(&first, policy()?)?;
        fill(&mut journal, 1, 20)?;
        journal.arm_continuation_pause(boundary);

        match journal.continue_as_file() {
            Err(JournalError::ContinuationPaused { boundary: at }) => assert_eq!(at, boundary),
            other => {
                return Err(format!(
                    "an armed continuation must stop at {boundary}, got {}",
                    describe(&other)
                )
                .into());
            }
        }
        let refused = append(
            &mut journal,
            &EffectEvent::IntentAdmitted {
                key: key_for_action(shared_action()?, 1)?,
            },
        );
        assert!(
            matches!(refused, Err(JournalError::Superseded { .. })),
            "a stopped seal leaves the handle read-only, which is what keeps a \
             process killed here from appending on either side of the boundary"
        );
    }
    Ok(())
}

/// The seal is one frame, written byte for byte into both files.
#[test]
fn a_seal_writes_exactly_its_own_frame_into_both_files() -> TestResult {
    let dir = scratch_dir("bytes")?;
    let _guard = TempGuard(dir.clone());
    let first = dir.join("run.jrnl");
    let sealed = dir.join("run.jrnl.cont").join("000001");
    let mut journal = FileJournal::open_continuing_with(&first, policy()?)?;
    fill(&mut journal, 1, 20)?;
    let before = std::fs::metadata(&first)?.len();
    assert!(
        !sealed.exists(),
        "the successor does not exist before the seal"
    );

    let successor = continue_once(&mut journal)?;
    drop(journal);
    let sealed_bytes = std::fs::read(&sealed)?;
    let first_bytes = std::fs::read(&first)?;

    assert_eq!(
        u64::try_from(first_bytes.len())?.saturating_sub(before),
        u64::try_from(sealed_bytes.len())?,
        "the predecessor grew by exactly what the successor's whole first frame is"
    );
    assert_eq!(
        &sealed_bytes[..],
        &first_bytes[usize::try_from(before)?..],
        "the seal frame's bytes are the same in both files, which is what lets the \
         successor's own chain start at the predecessor's tail"
    );
    assert_eq!(
        successor.committed()?.len(),
        0,
        "and the successor has admitted nothing of its own yet"
    );
    Ok(())
}

/// A continuation answer, rendered for a failure that cannot name its type.
fn describe(answer: &Result<Option<FileJournal>, JournalError>) -> String {
    match *answer {
        Ok(Some(ref journal)) => format!("a successor at {}", journal.path().display()),
        Ok(None) => "no successor".to_owned(),
        Err(ref error) => format!("{error}"),
    }
}

/// The shipped watermark is eight tenths of the shipped ceilings, and it leaves
/// room for an in-flight handoff to settle at either one.
#[test]
fn the_shipped_watermark_leaves_the_declared_headroom() -> TestResult {
    let declared = ContinuationPolicy::declared();
    assert_eq!(declared.events_at(), 80_000);
    assert!(
        u64::try_from(lgwks_bot::journal::MAX_JOURNAL_EVENTS)? > declared.events_at(),
        "the watermark is strictly inside the event ceiling, so an in-flight \
         handoff always has room to settle"
    );
    assert!(lgwks_bot::journal::MAX_JOURNAL_BYTES > declared.bytes_at());
    assert!(
        ContinuationPolicy::new(
            u64::try_from(lgwks_bot::journal::MAX_JOURNAL_EVENTS)?,
            declared.bytes_at()
        )
        .is_err(),
        "a trigger at the ceiling is refused rather than clamped to something unreachable"
    );
    assert!(Path::new("/tmp").is_absolute());
    let genesis = JournalPosition::genesis();
    assert_eq!(genesis.sequence(), 0);
    Ok(())
}

// ── A real SIGKILL at every seal boundary ───────────────────────────────────

/// Turns this binary into the seal-boundary probe child.
const SEAL_PROBE_ENV: &str = "LGWKS_SEAL_PROBE";
/// The directory the probe child's journal lives in.
const SEAL_PROBE_DIR: &str = "LGWKS_SEAL_PROBE_DIR";
/// Which of [`SealPause::all`] the probe child stops its continuation at.
const SEAL_PROBE_BOUNDARY: &str = "LGWKS_SEAL_PROBE_BOUNDARY";
/// The libtest name the probe child is started under.
const TEST_SEAL_KILL: &str =
    "a_kill_at_every_seal_boundary_leaves_exactly_one_authoritative_journal";
/// Settled attempts the probe child writes before it continues.
///
/// Well under the sixty-four-event trigger, so the only continuation in the run is
/// the armed one the kill lands inside.
const SEAL_PROBE_SETTLED: u64 = 9;
/// The attempt the probe child leaves unknown, so the kill is also a test of the
/// carry: an attempt with no outcome must be unknown in whichever file survives.
const SEAL_PROBE_UNKNOWN: u64 = SEAL_PROBE_SETTLED + 1;

/// The file the probe child writes once its continuation has stopped.
fn seal_marker(dir: &Path) -> PathBuf {
    dir.join("stopped")
}

/// The probe child: settle some attempts, leave one unknown, stop the seal at the
/// ordered boundary, announce it, and park until killed.
///
/// The stop is the shipped fault door, so the bytes on the disk at the kill are
/// exactly the ones a process that died at that instruction would leave: the
/// handle refuses every later write, and the kill runs no destructor that could
/// write one anyway.
fn seal_probe_body() -> TestResult {
    let dir = PathBuf::from(
        std::env::var_os(SEAL_PROBE_DIR).ok_or("the seal probe was started without a directory")?,
    );
    let index: usize = std::env::var(SEAL_PROBE_BOUNDARY)?.parse()?;
    let boundary = *SealPause::all()
        .get(index)
        .ok_or("the seal probe was given a boundary past the declared set")?;
    let mut journal = FileJournal::open_continuing_with(dir.join("run.jrnl"), policy()?)?;
    fill(&mut journal, 1, SEAL_PROBE_SETTLED)?;
    let unknown = key_for_action(shared_action()?, SEAL_PROBE_UNKNOWN)?;
    crate::journal_fixtures::walk_ladder(&mut journal, unknown, PREDICATE, 1, 2)?;
    journal.arm_continuation_pause(boundary);
    match journal.continue_as_file() {
        Err(JournalError::ContinuationPaused { boundary: at }) if at == boundary => {}
        other => {
            let refusal: TestResult =
                Err(format!("the seal did not stop at {boundary}: {}", describe(&other)).into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "seal_probe_body: the armed seal ran past its boundary");
            return refusal;
        }
    }
    let mut marker = std::fs::File::create(seal_marker(&dir))?;
    std::io::Write::write_all(&mut marker, b"stopped")?;
    marker.sync_all()?;
    for _ in 0..600 {
        crate::journal_fixtures::pause(100);
    }
    Err("the seal probe parked for its whole bound and was never killed".into())
}

/// A continuation killed by `SIGKILL` at each of its write and sync boundaries
/// leaves exactly one authoritative journal, holding exactly what was written.
///
/// For every boundary the child stops at, a reopen through the original path must:
///
/// - find one live journal, and the other file must refuse to be appended to:
///   either the predecessor is sealed and names its successor, or the successor
///   never became a journal;
/// - hold every settled attempt as settled and refuse each one a second time,
///   which is "nothing lost and nothing duplicated" at the journal's boundary;
/// - hold the attempt the child left unknown as unknown, wherever it survived;
/// - and continue again on demand, so a crash mid-continuation is a delay rather
///   than a journal that can never continue.
#[test]
fn a_kill_at_every_seal_boundary_leaves_exactly_one_authoritative_journal() -> TestResult {
    if std::env::var_os(SEAL_PROBE_ENV).is_some() {
        return seal_probe_body();
    }
    for (index, boundary) in SealPause::all().into_iter().enumerate() {
        let dir = scratch_dir("seal-kill")?;
        let _guard = TempGuard(dir.clone());
        let mut command = crate::probe_command(&crate::probe_test(module_path!(), TEST_SEAL_KILL))?;
        command
            .env(SEAL_PROBE_ENV, "1")
            .env(SEAL_PROBE_DIR, &dir)
            .env(SEAL_PROBE_BOUNDARY, index.to_string());
        let mut child = crate::journal_fixtures::ProbeGuard(Some(command.spawn()?));
        child.kill_after_marker(&seal_marker(&dir), TEST_SEAL_KILL)?;
        reopen_after_seal_kill(&dir, boundary)?;
    }
    Ok(())
}

/// Every assertion a reopen after a kill at `boundary` must satisfy.
fn reopen_after_seal_kill(dir: &Path, boundary: SealPause) -> TestResult {
    let base = dir.join("run.jrnl");
    let successor = dir.join("run.jrnl.cont").join("000001");
    let mut live = FileJournal::open_active(&base)?;
    let other = if live.path() == base.as_path() {
        &successor
    } else {
        &base
    };
    if other.exists() {
        let foreign = EffectEvent::IntentAdmitted {
            key: key_for_action(tenant_action(77)?, 1)?,
        };
        let refused =
            FileJournal::open(other).and_then(|mut journal| append(&mut journal, &foreign));
        assert!(
            refused.is_err(),
            "after a kill at {boundary}, {} is live and {} still took an append: two \
             journals are authoritative",
            live.path().display(),
            other.display()
        );
    }

    let latest = key_for_action(shared_action()?, SEAL_PROBE_SETTLED)?;
    let unknown = key_for_action(shared_action()?, SEAL_PROBE_UNKNOWN)?;
    let recovered = live.recover();
    assert_eq!(
        recovered.status(unknown),
        Some(AttemptStatus::OutcomeUnknown),
        "after a kill at {boundary}, the attempt left unknown is still unknown in {}",
        live.path().display()
    );
    assert_eq!(
        recovered.status(latest),
        Some(AttemptStatus::Verified),
        "after a kill at {boundary}, the last settled attempt is settled in {}",
        live.path().display()
    );
    for attempt in 1..=SEAL_PROBE_SETTLED {
        let again = EffectEvent::IntentAdmitted {
            key: key_for_action(shared_action()?, attempt)?,
        };
        assert!(
            append(&mut live, &again).is_err(),
            "after a kill at {boundary}, settled attempt {attempt} was admitted a second time"
        );
    }

    if live.generation() == 1 {
        let continued = continue_once(&mut live)?;
        assert_eq!(
            continued.recover().status(unknown),
            Some(AttemptStatus::OutcomeUnknown),
            "a continuation completed after a kill at {boundary} still carries the unknown"
        );
    }
    Ok(())
}
