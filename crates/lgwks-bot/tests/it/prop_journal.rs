//! Property tests over journal recovery, with shrinking (#273, the target
//! #261 left as NOT DONE).
//!
//! The input is a history of *proposed* appends: any key from a small pool,
//! any rung of the ladder, in any order, some of them against a stale tail.
//! The real [`MemoryJournal`] decides which land. Whatever it decides, the
//! properties below must hold, and a failing history shrinks to the shortest
//! one that still fails. One fixed seed drives every run, so a run replays
//! exactly; a failure's seed is written under `proptest-regressions/` and is
//! replayed first on every later run with no environment variable.

use crate::journal_fixtures as shared;

use crate::prop;

use std::error::Error;

use lgwks_bot::effect::{EffectKey, Id128};
use lgwks_bot::journal::{
    AttemptStatus, EffectEvent, EffectEvidence, EffectJournal as _, FileJournal, JournalPosition,
    MemoryJournal, Verification, VerificationResult, recover,
};
use proptest::collection::vec;
use proptest::prelude::Strategy;
use proptest::test_runner::{TestCaseError, TestRunner};

use prop::{Outcome, check, setup};

/// The seed every run starts from. Changing it is a new corpus, not a retry.
const SEED: u64 = 0x2730_c0de_c5ee_d002;

/// Attempts in play. Few enough that keys collide and ladders interleave.
const KEYS: u64 = 4;

/// One proposed append: which attempt, which rung, and whether it is offered
/// against a stale tail.
#[derive(Debug, Clone, Copy)]
struct Proposal {
    /// The attempt id: ids start at 1, and 0 is refused when the key is built.
    key: u64,
    /// 0 intent, 1 dispatch, 2 applied, 3 not applied, 4 verified, 5 failed
    /// verification. Any rung may be proposed at any time.
    rung: u8,
    /// Offer the append against the genesis tail rather than the real one.
    stale: bool,
}

/// Histories up to 40 proposals long; one in eight is offered stale.
fn histories() -> impl Strategy<Value = Vec<Proposal>> {
    let proposal = (1..=KEYS, 0u8..6, proptest::bool::weighted(0.125))
        .prop_map(|(key, rung, stale)| Proposal { key, rung, stale });
    vec(proposal, 0..40)
}

/// A runner for this target's properties.
fn runner(cases: u32) -> TestRunner {
    prop::runner(SEED, cases, file!())
}

/// The event a proposal describes.
fn event(proposal: Proposal) -> Result<EffectEvent, Box<dyn Error>> {
    let key = shared::key(proposal.key)?;
    let verified = |result| -> Result<EffectEvent, Box<dyn Error>> {
        Ok(EffectEvent::Verified {
            key,
            verification: Verification::new(
                Id128::from_hex(shared::ACTION)?,
                1,
                lgwks_std::hash::blake3(b"prop_journal"),
                result,
            ),
        })
    };
    Ok(match proposal.rung {
        0 => EffectEvent::IntentAdmitted { key },
        1 => EffectEvent::DispatchPrepared { key },
        2 => EffectEvent::OutcomeObserved {
            key,
            evidence: EffectEvidence::Applied,
        },
        3 => EffectEvent::OutcomeObserved {
            key,
            evidence: EffectEvidence::NotApplied,
        },
        4 => verified(VerificationResult::Satisfied)?,
        _ => verified(VerificationResult::NotSatisfied)?,
    })
}

/// Offer every proposal to a fresh memory journal, checking at each step that
/// a refused append changed nothing. Returns the events that landed.
fn replay(history: &[Proposal]) -> Result<(MemoryJournal, Vec<EffectEvent>), TestCaseError> {
    let mut journal = MemoryJournal::new();
    let mut landed = Vec::new();
    for &proposal in history {
        let event = setup(event(proposal))?;
        let before = journal.recover();
        let tail = journal.tail();
        let offered = if proposal.stale {
            JournalPosition::genesis()
        } else {
            tail
        };
        if journal.compare_and_append(offered, &event).is_ok() {
            landed.push(event);
        } else {
            check(
                journal.recover() == before && journal.tail() == tail,
                || format!("the refused {event} changed the journal"),
            )?;
        }
    }
    Ok((journal, landed))
}

/// What an attempt's last landed event says about it, independently of the
/// fold under test: the rule is "the newest fact wins".
///
/// An event or result this oracle does not know is a failed case, never a
/// guess: the generator only proposes the rungs below, so one appearing means
/// the ladder grew and the oracle must be taught it.
fn oracle(landed: &[EffectEvent]) -> Result<Vec<(EffectKey, AttemptStatus)>, TestCaseError> {
    let mut out: Vec<(EffectKey, AttemptStatus)> = Vec::new();
    for landed_event in landed {
        let status = match *landed_event {
            EffectEvent::IntentAdmitted { .. } => AttemptStatus::Prepared,
            EffectEvent::DispatchPrepared { .. } => AttemptStatus::OutcomeUnknown,
            EffectEvent::OutcomeObserved {
                evidence: EffectEvidence::NotApplied,
                ..
            } => AttemptStatus::NotApplied,
            EffectEvent::OutcomeObserved {
                evidence: EffectEvidence::Applied,
                ..
            } => AttemptStatus::Applied,
            EffectEvent::Verified { verification, .. } => match verification.result() {
                VerificationResult::Satisfied => AttemptStatus::Verified,
                VerificationResult::NotSatisfied => AttemptStatus::VerificationFailed,
                _ => {
                    lgwks_std::trace::debug!(%landed_event, "oracle: an unknown verification result");
                    return Err(TestCaseError::fail(format!(
                        "the oracle does not know the verification in {landed_event}"
                    )));
                }
            },
            _ => {
                lgwks_std::trace::debug!(%landed_event, "oracle: an unknown event");
                return Err(TestCaseError::fail(format!(
                    "the oracle does not know the event {landed_event}"
                )));
            }
        };
        let key = landed_event.key();
        match out.iter_mut().find(|&&mut (seen, _)| seen == key) {
            Some(entry) => entry.1 = status,
            None => out.push((key, status)),
        }
    }
    Ok(out)
}

/// A recovery fold's answer, as (key, status) in report order.
type Fold = fn(&[EffectEvent]) -> Vec<(EffectKey, AttemptStatus)>;

/// The shipped fold.
fn shipped(events: &[EffectEvent]) -> Vec<(EffectKey, AttemptStatus)> {
    recover(events)
        .attempts()
        .iter()
        .map(|attempt| (attempt.key(), attempt.status()))
        .collect()
}

/// The recovery properties, over the events a history landed.
///
/// The fold agrees with [`oracle`] and is pure (the same events fold to the
/// same answer); an attempt whose last fact is a prepared dispatch is
/// uncertain, never `NotApplied`; and `uncertain()` lists exactly those.
fn recovery_property(fold: Fold, history: &[Proposal]) -> Result<(), TestCaseError> {
    let (journal, landed) = replay(history)?;
    let answer = fold(&landed);
    let expected = oracle(&landed)?;
    check(answer == expected, || {
        format!("folded {answer:?}, expected {expected:?}")
    })?;
    check(fold(&landed) == answer, || {
        String::from("the fold is not pure")
    })?;
    for &(key, status) in &answer {
        let dispatched_last = landed
            .iter()
            .rev()
            .find(|landed_event| landed_event.key() == key)
            .is_some_and(|last| matches!(*last, EffectEvent::DispatchPrepared { .. }));
        check(
            !dispatched_last || status == AttemptStatus::OutcomeUnknown,
            || format!("{key:?} was dispatched with no outcome and recovered as {status}"),
        )?;
    }
    let uncertain: Vec<EffectKey> = answer
        .iter()
        .filter(|&&(_, status)| status == AttemptStatus::OutcomeUnknown)
        .map(|&(key, _)| key)
        .collect();
    check(journal.recover().uncertain() == uncertain, || {
        format!("uncertain() disagrees with the fold: {uncertain:?}")
    })
}

#[test]
fn recovery_is_the_newest_fact_per_attempt_whatever_the_order() -> Outcome {
    runner(512).run(&histories(), |history| recovery_property(shipped, &history))?;
    Ok(())
}

#[test]
fn a_reopened_file_journal_recovers_what_was_written() -> Outcome {
    // Fewer cases: each one opens, writes, syncs and reopens a real file.
    runner(64).run(&histories(), |history| {
        let (memory, landed) = replay(&history)?;
        let path = shared::scratch("prop-reopen");
        let _guard = shared::TempGuard(path.clone());
        let mut written = setup(FileJournal::open(&path))?;
        if !landed.is_empty() {
            setup(written.compare_and_append_all(&landed))?;
        }
        drop(written);
        let reopened = setup(FileJournal::open(&path))?;
        check(reopened.recover() == memory.recover(), || {
            format!(
                "reopened {:?}, written {:?}",
                reopened.recover(),
                memory.recover()
            )
        })?;
        check(reopened.events().count() == landed.len(), || {
            String::from("the reopened journal holds a different number of events")
        })
    })?;
    Ok(())
}

#[test]
fn the_property_catches_a_fold_that_reads_silence_as_not_applied() -> Outcome {
    /// Treats a dispatch with no outcome as one that did not land: the
    /// optimistic reading that sends a non-idempotent effect twice.
    fn optimistic(events: &[EffectEvent]) -> Vec<(EffectKey, AttemptStatus)> {
        shipped(events)
            .into_iter()
            .map(|(key, status)| match status {
                AttemptStatus::OutcomeUnknown => (key, AttemptStatus::NotApplied),
                other => (key, other),
            })
            .collect()
    }
    let minimal = prop::shrunk(SEED, &histories(), |history| {
        recovery_property(optimistic, &history)
    })?;
    // Which attempt it is does not matter, so the shrinker may leave any key;
    // the shape is one attempt admitted, then dispatched, both current.
    let shape: Vec<(u8, bool)> = minimal
        .iter()
        .map(|proposal| (proposal.rung, proposal.stale))
        .collect();
    let one_attempt = minimal
        .windows(2)
        .all(|pair| matches!(pair, [first, second] if first.key == second.key));
    assert_eq!(
        (shape, one_attempt),
        (vec![(0, false), (1, false)], true),
        "the smallest history that exposes it: admit, then dispatch: {minimal:?}"
    );
    Ok(())
}
