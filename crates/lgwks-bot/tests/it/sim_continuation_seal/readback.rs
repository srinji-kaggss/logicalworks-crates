//! What a reopen reads back: the streamed replay, every acknowledged position, and two interleaved tenants that never cross.

use super::*;

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
