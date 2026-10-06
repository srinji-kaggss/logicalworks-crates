//! Simulation family: the change-tick path against the value-comparison path.
//!
//! `Observe::revision` is a *second* way to decide that nothing moved, beside
//! the digest that has always decided it. Two ways to answer one question can
//! disagree, and the disagreement is not a slowdown: one of them decides whether
//! an effect fires at all. So this file runs the same workload twice — once
//! through sources that report a revision, once through sources that report
//! `None` and are compared by value — and asserts they fire on the same ticks.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `the_change_tick_and_the_digest_fire_on_the_same_ticks` | one seed, two engines, the identical sequence of (tick, chain, value) and the identical report, plus the revision engine polling no more than the engine it replaces |
//! | `a_revision_that_wraps_is_a_change` | a source whose counter runs off the end of `u64` back to zero still fires, because the comparison is equality and not ordering |
//! | `a_regressing_revision_is_a_change` | a revision that steps *backwards* is a change, and a revision that returns to one already committed is caught by the value comparison rather than trusted |
//! | `a_forced_refresh_beats_a_quiet_revision` | a source that declares its baseline unsound is polled despite reporting an unchanged revision, and the report names the cause |
//! | `the_change_tick_never_fires_twice_for_one_movement` | a movement fires once and then stops, with the source polled exactly as often as the run had movements |
//! | `a_seed_that_changes_the_schedule_changes_the_trace` | two seeds that draw different movement schedules do not hash the same, so the hash is a receipt and not a constant |
//! | `the_same_seed_replays` | the same seed produces the same trace hash, twice |
//!
//! # What is real and what is seeded
//!
//! The bot, the `bevy_ecs` schedule, the ledger, the revision bookkeeping and
//! the whole observation path are the shipped ones, driven through the public
//! `Observe`/`Execute` traits. Seeded is only the *schedule*: which chain's
//! source moves on which tick, and how it moves — forwards, by a wrap, or
//! backwards. No wall clock decides anything, and the trace records which chains
//! fired with which value rather than when, so a run replays exactly.
//!
//! The band sweep comes from `sim::band_of`, the shared seed space every other
//! `sim_*` family draws from, so a seed recorded against this file means the
//! same draw sequence as a seed recorded against `sim_task`.

use crate::sim;

// The band-declaration macro, defined once for the whole layer.
use crate::band_family;

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::rc::Rc;

use lgwks_bot::broker::Broker;
use lgwks_bot::effect::{EnvironmentId, FlowRevision, RunId};
use lgwks_bot::journal::MemoryJournal;
use lgwks_bot::spec::{EffectIdentity, EffectScope};
use lgwks_bot::{
    Auth, Bot, BotError, Cap, EffectLifetime, Execute, GrantSet, Observe, RefreshReason,
};

use sim::Band;
use sim::Rng;

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

// ── The schedule one seed decided ──────────────────────────────────────────

/// How a chain's revision moves on a tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Move {
    /// Nothing is asked of the source this tick.
    Still,
    /// The revision steps forward and the value with it.
    Forward,
    /// The revision runs off the end of `u64` and comes back at zero.
    Wrap,
    /// The revision steps *backwards*.
    Back,
    /// The source declares its baseline unsound, so the tick must read it
    /// whatever the revision says.
    Unsound,
}

/// What one seed decided one chain would do on one tick.
#[derive(Clone, Copy, Debug)]
struct Cell_ {
    /// Which chain the cell belongs to.
    chain: usize,
    /// What it does.
    movement: Move,
}

/// How many chains a run declares.
///
/// A constant rather than a draw, and not because a draw would be inconvenient:
/// `Bot::observe` returns a builder of a *different* type per source, so the
/// erasure boundary makes a runtime chain count uncountable at the call site.
/// Three is also what these families need — a chain that moves, a chain that
/// does not, and a sibling of either — while the width axis is carried to ten
/// thousand chains by `sim_scale`.
const CHAINS: usize = 3;

/// The longest run a plan may decide.
const MAX_TICKS: usize = 8;

/// One cell per chain per tick, and the plan never draws more than that because
/// a chain has at most one movement on a tick.
const MAX_CELLS: usize = CHAINS * MAX_TICKS;

/// The whole run one seed decided.
#[derive(Clone, Copy, Debug)]
struct Plan {
    /// How many ticks the run drives.
    ticks: usize,
    /// The cells, in tick-major order.
    cells: [Cell_; MAX_CELLS],
}

/// A plan drawn from `rng`.
///
/// The shape draw comes first and the movement of each cell after it, so that
/// changing how a *movement* is chosen cannot renumber the shape of another
/// family's schedule. A schedule that shifted wholesale would make a regression
/// in the movement dimension unbisectable against one in the shape.
fn plan(rng: &mut Rng) -> Result<Plan, Box<dyn Error>> {
    let longest = u32::try_from(MAX_TICKS)
        .map_err(|error| format!("the longest run does not fit a u32 on this target: {error}"))?;
    let drawn = rng.between(3, longest);
    let ticks = usize::try_from(drawn)
        .map_err(|error| format!("the drawn tick count does not fit a usize: {error}"))?
        .min(MAX_TICKS);
    let mut cells = [Cell_ {
        chain: 0,
        movement: Move::Still,
    }; MAX_CELLS];
    let mut index = 0;
    for _ in 0..ticks {
        for chain in 0..CHAINS {
            let draw = rng.below(100);
            let movement = match draw {
                // Roughly a third of the cells move, and the movement kinds are
                // drawn inside that: a run where every cell moved would prove
                // nothing about the quiet ticks, and a run with no wraps or
                // regressions would prove nothing about them either.
                0..=32 => match rng.below(4) {
                    0 => Move::Wrap,
                    1 => Move::Back,
                    _ => Move::Forward,
                },
                33..=36 => Move::Unsound,
                _ => Move::Still,
            };
            cells[index] = Cell_ { chain, movement };
            index = index.saturating_add(1);
        }
    }
    Ok(Plan { ticks, cells })
}

impl Plan {
    /// What `chain` does on `tick`, or [`Move::Still`] past the drawn cells.
    fn cell(&self, chain: usize, tick: usize) -> Move {
        let index = tick.saturating_mul(CHAINS).saturating_add(chain);
        self.cells
            .get(index)
            .filter(|cell| cell.chain == chain)
            .map_or(Move::Still, |cell| cell.movement)
    }
}

// ── The rig: three sources, three actions ──────────────────────────────────

/// How a source answers, and what it has been asked for.
///
/// Two answers from one type, because the two engines have to differ in exactly
/// one method and be identical everywhere else — a difference anywhere else
/// would let the equivalence pass for the wrong reason.
struct Counter {
    /// The revision this source would report now.
    current: Rc<Cell<u64>>,
    /// The value this source would return now.
    value: Rc<Cell<u64>>,
    /// What it declares about its own caching, if anything.
    declared: Rc<Cell<Option<RefreshReason>>>,
    /// How many times the substrate polled it.
    polls: Rc<Cell<usize>>,
    /// Capabilities the source needs.
    caps: Vec<Cap>,
    /// Whether this source answers the question with a revision.
    ///
    /// The whole experiment is this one flag, and it is read by
    /// [`Counter::revision`]: the engine that reports `None` is the value
    /// comparison the crate ran before this seam existed, unchanged.
    reports_revision: bool,
}

impl Counter {
    /// A source at revision `revision`, value `value`, that reports a revision
    /// when `reports_revision` is set.
    fn new(revision: u64, value: u64, reports_revision: bool) -> Self {
        Self {
            current: Rc::new(Cell::new(revision)),
            value: Rc::new(Cell::new(value)),
            declared: Rc::new(Cell::new(None)),
            polls: Rc::new(Cell::new(0)),
            caps: Vec::new(),
            reports_revision,
        }
    }
}

impl Observe for Counter {
    type Output = u64;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    fn revision(&self) -> Option<u64> {
        if self.reports_revision {
            Some(self.current.get())
        } else {
            None
        }
    }

    fn cache_state(&self) -> Option<RefreshReason> {
        self.declared.get()
    }

    async fn poll(&self, _call: (Auth, ())) -> Result<u64, BotError> {
        self.polls.set(self.polls.get().saturating_add(1));
        Ok(self.value.get())
    }

    fn domain_id(&self) -> &str {
        "sim::counter"
    }
}

/// An action that records every value it is handed.
struct Tally(Rc<RefCell<Vec<u64>>>);

impl Tally {
    /// A tally over a fresh log.
    fn new() -> (Self, Rc<RefCell<Vec<u64>>>) {
        let log = Rc::new(RefCell::new(Vec::new()));
        (Self(Rc::clone(&log)), log)
    }
}

impl Execute for Tally {
    type Input = u64;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &u64)) -> Result<(), BotError> {
        self.0.borrow_mut().push(*call.1);
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "sim::change_ticks"
    }
}

/// The effect scope a bot runs under.
///
/// One scope, in memory, local: the claim is about change detection and nothing
/// here needs a journal that outlives the process, and an in-memory scope is what
/// lets a run replay without leaving anything behind it.
fn scope() -> Result<EffectScope, Box<dyn Error>> {
    let run = RunId::from_hex("0102030405060708090a0b0c0d0e0f10")?;
    let environment = EnvironmentId::from_hex("2122232425262728292a2b2c2d2e2f30")?;
    let mut broker = Broker::new();
    broker.register(environment)?;
    Ok(EffectScope::new(
        EffectIdentity::new(
            run,
            environment,
            FlowRevision::from_tagged(
                "blake3_256",
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            )?,
        ),
        broker,
        Box::new(MemoryJournal::new()),
    ))
}

/// A bot over [`CHAINS`] sources, and the cells the run moves.
///
/// The three chains are written out rather than looped because the builder's
/// type is per-source: that is the erasure boundary, and asking for a runtime
/// chain count would be asking to cross it at a call site.
struct Subject {
    /// The bot under test.
    bot: Bot,
    /// Per chain, the revision cell its source reads.
    current: [Rc<Cell<u64>>; CHAINS],
    /// Per chain, the value cell its source returns.
    value: [Rc<Cell<u64>>; CHAINS],
    /// Per chain, the declaration cell.
    declared: [Rc<Cell<Option<RefreshReason>>>; CHAINS],
    /// Per chain, the poll counter.
    polls: [Rc<Cell<usize>>; CHAINS],
    /// Per chain, every value its action has been handed, in order.
    tallies: [Rc<RefCell<Vec<u64>>>; CHAINS],
}

impl Subject {
    /// A bot whose sources report revisions when `reports_revision` is set,
    /// every chain sitting at `revision` and `value`.
    fn build(revision: u64, value: u64, reports_revision: bool) -> Result<Self, Box<dyn Error>> {
        let [first, second, third] = [
            Counter::new(revision, value, reports_revision),
            Counter::new(revision, value, reports_revision),
            Counter::new(revision, value, reports_revision),
        ];
        let (tally_first, log_first) = Tally::new();
        let (tally_second, log_second) = Tally::new();
        let (tally_third, log_third) = Tally::new();
        // The cells are read back out of the counters *before* the builder takes
        // them by value, because the builder owns a source and a test that then
        // wanted to watch that source would be holding the half of it the
        // substrate already owns.
        let current = [
            Rc::clone(&first.current),
            Rc::clone(&second.current),
            Rc::clone(&third.current),
        ];
        let value = [
            Rc::clone(&first.value),
            Rc::clone(&second.value),
            Rc::clone(&third.value),
        ];
        let declared = [
            Rc::clone(&first.declared),
            Rc::clone(&second.declared),
            Rc::clone(&third.declared),
        ];
        let polls = [
            Rc::clone(&first.polls),
            Rc::clone(&second.polls),
            Rc::clone(&third.polls),
        ];
        let bot = Bot::builder("sim-change-ticks")
            .observe(first)
            .on(|_: &u64| true, tally_first)
            .observe(second)
            .on(|_: &u64| true, tally_second)
            .observe(third)
            .on(|_: &u64| true, tally_third)
            .with_effects(scope()?)
            .build(&GrantSet::empty())?;
        Ok(Self {
            bot,
            current,
            value,
            declared,
            polls,
            tallies: [log_first, log_second, log_third],
        })
    }

    /// Put every chain's revision and value onto `revision` and `value`.
    fn place(&self, revision: u64, value: u64) {
        for chain in 0..CHAINS {
            self.current[chain].set(revision);
            self.value[chain].set(value);
        }
    }

    /// Clear every chain's declaration.
    ///
    /// Cleared after the tick rather than before the next one so a declaration
    /// covers exactly the tick it was made on — which is what a source that
    /// declares for one tick means, and what makes the report's `forced` rows
    /// attributable to a tick rather than to a window.
    fn clear_declarations(&self) {
        for chain in 0..CHAINS {
            self.declared[chain].set(None);
        }
    }

    /// Every `(tick, chain, value)` its actions have fired since the last call.
    ///
    /// Read as a delta rather than in total, because the substrate decides when
    /// an action runs: attributing a value to the tick it arrived would let a
    /// delayed action look like it fired early.
    fn drain_fired(&self, tick: usize, into: &mut Vec<(usize, usize, u64)>) {
        for chain in 0..CHAINS {
            let values = std::mem::take(&mut *self.tallies[chain].borrow_mut());
            for value in values {
                into.push((tick, chain, value));
            }
        }
    }

    /// How many polls each chain's source has taken.
    fn poll_counts(&self) -> Vec<usize> {
        (0..CHAINS).map(|chain| self.polls[chain].get()).collect()
    }
}

// ── The run ────────────────────────────────────────────────────────────────

/// A revision that is provably not `current`.
///
/// A "movement" that leaves the revision alone is not a movement, and a revision
/// engine is *right* to skip it — so a harness that scheduled one would be
/// reporting its own arithmetic as a disagreement between the two paths, and the
/// disagreement it would have found is a true fact about the wrong thing. Every
/// movement therefore lands on `preferred` unless that is the revision already
/// committed, in which case it lands on the maximum: the step off the bottom of
/// the counter is the same step as the step off the top, and both are changes for
/// the same reason — the comparison is equality.
fn moved_from(current: u64, preferred: u64) -> u64 {
    if preferred != current {
        preferred
    } else {
        // The counter has nowhere to go in the direction the movement named —
        // `Forward` at the ceiling, `Back` at the floor — and a movement that
        // leaves the revision alone is not a movement. Stepping the other way is
        // still a change, and a change is the only property a movement needs to
        // have; which direction it stepped is recorded in the revision itself.
        current.wrapping_sub(1)
    }
}

/// What one engine recorded over a whole run.
#[derive(Clone, Debug, Default)]
struct Log {
    /// `(tick, chain, value)` for every fired action, in order.
    fired: Vec<(usize, usize, u64)>,
    /// How many times each chain's source was polled.
    polls: Vec<usize>,
    /// Every row the tick report published, as `(tick, chain, kind, detail)`.
    report: Vec<(usize, usize, &'static str, String)>,
}

/// Run one engine over `plan` and record what it did.
///
/// `revision_engine` is the whole experiment. Both engines see the identical
/// sequence of revisions and values, tick for tick; the only difference is
/// whether each source answers `Observe::revision`.
fn run(plan: &Plan, revision_engine: bool) -> Result<Log, Box<dyn Error>> {
    let mut subject = Subject::build(0, 0, revision_engine)?;
    let mut log = Log::default();
    // The first tick settles the baseline every chain compares against, so the
    // schedule's first movement is a movement rather than an initial read.
    let _outcome = subject.bot.tick();
    subject.drain_fired(0, &mut log.fired);
    for tick in 0..plan.ticks {
        for chain in 0..CHAINS {
            let current = subject.current[chain].get();
            let held = subject.value[chain].get();
            match plan.cell(chain, tick) {
                Move::Still => {}
                Move::Forward => {
                    subject.current[chain].set(moved_from(current, current.saturating_add(1)));
                    subject.value[chain].set(held.saturating_add(1));
                }
                Move::Wrap => {
                    subject.current[chain].set(moved_from(current, 0));
                    subject.value[chain].set(held.saturating_add(1));
                }
                Move::Back => {
                    // Backwards by one. A revision is an identity rather than a
                    // position, so stepping back is a change; an ordering here
                    // would make a source that counts down silently stop being
                    // believed.
                    subject.current[chain].set(moved_from(current, current.saturating_sub(1)));
                    subject.value[chain].set(held.saturating_add(1));
                }
                Move::Unsound => {
                    subject.declared[chain].set(Some(RefreshReason::Disconnected));
                }
            }
        }
        let _outcome = subject.bot.tick();
        subject.clear_declarations();
        subject.drain_fired(tick.saturating_add(1), &mut log.fired);
        let report = subject.bot.tick_report();
        for forced in report.forced() {
            log.report.push((
                tick,
                forced.chain(),
                "forced",
                forced.reason().as_str().to_owned(),
            ));
        }
        for passed in report.superseded() {
            log.report.push((
                tick,
                passed.chain(),
                "superseded",
                passed.revision().to_string(),
            ));
        }
    }
    log.polls = subject.poll_counts();
    Ok(log)
}

/// Fold one run into the trace, so a seed recorded against this file means the
/// same run in every other.
///
/// Recorded as text rather than through the numeric helper so the trace reads the
/// way the assertion does, and so a run on a 32-bit target records the same
/// labels rather than a different set of them.
fn record(sim: &mut sim::Sim, label: &str, log: &Log) {
    for &(tick, chain, value) in &log.fired {
        sim.record(&format!("{label} fired {tick} {chain} {value}"));
    }
    for (chain, polls) in log.polls.iter().enumerate() {
        sim.record(&format!("{label} polls {chain} {polls}"));
    }
    for &(tick, chain, kind, ref detail) in &log.report {
        sim.record(&format!("{label} report {tick} {chain} {kind} {detail}"));
    }
}

// ── The families ───────────────────────────────────────────────────────────

band_family::band_family! {
    receipt_band_00 => a_seed_that_changes_the_schedule_changes_the_trace, 0;
    receipt_band_01 => a_seed_that_changes_the_schedule_changes_the_trace, 1;
    receipt_band_02 => a_seed_that_changes_the_schedule_changes_the_trace, 2;
    replay_band_00 => the_same_seed_replays, 0;
    replay_band_01 => the_same_seed_replays, 1;
    replay_band_02 => the_same_seed_replays, 2;
    equivalence_band_00 => the_change_tick_and_the_digest_fire_on_the_same_ticks, 0;
    equivalence_band_01 => the_change_tick_and_the_digest_fire_on_the_same_ticks, 1;
    equivalence_band_02 => the_change_tick_and_the_digest_fire_on_the_same_ticks, 2;
    equivalence_band_03 => the_change_tick_and_the_digest_fire_on_the_same_ticks, 3;
    equivalence_band_04 => the_change_tick_and_the_digest_fire_on_the_same_ticks, 4;
    equivalence_band_05 => the_change_tick_and_the_digest_fire_on_the_same_ticks, 5;
    equivalence_band_06 => the_change_tick_and_the_digest_fire_on_the_same_ticks, 6;
    equivalence_band_07 => the_change_tick_and_the_digest_fire_on_the_same_ticks, 7;
}

/// One seed, two engines, one sequence of movements.
///
/// The property the whole file exists for: the revision seam is an optimisation,
/// so it may only ever change *how long* the substrate spends deciding, never
/// what it decides.
fn the_change_tick_and_the_digest_fire_on_the_same_ticks(band: Band) -> TestResult {
    // `sim::sweep` returns the first disagreement as the family's error, with
    // the seed and both engines' records in its message, and the per-seed
    // hashes otherwise. A family that wanted every disagreement at once would
    // collect them here — but it would be reporting a *sweep* rather than a
    // property, and the first one is the one that has to be read.
    sim::sweep(band, |sim| {
        let plan = plan(sim.rng())?;
        let with_revisions = run(&plan, true)?;
        let by_value = run(&plan, false)?;
        if with_revisions.fired != by_value.fired {
            return Err(format!(
                "seed {}: the change-tick engine fired {:?} and the \
                 value-comparison engine fired {:?} for the same movements",
                sim.seed, with_revisions.fired, by_value.fired,
            )
            .into());
        }
        if with_revisions.report != by_value.report {
            return Err(format!(
                "seed {}: the two engines reported different rows: {:?} against {:?}",
                sim.seed, with_revisions.report, by_value.report,
            )
            .into());
        }
        // And the polls: the engines differ here by design, and the revision
        // engine may only poll *less*, never more. A revision engine that polls
        // more than the engine it replaces has bought nothing.
        let total_with: usize = with_revisions.polls.iter().sum();
        let total_without: usize = by_value.polls.iter().sum();
        if total_with > total_without {
            return Err(format!(
                "seed {}: the change-tick engine polled {total_with} times and the \
                 value-comparison engine {total_without}, so the seam cost work",
                sim.seed,
            )
            .into());
        }
        record(sim, "rev", &with_revisions);
        record(sim, "val", &by_value);
        Ok(())
    })?;
    Ok(())
}

/// A source whose revision runs off the end of `u64` and comes back at zero.
///
/// The comparison is equality, so `u64::MAX` → `0` is a change rather than an
/// ordering a wrap could invalidate. If this ever became an ordering the value
/// would still move and the digest engine would still fire — which is why the
/// assertion is on the fired sequence and not on the revision arithmetic.
#[test]
fn a_revision_that_wraps_is_a_change() -> TestResult {
    let mut subject = Subject::build(0, 0, true)?;
    // Sit the committed revision at the end of the counter before the run, so
    // the wrap is a step *off* the end rather than a step onto it.
    subject.place(u64::MAX, 0);
    let _outcome = subject.bot.tick();

    let mut fired = Vec::new();
    // Off the end of the counter and back to zero, with the value moving: the
    // wrap. An ordering comparison would call this smaller rather than different
    // and skip it.
    subject.current[0].set(0);
    subject.value[0].set(1);
    fired.push(subject.bot.tick()?);
    // The same revision held while nothing moves: the skip.
    fired.push(subject.bot.tick()?);
    // And on from there, so the wrap is not the only movement and the tick after
    // it is a quiet one rather than the end of the run.
    subject.current[0].set(1);
    subject.value[0].set(2);
    fired.push(subject.bot.tick()?);

    assert_eq!(
        fired,
        vec![1, 0, 1],
        "the wrap from the maximum back to zero must be a change, and a revision held \
         over a held value must not be: {fired:?}"
    );
    Ok(())
}

/// A revision that steps backwards, and one that returns to a revision already
/// committed.
///
/// Both are handled, by two different rules, which is why both are here. A
/// backwards step is a change because the comparison is equality. A revision that
/// returns to one already committed while the *value* moves is not a revision
/// change at all, and it is caught by the value comparison: the skip is
/// suppressed by the revision and the value decides.
#[test]
fn a_regressing_revision_is_a_change() -> TestResult {
    let mut subject = Subject::build(0, 0, true)?;
    subject.place(10, 0);
    let _outcome = subject.bot.tick();

    let mut fired = Vec::new();
    // Forward, so the substrate commits revision 11 against value 1.
    subject.current[0].set(11);
    subject.value[0].set(1);
    fired.push(subject.bot.tick()?);
    // Backwards to 9, which it has never committed.
    subject.current[0].set(9);
    subject.value[0].set(2);
    fired.push(subject.bot.tick()?);
    // Back to 11, which it *has* committed: the revision is quiet, so the value
    // comparison is all that is left, and the value moved.
    subject.current[0].set(11);
    subject.value[0].set(3);
    fired.push(subject.bot.tick()?);
    // A revision it has never seen, holding a value it has already fired on: the
    // revision says change, the value says quiet, and the revision wins because
    // it is the cheaper answer and it is not contradicted.
    subject.current[0].set(3);
    subject.value[0].set(3);
    fired.push(subject.bot.tick()?);

    assert_eq!(
        fired,
        vec![1, 1, 1, 0],
        "a backwards step and a fresh revision are changes, a committed revision with a \
         moved value is caught by the value comparison, and a fresh revision over an \
         already-seen value fires nothing: {fired:?}"
    );
    Ok(())
}

/// A source that declares its baseline unsound is read whatever its revision.
///
/// The case where believing a revision would be exactly wrong: the substrate has
/// been told its held value is not what the source would report now, and a
/// revision is a claim about a value rather than a repair of one. Without this
/// the skip would turn a forced read into a skipped tick, and the report's
/// `forced` row is what tells a caller its cache was rebuilt.
#[test]
fn a_forced_refresh_beats_a_quiet_revision() -> TestResult {
    let mut subject = Subject::build(0, 0, true)?;
    subject.place(7, 0);
    let _outcome = subject.bot.tick();

    // Nothing moves and nothing is declared: the skip.
    let before = subject.polls[0].get();
    assert_eq!(subject.bot.tick()?, 0, "a quiet revision fires nothing");
    assert_eq!(
        subject.polls[0].get(),
        before,
        "and a quiet revision is not polled, which is the whole saving"
    );

    // The same revision, still quiet as a value, but the source declares its
    // cache unsound. The revision is unchanged, so the skip would apply — and
    // must not.
    subject.declared[0].set(Some(RefreshReason::StaleRemoteKey));
    let _outcome = subject.bot.tick();
    subject.clear_declarations();
    assert!(
        subject.polls[0].get() > before,
        "a source that declared its baseline unsound must be polled even when its \
         revision is unchanged, or a forced read is never taken"
    );
    let report = subject.bot.tick_report();
    assert!(
        !report.forced().is_empty(),
        "and the report must name the chain and the cause, because that is what tells \
         a caller its cache was rebuilt rather than skipped"
    );
    Ok(())
}

/// A movement fires once, and then the tick is quiet again.
///
/// The other half of the equivalence: a skip that fired once and then kept
/// firing would pass a comparison over tick *counts* while being wrong on every
/// tick after the first.
#[test]
fn the_change_tick_never_fires_twice_for_one_movement() -> TestResult {
    let mut subject = Subject::build(0, 0, true)?;
    subject.place(0, 0);
    let _outcome = subject.bot.tick();

    subject.current[0].set(1);
    subject.value[0].set(1);
    assert_eq!(subject.bot.tick()?, 1, "the movement fires once");
    let after_movement = subject.polls[0].get();

    for tick in 0..5 {
        assert_eq!(
            subject.bot.tick()?,
            0,
            "tick {tick} after the movement fired again from a movement that already \
             happened"
        );
    }
    assert_eq!(
        subject.polls[0].get(),
        after_movement,
        "five quiet ticks after a movement poll nothing: a skip that re-polls is not a \
         skip"
    );
    Ok(())
}

/// Two different schedules must not share a trace hash.
///
/// The receipt is only worth something if it distinguishes runs. A scenario where
/// every seed produced the same trace would make `the_same_seed_replays` pass for
/// a run that never varied.
fn a_seed_that_changes_the_schedule_changes_the_trace(band: Band) -> TestResult {
    let mut hashes = Vec::new();
    for seed in band.seeds().take(16) {
        let mut rng = Rng::new(seed);
        let plan = plan(&mut rng)?;
        let mut trace = sim::Trace::new();
        let log = run(&plan, true)?;
        for &(tick, chain, value) in &log.fired {
            trace.record(&format!("rev fired {tick} {chain} {value}"));
        }
        for (chain, polls) in log.polls.iter().enumerate() {
            trace.record(&format!("rev polls {chain} {polls}"));
        }
        hashes.push(trace.hash());
    }
    let mut distinct = hashes.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert!(
        distinct.len() > 1,
        "sixteen seeds produced {} distinct trace(s), so the hash does not distinguish \
         runs and is not a receipt: {hashes:?}",
        distinct.len()
    );
    Ok(())
}

/// The same seed, run twice, is the same run.
///
/// Everything this file claims is a claim about determinism, so a run that
/// cannot be replayed cannot be used to claim anything.
fn the_same_seed_replays(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let plan = plan(sim.rng())?;
        let with_revisions = run(&plan, true)?;
        let by_value = run(&plan, false)?;
        record(sim, "rev", &with_revisions);
        record(sim, "val", &by_value);
        Ok(())
    })
}
