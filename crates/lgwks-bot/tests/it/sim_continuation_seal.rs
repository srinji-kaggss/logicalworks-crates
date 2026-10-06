//! Simulation family: the mechanism a continuation rests on, one seeded property
//! per test.
//!
//! `sim_continuation` drives the *lifecycle* — long runs, carries, tenants and a
//! reopen against an oracle. This layer pins the *mechanism* underneath it, each
//! property over its own band so a failure names the property rather than a run:
//! where the watermark falls, what a successor is called and chains from, what the
//! seal writes, what a sealed or half-sealed chain refuses, what a checkpoint holds
//! at its two bounds, and what a reopen reads back. `journal_continuation.rs` states
//! each of these once, at one hand-picked shape; here the seed picks the shape and
//! the property has to hold for every one it picks.
//!
//! Every property drives the real [`FileJournal`] on a real directory through its
//! public doors, and every band is swept twice through [`sim::assert_replays`], so a
//! run whose trace depends on anything but its seed fails even when every assertion
//! passes.

use crate::sim;
use crate::sim_continuation::{
    Fate, Oracle, Pending, TestResult, action_of, assert_fold_agrees, continue_if_due, key_of,
    ladder, status_of, verdict,
};

use std::collections::BTreeSet;
use std::error::Error;
use std::path::Path;

use lgwks_bot::journal::{
    AttemptStatus, ContinuationPolicy, EffectEvent, EffectEvidence, EffectJournal, FileJournal,
    JournalError, JournalLimitKind, MAX_CHECKPOINT_SETTLED, MAX_CHECKPOINT_UNRESOLVED, SealPause,
    verify_chain_from,
};

use sim::Band;

/// Seeds each property sweeps, twice each.
///
/// Every property costs a few dozen flushes per seed, and what it pins is a
/// mechanism whose shape the seed varies inside one draw: three draws cover the
/// shapes without making the family a measurement of the device's flush rate.
const SEEDS: u64 = 3;

/// The first seed of this layer, clear of every other family's bands.
const FIRST_SEED: u64 = 5_000;

/// The byte trigger every family that is not about bytes opens with.
///
/// Far below the frame ceiling and far above anything these families write, so
/// only the event trigger or an explicit seal ever continues them.
const QUIET_BYTES: u64 = 8 * 1024 * 1024;

/// The band of property `index`, so no two properties share a seed.
const fn band(index: u64) -> Band {
    Band::new(
        FIRST_SEED.saturating_add(index.saturating_mul(SEEDS)),
        SEEDS,
    )
}

/// A continuing journal at `path`, triggered at `events_at` events or `bytes_at`
/// bytes, whichever comes first.
fn continuing(path: &Path, events_at: u64, bytes_at: u64) -> Result<FileJournal, Box<dyn Error>> {
    let policy = ContinuationPolicy::new(events_at, bytes_at)?;
    Ok(FileJournal::open_continuing_with(path, policy)?)
}

/// A continuing journal whose own trigger never falls inside a family, so every
/// continuation is one the family asked for.
fn quiet(path: &Path) -> Result<FileJournal, Box<dyn Error>> {
    Ok(FileJournal::open_continuing_with(
        path,
        ContinuationPolicy::declared(),
    )?)
}

/// Seal `journal` and hand back its successor.
fn seal(journal: &mut FileJournal) -> Result<FileJournal, Box<dyn Error>> {
    Ok(journal
        .continue_as_file()?
        .ok_or("a continuing journal must hand back a successor")?)
}

/// Write `count` attempts of `action`, numbered from `first`, each walked to the
/// fate `next` names for it.
fn write_with(
    journal: &mut FileJournal,
    oracle: &mut Oracle,
    action: u64,
    (first, count): (u64, u64),
    mut next: impl FnMut() -> Fate,
) -> TestResult {
    let mut pending = Pending::new(action);
    let last = first.saturating_add(count).saturating_sub(1);
    for number in first..=last {
        pending.push(journal, oracle, number, next(), number == last)?;
    }
    Ok(())
}

/// Write `count` attempts of `action` from `first`, each walked to `fate`.
fn write(
    journal: &mut FileJournal,
    oracle: &mut Oracle,
    action: u64,
    attempts: (u64, u64),
    fate: Fate,
) -> TestResult {
    write_with(journal, oracle, action, attempts, || fate)
}

/// Write `count` attempts of `action` from `first`, each to a fate the seed draws.
fn write_drawn(
    sim: &mut sim::Sim,
    journal: &mut FileJournal,
    oracle: &mut Oracle,
    action: u64,
    attempts: (u64, u64),
) -> TestResult {
    write_with(journal, oracle, action, attempts, || drawn(sim))
}

/// A fate the seed draws, unknown one time in eight.
///
/// Rare enough that no family's carry approaches [`MAX_CHECKPOINT_UNRESOLVED`]
/// unless the family is about that bound.
fn drawn(sim: &mut sim::Sim) -> Fate {
    match sim.rng().below(8) {
        0 => Fate::Unknown,
        1 | 2 => Fate::NotApplied,
        3 => Fate::VerificationFailed,
        _ => Fate::Applied,
    }
}

/// A settled fate the seed draws.
fn settled(sim: &mut sim::Sim) -> Fate {
    match sim.rng().below(3) {
        0 => Fate::NotApplied,
        1 => Fate::VerificationFailed,
        _ => Fate::Applied,
    }
}

/// A fresh intent for `attempt` of `action`.
fn intent(action: u64, attempt: u64) -> Result<EffectEvent, Box<dyn Error>> {
    Ok(EffectEvent::IntentAdmitted {
        key: key_of(action, attempt)?,
    })
}

/// The file's length on the disk.
fn length(path: &Path) -> Result<u64, Box<dyn Error>> {
    Ok(std::fs::metadata(path)?.len())
}

/// A draw in `low..low + span`.
fn draw(sim: &mut sim::Sim, low: u64, span: u32) -> u64 {
    low.saturating_add(u64::from(sim.rng().below(span)))
}

/// A refused continuation moved no byte of `journal` and wrote no successor.
fn assert_unwritten(journal: &FileJournal, before: u64) -> TestResult {
    assert_eq!(
        length(journal.path())?,
        before,
        "refused before a byte moved"
    );
    assert!(
        !journal.successor_path().exists(),
        "and no successor exists"
    );
    Ok(())
}

// ── The watermark ─────────────────────────────────────────────────────────

/// The event trigger falls on the flush that reaches it, and never before.
///
/// The watermark is checked against this family's own count of what it wrote and
/// against the file's length on the disk, so the trigger is a fact about the file
/// rather than a number the journal agrees with itself about.
fn event_watermark(sim: &mut sim::Sim) -> TestResult {
    let dir = sim.scratch("events")?;
    let path = dir.join("run.jrnl");
    let events_at = draw(sim, 24, 100);
    let mut journal = continuing(&path, events_at, QUIET_BYTES)?;
    let mut appended = 0_u64;
    let mut attempt = 0_u64;
    loop {
        let watermark = journal.continuation_watermark()?;
        assert_eq!(
            watermark.events().0,
            appended,
            "the watermark counts exactly the events this file committed"
        );
        assert_eq!(
            watermark.bytes().0,
            length(&path)?,
            "and exactly the bytes on the disk"
        );
        assert_eq!(
            watermark.is_due(),
            appended >= events_at,
            "due at {appended} events of a {events_at}-event trigger"
        );
        if watermark.is_due() {
            break;
        }
        attempt = attempt.saturating_add(1);
        let rungs = ladder(Fate::Applied, 1, attempt)?;
        journal.compare_and_append_all(&rungs)?;
        appended = appended.saturating_add(u64::try_from(rungs.len())?);
    }
    sim.record("event-watermark-falls-on-its-flush");
    sim.trace.record_number("events-at", events_at);
    sim.trace.record_number("events-appended", appended);
    Ok(())
}

/// The byte trigger alone makes a continuation due, with the event trigger unmet.
fn byte_watermark(sim: &mut sim::Sim) -> TestResult {
    let dir = sim.scratch("bytes")?;
    let path = dir.join("run.jrnl");
    let bytes_at = draw(sim, 2_048, 6_144);
    let events_at = ContinuationPolicy::declared().events_at();
    let mut journal = continuing(&path, events_at, bytes_at)?;
    let mut attempt = 0_u64;
    loop {
        let watermark = journal.continuation_watermark()?;
        let on_disk = length(&path)?;
        assert_eq!(
            watermark.is_due(),
            on_disk >= bytes_at,
            "due at {on_disk} bytes of a {bytes_at}-byte trigger"
        );
        assert!(
            watermark.events().0 < events_at,
            "the event trigger never fell, so the byte trigger is the one being tested"
        );
        if watermark.is_due() {
            break;
        }
        attempt = attempt.saturating_add(1);
        journal.compare_and_append_all(&ladder(Fate::Applied, 1, attempt)?)?;
    }
    let successor = seal(&mut journal)?;
    assert_eq!(
        successor.generation(),
        2,
        "a byte-triggered continuation is an ordinary one"
    );
    sim.record("byte-watermark-alone-continues");
    sim.trace.record_number("bytes-at", bytes_at);
    sim.trace.record_number("bytes-attempts", attempt);
    Ok(())
}

/// A journal opened without a lifecycle is never due and never continues.
fn never_due(sim: &mut sim::Sim) -> TestResult {
    let dir = sim.scratch("plain")?;
    let path = dir.join("run.jrnl");
    let attempts = draw(sim, 4, 28);
    let mut journal = FileJournal::open(&path)?;
    let mut oracle = Oracle::default();
    write_drawn(sim, &mut journal, &mut oracle, 1, (1, attempts))?;
    let watermark = journal.continuation_watermark()?;
    assert!(
        !watermark.continues() && !watermark.is_due(),
        "a journal that never chose a lifecycle reports none"
    );
    assert!(
        journal.continue_as_file()?.is_none(),
        "and asking it to continue is a no rather than a seal"
    );
    assert!(
        !journal.successor_path().exists(),
        "so no successor was written"
    );
    assert_eq!(journal.generation(), 1, "and it stays the first generation");
    assert_fold_agrees(&journal, &oracle, 1)?;
    sim.record("plain-journal-never-due");
    sim.trace.record_number("plain-attempts", attempts);
    Ok(())
}

/// The headroom falls by exactly what each flush appended, events and bytes both.
fn headroom(sim: &mut sim::Sim) -> TestResult {
    let dir = sim.scratch("headroom")?;
    let path = dir.join("run.jrnl");
    let mut journal = quiet(&path)?;
    let mut next = 1_u64;
    let flushes = draw(sim, 2, 6);
    for _ in 0..flushes {
        let before = journal.continuation_watermark()?;
        let length_before = length(&path)?;
        let count = draw(sim, 1, 12);
        let mut events = Vec::new();
        for number in next..next.saturating_add(count) {
            events.extend(ladder(drawn(sim), 1, number)?);
        }
        journal.compare_and_append_all(&events)?;
        next = next.saturating_add(count);
        let after = journal.continuation_watermark()?;
        assert_eq!(
            before
                .event_headroom()
                .saturating_sub(after.event_headroom()),
            u64::try_from(events.len())?,
            "the event headroom fell by the events this flush appended"
        );
        assert_eq!(
            before.byte_headroom().saturating_sub(after.byte_headroom()),
            length(&path)?.saturating_sub(length_before),
            "and the byte headroom by what the file grew"
        );
    }
    sim.record("headroom-falls-by-what-was-appended");
    sim.trace.record_number("headroom-attempts", next);
    Ok(())
}

// ── The chain ─────────────────────────────────────────────────────────────

/// Every successor is the next fixed-width name in its base's own directory, and
/// the file and its checkpoint name one generation.
fn generation_names(sim: &mut sim::Sim) -> TestResult {
    let dir = sim.scratch("names")?;
    let base = dir.join("run.jrnl");
    let continuations = draw(sim, 1, 5);
    let mut journal = quiet(&base)?;
    let mut oracle = Oracle::default();
    let mut next = 1_u64;
    for generation in 2..=continuations.saturating_add(1) {
        let count = draw(sim, 1, 6);
        write(&mut journal, &mut oracle, 1, (next, count), Fate::Applied)?;
        next = next.saturating_add(count);
        let predicted = journal.successor_path();
        journal = seal(&mut journal)?;
        let expected = dir
            .join("run.jrnl.cont")
            .join(format!("{:06}", generation.saturating_sub(1)));
        assert_eq!(
            journal.path(),
            predicted.as_path(),
            "the successor is where its predecessor said it would be"
        );
        assert_eq!(
            journal.path(),
            expected.as_path(),
            "a fixed-width number in the base's own `.cont` directory"
        );
        let carried = journal
            .checkpoint()
            .ok_or("a successor is opened from a checkpoint")?
            .generation();
        assert_eq!(
            (journal.generation(), carried),
            (generation, generation),
            "the file and the checkpoint it was opened from name one generation"
        );
    }
    assert_fold_agrees(&journal, &oracle, 1)?;
    sim.record("each-successor-is-the-next-name");
    sim.trace
        .record_number("names-continuations", continuations);
    sim.trace.record_number("names-attempts", next);
    Ok(())
}

/// A successor's checkpoint names the tail it sealed, and its own chain verifies
/// from there.
fn chain_base(sim: &mut sim::Sim) -> TestResult {
    let dir = sim.scratch("base")?;
    let path = dir.join("run.jrnl");
    let attempts = draw(sim, 2, 30);
    let mut journal = quiet(&path)?;
    let mut oracle = Oracle::default();
    write_drawn(sim, &mut journal, &mut oracle, 1, (1, attempts))?;
    let sealed_at = journal.tail();
    let mut successor = seal(&mut journal)?;
    let predecessor = successor
        .checkpoint()
        .ok_or("a successor is opened from a checkpoint")?
        .predecessor();
    assert_eq!(
        predecessor, sealed_at,
        "the checkpoint names the tail it sealed"
    );
    assert_eq!(
        successor.base(),
        sealed_at,
        "and the successor's chain starts there"
    );
    assert!(
        successor.committed()?.is_empty(),
        "a successor has admitted nothing of its own at birth"
    );
    let more = draw(sim, 1, 12);
    write_drawn(
        sim,
        &mut successor,
        &mut oracle,
        1,
        (attempts.saturating_add(1), more),
    )?;
    let entries = successor.committed_entries()?;
    assert_eq!(
        verify_chain_from(successor.chain_from(), &entries)?,
        successor.tail(),
        "the successor's own chain verifies from its own base to its tail"
    );
    assert_fold_agrees(&successor, &oracle, 1)?;
    sim.record("successor-chains-from-the-sealed-tail");
    sim.trace.record_number("base-attempts", attempts);
    sim.trace.record_number("base-more", more);
    Ok(())
}

/// A reopen through the base path reaches the live generation, at its tail, with
/// the fold the oracle recorded — after any number of continuations, none included.
fn reopen_reaches_live(sim: &mut sim::Sim) -> TestResult {
    let dir = sim.scratch("walk")?;
    let base = dir.join("run.jrnl");
    let continuations = u64::from(sim.rng().below(6));
    let mut journal = quiet(&base)?;
    let mut oracle = Oracle::default();
    let mut next = 1_u64;
    for _ in 0..continuations {
        let count = draw(sim, 1, 10);
        write_drawn(sim, &mut journal, &mut oracle, 1, (next, count))?;
        next = next.saturating_add(count);
        journal = seal(&mut journal)?;
    }
    let trailing = u64::from(sim.rng().below(10));
    write_drawn(sim, &mut journal, &mut oracle, 1, (next, trailing))?;
    let live = journal.path().to_path_buf();
    let generation = journal.generation();
    let tail = journal.tail();
    drop(journal);

    let reopened = FileJournal::open_active(&base)?;
    assert_eq!(
        reopened.path(),
        live.as_path(),
        "the base path reaches the live file"
    );
    assert_eq!(
        reopened.generation(),
        generation,
        "of the generation the run left"
    );
    assert_eq!(reopened.tail(), tail, "at the tail the run left");
    assert_fold_agrees(&reopened, &oracle, 1)?;
    sim.record("base-path-reaches-the-live-generation");
    sim.trace.record_number("walk-continuations", continuations);
    sim.trace.record_number("walk-trailing", trailing);
    Ok(())
}

/// A base named like a generation is refused before the file exists, and the same
/// name inside a chain's own directory is that chain's generation.
fn ambiguous_base(sim: &mut sim::Sim) -> TestResult {
    let dir = sim.scratch("named")?;
    let number = draw(sim, 1, 999_998);
    let name = format!("{number:06}");
    let path = dir.join(&name);
    let refused = FileJournal::open_continuing_with(&path, ContinuationPolicy::declared());
    assert!(
        matches!(
            refused.as_ref().err(),
            Some(&JournalError::CapacityExceeded {
                resource: JournalLimitKind::Generation,
                requested,
                ..
            }) if requested == number
        ),
        "a base named {name} could be read as a generation and must be refused naming \
         it: {:?}",
        refused.as_ref().err()
    );
    assert!(!path.exists(), "refused before the file was created");

    let chain = dir.join("run.jrnl.cont");
    std::fs::create_dir_all(&chain)?;
    let generation = FileJournal::open(chain.join(&name))?;
    assert_eq!(
        generation.generation(),
        number.saturating_add(1),
        "inside a chain's directory the same name is that chain's generation"
    );
    sim.record("generation-shaped-base-refused");
    sim.trace.record_number("named", number);
    Ok(())
}

// ── The seal ──────────────────────────────────────────────────────────────

/// The seal is one frame, byte-identical in both files.
fn seal_bytes(sim: &mut sim::Sim) -> TestResult {
    let dir = sim.scratch("frame")?;
    let base = dir.join("run.jrnl");
    let sealed = dir.join("run.jrnl.cont").join("000001");
    let attempts = draw(sim, 1, 40);
    let mut journal = quiet(&base)?;
    let mut oracle = Oracle::default();
    write_drawn(sim, &mut journal, &mut oracle, 1, (1, attempts))?;
    let before = length(&base)?;
    assert!(!sealed.exists(), "no successor exists before the seal");

    let successor = seal(&mut journal)?;
    let successor_bytes = std::fs::read(&sealed)?;
    let grown = std::fs::read(&base)?.split_off(usize::try_from(before)?);
    assert_eq!(
        grown, successor_bytes,
        "the predecessor grew by exactly the successor's whole first frame, byte for byte"
    );
    assert_fold_agrees(&successor, &oracle, 1)?;
    sim.record("seal-frame-identical-in-both-files");
    sim.trace.record_number("frame-attempts", attempts);
    sim.trace
        .record_number("frame-bytes", successor_bytes.len());
    Ok(())
}

/// A sealed predecessor refuses every append, every second seal and every open,
/// each naming its successor, and keeps the bytes the seal left.
fn sealed_refuses(sim: &mut sim::Sim) -> TestResult {
    let dir = sim.scratch("sealed")?;
    let base = dir.join("run.jrnl");
    let attempts = draw(sim, 1, 30);
    let mut journal = quiet(&base)?;
    let mut oracle = Oracle::default();
    write_drawn(sim, &mut journal, &mut oracle, 1, (1, attempts))?;
    let successor = seal(&mut journal)?;
    let next = successor.path().to_path_buf();
    let named = next.display().to_string();
    let sealed_length = length(&base)?;
    assert_eq!(
        journal.sealed_by(),
        Some(next.as_path()),
        "the sealed handle names where authority went"
    );

    let fresh = attempts.saturating_add(draw(sim, 1, 5));
    let refused = journal.compare_and_append(journal.tail(), &intent(1, fresh)?);
    assert!(
        matches!(&refused, Err(JournalError::Superseded { path }) if *path == named),
        "an append to the sealed handle names the successor: {refused:?}"
    );
    assert!(
        matches!(
            journal.continue_as_file(),
            Err(JournalError::Superseded { .. })
        ),
        "a sealed handle cannot seal a second time"
    );
    drop(journal);
    let reopened = FileJournal::open(&base);
    assert!(
        matches!(reopened.as_ref().err(), Some(JournalError::Superseded { path }) if *path == named),
        "opening the sealed file names its successor: {:?}",
        reopened.as_ref().err()
    );
    assert_eq!(
        length(&base)?,
        sealed_length,
        "every refusal left the sealed file's bytes where the seal put them"
    );
    assert_fold_agrees(&successor, &oracle, 1)?;
    sim.record("sealed-predecessor-refuses");
    sim.trace.record_number("sealed-attempts", attempts);
    Ok(())
}

/// A seal stopped at any boundary leaves exactly one file that takes appends, and
/// that file still holds what the oracle recorded, the unknown attempt included.
fn stopped_seal(sim: &mut sim::Sim) -> TestResult {
    let dir = sim.scratch("stopped")?;
    let base = dir.join("run.jrnl");
    let successor = dir.join("run.jrnl.cont").join("000001");
    let boundaries = SealPause::all();
    let boundary = boundaries[usize::try_from(sim.rng().below(4))?];
    let attempts = draw(sim, 2, 20);
    let unknown = attempts.saturating_add(1);
    let mut journal = quiet(&base)?;
    let mut oracle = Oracle::default();
    write_drawn(sim, &mut journal, &mut oracle, 1, (1, attempts))?;
    write(&mut journal, &mut oracle, 1, (unknown, 1), Fate::Unknown)?;

    journal.arm_continuation_pause(boundary);
    let stopped = journal.continue_as_file();
    assert!(
        matches!(stopped, Err(JournalError::ContinuationPaused { boundary: at }) if at == boundary),
        "an armed seal stops at {boundary}"
    );
    let after = journal.compare_and_append(journal.tail(), &intent(1, unknown.saturating_add(1))?);
    assert!(
        matches!(after, Err(JournalError::Superseded { .. })),
        "a stopped seal leaves its handle read-only: {after:?}"
    );
    drop(journal);

    let mut live = FileJournal::open_active(&base)?;
    let other = if live.path() == base.as_path() {
        successor
    } else {
        base
    };
    if other.exists() {
        let foreign = intent(77, 1)?;
        let refused = FileJournal::open(&other)
            .and_then(|mut file| file.compare_and_append(file.tail(), &foreign));
        assert!(
            refused.is_err(),
            "after a seal stopped at {boundary}, {} is live and {} still took an append",
            live.path().display(),
            other.display()
        );
    }
    assert_eq!(
        live.recover().status(key_of(1, unknown)?),
        Some(AttemptStatus::OutcomeUnknown),
        "the attempt left unknown is unknown in whichever file is live"
    );
    assert_fold_agrees(&live, &oracle, 1)?;
    for number in 1..=unknown {
        let again = live.compare_and_append(live.tail(), &intent(1, number)?);
        assert!(
            again.is_err(),
            "attempt {number} was admitted a second time after a seal stopped at {boundary}"
        );
    }
    live.compare_and_append_all(&ladder(Fate::Applied, 1, unknown.saturating_add(1))?)?;
    sim.record(&format!("stopped-at-{boundary}"));
    sim.trace
        .record_number("stopped-generation", live.generation());
    sim.trace.record_number("stopped-attempts", attempts);
    Ok(())
}

// ── The checkpoint ────────────────────────────────────────────────────────

/// A carry at the declared bound continues with every attempt still unknown, and
/// one more is refused before a byte moves.
fn carry_bound(sim: &mut sim::Sim) -> TestResult {
    let dir = sim.scratch("carry-bound")?;
    let actions = draw(sim, 1, 4);
    for carried in [
        MAX_CHECKPOINT_UNRESOLVED,
        MAX_CHECKPOINT_UNRESOLVED.saturating_add(1),
    ] {
        let mut journal = quiet(&dir.join(format!("carry-{carried}.jrnl")))?;
        let mut oracle = Oracle::default();
        let total = u64::try_from(carried)?;
        let share = total
            .checked_div(actions)
            .ok_or("the seed draws at least one action")?;
        let mut left = total;
        for action in 1..=actions {
            let count = if action == actions { left } else { share };
            write(&mut journal, &mut oracle, action, (1, count), Fate::Unknown)?;
            left = left.saturating_sub(count);
        }
        let before = length(journal.path())?;
        let answer = journal.continue_as_file();
        if carried <= MAX_CHECKPOINT_UNRESOLVED {
            let successor = answer?.ok_or("a carry at the bound continues")?;
            assert_eq!(
                successor.recover().uncertain().len(),
                carried,
                "every attempt at the bound is carried, still unknown"
            );
            assert_fold_agrees(&successor, &oracle, 1)?;
        } else {
            assert!(
                matches!(
                    answer,
                    Err(JournalError::CapacityExceeded {
                        resource: JournalLimitKind::CheckpointUnresolved,
                        ..
                    })
                ),
                "a carry past the bound is refused, never truncated"
            );
            assert_unwritten(&journal, before)?;
        }
    }
    sim.record("carry-bound-exact");
    sim.trace.record_number("carry-bound-actions", actions);
    Ok(())
}

/// A checkpoint folding the declared number of actions continues, and one more
/// action is refused before a byte moves.
fn action_bound(sim: &mut sim::Sim) -> TestResult {
    let dir = sim.scratch("action-bound")?;
    for folded in [
        MAX_CHECKPOINT_SETTLED,
        MAX_CHECKPOINT_SETTLED.saturating_add(1),
    ] {
        let mut journal = quiet(&dir.join(format!("actions-{folded}.jrnl")))?;
        let mut oracle = Oracle::default();
        let actions = u64::try_from(folded)?;
        let mut batch = Vec::new();
        for action in 1..=actions {
            let fate = settled(sim);
            batch.extend(ladder(fate, action, 1)?);
            oracle.dispatch(action, 1, fate);
            if batch.len() >= 64 || action == actions {
                journal.compare_and_append_all(&batch)?;
                batch.clear();
            }
        }
        let before = length(journal.path())?;
        let answer = journal.continue_as_file();
        if folded <= MAX_CHECKPOINT_SETTLED {
            let successor = answer?.ok_or("a fold at the bound continues")?;
            let checkpoint = successor
                .checkpoint()
                .ok_or("a successor is opened from a checkpoint")?;
            assert_eq!(checkpoint.settled().len(), folded, "one record per action");
            assert_fold_agrees(&successor, &oracle, 1)?;
        } else {
            assert!(
                matches!(
                    answer,
                    Err(JournalError::CapacityExceeded {
                        resource: JournalLimitKind::CheckpointActions,
                        ..
                    })
                ),
                "a fold past the bound is refused, never truncated"
            );
            assert_unwritten(&journal, before)?;
        }
    }
    sim.record("action-bound-exact");
    Ok(())
}

/// A successor, the oracle that wrote its history, and the latest attempt number
/// of every action it folded.
type Folded = (FileJournal, Oracle, Vec<(u64, u64)>);

/// A successor over a seeded history of settled attempts.
fn folded(sim: &mut sim::Sim, tag: &str) -> Result<Folded, Box<dyn Error>> {
    let dir = sim.scratch(tag)?;
    let mut journal = quiet(&dir.join("run.jrnl"))?;
    let mut oracle = Oracle::default();
    let mut latest = Vec::new();
    let actions = draw(sim, 1, 6);
    for action in 1..=actions {
        let attempts = draw(sim, 1, 6);
        write_with(&mut journal, &mut oracle, action, (1, attempts), || {
            settled(sim)
        })?;
        latest.push((action, attempts));
    }
    let successor = seal(&mut journal)?;
    Ok((successor, oracle, latest))
}

/// The checkpoint holds one record per action, naming the latest attempt and the
/// status the oracle recorded for it, whatever the history behind it.
fn one_record_per_action(sim: &mut sim::Sim) -> TestResult {
    let (successor, oracle, latest) = folded(sim, "per-action")?;
    let checkpoint = successor
        .checkpoint()
        .ok_or("a successor is opened from a checkpoint")?;
    assert_eq!(
        checkpoint.settled().len(),
        latest.len(),
        "one folded record per action"
    );
    assert!(
        checkpoint.unresolved().is_empty(),
        "a fully settled history carries nothing unresolved"
    );
    for &(action, last) in &latest {
        let held = checkpoint
            .settled_for(action_of(action)?)
            .ok_or("every action the predecessor walked is folded")?;
        let recorded = oracle
            .latest_of(action)
            .ok_or("the oracle holds every action it dispatched")?;
        assert_eq!(
            held.attempt().get(),
            last,
            "the fold keeps the latest attempt"
        );
        assert_eq!(
            held.status(),
            Some(status_of(recorded.fate)),
            "with the status the oracle recorded for it"
        );
    }
    sim.record("one-record-per-action");
    sim.trace.record_number("per-action-actions", latest.len());
    Ok(())
}

/// Every attempt a folded action already walked is refused after a continuation,
/// and the refusals move no byte.
fn older_refused(sim: &mut sim::Sim) -> TestResult {
    let (mut successor, oracle, latest) = folded(sim, "older")?;
    let before = length(successor.path())?;
    for &(action, last) in &latest {
        for number in 1..=last {
            let again = successor.compare_and_append(successor.tail(), &intent(action, number)?);
            assert!(
                again.is_err(),
                "attempt {number} of action {action} was admitted again after a continuation"
            );
        }
    }
    assert_eq!(
        length(successor.path())?,
        before,
        "every refusal left the successor's bytes alone"
    );
    assert_fold_agrees(&successor, &oracle, 1)?;
    sim.record("walked-attempts-refused");
    sim.trace.record_number("older-actions", latest.len());
    Ok(())
}

/// A newer attempt of every folded action is admitted in the successor and folds
/// to the status its ladder reached.
fn newer_admitted(sim: &mut sim::Sim) -> TestResult {
    let (mut successor, mut oracle, latest) = folded(sim, "newer")?;
    for &(action, last) in &latest {
        let next = last.saturating_add(draw(sim, 1, 3));
        let fate = drawn(sim);
        successor.compare_and_append_all(&ladder(fate, action, next)?)?;
        oracle.dispatch(action, next, fate);
        assert_eq!(
            successor.recover().status(key_of(action, next)?),
            Some(status_of(fate)),
            "attempt {next} of action {action} is admitted and folds to its fate"
        );
    }
    assert_fold_agrees(&successor, &oracle, 1)?;
    sim.record("newer-attempts-admitted");
    sim.trace.record_number("newer-actions", latest.len());
    Ok(())
}

/// A failed verification's digest crosses every continuation unchanged.
fn verification_crosses(sim: &mut sim::Sim) -> TestResult {
    let dir = sim.scratch("verdict")?;
    let mut journal = quiet(&dir.join("run.jrnl"))?;
    let mut oracle = Oracle::default();
    write(
        &mut journal,
        &mut oracle,
        1,
        (1, 1),
        Fate::VerificationFailed,
    )?;
    let expected = verdict(2, false)?;
    let hops = draw(sim, 1, 5);
    let mut next = 1_u64;
    for _ in 0..hops {
        let filler = draw(sim, 1, 4);
        write(&mut journal, &mut oracle, 2, (next, filler), Fate::Applied)?;
        next = next.saturating_add(filler);
        journal = seal(&mut journal)?;
        let held = journal
            .checkpoint()
            .ok_or("a successor is opened from a checkpoint")?
            .settled_for(action_of(1)?)
            .ok_or("the verified action is folded")?;
        assert_eq!(
            held.verification(),
            Some(expected),
            "the failed verification is carried byte for byte"
        );
        assert_eq!(
            journal.recover().status(key_of(1, 1)?),
            Some(AttemptStatus::VerificationFailed),
            "and the attempt it judged still folds as a failed verification"
        );
    }
    assert_fold_agrees(&journal, &oracle, 1)?;
    sim.record("verification-crosses-every-boundary");
    sim.trace.record_number("verdict-hops", hops);
    Ok(())
}

/// An attempt settled in a later generation stays settled through every
/// continuation after it, and is never re-admitted.
fn settled_later(sim: &mut sim::Sim) -> TestResult {
    let dir = sim.scratch("later")?;
    let mut journal = quiet(&dir.join("run.jrnl"))?;
    let mut oracle = Oracle::default();
    let stranded = draw(sim, 1, 10);
    write(&mut journal, &mut oracle, 1, (1, stranded), Fate::Unknown)?;
    let chosen = draw(sim, 1, u32::try_from(stranded)?);
    journal = seal(&mut journal)?;
    let key = key_of(1, chosen)?;
    journal.compare_and_append_all(&[
        EffectEvent::OutcomeObserved {
            key,
            evidence: EffectEvidence::Applied,
        },
        EffectEvent::Verified {
            key,
            verification: verdict(1, true)?,
        },
    ])?;
    oracle.settle(1, chosen);
    let hops = draw(sim, 1, 3);
    for _ in 0..hops {
        journal = seal(&mut journal)?;
        assert_eq!(
            journal.recover().status(key),
            Some(AttemptStatus::Verified),
            "an attempt settled in an earlier successor stays settled"
        );
        let again = journal.compare_and_append(journal.tail(), &intent(1, chosen)?);
        assert!(again.is_err(), "and is never admitted a second time");
        assert_eq!(
            journal.recover().uncertain().len(),
            oracle.unknown(),
            "the rest of the carry is still unknown, no more and no fewer"
        );
    }
    assert_fold_agrees(&journal, &oracle, 1)?;
    sim.record("settled-later-stays-settled");
    sim.trace.record_number("later-stranded", stranded);
    sim.trace.record_number("later-hops", hops);
    Ok(())
}

/// A not-applied attempt is carried as not applied, and the retry it invites is a
/// new attempt the successor admits.
fn not_applied_retry(sim: &mut sim::Sim) -> TestResult {
    let dir = sim.scratch("retry")?;
    let mut journal = quiet(&dir.join("run.jrnl"))?;
    let mut oracle = Oracle::default();
    let attempts = draw(sim, 1, 6);
    write(
        &mut journal,
        &mut oracle,
        1,
        (1, attempts),
        Fate::NotApplied,
    )?;
    let hops = draw(sim, 1, 3);
    for _ in 0..hops {
        journal = seal(&mut journal)?;
        assert_eq!(
            journal.recover().status(key_of(1, attempts)?),
            Some(AttemptStatus::NotApplied),
            "a not-applied attempt is carried as not applied"
        );
    }
    let retry = attempts.saturating_add(1);
    journal.compare_and_append_all(&ladder(Fate::Applied, 1, retry)?)?;
    oracle.dispatch(1, retry, Fate::Applied);
    assert_eq!(
        journal.recover().status(key_of(1, retry)?),
        Some(AttemptStatus::Verified),
        "the retry is a new attempt and lands"
    );
    let again = journal.compare_and_append(journal.tail(), &intent(1, attempts)?);
    assert!(again.is_err(), "while the attempt it retried stays refused");
    assert_fold_agrees(&journal, &oracle, 1)?;
    sim.record("not-applied-retried-as-new-attempt");
    sim.trace.record_number("retry-attempts", attempts);
    Ok(())
}

// ── What a reopen reads back ──────────────────────────────────────────────

/// The streaming replay of every generation yields exactly its committed events.
fn replay_agrees(sim: &mut sim::Sim) -> TestResult {
    let dir = sim.scratch("replay")?;
    let mut journal = quiet(&dir.join("run.jrnl"))?;
    let mut oracle = Oracle::default();
    let generations = draw(sim, 1, 4);
    let mut next = 1_u64;
    for generation in 1..=generations {
        let count = draw(sim, 1, 20);
        write_drawn(sim, &mut journal, &mut oracle, 1, (next, count))?;
        next = next.saturating_add(count);
        let streamed = journal.replay()?.collect::<Result<Vec<_>, _>>()?;
        assert_eq!(
            streamed,
            journal.committed()?,
            "generation {generation} streams exactly what it committed"
        );
        if generation < generations {
            journal = seal(&mut journal)?;
        }
    }
    assert_fold_agrees(&journal, &oracle, 1)?;
    sim.record("replay-equals-committed");
    sim.trace.record_number("replay-generations", generations);
    Ok(())
}

/// Every position a successor acknowledged reads back, as the event it
/// acknowledged, after a reopen.
fn positions_read_back(sim: &mut sim::Sim) -> TestResult {
    let dir = sim.scratch("positions")?;
    let base = dir.join("run.jrnl");
    let mut journal = quiet(&base)?;
    let mut oracle = Oracle::default();
    let hops = draw(sim, 1, 3);
    let mut next = 1_u64;
    for _ in 0..hops {
        let count = draw(sim, 1, 8);
        write(&mut journal, &mut oracle, 1, (next, count), Fate::Applied)?;
        next = next.saturating_add(count);
        journal = seal(&mut journal)?;
    }
    let mut acknowledged = Vec::new();
    for _ in 0..draw(sim, 2, 10) {
        for event in ladder(Fate::Applied, 1, next)? {
            let ack = journal.compare_and_append(journal.tail(), &event)?;
            acknowledged.push((ack.position(), event));
        }
        oracle.dispatch(1, next, Fate::Applied);
        next = next.saturating_add(1);
    }
    let generation = journal.generation();
    drop(journal);

    let reopened = FileJournal::open_active(&base)?;
    assert_eq!(
        reopened.generation(),
        generation,
        "the reopen reaches the generation that acknowledged them"
    );
    for &(position, ref event) in &acknowledged {
        let entry = reopened
            .committed_entry(position)?
            .ok_or("an acknowledged position reads back after a reopen")?;
        assert_eq!(entry.position(), position, "at the position acknowledged");
        assert_eq!(
            entry.event(),
            event,
            "and it holds the event that was acknowledged there"
        );
    }
    assert_fold_agrees(&reopened, &oracle, 1)?;
    sim.record("acknowledged-positions-read-back");
    sim.trace.record_number("positions", acknowledged.len());
    sim.trace.record_number("positions-generation", generation);
    Ok(())
}

/// Two tenants interleaved flush by flush in one directory continue on their own
/// triggers and never cross.
fn tenants_interleaved(sim: &mut sim::Sim) -> TestResult {
    let dir = sim.scratch("interleave")?;
    let paths = [dir.join("tenant-1.jrnl"), dir.join("tenant-2.jrnl")];
    let mut journals = [
        continuing(&paths[0], 96, QUIET_BYTES)?,
        continuing(&paths[1], 96, QUIET_BYTES)?,
    ];
    let mut oracles = [Oracle::default(), Oracle::default()];
    let mut next = [1_u64, 1];
    let mut continued = [0_u64, 0];
    let steps = draw(sim, 24, 24);
    for _ in 0..steps {
        let who = usize::try_from(sim.rng().below(2))?;
        let action = u64::MAX.saturating_sub(u64::try_from(who)?);
        let count = draw(sim, 1, 8);
        write_drawn(
            sim,
            &mut journals[who],
            &mut oracles[who],
            action,
            (next[who], count),
        )?;
        next[who] = next[who].saturating_add(count);
        if let Some(successor) = continue_if_due(&mut journals[who])? {
            journals[who] = successor;
            continued[who] = continued[who].saturating_add(1);
        }
    }
    for (who, journal) in journals.iter().enumerate() {
        let tenant = u64::try_from(who)?;
        assert_fold_agrees(journal, &oracles[who], tenant.saturating_add(1))?;
        let mine = action_of(u64::MAX.saturating_sub(tenant))?;
        for held in journal.recover().attempts() {
            assert_eq!(
                held.key().action(),
                mine,
                "tenant {who} recovered another tenant's attempt"
            );
        }
        sim.trace
            .record_number(&format!("interleave-{who}-continued"), continued[who]);
    }
    assert_ne!(
        journals[0].path(),
        journals[1].path(),
        "the two tenants never share a live file"
    );
    sim.record("interleaved-tenants-never-cross");
    sim.trace.record_number("interleave-steps", steps);
    Ok(())
}

// ── The sweep ─────────────────────────────────────────────────────────────

#[test]
fn the_event_watermark_falls_on_the_flush_that_reaches_it() -> TestResult {
    sim::assert_replays(band(0), event_watermark)
}

#[test]
fn the_byte_watermark_alone_makes_a_continuation_due() -> TestResult {
    sim::assert_replays(band(1), byte_watermark)
}

#[test]
fn a_journal_without_a_lifecycle_is_never_due() -> TestResult {
    sim::assert_replays(band(2), never_due)
}

#[test]
fn the_headroom_falls_by_exactly_what_each_flush_appended() -> TestResult {
    sim::assert_replays(band(3), headroom)
}

#[test]
fn every_successor_is_the_next_fixed_width_name_beside_its_base() -> TestResult {
    sim::assert_replays(band(4), generation_names)
}

#[test]
fn a_successor_chains_from_the_tail_its_predecessor_sealed() -> TestResult {
    sim::assert_replays(band(5), chain_base)
}

#[test]
fn a_reopen_through_the_base_reaches_the_live_generation() -> TestResult {
    sim::assert_replays(band(6), reopen_reaches_live)
}

#[test]
fn a_base_named_like_a_generation_is_refused_before_it_exists() -> TestResult {
    sim::assert_replays(band(7), ambiguous_base)
}

#[test]
fn a_seal_is_one_frame_byte_identical_in_both_files() -> TestResult {
    sim::assert_replays(band(8), seal_bytes)
}

#[test]
fn a_sealed_predecessor_refuses_everything_and_keeps_its_bytes() -> TestResult {
    sim::assert_replays(band(9), sealed_refuses)
}

#[test]
fn a_seal_stopped_at_any_boundary_leaves_one_authority() -> TestResult {
    sim::assert_replays(band(10), stopped_seal)
}

#[test]
fn the_carry_bound_admits_its_size_and_refuses_one_more_unwritten() -> TestResult {
    sim::assert_replays(band(11), carry_bound)
}

#[test]
fn the_action_bound_admits_its_size_and_refuses_one_more_unwritten() -> TestResult {
    sim::assert_replays(band(12), action_bound)
}

#[test]
fn a_checkpoint_holds_one_record_per_action_whatever_the_history() -> TestResult {
    sim::assert_replays(band(13), one_record_per_action)
}

#[test]
fn every_walked_attempt_is_refused_after_a_continuation() -> TestResult {
    sim::assert_replays(band(14), older_refused)
}

#[test]
fn a_newer_attempt_of_every_folded_action_is_admitted() -> TestResult {
    sim::assert_replays(band(15), newer_admitted)
}

#[test]
fn a_failed_verification_crosses_every_continuation_unchanged() -> TestResult {
    sim::assert_replays(band(16), verification_crosses)
}

#[test]
fn an_attempt_settled_in_a_successor_stays_settled_after_the_next() -> TestResult {
    sim::assert_replays(band(17), settled_later)
}

#[test]
fn a_not_applied_attempt_is_carried_and_its_retry_admitted() -> TestResult {
    sim::assert_replays(band(18), not_applied_retry)
}

#[test]
fn every_generation_streams_exactly_what_it_committed() -> TestResult {
    sim::assert_replays(band(19), replay_agrees)
}

#[test]
fn every_acknowledged_position_reads_back_after_a_reopen() -> TestResult {
    sim::assert_replays(band(20), positions_read_back)
}

#[test]
fn interleaved_tenants_continue_on_their_own_triggers_and_never_cross() -> TestResult {
    sim::assert_replays(band(21), tenants_interleaved)
}

/// The widest checkpoint both bounds admit at once is sealed by the shipped writer,
/// and its successor seals it again.
///
/// Every folded action carries a failed verification, the widest settled record,
/// beside a full unresolved carry. The two counts are charged before the frame
/// is, so a pair of counts the frame could not hold would pass both checks and
/// then be refused by the writer as a storage fault; here they must continue.
fn widest_carry(sim: &mut sim::Sim) -> TestResult {
    let dir = sim.scratch("widest")?;
    let mut journal = quiet(&dir.join("run.jrnl"))?;
    let mut oracle = Oracle::default();
    let actions = u64::try_from(MAX_CHECKPOINT_SETTLED)?;
    let unknown = u64::try_from(MAX_CHECKPOINT_UNRESOLVED)?;
    let holders = draw(sim, 1, 6);
    let mut batch = Vec::new();
    let mut attempts = (1..=actions)
        .map(|action| (action, 1, Fate::VerificationFailed))
        .collect::<Vec<_>>();
    for number in 1..=unknown {
        let holder = number
            .checked_rem(holders)
            .ok_or("the seed draws at least one holder")?;
        attempts.push((
            actions.saturating_add(1).saturating_add(holder),
            number,
            Fate::Unknown,
        ));
    }
    let last = attempts.len();
    for (index, &(action, attempt, fate)) in attempts.iter().enumerate() {
        batch.extend(ladder(fate, action, attempt)?);
        oracle.dispatch(action, attempt, fate);
        if batch.len() >= 64 || index.saturating_add(1) == last {
            journal.compare_and_append_all(&batch)?;
            batch.clear();
        }
    }
    let mut successor = seal(&mut journal)?;
    for generation in 1..=2_u64 {
        let checkpoint = successor
            .checkpoint()
            .ok_or("a successor is opened from a checkpoint")?;
        assert_eq!(
            checkpoint.settled().len(),
            MAX_CHECKPOINT_SETTLED,
            "generation {generation}: every folded action is carried"
        );
        assert_eq!(
            successor.recover().uncertain().len(),
            MAX_CHECKPOINT_UNRESOLVED,
            "generation {generation}: every unknown attempt is carried, still unknown"
        );
        assert_fold_agrees(&successor, &oracle, 1)?;
        if generation == 1 {
            successor = seal(&mut successor)?;
        }
    }
    sim.trace.record_number("widest-holders", holders);
    Ok(())
}

/// One frame's start offset per frame of `bytes`, read off the length prefixes.
fn frame_starts(bytes: &[u8]) -> Result<Vec<usize>, Box<dyn Error>> {
    let mut starts = Vec::new();
    let mut at = 0_usize;
    while at < bytes.len() {
        starts.push(at);
        let prefix: [u8; 4] = bytes
            .get(at..at.saturating_add(4))
            .ok_or("a whole prefix at every frame start")?
            .try_into()?;
        let payload = usize::try_from(u32::from_be_bytes(prefix))?;
        at = at
            .saturating_add(4)
            .saturating_add(payload)
            .saturating_add(32);
    }
    Ok(starts)
}

/// A lengthened acknowledged event in a successor is refused, wherever it sits
/// behind the seal the successor carries and by however much it was lengthened.
///
/// The open resolves a cut against the last whole frame before it, which in a
/// successor holding one event is the carried seal. Resolved against the genesis
/// instead, that one acknowledged event read as an append nobody finished and was
/// trimmed. So after every append the seed lengthens every event frame the
/// successor holds — the first append is the one-event successor — and each lie is
/// refused with the file byte-identical; restoring the bytes reopens them whole,
/// so what is refused is the lie and not the file.
fn lengthened_behind_seal(sim: &mut sim::Sim) -> TestResult {
    let dir = sim.scratch("lengthened")?;
    let mut journal = quiet(&dir.join("run.jrnl"))?;
    let mut oracle = Oracle::default();
    let before = draw(sim, 1, 6);
    write_drawn(sim, &mut journal, &mut oracle, 1, (1, before))?;
    let path = seal(&mut journal)?.path().to_path_buf();
    let own = draw(sim, 1, 4);
    for number in 1..=own {
        {
            let mut successor = FileJournal::open(&path)?;
            successor.compare_and_append(successor.tail(), &intent(2, number)?)?;
        }
        let bytes = std::fs::read(&path)?;
        let starts = frame_starts(&bytes)?;
        for (target, &at) in starts.iter().enumerate().skip(1) {
            let prefix: [u8; 4] = bytes
                .get(at..at.saturating_add(4))
                .ok_or("every frame has a prefix")?
                .try_into()?;
            let extra = 1_u32.saturating_add(sim.rng().below(96));
            let mut lied = bytes.clone();
            let lengthened = u32::from_be_bytes(prefix)
                .saturating_add(extra)
                .to_be_bytes();
            lied.splice(at..at.saturating_add(4), lengthened);
            std::fs::write(&path, &lied)?;
            let answer = FileJournal::open(&path);
            assert!(
                matches!(answer, Err(JournalError::Corrupt(_))),
                "event {target} of {number} lengthened by {extra} is refused, got {answer:?}"
            );
            assert_eq!(
                std::fs::read(&path)?,
                lied,
                "and the file is byte-identical"
            );
            sim.trace.record_number("lengthened-by", u64::from(extra));
        }
        std::fs::write(&path, &bytes)?;
        let whole = FileJournal::open(&path)?;
        assert_eq!(
            u64::try_from(whole.committed()?.len())?,
            number,
            "the restored file reopens with every acknowledged event"
        );
    }
    sim.trace.record_number("lengthened-events", own);
    Ok(())
}

#[test]
fn the_widest_carry_both_bounds_admit_is_sealed_twice() -> TestResult {
    sim::assert_replays(band(25), widest_carry)
}

#[test]
fn a_lengthened_event_behind_a_carried_seal_is_refused_on_every_seed() -> TestResult {
    sim::assert_replays(band(26), lengthened_behind_seal)
}

/// The same seed replays to one hash and distinct seeds reach distinct ones.
///
/// `assert_replays` proves the first half for every family above; the second
/// half is what keeps it honest, because a family whose trace ignored its seed
/// would replay perfectly while testing one shape.
#[test]
fn the_same_seed_replays_and_distinct_seeds_diverge() -> TestResult {
    let wide = Band::new(FIRST_SEED.saturating_add(22_u64.saturating_mul(SEEDS)), 8);
    let first = sim::sweep(wide, generation_names)?;
    let second = sim::sweep(wide, generation_names)?;
    assert_eq!(first, second, "one seed, one trace hash");
    let distinct: BTreeSet<u64> = first.iter().copied().collect();
    assert!(
        distinct.len() > 1,
        "eight seeds reached {} distinct trace(s), so the trace does not depend on the seed",
        distinct.len()
    );
    Ok(())
}
