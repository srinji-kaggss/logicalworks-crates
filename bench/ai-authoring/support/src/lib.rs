//! Harness-owned instrumented world for the fixed-model AI-authoring benchmark.
//!
//! Two instruments live here, one per task the model is asked to author:
//!
//! - [`Fetcher`]: the `aggregate` task's async, fallible fetch. It records how
//!   many fetches are live, the high-water mark, how many started, how many
//!   started after the first failure was observed, and how many completed or
//!   failed. A drop guard decrements the live count, so a fetch whose future is
//!   cancelled (or whose task is aborted) is counted out too.
//! - [`Stage`]: the `pipeline` task's named stages (`fetch_a`, `fetch_b`,
//!   `combine`, `publish`), with configurable failures and slow stages, and the
//!   same live-count instrumentation.
//! - [`recovery::World`]: the `recovery` task's durable units and their shared
//!   effect ledger, in a module of its own so a reader of the two above is not
//!   reading a third instrument they never call.
//!
//! The newer tasks' fixed vocabularies live here too, one module per task, so
//! every arm's reference solution re-exports one definition rather than
//! copying the prompt's items per file:
//!
//! - [`capture`]: the `capture` task's error and bounded result.
//! - [`tenant_cache`]: the `tenant-cache` task's refusal.
//! - [`durable`]: the `durable-retry` task's recovery error.
//!
//! The world is owned by the harness, never by the solution: a solution calls
//! these methods and returns the numbers; the oracle reads the counters. Nothing
//! here sleeps on a wall clock except through [`lgwks_bot::rt::time::sleep`].

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// Locks `mutex`, recovering from poisoning.
///
/// Every `Mutex` in this crate guards either a copyable snapshot or a
/// `StageValues`, and every write to one is a single whole-struct assignment:
/// there is no path that leaves a guarded value half-updated, because the
/// assignment either stores every field or stores none. Poisoning records that
/// some thread panicked while holding the guard, not that the value became
/// inconsistent, so taking the poisoned guard's inner value keeps the
/// instrumentation reading what actually happened to the run rather than
/// discarding every count after the first panic.
///
/// This is the one place in the crate that decides what a poisoned lock means,
/// so [`recovery`](crate::recovery) uses it too instead of repeating the
/// recovery per mutex.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// The `recovery` task's durable units and its effect ledger.
///
/// A module of this crate rather than a second one, and a module rather than
/// more items at the root because it is a third instrument the other two tasks
/// never call: a reader of [`Fetcher`] and [`Stage`] should not have to read it.
pub mod recovery;

/// The `capture` task's fixed error and bounded result.
pub mod capture;

/// The `tenant-cache` task's fixed refusal.
pub mod tenant_cache;

/// The `durable-retry` task's fixed recovery error.
pub mod durable;

/// The `aggregate` task's fixed sum error, for references that re-export one
/// definition rather than copying the prompt's items per file.
pub mod aggregate;

/// The `pipeline` task's fixed stage error, for the same reason.
pub mod pipeline;

// ── The deterministic delay plan ─────────────────────────────────────────────

/// SplitMix64, so a plan delay is a pure function of `(seed, key)` and a run
/// replays exactly. No entropy, no wall clock, no third-party RNG.
fn splitmix64(mut state: u64) -> u64 {
    state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A delay in `1..=4 ms`, drawn from the seeded plan.
///
/// One plan for every instrument in the harness, so a seed names one world
/// across the `aggregate`, `pipeline` and `recovery` tasks rather than a world
/// per module. `recovery` draws through this too, so its doc claim holds.
pub(crate) fn planned_delay(seed: u64, key: u64) -> Duration {
    let draw = splitmix64(seed ^ splitmix64(key));
    Duration::from_millis(1 + (draw % 4))
}

// ── Fetcher ──────────────────────────────────────────────────────────────────

/// Report one refusal the rig cannot return to its caller.
///
/// The solutions and the oracle answer in their own error types, which carry no
/// cause; the cause goes to the estate's trace stream instead, where a reader of
/// the run can still find it.
pub fn diagnostic(line: fmt::Arguments<'_>) {
    lgwks_std::trace::warn!("{line}");
}

/// Why a fetch did not return a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchError {
    /// The fetch for `id` was configured to fail.
    Failed {
        /// The id that failed.
        id: u32,
    },
}

impl FetchError {
    /// The id whose fetch failed.
    #[must_use]
    pub const fn id(self) -> u32 {
        match self {
            Self::Failed { id } => id,
        }
    }
}

impl fmt::Display for FetchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Failed { id } => write!(formatter, "fetch id {id} failed"),
        }
    }
}

impl std::error::Error for FetchError {}

/// A point-in-time reading of the fetch instrumentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FetchStats {
    /// Fetches whose future is alive right now.
    pub live: u32,
    /// The most fetches that were ever alive at once.
    pub max_live: u32,
    /// Fetches that began, whether or not they finished.
    pub started: u64,
    /// Fetches that began after the first failure had already been observed.
    pub started_after_first_failure: u64,
    /// Fetches that returned a value.
    pub completed: u64,
    /// Fetches that returned [`FetchError`].
    pub failed: u64,
}

/// The counters behind [`FetchStats`]. Shared by every clone of a [`Fetcher`].
#[derive(Debug, Default)]
struct FetchCounters {
    /// Live fetches; raised on entry and lowered by the drop guard.
    live: AtomicU32,
    /// High-water mark of `live`.
    max_live: AtomicU32,
    /// Total fetches that began.
    started: AtomicU64,
    /// Fetches that began after a failure had been observed.
    started_after_first_failure: AtomicU64,
    /// Fetches that returned a value.
    completed: AtomicU64,
    /// Fetches that returned an error.
    failed: AtomicU64,
    /// Set once any fetch has observed its configured failure.
    failure_seen: AtomicBool,
}

impl FetchCounters {
    /// Read every counter into a [`FetchStats`].
    fn snapshot(&self) -> FetchStats {
        FetchStats {
            live: self.live.load(Ordering::Acquire),
            max_live: self.max_live.load(Ordering::Acquire),
            started: self.started.load(Ordering::Acquire),
            started_after_first_failure: self.started_after_first_failure.load(Ordering::Acquire),
            completed: self.completed.load(Ordering::Acquire),
            failed: self.failed.load(Ordering::Acquire),
        }
    }
}

/// Raises the live count on construction and lowers it on drop, so a fetch
/// whose future is dropped mid-`sleep` (a cancellation or an abort) is counted
/// out exactly once.
struct FetchLiveGuard {
    /// The counters to adjust.
    counters: Arc<FetchCounters>,
}

impl FetchLiveGuard {
    /// Enter the live set and update the high-water mark.
    fn enter(counters: Arc<FetchCounters>) -> Self {
        let live = counters
            .live
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        counters.max_live.fetch_max(live, Ordering::AcqRel);
        Self { counters }
    }
}

impl Drop for FetchLiveGuard {
    fn drop(&mut self) {
        self.counters.live.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The `aggregate` task's async, fallible fetch.
///
/// Cloneable and shareable: cloning a `Fetcher` shares the same counters, so a
/// solution that moves clones into concurrent futures is observed as one world.
#[derive(Clone)]
pub struct Fetcher {
    /// The shared instrumentation.
    counters: Arc<FetchCounters>,
    /// The ids configured to fail.
    failures: Arc<BTreeSet<u32>>,
    /// Per-id delay overrides.
    delays: Arc<BTreeMap<u32, Duration>>,
    /// The delay applied when no per-id override exists.
    uniform: Option<Duration>,
    /// The plan seed.
    seed: u64,
}

impl Fetcher {
    /// A fetch world seeded by `seed`, with no failures and a small planned
    /// delay per id.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            counters: Arc::new(FetchCounters::default()),
            failures: Arc::new(BTreeSet::new()),
            delays: Arc::new(BTreeMap::new()),
            uniform: None,
            seed,
        }
    }

    /// Configure `ids` to fail.
    #[must_use]
    pub fn failing<I: IntoIterator<Item = u32>>(mut self, ids: I) -> Self {
        let mut failures: BTreeSet<u32> = (*self.failures).clone();
        failures.extend(ids);
        self.failures = Arc::new(failures);
        self
    }

    /// Override the delay for one id.
    #[must_use]
    pub fn delay(mut self, id: u32, delay: Duration) -> Self {
        let mut delays: BTreeMap<u32, Duration> = (*self.delays).clone();
        delays.insert(id, delay);
        self.delays = Arc::new(delays);
        self
    }

    /// Use `delay` for every id that has no per-id override.
    #[must_use]
    pub fn uniform_delay(mut self, delay: Duration) -> Self {
        self.uniform = Some(delay);
        self
    }

    /// Read the instrumentation.
    #[must_use]
    pub fn stats(&self) -> FetchStats {
        self.counters.snapshot()
    }

    /// Fetch `id`, returning `id * 3` after the planned delay.
    ///
    /// # Errors
    ///
    /// [`FetchError::Failed`] when `id` is in the configured failure set.
    pub async fn fetch(&self, id: u32) -> Result<u64, FetchError> {
        if self.counters.failure_seen.load(Ordering::Acquire) {
            self.counters
                .started_after_first_failure
                .fetch_add(1, Ordering::AcqRel);
        }
        self.counters.started.fetch_add(1, Ordering::AcqRel);
        let _live = FetchLiveGuard::enter(Arc::clone(&self.counters));
        // Three delay tiers, most specific first: a per-id override, then the
        // uniform default, then the seeded plan. Spelled out rather than
        // chained so the precedence a run depends on is readable in one place.
        let delay = match self.delays.get(&id).copied() {
            Some(override_delay) => override_delay,
            None => match self.uniform {
                Some(uniform_delay) => uniform_delay,
                None => planned_delay(self.seed, u64::from(id)),
            },
        };
        lgwks_bot::rt::time::sleep(delay).await;
        if self.failures.contains(&id) {
            self.counters.failure_seen.store(true, Ordering::Release);
            self.counters.failed.fetch_add(1, Ordering::AcqRel);
            Err(FetchError::Failed { id })
        } else {
            self.counters.completed.fetch_add(1, Ordering::AcqRel);
            Ok(u64::from(id).saturating_mul(3))
        }
    }
}

// ── Stage ────────────────────────────────────────────────────────────────────

/// One named stage of the `pipeline` task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StageName {
    /// Produces `a`.
    FetchA,
    /// Produces `b`; independent of [`StageName::FetchA`].
    FetchB,
    /// Needs both `a` and `b` to have succeeded.
    Combine,
    /// Needs [`StageName::Combine`].
    Publish,
}

impl StageName {
    /// The stage's name, for logs and messages.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FetchA => "fetch_a",
            Self::FetchB => "fetch_b",
            Self::Combine => "combine",
            Self::Publish => "publish",
        }
    }

    /// A stable key for the planned delay.
    const fn plan_key(self) -> u64 {
        match self {
            Self::FetchA => 0,
            Self::FetchB => 1,
            Self::Combine => 2,
            Self::Publish => 3,
        }
    }
}

impl fmt::Display for StageName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Why a stage did not produce its artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageError {
    /// The stage produced no value: it was configured to fail, or an input it
    /// requires was never produced.
    Stage {
        /// The stage that produced no value.
        name: StageName,
    },
}

impl fmt::Display for StageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Stage { name } => write!(formatter, "stage {name} failed"),
        }
    }
}

impl std::error::Error for StageError {}

/// A point-in-time reading of the stage instrumentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StageStats {
    /// Stages whose future is alive right now.
    pub live: u32,
    /// The most stages that were ever alive at once.
    pub max_live: u32,
    /// How many times `fetch_a` ran.
    pub fetch_a: u32,
    /// How many times `fetch_b` ran.
    pub fetch_b: u32,
    /// How many times `combine` ran.
    pub combine: u32,
    /// How many times `publish` ran.
    pub publish: u32,
}

impl StageStats {
    /// How many times `name` ran.
    #[must_use]
    pub const fn runs(&self, name: StageName) -> u32 {
        match name {
            StageName::FetchA => self.fetch_a,
            StageName::FetchB => self.fetch_b,
            StageName::Combine => self.combine,
            StageName::Publish => self.publish,
        }
    }

    /// The total number of stage runs.
    #[must_use]
    pub const fn total(&self) -> u32 {
        self.fetch_a
            .saturating_add(self.fetch_b)
            .saturating_add(self.combine)
            .saturating_add(self.publish)
    }
}

/// The value one stage produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Artifact(u64);

impl Artifact {
    /// An artifact carrying `value`.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Read back the number this artifact carries, which is the unit's own result, unchanged by publishing.
    #[must_use]
    pub const fn value(&self) -> u64 {
        self.0
    }
}

/// The value a completed pipeline publishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Published(u64);

impl Published {
    /// A published value.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Read back the number that was published for this run, exactly as the publisher received it.
    #[must_use]
    pub const fn value(&self) -> u64 {
        self.0
    }
}

impl From<Artifact> for Published {
    fn from(artifact: Artifact) -> Self {
        Self(artifact.0)
    }
}

/// The values the pipeline threads between stages.
///
/// One struct under one lock rather than a lock per stage: `combine` needs
/// `a` *and* `b`, and three separate locks let it read a pair from two different
/// moments — a `fetch_a` that lands between the two reads is a pair no instant
/// of the run ever held. Under one lock the pair is always one the run actually
/// had.
#[derive(Debug, Default)]
struct StageValues {
    /// The value `fetch_a` produced, once it has.
    a: Option<u64>,
    /// The value `fetch_b` produced, once it has.
    b: Option<u64>,
    /// The value `combine` produced, once it has.
    combined: Option<u64>,
}

/// The counters and threaded values behind a [`Stage`].
#[derive(Debug, Default)]
struct StageState {
    /// Stage-run counters.
    fetch_a: AtomicU32,
    fetch_b: AtomicU32,
    combine: AtomicU32,
    publish: AtomicU32,
    /// Live stages.
    live: AtomicU32,
    /// High-water mark of `live`.
    max_live: AtomicU32,
    /// Every value a stage has produced, as one consistent set.
    values: Mutex<StageValues>,
}

impl StageState {
    /// Read every counter into a [`StageStats`].
    fn snapshot(&self) -> StageStats {
        StageStats {
            live: self.live.load(Ordering::Acquire),
            max_live: self.max_live.load(Ordering::Acquire),
            fetch_a: self.fetch_a.load(Ordering::Acquire),
            fetch_b: self.fetch_b.load(Ordering::Acquire),
            combine: self.combine.load(Ordering::Acquire),
            publish: self.publish.load(Ordering::Acquire),
        }
    }

    /// Count one run of `name`.
    fn bump(&self, name: StageName) {
        let counter = match name {
            StageName::FetchA => &self.fetch_a,
            StageName::FetchB => &self.fetch_b,
            StageName::Combine => &self.combine,
            StageName::Publish => &self.publish,
        };
        counter.fetch_add(1, Ordering::AcqRel);
    }

    /// Takes the one lock, recovering from poisoning as [`lock`] documents.
    fn values(&self) -> MutexGuard<'_, StageValues> {
        lock(&self.values)
    }
}

/// Raises the live count on construction and lowers it on drop.
struct StageLiveGuard {
    /// The counters to adjust.
    state: Arc<StageState>,
}

impl StageLiveGuard {
    /// Enter the live set and update the high-water mark.
    fn enter(state: Arc<StageState>) -> Self {
        let live = state.live.fetch_add(1, Ordering::AcqRel).saturating_add(1);
        state.max_live.fetch_max(live, Ordering::AcqRel);
        Self { state }
    }
}

impl Drop for StageLiveGuard {
    fn drop(&mut self) {
        self.state.live.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The `pipeline` task's named stages, with configurable faults.
///
/// Cloneable and shareable, like [`Fetcher`].
#[derive(Clone)]
pub struct Stage {
    /// The shared instrumentation and threaded values.
    state: Arc<StageState>,
    /// The stages configured to fail.
    failures: Arc<BTreeSet<StageName>>,
    /// Per-stage delay overrides.
    delays: Arc<BTreeMap<StageName, Duration>>,
    /// The plan seed.
    seed: u64,
}

impl Stage {
    /// A stage world seeded by `seed`, with no failures and a small planned
    /// delay per stage.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            state: Arc::new(StageState::default()),
            failures: Arc::new(BTreeSet::new()),
            delays: Arc::new(BTreeMap::new()),
            seed,
        }
    }

    /// Configure `names` to fail.
    #[must_use]
    pub fn failing<I: IntoIterator<Item = StageName>>(mut self, names: I) -> Self {
        let mut failures: BTreeSet<StageName> = (*self.failures).clone();
        failures.extend(names);
        self.failures = Arc::new(failures);
        self
    }

    /// Override the delay for one stage.
    #[must_use]
    pub fn slow(mut self, name: StageName, delay: Duration) -> Self {
        let mut delays: BTreeMap<StageName, Duration> = (*self.delays).clone();
        delays.insert(name, delay);
        self.delays = Arc::new(delays);
        self
    }

    /// Read the instrumentation.
    #[must_use]
    pub fn stats(&self) -> StageStats {
        self.state.snapshot()
    }

    /// Run `name`.
    ///
    /// `fetch_a` produces 11, `fetch_b` produces 22, `combine` produces their
    /// sum, and `publish` produces twice the combined value. A stage that is not
    /// one of those four is not expressible by the type.
    ///
    /// # Errors
    ///
    /// [`StageError::Stage`] when `name` is in the configured failure set, and
    /// when `name` requires an input no earlier run produced — `combine`
    /// without both of `a` and `b`, or `publish` without a `combine`. A missing
    /// input is refused rather than read as `0`, because a `0` here is an
    /// artifact no stage ever produced and a pipeline that publishes it has
    /// published a number the run never saw.
    pub async fn run(&self, name: StageName) -> Result<Artifact, StageError> {
        self.state.bump(name);
        let _live = StageLiveGuard::enter(Arc::clone(&self.state));
        // Two delay tiers, most specific first: a per-stage override, then the
        // seeded plan.
        let delay = match self.delays.get(&name).copied() {
            Some(override_delay) => override_delay,
            None => planned_delay(self.seed, name.plan_key()),
        };
        lgwks_bot::rt::time::sleep(delay).await;
        if self.failures.contains(&name) {
            // A scripted failure is the run's own design, not a fault, so it
            // is recorded at debug rather than reported as a refusal.
            let refusal = Err(StageError::Stage { name });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "run: a scripted stage failure");
            return refusal;
        }
        match name {
            StageName::FetchA => {
                let mut values = self.state.values();
                values.a = Some(11);
                Ok(Artifact::new(11))
            }
            StageName::FetchB => {
                let mut values = self.state.values();
                values.b = Some(22);
                Ok(Artifact::new(22))
            }
            StageName::Combine => {
                let mut values = self.state.values();
                // One lock for the pair, so the two inputs are always a pair
                // some instant of this run actually held.
                match (values.a, values.b) {
                    (Some(a), Some(b)) => {
                        let combined = a.saturating_add(b);
                        values.combined = Some(combined);
                        Ok(Artifact::new(combined))
                    }
                    (a, b) => {
                        diagnostic(format_args!(
                            "combine ran with a={} b={}: both fetch_a and fetch_b must \
                             have produced a value first",
                            described(a),
                            described(b)
                        ));
                        Err(StageError::Stage { name })
                    }
                }
            }
            StageName::Publish => match self.state.values().combined {
                Some(combined) => Ok(Artifact::new(combined.saturating_mul(2))),
                None => {
                    diagnostic(format_args!(
                        "publish ran with combined=none: combine must have produced a \
                         value first"
                    ));
                    Err(StageError::Stage { name })
                }
            },
        }
    }
}

/// Renders an optional stage value for a diagnostic line.
///
/// `None` is written as `none` rather than as `0`, so a diagnostic about a
/// missing input never reads like a report of a zero-valued one.
fn described(value: Option<u64>) -> String {
    match value {
        Some(value) => value.to_string(),
        None => "none".to_owned(),
    }
}
