//! T12: evidence carrying the wrong identity is refused, and refuses leave the
//! journal byte-identical.
//!
//! The row names seven identity fields and two properties: each wrong field is
//! refused without mutation, and a checked counter's exhaustion never aliases an
//! identity it has already issued.
//!
//! What existed is real but partial. `ecs_tick.rs` refuses a stale *binding*
//! (the action digest) and `durable_dispatch.rs` covers a readable journal with no
//! such outcome. Neither sweeps the seven fields, so nothing asserted that each
//! one is refused by the check that is *about* it rather than by an earlier check
//! that happens to catch it too.
//!
//! This file sweeps them. One held entry, one correct identity, and seven
//! deliveries — the correct one and six that each differ in exactly one field.
//! Each wrong delivery must be refused by the variant its field calls for, the
//! journal must not move, the action must not be re-entered, and the correct
//! delivery must still succeed. That last clause is what makes the sweep a test
//! of the identity checks: a blanket "refuse everything" passes five of the seven
//! assertions and fails that one.
//!
//! # What the refusals actually are, and why
//!
//! Asserting "an error" would be worthless here, because an earlier check often
//! catches the evidence first. The arms group by the *reason*, and the grouping
//! is the interesting output of writing the test:
//!
//! - An `ActionId` is derived from the bot's name, its position and its action's
//!   domain — **not** from the run, the flow revision or the environment. So a
//!   key that gets one of those three wrong still names an action this bot does
//!   declare, and is refused one level later: the ledger speaks for one run, and
//!   a key naming another is not its work.
//! - None of those four refusals is `NoSuchWork`. That variant means "there is
//!   no held effect at this address", and telling a caller that about work it
//!   *does* hold is the confusion the variant exists to prevent.
//!
//! The counter clause needs no bot and is asserted against the values the types
//! expose rather than by manufacturing `u64::MAX` appends.

/// The scratch and cleanup fixtures, shared with the journal families.
///
/// One home for "a path no other run will use" and "remove it when the test
/// ends", so this file cannot assert a different cleanup discipline than the
/// families that also observe real kills.
use crate::journal_fixtures as shared;

use shared::{TempGuard, scratch_dir};

/// The predicate this file's fixture verifications are written under.
const PREDICATE: &str = "4142434445464748494a4b4c4d4e4f50";
/// The whole ladder, which the file-journal row walks end to end.
const WHOLE_LADDER: usize = 4;

use crate::sim;

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::rc::Rc;

use lgwks_bot::effect::{
    ActionDigest, ActionId, AttemptId, EffectKey, EnvironmentEpoch, EnvironmentId, FlowRevision,
    RunId,
};
use lgwks_bot::journal::{
    EffectEvent, EffectJournal, EventKind, FileJournal, JournalError, JournalPosition,
    MemoryJournal,
};
use lgwks_bot::spec::{Bot, EffectEvidence as Settled, EffectIdentity};
use lgwks_bot::{BotError, GrantSet};

use sim::rig::{FixedSource, NeverSettles, RUN_NAME, is_value, recorded, scope};

/// What a row reports.
///
/// A `Result` rather than a panic because the workspace forbids the panicking
/// path everywhere, test code included: a row that unwound would report a line
/// number inside a macro rather than which of the facts it was checking had
/// moved.
type TestResult = Result<(), Box<dyn Error>>;

/// The environment this file's ledger acts on, as a parsed value.
fn environment() -> Result<EnvironmentId, Box<dyn Error>> {
    Ok(EnvironmentId::from_hex(sim::rig::ENV)?)
}

/// The run the ledger under test speaks for.
fn ledger_run() -> Result<RunId, Box<dyn Error>> {
    Ok(RunId::from_hex("0102030405060708090a0b0c0d0e0f10")?)
}

/// The flow revision the ledger under test was admitted under.
fn ledger_flow() -> Result<FlowRevision, Box<dyn Error>> {
    Ok(FlowRevision::from_tagged(
        "blake3_256",
        "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
    )?)
}

/// One of the seven identity fields a settlement can get wrong.
///
/// An enum rather than seven closures, because the row is about *which field*
/// disagreed and an assertion that only said "refused" would pass for a refusal
/// that named the wrong one. Each arm rewrites exactly one field of a correct
/// key, so a key this produces differs from the correct one in that field and
/// nowhere else — which is what makes the test discriminating rather than a
/// blanket "a wrong key is refused".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WrongField {
    /// A key whose run is not the ledger's.
    Run,
    /// A key whose action the ledger does not declare.
    Action,
    /// A key bound to a different payload than the live generation.
    Payload,
    /// A key admitted under a different flow revision.
    Revision,
    /// A key naming an environment the broker never issued.
    Environment,
    /// A key naming a different generation of the right environment.
    Epoch,
    /// A key naming a later attempt on the same entry.
    Attempt,
}

impl WrongField {
    /// Every arm, in a fixed order, so a family that sweeps them cannot reorder
    /// its draws and renumber another family's trace.
    const ALL: [Self; 7] = [
        Self::Run,
        Self::Action,
        Self::Payload,
        Self::Revision,
        Self::Environment,
        Self::Epoch,
        Self::Attempt,
    ];

    /// A stable tag for the trace, so a trace hash does not move when an arm's
    /// `Debug` spelling does.
    const fn tag(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Action => "action",
            Self::Payload => "payload",
            Self::Revision => "revision",
            Self::Environment => "environment",
            Self::Epoch => "epoch",
            Self::Attempt => "attempt",
        }
    }

    /// The rewrite this field means, applied to `held`.
    fn key(self, held: EffectKey) -> Result<EffectKey, Box<dyn Error>> {
        match self {
            Self::Run => replace_run(held),
            Self::Action => replace_action(held),
            Self::Payload => replace_digest(held),
            Self::Revision => replace_flow(held),
            Self::Environment => replace_environment(held),
            Self::Epoch => replace_epoch(held),
            Self::Attempt => replace_attempt(held),
        }
    }
}

/// The identity `key` was minted under: its run, environment and flow.
const fn identity_of(key: EffectKey) -> EffectIdentity {
    EffectIdentity::new(key.run(), key.environment(), key.flow())
}

/// `key`'s own attempt — its action, attempt, digest and epoch — minted under
/// `identity`, which is what a rewrite of the run or the flow asks for.
const fn under(identity: EffectIdentity, key: EffectKey) -> EffectKey {
    identity.key(key.action(), key.attempt(), key.digest(), key.epoch())
}

/// An action id this run's bots do not declare.
///
/// An action id is a digest over the bot's name, its position and its action's
/// domain, so no hex constant is *guaranteed* undeclared — a test that picked one
/// and got lucky would be asserting something for a reason it does not control.
/// The declared set here is one action and every sweep below asserts the
/// rewritten key differs from the held one before it asserts anything about the
/// refusal, so a future change to what the bot derives fails the row rather than
/// quietly making an arm test the held action a second time. The assertion, not
/// the constant, is what carries the guarantee.
const UNHELD_ACTION: &str = "ee00112233445566778899aabbccddee";

/// A key that differs from `key` in its run alone.
///
/// Every rewrite function is named after the enum arm it serves and documented
/// only by the field it moves: the arm's own doc already says which field that
/// is, and a second copy of the sentence is a second statement to keep in step.
fn replace_run(key: EffectKey) -> Result<EffectKey, Box<dyn Error>> {
    let run = RunId::from_hex("1112131415161718191a1b1c1d1e1f20")?;
    Ok(under(
        EffectIdentity::new(run, key.environment(), key.flow()),
        key,
    ))
}

/// A key that differs from `key` in its action alone.
fn replace_action(key: EffectKey) -> Result<EffectKey, Box<dyn Error>> {
    let action = ActionId::from_hex(UNHELD_ACTION)?;
    Ok(identity_of(key).key(action, key.attempt(), key.digest(), key.epoch()))
}

/// A key that differs from `key` in its payload digest alone.
fn replace_digest(key: EffectKey) -> Result<EffectKey, Box<dyn Error>> {
    let digest = ActionDigest::from_tagged(
        "blake3_256",
        "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e2f",
    )?;
    Ok(identity_of(key).key(key.action(), key.attempt(), digest, key.epoch()))
}

/// A key that differs from `key` in its flow revision alone.
fn replace_flow(key: EffectKey) -> Result<EffectKey, Box<dyn Error>> {
    let flow = FlowRevision::from_tagged(
        "blake3_256",
        "1f1e1d1c1b1a191817161514131211100f0e0d0c0b0a09080706050403020100",
    )?;
    Ok(under(
        EffectIdentity::new(key.run(), key.environment(), flow),
        key,
    ))
}

/// A key that differs from `key` in its environment alone.
fn replace_environment(key: EffectKey) -> Result<EffectKey, Box<dyn Error>> {
    let environment = EnvironmentId::from_hex("2122232425262728292a2b2c2d2e2f31")?;
    Ok(EffectIdentity::new(key.run(), environment, key.flow()).at_epoch(key, key.epoch()))
}

/// A key naming a different generation of the right environment.
///
/// A *fixed* generation would be wrong half the time: the held key is already at
/// whatever generation the broker minted, and writing `1` again would produce the
/// same key whenever that generation *is* one. Two generations past is always a
/// different value, and it is the stale direction the fence cares about — a key
/// from a generation the environment has been replaced past.
fn replace_epoch(key: EffectKey) -> Result<EffectKey, Box<dyn Error>> {
    let ahead = key
        .epoch()
        .checked_next()
        .and_then(EnvironmentEpoch::checked_next)
        .filter(|ahead| *ahead != key.epoch())
        .or_else(|| (key.epoch() != EnvironmentEpoch::FIRST).then_some(EnvironmentEpoch::FIRST))
        .ok_or("no generation differs from the held one")?;
    Ok(identity_of(key).at_epoch(key, ahead))
}

/// A key naming a later attempt on the same entry, and no other change.
fn replace_attempt(key: EffectKey) -> Result<EffectKey, Box<dyn Error>> {
    let attempt = key
        .attempt()
        .checked_next()
        .ok_or("an attempt counter with one value left cannot be advanced")?;
    Ok(key.with_attempt(attempt))
}

// ── The bot-shaped rows ─────────────────────────────────────────────────────

/// A bot whose one action is held as an unknown outcome, with its live binding.
///
/// One constructor for every scenario in this file, because all of them need the
/// same arrangement — a held entry, a live binding, and a journal holding only
/// the two write-ahead rungs — and a copy per row would be six places that
/// arrangement could drift out from under the assertions about it.
///
/// The counter is what makes "held, not retried" and "refused" distinguishable:
/// every row asserts the action's body did *not* run again, which a refusal that
/// re-entered it would satisfy by looking like a retry.
struct Held {
    /// The bot under test.
    bot: Bot,
    /// The journal its effects are recorded in.
    store: Rc<RefCell<MemoryJournal>>,
    /// The key that settles the held entry.
    binding: EffectKey,
    /// How many times the effect's body has run.
    entered: Rc<Cell<u32>>,
}

/// A held bot over `store`, and the binding its held entry reports.
///
/// The action is [`NeverSettles`], so the tick's own error is
/// `EffectIndeterminate` rather than a tick that fired: that is the state the
/// whole ledger exists for, and an entry that was merely undecided would not be
/// settleable at all.
fn held_under(store: Rc<RefCell<MemoryJournal>>) -> Result<Held, Box<dyn Error>> {
    let entered = Rc::new(Cell::new(0));
    let mut bot = Bot::builder(RUN_NAME)
        .observe(FixedSource)
        .on(
            is_value,
            NeverSettles {
                entered: Rc::clone(&entered),
            },
        )
        .with_effects(scope(
            EffectIdentity::new(ledger_run()?, environment()?, ledger_flow()?),
            Rc::clone(&store),
        )?)
        .build(&GrantSet::empty())?;
    match bot.tick() {
        Ok(fired) => {
            let refusal =
                Err(format!("an indeterminate effect was reported as {fired} fired").into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "held_under: returning an error to the caller");
            return refusal;
        }
        Err(error) => {
            if !matches!(error, BotError::EffectIndeterminate { .. }) {
                let refusal = Err(format!("expected the action's own error, got {error:?}").into());
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "held_under: returning an error to the caller");
                return refusal;
            }
        }
    }
    let binding = bot
        .pending()
        .first()
        .ok_or("the held entry must be reported")?
        .key()
        .ok_or("a held entry is held by an attempt, so it must carry its key")?;
    Ok(Held {
        bot,
        store,
        binding,
        entered,
    })
}

/// Every wrong identity field is refused, and the correct one is not.
///
/// The row's core, and the reason it is a sweep rather than six tests: one
/// delivery with the right identity, and six that each differ from it in exactly
/// one field. The correct delivery must succeed, so a blanket "refuse everything"
/// implementation fails this row; and each wrong one must be refused with the
/// variant that matches its field, so an implementation that refused everything
/// with `NoSuchWork` would also fail it.
///
/// The refusals are the *ledger's* here, and the fields fall into two groups. A
/// wrong **payload**, **revision**, **epoch** or **attempt** names work the
/// ledger does hold, so it can say precisely which: `EvidenceSuperseded` for the
/// generation and `EvidenceStaleAttempt` for the attempt, which is what lets a
/// caller decide whether to re-read the pending work or to send a different
/// report. A wrong **run** is not the ledger's work at all, and a wrong
/// **action** is not declared here, so both are refused before classification —
/// `NoSuchWork` with no address, and `ActionNotDeclared` with the action, which
/// is the most precise answer either can give. Asserting the exact variant per
/// field, rather than "an error", is what makes this a test of the identity
/// checks and not of the fact that keys are `Eq`.
#[test]
fn every_wrong_identity_field_is_refused_and_the_correct_one_is_not() -> TestResult {
    for wrong in WrongField::ALL {
        let held = held_under(Rc::new(RefCell::new(MemoryJournal::new())))?;
        let mut bot = held.bot;
        let before = recorded(&held.store)?;
        let submitted = wrong.key(held.binding)?;
        assert_ne!(
            submitted,
            held.binding,
            "{}: the submitted key must actually differ from the held one — an \
             arm that rewrites nothing is testing nothing",
            wrong.tag()
        );
        if wrong == WrongField::Action {
            assert_ne!(
                submitted.action(),
                held.binding.action(),
                "action: the rewrite must name a different action, not a \
                 different attempt under the same one"
            );
        }

        let outcome = bot.resolve_effect(&submitted, Settled::Applied);
        assert!(
            outcome.is_err(),
            "{}: evidence naming a different {} was accepted",
            wrong.tag(),
            wrong.tag()
        );
        assert_refusal_for(wrong, &outcome)?;
        assert_eq!(
            recorded(&held.store)?,
            before,
            "{}: the refusal wrote to the journal",
            wrong.tag()
        );
        assert_eq!(
            held.entered.get(),
            1,
            "{}: the refusal must not re-enter the action",
            wrong.tag()
        );
        assert_eq!(
            bot.pending().len(),
            1,
            "{}: the entry must still be held after the refusal",
            wrong.tag()
        );

        // And the correct identity, on the same bot, is accepted — which is what
        // makes the refusals above refusals rather than a door that is shut.
        bot.resolve_effect(&held.binding, Settled::Applied)?;
        assert!(
            bot.pending().is_empty(),
            "{}: the correct identity settled the entry, so the refusals above \
             were about the field and not about the door",
            wrong.tag()
        );
    }
    Ok(())
}

/// The refusal this file requires for one wrong field.
///
/// Asserted as a `match` over the typed variant rather than as a string, so a
/// change that renames a variant breaks the row instead of quietly widening what
/// it accepts. `&_` on the error, because the workspace forbids matching a
/// non-`Copy` payload by value out of a reference.
///
/// The pattern across the arms is the thing worth stating. An `ActionId` is
/// derived from the bot's name, its position and its action's domain — *not*
/// from the run, the flow revision or the environment — so the six fields fall
/// into three groups with three different honest answers:
///
/// - `Payload` describes a generation that is gone, and the refusal names both
///   bindings so the caller can re-read the pending work.
/// - `Epoch` names a generation the journal holds no ladder for, so the
///   settlement cannot be recorded at all and the refusal is `EffectUnrecorded`.
/// - `Run`, `Revision`, `Environment` and `Action` name work outside what this
///   ledger declares at all, and are refused as `ActionNotDeclared` — never as
///   `NoSuchWork`, which means "no held effect here" and would be a lie about
///   work that *is* held.
/// - `Attempt` describes an attempt that has been overtaken, and the refusal
///   names both attempt numbers.
///
/// A caller who wanted those four to answer uniformly would be asking for a
/// refusal that cannot name the field, which is the one thing the row forbids.
fn assert_refusal_for(wrong: WrongField, outcome: &Result<(), BotError>) -> TestResult {
    match (wrong, outcome.as_ref().err()) {
        (WrongField::Payload, Some(&BotError::EvidenceSuperseded { .. })) => {}
        // A key from a *later* generation is not merely reporting on gone work:
        // the journal has no ladder for it at all, so the settlement cannot even
        // be recorded. `EffectUnrecorded` is the honest answer — the caller
        // named an attempt whose intent was never admitted under that generation.
        (WrongField::Epoch, Some(&BotError::EffectUnrecorded { .. })) => {}
        (WrongField::Attempt, Some(&BotError::EvidenceStaleAttempt { .. })) => {}
        // Everything outside the identity this ledger speaks for. A key that
        // names another run, another flow revision or another environment is not
        // its work, and one that names an action it does not declare is not
        // addressable at all. All four land on `ActionNotDeclared`, and that is
        // the right answer rather than a limitation: the ledger's own arm says it
        // exists for "a key for an action this bot does not declare", and a key
        // from another run reaches that state because the two runs declare
        // different actions from the same bot name.
        //
        // `NoSuchWork` is *not* substituted here on purpose. It means "there is
        // no held effect at this address", and a caller told that about an action
        // it does hold work for is being told the wrong thing about work that is
        // still held. The variant that says "not declared" is the one a caller can
        // act on.
        (WrongField::Run, Some(&BotError::ActionNotDeclared { .. }))
        | (WrongField::Revision, Some(&BotError::ActionNotDeclared { .. }))
        | (WrongField::Environment, Some(&BotError::ActionNotDeclared { .. })) => {}
        // And the action itself, which the refusal must name so a caller can see
        // *which* action it was not declared.
        (WrongField::Action, Some(&BotError::ActionNotDeclared { .. })) => {}
        // Anything else is a failure of the row, reported through the caller's
        // own `Result` rather than through a panic, which this workspace forbids.
        (wrong, actual) => {
            let refusal = Err(format!(
                "{}: the refusal was {actual:?}, not the one this field must \
             produce",
                wrong.tag()
            )
            .into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "assert_refusal_for: returning an error to the caller");
            return refusal;
        }
    }
    Ok(())
}

/// The duplicate is idempotent and the contradiction is refused, so the ledger
/// distinguishes the two deliveries the row's callers depend on.
///
/// The control that stops a blanket "refuse everything" answer from passing the
/// row above: the same key with the same evidence twice is a *success* — a
/// caller whose first delivery it never saw land has to be able to repeat it —
/// and the same key with the other evidence is a *refusal* that leaves the
/// standing settlement alone.
#[test]
fn a_duplicate_is_idempotent_and_a_contradiction_is_refused() -> TestResult {
    let held = held_under(Rc::new(RefCell::new(MemoryJournal::new())))?;
    let mut bot = held.bot;
    bot.resolve_effect(&held.binding, Settled::Applied)?;
    let settled = recorded(&held.store)?;

    // The repeat. A delivery whose acknowledgement the caller never saw is the
    // ordinary case, not a caller mistake.
    bot.resolve_effect(&held.binding, Settled::Applied)?;
    assert_eq!(
        recorded(&held.store)?,
        settled,
        "a duplicate delivery wrote a second settlement"
    );

    // The contradiction.
    match bot.resolve_effect(&held.binding, Settled::NotApplied) {
        Err(BotError::EvidenceContradicted {
            settled: Settled::Applied,
            submitted: Settled::NotApplied,
            ..
        }) => {}
        other => {
            return Err(format!(
                "evidence contradicting a settlement must be refused as \
                 EvidenceContradicted, got {other:?}"
            )
            .into());
        }
    }
    assert_eq!(
        recorded(&held.store)?,
        settled,
        "the contradicted delivery wrote to the journal"
    );
    assert!(
        bot.pending().is_empty(),
        "the acknowledged generation is still acknowledged"
    );
    Ok(())
}

// ── T11: late evidence for an attempt that is over ──────────────────────────

/// Deliver `evidence` for `stale` and require the stale-attempt refusal that
/// names `stale`'s own attempt and the one outstanding, with nothing moved.
fn refused_as_stale(
    bot: &mut Bot,
    stale: EffectKey,
    outstanding: EffectKey,
    evidence: Settled,
) -> Result<(), Box<dyn Error>> {
    match bot.resolve_effect(&stale, evidence) {
        Err(BotError::EvidenceStaleAttempt {
            reported,
            outstanding: live,
            ..
        }) if reported == stale.attempt() && live == outstanding.attempt() => Ok(()),
        other => {
            let refusal = Err(format!(
                "late {evidence:?} for attempt {:?} must be refused as stale, naming it and the \
                 outstanding attempt {:?}; got {other:?}",
                stale.attempt(),
                outstanding.attempt()
            )
            .into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "refused_as_stale: returning an error to the caller");
            refusal
        }
    }
}

/// T11: A goes Unknown, is settled NotApplied, and its retry B goes Unknown;
/// A's evidence delivered late, whether a repeat of NotApplied or a
/// contradicting Applied, cannot alter B, and each refusal is attributed to A.
///
/// A and B are two attempts on one entry, which is the case the row is about:
/// NotApplied makes the entry eligible again, the retry is a new attempt with
/// its own key, and from then on A's evidence describes an attempt that is
/// over. Accepting the repeat as an idempotent success would be a lie about
/// which attempt it moved; accepting the Applied would retire B, an attempt
/// whose effect may be live, on evidence that was never about it.
///
/// Each is refused by the check that is about A. The repeat agrees with A's
/// durable record, so it is refused as `EvidenceStaleAttempt` carrying A's
/// attempt and B's. The Applied disagrees with that record, and the record is
/// keyed by A's attempt, so it is refused as `EvidenceContradicted` naming A's
/// `NotApplied`. Either way the journal does not move, B stays held under its
/// own key, and B's body is not entered again. The control is the last step:
/// B's own Applied, the same evidence A's refusal named as a contradiction,
/// still settles B, so the refusals were about A's key and not a frozen entry.
#[test]
fn late_evidence_for_a_settled_attempt_cannot_alter_its_retry_t11() -> TestResult {
    use lgwks_bot::journal::AttemptStatus;

    let held = held_under(Rc::new(RefCell::new(MemoryJournal::new())))?;
    let mut bot = held.bot;
    let attempt_a = held.binding;

    // A Unknown -> NotApplied: the entry is eligible for an attempt again.
    bot.resolve_effect(&attempt_a, Settled::NotApplied)?;
    // B Unknown: the retry runs the body once more and is held in turn.
    match bot.tick() {
        Err(BotError::EffectIndeterminate { .. }) => {}
        other => {
            let refusal =
                Err(format!("the retry must be held as indeterminate, got {other:?}").into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "late_evidence_for_a_settled_attempt_cannot_alter_its_retry_t11: returning an error to the caller");
            return refusal;
        }
    }
    let attempt_b = bot
        .pending()
        .first()
        .ok_or("the retry must be held")?
        .key()
        .ok_or("a held retry carries its key")?;
    assert_ne!(
        attempt_b.attempt(),
        attempt_a.attempt(),
        "the retry is a new attempt, not attempt A again"
    );
    assert_eq!(
        held.entered.get(),
        2,
        "the body ran for A and once more for B"
    );
    let before = recorded(&held.store)?;

    // Delayed A NotApplied: a repeat of A's own answer, about an attempt that is over.
    refused_as_stale(&mut bot, attempt_a, attempt_b, Settled::NotApplied)?;
    // Delayed A Applied: a contradiction of A's own durable record.
    match bot.resolve_effect(&attempt_a, Settled::Applied) {
        Err(BotError::EvidenceContradicted {
            settled: Settled::NotApplied,
            submitted: Settled::Applied,
            ..
        }) => {}
        other => {
            let refusal = Err(format!(
                "late Applied for attempt A must be refused as contradicting A's own NotApplied, \
                 got {other:?}"
            )
            .into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "late_evidence_for_a_settled_attempt_cannot_alter_its_retry_t11: returning an error to the caller");
            return refusal;
        }
    }

    assert_eq!(
        recorded(&held.store)?,
        before,
        "late evidence for attempt A wrote to the journal"
    );
    let still = bot.pending();
    assert_eq!(still.len(), 1, "B is still the one held entry");
    assert_eq!(
        still.first().and_then(|work| work.key()),
        Some(attempt_b),
        "and it is still held under B's key"
    );
    assert_eq!(held.entered.get(), 2, "no refusal re-entered the body");
    let recovered = held.store.borrow().recover();
    assert_eq!(
        recovered.status(attempt_a),
        Some(AttemptStatus::NotApplied),
        "A keeps its own settlement"
    );
    assert_eq!(
        recovered.status(attempt_b),
        Some(AttemptStatus::OutcomeUnknown),
        "B stays Unknown"
    );

    // The control: B's own evidence still settles B.
    bot.resolve_effect(&attempt_b, Settled::Applied)?;
    assert!(
        bot.pending().is_empty(),
        "B's own evidence retires the entry"
    );
    assert_eq!(
        held.store.borrow().recover().status(attempt_b),
        Some(AttemptStatus::Applied),
        "and the journal records it against B"
    );
    Ok(())
}

// ── The byte-identical half, on a real file ─────────────────────────────────

/// A refused settlement moves no byte of a real journal.
///
/// The other half of "without mutation". An in-memory `Vec` cannot show that a
/// refusal left a *device* alone, and the claim that matters is about the file a
/// crash would later replay: a refusal that wrote a partial frame, repaired a
/// torn tail, or moved the tail would change what a restart recovers even though
/// it "refused".
///
/// What refuses, and what does not, is worth stating because a test written
/// against the wrong expectation would be asserting the ladder does not do what
/// it does. The ordering ladder is **per key**: a key that differs in one
/// identity field is not on the ladder of the key the journal holds — it is at
/// the foot of its own, and admitting it is correct, because a distinct identity
/// is distinct work. What the journal refuses is a rung that cannot follow what
/// is committed *for that key*: a repeat, and anything at all on a completed
/// ladder. Both are asserted here, and both must leave the file byte-identical.
///
/// The ladder is not the settlement path the ledger rows above exercise; it is
/// the device's own ordering rule, and this row is about the device.
#[test]
fn a_refused_settlement_leaves_a_real_file_journal_byte_identical() -> TestResult {
    let dir = scratch_dir("t12-file")?;
    let _guard = TempGuard(dir.clone());
    let path = dir.join("journal.log");
    let mut journal = FileJournal::open(&path)?;
    let key = recorded_key()?;

    // A whole attempt on the real file, so the journal holds four committed
    // frames and the wrong-field deliveries have a completed ladder to sit
    // beside. The ladder itself is the shared support module's — the journal's
    // ordering rule, spelled once for every harness that walks it.
    shared::walk_ladder(&mut journal, key, PREDICATE, 1, WHOLE_LADDER)?;

    let completed_events = journal.committed()?;
    let completed_bytes = std::fs::read(&path)?;
    assert_eq!(
        completed_events.len(),
        WHOLE_LADDER,
        "the attempt this journal holds is complete, one rung at a time"
    );

    // Every refusal the ladder can produce for this key, and nothing written.
    // `Verified` is the one rung that is not refused after the ladder completes:
    // a later verification supersedes the verdict by design (#259), and the tests
    // in `durable_crash_observation` cover that acceptance, so it is not a
    // refusal to assert here.
    for event in shared::ladder(key, PREDICATE, 1)? {
        if event.kind() == EventKind::Verified {
            continue;
        }
        let tail = journal.tail();
        match journal.compare_and_append(tail, &event) {
            Err(JournalError::OutOfOrder { .. }) => {}
            other => {
                return Err(
                    format!("the completed ladder accepted another rung: {other:?}").into(),
                );
            }
        }
        assert_eq!(
            std::fs::read(&path)?,
            completed_bytes,
            "the refused append moved bytes on the device"
        );
    }

    // A key that differs in one identity field is at the foot of its own ladder,
    // and admitting it is *correct* — a distinct identity is distinct work. The
    // control: a test that asserted every delivery was refused would pass against
    // an implementation that refused to record work it had never seen.
    let mut admitted = 0usize;
    for wrong in WrongField::ALL {
        let submitted = wrong.key(key)?;
        assert_ne!(
            submitted,
            key,
            "{}: the wrong-field key must differ from the one the journal holds",
            wrong.tag()
        );
        let tail = journal.tail();
        journal.compare_and_append(tail, &EffectEvent::IntentAdmitted { key: submitted })?;
        admitted = admitted.saturating_add(1);
    }
    let after_admissions = std::fs::read(&path)?;
    assert_eq!(
        journal.committed()?.len(),
        completed_events.len().saturating_add(admitted),
        "each distinct identity was admitted at the foot of its own ladder"
    );

    // And now the second rung for each of *those*, which the ladder does refuse,
    // because the first already committed under the same key.
    for wrong in WrongField::ALL {
        let submitted = wrong.key(key)?;
        let tail = journal.tail();
        match journal.compare_and_append(tail, &EffectEvent::IntentAdmitted { key: submitted }) {
            Err(JournalError::OutOfOrder { .. }) => {}
            other => {
                return Err(format!(
                    "{}: a repeated admission for a key already holding one was \
                     accepted: {other:?}",
                    wrong.tag()
                )
                .into());
            }
        }
    }

    assert_eq!(
        std::fs::read(&path)?,
        after_admissions,
        "the repeated admissions moved bytes on the device"
    );
    assert!(
        after_admissions.starts_with(&completed_bytes),
        "and the completed attempt's bytes are still a literal prefix: a refusal \
         may only extend the file, never rewrite it"
    );

    // The chain still verifies, which is the byte-level claim's consequence: a
    // refusal that had corrupted a frame would fail here rather than passing an
    // equality check on a file nobody can reopen.
    assert!(
        lgwks_bot::journal::verify_chain(&journal.committed_entries()?).is_ok(),
        "every committed frame still follows from the ones before it"
    );

    drop(journal);
    Ok(())
}

/// The key the file row walks, built from the shared rig's own identity.
///
/// One constructor so the file row and the ledger rows are about the same action
/// family rather than about two actions that merely look similar, and so the
/// wrong-field rows and the ladder rows rewrite fields of one key rather than
/// two.
fn recorded_key() -> Result<EffectKey, Box<dyn Error>> {
    sim::rig::attempt_key(2)
}

// ── The counter clause ──────────────────────────────────────────────────────

/// A checked counter's exhaustion never aliases an identity it already issued.
///
/// The row's second property, and the one with no bot in it. [`AttemptId`] and
/// [`EnvironmentEpoch`] are `NonZeroU64` newtypes over a monotonic sequence, and
/// the failure mode this rules out is specific: a wrapped counter restarts at
/// [`AttemptId::FIRST`], which is a value this type has already handed out, and
/// a settlement arriving under it would compare equal to one from the beginning
/// of the run — so a genuine settlement would be refused as a duplicate. Running
/// out is recoverable; silently reusing an identity is not.
#[test]
fn a_checked_counter_exhaustion_never_aliases_an_issued_identity() -> TestResult {
    let one = std::num::NonZeroU64::new(1).ok_or("one is non-zero")?;
    let last = std::num::NonZeroU64::new(u64::MAX).ok_or("u64::MAX is non-zero")?;

    let last_attempt = AttemptId::new(last);
    assert!(
        last_attempt.is_exhausted(),
        "the last attempt reports itself spent"
    );
    assert_eq!(
        last_attempt.checked_next(),
        None,
        "the last attempt has no next one: a wrapped counter would hand out \
         AttemptId::FIRST, which this sequence already issued"
    );
    assert_ne!(
        last_attempt.checked_next(),
        Some(AttemptId::new(one)),
        "exhaustion must not alias the first identity the sequence issued"
    );
    // And the value *before* the last still advances normally, which is what
    // makes exhaustion a boundary rather than a wall.
    let penultimate = AttemptId::new(std::num::NonZeroU64::new(u64::MAX - 1).ok_or("non-zero")?);
    assert_eq!(
        penultimate.checked_next(),
        Some(last_attempt),
        "the attempt before the last still advances to it"
    );
    assert!(!penultimate.is_exhausted(), "and is not itself exhausted");

    // The generation counter: the same refusal, the same reason, and a different
    // role name so a refusal message names the counter that ran out.
    let last_generation = EnvironmentEpoch::new(last);
    assert!(
        last_generation.is_exhausted(),
        "the last generation is spent"
    );
    assert_eq!(
        last_generation.checked_next(),
        None,
        "a wrapped generation would re-authorize commands the broker already \
         fenced, which is the one thing the counter exists to prevent"
    );
    assert_eq!(
        AttemptId::role(),
        "attempt",
        "and each counter names which sequence it is"
    );
    assert_eq!(
        EnvironmentEpoch::role(),
        "environment epoch",
        "so a refusal about exhaustion names the right one"
    );
    Ok(())
}

/// The journal's position counter is a pair, and a wrap would be detectable
/// only because of the head.
///
/// The part of the counter clause that reaches the journal. A position is
/// `(sequence, head)` precisely so that a wrapped sequence — which would make
/// two positions compare equal and defeat the guard the sequence exists to
/// provide — is detectable: the head changes with every append, so a position
/// that reappeared the same sequence could not also reappear the same head.
///
/// `u64::MAX` appends cannot be driven here, so the exhaustion refusal is
/// asserted as what it *is* — a named variant on the append path — rather than
/// by manufacturing the state. What is proved is the shape that makes the wrap
/// detectable, and that the refusal exists rather than a silent wrap.
#[test]
fn a_journal_position_carries_a_head_so_a_wrapped_sequence_is_detectable() -> TestResult {
    let mut journal = MemoryJournal::new();
    let genesis = journal.tail();
    assert_eq!(
        genesis,
        JournalPosition::genesis(),
        "an empty journal's tail is the genesis"
    );

    // Exhaustion is reachable and named: the variant exists on the append path
    // and says what was spent rather than reading as a storage failure.
    assert_eq!(
        JournalError::Exhausted.to_string(),
        "journal position exhausted",
        "the refusal names itself rather than reading as a storage failure"
    );

    let tail = journal.tail();
    let first = journal.compare_and_append(
        tail,
        &EffectEvent::IntentAdmitted {
            key: sim::rig::attempt_key(1)?,
        },
    )?;
    let after = journal.tail();
    assert_eq!(
        after.sequence(),
        genesis.sequence().saturating_add(1),
        "the first append moves the sequence by one, so it never reuses the \
         genesis value"
    );
    assert_ne!(
        after.head(),
        genesis.head(),
        "and the head moves with it, so a repeated sequence could not be \
         mistaken for the same position"
    );
    assert_eq!(
        first.position(),
        after,
        "the acknowledgment names the tail the journal then held"
    );
    Ok(())
}

/// A settlement carrying another run's identity is refused, and the ledger it was
/// refused by is unchanged.
///
/// The run clause of T12, on its own. An `ActionId` is derived from the bot's
/// name, its position and its action's domain — **not** from the run — so two
/// ledgers of the same shape under the same bot name declare the *same* action,
/// and "two ledgers" cannot be built by differing the run alone. What can be, and
/// is here, is one ledger offered a settlement whose key names another run: the
/// ledger speaks for exactly one run, and a key that names another is refused
/// before it can be classified against any entry.
///
/// The refusal is `ActionNotDeclared` rather than `NoSuchWork`, and the row says
/// so explicitly because the two mean opposite things to a caller. `NoSuchWork`
/// is "there is no held effect at this address"; telling a caller that about work
/// it *does* hold — which this entry is — is the confusion the variant exists to
/// prevent. The journal must not move, the action must not be re-entered, and the
/// correct identity must still settle, which is what makes the refusal a refusal
/// and not a closed door.
///
/// Not claimed: two ledgers over one shared journal in one process. The ledger's
/// actions are derived from the bot's structure, so two of them with the same
/// name are the same ledger as far as the identity model is concerned, and a row
/// that claimed cross-ledger isolation here would be claiming something the type
/// does not distinguish.
#[test]
fn a_settlement_carrying_another_runs_identity_is_refused() -> TestResult {
    let store = Rc::new(RefCell::new(MemoryJournal::new()));
    let mut held = held_under(Rc::clone(&store))?;
    let before = recorded(&store)?;

    let foreign = replace_run(held.binding)?;
    assert_ne!(
        foreign, held.binding,
        "the foreign key must be a different identity, or the row proves nothing"
    );
    assert_ne!(
        foreign.run(),
        held.binding.run(),
        "and it must be the *run* that differs, since the action is shared"
    );

    match held.bot.resolve_effect(&foreign, Settled::Applied) {
        Err(BotError::ActionNotDeclared { .. }) => {}
        other => {
            return Err(format!(
                "a settlement naming another run must be refused as not declared: \
                 {other:?}"
            )
            .into());
        }
    }

    assert_eq!(
        recorded(&store)?,
        before,
        "the refused settlement wrote to the journal"
    );
    assert_eq!(
        held.entered.get(),
        1,
        "the refusal must not re-enter the action"
    );
    assert_eq!(
        held.bot.pending().len(),
        1,
        "the entry is still held after the refusal"
    );

    // And the correct identity still settles, which is what makes the refusal
    // about the field rather than about the door.
    held.bot.resolve_effect(&held.binding, Settled::Applied)?;
    assert!(
        held.bot.pending().is_empty(),
        "the correct identity settled the entry"
    );
    Ok(())
}
