//! Black-box acceptance for the durable half of a dispatch: what is written
//! down before an effect leaves the process, and what a reconstructed bot does
//! with the record it finds.
//!
//! Three facts are pinned here, and none of them was observable from outside
//! the crate before this file existed.
//!
//! - **Write-ahead.** The journal holds `IntentAdmitted` and `DispatchPrepared`
//!   for an attempt *before* the action's own body runs. That is the whole of
//!   RQ-007, and the only vantage point from which it can be measured is the
//!   effect itself: an assertion made after the tick returns cannot tell
//!   "recorded before the handoff" from "recorded instead of the handoff".
//! - **A recovered unknown is held, not retried.** A bot rebuilt against a
//!   journal that records a dispatch with no outcome refuses to begin another
//!   attempt on that action, reports it through
//!   [`Bot::pending`](lgwks_bot::spec::Bot::pending) with the key to settle it
//!   by, and does not enter the action. Nothing established that the bytes did
//!   not arrive, so sending them again is how a merge is duplicated.
//! - **A recovered attempt is not re-minted.** The journal's ladder allows one
//!   walk of `IntentAdmitted → DispatchPrepared → OutcomeObserved → Verified`
//!   per key, so an attempt the journal already records cannot be minted a
//!   second time under the same identity. A restart therefore continues the
//!   attempt sequence rather than restarting it, and an attempt whose effect is
//!   recorded as landed is retired instead of re-dispatched.
//!
//! # Why these tests cannot pass on the tree before the change
//!
//! They are not weakened versions of something that already ran: on the
//! pre-change tree there is no journal on the dispatch path at all, no
//! `EffectScope` to hand one over with, and no key on a pending report. This
//! file does not compile against it. That is the honest statement of what it
//! would have caught, and it is why the assertions below are on the effect's
//! own observations rather than on what a tick returned.

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::rc::Rc;

use lgwks_bot::broker::{Broker, DispatchError};
use lgwks_bot::effect::{AttemptId, EffectKey, EnvironmentId, FlowRevision, RunId};
use lgwks_bot::journal::{
    DurabilityPromise, DurableAck, EffectEvent, EffectJournal, EventKind, JournalEntry,
    JournalError, JournalPosition, MemoryJournal,
};
use lgwks_bot::spec::{Bot, EffectEvidence, EffectIdentity, EffectScope, TransitionHold};
use lgwks_bot::{
    Auth, BotError, Cap, DispatchCertainty, EffectLifetime, EventId, Execute, GrantSet, Observe,
};

/// What a test reports when its precondition did not hold.
///
/// A scope crosses `IdError` and `BrokerError` as well as `BotError`, and none
/// of the three converts into another, so a test that builds one reports
/// through `Box<dyn Error>` and names the failure rather than flattening it
/// into a variant it is not.
type TestResult = Result<(), Box<dyn Error>>;

/// The name every bot in this file is built under.
///
/// A constant rather than a literal per call site because an action's identity
/// is derived from the bot's name as well as from its position: two bots whose
/// names differ declare different actions, and the restart test's whole subject
/// is that the second bot resolves the first one's keys.
const NAME: &str = "durable-dispatch";

/// The source's value on every poll. Constant, so a source that moves is never
/// what a test is accidentally measuring.
const VALUE: u32 = 1;

// ── The store a test can read from inside an effect ────────────────────────

/// A journal that shares its store with the test.
///
/// An [`EffectScope`] owns its journal by value, so the only way a test can
/// read the record at the instant an effect runs is for the journal to hand out
/// a handle to the same store. That is not a test-only trick: `EffectJournal`
/// is implementable outside the crate, and an adapter holding a store it did
/// not allocate is exactly what a host with a real durable backend writes.
///
/// Every method forwards to [`MemoryJournal`] rather than reimplementing the
/// ladder, so what these tests exercise is the shipped append logic and not a
/// second copy of it that could drift.
struct ProbeJournal {
    /// The store, shared with the test and with any action the test arms.
    store: Rc<RefCell<MemoryJournal>>,
}

impl EffectJournal for ProbeJournal {
    fn durability(&self) -> DurabilityPromise {
        self.store.borrow().durability()
    }

    fn tail(&self) -> JournalPosition {
        self.store.borrow().tail()
    }

    fn committed(&self) -> Result<Vec<EffectEvent>, JournalError> {
        EffectJournal::committed(&*self.store.borrow())
    }

    fn committed_entries(&self) -> Result<Vec<JournalEntry>, JournalError> {
        Ok(self.store.borrow().committed().to_vec())
    }

    fn compare_and_append(
        &mut self,
        expected_tail: JournalPosition,
        event: &EffectEvent,
    ) -> Result<DurableAck, JournalError> {
        self.store
            .borrow_mut()
            .compare_and_append(expected_tail, event)
    }
}

/// Every event the store has committed, in order.
///
/// Read through the trait rather than through [`MemoryJournal`]'s own
/// `committed`, which returns the entries and shadows it. What these tests are
/// about is what an [`EffectJournal`] promises, so the trait is the way in: a
/// test that read the concrete type would keep passing if the trait's promise
/// changed.
fn recorded(store: &Rc<RefCell<MemoryJournal>>) -> Result<Vec<EffectEvent>, JournalError> {
    EffectJournal::committed(&*store.borrow())
}

// ── The run ────────────────────────────────────────────────────────────────

/// The three run-level facts every bot here is built against.
///
/// One definition rather than one per call site, because the restart test's
/// subject is that two bots agree on them: a second copy that drifted would
/// make the second bot's recovered key foreign, and the test would then pass
/// for the reason it exists to exclude.
fn identity() -> Result<EffectIdentity, Box<dyn Error>> {
    Ok(EffectIdentity::new(
        RunId::from_hex("0102030405060708090a0b0c0d0e0f10")?,
        EnvironmentId::from_hex("2122232425262728292a2b2c2d2e2f30")?,
        FlowRevision::from_tagged(
            "blake3_256",
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
        )?,
    ))
}

/// The broker for a run acting on `identity`'s environment, at its first
/// generation.
///
/// Fresh per bot rather than carried across a restart, which is what a restart
/// is: the host registers the environment again and the broker hands out the
/// generation it hands the first caller. A recovered key therefore still
/// fences — its epoch is the one a fresh registration mints — which is the
/// property the hold in these tests depends on.
fn broker(environment: EnvironmentId) -> Result<Broker, Box<dyn Error>> {
    let mut broker = Broker::new();
    broker.register(environment)?;
    Ok(broker)
}

/// A scope over `store`, for a bot acting on `identity`.
fn scope(
    identity: EffectIdentity,
    store: Rc<RefCell<MemoryJournal>>,
) -> Result<EffectScope, Box<dyn Error>> {
    Ok(EffectScope::new(
        identity,
        broker(identity.environment())?,
        Box::new(ProbeJournal { store }),
    ))
}

// ── Sources, conditions and actions ───────────────────────────────────────

/// A source that yields the same value on every poll.
///
/// The first poll still moves the observed revision, from "never polled" to
/// one, so the one chain fires once and a second tick selects nothing.
struct FixedSource;

impl Observe for FixedSource {
    type Output = u32;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        call.0.check(Observe::required_caps(self))?;
        Ok(VALUE)
    }

    fn domain_id(&self) -> &str {
        "test::fixed_source"
    }
}

/// `true` for the one value [`FixedSource`] yields.
fn is_value(seen: &u32) -> bool {
    *seen == VALUE
}

/// An action that records what the journal held at the instant it ran.
///
/// This is the write-ahead claim measured from the effect's own vantage point:
/// the record must be there *before* this body executes, not merely before the
/// tick returns. The body reads the shared store rather than a copy, so what it
/// snapshots is the kernel's own journal at the moment the effect is live.
struct NoteWhatIsRecorded {
    /// The journal's store, read synchronously at the moment of the effect.
    store: Rc<RefCell<MemoryJournal>>,
    /// What the journal held, once this body has run.
    seen: Rc<RefCell<Option<Vec<EventKind>>>>,
    /// The key those events were about.
    key: Rc<RefCell<Option<EffectKey>>>,
}

impl Execute for NoteWhatIsRecorded {
    type Input = u32;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
        call.0.check(Execute::required_caps(self))?;
        let journal = self.store.borrow();
        let events: Vec<EffectEvent> = journal.events().copied().collect();
        *self.seen.borrow_mut() = Some(events.iter().map(|event| event.kind()).collect());
        *self.key.borrow_mut() = events.first().map(|event| event.key());
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::note_what_is_recorded"
    }
}

/// An action that always reports the outcome as unknown, and counts how many
/// times it was entered.
///
/// The counter is the whole instrument: "held, not retried" and "exhausted its
/// budget" both leave a pending report behind, and only the number of times the
/// body ran distinguishes them.
struct NeverSettles {
    /// How many times the body has been entered.
    entered: Rc<Cell<u32>>,
}

impl Execute for NeverSettles {
    type Input = u32;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
        call.0.check(Execute::required_caps(self))?;
        self.entered.set(self.entered.get().saturating_add(1));
        Err(BotError::EffectIndeterminate {
            domain: "test::never_settles".to_owned(),
            cause: "the acknowledgement never arrived".to_owned(),
        })
    }

    fn domain_id(&self) -> &str {
        "test::never_settles"
    }
}

/// An action that succeeds, and counts how many times it was entered.
struct AlwaysLands {
    /// How many times the body has been entered.
    entered: Rc<Cell<u32>>,
}

impl Execute for AlwaysLands {
    type Input = u32;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
        call.0.check(Execute::required_caps(self))?;
        self.entered.set(self.entered.get().saturating_add(1));
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::always_lands"
    }
}

// ── The tests ──────────────────────────────────────────────────────────────

/// The intent and the prepared dispatch are both committed before the effect
/// runs, and the outcome is committed after it.
///
/// The ordering is asserted from inside the action rather than after the tick,
/// because that is what makes the claim mean "before the handoff". A journal
/// written after the effect returned would satisfy an after-the-tick assertion
/// just as well, and would be no record at all: the crash it is meant to
/// survive is the one that happens while the bytes are in flight.
#[test]
fn the_dispatch_intent_is_recorded_before_the_effect_leaves_the_process() -> TestResult {
    let store = Rc::new(RefCell::new(MemoryJournal::new()));
    let seen = Rc::new(RefCell::new(None));
    let key = Rc::new(RefCell::new(None));

    let mut bot = Bot::builder(NAME)
        .observe(FixedSource)
        .on(
            is_value,
            NoteWhatIsRecorded {
                store: Rc::clone(&store),
                seen: Rc::clone(&seen),
                key: Rc::clone(&key),
            },
        )
        .with_effects(scope(identity()?, Rc::clone(&store))?)
        .build(&GrantSet::empty())?;

    let fired = bot.tick()?;
    assert_eq!(fired, 1, "the one chain must dispatch its one action");

    let at_handoff_seen = seen
        .borrow()
        .clone()
        .ok_or("the action never ran, so the write-ahead claim was never measured")?;
    assert_eq!(
        at_handoff_seen.as_slice(),
        [EventKind::IntentAdmitted, EventKind::DispatchPrepared],
        "at the instant the effect ran, the journal must already hold the intent and the \
         prepared dispatch for it, and nothing else: an append that landed later would be \
         no record of the crash this exists to survive"
    );

    let at_handoff = key
        .borrow()
        .ok_or("the journal held no event to take a key from")?;
    let after = recorded(&store)?;
    assert_eq!(
        after.len(),
        3,
        "the outcome is committed after the handoff, so the run must end with exactly one \
         more event than the action saw: {after:?}"
    );
    assert_eq!(after[0].key(), at_handoff);
    assert_eq!(after[1].key(), at_handoff);
    assert_eq!(after[2].key(), at_handoff);
    assert_eq!(
        after[2].kind(),
        EventKind::OutcomeObserved,
        "the third append is what the attempt became, and it is the only one that may \
         follow the handoff"
    );
    assert!(
        matches!(
            after[2],
            EffectEvent::OutcomeObserved {
                evidence: EffectEvidence::Applied,
                ..
            }
        ),
        "an action that returned Ok landed, and the journal must say so: {:?}",
        after[2]
    );
    Ok(())
}

/// A bot rebuilt against a journal refuses to begin an attempt the journal
/// records as dispatched with no outcome, and reports it instead.
///
/// The counter is the assertion that matters. A substrate that had retried
/// would leave the same pending report behind — the entry is held either way —
/// so "how many times did the effect run" is the only observable that
/// separates a hold from a resend.
#[test]
fn a_restart_holds_an_uncertain_attempt_rather_than_resending_it() -> TestResult {
    let store = Rc::new(RefCell::new(MemoryJournal::new()));
    let entered = Rc::new(Cell::new(0));
    let identity = identity()?;

    let mut first = Bot::builder(NAME)
        .observe(FixedSource)
        .on(
            is_value,
            NeverSettles {
                entered: Rc::clone(&entered),
            },
        )
        .with_effects(scope(identity, Rc::clone(&store))?)
        .build(&GrantSet::empty())?;

    match first.tick() {
        Err(BotError::EffectIndeterminate { .. }) => {}
        other => {
            return Err(format!(
                "an effect that may or may not have happened must be reported as \
                 indeterminate; the tick returned {other:?}"
            )
            .into());
        }
    }
    assert_eq!(
        entered.get(),
        1,
        "the action is entered once, on the first attempt"
    );

    let at_rest = recorded(&store)?;
    assert_eq!(
        at_rest.len(),
        2,
        "an indeterminate effect records no outcome, so the journal holds exactly the \
         intent and the prepared dispatch: {at_rest:?}"
    );
    let key = at_rest[0].key();
    assert_eq!(
        at_rest[1].key(),
        key,
        "both events are about the one attempt"
    );

    // The restart: same identity, same journal, a new broker and a new bot.
    let mut second = Bot::builder(NAME)
        .observe(FixedSource)
        .on(
            is_value,
            NeverSettles {
                entered: Rc::clone(&entered),
            },
        )
        .with_effects(scope(identity, Rc::clone(&store))?)
        .build(&GrantSet::empty())?;

    let refused = second.tick();
    match refused {
        Err(BotError::PendingTransition { work, outstanding }) => {
            assert_eq!(
                outstanding, 1,
                "the one held entry must be counted, and nothing else must be reported \
                 as open"
            );
            assert_eq!(
                work.key(),
                Some(key),
                "the refusal must name the key the caller settles by; a report that named \
                 the entry alone would leave the caller deriving an identity the journal \
                 already has"
            );
            assert!(
                matches!(
                    work.hold(),
                    TransitionHold::UnsettledByRecovery { action } if *action == key.action()
                ),
                "the refusal must say the entry is held because an earlier attempt never \
                 settled, rather than reading as a fresh attempt or as an exhausted \
                 budget: {:?}",
                work.hold()
            );
        }
        other => {
            return Err(format!(
                "a tick that finds a recovered unknown must refuse and name it, not \
                 dispatch it or report a clean tick; it returned {other:?}"
            )
            .into());
        }
    }
    assert_eq!(
        entered.get(),
        1,
        "the effect body must not have been entered a second time: a resend is the \
         duplicate merge this refusal exists to prevent"
    );

    let held = second.pending();
    assert_eq!(
        held.len(),
        1,
        "the held attempt must be reported, not silently absorbed: {held:?}"
    );
    assert_eq!(
        held[0].key(),
        Some(key),
        "the report a caller acts on must name the same key the tick refused with"
    );
    assert!(
        matches!(
            held[0].hold(),
            TransitionHold::UnsettledByRecovery { action } if *action == key.action()
        ),
        "the pending report must agree with the refusal about why the entry is held: {:?}",
        held[0].hold()
    );
    Ok(())
}

/// Evidence that the effect did not land lets a recovered attempt be tried
/// again, under a new attempt identity.
///
/// Without this, `resolve_effect`'s `NotApplied` would be a promise the durable
/// path cannot keep: the entry would be "eligible for an attempt again" and the
/// attempt would then be refused, because a restart that mints attempt one
/// again reproduces a key the journal has already walked past. The journal
/// allows one walk of its ladder per key, so continuing a run means continuing
/// its attempt sequence.
#[test]
fn evidence_that_the_effect_did_not_land_lets_a_recovered_attempt_be_tried_again() -> TestResult {
    let store = Rc::new(RefCell::new(MemoryJournal::new()));
    let entered = Rc::new(Cell::new(0));
    let identity = identity()?;

    let mut first = Bot::builder(NAME)
        .observe(FixedSource)
        .on(
            is_value,
            NeverSettles {
                entered: Rc::clone(&entered),
            },
        )
        .with_effects(scope(identity, Rc::clone(&store))?)
        .build(&GrantSet::empty())?;
    match first.tick() {
        Err(BotError::EffectIndeterminate { .. }) => {}
        other => {
            return Err(format!(
                "the first run's effect must be reported as indeterminate; the tick \
                 returned {other:?}"
            )
            .into());
        }
    }
    let key = recorded(&store)?
        .first()
        .map(|event| event.key())
        .ok_or("the first attempt must have been recorded")?;

    let mut second = Bot::builder(NAME)
        .observe(FixedSource)
        .on(
            is_value,
            NeverSettles {
                entered: Rc::clone(&entered),
            },
        )
        .with_effects(scope(identity, Rc::clone(&store))?)
        .build(&GrantSet::empty())?;
    match second.tick() {
        Err(BotError::PendingTransition { .. }) => {}
        other => {
            return Err(format!(
                "the first tick of the restart must hold the recovered attempt and say \
                 so; it returned {other:?}"
            )
            .into());
        }
    }
    assert_eq!(
        entered.get(),
        1,
        "the first tick of the restart must hold, not resend"
    );

    second.resolve_effect(&key, EffectEvidence::NotApplied)?;
    let settled = recorded(&store)?;
    assert_eq!(
        settled.last().map(|event| event.kind()),
        Some(EventKind::OutcomeObserved),
        "settling a recovered attempt writes the fact down; a bot that only dropped the \
         key from its own list would come back with the same unknown it just cleared"
    );

    let retried = second.tick();
    assert!(
        !matches!(retried, Err(BotError::EffectRefused { .. })),
        "evidence that nothing was delivered makes the entry eligible for another \
         attempt, and the second attempt must not be refused by the journal as a repeat \
         of the first: {retried:?}"
    );
    assert_eq!(
        entered.get(),
        2,
        "the action must have been entered again: the attempt the caller repaired has to \
         reach the effect, not stop at the record"
    );
    assert_eq!(
        recorded(&store)?.len(),
        5,
        "the second attempt is a new key and walks the ladder from the beginning, so the \
         journal holds the first attempt's intent, dispatch and outcome and then the \
         second attempt's intent and dispatch"
    );
    Ok(())
}

/// A recovered attempt whose effect the journal records as landed is retired,
/// not dispatched again.
///
/// The crash this covers is the benign one: the effect landed and the record of
/// it landed too, and only the process died. Re-dispatching on the way back
/// would be a duplicate of an effect nothing is uncertain about — and skipping
/// the entry without retiring it would leave the chain reporting the same
/// finished work as outstanding forever.
#[test]
fn a_recovered_attempt_whose_effect_landed_is_retired_rather_than_sent_again() -> TestResult {
    let store = Rc::new(RefCell::new(MemoryJournal::new()));
    let entered = Rc::new(Cell::new(0));
    let identity = identity()?;

    let mut first = Bot::builder(NAME)
        .observe(FixedSource)
        .on(
            is_value,
            AlwaysLands {
                entered: Rc::clone(&entered),
            },
        )
        .with_effects(scope(identity, Rc::clone(&store))?)
        .build(&GrantSet::empty())?;
    let fired = first.tick()?;
    assert_eq!(fired, 1, "the effect lands on the run before the crash");
    assert_eq!(entered.get(), 1);

    let mut second = Bot::builder(NAME)
        .observe(FixedSource)
        .on(
            is_value,
            AlwaysLands {
                entered: Rc::clone(&entered),
            },
        )
        .with_effects(scope(identity, Rc::clone(&store))?)
        .build(&GrantSet::empty())?;

    let refused = second.tick();
    assert!(
        !matches!(refused, Err(BotError::EffectRefused { .. })),
        "a crash after the record of an effect landed must not make the run unable to \
         continue past that entry: {refused:?}"
    );
    assert_eq!(
        entered.get(),
        1,
        "the journal records the effect as landed, so nothing may send it again"
    );
    assert!(
        second.pending().is_empty(),
        "the entry is done for the generation its effect landed in, so nothing may be \
         reported as outstanding: {:?}",
        second.pending()
    );
    Ok(())
}

/// A live settlement is journaled before it is acknowledged, so a restart does
/// not resurrect an attempt this process already settled.
///
/// The counterexample (issue #106): `Ledger::settle` used to mutate only
/// in-memory state and return `Ok`, while the durable record still said
/// `DispatchPrepared` with no outcome. Rebuilding then restored the old
/// unknown and blocked the action again. The repair unifies live and recovered
/// settlement through one `OutcomeObserved` append that happens *before*
/// either is acknowledged.
#[test]
fn a_live_settlement_is_journaled_before_it_is_acknowledged() -> TestResult {
    let store = Rc::new(RefCell::new(MemoryJournal::new()));
    let entered = Rc::new(Cell::new(0));
    let identity = identity()?;

    let mut first = Bot::builder(NAME)
        .observe(FixedSource)
        .on(
            is_value,
            NeverSettles {
                entered: Rc::clone(&entered),
            },
        )
        .with_effects(scope(identity, Rc::clone(&store))?)
        .build(&GrantSet::empty())?;

    match first.tick() {
        Err(BotError::EffectIndeterminate { .. }) => {}
        other => {
            return Err(format!("expected indeterminate, got {other:?}").into());
        }
    }
    let at_rest = recorded(&store)?;
    assert_eq!(
        at_rest.len(),
        2,
        "intent and prepared dispatch: {at_rest:?}"
    );
    let key = at_rest[0].key();

    let held = first.pending();
    assert_eq!(held.len(), 1, "one live unknown: {held:?}");
    assert_eq!(held[0].key(), Some(key));

    // Live settlement. The call must write the fact down, not only move memory.
    first
        .resolve_effect(&key, EffectEvidence::Applied)
        .map_err(|error| format!("live resolve failed: {error:?}"))?;
    let after = recorded(&store)?;
    assert_eq!(
        after.len(),
        3,
        "the settlement is a committed event, not an in-memory ack: {after:?}"
    );
    assert!(
        matches!(
            after[2],
            EffectEvent::OutcomeObserved {
                evidence: EffectEvidence::Applied,
                ..
            }
        ),
        "the third event is the settlement fact: {:?}",
        after[2]
    );

    // Reconstruct against the same journal. The settled attempt must not come
    // back as an unknown.
    let mut second = Bot::builder(NAME)
        .observe(FixedSource)
        .on(
            is_value,
            NeverSettles {
                entered: Rc::clone(&entered),
            },
        )
        .with_effects(scope(identity, Rc::clone(&store))?)
        .build(&GrantSet::empty())?;

    assert!(
        second.pending().is_empty(),
        "a journaled live settlement must not resurrect as an unknown: {:?}",
        second.pending()
    );
    assert_eq!(
        entered.get(),
        1,
        "and nothing may re-enter the action that already ran"
    );

    // A repeat of the same evidence is idempotent, before and after restart.
    first
        .resolve_effect(&key, EffectEvidence::Applied)
        .map_err(|error| format!("repeat on first failed: {error:?}"))?;
    second
        .resolve_effect(&key, EffectEvidence::Applied)
        .map_err(|error| format!("repeat on second failed: {error:?}"))?;
    let final_events = recorded(&store)?;
    assert_eq!(
        final_events.len(),
        3,
        "a repeated settlement does not append twice: {final_events:?}"
    );
    Ok(())
}

// ── A source the test can move, and the successor's action ─────────────────

/// A source whose value the test sets between bots.
///
/// The #104 scenario is a source that *moves* across the restart: the first
/// bot sees `1`, the reconstructed one sees `2`. A constant source cannot
/// express "the condition is false now", which is the half of the scenario
/// that used to bury the unknown.
struct Shifting(Rc<Cell<u32>>);

impl Observe for Shifting {
    type Output = u32;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        call.0.check(Observe::required_caps(self))?;
        Ok(self.0.get())
    }

    fn domain_id(&self) -> &str {
        "test::shifting"
    }
}

/// A source that refuses every poll.
struct FailsToPoll;

impl Observe for FailsToPoll {
    type Output = u32;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        call.0.check(Observe::required_caps(self))?;
        Err(BotError::DomainError {
            domain: "test::fails_to_poll".to_owned(),
            certainty: DispatchCertainty::NotDelivered,
            cause: "the poll refused".to_owned(),
        })
    }

    fn domain_id(&self) -> &str {
        "test::fails_to_poll"
    }
}

/// The successor's action: counts every entry, and never fails.
struct Counts(Rc<Cell<u32>>);

impl Execute for Counts {
    type Input = u32;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
        call.0.check(Execute::required_caps(self))?;
        self.0.set(self.0.get().saturating_add(1));
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::counts"
    }
}

/// `true` only for `1`, so a source that moves to `2` makes it false.
fn is_one(seen: &u32) -> bool {
    *seen == 1
}

/// Always true: the successor's condition in the #104 scenario.
fn always(_seen: &u32) -> bool {
    true
}

/// The reserve-then-publish bot the #104 scenario is written against.
///
/// One chain, two entries: `(value == 1, reserve)` then `(always, publish)`.
/// Shared by the barrier tests so a drift in the declaration would not be
/// readable as a difference between them.
fn reserve_then_publish(
    value: Rc<Cell<u32>>,
    entered: Rc<Cell<u32>>,
    published: Rc<Cell<u32>>,
    store: Rc<RefCell<MemoryJournal>>,
    identity: EffectIdentity,
) -> Result<Bot, Box<dyn Error>> {
    Ok(Bot::builder(NAME)
        .observe(Shifting(Rc::clone(&value)))
        .on(
            is_one,
            NeverSettles {
                entered: Rc::clone(&entered),
            },
        )
        .on(always, Counts(Rc::clone(&published)))
        .with_effects(scope(identity, store)?)
        .build(&GrantSet::empty())?)
}

/// A recovered unknown is a barrier in front of a false condition.
///
/// Issue #104's concrete public scenario: the first bot observes `1`, reserve
/// reports `EffectIndeterminate`, and publish must not run. The journal is
/// rebuilt while the source yields `2`, so reserve's condition is now false.
/// The plan used to emit `Decision::Skip`, the walk used to mark the entry
/// `Skipped` before it ever consulted `effects.blocks`, and publish ran. A
/// changed source is not evidence that the earlier action did not occur.
#[test]
fn a_recovered_unknown_blocks_a_false_condition_and_its_successor() -> TestResult {
    let store = Rc::new(RefCell::new(MemoryJournal::new()));
    let entered = Rc::new(Cell::new(0));
    let published = Rc::new(Cell::new(0));
    let value = Rc::new(Cell::new(1));
    let identity = identity()?;

    let mut first = reserve_then_publish(
        Rc::clone(&value),
        Rc::clone(&entered),
        Rc::clone(&published),
        Rc::clone(&store),
        identity,
    )?;

    match first.tick() {
        Err(BotError::EffectIndeterminate { .. }) => {}
        other => {
            return Err(format!("expected indeterminate, got {other:?}").into());
        }
    }
    assert_eq!(entered.get(), 1, "reserve ran once, on the first attempt");
    assert_eq!(
        published.get(),
        0,
        "publish is behind reserve and must not have run"
    );
    let at_rest = recorded(&store)?;
    assert_eq!(
        at_rest.len(),
        2,
        "intent and prepared dispatch, no outcome: {at_rest:?}"
    );
    let key = at_rest[0].key();

    // The restart, with the source moved: reserve's condition is false now.
    value.set(2);
    let mut second = reserve_then_publish(
        Rc::clone(&value),
        Rc::clone(&entered),
        Rc::clone(&published),
        Rc::clone(&store),
        identity,
    )?;

    // Immediately after reconstruction, before any poll: the barrier is
    // durable and does not wait for a transition to open. The successor is
    // not reported yet — it has no transition to be NotStarted in — and that
    // is the one moment the two can disagree with the post-tick list.
    let before = second.pending();
    assert_eq!(
        before.len(),
        1,
        "the recovered unknown is already reported: {before:?}"
    );
    assert_eq!(
        before[0].key(),
        Some(key),
        "and it names the key to settle by"
    );
    assert!(
        matches!(
            before[0].hold(),
            TransitionHold::UnsettledByRecovery { action } if *action == key.action()
        ),
        "held because an earlier attempt never settled: {:?}",
        before[0].hold()
    );

    match second.tick() {
        Err(BotError::PendingTransition { work, outstanding }) => {
            // The barrier *and* the entry it blocks, which is the same pair
            // an abandonment reports. A count of 1 beside a two-entry hold
            // would read as "one thing is stuck", which is not what happened.
            assert_eq!(
                outstanding, 2,
                "the count and the report agree: the barrier and the entry behind it"
            );
            assert_eq!(
                work.id(),
                before[0].id(),
                "the tick names the barrier, not the successor behind it"
            );
            assert!(
                matches!(
                    work.hold(),
                    TransitionHold::UnsettledByRecovery { action } if *action == key.action()
                ),
                "the tick names the recovery hold, not a skip and not a fresh attempt: {:?}",
                work.hold()
            );
        }
        other => {
            return Err(format!("a false condition must not produce {other:?}").into());
        }
    }
    assert_eq!(
        published.get(),
        0,
        "the successor did not run behind a recovered unknown"
    );
    assert_eq!(
        entered.get(),
        1,
        "and the unknown attempt was not re-entered"
    );

    // A quiet tick does not erase the report.
    let quiet = second.tick();
    assert!(
        matches!(
            quiet,
            Err(BotError::PendingTransition { outstanding: 2, .. })
        ),
        "a still source leaves the barrier standing: {quiet:?}"
    );
    let after = second.pending();
    assert_eq!(after.len(), 2, "still reported, barrier first: {after:?}");
    assert_eq!(after[0].key(), Some(key));
    assert!(
        matches!(after[0].hold(), TransitionHold::UnsettledByRecovery { .. }),
        "{:?}",
        after[0].hold()
    );
    Ok(())
}

/// The same barrier in front of a *true* condition.
///
/// The other half of "independent of current condition": a condition that
/// holds would have planned an Attempt, and `run_chain`'s `blocks` guard
/// already stopped that. What it did not stop is the plan treating the
/// recovered work as fresh — and a tick that returned `EffectUnsettled` from
/// `begin` is still a re-entry of the dispatch path rather than a held report.
/// Either way publish must not run and the hold must be the recovery one.
#[test]
fn a_recovered_unknown_blocks_a_true_condition_and_its_successor() -> TestResult {
    let store = Rc::new(RefCell::new(MemoryJournal::new()));
    let entered = Rc::new(Cell::new(0));
    let published = Rc::new(Cell::new(0));
    let value = Rc::new(Cell::new(1));
    let identity = identity()?;

    let mut first = reserve_then_publish(
        Rc::clone(&value),
        Rc::clone(&entered),
        Rc::clone(&published),
        Rc::clone(&store),
        identity,
    )?;
    match first.tick() {
        Err(BotError::EffectIndeterminate { .. }) => {}
        other => return Err(format!("expected indeterminate, got {other:?}").into()),
    }

    // Same value: reserve's condition still holds.
    let mut second = reserve_then_publish(
        Rc::clone(&value),
        Rc::clone(&entered),
        Rc::clone(&published),
        Rc::clone(&store),
        identity,
    )?;
    match second.tick() {
        Err(BotError::PendingTransition { work, outstanding }) => {
            assert_eq!(
                outstanding, 2,
                "the barrier and the entry it blocks are both still reported"
            );
            assert!(
                matches!(work.hold(), TransitionHold::UnsettledByRecovery { .. }),
                "held for recovery, not re-planned as a fresh attempt: {:?}",
                work.hold()
            );
        }
        other => return Err(format!("expected the recovery hold, got {other:?}").into()),
    }
    assert_eq!(published.get(), 0, "the successor did not run");
    assert_eq!(entered.get(), 1, "reserve was not re-entered");
    Ok(())
}

/// A poll failure does not erase a recovered unknown.
///
/// The barrier has to survive a tick that never opens a transition at all:
/// the walk stops at the poll, and the only thing that still knows the effect
/// may be live is the journal.
#[test]
fn a_poll_failure_does_not_erase_a_recovered_unknown() -> TestResult {
    let store = Rc::new(RefCell::new(MemoryJournal::new()));
    let entered = Rc::new(Cell::new(0));
    let published = Rc::new(Cell::new(0));
    let value = Rc::new(Cell::new(1));
    let identity = identity()?;

    let mut first = reserve_then_publish(
        Rc::clone(&value),
        Rc::clone(&entered),
        Rc::clone(&published),
        Rc::clone(&store),
        identity,
    )?;
    match first.tick() {
        Err(BotError::EffectIndeterminate { .. }) => {}
        other => return Err(format!("expected indeterminate, got {other:?}").into()),
    }
    let key = recorded(&store)?[0].key();

    let mut second = Bot::builder(NAME)
        .observe(FailsToPoll)
        .on(
            is_one,
            NeverSettles {
                entered: Rc::clone(&entered),
            },
        )
        .on(always, Counts(Rc::clone(&published)))
        .with_effects(scope(identity, Rc::clone(&store))?)
        .build(&GrantSet::empty())?;

    match second.tick() {
        Err(BotError::DomainError { .. }) => {}
        other => return Err(format!("expected the poll's own failure, got {other:?}").into()),
    }
    let held = second.pending();
    assert_eq!(
        held.len(),
        1,
        "the recovered unknown survives the failed poll: {held:?}"
    );
    assert_eq!(held[0].key(), Some(key));
    assert!(
        matches!(held[0].hold(), TransitionHold::UnsettledByRecovery { .. }),
        "{:?}",
        held[0].hold()
    );
    assert_eq!(published.get(), 0);
    Ok(())
}

// ── Issue #101: admitted input identity ────────────────────────────────────

/// A source the test can retarget to a new `(id, payload)` pair.
///
/// The id is part of `PartialEq` and of [`InputIdentity`], which is the point:
/// two events with equal payloads must still be two events.
struct Requests(Rc<RefCell<EventId<u32>>>);

impl Observe for Requests {
    type Output = EventId<u32>;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<EventId<u32>, BotError> {
        call.0.check(Observe::required_caps(self))?;
        Ok(*self.0.borrow())
    }

    fn domain_id(&self) -> &str {
        "test::requests"
    }
}

/// The event-shaped counterpart of [`NeverSettles`].
struct NeverSettlesEvent(Rc<Cell<u32>>);

impl Execute for NeverSettlesEvent {
    type Input = EventId<u32>;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &EventId<u32>)) -> Result<(), BotError> {
        call.0.check(Execute::required_caps(self))?;
        self.0.set(self.0.get().saturating_add(1));
        Err(BotError::EffectIndeterminate {
            domain: "test::stale-controller".to_owned(),
            cause: "unknown".to_owned(),
        })
    }

    fn domain_id(&self) -> &str {
        "test::never-settles-event"
    }
}

/// A shared journal that advances immediately after recovery reads its history.
///
/// This models the construction interleaving that an append fence alone cannot
/// observe: another controller commits after `committed()` returns and before
/// this controller adopts `tail()`. The adapter owns the one injected event so
/// later tail reads are ordinary observations.
struct SnapshotRaceJournal {
    store: Rc<RefCell<MemoryJournal>>,
    inject_on_tail: RefCell<Option<EffectEvent>>,
}

impl EffectJournal for SnapshotRaceJournal {
    fn durability(&self) -> DurabilityPromise {
        self.store.borrow().durability()
    }

    fn tail(&self) -> JournalPosition {
        if let Some(event) = self.inject_on_tail.borrow_mut().take() {
            let mut store = self.store.borrow_mut();
            let expected = store.tail();
            if store.compare_and_append(expected, &event).is_err() {
                return expected;
            }
        }
        self.store.borrow().tail()
    }

    fn committed(&self) -> Result<Vec<EffectEvent>, JournalError> {
        EffectJournal::committed(&*self.store.borrow())
    }

    fn compare_and_append(
        &mut self,
        expected_tail: JournalPosition,
        event: &EffectEvent,
    ) -> Result<DurableAck, JournalError> {
        self.store
            .borrow_mut()
            .compare_and_append(expected_tail, event)
    }
}

fn snapshot_race_scope(
    identity: EffectIdentity,
    store: Rc<RefCell<MemoryJournal>>,
    event: EffectEvent,
) -> Result<EffectScope, Box<dyn Error>> {
    Ok(EffectScope::new(
        identity,
        broker(identity.environment())?,
        Box::new(SnapshotRaceJournal {
            store,
            inject_on_tail: RefCell::new(Some(event)),
        }),
    ))
}

/// A hostile adapter that appends a foreign unknown handoff, then returns that
/// later position as the acknowledgment for the caller's intent.
struct ForeignTailAckJournal {
    store: Rc<RefCell<MemoryJournal>>,
    forge_once: Cell<bool>,
}

impl EffectJournal for ForeignTailAckJournal {
    fn durability(&self) -> DurabilityPromise {
        self.store.borrow().durability()
    }

    fn tail(&self) -> JournalPosition {
        self.store.borrow().tail()
    }

    fn committed(&self) -> Result<Vec<EffectEvent>, JournalError> {
        EffectJournal::committed(&*self.store.borrow())
    }

    fn committed_entries(&self) -> Result<Vec<JournalEntry>, JournalError> {
        Ok(self.store.borrow().committed().to_vec())
    }

    fn compare_and_append(
        &mut self,
        expected_tail: JournalPosition,
        event: &EffectEvent,
    ) -> Result<DurableAck, JournalError> {
        let acknowledgment = self
            .store
            .borrow_mut()
            .compare_and_append(expected_tail, event)?;
        if self.forge_once.replace(false) && matches!(event, EffectEvent::IntentAdmitted { .. }) {
            let foreign_key = event
                .key()
                .with_attempt(AttemptId::from_decimal("2").map_err(|_| JournalError::Exhausted)?);
            let mut store = self.store.borrow_mut();
            let tail = store.tail();
            store.compare_and_append(tail, &EffectEvent::IntentAdmitted { key: foreign_key })?;
            let tail = store.tail();
            store.compare_and_append(tail, &EffectEvent::DispatchPrepared { key: foreign_key })?;
            return Ok(DurableAck::new(store.tail(), acknowledgment.promise()));
        }
        Ok(acknowledgment)
    }
}

/// A stale controller must not replace its recovery fold with a newly-read
/// tail. Controller A records an unknown handoff, while controller B still
/// holds the empty fold and observes a different input for the same action.
/// B's append is fenced before its receiver can run (issue #120).
#[test]
fn a_stale_controller_cannot_dispatch_past_another_controllers_unknown_effect() -> TestResult {
    let store = Rc::new(RefCell::new(MemoryJournal::new()));
    let first_input = Rc::new(RefCell::new(EventId::new(1, 1)));
    let second_input = Rc::new(RefCell::new(EventId::new(2, 2)));
    let entered = Rc::new(Cell::new(0));
    let identity = identity()?;

    let mut first = Bot::builder(NAME)
        .observe(Requests(Rc::clone(&first_input)))
        .on(
            |_seen: &EventId<u32>| true,
            NeverSettlesEvent(Rc::clone(&entered)),
        )
        .with_effects(scope(identity, Rc::clone(&store))?)
        .build(&GrantSet::empty())?;
    let mut stale = Bot::builder(NAME)
        .observe(Requests(second_input))
        .on(
            |_seen: &EventId<u32>| true,
            NeverSettlesEvent(Rc::clone(&entered)),
        )
        .with_effects(scope(identity, Rc::clone(&store))?)
        .build(&GrantSet::empty())?;

    assert!(matches!(
        first.tick(),
        Err(BotError::EffectIndeterminate { .. })
    ));
    assert_eq!(entered.get(), 1, "the first controller entered once");

    assert!(matches!(
        stale.tick(),
        Err(BotError::EffectRefused {
            cause: DispatchError::Journal(JournalError::TailMismatch { .. })
        })
    ));
    assert_eq!(
        entered.get(),
        1,
        "the stale controller must not dispatch a distinct input past the unknown effect"
    );
    Ok(())
}

/// Assembly rejects a journal that advances between its recovery fold and
/// retained append fence. Accepting that mixed snapshot would let the new bot
/// treat the injected outcome as absent while appending after it (issue #120).
#[test]
fn assembly_refuses_a_tail_that_advanced_after_recovery() -> TestResult {
    let store = Rc::new(RefCell::new(MemoryJournal::new()));
    let entered = Rc::new(Cell::new(0));
    let identity = identity()?;
    let mut first = Bot::builder(NAME)
        .observe(Requests(Rc::new(RefCell::new(EventId::new(1, 1)))))
        .on(
            |_seen: &EventId<u32>| true,
            NeverSettlesEvent(Rc::clone(&entered)),
        )
        .with_effects(scope(identity, Rc::clone(&store))?)
        .build(&GrantSet::empty())?;

    assert!(matches!(
        first.tick(),
        Err(BotError::EffectIndeterminate { .. })
    ));
    let events = recorded(&store)?;
    assert_eq!(
        events.len(),
        2,
        "the unknown handoff has intent and prepare"
    );
    let key = events[0].key();

    let result = Bot::builder(NAME)
        .observe(Requests(Rc::new(RefCell::new(EventId::new(2, 2)))))
        .on(
            |_seen: &EventId<u32>| true,
            NeverSettlesEvent(Rc::clone(&entered)),
        )
        .with_effects(snapshot_race_scope(
            identity,
            store,
            EffectEvent::OutcomeObserved {
                key,
                evidence: EffectEvidence::NotApplied,
            },
        )?)
        .build(&GrantSet::empty());

    assert!(matches!(
        result,
        Err(BotError::EffectRefused {
            cause: DispatchError::Journal(JournalError::SnapshotStale {
                recovered_events: 2,
                committed_events: 3,
            }),
        })
    ));
    assert_eq!(
        entered.get(),
        1,
        "assembly must not dispatch while recovering"
    );
    Ok(())
}

/// A later tail is not an acknowledgement for this controller's intent. The
/// foreign attempt is deliberately valid and unknown, so adopting its tail
/// would permit this receiver to run past a fact it never folded (issue #120).
#[test]
fn a_forged_intent_acknowledgement_cannot_launder_a_foreign_unknown_effect() -> TestResult {
    let store = Rc::new(RefCell::new(MemoryJournal::new()));
    let entered = Rc::new(Cell::new(0));
    let identity = identity()?;
    let mut bot = Bot::builder(NAME)
        .observe(Requests(Rc::new(RefCell::new(EventId::new(1, 1)))))
        .on(
            |_seen: &EventId<u32>| true,
            NeverSettlesEvent(Rc::clone(&entered)),
        )
        .with_effects(EffectScope::new(
            identity,
            broker(identity.environment())?,
            Box::new(ForeignTailAckJournal {
                store,
                forge_once: Cell::new(true),
            }),
        ))
        .build(&GrantSet::empty())?;

    assert!(matches!(
        bot.tick(),
        Err(BotError::EffectRefused {
            cause: DispatchError::Journal(JournalError::ReceiptMismatch { .. })
        })
    ));
    assert_eq!(
        entered.get(),
        0,
        "a forged acknowledgement cannot reach the receiver"
    );
    Ok(())
}

/// Sequential ownership transfer still works: a settled handoff does not fence
/// out the successor (issue #120 acceptance).
///
/// This is the positive control for the three refusals above. Without it a
/// fence that rejected every second dispatch would pass them all. Controller A
/// runs a definite effect to completion, so the journal holds intent, prepare
/// and an `Applied` outcome rather than an unknown. Controller B is built over
/// the same store *after* those events — its recovery fold is the advanced
/// tail, not the empty one the stale controller held — and dispatches a
/// different input for the same action. B's receiver runs.
#[test]
fn a_successor_dispatches_after_a_settled_handoff() -> TestResult {
    let store = Rc::new(RefCell::new(MemoryJournal::new()));
    let first_input = Rc::new(RefCell::new(EventId::new(1, 1)));
    let second_input = Rc::new(RefCell::new(EventId::new(2, 2)));
    let log = Rc::new(RefCell::new(Vec::new()));
    let identity = identity()?;

    let mut first = Bot::builder(NAME)
        .observe(Requests(Rc::clone(&first_input)))
        .on(|_seen: &EventId<u32>| true, Records(Rc::clone(&log)))
        .with_effects(scope(identity, Rc::clone(&store))?)
        .build(&GrantSet::empty())?;
    let fired = first.tick()?;
    assert_eq!(fired, 1, "the first controller dispatched its one action");
    assert_eq!(
        log.borrow().as_slice(),
        [EventId::new(1, 1)],
        "the first receiver saw its input"
    );
    let after_first = recorded(&store)?;
    assert_eq!(
        after_first.len(),
        3,
        "intent, prepare, and a settled outcome: {after_first:?}"
    );
    assert!(
        matches!(
            after_first[2],
            EffectEvent::OutcomeObserved {
                evidence: EffectEvidence::Applied,
                ..
            }
        ),
        "a definite effect settles as applied, which is what makes this a transfer: {:?}",
        after_first[2]
    );
    let first_key = after_first[0].key();

    // The successor is built over the advanced journal, so its fold includes
    // the settled attempt rather than remaining ignorant of it.
    let mut successor = Bot::builder(NAME)
        .observe(Requests(second_input))
        .on(|_seen: &EventId<u32>| true, Records(Rc::clone(&log)))
        .with_effects(scope(identity, Rc::clone(&store))?)
        .build(&GrantSet::empty())?;
    let fired = successor.tick()?;
    assert_eq!(
        fired, 1,
        "a successor that folded the settled handoff is not fenced out"
    );
    assert_eq!(
        log.borrow().as_slice(),
        [EventId::new(1, 1), EventId::new(2, 2)],
        "both receivers ran, in ownership order"
    );
    let after_second = recorded(&store)?;
    assert_eq!(
        after_second.len(),
        6,
        "the successor appended its own ladder: {after_second:?}"
    );
    assert_eq!(after_second[3].key(), after_second[4].key());
    assert_eq!(after_second[4].key(), after_second[5].key());
    assert_ne!(
        after_second[3].key(),
        first_key,
        "a distinct input is a distinct key, which is what makes the stale-controller \
         falsifier a real one and this transfer a real transfer"
    );
    Ok(())
}

/// An action that records every input it was entered with.
struct Records(Rc<RefCell<Vec<EventId<u32>>>>);

impl Execute for Records {
    type Input = EventId<u32>;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &EventId<u32>)) -> Result<(), BotError> {
        call.0.check(Execute::required_caps(self))?;
        self.0.borrow_mut().push(*call.1);
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::records"
    }
}

/// `true` for the payload half alone, so a new event id with the old payload
/// still holds and a new payload does not.
fn payload_is(seen: &EventId<u32>, want: u32) -> bool {
    *seen.value() == want
}

/// A restarted bot must not read a *different* request as already applied.
///
/// Counterexample A in issue #101: process A observes request A, the action
/// lands and is journaled `Applied`. The journal is rebuilt while the source
/// now returns request B. A digest bound to the process-local `Revision`
/// counter derives the same identity for both (both are revision 1 after a
/// restart) and skips B. B must run.
#[test]
fn a_changed_request_after_reconstruction_is_not_skipped_as_applied() -> TestResult {
    let store = Rc::new(RefCell::new(MemoryJournal::new()));
    let seen = Rc::new(RefCell::new(EventId::new(1, 10)));
    let log = Rc::new(RefCell::new(Vec::new()));
    let identity = identity()?;

    let mut first = Bot::builder(NAME)
        .observe(Requests(Rc::clone(&seen)))
        .on(
            |seen: &EventId<u32>| payload_is(seen, 10),
            Records(Rc::clone(&log)),
        )
        .with_effects(scope(identity, Rc::clone(&store))?)
        .build(&GrantSet::empty())?;
    assert_eq!(first.tick()?, 1, "request A runs");
    assert_eq!(*log.borrow(), vec![EventId::new(1, 10)]);

    // A different request, with a different event id and a different payload.
    // The condition is the same declaration as A's ("this payload"), so what
    // is under test is the digest, not a second builder shape.
    *seen.borrow_mut() = EventId::new(2, 20);
    let mut second = Bot::builder(NAME)
        .observe(Requests(Rc::clone(&seen)))
        .on(
            |seen: &EventId<u32>| payload_is(seen, 20),
            Records(Rc::clone(&log)),
        )
        .with_effects(scope(identity, Rc::clone(&store))?)
        .build(&GrantSet::empty())?;
    assert_eq!(
        second.tick()?,
        1,
        "request B runs; it was not silently omitted as already applied"
    );
    assert_eq!(
        *log.borrow(),
        vec![EventId::new(1, 10), EventId::new(2, 20)],
        "and the receiver sees the exact values, in order"
    );
    Ok(())
}

/// Two events with equal payloads are two events.
///
/// Content equality is not event identity (issue #101). The id is part of
/// `EventId`'s `InputIdentity` and its `PartialEq`, so neither the change
/// filter nor the dispatch digest can collapse them.
#[test]
fn two_equal_payloads_with_distinct_event_ids_are_two_events() -> TestResult {
    let store = Rc::new(RefCell::new(MemoryJournal::new()));
    let seen = Rc::new(RefCell::new(EventId::new(1, 7)));
    let log = Rc::new(RefCell::new(Vec::new()));
    let identity = identity()?;

    let mut bot = Bot::builder(NAME)
        .observe(Requests(Rc::clone(&seen)))
        .on(|_v: &EventId<u32>| true, Records(Rc::clone(&log)))
        .with_effects(scope(identity, Rc::clone(&store))?)
        .build(&GrantSet::empty())?;

    assert_eq!(bot.tick()?, 1, "event 1 runs");
    *seen.borrow_mut() = EventId::new(2, 7);
    assert_eq!(
        bot.tick()?,
        1,
        "event 2 runs even though the payload is equal"
    );
    assert_eq!(
        *log.borrow(),
        vec![EventId::new(1, 7), EventId::new(2, 7)],
        "two distinct events, both delivered"
    );
    Ok(())
}

/// A journal from another flow is refused at assembly.
///
/// The ownership identity is the whole of run + flow + environment, not the
/// two fields assembly used to check (issue #101).
#[test]
fn a_journal_from_another_flow_is_refused() -> TestResult {
    let store = Rc::new(RefCell::new(MemoryJournal::new()));
    let seen = Rc::new(RefCell::new(EventId::new(1, 1)));
    let log = Rc::new(RefCell::new(Vec::new()));
    let mine = identity()?;

    let mut first = Bot::builder(NAME)
        .observe(Requests(Rc::clone(&seen)))
        .on(|_v: &EventId<u32>| true, Records(Rc::clone(&log)))
        .with_effects(scope(mine, Rc::clone(&store))?)
        .build(&GrantSet::empty())?;
    first.tick()?;
    drop(first);

    let other_flow = EffectIdentity::new(
        mine.run(),
        mine.environment(),
        FlowRevision::from_tagged(
            "blake3_256",
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        )?,
    );
    match Bot::builder(NAME)
        .observe(Requests(Rc::clone(&seen)))
        .on(|_v: &EventId<u32>| true, Records(Rc::clone(&log)))
        .with_effects(scope(other_flow, Rc::clone(&store))?)
        .build(&GrantSet::empty())
    {
        Err(BotError::ActionNotDeclared { .. }) => Ok(()),
        other => Err(format!("expected a foreign-journal refusal, got {other:?}").into()),
    }
}

// ── Issue #100: external handoff is admitted on the real path ──────────────

/// An action that would leave the process, and records whether it was entered.
struct ExternalMarker(Rc<Cell<bool>>);

impl Execute for ExternalMarker {
    type Input = EventId<u32>;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    // The default is already `External`; stated so the test's subject is
    // visible at the declaration rather than inferred from a missing override.
    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::External
    }

    async fn execute_action(&self, call: (Auth, &EventId<u32>)) -> Result<(), BotError> {
        call.0.check(Execute::required_caps(self))?;
        self.0.set(true);
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::external_marker"
    }
}

/// A journal that advertises `ProcessCrash` and then acks `Ephemeral`.
///
/// The advertisement is not what the handoff is checked against: the
/// acknowledgment is (issue #100).
struct WeakAckJournal {
    inner: MemoryJournal,
}

impl EffectJournal for WeakAckJournal {
    fn durability(&self) -> DurabilityPromise {
        DurabilityPromise::ProcessCrash
    }

    fn tail(&self) -> JournalPosition {
        self.inner.tail()
    }

    fn committed(&self) -> Result<Vec<EffectEvent>, JournalError> {
        EffectJournal::committed(&self.inner)
    }

    fn compare_and_append(
        &mut self,
        expected_tail: JournalPosition,
        event: &EffectEvent,
    ) -> Result<DurableAck, JournalError> {
        self.inner.compare_and_append(expected_tail, event)?;
        Ok(DurableAck::new(
            self.inner.tail(),
            DurabilityPromise::Ephemeral,
        ))
    }
}

/// An ephemeral scope refuses an external effect before the action runs.
///
/// The counterexample in issue #100: the in-memory ladder accepted the two
/// write-ahead events and the adapter was entered, so its filesystem effect
/// outlived the process despite the advertised refusal. The refusal is now on
/// the handoff path, and the marker is the receiver that must never appear.
#[test]
fn an_ephemeral_scope_refuses_an_external_effect_before_it_runs() -> TestResult {
    let store = Rc::new(RefCell::new(MemoryJournal::new()));
    let seen = Rc::new(RefCell::new(EventId::new(1, 1)));
    let entered = Rc::new(Cell::new(false));
    let identity = identity()?;

    let mut bot = Bot::builder(NAME)
        .observe(Requests(Rc::clone(&seen)))
        .on(
            |_seen: &EventId<u32>| true,
            ExternalMarker(Rc::clone(&entered)),
        )
        .with_effects(scope(identity, store)?)
        .build(&GrantSet::empty())?;

    match bot.tick() {
        Err(BotError::EffectRefused { .. }) => {}
        other => {
            return Err(format!("expected a handoff refusal, got {other:?}").into());
        }
    }
    assert!(
        !entered.get(),
        "the external action must not be entered when the journal cannot outlive the process"
    );
    Ok(())
}

/// A local effect still runs on an ephemeral scope.
///
/// The other half of the contract: `MemoryJournal` stays useful for explicitly
/// local work (issue #100).
#[test]
fn an_ephemeral_scope_runs_a_local_effect() -> TestResult {
    let store = Rc::new(RefCell::new(MemoryJournal::new()));
    let seen = Rc::new(RefCell::new(EventId::new(1, 1)));
    let log = Rc::new(RefCell::new(Vec::new()));
    let identity = identity()?;

    let mut bot = Bot::builder(NAME)
        .observe(Requests(Rc::clone(&seen)))
        .on(|_seen: &EventId<u32>| true, Records(Rc::clone(&log)))
        .with_effects(scope(identity, store)?)
        .build(&GrantSet::empty())?;

    assert_eq!(bot.tick()?, 1, "a declared-local action runs");
    assert_eq!(*log.borrow(), vec![EventId::new(1, 1)]);
    Ok(())
}

/// A journal whose acknowledgment is weaker than its advertisement is refused.
///
/// The handoff checks the per-append promise, not `durability()`: a store that
/// claims `ProcessCrash` and acks `Ephemeral` is weaker than it says (issue
/// #100).
#[test]
fn an_ack_weaker_than_the_advertised_grade_is_refused() -> TestResult {
    let seen = Rc::new(RefCell::new(EventId::new(1, 1)));
    let entered = Rc::new(Cell::new(false));
    let identity = identity()?;
    let environment = identity.environment();
    let mut broker = Broker::new();
    broker.register(environment)?;

    let mut bot = Bot::builder(NAME)
        .observe(Requests(Rc::clone(&seen)))
        .on(
            |_seen: &EventId<u32>| true,
            ExternalMarker(Rc::clone(&entered)),
        )
        .with_effects(EffectScope::new(
            identity,
            broker,
            Box::new(WeakAckJournal {
                inner: MemoryJournal::new(),
            }),
        ))
        .build(&GrantSet::empty())?;

    match bot.tick() {
        Err(BotError::EffectRefused { .. }) => {}
        other => {
            return Err(format!("expected the weak ack to be refused, got {other:?}").into());
        }
    }
    assert!(!entered.get(), "the action did not run on a weak ack");
    Ok(())
}

// ── Red-team counterexamples: the same input must retire in-process ─────────

/// A source that returns to a previously landed `EventId` must retire rather
/// than refuse or re-enter.
///
/// The retire check is `Effects::applied_in`, which only knows what live
/// settlement folded into `applied`. A raw `OutcomeObserved` append left that
/// set empty, so the returning event minted `AttemptId::FIRST` again and the
/// journal answered `OutOfOrder` after the entry was already marked
/// `Unrecorded` — a false barrier instead of a retirement (issue #101).
#[test]
fn a_returning_landed_event_is_retired_not_refused() -> TestResult {
    let store = Rc::new(RefCell::new(MemoryJournal::new()));
    let seen = Rc::new(RefCell::new(EventId::new(1, 10)));
    let log = Rc::new(RefCell::new(Vec::new()));
    let identity = identity()?;

    let mut bot = Bot::builder(NAME)
        .observe(Requests(Rc::clone(&seen)))
        .on(|_seen: &EventId<u32>| true, Records(Rc::clone(&log)))
        .with_effects(scope(identity, Rc::clone(&store))?)
        .build(&GrantSet::empty())?;

    assert_eq!(bot.tick()?, 1, "event 1 runs");
    *seen.borrow_mut() = EventId::new(2, 20);
    assert_eq!(bot.tick()?, 1, "event 2 runs");
    assert_eq!(
        *log.borrow(),
        vec![EventId::new(1, 10), EventId::new(2, 20)]
    );

    // Event 1 returns with the same identity. It already landed, so it retires.
    *seen.borrow_mut() = EventId::new(1, 10);
    match bot.tick() {
        Ok(fired) => {
            assert_eq!(fired, 0, "event 1 must be retired, not re-dispatched");
            assert_eq!(
                *log.borrow(),
                vec![EventId::new(1, 10), EventId::new(2, 20)],
                "no second delivery of event 1"
            );
            Ok(())
        }
        Err(error) => Err(format!(
            "expected retire/skip, got {error:?}; log={:?}",
            log.borrow()
        )
        .into()),
    }
}

/// A journal wrapper that keeps a shared view of what landed.
struct SharedJournal {
    store: Rc<RefCell<MemoryJournal>>,
    /// What `admit_external_handoff` sees.
    advertised: DurabilityPromise,
    /// What every acknowledgment reports.
    ack: DurabilityPromise,
}

impl EffectJournal for SharedJournal {
    fn durability(&self) -> DurabilityPromise {
        self.advertised
    }

    fn tail(&self) -> JournalPosition {
        self.store.borrow().tail()
    }

    fn committed(&self) -> Result<Vec<EffectEvent>, JournalError> {
        EffectJournal::committed(&*self.store.borrow())
    }

    fn compare_and_append(
        &mut self,
        expected_tail: JournalPosition,
        event: &EffectEvent,
    ) -> Result<DurableAck, JournalError> {
        self.store
            .borrow_mut()
            .compare_and_append(expected_tail, event)?;
        Ok(DurableAck::new(self.store.borrow().tail(), self.ack))
    }
}

/// An external journal that returns a receipt for either the named outcome or
/// an unrelated position, to exercise receipt identity verification.
struct OutcomeReceiptJournal {
    store: Rc<RefCell<MemoryJournal>>,
    receipt: Rc<Cell<DurabilityPromise>>,
    unrelated_receipt: Rc<Cell<bool>>,
    unrelated_append_ack: Rc<Cell<bool>>,
}

impl EffectJournal for OutcomeReceiptJournal {
    fn durability(&self) -> DurabilityPromise {
        DurabilityPromise::ProcessCrash
    }

    fn tail(&self) -> JournalPosition {
        self.store.borrow().tail()
    }

    fn committed(&self) -> Result<Vec<EffectEvent>, JournalError> {
        EffectJournal::committed(&*self.store.borrow())
    }

    fn committed_entries(&self) -> Result<Vec<JournalEntry>, JournalError> {
        Ok(self.store.borrow().committed().to_vec())
    }

    fn compare_and_append(
        &mut self,
        expected_tail: JournalPosition,
        event: &EffectEvent,
    ) -> Result<DurableAck, JournalError> {
        let acknowledgment = self
            .store
            .borrow_mut()
            .compare_and_append(expected_tail, event)?;
        let promise = if matches!(event, EffectEvent::OutcomeObserved { .. }) {
            DurabilityPromise::Ephemeral
        } else {
            DurabilityPromise::ProcessCrash
        };
        let position = if self.unrelated_append_ack.get()
            && matches!(event, EffectEvent::OutcomeObserved { .. })
        {
            JournalPosition::genesis()
        } else {
            acknowledgment.position()
        };
        let promise = if self.unrelated_append_ack.get()
            && matches!(event, EffectEvent::OutcomeObserved { .. })
        {
            DurabilityPromise::ProcessCrash
        } else {
            promise
        };
        Ok(DurableAck::new(position, promise))
    }

    fn confirm_outcome(
        &mut self,
        _key: EffectKey,
        _evidence: EffectEvidence,
        position: JournalPosition,
        _required: DurabilityPromise,
    ) -> Result<DurableAck, JournalError> {
        Ok(DurableAck::new(
            if self.unrelated_receipt.get() {
                JournalPosition::genesis()
            } else {
                position
            },
            self.receipt.get(),
        ))
    }
}

/// One external action invocation.
struct ExternalCount(Rc<Cell<usize>>);

impl Execute for ExternalCount {
    type Input = EventId<u32>;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::External
    }

    async fn execute_action(&self, call: (Auth, &EventId<u32>)) -> Result<Self::Output, BotError> {
        call.0.check(Execute::required_caps(self))?;
        self.0.set(self.0.get().saturating_add(1));
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::external_count"
    }
}

/// An unrelated durable receipt must not settle an outcome or authorize a
/// recovered action to run again.
#[test]
fn an_unrelated_outcome_receipt_is_refused_without_reentering_the_effect() -> TestResult {
    let store = Rc::new(RefCell::new(MemoryJournal::new()));
    let receipt = Rc::new(Cell::new(DurabilityPromise::ProcessCrash));
    let unrelated_receipt = Rc::new(Cell::new(true));
    let unrelated_append_ack = Rc::new(Cell::new(true));
    let seen = Rc::new(RefCell::new(EventId::new(1, 1)));
    let entered = Rc::new(Cell::new(0));
    let identity = identity()?;
    let environment = identity.environment();
    let mut run_broker = Broker::new();
    run_broker.register(environment)?;

    let mut bot = Bot::builder(NAME)
        .observe(Requests(Rc::clone(&seen)))
        .on(
            |_seen: &EventId<u32>| true,
            ExternalCount(Rc::clone(&entered)),
        )
        .with_effects(EffectScope::new(
            identity,
            run_broker,
            Box::new(OutcomeReceiptJournal {
                store: Rc::clone(&store),
                receipt: Rc::clone(&receipt),
                unrelated_receipt: Rc::clone(&unrelated_receipt),
                unrelated_append_ack: Rc::clone(&unrelated_append_ack),
            }),
        ))
        .build(&GrantSet::empty())?;

    match bot.tick() {
        Err(BotError::EffectUnrecorded { cause, .. }) => assert!(
            matches!(
                *cause,
                DispatchError::Journal(JournalError::ReceiptMismatch { .. })
            ),
            "an unrelated receipt must be rejected as an identity mismatch, got {cause:?}"
        ),
        other => return Err(format!("expected receipted recording failure, got {other:?}").into()),
    }
    assert_eq!(entered.get(), 1, "the external action ran exactly once");
    drop(bot);

    let mut recovered = Bot::builder(NAME)
        .observe(Requests(Rc::clone(&seen)))
        .on(
            |_seen: &EventId<u32>| true,
            ExternalCount(Rc::clone(&entered)),
        )
        .with_effects(EffectScope::new(
            identity,
            broker(identity.environment())?,
            Box::new(OutcomeReceiptJournal {
                store,
                receipt,
                unrelated_receipt: Rc::clone(&unrelated_receipt),
                unrelated_append_ack,
            }),
        ))
        .build(&GrantSet::empty())?;

    let held = recovered.pending();
    assert_eq!(held.len(), 1, "the unverified durable outcome remains held");
    assert!(matches!(
        held[0].hold(),
        TransitionHold::RecordingFailed { .. }
    ));
    let key = held[0]
        .key()
        .ok_or("the recovered receipt hold has a key")?;
    unrelated_receipt.set(false);
    recovered.resolve_effect(&key, EffectEvidence::Applied)?;
    assert!(
        recovered.pending().is_empty(),
        "the bound receipt clears the hold"
    );
    assert_eq!(
        entered.get(),
        1,
        "recovery did not re-enter the external action"
    );
    Ok(())
}

/// Whether the named kind was appended for any key.
fn journal_has(store: &Rc<RefCell<MemoryJournal>>, kind: EventKind) -> bool {
    let committed = EffectJournal::committed(&*store.borrow()).unwrap_or_default();
    committed.iter().any(|event| event.kind() == kind)
}

/// A handoff refused before the action ran must not leave a false
/// `Unrecorded` barrier, and a later tick must not treat the entry as
/// "may be live" (issue #100).
#[test]
fn a_refused_handoff_leaves_no_unrecorded_barrier() -> TestResult {
    let store = Rc::new(RefCell::new(MemoryJournal::new()));
    let seen = Rc::new(RefCell::new(EventId::new(1, 1)));
    let entered = Rc::new(Cell::new(false));
    let identity = identity()?;
    let environment = identity.environment();
    let mut broker = Broker::new();
    broker.register(environment)?;

    let mut bot = Bot::builder(NAME)
        .observe(Requests(Rc::clone(&seen)))
        .on(
            |_seen: &EventId<u32>| true,
            ExternalMarker(Rc::clone(&entered)),
        )
        .with_effects(EffectScope::new(
            identity,
            broker,
            Box::new(SharedJournal {
                store: Rc::clone(&store),
                advertised: DurabilityPromise::Ephemeral,
                ack: DurabilityPromise::Ephemeral,
            }),
        ))
        .build(&GrantSet::empty())?;

    match bot.tick() {
        Err(BotError::EffectRefused { .. }) => {}
        other => return Err(format!("expected EffectRefused, got {other:?}").into()),
    }
    assert!(!entered.get(), "the action did not run");
    // `MemoryJournal` advertises `Ephemeral`, so admission refuses before the
    // intent is written. Nothing may be prepared either way.
    assert!(
        !journal_has(&store, EventKind::DispatchPrepared),
        "a handoff that cannot outlive the process must not be prepared"
    );

    match bot.tick() {
        Ok(_) => Ok(()),
        Err(BotError::PendingTransition { work, outstanding }) => {
            let hold = format!("{:?}", work.hold());
            if hold.contains("Unrecorded")
                || hold.contains("OutcomeUnknown")
                || hold.contains("UnsettledByRecovery")
            {
                Err(format!(
                    "false barrier after handoff refusal: hold={hold} outstanding={outstanding}"
                )
                .into())
            } else {
                // A real hold is allowed; a false "may be live" is not.
                Ok(())
            }
        }
        // Refusing again is the configuration error still being true. It is
        // not the defect; the defect is a false unknown barrier.
        Err(BotError::EffectRefused { .. }) => Ok(()),
        Err(other) => Err(format!("unexpected second tick error: {other:?}").into()),
    }
}

/// A weak per-append ack must be refused before `DispatchPrepared` is
/// committed, so recovery cannot fold a dispatch that never happened as an
/// unknown outcome (issue #100).
#[test]
fn a_weak_ack_does_not_record_dispatch_prepared() -> TestResult {
    let store = Rc::new(RefCell::new(MemoryJournal::new()));
    let seen = Rc::new(RefCell::new(EventId::new(1, 1)));
    let entered = Rc::new(Cell::new(false));
    let identity = identity()?;
    let environment = identity.environment();
    let mut broker = Broker::new();
    broker.register(environment)?;

    let mut bot = Bot::builder(NAME)
        .observe(Requests(Rc::clone(&seen)))
        .on(
            |_seen: &EventId<u32>| true,
            ExternalMarker(Rc::clone(&entered)),
        )
        .with_effects(EffectScope::new(
            identity,
            broker,
            Box::new(SharedJournal {
                store: Rc::clone(&store),
                // Advertised as durable enough for an external handoff, but
                // every acknowledgment comes back `Ephemeral`.
                advertised: DurabilityPromise::ProcessCrash,
                ack: DurabilityPromise::Ephemeral,
            }),
        ))
        .build(&GrantSet::empty())?;

    match bot.tick() {
        Err(BotError::EffectRefused { .. }) => {}
        other => return Err(format!("expected EffectRefused, got {other:?}").into()),
    }
    assert!(!entered.get(), "the action did not run on a weak ack");
    assert!(
        journal_has(&store, EventKind::IntentAdmitted),
        "the intent is the write-ahead fact that was actually committed"
    );
    assert!(
        !journal_has(&store, EventKind::DispatchPrepared),
        "no DispatchPrepared for a handoff refused on its acknowledgment"
    );

    match bot.tick() {
        Ok(_) => Ok(()),
        Err(BotError::PendingTransition { work, .. }) => {
            let hold = format!("{:?}", work.hold());
            if hold.contains("Unrecorded")
                || hold.contains("OutcomeUnknown")
                || hold.contains("UnsettledByRecovery")
            {
                Err(format!("weak ack left a false unknown barrier: {hold}").into())
            } else {
                Ok(())
            }
        }
        Err(BotError::EffectRefused { .. }) => Ok(()),
        Err(other) => Err(format!("unexpected second tick error: {other:?}").into()),
    }
}
