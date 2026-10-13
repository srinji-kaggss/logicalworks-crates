//! The seal at its limits: the widest checkpoint both bounds admit, and a lengthened event behind a carried seal.

use super::*;

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
