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
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use lgwks_bot::script::{FlowError, Scope, remember};

/// A unit's delay, drawn from the plan when the oracle configured none.
///
/// The same SplitMix64 the rest of this crate draws with, so one plan governs
/// every instrument in the harness and a seed means one world.
fn planned_delay(seed: u64, index: u32) -> Duration {
    let mut state = seed
        .wrapping_add(0x9E37_79B9_7F4A_7C15)
        .wrapping_mul(0xBF58_476D_1CE4_E5B9)
        .wrapping_add(u64::from(index));
    state = (state ^ (state >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    Duration::from_millis(1 + ((state ^ (state >> 27)) >> 31) % 4)
}

/// Why a durable unit did not produce its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitError {
    /// The unit at this index was configured to fail.
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
    /// A missing index reads as zero rather than as a panic, because the answer
    /// to "did this unit's body run" must be a number for every index a caller
    /// asks about, including one this world is narrower than.
    #[must_use]
    pub fn runs(&self, index: u32) -> u32 {
        self.body_runs
            .get(index as usize)
            .copied()
            .unwrap_or_default()
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

    /// The counter-vector lock, recovering from poisoning.
    fn lock(&self) -> MutexGuard<'_, Vec<AtomicU32>> {
        self.body_runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
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
        let live = counters.live.fetch_add(1, Ordering::AcqRel).saturating_add(1);
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
    /// [`UnitError::Unit`] when `index` is configured to fail, or when its record
    /// could not be written — the two are one error on purpose, because a caller
    /// that cannot tell them apart would re-run a unit whose value the store
    /// already refused to keep, and the clause would then measure the harness
    /// rather than the solution.
    pub async fn unit(&self, scope: &Scope, index: u32) -> Result<u64, UnitError> {
        let _live = UnitLiveGuard::enter(Arc::clone(&self.units));
        let delay = self
            .delays
            .get(&index)
            .copied()
            .unwrap_or_else(|| planned_delay(self.seed, index));
        lgwks_bot::rt::time::sleep(delay).await;
        if self.failures.contains(&index) {
            return Err(UnitError::Unit { index });
        }
        let counters = Arc::clone(&self.units);
        let step = unit_step(index);
        let value = u64::from(index).saturating_mul(3);
        remember(scope, &step, move || async move {
            if let Some(slot) = counters.lock().get(index as usize) {
                slot.fetch_add(1, Ordering::AcqRel);
            }
            Ok::<u64, FlowError>(value)
        })
        .await
        .map_err(|_| UnitError::Unit { index })
    }
}

/// The effect the recovered run must apply, exactly once.
///
/// [`Ledger::applies`] counts every call and duplicates included, which is the
/// number the duplicate-effect clause reads. [`Ledger::applied`] is the
/// deduplication door, and it is a read rather than a claim: a solution that
/// never asks gets a duplicate like any other.
#[derive(Debug, Default)]
pub struct Ledger {
    /// How many times the effect was applied.
    applies: AtomicU64,
    /// The effect names that have been applied.
    names: Mutex<BTreeSet<String>>,
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

    /// The names lock, recovering from poisoning.
    fn lock(&self) -> MutexGuard<'_, BTreeSet<String>> {
        self.names.lock().unwrap_or_else(PoisonError::into_inner)
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

/// A one-shot flag the oracle can raise from outside a parked run, so an
/// interrupted attempt has a way to be released rather than only a way to be
/// killed.
#[derive(Debug, Clone, Default)]
pub struct Signal {
    /// Whether the signal has been raised.
    raised: Arc<AtomicBool>,
}

impl Signal {
    /// A signal that has not been raised.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Raise it. Idempotent: raising twice is one raise.
    pub fn raise(&self) {
        self.raised.store(true, Ordering::SeqCst);
    }

    /// Has it been raised?
    #[must_use]
    pub fn raised(&self) -> bool {
        self.raised.load(Ordering::SeqCst)
    }

    /// Wait until it is raised, on the engine's clock rather than a thread sleep.
    pub async fn wait(&self) {
        while !self.raised() {
            lgwks_bot::rt::time::sleep(Duration::from_millis(1)).await;
        }
    }
}