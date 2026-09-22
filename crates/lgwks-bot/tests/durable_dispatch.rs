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

use lgwks_bot::broker::Broker;
use lgwks_bot::effect::{EffectKey, EnvironmentId, FlowRevision, RunId};
use lgwks_bot::journal::{
    DurabilityPromise, DurableAck, EffectEvent, EffectJournal, EventKind, JournalError,
    JournalPosition, MemoryJournal,
};
use lgwks_bot::spec::{Bot, EffectEvidence, EffectIdentity, EffectScope, TransitionHold};
use lgwks_bot::{Auth, BotError, Cap, Execute, GrantSet, Observe};

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
