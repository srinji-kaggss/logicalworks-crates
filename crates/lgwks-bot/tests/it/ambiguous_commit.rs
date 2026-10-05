//! The public journey for an ambiguous post-effect append (issue #118, item 1).
//!
//! `EffectJournal::compare_and_append` has three outcomes, and the middle one is
//! the subject here: an append that *may* have committed while its
//! acknowledgment was lost, reported as `JournalError::OutcomeUnknown`. A
//! wrapper that committed and then returned a generic error would violate the
//! trait's stated contract, so the fault injected below is the conforming one —
//! the record is in the store and the reply is gone.
//!
//! What a controller must do is not resend: read the committed record back,
//! reconcile the same evidence against it, and settle. These tests drive the
//! shipped public surface — `Bot`, `EffectScope`, `Bot::resolve_effect` and
//! `Bot::pending` — so a change that broke the reconciliation breaks them first.
//!
//! The unit tests inside `ecs.rs` prove the same decisions against a journal the
//! crate owns; this file proves the journey a downstream host actually walks, in
//! the exact package/feature environment a consumer compiles in. The
//! store-sharing adapter lives in `sim::rig`, beside `ProbeJournal`, so its
//! forwarding to the shipped ladder is written once.

use std::cell::Cell;
use std::error::Error;
use std::rc::Rc;

use lgwks_bot::journal::{EffectEvent, EffectEvidence};
use lgwks_bot::spec::{Bot, EffectScope};
use lgwks_bot::{BotError, DispatchCertainty, GrantSet};

use crate::sim;

use sim::rig::{
    AlwaysLands, FixedSource, LostReplyJournal, RUN_NAME, broker, identity, is_value, recorded,
};

/// What a test reports when `Box<dyn Error>` is not enough.
type TestResult = Result<(), Box<dyn Error>>;

/// A bot over `journal`, armed with an action that lands and counts entries.
fn bot_with(journal: LostReplyJournal, entered: Rc<Cell<u32>>) -> Result<Bot, Box<dyn Error>> {
    let identity = identity()?;
    let environment = identity.environment();
    Ok(Bot::builder(RUN_NAME)
        .observe(FixedSource)
        .on(is_value, AlwaysLands { entered })
        .with_effects(EffectScope::new(
            identity,
            broker(environment)?,
            Box::new(journal),
        ))
        .build(&GrantSet::empty())?)
}

/// The effect happened, the record landed, and the reply was lost: the tick
/// settles from the readback instead of stalling or resending.
#[test]
fn a_committed_outcome_with_a_lost_reply_settles_without_resending() -> TestResult {
    let journal = LostReplyJournal::new();
    let ambiguous_once = Rc::clone(&journal.ambiguous_once);
    let store = Rc::clone(&journal.store);
    let entered = Rc::new(Cell::new(0));
    let mut bot = bot_with(journal, Rc::clone(&entered))?;

    ambiguous_once.set(true);
    assert_eq!(bot.tick()?, 1, "the one chain must dispatch its one action");
    assert_eq!(entered.get(), 1, "the external action ran exactly once");
    assert!(
        bot.pending().is_empty(),
        "a record provably in the journal is settled work, not an ambiguity to hold"
    );

    let committed = recorded(&store)?;
    assert!(
        matches!(
            committed.last(),
            Some(EffectEvent::OutcomeObserved {
                evidence: EffectEvidence::Applied,
                ..
            })
        ),
        "the store must hold the outcome whose reply was lost: {committed:?}"
    );
    let key = committed.first().ok_or("the attempt was recorded")?.key();

    // The public reconciliation: a repeat of the same evidence is the same
    // fact, answered from the committed record, and it does not re-enter the
    // action.
    bot.resolve_effect(&key, EffectEvidence::Applied)?;
    assert_eq!(
        bot.tick()?,
        0,
        "a settled transition dispatches nothing more"
    );
    assert_eq!(entered.get(), 1, "a repeat settlement must not resend");
    assert!(bot.pending().is_empty());
    Ok(())
}

/// The reply was lost and nothing landed: the caller is told the occurrence is
/// certain, and the retry appends the record without ever re-running the action.
#[test]
fn an_unknown_outcome_that_did_not_commit_reports_occurrence_and_records_on_retry() -> TestResult {
    let journal = LostReplyJournal::new();
    let unknown_once = Rc::clone(&journal.unknown_once);
    let store = Rc::clone(&journal.store);
    let entered = Rc::new(Cell::new(0));
    let mut bot = bot_with(journal, Rc::clone(&entered))?;

    unknown_once.set(true);
    match bot.tick() {
        Err(error @ BotError::EffectUnrecorded { .. }) => {
            assert_eq!(
                error.dispatch_certainty(),
                DispatchCertainty::Occurred,
                "the effect ran; only its record's reply was lost: {error:?}"
            );
        }
        other => return Err(format!("expected a lost reply, got {other:?}").into()),
    }
    assert_eq!(entered.get(), 1);
    assert!(
        !recorded(&store)?
            .iter()
            .any(|event| matches!(event, EffectEvent::OutcomeObserved { .. })),
        "an unknown outcome without a commit must leave the ladder free"
    );

    assert_eq!(
        bot.tick()?,
        1,
        "the record lands on the recovery tick and counts as fired"
    );
    assert_eq!(
        entered.get(),
        1,
        "a lost reply must never re-run the action"
    );
    assert!(bot.pending().is_empty());
    assert!(matches!(
        recorded(&store)?.last(),
        Some(EffectEvent::OutcomeObserved {
            evidence: EffectEvidence::Applied,
            ..
        })
    ));
    Ok(())
}
