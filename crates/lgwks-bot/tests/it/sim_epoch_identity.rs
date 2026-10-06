//! Seeded refusals of a takeover and of a wrong identity: two facts this branch
//! claims, each swept across every seed a band cuts.
//!
//! Both are properties of a *decision made from an identity*, and a single point
//! would pass for a check that happened to answer its own case. A sweep fails for
//! the one seed whose generation, field or order the check was not written for.
//!
//! - **INV-BOT-56**: an owner epoch is a fact on the disk, not a constant each
//!   process chooses. `Broker::register` starts an environment at generation one
//!   whatever the journal holds, while `Broker::adopt` reads the generation the
//!   journal's own committed history was written at and claims the one after it —
//!   so a process adopting another worker's journal cannot mint warrants for a
//!   generation that worker was already replaced past. A warrant from the
//!   replaced generation is `Superseded`; the claimed generation still mints.
//! - **INV-BOT-58**: each identity field is refused by the check that is *about*
//!   it. Seven deliveries — one correct and six differing in exactly one field —
//!   are refused as six distinct typed variants, because asserting "an error"
//!   would be satisfied by an earlier check that happened to catch the evidence
//!   first. A refused delivery moves no byte, and the correct one still settles.
//!
//! # What is real and what is virtual
//!
//! The real `FileJournal`, the real `Broker`, the real `Bot` and the real
//! refusals. Only the seed is virtualised: it draws which identity field is
//! wrong, the takeover order, and how many attempts the journal carries. Nothing
//! here reads a key value into the trace — only tags — so a seed replays to the
//! same hash.

#![cfg(all(feature = "script", feature = "ephemeral"))]

use crate::sim;

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::rc::Rc;

use lgwks_bot::broker::{Broker, BrokerError};
use lgwks_bot::effect::{
    ActionDigest, ActionId, EffectKey, EnvironmentEpoch, EnvironmentId, FlowRevision, RunId,
};
use lgwks_bot::journal::{EffectEvent, EffectJournal, FileJournal, MemoryJournal};
use lgwks_bot::spec::{Bot, EffectEvidence as Settled};
use lgwks_bot::{BotError, GrantSet};

use sim::rig::{
    self, FixedSource, NeverSettles, RUN_NAME, attempt_key, attempt_key_at, is_value, recorded,
    scope,
};

type TestResult = Result<(), Box<dyn Error>>;

// ── INV-BOT-58: each identity field is refused by the check about it ─────────

/// How many identity fields a seed draws from.
///
/// The draw is an index rather than an enum arm, and the two tables below are
/// indexed the same way, so the field a seed rewrote and the refusal it expects
/// cannot be reordered against each other by a `match` rewritten in isolation —
/// which is the failure a set of parallel arms invites.
const FIELDS: u32 = 7;

/// The stable tag of each identity field, in draw order.
///
/// What the trace records, so a hash does not move when a variant's `Debug`
/// spelling does.
const FIELD_TAGS: [&str; 7] = [
    "run",
    "action",
    "payload",
    "revision",
    "environment",
    "epoch",
    "attempt",
];

/// The refusal each field calls for, as a stable tag, in the same draw order.
///
/// The expectation is stated here rather than read back from what the bot
/// produced, so a check that stopped discriminating would fail against this table
/// rather than against itself. An `ActionId` is derived from the bot's name, its
/// position and its action's domain — *not* from the run, the flow revision or
/// the environment — so those four still name a declared action and are refused
/// one level later as `ActionNotDeclared`. A changed payload and a later attempt
/// describe work the ledger does hold, so they are classified precisely
/// (`EvidenceSuperseded`, `EvidenceStaleAttempt`), and a generation the journal
/// holds no ladder for cannot be recorded at all (`EffectUnrecorded`).
const FIELD_REFUSALS: [&str; 7] = [
    "action-not-declared",
    "action-not-declared",
    "evidence-superseded",
    "action-not-declared",
    "action-not-declared",
    "effect-unrecorded",
    "evidence-stale-attempt",
];

/// A run this ledger does not speak for.
const OTHER_RUN: &str = "00112233445566778899aabbccddeeff";
/// An action this bot does not declare.
const OTHER_ACTION: &str = "0102030405060708090a0b0c0d0e0f11";
/// An environment the broker never issued.
const OTHER_ENVIRONMENT: &str = "3132333435363738393a3b3c3d3e3f41";
/// A flow revision this run was not admitted under.
const OTHER_FLOW: &str = "100f0e0d0c0b0a09080706050403020100112233445566778899aabbccddeeff";
/// A payload binding this generation does not hold.
const OTHER_DIGEST: &str = "ffeeddccbbaa9988776655443322110000112233445566778899aabbccddeeff";

/// The tag of the field a seed's index names.
///
/// A seed names a field the table declares, so this is total by construction: the
/// index is the field number the caller already holds, and a tag for a field this
/// table does not name would be a lie the trace would then assert by name.
fn field_tag(field: u32) -> &'static str {
    match usize::try_from(field) {
        Ok(index) => match FIELD_TAGS.get(index) {
            Some(tag) => *tag,
            None => unmapped(field, FIELD_TAGS.len()),
        },
        Err(_too_wide) => outside(field, FIELD_TAGS.len()),
    }
}

/// The tag for a field this table leaves out on purpose.
///
/// The identity has seven fields and the tables name the ones a seed may change,
/// so "unmapped" is a fact about the table rather than a failure — and it is
/// named as itself because a trace that asserted `unknown` for it would be
/// asserting something nobody wrote.
fn unmapped(field: u32, len: usize) -> &'static str {
    match u32::try_from(len) {
        Ok(index) if index == field => "unmapped-field",
        _ => outside(field, len),
    }
}

/// The tag for a field index this table could not hold at all.
fn outside(field: u32, len: usize) -> &'static str {
    match u32::try_from(len) {
        Ok(_) => "field-outside-the-table",
        // The table is wider than the field numbering can reach, so no field
        // index is outside it; the tag says so rather than guessing which case
        // this is.
        Err(_) => "field-table-wider-than-the-field-numbering",
    }
}

/// The refusal tag the field a seed's index names calls for.
/// The refusal tag the field a seed's index names, under the same rule.
fn field_refusal(field: u32) -> &'static str {
    match usize::try_from(field) {
        Ok(index) => match FIELD_REFUSALS.get(index) {
            Some(tag) => *tag,
            None => unmapped(field, FIELD_REFUSALS.len()),
        },
        Err(_too_wide) => outside(field, FIELD_REFUSALS.len()),
    }
}

/// The held key with exactly the `field`th field changed.
///
/// One `EffectKey::new` for the whole file: the seven near-identical ones this
/// replaced could disagree about which field each was rewriting, which is exactly
/// the distinction the assertions rest on. The two counters advance rather than
/// reset, so they cannot alias a value the run has already issued.
fn rewrite(field: u32, held: EffectKey) -> Result<EffectKey, Box<dyn Error>> {
    let mut run = held.run();
    let mut action = held.action();
    let mut attempt = held.attempt();
    let mut flow = held.flow();
    let mut digest = held.digest();
    let mut environment = held.environment();
    let mut epoch = held.epoch();
    let other_run = RunId::from_hex(OTHER_RUN)?;
    let other_action = ActionId::from_hex(OTHER_ACTION)?;
    let other_digest = ActionDigest::from_tagged("blake3_256", OTHER_DIGEST)?;
    let other_flow = FlowRevision::from_tagged("blake3_256", OTHER_FLOW)?;
    let other_environment = EnvironmentId::from_hex(OTHER_ENVIRONMENT)?;
    match field {
        0 => run = other_run,
        1 => action = other_action,
        2 => digest = other_digest,
        3 => flow = other_flow,
        4 => environment = other_environment,
        5 => {
            epoch = epoch
                .checked_next()
                .ok_or("the generation cannot be advanced past its ceiling")?;
        }
        6 => {
            attempt = attempt
                .checked_next()
                .ok_or("the attempt cannot be advanced past its ceiling")?;
        }
        other => {
            let refusal = Err(format!("field {other} is not an identity field").into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "rewrite: returning an error to the caller");
            return refusal;
        }
    }
    Ok(EffectKey::new(
        run,
        action,
        attempt,
        flow,
        digest,
        environment,
        epoch,
    ))
}

/// The refusal the `field`th rewrite must produce, asserted as a typed variant.
///
/// A `match` over the typed variant rather than a string, so a renamed variant
/// breaks the row instead of quietly widening what it accepts.
fn refused_as(field: u32, outcome: &Result<(), BotError>) -> TestResult {
    match (field, outcome.as_ref().err()) {
        (2, Some(&BotError::EvidenceSuperseded { .. })) => {}
        (5, Some(&BotError::EffectUnrecorded { .. })) => {}
        (6, Some(&BotError::EvidenceStaleAttempt { .. })) => {}
        (0 | 1 | 3 | 4, Some(&BotError::ActionNotDeclared { .. })) => {}
        (field, actual) => {
            let refusal = Err(format!(
                "{}: the refusal was {actual:?}, not the one this field must produce",
                field_tag(field)
            )
            .into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "refused_as: returning an error to the caller");
            return refusal;
        }
    }
    Ok(())
}

/// Hold one entry indeterminate, refuse a wrong-field delivery, and settle the
/// right one — recorded into `sim`'s trace.
///
/// The bot is the shared rig's: a `FixedSource` fires one value into a
/// `NeverSettles` action, so the tick leaves the entry held with an unknown
/// outcome and a key. The seed's `field` is rewritten from that key, and the
/// delivery must be refused by the check that is about that field, move no byte,
/// re-enter no action, and leave the entry held — after which the *correct* key
/// settles, which is what makes the refusals refusals rather than a shut door.
fn identity_case(sim: &mut sim::Sim, field: u32) -> TestResult {
    let entered = Rc::new(Cell::new(0));
    let store = Rc::new(RefCell::new(MemoryJournal::new()));
    let mut bot = Bot::builder(RUN_NAME)
        .observe(FixedSource)
        .on(
            is_value,
            NeverSettles {
                entered: Rc::clone(&entered),
            },
        )
        .with_effects(scope(rig::identity()?, Rc::clone(&store))?)
        .build(&GrantSet::empty())?;
    match bot.tick() {
        Err(BotError::EffectIndeterminate { .. }) => {}
        Err(other) => {
            let refusal = Err(format!("expected the action's own error, got {other:?}").into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "identity_case: returning an error to the caller");
            return refusal;
        }
        Ok(fired) => {
            {
                let refusal =
                    Err(format!("an indeterminate effect was reported as {fired} fired").into());
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "identity_case: returning an error to the caller");
                return refusal;
            };
        }
    }
    let held = bot
        .pending()
        .first()
        .ok_or("the held entry must be reported")?
        .key()
        .ok_or("a held entry is held by an attempt, so it must carry its key")?;
    let before = recorded(&store)?;

    let submitted = rewrite(field, held)?;
    assert_ne!(
        submitted,
        held,
        "{}: the submitted key must actually differ from the held one — an arm that rewrites nothing is testing nothing",
        field_tag(field)
    );
    let outcome = bot.resolve_effect(&submitted, Settled::Applied);
    assert!(
        outcome.is_err(),
        "{}: evidence naming a different field was accepted",
        field_tag(field)
    );
    refused_as(field, &outcome)?;
    assert_eq!(
        recorded(&store)?,
        before,
        "{}: the refusal wrote to the journal",
        field_tag(field)
    );
    assert_eq!(
        entered.get(),
        1,
        "{}: the refusal must not re-enter the action",
        field_tag(field)
    );
    assert_eq!(
        bot.pending().len(),
        1,
        "{}: the entry must still be held after the refusal",
        field_tag(field)
    );

    // And the correct identity, on the same bot, is accepted.
    bot.resolve_effect(&held, Settled::Applied)?;
    assert!(
        bot.pending().is_empty(),
        "{}: the correct identity did not settle the entry",
        field_tag(field)
    );

    sim.record(field_tag(field));
    sim.record(field_refusal(field));
    Ok(())
}

/// Each identity field is refused by the check that is about it, and the correct
/// one is not.
///
/// The row's core, swept rather than stated at one point. The seed draws which of
/// the seven fields is wrong, so the band covers every arm; each wrong delivery
/// must be refused with the variant its field calls for, and the correct delivery
/// must still succeed — which is what a blanket "refuse everything" fails.
fn every_identity_field_is_refused_by_its_own_check(band: sim::Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let field = sim.rng().below(FIELDS);
        sim.trace.record_u64("field", u64::from(field));
        identity_case(sim, field)
    })
}

// ── INV-BOT-56: an owner epoch is a fact on the disk ─────────────────────────

/// The environment both brokers in a scenario act on.
fn environment() -> Result<EnvironmentId, Box<dyn Error>> {
    Ok(EnvironmentId::from_hex(sim::rig::ENV)?)
}

/// The generation one, the first a fresh broker mints.
fn first_generation() -> Result<EnvironmentEpoch, Box<dyn Error>> {
    Ok(EnvironmentEpoch::from_decimal("1")?)
}

/// The generation after one, the one an adopted journal of generation-one
/// history is claimed at.
fn second_generation() -> Result<EnvironmentEpoch, Box<dyn Error>> {
    Ok(EnvironmentEpoch::from_decimal("2")?)
}

/// One takeover order, in the sequence the scenario performs it.
///
/// The seed draws an arm so that "which order the process did open, takeover and
/// append in" is a swept dimension rather than an assumption: an adopt on an empty
/// journal is refused rather than silently claiming generation one, and a
/// re-adopt on a broker that already owns the environment is refused rather than
/// renumbering it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Order {
    /// Append the history, then take it over.
    AppendThenAdopt,
    /// Try to take over an empty journal, then append, then take it over.
    AdoptBeforeAppend,
    /// Append, take over, append at the new generation, and try to take over again.
    AppendAdoptAppendAdopt,
}

impl Order {
    /// The three arms, in a fixed order.
    const ALL: [Self; 3] = [
        Self::AppendThenAdopt,
        Self::AdoptBeforeAppend,
        Self::AppendAdoptAppendAdopt,
    ];

    /// The arm a seed's draw names.
    fn drawn(sim: &mut sim::Sim) -> Self {
        // The draw is bounded by the table's own length, so the match is total
        // for every seed; the arm past the end reports which it was rather than
        // silently running the first order, which is the arm this family would
        // then measure as if it had been chosen.
        match usize::try_from(sim.rng().below(3))
            .ok()
            .and_then(|index| Self::ALL.get(index))
        {
            Some(arm) => *arm,
            None => Self::ALL[Self::ALL.len().saturating_sub(1)],
        }
    }

    /// The tag the trace records for this order.
    const fn tag(self) -> &'static str {
        match self {
            Self::AppendThenAdopt => "append-then-adopt",
            Self::AdoptBeforeAppend => "adopt-before-append",
            Self::AppendAdoptAppendAdopt => "append-adopt-append-adopt",
        }
    }
}

/// Take over a journal another worker wrote, in a seeded order, and prove the
/// generation is the disk's fact rather than a constant.
///
/// `register` is the control: it mints generation one whatever the journal holds,
/// so a process that adopted with it would fence nothing. `adopt` reads the
/// generation the journal's own history was written at and claims the one after
/// it, so a warrant from the replaced generation is `Superseded` while the
/// claimed one still mints. The order the history and the takeover happen in is
/// the arm, and each arm asserts what its order makes true.
fn epoch_case(sim: &mut sim::Sim, order: Order, attempts: u32) -> TestResult {
    let scratch = sim.scratch("sim-epoch")?;
    let path = scratch.join("journal.log");
    let mut journal = FileJournal::open(&path)?;
    let environment = environment()?;

    // A fresh broker, which is what a worker starting up against an existing
    // journal would otherwise do — and which mints generation one regardless.
    let mut fresh = Broker::new();
    let armed = fresh.register(environment)?;
    assert_eq!(
        armed,
        first_generation()?,
        "register starts an environment at generation one whatever the journal holds"
    );

    let mut adopter = Broker::new();
    if order == Order::AdoptBeforeAppend {
        match adopter.adopt(environment, &journal) {
            Err(BrokerError::NothingToAdopt { .. }) => {}
            other => {
                let refusal = Err(format!(
                "adopting an empty journal must be refused, not silently claim generation one: {other:?}"
                )
                .into());
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "epoch_case: returning an error to the caller");
                return refusal;
            }
        }
    }

    // The history the previous owner wrote, entirely at generation one.
    for attempt in 1..=u64::from(attempts) {
        let key = attempt_key(attempt)?;
        let tail = journal.tail();
        journal.compare_and_append(tail, &EffectEvent::IntentAdmitted { key })?;
    }

    let taken = adopter.adopt(environment, &journal)?;
    assert_eq!(
        taken,
        second_generation()?,
        "adoption claims the generation after the history's own, not generation one"
    );
    assert_eq!(
        adopter.epoch(environment),
        Some(taken),
        "the adopter holds the generation it claimed"
    );
    assert_eq!(
        fresh.epoch(environment),
        Some(armed),
        "register holds the generation it minted"
    );
    assert_ne!(
        armed, taken,
        "a takeover must move the generation, or nothing below is fenced"
    );

    // A warrant from the replaced generation is refused, naming both generations.
    let stale = attempt_key(1)?;
    match adopter.authorize(stale) {
        Err(BrokerError::Superseded {
            presented, current, ..
        }) => {
            assert_eq!(
                presented, armed,
                "the refusal names the generation presented"
            );
            assert_eq!(current, taken, "and the one the broker holds now");
        }
        other => {
            let refusal = Err(format!(
                "a warrant from the replaced generation must be Superseded, got {other:?}"
            )
            .into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "epoch_case: returning an error to the caller");
            return refusal;
        }
    }

    // ...while the claimed generation still mints authority for its own attempt.
    let current = attempt_key_at(2, 1)?;
    adopter.authorize(current)?;

    if order == Order::AppendAdoptAppendAdopt {
        let tail = journal.tail();
        journal.compare_and_append(tail, &EffectEvent::IntentAdmitted { key: current })?;
        match adopter.adopt(environment, &journal) {
            Err(BrokerError::AlreadyRegistered { .. }) => {}
            other => {
                let refusal = Err(format!(
                    "a second adoption on one broker must be refused, got {other:?}"
                )
                .into());
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "epoch_case: returning an error to the caller");
                return refusal;
            }
        }
    }

    // The fresh broker still mints at generation one: register never read the
    // disk, which is the two processes that are each internally consistent and
    // jointly wrong.
    fresh.authorize(stale)?;

    sim.record(order.tag());
    sim.trace.record_u64("attempts", u64::from(attempts));
    Ok(())
}

/// A seeded takeover order answers the same generation question every time.
fn seeded_takeover_orders_keep_the_generation_on_the_disk(band: sim::Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let order = Order::drawn(sim);
        let attempts = sim.rng().between(1, 4);
        epoch_case(sim, order, attempts)
    })
}

// ── The replay receipt ───────────────────────────────────────────────────────

/// The same seed produces the same trace, and a different seed a different one.
///
/// The replay receipt, asserted directly as well as through
/// [`sim::assert_replays`] so the two halves are separate failures.
fn same_seed_same_trace_hash(band: sim::Band) -> TestResult {
    let one_run = |sim: &mut sim::Sim| -> TestResult {
        let field = sim.rng().below(FIELDS);
        identity_case(sim, field)?;
        let order = Order::drawn(sim);
        let attempts = sim.rng().between(1, 4);
        epoch_case(sim, order, attempts)
    };
    sim::assert_replays(band, one_run)?;

    let first = sim::sweep(sim::Band::new(1, 1), one_run)?;
    let second = sim::sweep(sim::Band::new(2, 1), one_run)?;
    assert_ne!(
        first, second,
        "two seeds produced the same trace hash, so the hash is not a receipt of anything this scenario decided"
    );
    Ok(())
}

// The tests below are written out rather than declared through `band_family!`,
// and that is deliberate: the gate that counts simulation evidence reads `#[test]`
// attributes out of the sources, so a family declared through a macro is real,
// runs, and is invisible to it. Each one names the band it sweeps.

#[test]
fn every_identity_field_is_refused_by_its_own_check_band_00() -> TestResult {
    every_identity_field_is_refused_by_its_own_check(sim::band_of(0))
}

#[test]
fn every_identity_field_is_refused_by_its_own_check_band_01() -> TestResult {
    every_identity_field_is_refused_by_its_own_check(sim::band_of(1))
}

#[test]
fn every_identity_field_is_refused_by_its_own_check_band_02() -> TestResult {
    every_identity_field_is_refused_by_its_own_check(sim::band_of(2))
}

#[test]
fn every_identity_field_is_refused_by_its_own_check_band_03() -> TestResult {
    every_identity_field_is_refused_by_its_own_check(sim::band_of(3))
}

#[test]
fn seeded_takeover_orders_keep_the_generation_on_the_disk_band_04() -> TestResult {
    seeded_takeover_orders_keep_the_generation_on_the_disk(sim::band_of(4))
}

#[test]
fn seeded_takeover_orders_keep_the_generation_on_the_disk_band_05() -> TestResult {
    seeded_takeover_orders_keep_the_generation_on_the_disk(sim::band_of(5))
}

#[test]
fn seeded_takeover_orders_keep_the_generation_on_the_disk_band_06() -> TestResult {
    seeded_takeover_orders_keep_the_generation_on_the_disk(sim::band_of(6))
}

#[test]
fn seeded_takeover_orders_keep_the_generation_on_the_disk_band_07() -> TestResult {
    seeded_takeover_orders_keep_the_generation_on_the_disk(sim::band_of(7))
}

#[test]
fn same_seed_same_trace_hash_band_08() -> TestResult {
    same_seed_same_trace_hash(sim::band_of(8))
}

#[test]
fn same_seed_same_trace_hash_band_09() -> TestResult {
    same_seed_same_trace_hash(sim::band_of(9))
}
