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

mod seal_limits;

mod readback;

mod checkpoint;
