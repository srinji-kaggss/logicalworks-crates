//! Row-addressed seeded sweeps: one band per T-row, named for its row.
//!
//! The companion to [`t_rows`](super::t_rows), which holds the rows whose
//! falsifier is one real journey. This file holds the rows where the honest
//! oracle is a *sweep*: the seed chooses the shape of the scenario -- how many
//! tenants, which chain fails, how wide a held mass, how many restarts -- and
//! the property must hold for every seed in the band.
//!
//! # Why a sweep is the stronger claim
//!
//! A single run of "tenants never cross" is a sample. A sweep is a statement
//! about every seed in a declared band, and every family here runs its band
//! twice and requires the two trace hashes to match, so a nondeterministic run
//! fails even when every assertion happens to pass. The hash is built from
//! dispositions, counts and committed values, never from elapsed time, which is
//! what makes it a receipt a reader can compare rather than a log they must
//! diff.
//!
//! # What is real and what is seeded
//!
//! The bot, the broker, the real `MemoryJournal` and the real artifact store are
//! all the shipped types. What the seed decides is the scenario, and the clock
//! is never read at all. Nothing here reimplements the contract under test: a
//! simulation with its own copy of the ladder would pass while the shipped
//! ladder rotted.
//!
//! # Naming
//!
//! Every test here ends in its row id, so
//! `cargo nextest list --workspace -E 'test(/_t08$/)'` answers "which tests
//! address T08" without reading the row map.

use crate::sim;

// The band-declaration macro, defined once for the whole layer.
use crate::band_family::band_family;

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::rc::Rc;

use lgwks_bot::journal::{EffectEvent, EffectJournal, MemoryJournal};
use lgwks_bot::spec::{Bot, EffectScope};
use lgwks_bot::{Auth, BotError, Cap, EffectLifetime, Execute, GrantSet, RefreshReason};

use sim::Band;

use sim::rig::{FixedSource, NeverSettles, identity, is_value, scope};

// The one source the observation rows share.
use crate::declarable::Declarable;

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

/// What a tick is expected to do, so the reader of one scenario sees which case
/// it is exercising rather than inferring it from the assertions after.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expect {
    /// The tick fired, or held an already-held entry. Both are facts here.
    Settled,
    /// The tick must report the source's own refusal.
    Refusal,
    /// The tick must report an action whose outcome never arrived, as either of
    /// the two variants a hold takes.
    Hold,
}

/// What an [`Acting`] body reports when it runs.
///
/// One enum rather than one struct per behaviour, because the two behaviours
/// differ only in the outcome they report, and two `Execute` impls would be two
/// copies of the same admission prologue for the sake of a one-line difference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    /// The effect landed, and the value it ran with is recorded.
    Lands,
    /// The acknowledgement never arrived, so the entry is held rather than done.
    Held,
}

/// An action that records its inputs and then lands or holds, per [`Outcome`].
///
/// The held shape is what a starved-or-duplicated walk has to survive: the body
/// genuinely ran, and nothing proved the bytes did not arrive, so the walk must
/// report the entry as outstanding rather than treat its silence as completion.
#[derive(Clone)]
struct Acting {
    /// What this body reports when it runs.
    outcome: Outcome,
    /// Every value it ran with, in order.
    ran: Rc<RefCell<Vec<u32>>>,
    /// How many times the body was entered.
    entered: Rc<Cell<u32>>,
}

impl Acting {
    /// An action that lands, recording each value it ran with.
    fn lands() -> Self {
        Self::with(Outcome::Lands)
    }

    /// An action that holds, counting its entries.
    fn holds() -> Self {
        Self::with(Outcome::Held)
    }

    /// An action with a fresh record and a zeroed counter.
    fn with(outcome: Outcome) -> Self {
        Self {
            outcome,
            ran: Rc::new(RefCell::new(Vec::new())),
            entered: Rc::new(Cell::new(0)),
        }
    }

    /// A handle on this action's own record, cloned so the caller and the action
    /// both read the same vector.
    fn record(&self) -> Rc<RefCell<Vec<u32>>> {
        Rc::clone(&self.ran)
    }

    /// How many times the body ran.
    fn entries(&self) -> u32 {
        self.entered.get()
    }
}

impl Execute for Acting {
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
        self.entered.set(self.entries().saturating_add(1));
        self.ran.borrow_mut().push(*call.1);
        match self.outcome {
            Outcome::Lands => Ok(()),
            Outcome::Held => Err(BotError::EffectIndeterminate {
                domain: "test::acting".to_owned(),
                cause: "the acknowledgement never arrived".to_owned(),
            }),
        }
    }

    fn domain_id(&self) -> &str {
        "test::acting"
    }
}

/// The domain every held chain in the T06 mass reports under.
///
/// One static rather than a minted name per chain: the row is about how many held
/// chains there are, not about telling them apart by name, and a name minted per
/// chain would have to be leaked to outlive the builder that borrows it. The
/// chain index a report names comes from its position in the walk, which is
/// already distinct.
const HELD_DOMAIN: &str = "test::t06_held";

/// The four reasons a source may declare its own baseline unsound, with the
/// spelling a failure message uses.
///
/// One table rather than four literals per call site: T08 asks that *every* reason
/// reach the same repair, and a family that named three of the four would pass
/// while the fourth went unexercised.
const CAUSES: [(RefreshReason, &str); 4] = [
    (RefreshReason::Disconnected, "disconnect"),
    (RefreshReason::WatchOverflow, "watch overflow"),
    (RefreshReason::StaleRemoteKey, "stale remote key"),
    (RefreshReason::InvalidationFailed, "invalidation failure"),
];

/// The effect scope a bot in this file dispatches through.
fn scope_of(store: &Rc<RefCell<MemoryJournal>>) -> Result<EffectScope, Box<dyn Error>> {
    scope(identity()?, Rc::clone(store))
}

/// Every committed event in `store`, through the trait rather than the concrete
/// adapter: what these families are about is what the journal promises, and a
/// test reading `MemoryJournal` directly would keep passing if the promise
/// changed.
fn committed(store: &Rc<RefCell<MemoryJournal>>) -> Result<Vec<EffectEvent>, Box<dyn Error>> {
    Ok(EffectJournal::committed(&*store.borrow())?)
}

/// Tick `bot` and check the outcome against `expect`.
///
/// One helper rather than a `tick` and a `refused_tick`, because they differ only
/// in which arm of the same `match` they treat as the failure: spelled twice, the
/// two would drift and a scenario would quietly stop checking the refusal it
/// exists to provoke. Ticking returns `Err` for a hold as well as for a fault,
/// and several rows are about a run that fires nothing precisely because
/// something was held, so `Anything` accepts both.
fn tick_as(bot: &mut Bot, expect: Expect) -> TestResult {
    let held = |error: &BotError| {
        matches!(
            error,
            BotError::EffectIndeterminate { .. } | BotError::PendingTransition { .. }
        )
    };
    match (bot.tick(), expect) {
        (Ok(fired), Expect::Settled) => {
            assert_eq!(
                fired,
                bot.tick_report().fired(),
                "the tick's count and its report are one fact read twice"
            );
            Ok(())
        }
        (Err(error), Expect::Settled) if held(&error) => Ok(()),
        (Ok(fired), Expect::Refusal) => Err(format!(
            "a source that refused read as a {fired}-effect quiet tick; the refusal has to be \
             reported"
        )
        .into()),
        (Err(error), Expect::Refusal) => {
            assert!(
                matches!(error, BotError::DomainError { .. }),
                "the refusal keeps the source's own type: {error:?}"
            );
            Ok(())
        }
        (Ok(fired), Expect::Hold) => Err(format!(
            "an action whose outcome was unknown read as a {fired}-effect clean tick; the hold \
             has to be reported"
        )
        .into()),
        (Err(error), Expect::Hold) => {
            assert!(
                held(&error),
                "the hold keeps a typed variant, because a retry classifier reads it and \
                 neither arm of this match is a decision: {error:?}"
            );
            Ok(())
        }
        (Err(error), _) => Err(format!("the tick failed with {error:?}").into()),
    }
}

/// One chain's committed values, recorded into `sim`'s trace.
///
/// A helper rather than a `record_u64` per call site, because a trace that
/// recorded a labelled value at one site and a bare one at another would make two
/// families' hashes incomparable, which is the one thing the receipt exists to
/// prevent.
fn record_values(sim: &mut sim::Sim, label: &str, values: impl Iterator<Item = u32>) {
    for value in values {
        sim.trace.record_u64(label, u64::from(value));
    }
}

// ── T08: every declared reason forces a refresh ─────────────────────────────

/// T08: each of the four declared reasons forces the next tick to re-read the
/// source rather than settling into permanent quiet, and the forced read is
/// reported as forced with that reason.
///
/// The counterexample this closes: a source declares its baseline unsound, the
/// substrate compares the next read against the value it already holds, sees
/// *equal*, and never looks again. Zero effects fired is then indistinguishable
/// from a healthy source that held still.
fn every_declared_reason_forces_a_refresh(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        for (cause, spelling) in CAUSES {
            let source = Declarable::new(1, "test::t08_source");
            let (_value, reason, _refusing, polls) = source.handles();
            let store = Rc::new(RefCell::new(MemoryJournal::new()));
            let action = Acting::lands();
            let ran = action.record();
            let mut bot = Bot::builder("sim-t08")
                .observe(source)
                .on(|observed: &u32| *observed > 0, action)
                .with_effects(scope_of(&store)?)
                .build(&GrantSet::empty())?;

            // A clean baseline tick, so nothing is armed and nothing should be
            // forced: a bot that forced every source would satisfy this row
            // without reading anything.
            tick_as(&mut bot, Expect::Settled)?;
            let baseline = polls.get();
            assert!(
                baseline >= 1,
                "{spelling}: the baseline tick must have polled the source"
            );
            assert!(
                !bot.tick_report().forced_any(),
                "{spelling}: an undeclared source is not forced"
            );
            let fired_before = ran.borrow().len();
            assert!(
                fired_before >= 1,
                "{spelling}: the baseline tick must have fired its entry once"
            );

            // The value has not moved at all, and the baseline is declared
            // unsound. A correct implementation re-reads anyway.
            reason.set(Some(cause));
            tick_as(&mut bot, Expect::Settled)?;
            assert!(
                polls.get() > baseline,
                "{spelling}: the declared-unsound source was re-read"
            );
            assert_eq!(
                bot.tick_report()
                    .forced()
                    .first()
                    .map(|forced| forced.reason()),
                Some(cause),
                "{spelling}: the report names the cause it forced for"
            );
            assert_eq!(
                ran.borrow().len(),
                fired_before,
                "{spelling}: a refresh of an unchanged value commits nothing and fires nothing, \
                 so the repair is not 're-read forever'"
            );

            // The declaration is spent by the read it forced, so the next tick
            // has a sound baseline and is not forced.
            reason.set(None);
            tick_as(&mut bot, Expect::Settled)?;
            assert!(
                !bot.tick_report().forced_any(),
                "{spelling}: a source that declared nothing is not forced, so the previous \
                 declaration was spent by the read it forced"
            );
            sim.record(spelling);
        }
        Ok(())
    })
}

// ── T07: one chain's failed group never rolls back another's commit ─────────

/// T07: one chain succeeds on a new fingerprint, the other chain's atomic
/// observation group is then refused twice, and after that chain recovers the
/// first chain's uncommitted update has still executed exactly once.
///
/// The counterexample this closes is a *group* failure contaminating a sibling:
/// if the substrate treated one chain's refused observation as a reason to doubt
/// every chain's, the healthy chain's newer value would be erased, duplicated or
/// silently re-fired. The row's observable is the healthy chain's run record --
/// one entry per executed update, in order, across the whole sequence.
///
/// Two facts about the substrate shape the sequence, and both are measured rather
/// than assumed. A settled chain re-fires only when its source declares a new
/// fingerprint, which is what "A succeeds with a new fingerprint" means. And a
/// refusal ends the tick before anything dispatches, so A's update lands on a
/// tick of its own rather than on the tick B fails -- which is exactly why the
/// record, not the tick, is the oracle.
fn a_failed_group_leaves_its_own_chain_forced(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let broken = Declarable::new(1, "test::t07_broken");
        let (broken_value, broken_reason, broken_refusing, broken_polls) = broken.handles();
        let healthy = Declarable::new(1, "test::t07_healthy");
        let (healthy_value, healthy_reason, _, _) = healthy.handles();
        let broken_chain = Acting::lands();
        let broken_ran = broken_chain.record();
        let healthy_chain = Acting::lands();
        let healthy_ran = healthy_chain.record();
        let store = Rc::new(RefCell::new(MemoryJournal::new()));
        let mut bot = Bot::builder("sim-t07")
            .observe(healthy)
            .on(|observed: &u32| *observed > 0, healthy_chain)
            .observe(broken)
            .on(|observed: &u32| *observed > 0, broken_chain)
            .with_effects(scope_of(&store)?)
            .build(&GrantSet::empty())?;

        tick_as(&mut bot, Expect::Settled)?;
        assert_eq!(healthy_ran.borrow().as_slice(), [1], "baseline commit");
        assert_eq!(broken_ran.borrow().as_slice(), [1], "baseline commit");

        // A succeeds on a new fingerprint.
        healthy_reason.set(Some(RefreshReason::StaleRemoteKey));
        healthy_value.set(2);
        tick_as(&mut bot, Expect::Settled)?;
        assert_eq!(
            healthy_ran.borrow().as_slice(),
            [1, 2],
            "the new fingerprint committed the newer value and fired it once"
        );

        // B's observation group is refused, and stays refused: the mark must
        // outlive the tick that made it, or B would go quiet for the same reason
        // it was quiet before.
        broken_reason.set(Some(RefreshReason::Disconnected));
        broken_refusing.set(true);
        let polls_before = broken_polls.get();
        tick_as(&mut bot, Expect::Refusal)?;
        assert!(
            broken_polls.get() > polls_before,
            "the refused chain was re-read"
        );
        tick_as(&mut bot, Expect::Refusal)?;
        assert_eq!(
            broken_ran.borrow().as_slice(),
            [1],
            "a chain whose group never landed commits nothing, however often it is re-read"
        );
        assert_eq!(
            healthy_ran.borrow().as_slice(),
            [1, 2],
            "and its sibling's update was neither erased nor duplicated by the failure: this \
             is the row's whole claim"
        );

        // B recovers, and commits exactly once.
        broken_refusing.set(false);
        broken_value.set(5);
        tick_as(&mut bot, Expect::Settled)?;
        assert_eq!(
            broken_ran.borrow().as_slice(),
            [1, 5],
            "the recovered chain executes its own update exactly once"
        );
        assert_eq!(
            healthy_ran.borrow().as_slice(),
            [1, 2],
            "and its sibling's record is unchanged"
        );

        // Subsequent changes on both chains: the first move was not a one-shot.
        healthy_reason.set(Some(RefreshReason::StaleRemoteKey));
        healthy_value.set(3);
        broken_reason.set(Some(RefreshReason::Disconnected));
        broken_value.set(6);
        tick_as(&mut bot, Expect::Settled)?;
        assert_eq!(
            healthy_ran.borrow().as_slice(),
            [1, 2, 3],
            "a subsequent new fingerprint on the healthy chain fires once"
        );
        assert_eq!(
            broken_ran.borrow().as_slice(),
            [1, 5, 6],
            "and a subsequent one on the recovered chain fires once"
        );

        record_values(sim, "broken", broken_ran.borrow().iter().copied());
        record_values(sim, "healthy", healthy_ran.borrow().iter().copied());
        Ok(())
    })
}

// ── T06: a saturated mass never starves an independent chain ────────────────

/// T06: a drawn mass of held chains, each of which enters its action once and
/// then reports itself indeterminate, never starves the independent chain
/// declared beside it.
///
/// The seed draws the mass, because "a hundred held chains" is a sample and a
/// thousand is a different claim. The oracle is a controlled completion -- a held
/// action's own reported outcome -- and never a sleep.
fn a_held_mass_never_starves_an_independent_chain(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let mass = sim.rng().between(2, 64);
        let independent = Acting::lands();
        let independent_ran = independent.record();
        let store = Rc::new(RefCell::new(MemoryJournal::new()));

        // Declared first, so it is chain 0 and its effect is the first the walk
        // reaches -- the position a starved walk would lose.
        let mut builder = Bot::builder("sim-t06")
            .observe(Declarable::new(1, "test::t06_independent"))
            .on(|observed: &u32| *observed > 0, independent);
        for _ in 0..mass {
            builder = builder
                .observe(Declarable::new(1, HELD_DOMAIN))
                .on(|observed: &u32| *observed > 0, Acting::holds());
        }
        let mut bot = builder
            .with_effects(scope_of(&store)?)
            .build(&GrantSet::empty())?;

        // The held chains each report their own indeterminate outcome. The walk
        // reports the first hold; the chains behind it still ran, because a walk
        // that stopped at the first hold would leave them unentered, which is the
        // starvation this row forbids.
        tick_as(&mut bot, Expect::Hold)?;

        assert_eq!(
            independent_ran.borrow().as_slice(),
            [1],
            "at a mass of {mass} the independent chain's action still ran"
        );
        assert_eq!(
            bot.source_domains().len(),
            usize::try_from(mass)?.saturating_add(1),
            "the bot declares one independent chain and {mass} held ones"
        );
        assert_eq!(
            bot.pending().len(),
            usize::try_from(mass)?,
            "every held chain is reported as held rather than silently dropped"
        );
        sim.trace.record_u64("mass", u64::from(mass));
        Ok(())
    })
}

// ── T10: a required predecessor's state gates its dependent ────────────────

/// T10: over a drawn number of recovery cycles, a predecessor whose effect never
/// settled keeps its dependent from running -- the dependent does not proceed
/// merely because the predecessor is no longer active.
///
/// The seed draws the number of cycles rather than the states, because the
/// property under test is that *no* number of restarts resolves the dependency by
/// omission.
fn a_required_predecessor_holds_its_dependent(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let cycles = sim.rng().between(1, 8);
        let store = Rc::new(RefCell::new(MemoryJournal::new()));
        let entered = Rc::new(Cell::new(0));
        let dependent = Acting::lands();
        let dependent_ran = dependent.record();

        for cycle in 0..cycles {
            // Every cycle rebuilds the bot over the same journal, which is what a
            // restart does: the recovered attempt is in the store and nothing has
            // told the store it landed.
            let mut bot = Bot::builder("sim-t10")
                .observe(FixedSource)
                .on(
                    is_value,
                    NeverSettles {
                        entered: Rc::clone(&entered),
                    },
                )
                .on(|_: &u32| true, dependent.clone())
                .with_effects(scope_of(&store)?)
                .build(&GrantSet::empty())?;
            tick_as(&mut bot, Expect::Hold)?;
            assert_eq!(
                dependent_ran.borrow().len(),
                0,
                "cycle {cycle}: the dependent ran behind a predecessor the journal never settled"
            );
        }
        assert_eq!(
            entered.get(),
            1,
            "however many cycles ran, the predecessor's body was entered exactly once: a \
             recovered attempt that is held is not re-entered, and the dependent never ran"
        );
        assert!(
            !committed(&store)?.is_empty(),
            "the attempt is on the record, so nothing resolves by omission"
        );
        sim.trace.record_u64("cycles", u64::from(cycles));
        Ok(())
    })
}

// ── T11: a late settlement stays attributable to its own attempt ────────────

/// T11: a predecessor that reported itself indeterminate is never re-entered
/// across a drawn number of restarts, and every restart holds it under the same
/// key rather than resolving it by omission.
///
/// The row's subject is attribution: an effect whose outcome never arrived cannot
/// be re-sent, because the bytes may well have landed. The seed draws how many
/// restarts happen, so "one restart held it" is not the whole claim.
fn an_indeterminate_predecessor_is_never_resent(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let restarts = sim.rng().between(1, 8);
        let store = Rc::new(RefCell::new(MemoryJournal::new()));
        let entered = Rc::new(Cell::new(0));
        let rebuild = || -> Result<Bot, Box<dyn Error>> {
            Ok(Bot::builder("sim-t11")
                .observe(FixedSource)
                .on(
                    is_value,
                    NeverSettles {
                        entered: Rc::clone(&entered),
                    },
                )
                .with_effects(scope_of(&store)?)
                .build(&GrantSet::empty())?)
        };

        let mut first = rebuild()?;
        tick_as(&mut first, Expect::Hold)?;
        let after_first = entered.get();
        let key = committed(&store)?
            .first()
            .map(|event| event.key())
            .ok_or("the attempt was not on the record")?;

        for restart in 0..restarts {
            let mut rebuilt = rebuild()?;
            let held = rebuilt.pending();
            assert_eq!(
                held.len(),
                1,
                "restart {restart}: the recovered unknown is reported, not forgotten"
            );
            assert_eq!(
                held[0].key(),
                Some(key),
                "restart {restart}: and it names the same key, so the hold is attributable"
            );
            tick_as(&mut rebuilt, Expect::Hold)?;
            assert_eq!(
                entered.get(),
                after_first,
                "restart {restart}: the body was not re-entered, so nothing was sent twice"
            );
        }
        sim.trace.record_u64("restarts", u64::from(restarts));
        Ok(())
    })
}

// ── T12: one tenant's store is not another's ───────────────────────────────

/// T12: a drawn number of tenants, each holding the *same bytes*, commit one
/// artifact each and read back only their own -- identical digests across tenants
/// must not become a shared shelf, a shared count, or a shared read.
#[cfg(feature = "script")]
fn identical_digests_never_cross_tenants(band: Band) -> TestResult {
    use lgwks_bot::proposal::ArtifactStore;

    sim::assert_replays(band, |sim| {
        let tenants = sim.rng().between(2, 16);
        let store = ArtifactStore::new();
        let bytes = b"the same bytes from many tenants".to_vec();
        let digest = ArtifactStore::digest_of(&bytes)?;
        let names: Vec<String> = (0..tenants)
            .map(|index| format!("tenant-{index}"))
            .collect();

        for tenant in &names {
            store.write(tenant, &bytes)?;
            assert!(
                store.holds(tenant, &digest),
                "{tenant}: holds its own artifact under a digest every tenant shares"
            );
            assert!(
                store.read(tenant, &digest).is_some(),
                "{tenant}: reads its own bytes"
            );
            assert_eq!(
                store.artifacts(tenant),
                1,
                "{tenant}: exactly one artifact, however many writers share its digest"
            );
            assert_eq!(
                store.writers(tenant, &digest),
                1,
                "{tenant}: the writer count under a shared digest is this tenant's own, never a \
                 neighbour's"
            );
        }
        assert!(
            store.read("a-tenant-that-never-wrote", &digest).is_none(),
            "a tenant that never wrote reads nothing under a digest it does not own"
        );
        sim.trace.record_u64("tenants", u64::from(tenants));
        Ok(())
    })
}

// ── The families, one declared test per band ───────────────────────────────

band_family! {
    every_declared_reason_forces_a_refresh_t08 => every_declared_reason_forces_a_refresh, 36;
    a_failed_group_leaves_its_own_chain_forced_t07 => a_failed_group_leaves_its_own_chain_forced, 37;
    a_held_mass_never_starves_an_independent_chain_t06 => a_held_mass_never_starves_an_independent_chain, 38;
    a_required_predecessor_holds_its_dependent_t10 => a_required_predecessor_holds_its_dependent, 39;
    an_indeterminate_predecessor_is_never_resent_t11 => an_indeterminate_predecessor_is_never_resent, 40;
}

// `ArtifactStore` lives in `proposal`, which is gated on `script`.
#[cfg(feature = "script")]
band_family! {
    identical_digests_never_cross_tenants_t12 => identical_digests_never_cross_tenants, 41;
}
