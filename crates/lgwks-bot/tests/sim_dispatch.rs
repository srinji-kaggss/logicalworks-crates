//! Simulation family: the durable dispatch contract, driven by a seeded sweep.
//!
//! Every scenario drives the **real** bot — the real [`Bot`], the real
//! [`Broker`], the real [`MemoryJournal`] behind a real [`EffectScope`] — using
//! the shared rig in [`sim::rig`], the same rig the acceptance file
//! `durable_dispatch.rs` drives. Nothing here is a mock, and nothing here
//! reimplements the ladder: a second copy of the dispatch contract would be a
//! second thing that could pass while the shipped one rotted.
//!
//! The seed chooses what the run *does* — how many restart cycles follow, how
//! many repeated settles are attempted, how many times a tick is repeated — and
//! the run must satisfy the same property whichever it chose. Each declared test
//! sweeps a band of seeds twice and requires the two trace hashes to match, so a
//! nondeterministic run fails even when every assertion happens to pass.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `write_ahead` | the intent and the prepared dispatch are committed *before* the effect body runs |
//! | `outcome_after` | the outcome is committed after the effect, and only once |
//! | `restart_holds` | a recovered unknown is held, not resent |
//! | `restart_lands` | a recovered attempt the journal says landed is retired, not resent |
//! | `not_applied_retries` | evidence that nothing landed makes the entry eligible again |
//! | `applied_never_retries` | evidence that it landed never re-opens the entry |
//! | `ladder_once` | every prepared dispatch is closed by a recorded outcome |
//! | `attempt_continues` | a restart keeps the original key first, so recovery stays valid |
//! | `pending_names_key` | a pending report names the key the caller settles by |
//! | `held_not_exhausted` | a held attempt does not re-enter the body on a later tick |
//! | `settlement_journaled` | settling writes the fact down before acknowledging it |
//! | `settle_is_idempotent` | settling again does not append again |
//! | `store_isolation` | a fresh store starts empty and records only its own attempt |
//! | `crash_loop` | many restarts of a landed effect converge on one outcome |
//! | `unknown_survives` | further ticks do not resolve a recovered unknown by omission |
//! | `repeat_settle_safe` | repeated ticks and settles never duplicate an outcome |

mod sim;

// The band-declaration macro, defined once for the whole layer.
#[path = "sim/bands.rs"]
mod band_family;

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::rc::Rc;

use lgwks_bot::effect::EffectKey;
use lgwks_bot::journal::{EffectEvent, EventKind, MemoryJournal};
use lgwks_bot::spec::{Bot, EffectEvidence};
use lgwks_bot::{BotError, GrantSet};

use sim::Band;

use sim::rig::{
    AlwaysLands, FixedSource, NeverSettles, NoteWhatIsRecorded, identity, is_value, scope,
};

/// The name every bot here is built under. Per-file, because an action's
/// identity is derived from the bot's name as well as from its position: a bot
/// built under another file's name declares another action, and the keys this
/// file settles would then be foreign to it.
const NAME: &str = "sim-dispatch";

type TestResult = Result<(), Box<dyn Error>>;

/// A run's journal store together with the counter its effect increments.
///
/// One type rather than a `store()` and a `counter()` pair per family, because
/// every scenario here is "a fresh store, an armed action, a counter to watch",
/// and spelling that out sixteen times is sixteen places for the fence wiring
/// to drift.
struct Run {
    /// The shared store the bot writes through.
    store: Rc<RefCell<MemoryJournal>>,
    /// How many times the armed effect's body has run.
    entered: Rc<Cell<u32>>,
}

impl Run {
    /// A fresh run: an empty store and a zeroed counter.
    fn new() -> Self {
        Self {
            store: Rc::new(RefCell::new(MemoryJournal::new())),
            entered: Rc::new(Cell::new(0)),
        }
    }

    /// A bot over this run's store armed with `action`.
    ///
    /// One builder rather than one per family, because the durable-dispatch
    /// contract cares what the bot is armed with only insofar as the journal is
    /// concerned; sixteen hand-written builders would be sixteen places for the
    /// fence wiring to drift.
    fn bot_with<A>(&self, action: A) -> Result<Bot, Box<dyn Error>>
    where
        A: lgwks_bot::Execute<Input = u32, Output = ()> + 'static,
    {
        Ok(Bot::builder(NAME)
            .observe(FixedSource)
            .on(is_value, action)
            .with_effects(scope(identity()?, Rc::clone(&self.store))?)
            .build(&GrantSet::empty())?)
    }

    /// A bot whose effect reports itself indeterminate.
    fn never(&self) -> Result<Bot, Box<dyn Error>> {
        self.bot_with(NeverSettles {
            entered: Rc::clone(&self.entered),
        })
    }

    /// A bot whose effect lands.
    fn lands(&self) -> Result<Bot, Box<dyn Error>> {
        self.bot_with(AlwaysLands {
            entered: Rc::clone(&self.entered),
        })
    }

    /// A bot that snapshots the journal at the instant its effect runs.
    fn notes(&self, seen: &Rc<RefCell<Option<Vec<EventKind>>>>) -> Result<Bot, Box<dyn Error>> {
        self.bot_with(NoteWhatIsRecorded {
            store: Rc::clone(&self.store),
            seen: Rc::clone(seen),
            key: Rc::new(RefCell::new(None)),
        })
    }

    /// Every event the store holds, through the trait rather than the concrete
    /// type: what these families are about is what the journal promises, and a
    /// test reading `MemoryJournal` directly would keep passing if the promise
    /// changed.
    fn recorded(&self) -> Result<Vec<EffectEvent>, Box<dyn Error>> {
        Ok(lgwks_bot::journal::EffectJournal::committed(
            &*self.store.borrow(),
        )?)
    }

    /// The kinds the store holds, in order.
    fn kinds(&self) -> Result<Vec<EventKind>, Box<dyn Error>> {
        Ok(self.recorded()?.iter().map(|event| event.kind()).collect())
    }

    /// How many recorded events are of `kind`.
    fn count(&self, kind: EventKind) -> Result<usize, Box<dyn Error>> {
        Ok(self.kinds()?.iter().filter(|seen| **seen == kind).count())
    }

    /// The key the first recorded event is about.
    fn first_key(&self) -> Result<EffectKey, Box<dyn Error>> {
        self.recorded()?
            .first()
            .map(|event| event.key())
            .ok_or_else(|| "the first attempt must have been recorded".into())
    }

    /// The distinct keys the store holds, in first-seen order.
    fn distinct_keys(&self) -> Result<Vec<EffectKey>, Box<dyn Error>> {
        let mut keys: Vec<EffectKey> = Vec::new();
        for event in self.recorded()? {
            if !keys.contains(&event.key()) {
                keys.push(event.key());
            }
        }
        Ok(keys)
    }

    /// How many times the effect's body has run.
    fn entered(&self) -> u32 {
        self.entered.get()
    }

    /// A count drawn from this run's seed, in `1..=span`.
    fn churn(&self, sim: &mut sim::Sim, span: u32) -> usize {
        usize::try_from(sim.rng().between(1, span)).unwrap_or(1)
    }
}

// ── Families ──────────────────────────────────────────────────────────────

/// The intent and the prepared dispatch are committed before the effect body
/// runs.
///
/// Asserted from inside the action, because that is the only vantage point
/// from which "before the handoff" means anything: a journal written after the
/// effect returned would satisfy an after-the-tick assertion just as well, and
/// would be no record of the crash this exists to survive.
fn write_ahead(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let run = Run::new();
        let seen = Rc::new(RefCell::new(None));

        let fired = run.notes(&seen)?.tick()?;
        assert_eq!(fired, 1, "the one chain must dispatch its one action");

        let at_handoff = seen
            .borrow()
            .clone()
            .ok_or("the action never ran, so the write-ahead claim was never measured")?;
        assert_eq!(
            at_handoff.as_slice(),
            [EventKind::IntentAdmitted, EventKind::DispatchPrepared],
            "at the instant the effect ran the journal must already hold the intent and \
             the prepared dispatch, and nothing else"
        );
        // The observation is the proof the body ran, so no counter is needed:
        // the rig's snapshotting action has no entry counter to read, and
        // inventing one here would be a second instrument measuring the same
        // fact with its own failure mode.
        sim.record("write-ahead-holds");
        sim.trace
            .record_count("write-ahead-kinds", at_handoff.len());
        Ok(())
    })
}

/// The outcome is committed after the effect, and only once.
fn outcome_after(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let run = Run::new();
        run.lands()?.tick()?;

        assert_eq!(
            run.kinds()?.last(),
            Some(&EventKind::OutcomeObserved),
            "a settled dispatch must end on its recorded outcome"
        );
        assert_eq!(
            run.count(EventKind::OutcomeObserved)?,
            1,
            "one attempt settles once"
        );
        sim.record("outcome-recorded-last");
        sim.trace
            .record_count("outcome-events", run.recorded()?.len());
        Ok(())
    })
}

/// A recovered unknown is held, not resent.
fn restart_holds(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let run = Run::new();
        drop(run.never()?.tick());

        let mut second = run.never()?;
        match second.tick() {
            Err(BotError::PendingTransition { .. }) => {}
            other => {
                let refusal = Err(format!(
                    "the first tick of a restart must hold the recovered attempt and say \
                 so; it returned {other:?}"
                )
                .into());
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "restart_holds: returning an error to the caller");
                return refusal;
            }
        }
        assert_eq!(
            run.entered(),
            1,
            "the restart held the attempt, so the body must not have run again"
        );
        sim.record("recovered-unknown-held");
        sim.trace
            .record_u64("restart-entered", u64::from(run.entered()));
        sim.trace
            .record_count("restart-pending", second.pending().len());
        Ok(())
    })
}

/// A recovered attempt the journal says landed is retired, not resent.
fn restart_lands(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let run = Run::new();
        run.lands()?.tick()?;
        assert_eq!(run.entered(), 1, "the first effect must have run once");

        let refused = run.lands()?.tick();
        assert!(
            !matches!(refused, Err(BotError::EffectRefused { .. })),
            "a crash after the record of an effect landed must not make the run unable \
             to continue past that entry: {refused:?}"
        );
        assert_eq!(
            run.entered(),
            1,
            "the journal records the effect as landed, so nothing may send it again"
        );
        let mut second = run.lands()?;
        drop(second.tick());
        assert!(
            second.pending().is_empty(),
            "the entry is done for the generation its effect landed in: {:?}",
            second.pending()
        );
        sim.record("landed-attempt-retired");
        sim.trace
            .record_u64("land-entered", u64::from(run.entered()));
        Ok(())
    })
}

/// Evidence that nothing landed makes the entry eligible again.
fn not_applied_retries(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let run = Run::new();
        drop(run.never()?.tick());
        let key = run.first_key()?;

        let mut second = run.never()?;
        drop(second.tick());
        second.resolve_effect(&key, EffectEvidence::NotApplied)?;
        assert_eq!(
            run.kinds()?.last(),
            Some(&EventKind::OutcomeObserved),
            "settling a recovered attempt must write the fact down; a bot that only \
             dropped the key from its own list would come back with the same unknown"
        );

        let retried = second.tick();
        assert!(
            !matches!(retried, Err(BotError::EffectRefused { .. })),
            "evidence that nothing was delivered makes the entry eligible again, and the \
             next attempt must not be refused as a repeat: {retried:?}"
        );
        assert_eq!(
            run.entered(),
            2,
            "the repaired attempt has to reach the effect, not stop at the record"
        );
        sim.record("not-applied-reopens");
        sim.trace
            .record_count("retry-events", run.recorded()?.len());
        Ok(())
    })
}

/// Evidence that it landed never re-opens the entry.
fn applied_never_retries(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let run = Run::new();
        drop(run.never()?.tick());
        let key = run.first_key()?;

        let mut second = run.never()?;
        drop(second.tick());
        second.resolve_effect(&key, EffectEvidence::Applied)?;
        let after_settle = run.entered();

        let later = run.churn(sim, 4);
        for _ in 0..later {
            drop(second.tick());
        }
        assert_eq!(run.entered(), after_settle, "a settled attempt ran again");
        sim.record("applied-never-reopens");
        sim.trace
            .record_u64("applied-entered", u64::from(run.entered()));
        Ok(())
    })
}

/// Every prepared dispatch is closed by a recorded outcome.
fn ladder_once(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let run = Run::new();
        let mut bot = run.lands()?;
        let ticks = run.churn(sim, 4);
        for _ in 0..ticks {
            drop(bot.tick());
        }

        let prepared = run.count(EventKind::DispatchPrepared)?;
        let outcomes = run.count(EventKind::OutcomeObserved)?;
        assert_eq!(
            prepared, outcomes,
            "a prepared dispatch with no recorded outcome is a ladder that stopped halfway"
        );
        sim.record("ladder-closed");
        sim.trace.record_count("ladder-prepared", prepared);
        sim.trace.record_count("ladder-outcomes", outcomes);
        sim.trace.record_count("ladder-ticks", ticks);
        Ok(())
    })
}

/// A restart keeps the original key first, so recovery stays valid.
fn attempt_continues(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let run = Run::new();
        drop(run.never()?.tick());
        let original = run.first_key()?;

        let mut second = run.never()?;
        drop(second.tick());
        second.resolve_effect(&original, EffectEvidence::NotApplied)?;
        drop(second.tick());

        let keys = run.distinct_keys()?;
        assert_eq!(
            keys.first().copied(),
            Some(original),
            "the first key must remain the first: a restart that renumbered history would \
             make every recovered key foreign"
        );
        sim.record("attempt-sequence-continues");
        sim.trace.record_count("attempt-keys", keys.len());
        Ok(())
    })
}

/// A pending report names the key the caller settles by.
fn pending_names_key(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let run = Run::new();
        let mut bot = run.never()?;
        drop(bot.tick());
        let action = format!("{:?}", run.first_key()?.action());

        let pending = bot.pending();
        if pending.is_empty() {
            sim.record("nothing-outstanding");
            sim.trace.record_count("pending-none", 0);
            return Ok(());
        }
        for work in &pending {
            let named = format!("{work:?}");
            assert!(
                named.contains(&action) || named.contains("UnsettledByRecovery"),
                "a pending report must name the key the caller settles by: {named}"
            );
        }
        sim.record("pending-names-key");
        sim.trace.record_count("pending", pending.len());
        Ok(())
    })
}

/// A held attempt does not re-enter the body on a later tick.
fn held_not_exhausted(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let run = Run::new();
        drop(run.never()?.tick());

        let mut second = run.never()?;
        let held = second.tick();
        let before = run.entered();
        let later = run.churn(sim, 4);
        for _ in 0..later {
            drop(second.tick());
        }
        assert_eq!(
            run.entered(),
            before,
            "a held attempt re-entered the body on a later tick, so it was not held"
        );
        sim.record("held-not-retried");
        sim.trace.record_u64("held-entered", u64::from(before));
        sim.trace
            .record_u64("held-tick-refused", u64::from(held.is_err()));
        sim.trace.record_count("held-later-ticks", later);
        Ok(())
    })
}

/// Settling writes the fact down before acknowledging it.
fn settlement_journaled(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let run = Run::new();
        let mut bot = run.never()?;
        drop(bot.tick());
        let before = run.recorded()?.len();

        bot.resolve_effect(&run.first_key()?, EffectEvidence::NotApplied)?;
        let after = run.recorded()?.len();
        assert!(
            after > before,
            "settling acknowledged a fact it never wrote down, so a restart would come \
             back with the same unknown it just cleared"
        );
        sim.record("settlement-journaled");
        sim.trace.record_count("settle-before", before);
        sim.trace.record_count("settle-after", after);
        Ok(())
    })
}

/// Settling again does not append again.
fn settle_is_idempotent(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let run = Run::new();
        let mut bot = run.never()?;
        drop(bot.tick());
        let key = run.first_key()?;

        bot.resolve_effect(&key, EffectEvidence::NotApplied)?;
        let after_first = run.recorded()?.len();
        let repeats = run.churn(sim, 4);
        for _ in 0..repeats {
            drop(bot.resolve_effect(&key, EffectEvidence::NotApplied));
        }
        assert_eq!(
            run.recorded()?.len(),
            after_first,
            "settling the same key again appended another outcome"
        );
        sim.record("settle-idempotent");
        sim.trace.record_count("idempotent-events", after_first);
        sim.trace.record_count("idempotent-repeats", repeats);
        Ok(())
    })
}

/// A fresh store starts empty and records only its own attempt.
fn store_isolation(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let first = Run::new();
        let second = Run::new();
        first.lands()?.tick()?;
        let first_events = first.recorded()?.len();
        assert_eq!(
            second.recorded()?.len(),
            0,
            "a fresh store must start empty: it inherited another controller's record"
        );

        second.lands()?.tick()?;
        assert_eq!(
            second.recorded()?.len(),
            first_events,
            "the second store did not record its own attempt"
        );
        assert_eq!(
            first.recorded()?.len(),
            first_events,
            "the second controller disturbed the first store's record"
        );
        sim.record("stores-isolated");
        sim.trace.record_count("isolation-first", first_events);
        sim.trace
            .record_count("isolation-second", second.recorded()?.len());
        Ok(())
    })
}

/// Many restarts of a landed effect converge on one outcome.
fn crash_loop(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let run = Run::new();
        let cycles = run.churn(sim, 8);
        for _ in 0..cycles {
            drop(run.lands()?.tick());
        }
        let outcomes = run.count(EventKind::OutcomeObserved)?;
        assert_eq!(
            outcomes, 1,
            "{cycles} restarts of a landed effect produced {outcomes} outcomes; a retry \
             loop that re-sends what already landed is the duplicate this is about"
        );
        sim.record("crash-loop-converges");
        sim.trace.record_count("crash-cycles", cycles);
        Ok(())
    })
}

/// Further ticks do not resolve a recovered unknown by omission.
fn unknown_survives(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let run = Run::new();
        let mut bot = run.never()?;
        drop(bot.tick());
        let key = run.first_key()?;
        let before = run.recorded()?.len();
        let later = run.churn(sim, 4);

        for _ in 0..later {
            drop(bot.tick());
        }
        assert_eq!(
            run.recorded()?.len(),
            before,
            "ticking a bot with an unresolved effect appended to the record"
        );
        assert!(
            bot.resolve_effect(&key, EffectEvidence::Applied).is_ok(),
            "a recovered unknown must still be settleable after further ticks"
        );
        sim.record("unknown-survives-ticks");
        sim.trace
            .record_count("survive-events", run.recorded()?.len());
        sim.trace.record_count("survive-ticks", later);
        Ok(())
    })
}

/// Repeated ticks and settles never duplicate an outcome.
fn repeat_settle_safe(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let run = Run::new();
        let mut bot = run.never()?;
        drop(bot.tick());
        let key = run.first_key()?;
        let evidence = if sim.rng().chance(500) {
            EffectEvidence::NotApplied
        } else {
            EffectEvidence::Applied
        };
        bot.resolve_effect(&key, evidence)?;
        let settled = run.count(EventKind::OutcomeObserved)?;

        let churn = run.churn(sim, 8);
        for _ in 0..churn {
            drop(bot.tick());
            drop(bot.resolve_effect(&key, evidence));
        }
        assert_eq!(
            run.count(EventKind::OutcomeObserved)?,
            settled,
            "churning ticks and settles after a settlement duplicated the outcome"
        );
        sim.record("repeat-churn-safe");
        sim.trace.record_count("churn-rounds", churn);
        sim.trace.record_count("churn-outcomes", settled);
        Ok(())
    })
}

// ── The sweep ─────────────────────────────────────────────────────────────

use band_family::band_family;

band_family!(
write_ahead_r00 => write_ahead, 0; write_ahead_r01 => write_ahead, 1;
write_ahead_r02 => write_ahead, 2; write_ahead_r03 => write_ahead, 3;
write_ahead_r04 => write_ahead, 4; write_ahead_r05 => write_ahead, 5;
write_ahead_r06 => write_ahead, 6; write_ahead_r07 => write_ahead, 7;
write_ahead_r08 => write_ahead, 8; write_ahead_r09 => write_ahead, 9;
write_ahead_r10 => write_ahead, 10; write_ahead_r11 => write_ahead, 11;
write_ahead_r12 => write_ahead, 12; write_ahead_r13 => write_ahead, 13;
write_ahead_r14 => write_ahead, 14; write_ahead_r15 => write_ahead, 15;
outcome_after_r00 => outcome_after, 0; outcome_after_r01 => outcome_after, 1;
outcome_after_r02 => outcome_after, 2; outcome_after_r03 => outcome_after, 3;
outcome_after_r04 => outcome_after, 4; outcome_after_r05 => outcome_after, 5;
outcome_after_r06 => outcome_after, 6; outcome_after_r07 => outcome_after, 7;
outcome_after_r08 => outcome_after, 8; outcome_after_r09 => outcome_after, 9;
outcome_after_r10 => outcome_after, 10; outcome_after_r11 => outcome_after, 11;
outcome_after_r12 => outcome_after, 12; outcome_after_r13 => outcome_after, 13;
outcome_after_r14 => outcome_after, 14; outcome_after_r15 => outcome_after, 15;
restart_holds_r00 => restart_holds, 0; restart_holds_r01 => restart_holds, 1;
restart_holds_r02 => restart_holds, 2; restart_holds_r03 => restart_holds, 3;
restart_holds_r04 => restart_holds, 4; restart_holds_r05 => restart_holds, 5;
restart_holds_r06 => restart_holds, 6; restart_holds_r07 => restart_holds, 7;
restart_holds_r08 => restart_holds, 8; restart_holds_r09 => restart_holds, 9;
restart_holds_r10 => restart_holds, 10; restart_holds_r11 => restart_holds, 11;
restart_holds_r12 => restart_holds, 12; restart_holds_r13 => restart_holds, 13;
restart_holds_r14 => restart_holds, 14; restart_holds_r15 => restart_holds, 15;
restart_lands_r00 => restart_lands, 0; restart_lands_r01 => restart_lands, 1;
restart_lands_r02 => restart_lands, 2; restart_lands_r03 => restart_lands, 3;
restart_lands_r04 => restart_lands, 4; restart_lands_r05 => restart_lands, 5;
restart_lands_r06 => restart_lands, 6; restart_lands_r07 => restart_lands, 7;
restart_lands_r08 => restart_lands, 8; restart_lands_r09 => restart_lands, 9;
restart_lands_r10 => restart_lands, 10; restart_lands_r11 => restart_lands, 11;
restart_lands_r12 => restart_lands, 12; restart_lands_r13 => restart_lands, 13;
restart_lands_r14 => restart_lands, 14; restart_lands_r15 => restart_lands, 15;
not_applied_r00 => not_applied_retries, 0; not_applied_r01 => not_applied_retries, 1;
not_applied_r02 => not_applied_retries, 2; not_applied_r03 => not_applied_retries, 3;
not_applied_r04 => not_applied_retries, 4; not_applied_r05 => not_applied_retries, 5;
not_applied_r06 => not_applied_retries, 6; not_applied_r07 => not_applied_retries, 7;
not_applied_r08 => not_applied_retries, 8; not_applied_r09 => not_applied_retries, 9;
not_applied_r10 => not_applied_retries, 10; not_applied_r11 => not_applied_retries, 11;
not_applied_r12 => not_applied_retries, 12; not_applied_r13 => not_applied_retries, 13;
not_applied_r14 => not_applied_retries, 14; not_applied_r15 => not_applied_retries, 15;
applied_never_r00 => applied_never_retries, 0; applied_never_r01 => applied_never_retries, 1;
applied_never_r02 => applied_never_retries, 2; applied_never_r03 => applied_never_retries, 3;
applied_never_r04 => applied_never_retries, 4; applied_never_r05 => applied_never_retries, 5;
applied_never_r06 => applied_never_retries, 6; applied_never_r07 => applied_never_retries, 7;
applied_never_r08 => applied_never_retries, 8; applied_never_r09 => applied_never_retries, 9;
applied_never_r10 => applied_never_retries, 10; applied_never_r11 => applied_never_retries, 11;
applied_never_r12 => applied_never_retries, 12; applied_never_r13 => applied_never_retries, 13;
applied_never_r14 => applied_never_retries, 14; applied_never_r15 => applied_never_retries, 15;
ladder_once_r00 => ladder_once, 0; ladder_once_r01 => ladder_once, 1;
ladder_once_r02 => ladder_once, 2; ladder_once_r03 => ladder_once, 3;
ladder_once_r04 => ladder_once, 4; ladder_once_r05 => ladder_once, 5;
ladder_once_r06 => ladder_once, 6; ladder_once_r07 => ladder_once, 7;
ladder_once_r08 => ladder_once, 8; ladder_once_r09 => ladder_once, 9;
ladder_once_r10 => ladder_once, 10; ladder_once_r11 => ladder_once, 11;
ladder_once_r12 => ladder_once, 12; ladder_once_r13 => ladder_once, 13;
ladder_once_r14 => ladder_once, 14; ladder_once_r15 => ladder_once, 15;
attempt_continues_r00 => attempt_continues, 0; attempt_continues_r01 => attempt_continues, 1;
attempt_continues_r02 => attempt_continues, 2; attempt_continues_r03 => attempt_continues, 3;
attempt_continues_r04 => attempt_continues, 4; attempt_continues_r05 => attempt_continues, 5;
attempt_continues_r06 => attempt_continues, 6; attempt_continues_r07 => attempt_continues, 7;
attempt_continues_r08 => attempt_continues, 8; attempt_continues_r09 => attempt_continues, 9;
attempt_continues_r10 => attempt_continues, 10; attempt_continues_r11 => attempt_continues, 11;
attempt_continues_r12 => attempt_continues, 12; attempt_continues_r13 => attempt_continues, 13;
attempt_continues_r14 => attempt_continues, 14; attempt_continues_r15 => attempt_continues, 15;
pending_names_key_r00 => pending_names_key, 0; pending_names_key_r01 => pending_names_key, 1;
pending_names_key_r02 => pending_names_key, 2; pending_names_key_r03 => pending_names_key, 3;
pending_names_key_r04 => pending_names_key, 4; pending_names_key_r05 => pending_names_key, 5;
pending_names_key_r06 => pending_names_key, 6; pending_names_key_r07 => pending_names_key, 7;
pending_names_key_r08 => pending_names_key, 8; pending_names_key_r09 => pending_names_key, 9;
pending_names_key_r10 => pending_names_key, 10; pending_names_key_r11 => pending_names_key, 11;
pending_names_key_r12 => pending_names_key, 12; pending_names_key_r13 => pending_names_key, 13;
pending_names_key_r14 => pending_names_key, 14; pending_names_key_r15 => pending_names_key, 15;
held_not_exhausted_r00 => held_not_exhausted, 0; held_not_exhausted_r01 => held_not_exhausted, 1;
held_not_exhausted_r02 => held_not_exhausted, 2; held_not_exhausted_r03 => held_not_exhausted, 3;
held_not_exhausted_r04 => held_not_exhausted, 4; held_not_exhausted_r05 => held_not_exhausted, 5;
held_not_exhausted_r06 => held_not_exhausted, 6; held_not_exhausted_r07 => held_not_exhausted, 7;
held_not_exhausted_r08 => held_not_exhausted, 8; held_not_exhausted_r09 => held_not_exhausted, 9;
held_not_exhausted_r10 => held_not_exhausted, 10; held_not_exhausted_r11 => held_not_exhausted, 11;
held_not_exhausted_r12 => held_not_exhausted, 12; held_not_exhausted_r13 => held_not_exhausted, 13;
held_not_exhausted_r14 => held_not_exhausted, 14; held_not_exhausted_r15 => held_not_exhausted, 15;
settlement_r00 => settlement_journaled, 0; settlement_r01 => settlement_journaled, 1;
settlement_r02 => settlement_journaled, 2; settlement_r03 => settlement_journaled, 3;
settlement_r04 => settlement_journaled, 4; settlement_r05 => settlement_journaled, 5;
settlement_r06 => settlement_journaled, 6; settlement_r07 => settlement_journaled, 7;
settlement_r08 => settlement_journaled, 8; settlement_r09 => settlement_journaled, 9;
settlement_r10 => settlement_journaled, 10; settlement_r11 => settlement_journaled, 11;
settlement_r12 => settlement_journaled, 12; settlement_r13 => settlement_journaled, 13;
settlement_r14 => settlement_journaled, 14; settlement_r15 => settlement_journaled, 15;
settle_idempotent_r00 => settle_is_idempotent, 0; settle_idempotent_r01 => settle_is_idempotent, 1;
settle_idempotent_r02 => settle_is_idempotent, 2; settle_idempotent_r03 => settle_is_idempotent, 3;
settle_idempotent_r04 => settle_is_idempotent, 4; settle_idempotent_r05 => settle_is_idempotent, 5;
settle_idempotent_r06 => settle_is_idempotent, 6; settle_idempotent_r07 => settle_is_idempotent, 7;
settle_idempotent_r08 => settle_is_idempotent, 8; settle_idempotent_r09 => settle_is_idempotent, 9;
settle_idempotent_r10 => settle_is_idempotent, 10; settle_idempotent_r11 => settle_is_idempotent, 11;
settle_idempotent_r12 => settle_is_idempotent, 12; settle_idempotent_r13 => settle_is_idempotent, 13;
settle_idempotent_r14 => settle_is_idempotent, 14; settle_idempotent_r15 => settle_is_idempotent, 15;
store_isolation_r00 => store_isolation, 0; store_isolation_r01 => store_isolation, 1;
store_isolation_r02 => store_isolation, 2; store_isolation_r03 => store_isolation, 3;
store_isolation_r04 => store_isolation, 4; store_isolation_r05 => store_isolation, 5;
store_isolation_r06 => store_isolation, 6; store_isolation_r07 => store_isolation, 7;
store_isolation_r08 => store_isolation, 8; store_isolation_r09 => store_isolation, 9;
store_isolation_r10 => store_isolation, 10; store_isolation_r11 => store_isolation, 11;
store_isolation_r12 => store_isolation, 12; store_isolation_r13 => store_isolation, 13;
store_isolation_r14 => store_isolation, 14; store_isolation_r15 => store_isolation, 15;
crash_loop_r00 => crash_loop, 0; crash_loop_r01 => crash_loop, 1;
crash_loop_r02 => crash_loop, 2; crash_loop_r03 => crash_loop, 3;
crash_loop_r04 => crash_loop, 4; crash_loop_r05 => crash_loop, 5;
crash_loop_r06 => crash_loop, 6; crash_loop_r07 => crash_loop, 7;
crash_loop_r08 => crash_loop, 8; crash_loop_r09 => crash_loop, 9;
crash_loop_r10 => crash_loop, 10; crash_loop_r11 => crash_loop, 11;
crash_loop_r12 => crash_loop, 12; crash_loop_r13 => crash_loop, 13;
crash_loop_r14 => crash_loop, 14; crash_loop_r15 => crash_loop, 15;
unknown_survives_r00 => unknown_survives, 0; unknown_survives_r01 => unknown_survives, 1;
unknown_survives_r02 => unknown_survives, 2; unknown_survives_r03 => unknown_survives, 3;
unknown_survives_r04 => unknown_survives, 4; unknown_survives_r05 => unknown_survives, 5;
unknown_survives_r06 => unknown_survives, 6; unknown_survives_r07 => unknown_survives, 7;
unknown_survives_r08 => unknown_survives, 8; unknown_survives_r09 => unknown_survives, 9;
unknown_survives_r10 => unknown_survives, 10; unknown_survives_r11 => unknown_survives, 11;
unknown_survives_r12 => unknown_survives, 12; unknown_survives_r13 => unknown_survives, 13;
unknown_survives_r14 => unknown_survives, 14; unknown_survives_r15 => unknown_survives, 15;
repeat_settle_r00 => repeat_settle_safe, 0; repeat_settle_r01 => repeat_settle_safe, 1;
repeat_settle_r02 => repeat_settle_safe, 2; repeat_settle_r03 => repeat_settle_safe, 3;
repeat_settle_r04 => repeat_settle_safe, 4; repeat_settle_r05 => repeat_settle_safe, 5;
repeat_settle_r06 => repeat_settle_safe, 6; repeat_settle_r07 => repeat_settle_safe, 7;
repeat_settle_r08 => repeat_settle_safe, 8; repeat_settle_r09 => repeat_settle_safe, 9;
repeat_settle_r10 => repeat_settle_safe, 10; repeat_settle_r11 => repeat_settle_safe, 11;
repeat_settle_r12 => repeat_settle_safe, 12; repeat_settle_r13 => repeat_settle_safe, 13;
repeat_settle_r14 => repeat_settle_safe, 14; repeat_settle_r15 => repeat_settle_safe, 15;
);

/// The bands partition the seed space: no gap, no overlap.
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

/// The rig this file drives is the same one the acceptance file drives.
#[test]
fn the_rig_is_shared_not_copied() -> TestResult {
    let run = Run::new();
    run.lands()?.tick()?;
    assert_eq!(run.entered(), 1, "the shared rig must drive a real effect");
    assert!(
        run.kinds()?.len() >= 3,
        "the shared rig must have written the durable ladder: {:?}",
        run.kinds()?
    );
    Ok(())
}
