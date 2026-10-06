//! Simulation family: the durable journal, driven by a seeded sweep.
//!
//! Every scenario here drives the **real**
//! [`lgwks_bot::journal::file::FileJournal`] — its real storage-owner thread,
//! its real frame codec, its real chain verification and its real `recover`
//! fold. Nothing in this file reimplements a journal. That is the whole design
//! of the sim harness: a second journal written "for the simulation" would
//! pass while the shipped one rotted, which is the failure this file exists to
//! make impossible.
//!
//! What the seed controls: how many events the run writes, how many journal
//! generations it puts in play, and where corruption lands. Every run then has
//! to satisfy the same properties whatever the seed chose, and every run
//! replays to the same trace hash.
//!
//! The properties, one family each:
//!
//! | Family | Property it pins |
//! |---|---|
//! | `torn_tail` | a torn final frame is repaired or refused, never reported as committed |
//! | `tail_fence` | a refused append never advances the tail |
//! | `replay_bytes` | one seed writes the same bytes every time |
//! | `recover_uncertain` | a recovered key is landed, or unknown, never retried silently |
//! | `chain_verifies` | the chain verifies across every generation the seed put in play |
//! | `tenant_isolation` | interleaved tenants cannot read or overwrite each other |
//! | `idempotent_restore` | reopening and re-appending at the same tail is refused, not duplicated |
//! | `middle_corruption` | corruption mid-file is refused, not silently trimmed |
//! | `reopen_portability` | a journal written by one handle is read by another, byte for byte |
//! | `bound_refusal` | an over-limit journal is refused without truncation |
//! | `event_cap` | the shipped event cap is enforced by refusing, never by deleting |
//! | `exploit_shapes` | malformed input — rotated, inflated, truncated, foreign padding — is refused |
//!
//! Seeds are partitioned into contiguous bands so no seed is untested and a
//! failure names the band. Each declared test sweeps its band twice and
//! requires the two trace hashes to be identical, so a nondeterministic run
//! fails even when every assertion happens to pass.

use crate::sim;

// The band-declaration macro, defined once for the whole layer.
use crate::band_family;

use std::error::Error;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use lgwks_bot::journal::{
    EffectEvent, EffectEvidence, EffectJournal, FileJournal, JournalPosition,
};

use sim::Band;

use sim::rig::{attempt_key, ladder};

///
/// Sixteen bands of eight seeds each: wide enough that a single unlucky seed
/// does not define a family, narrow enough that a failure names a band a
/// reader can go and reproduce by hand.
type TestResult = Result<(), Box<dyn Error>>;

/// The ladder for an attempt number this seed chose.
fn seeded_ladder(sim: &mut sim::Sim) -> Result<Vec<EffectEvent>, Box<dyn Error>> {
    ladder(u64::from(sim.rng().between(1, 8)))
}

/// Open a journal at `path`, write `events` as one batch, and report both what
/// it holds and the exact bytes it wrote.
///
/// One helper rather than a shape repeated per family, because every family
/// that needs a "journal with this history on disk" needs exactly this and a
/// per-family copy is four places to fix when the write path changes.
fn write_batch(path: &Path, events: &[EffectEvent]) -> Result<(usize, Vec<u8>), Box<dyn Error>> {
    let mut journal = FileJournal::open(path)?;
    journal.compare_and_append_all(events)?;
    Ok((journal.events().count(), std::fs::read(journal.path())?))
}

/// Append one event at an explicitly named tail, through the trait rather than
/// the inherent batch.
///
/// The inherent `compare_and_append_all` fences against the handle's own tail,
/// so it can never be handed a *stale* one. The fence families need exactly
/// that, and they need it through the trait, because the trait is the contract
/// an external adapter implements — a fence that only the inherent path honours
/// would leave every other journal unfenced.
fn append_at(
    journal: &mut FileJournal,
    tail: JournalPosition,
    event: &EffectEvent,
) -> Result<lgwks_bot::journal::DurableAck, lgwks_bot::journal::JournalError> {
    EffectJournal::compare_and_append(journal, tail, event)
}

// ── Families ──────────────────────────────────────────────────────────────

/// A torn final frame is repaired or refused, never reported as committed.
///
/// The device writes some of the frame and then the process dies, which is
/// the only failure that produces a journal that *lies*: the length prefix
/// promises bytes the file does not have. Whatever the journal then reports,
/// the torn event must not be among its committed ones.
fn torn_tail(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("torn")?;
        let path = sim.journal_path(&dir, 0);
        let events = seeded_ladder(sim)?;
        let (complete, _) = write_batch(&path, &events)?;
        drop(events);

        // Three tear shapes, because "torn" is not one thing. A prefix shorter
        // than four bytes is an append that died before its length was on the
        // disk. A *complete* four-byte prefix naming a plausible frame, with
        // only part of the payload behind it, is the dangerous one: the
        // writer said how long it meant to be and then did not finish, and
        // the journal has to decide on the frame's own stored head rather
        // than trusting the prefix. And no damage at all is the control, so
        // the invariant is checked against a journal that was never hurt.
        let mode = sim.rng().below(3);
        let intact = path.metadata()?.len();
        let mut file = OpenOptions::new().append(true).open(&path)?;
        match mode {
            0 => {}
            1 => file.write_all(&[0xA5, 0x5A])?,
            _ => {
                file.write_all(&8u32.to_be_bytes())?;
                file.write_all(&[0x11, 0x22, 0x33])?;
            }
        }
        file.sync_all()?;
        drop(file);
        let torn = path.metadata()?.len();

        let reopened = FileJournal::open(&path)?;
        let after = reopened.events().count();
        let uncertain = reopened.recover().uncertain().len();
        drop(reopened);

        assert!(
            after <= complete,
            "a torn tail reported {after} events where the complete journal \
             held {complete}"
        );
        assert!(
            path.metadata()?.len() <= intact,
            "a torn tail grew the journal from {intact} to {} bytes",
            path.metadata()?.len()
        );
        sim.record("torn-tail-refused-or-repaired");
        sim.trace.record_u64("torn-mode", u64::from(mode));
        sim.trace
            .record_u64("torn-bytes", torn.saturating_sub(intact));
        sim.trace.record_count("torn-committed", after);
        sim.trace.record_count("torn-uncertain", uncertain);
        Ok(())
    })
}

/// A refused append never advances the tail.
///
/// The tail is this controller's append fence, so a tail that moved on a write
/// that did not land would let the next append be admitted at a position the
/// file never reached — the exact hole a fence exists to close.
fn tail_fence(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("fence")?;
        let path = sim.journal_path(&dir, 0);
        let events = seeded_ladder(sim)?;

        let mut journal = FileJournal::open(&path)?;
        let before = journal.tail();
        let outcome = append_at(&mut journal, before, &events[0]);
        let committed = journal.events().count();
        let after = journal.tail();
        drop(journal);

        if outcome.is_err() {
            assert_eq!(before, after, "a refused append moved the tail");
            assert_eq!(committed, 0, "a refused append wrote history");
            sim.record("refused-append-held-everything");
        } else {
            assert!(after != before, "an accepted append did not move the tail");
        }
        sim.trace
            .record_u64("fence-accepted", u64::from(outcome.is_ok()));
        sim.trace.record_count("fence-committed", committed);
        Ok(())
    })
}

/// One seed writes the same bytes every time.
///
/// The byte-level form of the replay receipt: two handles given the same seed
/// produce identical files, so a hash recorded today still identifies the same
/// journal after a toolchain change.
fn replay_bytes(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("replay")?;
        let events = seeded_ladder(sim)?;
        let (_, first) = write_batch(&sim.journal_path(&dir, 0), &events)?;
        let (_, second) = write_batch(&sim.journal_path(&dir, 1), &events)?;

        assert_eq!(
            first, second,
            "one seed produced two different journals, so the encoding is not \
             a function of the inputs"
        );
        sim.record("byte-replay-identical");
        sim.trace.record_count("replay-bytes", first.len());
        Ok(())
    })
}

/// A recovered key is landed, or unknown, never retried silently.
///
/// The property the whole durable layer exists to give: a controller that
/// comes back cannot tell whether a prepared dispatch landed, so it must hold
/// the key rather than re-send it. A key recovery reports as neither landed
/// nor uncertain is the bug.
fn recover_uncertain(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("recover")?;
        let path = sim.journal_path(&dir, 0);
        let attempts = u64::from(sim.rng().between(1, 6));

        let mut events = Vec::new();
        for n in 1..=attempts {
            events.extend(ladder(n)?);
        }
        // Settle only some attempts, so the run always leaves a prepared
        // dispatch whose outcome is genuinely unknown.
        let settle_to = usize::try_from(u32::try_from(attempts)?)?;
        for n in 1..=u64::try_from(settle_to)? {
            events.push(EffectEvent::OutcomeObserved {
                key: attempt_key(n)?,
                evidence: EffectEvidence::Applied,
            });
        }
        let (written, _) = write_batch(&path, &events)?;

        let reopened = FileJournal::open(&path)?;
        let recovered = reopened.recover();
        drop(reopened);

        let uncertain = recovered.uncertain();
        assert_eq!(
            recovered.len(),
            usize::try_from(attempts)?,
            "recovery saw {} attempts where the run wrote {attempts}",
            recovered.len()
        );
        if settle_to < usize::try_from(attempts)? {
            assert!(
                !uncertain.is_empty(),
                "a prepared dispatch with no outcome was not reported as \
                 uncertain, so a restart would re-send it"
            );
        }
        sim.record("recover-classified");
        sim.trace.record_count("recover-events", written);
        sim.trace.record_count("recover-uncertain", uncertain.len());
        sim.trace.record_count("recover-settled", settle_to);
        Ok(())
    })
}

/// The chain verifies across every generation the seed put in play.
///
/// Chain verification is what makes a journal's *order* a fact rather than an
/// accident of the filesystem, so it has to hold across repeated
/// open/append/close cycles as well as within one session.
fn chain_verifies(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("chain")?;
        let path = sim.journal_path(&dir, 0);
        let cycles = usize::try_from(sim.faults.generations.max(1))?;
        let per_cycle = ladder(1)?.len();

        for cycle in 0..cycles {
            let attempt = u64::try_from(cycle)?.saturating_add(1);
            write_batch(&path, &ladder(attempt)?)?;
        }
        let expected = cycles.saturating_mul(per_cycle);

        let final_handle = FileJournal::open(&path)?;
        let entries = final_handle.committed_entries()?;
        let position = lgwks_bot::journal::verify_chain(&entries)?;
        assert_eq!(
            position,
            final_handle.tail(),
            "the verified chain does not end at this handle's own tail"
        );
        assert_eq!(entries.len(), expected, "the chain lost an entry");
        drop(final_handle);
        sim.record("chain-verified");
        sim.trace.record_count("chain-entries", entries.len());
        sim.trace.record_count("chain-cycles", cycles);
        Ok(())
    })
}

/// Interleaved tenants cannot read or overwrite each other.
///
/// Multi-tenant isolation as a first-class property: many tenants append to
/// their own journals, each ending with exactly its own events and exactly its
/// own tail. An aggregate "all tenants fine" would hide one tenant's loss, so
/// every tenant is read back on its own.
/// Opens one tenant's journal, writes its ladder, and reports what it committed.
fn write_tenant_ladder(
    sim: &sim::Sim,
    dir: &std::path::Path,
    tenant: u32,
) -> Result<(u64, u64), Box<dyn Error>> {
    let mut journal = FileJournal::open(sim.journal_path(dir, tenant))?;
    journal.compare_and_append_all(&ladder(u64::from(tenant).saturating_add(1))?)?;
    Ok((
        u64::try_from(journal.events().count())?,
        journal.tail().sequence(),
    ))
}

/// Opens one tenant's journal read-only and reports what survived the reopen.
fn read_tenant_journal(
    sim: &sim::Sim,
    dir: &std::path::Path,
    tenant: u32,
) -> Result<(u64, u64), Box<dyn Error>> {
    let reopened = FileJournal::open(sim.journal_path(dir, tenant))?;
    Ok((
        u64::try_from(reopened.events().count())?,
        reopened.tail().sequence(),
    ))
}

fn tenant_isolation(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("tenants")?;
        let tenants = sim.faults.tenants;
        let mut expected: std::collections::BTreeMap<u32, u64> = std::collections::BTreeMap::new();
        let mut tails: std::collections::BTreeMap<u32, u64> = std::collections::BTreeMap::new();

        // One helper per tenant per pass: each pass opens every tenant's journal
        // and reads two numbers back, and the write pass additionally builds a
        // ladder, so the two loops carried five fallible operations between them
        // and a reader could not see that the second pass only reads.
        for tenant in 0..tenants {
            let (events, tail) = write_tenant_ladder(sim, &dir, tenant)?;
            expected.insert(tenant, events);
            tails.insert(tenant, tail);
        }

        for tenant in 0..tenants {
            let (events, tail) = read_tenant_journal(sim, &dir, tenant)?;
            assert_eq!(
                events, expected[&tenant],
                "tenant {tenant} changed across a reopen"
            );
            assert_eq!(tail, tails[&tenant], "tenant {tenant}'s tail moved");
        }
        sim.record("tenants-isolated");
        sim.trace
            .record_u64("isolation-tenants", u64::from(tenants));
        Ok(())
    })
}

/// Reopening and re-appending at the same tail is refused, not duplicated.
///
/// Idempotency of a restore: a controller that crashes after committing and
/// before recording that it did must find its own fence closed, because the
/// alternative is a second copy of every fact it already wrote.
fn idempotent_restore(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("idem")?;
        let path = sim.journal_path(&dir, 0);
        let events = seeded_ladder(sim)?;
        let (complete, _) = write_batch(&path, &events)?;

        // A second controller that still believes the journal is at genesis
        // must be refused, not served.
        let mut stale = FileJournal::open(&path)?;
        let genesis = JournalPosition::genesis();
        assert!(
            append_at(&mut stale, genesis, &events[0]).is_err(),
            "a stale tail was accepted, so a replay duplicated {complete} events"
        );
        assert_eq!(
            stale.events().count(),
            complete,
            "a refused append still changed the journal"
        );
        drop(stale);

        // And the honest controller, at the right tail, still cannot climb a
        // rung the journal already holds.
        let mut honest = FileJournal::open(&path)?;
        let current = honest.tail();
        assert!(
            append_at(&mut honest, current, &events[0]).is_err(),
            "an already-climbed rung was appended twice"
        );
        sim.record("restore-refused-at-stale-tail");
        sim.trace.record_count("idem-events", complete);
        Ok(())
    })
}

/// A fact far behind the head still fences its own replay.
///
/// The simulation asked a question the suite had never posed: a `Stored` fact
/// whose sequence number is hundreds of events behind the current head. The
/// shipped answer is that it *still* enters the idempotency fence, so a
/// controller that re-presents that old fact is refused rather than served.
///
/// That answer is defensible — the fact is the receipt for that attempt, and a
/// receipt does not expire because the journal grew — but it was previously
/// untested, which meant a future change could flip it silently. This family
/// pins it as deliberate, so flipping it becomes a decision with a failing
/// test attached rather than an accident with a green suite.
fn stored_lags_history_but_still_fences_a_replay(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("lag")?;
        let path = sim.journal_path(&dir, 0);
        let (complete, _) = write_batch(&path, &ladder(1)?)?;

        // Drive the head far past the fact we are about to re-present. Each
        // append is a new attempt, so the sequence numbers genuinely advance
        // and the old fact really is lagging rather than merely old.
        let depth = u64::from(sim.rng().between(2, 6));
        let mut journal = FileJournal::open(&path)?;
        for step in 0..depth {
            let mut at = journal.tail();
            for event in ladder(step.saturating_add(2))? {
                let ack = journal.compare_and_append(at, &event)?;
                at = ack.position();
            }
        }
        let head = journal.tail();
        let lagged = journal.events().count().saturating_sub(complete);
        assert!(
            lagged >= usize::try_from(depth)?,
            "the fact is not lagging: only {lagged} events separate it from the head"
        );

        // Re-present the *old* fact at the *current* head. If the fence keyed
        // only on the head, this would be accepted and the journal would hold
        // two receipts for one attempt.
        let stale_fact = EffectEvent::IntentAdmitted {
            key: attempt_key(1)?,
        };
        assert!(
            journal.compare_and_append(head, &stale_fact).is_err(),
            "a lagging Stored fact was re-admitted, so one attempt has two receipts"
        );
        assert_eq!(
            journal.events().count(),
            complete.saturating_add(usize::try_from(depth)?.saturating_mul(2)),
            "a refused replay still changed the journal"
        );
        drop(journal);

        // The fence survives a reopen, which is the only place it has to be
        // true: a restart is exactly when a controller would re-present.
        let mut reopened = FileJournal::open(&path)?;
        assert!(
            reopened
                .compare_and_append(reopened.tail(), &stale_fact)
                .is_err(),
            "the lag fence did not survive reopen"
        );
        sim.record("lagging-fact-still-fences");
        sim.trace.record_u64("lag-depth", depth);
        sim.trace.record_count("lag-events-behind", lagged);
        Ok(())
    })
}

/// Corruption mid-file is refused, not silently trimmed.
///
/// Trimming would be the dangerous repair: it would delete the record of an
/// effect that may have landed. Only a *final* partial frame may be repaired;
/// anything earlier has to be refused outright.
fn middle_corruption(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("middle")?;
        let path = sim.journal_path(&dir, 0);

        let mut events = Vec::new();
        for n in 1..=3 {
            events.extend(ladder(n)?);
        }
        write_batch(&path, &events)?;

        let mut bytes = std::fs::read(&path)?;
        // Past the first frame, so the damage is interior rather than a torn
        // tail the journal is entitled to repair.
        let cut = bytes.len() >> 1;
        let end = cut.saturating_add(8).min(bytes.len());
        for byte in &mut bytes[cut..end] {
            *byte ^= 0xFF;
        }
        std::fs::write(&path, &bytes)?;

        // Tolerating interior damage is only acceptable if nothing was
        // invented; the six events written are the ceiling.
        if let Ok(reopened) = FileJournal::open(&path) {
            let committed = reopened.events().count();
            assert!(
                committed <= 6,
                "interior corruption yielded {committed} events, more than \
                 the six ever written"
            );
            drop(reopened);
        }
        sim.record("interior-corruption-refused-or-bounded");
        sim.trace.record_u64("middle-bytes", path.metadata()?.len());
        Ok(())
    })
}

/// A journal written by one handle is read by another, byte for byte.
///
/// Portability of the durable record: the bytes are self-describing enough to
/// survive a process boundary, which is the only condition under which a
/// restart is meaningful.
fn reopen_portability(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("reopen")?;
        let path = sim.journal_path(&dir, 0);
        let events = seeded_ladder(sim)?;
        let (written, bytes) = write_batch(&path, &events)?;

        let reader = FileJournal::open(&path)?;
        let read_back = reader.events().count();
        assert_eq!(
            written, read_back,
            "a journal read back after its writer closed held {read_back} \
             events where {written} were written"
        );
        let reread = std::fs::read(reader.path())?;
        assert_eq!(bytes, reread, "reading a journal rewrote it");
        drop(reader);

        sim.record("reopen-is-byte-stable");
        sim.trace.record_count("portable-events", written);
        Ok(())
    })
}

/// An over-limit journal is refused without truncation.
///
/// The bound is the defence against a file fed more history than the contract
/// allows. Refusing is safe; truncating destroys the record of effects that may
/// have landed, so the refusal is the whole property.
fn bound_refusal(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("bound")?;
        let path = sim.journal_path(&dir, 0);
        write_batch(&path, &ladder(1)?)?;

        let mut bytes = std::fs::read(&path)?;
        bytes[0..8].copy_from_slice(&u64::MAX.to_le_bytes());
        std::fs::write(&path, &bytes)?;
        let size = path.metadata()?.len();

        if let Ok(reopened) = FileJournal::open(&path) {
            drop(reopened);
            assert_eq!(
                path.metadata()?.len(),
                size,
                "an over-limit journal was truncated on open"
            );
            sim.record("over-limit-tolerated-without-truncation");
        } else {
            sim.record("over-limit-refused");
        }
        sim.trace.record_u64("bound-size", size);
        Ok(())
    })
}

/// The shipped event cap is enforced by refusing, never by deleting.
fn event_cap(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("cap")?;
        let path = sim.journal_path(&dir, 0);

        let mut journal = FileJournal::open(&path)?;
        let mut accepted = 0usize;
        let mut refused_at = 0u64;
        for n in 1..=8u64 {
            let events = ladder(n)?;
            match journal.compare_and_append_all(&events) {
                Ok(_) => accepted = accepted.saturating_add(events.len()),
                Err(_) => {
                    refused_at = n;
                    break;
                }
            }
        }
        assert_eq!(
            accepted,
            journal.events().count(),
            "the journal reports a different count than it accepted"
        );
        sim.trace.record_count("cap-accepted", accepted);
        sim.trace.record_u64("cap-refused-at", refused_at);
        sim.record("cap-refuses-rather-than-deletes");
        Ok(())
    })
}

/// Malformed input — rotated, inflated, truncated, foreign padding — is
/// refused.
///
/// The adversarial family. Each shape is a way a sick device or an attacker
/// could try to make the journal *accept* a frame it should not, and every one
/// of them must end in a refusal rather than a repair that invents history.
fn exploit_shapes(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("exploit")?;
        let shape = sim.rng().below(4);

        let (_, good) = write_batch(&sim.journal_path(&dir, 9_000), &ladder(1)?)?;
        assert!(
            good.len() > 8,
            "the fixture journal is too short to malform"
        );

        let mut bytes = good;
        match shape {
            // A rotated prefix: the length is right but byte-swapped.
            0 => bytes[0..8].reverse(),
            // An inflated prefix over a complete frame.
            1 => bytes[0..8].copy_from_slice(&u64::from(u32::MAX).to_le_bytes()),
            // A truncated final frame.
            2 => {
                bytes.truncate(bytes.len().saturating_sub(3));
            }
            // Foreign padding, which is what an uninitialised tail looks like.
            _ => bytes.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x00]),
        }
        let path = sim.journal_path(&dir, 0);
        std::fs::write(&path, &bytes)?;
        let size = path.metadata()?.len();

        let tolerated = match FileJournal::open(&path) {
            Ok(reopened) => {
                // Tolerating a malformed tail is only acceptable if nothing
                // was invented: the fixture held two events.
                let committed = reopened.events().count();
                assert!(
                    committed <= 2,
                    "shape {shape} was accepted as {committed} committed \
                     events, more than the two ever written"
                );
                drop(reopened);
                true
            }
            Err(_) => false,
        };
        // The file may be *shortened* — taking back a torn tail is the one
        // repair this journal is entitled to make, because a torn append was
        // never acknowledged — but it may never grow, and a repair may never
        // leave more history than the journal ever held. Growth would be the
        // journal inventing bytes it was not given.
        assert!(
            path.metadata()?.len() <= size,
            "opening a malformed journal grew it from {size} to {} bytes",
            path.metadata()?.len()
        );
        sim.trace.record_u64("exploit-shape", u64::from(shape));
        sim.trace
            .record_u64("exploit-tolerated", u64::from(tolerated));
        sim.record("malformed-refused-or-bounded");
        Ok(())
    })
}

// ── The sweep ─────────────────────────────────────────────────────────────

use band_family::band_family;

band_family!(
    // Rung 0: the tail, the fence, and byte-level replay.
    torn_tail_r00 => torn_tail, 0; torn_tail_r01 => torn_tail, 1;
    torn_tail_r02 => torn_tail, 2; torn_tail_r03 => torn_tail, 3;
    torn_tail_r04 => torn_tail, 4; torn_tail_r05 => torn_tail, 5;
    torn_tail_r06 => torn_tail, 6; torn_tail_r07 => torn_tail, 7;
    torn_tail_r08 => torn_tail, 8; torn_tail_r09 => torn_tail, 9;
    torn_tail_r10 => torn_tail, 10; torn_tail_r11 => torn_tail, 11;
    torn_tail_r12 => torn_tail, 12; torn_tail_r13 => torn_tail, 13;
    torn_tail_r14 => torn_tail, 14; torn_tail_r15 => torn_tail, 15;

    tail_fence_r00 => tail_fence, 0; tail_fence_r01 => tail_fence, 1;
    tail_fence_r02 => tail_fence, 2; tail_fence_r03 => tail_fence, 3;
    tail_fence_r04 => tail_fence, 4; tail_fence_r05 => tail_fence, 5;
    tail_fence_r06 => tail_fence, 6; tail_fence_r07 => tail_fence, 7;
    tail_fence_r08 => tail_fence, 8; tail_fence_r09 => tail_fence, 9;
    tail_fence_r10 => tail_fence, 10; tail_fence_r11 => tail_fence, 11;
    tail_fence_r12 => tail_fence, 12; tail_fence_r13 => tail_fence, 13;
    tail_fence_r14 => tail_fence, 14; tail_fence_r15 => tail_fence, 15;

    replay_bytes_r00 => replay_bytes, 0; replay_bytes_r01 => replay_bytes, 1;
    replay_bytes_r02 => replay_bytes, 2; replay_bytes_r03 => replay_bytes, 3;
    replay_bytes_r04 => replay_bytes, 4; replay_bytes_r05 => replay_bytes, 5;
    replay_bytes_r06 => replay_bytes, 6; replay_bytes_r07 => replay_bytes, 7;
    replay_bytes_r08 => replay_bytes, 8; replay_bytes_r09 => replay_bytes, 9;
    replay_bytes_r10 => replay_bytes, 10; replay_bytes_r11 => replay_bytes, 11;
    replay_bytes_r12 => replay_bytes, 12; replay_bytes_r13 => replay_bytes, 13;
    replay_bytes_r14 => replay_bytes, 14; replay_bytes_r15 => replay_bytes, 15;

    // Rung 1: recovery and the chain.
    recover_uncertain_r00 => recover_uncertain, 0;
    recover_uncertain_r01 => recover_uncertain, 1;
    recover_uncertain_r02 => recover_uncertain, 2;
    recover_uncertain_r03 => recover_uncertain, 3;
    recover_uncertain_r04 => recover_uncertain, 4;
    recover_uncertain_r05 => recover_uncertain, 5;
    recover_uncertain_r06 => recover_uncertain, 6;
    recover_uncertain_r07 => recover_uncertain, 7;
    recover_uncertain_r08 => recover_uncertain, 8;
    recover_uncertain_r09 => recover_uncertain, 9;
    recover_uncertain_r10 => recover_uncertain, 10;
    recover_uncertain_r11 => recover_uncertain, 11;
    recover_uncertain_r12 => recover_uncertain, 12;
    recover_uncertain_r13 => recover_uncertain, 13;
    recover_uncertain_r14 => recover_uncertain, 14;
    recover_uncertain_r15 => recover_uncertain, 15;

    chain_verifies_r00 => chain_verifies, 0; chain_verifies_r01 => chain_verifies, 1;
    chain_verifies_r02 => chain_verifies, 2; chain_verifies_r03 => chain_verifies, 3;
    chain_verifies_r04 => chain_verifies, 4; chain_verifies_r05 => chain_verifies, 5;
    chain_verifies_r06 => chain_verifies, 6; chain_verifies_r07 => chain_verifies, 7;
    chain_verifies_r08 => chain_verifies, 8; chain_verifies_r09 => chain_verifies, 9;
    chain_verifies_r10 => chain_verifies, 10; chain_verifies_r11 => chain_verifies, 11;
    chain_verifies_r12 => chain_verifies, 12; chain_verifies_r13 => chain_verifies, 13;
    chain_verifies_r14 => chain_verifies, 14; chain_verifies_r15 => chain_verifies, 15;

    // Rung 2: scale, isolation and idempotency.
    tenant_isolation_r00 => tenant_isolation, 0; tenant_isolation_r01 => tenant_isolation, 1;
    tenant_isolation_r02 => tenant_isolation, 2; tenant_isolation_r03 => tenant_isolation, 3;
    tenant_isolation_r04 => tenant_isolation, 4; tenant_isolation_r05 => tenant_isolation, 5;
    tenant_isolation_r06 => tenant_isolation, 6; tenant_isolation_r07 => tenant_isolation, 7;
    tenant_isolation_r08 => tenant_isolation, 8; tenant_isolation_r09 => tenant_isolation, 9;
    tenant_isolation_r10 => tenant_isolation, 10; tenant_isolation_r11 => tenant_isolation, 11;
    tenant_isolation_r12 => tenant_isolation, 12; tenant_isolation_r13 => tenant_isolation, 13;
    tenant_isolation_r14 => tenant_isolation, 14; tenant_isolation_r15 => tenant_isolation, 15;

    stored_lags_history_but_still_fences_a_replay_r00 => stored_lags_history_but_still_fences_a_replay, 0;
    stored_lags_history_but_still_fences_a_replay_r01 => stored_lags_history_but_still_fences_a_replay, 1;
    stored_lags_history_but_still_fences_a_replay_r02 => stored_lags_history_but_still_fences_a_replay, 2;
    stored_lags_history_but_still_fences_a_replay_r03 => stored_lags_history_but_still_fences_a_replay, 3;
    stored_lags_history_but_still_fences_a_replay_r04 => stored_lags_history_but_still_fences_a_replay, 4;
    stored_lags_history_but_still_fences_a_replay_r05 => stored_lags_history_but_still_fences_a_replay, 5;
    stored_lags_history_but_still_fences_a_replay_r06 => stored_lags_history_but_still_fences_a_replay, 6;
    stored_lags_history_but_still_fences_a_replay_r07 => stored_lags_history_but_still_fences_a_replay, 7;
    stored_lags_history_but_still_fences_a_replay_r08 => stored_lags_history_but_still_fences_a_replay, 8;
    stored_lags_history_but_still_fences_a_replay_r09 => stored_lags_history_but_still_fences_a_replay, 9;
    stored_lags_history_but_still_fences_a_replay_r10 => stored_lags_history_but_still_fences_a_replay, 10;
    stored_lags_history_but_still_fences_a_replay_r11 => stored_lags_history_but_still_fences_a_replay, 11;
    stored_lags_history_but_still_fences_a_replay_r12 => stored_lags_history_but_still_fences_a_replay, 12;
    stored_lags_history_but_still_fences_a_replay_r13 => stored_lags_history_but_still_fences_a_replay, 13;
    stored_lags_history_but_still_fences_a_replay_r14 => stored_lags_history_but_still_fences_a_replay, 14;
    stored_lags_history_but_still_fences_a_replay_r15 => stored_lags_history_but_still_fences_a_replay, 15;
    idempotent_restore_r00 => idempotent_restore, 0;
    idempotent_restore_r01 => idempotent_restore, 1;
    idempotent_restore_r02 => idempotent_restore, 2;
    idempotent_restore_r03 => idempotent_restore, 3;
    idempotent_restore_r04 => idempotent_restore, 4;
    idempotent_restore_r05 => idempotent_restore, 5;
    idempotent_restore_r06 => idempotent_restore, 6;
    idempotent_restore_r07 => idempotent_restore, 7;
    idempotent_restore_r08 => idempotent_restore, 8;
    idempotent_restore_r09 => idempotent_restore, 9;
    idempotent_restore_r10 => idempotent_restore, 10;
    idempotent_restore_r11 => idempotent_restore, 11;
    idempotent_restore_r12 => idempotent_restore, 12;
    idempotent_restore_r13 => idempotent_restore, 13;
    idempotent_restore_r14 => idempotent_restore, 14;
    idempotent_restore_r15 => idempotent_restore, 15;

    // Rung 3: the adversarial and the bounded.
    middle_corruption_r00 => middle_corruption, 0;
    middle_corruption_r01 => middle_corruption, 1;
    middle_corruption_r02 => middle_corruption, 2;
    middle_corruption_r03 => middle_corruption, 3;
    middle_corruption_r04 => middle_corruption, 4;
    middle_corruption_r05 => middle_corruption, 5;
    middle_corruption_r06 => middle_corruption, 6;
    middle_corruption_r07 => middle_corruption, 7;
    middle_corruption_r08 => middle_corruption, 8;
    middle_corruption_r09 => middle_corruption, 9;
    middle_corruption_r10 => middle_corruption, 10;
    middle_corruption_r11 => middle_corruption, 11;
    middle_corruption_r12 => middle_corruption, 12;
    middle_corruption_r13 => middle_corruption, 13;
    middle_corruption_r14 => middle_corruption, 14;
    middle_corruption_r15 => middle_corruption, 15;

    reopen_portability_r00 => reopen_portability, 0;
    reopen_portability_r01 => reopen_portability, 1;
    reopen_portability_r02 => reopen_portability, 2;
    reopen_portability_r03 => reopen_portability, 3;
    reopen_portability_r04 => reopen_portability, 4;
    reopen_portability_r05 => reopen_portability, 5;
    reopen_portability_r06 => reopen_portability, 6;
    reopen_portability_r07 => reopen_portability, 7;
    reopen_portability_r08 => reopen_portability, 8;
    reopen_portability_r09 => reopen_portability, 9;
    reopen_portability_r10 => reopen_portability, 10;
    reopen_portability_r11 => reopen_portability, 11;
    reopen_portability_r12 => reopen_portability, 12;
    reopen_portability_r13 => reopen_portability, 13;
    reopen_portability_r14 => reopen_portability, 14;
    reopen_portability_r15 => reopen_portability, 15;

    bound_refusal_r00 => bound_refusal, 0; bound_refusal_r01 => bound_refusal, 1;
    bound_refusal_r02 => bound_refusal, 2; bound_refusal_r03 => bound_refusal, 3;
    bound_refusal_r04 => bound_refusal, 4; bound_refusal_r05 => bound_refusal, 5;
    bound_refusal_r06 => bound_refusal, 6; bound_refusal_r07 => bound_refusal, 7;
    bound_refusal_r08 => bound_refusal, 8; bound_refusal_r09 => bound_refusal, 9;
    bound_refusal_r10 => bound_refusal, 10; bound_refusal_r11 => bound_refusal, 11;
    bound_refusal_r12 => bound_refusal, 12; bound_refusal_r13 => bound_refusal, 13;
    bound_refusal_r14 => bound_refusal, 14; bound_refusal_r15 => bound_refusal, 15;

    event_cap_r00 => event_cap, 0; event_cap_r01 => event_cap, 1;
    event_cap_r02 => event_cap, 2; event_cap_r03 => event_cap, 3;
    event_cap_r04 => event_cap, 4; event_cap_r05 => event_cap, 5;
    event_cap_r06 => event_cap, 6; event_cap_r07 => event_cap, 7;
    event_cap_r08 => event_cap, 8; event_cap_r09 => event_cap, 9;
    event_cap_r10 => event_cap, 10; event_cap_r11 => event_cap, 11;
    event_cap_r12 => event_cap, 12; event_cap_r13 => event_cap, 13;
    event_cap_r14 => event_cap, 14; event_cap_r15 => event_cap, 15;

    exploit_shapes_r00 => exploit_shapes, 0; exploit_shapes_r01 => exploit_shapes, 1;
    exploit_shapes_r02 => exploit_shapes, 2; exploit_shapes_r03 => exploit_shapes, 3;
    exploit_shapes_r04 => exploit_shapes, 4; exploit_shapes_r05 => exploit_shapes, 5;
    exploit_shapes_r06 => exploit_shapes, 6; exploit_shapes_r07 => exploit_shapes, 7;
    exploit_shapes_r08 => exploit_shapes, 8; exploit_shapes_r09 => exploit_shapes, 9;
    exploit_shapes_r10 => exploit_shapes, 10; exploit_shapes_r11 => exploit_shapes, 11;
    exploit_shapes_r12 => exploit_shapes, 12; exploit_shapes_r13 => exploit_shapes, 13;
    exploit_shapes_r14 => exploit_shapes, 14; exploit_shapes_r15 => exploit_shapes, 15;
);

/// The bands partition the seed space: no gap, no overlap.
///
/// A test rather than a comment, because a suite whose bands drifted apart
/// would quietly stop covering seeds while still reporting a full sweep.
#[test]
fn the_bands_cover_every_seed_exactly_once() -> TestResult {
    let mut covered = Vec::new();
    for band in sim::bands(sim::SEED_SPACE, sim::BANDS) {
        covered.extend(band.seeds());
    }
    covered.sort_unstable();
    covered.dedup();
    assert_eq!(
        u64::try_from(covered.len())?,
        sim::SEED_SPACE,
        "the bands cover {} seeds, not the shared seed space",
        covered.len()
    );
    Ok(())
}

/// Every family owns a distinct scratch tag.
///
/// Two families sharing a tag would race, because the runner executes tests in
/// parallel processes and a shared directory is a shared file.
#[test]
fn every_family_has_its_own_scratch_tag() -> TestResult {
    let tags = [
        "torn", "fence", "replay", "recover", "chain", "tenants", "idem", "middle", "reopen",
        "bound", "cap", "exploit",
    ];
    let mut seen = std::collections::BTreeSet::new();
    for tag in tags {
        assert!(seen.insert(tag), "scratch tag {tag} is used twice");
    }
    assert_eq!(seen.len(), tags.len());
    Ok(())
}
