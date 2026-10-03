//! The deterministic simulation substrate every `sim_*` target is written on.
//!
//! This module owns the three things that make a simulation a simulation
//! rather than a slow unit test:
//!
//! 1. **One seed controls everything.** A scenario is a single `u64`. From it
//!    are derived the fault schedule, the tenant count, the concurrency, the
//!    partition windows and every draw the scenario makes afterwards. Nothing
//!    reads the clock, the OS scheduler or an unseeded source of entropy, so a
//!    seed replays exactly.
//! 2. **Time is virtual.** [`SimClock`] is a `u64` tick counter. The wall
//!    clock is never consulted, so a three-day partition costs three
//!    arithmetic operations rather than three days, and a run that is green on
//!    a laptop is green on a busy CI box.
//! 3. **A trace is hashed.** Every fact a scenario asserts is also appended to
//!    a [`Trace`], and the trace hashes to a `u64`. Two runs of the same seed
//!    must produce the same hash; a run that diverges is a defect whether or
//!    not the assertions happened to pass. The hash is the replay receipt, and
//!    it is what makes a wide sweep cheap to compare: a differing seed is one
//!    `u64` comparison, not a diff of logs.
//!
//! # What is real and what is virtual
//!
//! The logic under test is **real**: the real `FileJournal` with its real
//! storage-owner thread, the real frame codec, the real chain verification,
//! the real `recover` fold, the real `Broker` and the real `Bot`. The sim
//! virtualizes only the three things a test cannot otherwise control —
//! storage faults, the network, and time. That is the same split a VOPR makes:
//! deterministic control of the environment, real code in the middle.
//!
//! Writing a second journal "for the simulation" would be the failure mode
//! this design exists to avoid: the copy would pass while the shipped journal
//! rotted. Everything here drives the shipped types.
//!
//! # Dependencies
//!
//! `std` only, like everything else in this repository. The PRNG is xoshiro
//! 256** and the hash is FNV-1a 64, both written out here rather than taken
//! from a crate, because a simulation whose entropy source is a dependency is
//! not reproducible across a lockfile bump.

// Each `sim_*` target is its own crate and drives a different subset of this
// harness, so every family sees items the others never touch. The lint is real
// per-crate and the allowance is inherent to sharing one harness across them.
#![allow(
    dead_code,
    reason = "each sim_* target uses a different subset of the shared harness"
)]

pub mod rig;

use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

// ── Randomness and the trace ──────────────────────────────────────────────

mod seed;

pub use seed::{Rng, Trace};

// ── Time ───────────────────────────────────────────────────────────────────

/// A virtual clock.
///
/// Ticks, not timestamps, and it only ever moves when a scenario moves it.
/// Nothing here can observe the wall clock, which is what makes a three-day
/// outage testable in a second.
#[derive(Clone, Copy, Debug, Default)]
pub struct SimClock {
    now: u64,
}

impl SimClock {
    /// The clock at tick zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// The current tick.
    pub fn now(&self) -> u64 {
        self.now
    }

    /// Advance by `ticks`.
    fn advance(&mut self, ticks: u64) {
        self.now = self.now.saturating_add(ticks);
    }
}

// ── The fault schedule ────────────────────────────────────────────────────

/// Everything one seed decided the environment would do.
///
/// Derived wholly from the seed, so the same seed is the same disaster. The
/// fields are per-mille probabilities and tick windows rather than enums so
/// that a scenario sweeps a *range* of severity instead of a fixed one: a
/// single "network drops things" toggle proves far less than a sweep from one
/// drop in a thousand to a total partition.
#[derive(Clone, Copy, Debug)]
pub struct Faults {
    /// Per-mille chance a device write is refused outright.
    pub disk_refuse: u32,
    /// Per-mille chance a write is torn: the bytes reach the file and the tail
    /// is garbage, which is the only failure that produces a *lying* journal
    /// rather than a refusing one.
    pub disk_tear: u32,
    /// Per-mille chance a message is dropped in flight.
    pub net_drop: u32,
    /// Per-mille chance a message is delivered twice.
    pub net_duplicate: u32,
    /// The tick before which the network is wholly partitioned.
    pub partition_until: u64,
    /// The tick at which the process dies, if it does.
    pub crash_at: u64,
    /// How many tenants share the run.
    pub tenants: u32,
    /// How many effects are in flight at once.
    pub concurrency: u32,
    /// How many journal generations the run puts in play, so chain forks are
    /// exercised rather than assumed away.
    pub generations: u32,
}

impl Faults {
    /// The schedule for `seed`.
    ///
    /// The tenant and concurrency counts are cut first, from their own draws,
    /// so that a change to a fault probability does not renumber every tenant
    /// in every other scenario. A schedule that shifted wholesale would make
    /// a regression in one dimension unbisectable in the others.
    pub fn for_seed(seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let tenants = rng.between(1, 65);
        let concurrency = rng.between(1, 257);
        Self {
            disk_refuse: rng.between(0, 200),
            disk_tear: rng.between(0, 120),
            net_drop: rng.between(0, 300),
            net_duplicate: rng.between(0, 150),
            partition_until: if rng.chance(250) {
                u64::from(rng.between(1, 512))
            } else {
                0
            },
            crash_at: if rng.chance(300) {
                u64::from(rng.between(1, 1024))
            } else {
                u64::MAX
            },
            tenants,
            concurrency,
            generations: rng.between(1, 4),
        }
    }
}

// ── The network ───────────────────────────────────────────────────────────

/// One message between two tenants.
///
/// Fields are crate-visible rather than public: a delivered envelope is a
/// fact about the past, and letting a scenario reach in and edit one would let
/// a test fabricate a delivery the network never made. Everything here reaches
/// the fields only to read them, and the one place that writes them is
/// [`Network::send`] and [`Network::deliver`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Envelope {
    /// Who sent it.
    pub(crate) from: u32,
    /// Who it is for.
    pub(crate) to: u32,
    /// The payload's exact bytes. Compared, never interpreted: a simulation
    /// that understood its own payloads would be able to pass while the real
    /// decoder was wrong.
    pub(crate) body: Vec<u8>,
    /// The tick it becomes visible at.
    pub(crate) due: u64,
    /// How many times it has been put on the wire.
    pub(crate) attempts: u32,
}

impl Envelope {
    /// Who sent this envelope.
    pub fn from(&self) -> u32 {
        self.from
    }

    /// Who this envelope is for.
    pub fn to(&self) -> u32 {
        self.to
    }

    /// The payload, as bytes. A borrow, so a scenario can look without being
    /// able to rewrite what the network carried.
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// The tick this envelope became visible at.
    pub fn due(&self) -> u64 {
        self.due
    }

    /// How many times this envelope has been put on the wire. Above one means
    /// a duplicate is being delivered, which is the case an at-least-once
    /// consumer has to survive.
    pub fn attempts(&self) -> u32 {
        self.attempts
    }
}

/// A lossy, duplicating, reorderable network.
///
/// Every fault the contract names for a network is a parameter here rather
/// than a special case: a partition is a delivery the scheduler refuses, a
/// duplicate is the same envelope enqueued twice, and a reorder is a due date
/// drawn from a wider window than a delivery in order. There is no path
/// through this type that is not routed through the scheduler, so a scenario
/// cannot accidentally get an in-order, lossless, exactly-once channel by
/// forgetting to configure it.
#[derive(Clone, Debug, Default)]
pub struct Network {
    in_flight: Vec<Envelope>,
    delivered: u32,
    dropped: u32,
    duplicated: u32,
}

impl Network {
    /// An empty network.
    pub fn new() -> Self {
        Self::default()
    }

    /// Put `body` on the wire from `from` to `to`.
    ///
    /// `due` is an absolute tick, not a latency, so a scenario controls the
    /// arrival order directly: jitter is a seeded draw the scenario makes
    /// itself, rather than something hidden inside the transport where a
    /// determinism bug would be invisible.
    pub fn send(&mut self, from: u32, to: u32, body: Vec<u8>, due: u64) {
        self.in_flight.push(Envelope {
            from,
            to,
            body,
            due,
            attempts: 1,
        });
    }

    /// Take everything due at or before `now`.
    ///
    /// Applies the partition, the drops and the duplicates here, so every
    /// scenario sees the same accounting and the trace can be compared across
    /// families.
    pub fn deliver(&mut self, now: u64, rng: &mut Rng, faults: &Faults) -> Vec<Envelope> {
        let mut ready = Vec::new();
        let mut still = Vec::new();
        for envelope in std::mem::take(&mut self.in_flight) {
            if envelope.due > now {
                still.push(envelope);
                continue;
            }
            if now < faults.partition_until {
                self.dropped = self.dropped.saturating_add(1);
                continue;
            }
            if rng.chance(faults.net_drop) {
                self.dropped = self.dropped.saturating_add(1);
                continue;
            }
            if rng.chance(faults.net_duplicate) {
                self.duplicated = self.duplicated.saturating_add(1);
                let mut again = envelope.clone();
                again.due = now
                    .saturating_add(1)
                    .saturating_add(u64::from(rng.below(8)));
                again.attempts = again.attempts.saturating_add(1);
                still.push(again);
            }
            self.delivered = self.delivered.saturating_add(1);
            ready.push(envelope);
        }
        self.in_flight = still;
        ready
    }

    /// How many envelopes are still on the wire.
    pub fn in_flight(&self) -> usize {
        self.in_flight.len()
    }

    /// Delivered, dropped and duplicated, for the trace.
    pub fn counts(&self) -> (u32, u32, u32) {
        (self.delivered, self.dropped, self.duplicated)
    }
}

// ── The scenario ──────────────────────────────────────────────────────────

/// One simulated run: a seed, a clock, a network, a fault schedule, a trace.
#[derive(Debug)]
pub struct Sim {
    /// The seed this run replays from.
    pub seed: u64,
    /// The virtual clock.
    pub clock: SimClock,
    /// The network under test.
    pub net: Network,
    /// What the environment will do.
    pub faults: Faults,
    /// The replay receipt.
    pub trace: Trace,
    rng: Rng,
    dirs: Vec<PathBuf>,
}

impl Sim {
    /// A run replaying from `seed`.
    pub fn new(seed: u64) -> Self {
        Self {
            seed,
            clock: SimClock::new(),
            net: Network::new(),
            faults: Faults::for_seed(seed),
            trace: Trace::new(),
            rng: Rng::new(seed ^ 0x5eed_5eed_5eed_5eed),
            dirs: Vec::new(),
        }
    }

    /// The next draw, from the same stream the fault schedule was cut from.
    pub fn rng(&mut self) -> &mut Rng {
        &mut self.rng
    }

    /// Move the virtual clock forward, recording it.
    pub fn tick(&mut self, ticks: u64) -> u64 {
        self.clock.advance(ticks);
        self.trace.record_u64("tick", self.clock.now());
        self.clock.now()
    }

    /// Take everything the network has due at `now`.
    ///
    /// One method rather than three field borrows at every call site, because
    /// the scheduler needs the run's RNG *and* its fault schedule *and* the
    /// network, and a caller cannot borrow all three out of `&mut self` at
    /// once. The copies are taken here so the borrow split is this module's
    /// problem once rather than every scenario's.
    pub fn drain(&mut self, now: u64) -> Vec<Envelope> {
        let faults = self.faults;
        self.net.deliver(now, &mut self.rng, &faults)
    }

    /// Put one envelope on the wire from this run's network.
    ///
    /// A method for the same reason as [`Sim::drain`]: a scenario that calls
    /// `sim.net.send(.., sim.tick(..))` borrows the network mutably and the run
    /// mutably at once, which is a borrow error at every call site rather than
    /// a fact of the design.
    pub fn send(&mut self, from: u32, to: u32, body: Vec<u8>, due: u64) {
        self.net.send(from, to, body, due);
    }

    /// Send at the current tick, so the envelope is due now.
    pub fn send_now(&mut self, from: u32, to: u32, body: Vec<u8>) {
        let due = self.clock.now();
        self.net.send(from, to, body, due);
    }

    /// Move the clock and take everything due, in one borrow.
    ///
    /// The same reason as [`Sim::drain`]: `sim.drain(sim.tick(n))` is two
    /// mutable borrows of the same run, and a scenario that has to express
    /// "advance, then deliver" should not have to name a temporary.
    pub fn advance_and_drain(&mut self, ticks: u64) -> Vec<Envelope> {
        let now = self.tick(ticks);
        self.drain(now)
    }

    /// Whether the process has died by this point in the run.
    pub fn has_crashed(&self) -> bool {
        self.clock.now() >= self.faults.crash_at
    }

    /// Record a fact.
    pub fn record(&mut self, fact: &str) {
        self.trace.record(fact);
    }

    /// The replay receipt for this run.
    pub fn hash(&self) -> u64 {
        self.trace.hash()
    }

    /// A fresh, empty directory for this run's state.
    ///
    /// Under the system temp directory, named by the seed and by random bytes
    /// from the estate's entropy source, so a failing run leaves evidence a
    /// reader can open rather than a number they must trust and two runs never
    /// collide. The randomness names the *directory* and never enters the
    /// trace, so a run still replays to the same hash. A process identifier
    /// would have been the shorter answer and the wrong one: the OS reuses it,
    /// so it is not an identity. Nothing in the repository's own tree is ever
    /// written to.
    pub fn scratch(&mut self, tag: &str) -> Result<PathBuf, Box<dyn Error>> {
        let unique = lgwks_std::random::bytes::<8>()?;
        let suffix: Vec<String> = unique.iter().map(|byte| format!("{byte:02x}")).collect();
        let path =
            std::env::temp_dir().join(format!("lgwks-sim-{tag}-{}-{}", self.seed, suffix.join("")));
        if path.exists() {
            fs::remove_dir_all(&path)?;
        }
        fs::create_dir_all(&path)?;
        self.dirs.push(path.clone());
        Ok(path)
    }

    /// A journal path inside this run's scratch space.
    pub fn journal_path(&self, dir: &Path, tenant: u32) -> PathBuf {
        dir.join(format!("tenant-{tenant}.jrnl"))
    }
}

impl Drop for Sim {
    fn drop(&mut self) {
        for dir in &self.dirs {
            drop(fs::remove_dir_all(dir));
        }
    }
}

// ── Scenario drivers ──────────────────────────────────────────────────────

/// A seed band: the seeds one declared test sweeps.
///
/// A band rather than a single seed so that a test is a *sweep* and not a
/// point sample, and a band rather than every seed so that a failure names the
/// band and the offending seed is recovered from the recorded trace. The union
/// of the bands is a contiguous sweep of the whole space, so no seed is
/// quietly untested because of how the bands were cut.
#[derive(Clone, Copy, Debug)]
pub struct Band {
    /// The first seed, inclusive.
    pub first: u64,
    /// How many seeds the band covers.
    pub count: u64,
}

impl Band {
    /// A band of `count` seeds starting at `first`.
    pub const fn new(first: u64, count: u64) -> Self {
        Self { first, count }
    }

    /// Every seed in the band.
    pub fn seeds(&self) -> impl Iterator<Item = u64> + '_ {
        self.first..self.first.saturating_add(self.count)
    }
}

/// Run `body` once per seed in `band`, collecting each run's trace hash.
///
/// Returns the hashes in seed order, so a caller can assert the whole sweep is
/// deterministic by comparing two runs' vectors — a stronger statement than
/// any single run's assertions, because it covers every seed in the band
/// rather than one of them.
pub fn sweep<F>(band: Band, mut body: F) -> Result<Vec<u64>, Box<dyn Error>>
where
    F: FnMut(&mut Sim) -> Result<(), Box<dyn Error>>,
{
    let mut hashes = Vec::with_capacity(usize::try_from(band.count).unwrap_or(0));
    for seed in band.seeds() {
        let mut sim = Sim::new(seed);
        if let Err(err) = body(&mut sim) {
            return Err(format!("seed {seed} did not satisfy the scenario: {err}").into());
        }
        hashes.push(sim.hash());
    }
    Ok(hashes)
}

/// The bands that partition `0..total` into `parts` of them.
///
/// Even division with the remainder spread over the low bands, so the bands
/// are contiguous, disjoint, and together cover every seed with no gap and no
/// overlap. A suite that leaves a hole in the seed space would be reporting a
/// coverage it does not have.
/// The seed space every family sweeps, and how many bands it is cut into.
///
/// One pair for the whole simulation layer. Each file that declared its own
/// would be free to drift, and a family sweeping 64 seeds next to one sweeping
/// 128 would make a pass rate that means nothing. The seed space is doubled
/// alongside the band count, so every band holds eight seeds rather than eight
/// families sharing one — a family registered against band 30 would otherwise
/// silently sweep the same eight seeds as band 14.
pub const SEED_SPACE: u64 = 256;
pub const BANDS: u64 = 32;

/// The one band of seeds a declared test sweeps.
///
/// `index` is a registration, not a computation: every call site is a literal
/// written beside the family that declares it, so an index past the last band is a
/// registration mistake. It is folded onto a declared band rather than refused,
/// because `swap_remove` answered it with an out-of-bounds panic whose message
/// names the length rather than the mistake — and because the alternative,
/// `expect`, is forbidden here too. Folding means a mistyped band silently reuses
/// another family's seeds, so the band count is doubled alongside the seed space
/// and every declared index stays inside it.
pub fn band_of(index: usize) -> Band {
    let table = bands(SEED_SPACE, BANDS);
    let mut widened = table.clone();
    while widened.len() <= index {
        match table.last().copied() {
            Some(last) => widened.push(last),
            None => return Band::new(0, 1),
        }
    }
    widened.get(index).copied().unwrap_or(Band::new(0, 1))
}

pub fn bands(total: u64, parts: u64) -> Vec<Band> {
    let base = total.checked_div(parts).unwrap_or(0);
    let remainder = total.checked_rem(parts).unwrap_or(0);
    let mut out = Vec::with_capacity(usize::try_from(parts).unwrap_or(0));
    let mut cursor = 0u64;
    for index in 0..parts {
        let count = base.saturating_add(u64::from(index < remainder));
        out.push(Band::new(cursor, count));
        cursor = cursor.saturating_add(count);
    }
    out
}

/// Assert that `body` replays exactly, for every seed in `band`.
///
/// The determinism check is the point of the whole harness, so it is one call
/// rather than something each family reimplements: the band is swept twice and
/// the two hash vectors must be identical. A family that only asserted
/// outcomes would pass while the run was nondeterministic, which is the
/// failure this exists to catch.
pub fn assert_replays<F>(band: Band, mut body: F) -> Result<(), Box<dyn Error>>
where
    F: FnMut(&mut Sim) -> Result<(), Box<dyn Error>>,
{
    let first = sweep(band, &mut body)?;
    let second = sweep(band, &mut body)?;
    assert_eq!(
        first, second,
        "the same seed produced two different traces, so the run is not \
         deterministic and its hash is not a replay receipt"
    );
    Ok(())
}

/// The observation a scenario ends with, for cross-family comparison.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    /// Events the journal holds.
    pub committed: usize,
    /// Attempts left uncertain by recovery.
    pub uncertain: usize,
    /// Attempts recovery knows the status of.
    pub attempts: usize,
    /// Effects that actually ran.
    pub executed: u32,
    /// Deliveries the network made.
    pub delivered: u32,
    /// Deliveries the network lost.
    pub dropped: u32,
    /// Deliveries the network made twice.
    pub duplicated: u32,
}

impl Outcome {
    /// Fold an outcome into a trace, so a family that runs a different
    /// scenario can still be compared with one that runs this one.
    pub fn record(&self, trace: &mut Trace) {
        trace.record_u64("committed", u64::try_from(self.committed).unwrap_or(0));
        trace.record_u64("uncertain", u64::try_from(self.uncertain).unwrap_or(0));
        trace.record_u64("attempts", u64::try_from(self.attempts).unwrap_or(0));
        trace.record_u64("executed", u64::from(self.executed));
        trace.record_u64("delivered", u64::from(self.delivered));
        trace.record_u64("dropped", u64::from(self.dropped));
        trace.record_u64("duplicated", u64::from(self.duplicated));
    }

    /// The outcome a run that achieved nothing reports.
    ///
    /// One definition rather than a literal per call site, because a family
    /// that spelled the zero case differently from another would make the two
    /// families' traces incomparable, which is the one thing the receipt
    /// exists to prevent.
    pub fn empty() -> Self {
        Self {
            committed: 0,
            uncertain: 0,
            attempts: 0,
            executed: 0,
            delivered: 0,
            dropped: 0,
            duplicated: 0,
        }
    }
}

/// A per-tenant tally, used by the multi-tenant families to prove isolation
/// rather than to report an aggregate.
///
/// A map rather than a counter because "5,000 tenants, all fine" is a claim
/// about an aggregate and a defect in one tenant hides inside it. The families
/// that use this read every entry rather than summing them.
pub type Tenants = BTreeMap<u32, u32>;
