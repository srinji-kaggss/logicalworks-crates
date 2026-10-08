//! The `recovery` task's instrumented world: durable units of work, and the
//! effect that must happen exactly once across an interruption.
//!
//! A module of the same harness-owned crate rather than a second crate: the
//! point of `support/` is that the world is owned by the harness and never by
//! the solution. It is split out of [`crate`] so a reader of the `aggregate` and
//! `pipeline` fixtures is not reading a third instrument they never call.
//!
//! The two halves are the whole task:
//!
//! - [`World::unit`] is a **durable unit**. It counts its own body, records the
//!   value through [`lgwks_bot::script::remember`], and only then returns it. The
//!   `remember` is what makes it replayable: after an interruption, a recorded
//!   unit is replayed without its body running again. The per-index run counter
//!   is what separates "replayed" from "ran again" — both return the same value,
//!   so only the count tells them apart.
//! - [`Ledger`] is the **effect**. [`Ledger::apply`] counts every call, duplicates
//!   included, and nothing here refuses a second apply. Deduplication is the
//!   solution's job, and [`Ledger::applied`] is the only door this crate offers
//!   it: a harness that refused a duplicate would make the clause unfalsifiable,
//!   which is exactly what the clause exists to detect.
//!
//! Every delay is drawn from the seeded SplitMix64 plan the rest of this crate
//! uses, so a seed names one world and a run replays exactly. Every wait goes
//! through [`lgwks_bot::rt::time::sleep`]; there is no `std::thread::sleep`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use lgwks_bot::script::{FlowError, Scope, remember};

/// A unit's delay, drawn from this crate's plan when the oracle configured none.
///
/// One plan governs every instrument in the harness, so a seed names one world
/// across all three tasks rather than a world per module.
fn planned_delay(seed: u64, index: u32) -> Duration {
    crate::planned_delay(seed, u64::from(index))
}

/// Why a durable unit did not produce its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitError {    /// The unit at this index was configured to fail.
    Unit {
        /// The unit's index, which is its identity.
        index: u32,
    },
}

impl fmt::Display for UnitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Unit { index } => write!(formatter, "unit {index} failed"),
        }
    }
}

impl std::error::Error for UnitError {}

/// Why a `recovery` run did not return its total.
///
/// The task's fixed vocabulary, kept beside the world it is measured against:
/// one definition for every arm, so a copy per reference file is how the
/// copies drift. New references re-export this; model authors write their own
/// from the prompt.
#[derive(Debug)]
pub enum RecoveryError {
    /// The overall deadline passed before the total resolved.
    Deadline,
    /// The run has no durable store to replay from.
    NoStore,
    /// The run has no repair ledger to resume against.
    NoLedger,
    /// A unit failed, naming the unit's index.
    Unit { index: u32 },
}

/// A point-in-time reading of the unit instrumentation.
///
/// Every field is private behind an accessor: a caller that could hand-edit what
/// it believes it observed would be able to make a unit that ran twice read as
/// one that ran once, which is the one number the recovery clauses exist to read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UnitStats {
    /// Unit bodies alive right now.
    live: u32,
    /// The most unit bodies alive at once.
    max_live: u32,
    /// How many times each unit's body ran, by index.
    body_runs: Vec<u32>,
}

impl UnitStats {
    /// Unit bodies alive right now.
    #[must_use]
    pub const fn live(&self) -> u32 {
        self.live
    }

    /// The most unit bodies that were alive at once.
    #[must_use]
    pub const fn max_live(&self) -> u32 {
        self.max_live
    }

    /// How many times the body of unit `index` ran.
    ///
    /// [`None`] means *no such unit*: an index outside this world's width, and
    /// so no counter was ever kept for it. That is not the same answer as `Some(0)`,
    /// which is a unit that exists and whose body has not run — a distinction the
    /// recovery clauses depend on, since a missing counter reported as zero would
    /// let a unit that was never attempted read as one that was attempted once
    /// and replayed.
    #[must_use]
    pub fn runs(&self, index: u32) -> Option<u32> {
        let index = usize::try_from(index).ok()?;
        self.body_runs.get(index).copied()
    }

    /// The per-index body-run counters, in index order.
    ///
    /// The slice rather than a mutable view: a reading of the instrumentation
    /// must not hand the caller the right to rewrite it.
    #[must_use]
    pub fn body_runs(&self) -> &[u32] {
        &self.body_runs
    }

    /// The total number of unit-body runs across every index.
    #[must_use]
    pub fn total_runs(&self) -> u32 {
        self.body_runs.iter().copied().sum()
    }
}

/// The counters behind [`UnitStats`].
#[derive(Debug)]
struct UnitCounters {
    /// Live unit bodies.
    live: AtomicU32,
    /// High-water mark of `live`.
    max_live: AtomicU32,
    /// One body-run counter per unit index.
    body_runs: Mutex<Vec<AtomicU32>>,
}

impl UnitCounters {
    /// A counter set sized for `width` units.
    fn new(width: u32) -> Self {
        Self {
            live: AtomicU32::new(0),
            max_live: AtomicU32::new(0),
            body_runs: Mutex::new((0..width).map(|_| AtomicU32::new(0)).collect::<Vec<_>>()),
        }
    }

    /// The counter-vector lock, recovering from poisoning as [`crate::lock`]
    /// documents.
    fn lock(&self) -> MutexGuard<'_, Vec<AtomicU32>> {
        crate::lock(&self.body_runs)
    }

    /// Read every counter into a [`UnitStats`].
    fn snapshot(&self) -> UnitStats {
        let counters = self.lock();
        UnitStats {
            live: self.live.load(Ordering::Acquire),
            max_live: self.max_live.load(Ordering::Acquire),
            body_runs: counters
                .iter()
                .map(|count| count.load(Ordering::Acquire))
                .collect(),
        }
    }

    /// Count one body run of unit `index`.
    ///
    /// Refuses an index with no counter rather than skipping it: the caller has
    /// already refused such an index once, so reaching here is a harness bug,
    /// and a skipped count would leave the recovery clauses reading a body run
    /// that never happened.
    fn count_body(&self, index: u32) -> Result<(), FlowError> {
        let counters = self.lock();
        let counted = match usize::try_from(index) {
            Ok(index) => match counters.get(index) {
                Some(slot) => {
                    slot.fetch_add(1, Ordering::AcqRel);
                    true
                }
                None => false,
            },
            Err(_overflow) => false,
        };
        if counted {
            Ok(())
        } else {
            Err(FlowError::failed(format_args!(
                "unit {index} has no counter in a world of {} units",
                counters.len()
            )))
        }
    }
}

/// Raises the live count on construction and lowers it on drop, so a unit body
/// that is cancelled mid-`sleep` is counted out exactly once.
struct UnitLiveGuard {
    /// The counters to adjust.
    counters: Arc<UnitCounters>,
}

impl UnitLiveGuard {
    /// Enter the live set and update the high-water mark.
    fn enter(counters: Arc<UnitCounters>) -> Self {
        let live = counters
            .live
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        counters.max_live.fetch_max(live, Ordering::AcqRel);
        Self { counters }
    }
}

impl Drop for UnitLiveGuard {
    fn drop(&mut self) {
        self.counters.live.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The durable step name unit `index` records under.
///
/// One definition, because the step name is part of the *path* a durable value
/// is keyed under: a solution that spelled its own names would be measuring its
/// author's naming rather than its recovery. Each index gets its own name, so
/// the units are independently replayable rather than one collapsed record —
/// which is what makes "no completed unit re-ran" a per-unit question.
#[must_use]
pub fn unit_step(index: u32) -> String {
    format!("unit-{index}")
}

/// The `recovery` task's durable units and their shared effect ledger.
///
/// Cloneable and shareable: cloning shares every counter, so a solution that
/// moves clones into concurrent futures is observed as one world.
#[derive(Clone)]
pub struct World {
    /// The shared unit instrumentation.
    units: Arc<UnitCounters>,
    /// The shared effect ledger.
    ledger: Arc<Ledger>,
    /// The indices configured to fail.
    failures: Arc<BTreeSet<u32>>,
    /// Per-index delay overrides.
    delays: Arc<BTreeMap<u32, Duration>>,
    /// The plan seed.
    seed: u64,
    /// How many units this world has.
    width: u32,
}

impl World {
    /// A world of `width` durable units, seeded by `seed`, over an empty ledger.
    #[must_use]
    pub fn new(seed: u64, width: u32) -> Self {
        Self {
            units: Arc::new(UnitCounters::new(width)),
            ledger: Arc::new(Ledger::new()),
            failures: Arc::new(BTreeSet::new()),
            delays: Arc::new(BTreeMap::new()),
            seed,
            width,
        }
    }

    /// Configure `indices` to fail.
    #[must_use]
    pub fn failing<I: IntoIterator<Item = u32>>(mut self, indices: I) -> Self {
        let mut failures: BTreeSet<u32> = (*self.failures).clone();
        failures.extend(indices);
        self.failures = Arc::new(failures);
        self
    }

    /// Override the delay for one unit index.
    #[must_use]
    pub fn delay(mut self, index: u32, delay: Duration) -> Self {
        let mut delays: BTreeMap<u32, Duration> = (*self.delays).clone();
        delays.insert(index, delay);
        self.delays = Arc::new(delays);
        self
    }

    /// How many units this world has.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Read the unit instrumentation.
    #[must_use]
    pub fn stats(&self) -> UnitStats {
        self.units.snapshot()
    }

    /// The effect ledger this world's units may apply.
    #[must_use]
    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    /// Run unit `index` and **record** its value, so a later attempt at the same
    /// step replays that record instead of running this body again.
    ///
    /// This is the harness's side of the durability contract. The body is counted
    /// inside the `remember` closure, so a replayed record does not increment it:
    /// the count answers "did the *effect* happen again", which is the question
    /// the recovery clause asks.
    ///
    /// # Errors
    ///
    /// [`UnitError::Unit`] when `index` is configured to fail, when `index` is
    /// outside this world's width, or when its record could not be written — one
    /// error on purpose, because a caller that could tell them apart would
    /// re-run a unit whose value the store already refused to keep, and the
    /// clause would then measure the harness rather than the solution.
    pub async fn unit(&self, scope: &Scope, index: u32) -> Result<u64, UnitError> {
        // An index past the end has no counter to run and no record to keep, so
        // it is refused before the live guard and the sleep: entering the live
        // set for a unit that cannot exist would report a body running that
        // never did.
        if index >= self.width {
            let refusal = Err(UnitError::Unit { index });
            lgwks_std::trace::warn!(
                "unit {index} was run in a world of width {}: no such unit",
                self.width
            );
            return refusal;
        }
        let _live = UnitLiveGuard::enter(Arc::clone(&self.units));
        // Two delay tiers, most specific first: a per-index override, then the
        // seeded plan.
        let delay = match self.delays.get(&index).copied() {
            Some(override_delay) => override_delay,
            None => planned_delay(self.seed, index),
        };
        lgwks_bot::rt::time::sleep(delay).await;
        if self.failures.contains(&index) {
            Err(UnitError::Unit { index })
        } else {
            self.record_unit(scope, index).await
        }
    }

    /// Record the unit's value under its own step and count the body that ran.
    ///
    /// A record the store refuses is the same error as a configured failure, for
    /// the reason [`Self::unit`] gives; the refusal's own cause goes to stderr so
    /// the two stay distinguishable to whoever reads the run.
    async fn record_unit(&self, scope: &Scope, index: u32) -> Result<u64, UnitError> {
        let counters = Arc::clone(&self.units);
        let step = unit_step(index);
        let value = u64::from(index).saturating_mul(3);
        remember(scope, &step, move || async move {
            // The counter is counted inside the `remember` closure, so a replayed
            // record does not increment it.
            counters.count_body(index)?;
            Ok::<u64, FlowError>(value)
        })
        .await
        .map_err(|cause| {
            crate::diagnostic(format_args!(
                "unit {index}: its record was refused: {cause}"
            ));
            UnitError::Unit { index }
        })
    }
}

/// The effect the recovered run must apply, exactly once.
///
/// [`Ledger::applies`] counts every call and duplicates included, which is the
/// number the duplicate-effect clause reads. [`Ledger::applied`] is the
/// deduplication door, and it is a read rather than a claim: a solution that
/// never asks gets a duplicate like any other.
///
/// `Clone` because a task body is an `Fn`, called once per run: the closure
/// cannot consume the ledger it closes over, so every attempt reaches the same
/// ledger through its own clone. That is the fixture's job to be honest about —
/// one ledger, many handles — and it is why `applies` is an `AtomicU64` under a
/// lock rather than a counter the clone could take with it.
#[derive(Debug, Default, Clone)]
pub struct Ledger {
    /// How many times the effect was applied.
    applies: Arc<AtomicU64>,
    /// The effect names that have been applied.
    names: Arc<Mutex<BTreeSet<String>>>,
}

impl Ledger {
    /// A ledger with nothing applied.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Has `name`'s effect already been applied?
    #[must_use]
    pub fn applied(&self, name: &str) -> bool {
        self.lock().contains(name)
    }

    /// Apply `name`'s effect, unconditionally. Counts every call.
    pub fn apply(&self, name: &str) {
        self.applies.fetch_add(1, Ordering::AcqRel);
        self.lock().insert(name.to_owned());
    }

    /// How many times the effect was applied, duplicates included.
    #[must_use]
    pub fn applies(&self) -> u64 {
        self.applies.load(Ordering::Acquire)
    }

    /// How many distinct effects were applied.
    #[must_use]
    pub fn distinct(&self) -> usize {
        self.lock().len()
    }

    /// The names lock, recovering from poisoning as [`crate::lock`] documents.
    fn lock(&self) -> MutexGuard<'_, BTreeSet<String>> {
        crate::lock(&self.names)
    }
}

/// Whether no unit body is live right now.
///
/// Distinct from "the run finished", which is the solution's own claim: this is
/// the instrument, and it is what the drop clause reads.
#[must_use]
pub fn at_rest(world: &World) -> bool {
    world.stats().live() == 0
}

/// How long the drop clause waits for cancelled unit bodies to be counted out.
///
/// A cancelled body is dropped at the await point that was pending when the run
/// was stopped, so its guard runs on the same turn; the wait is the margin that
/// makes "still live" an observation rather than a race. It is the same 200 ms
/// the other two tasks' drop clauses allow.
pub const SETTLE: Duration = Duration::from_millis(200);
