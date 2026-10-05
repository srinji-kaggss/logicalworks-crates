//! The open-loop half of the rig: an offered-rate generator, a log-bucketed
//! latency recorder, and the deterministic model both of them are checked
//! against.
//!
//! # Why open loop, and what "open" means here
//!
//! Every closed-loop measurement in this rig — and every `wrk`-style client —
//! waits for request `i` to finish before it starts request `i + 1`. Past
//! saturation that *slows the offered rate down* exactly as the service slows
//! down, so the client measures itself: the reported latency never includes the
//! time a request spent waiting for the server to become free, because a
//! saturated server is precisely the case where the client stopped offering.
//! This is Gil Tene's **coordinated omission**, and it under-reports p99 at
//! saturation by orders of magnitude.
//!
//! The fix is one line of arithmetic: a request's latency is measured from the
//! instant it was *intended* to start (`t0 + i/λ`), not from the instant the
//! generator actually got to it. A generator that falls behind therefore reports
//! the queue it created, and a benchmark that cannot keep up reports a rate
//! rather than a latency.
//!
//! [`intended_arrival`] is that one line, and it is shared by the live driver in
//! `main.rs` and by the seeded model below — so the simulation tests the same
//! schedule arithmetic the measurement runs, rather than a copy of it that could
//! agree with itself while the real one drifts.
//!
//! # Why the recorder is log-bucketed and hand-written
//!
//! The issue requires HdrHistogram-style buckets **inside the bench**, with no
//! new published-crate edge. A recorder that kept every sample would also be a
//! memory leak proportional to the offered rate (a million arrivals per second
//! for thirty seconds is thirty million samples), so the histogram is not an
//! optimisation here — it is the only bounded answer.
//!
//! The shape is the same one HdrHistogram uses: values are placed in
//! [`SUB_BUCKETS`] equal-width buckets inside each power-of-two octave, so the
//! relative error of any reported value is at most `1 / SUB_BUCKETS` (0.39% at
//! [`SUB_BITS`] = 8) at *every* magnitude — from a 40 ns body to a 4 s stall.
//! A linear histogram of microsecond counts would quantise a 40 ns body to
//! zero; this one does not.
//!
//! Two facts are kept exactly rather than bucketed, because a reader needs them
//! exactly: the minimum and the maximum, and the total count. A reported
//! percentile is the **midpoint** of its bucket, which halves the worst-case
//! bias of reporting the bucket's upper bound; the quantization bound is
//! [`SUB_BUCKETS`] and is stated rather than assumed.
//!
//! # Concurrency
//!
//! Tasks run on every worker thread, so the recorder is sharded and every counter
//! is an atomic: [`Recorder::record`] is one relaxed fetch-add on the bucket plus
//! the running total, with no lock and no allocation. A task takes a shard once,
//! at placement, from [`RecorderSet::take`]. [`Recorder::reset`] exists for the
//! recovery phase, which needs a *per-window* percentile; it is called between
//! windows and never inside a timed one, and its cost (one pass over the bucket
//! array) is stated rather than hidden.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

/// Sub-buckets per power-of-two octave.
///
/// The reported-value error bound is `1 / SUB_BUCKETS`. HdrHistogram's own
/// default is three significant decimal digits (a finer resolution than this);
/// two is enough to make a p99 that claims to differ from another p99 by more
/// than half a percent meaningful, and it keeps a shard at 74 KiB.
pub const SUB_BITS: u32 = 8;

/// Buckets per octave, [`SUB_BITS`] of them.
pub const SUB_BUCKETS: usize = 1 << SUB_BITS;

/// The number of doublings above `2^SUB_BUCKETS` that the array covers.
///
/// Every latency up to `2^(SUB_BITS + OCTAVES)` nanoseconds has a bucket — about 9.2
/// hours at `OCTAVES = 27` — and the ceiling is a bound rather than an assumption: a
/// sample past it is counted in [`Histogram::clamped`] and lands in the top bucket, so
/// a stalled run reports "some samples were past the ceiling" instead of quietly
/// reading as a percentile of the range it could represent.
pub const OCTAVES: usize = 27;

/// Bucket array length.
///
/// The first [`SUB_BUCKETS`] buckets are the exact small values, one bucket each; every
/// doubling above them adds [`SUB_BUCKETS`] buckets of width `2^(octave - SUB_BITS)`.
pub const SLOTS: usize = SUB_BUCKETS + OCTAVES * SUB_BUCKETS;

/// Largest latency a bucket can represent, in nanoseconds.
pub const CEILING_NANOS: u64 = 1 << (SUB_BITS + OCTAVES as u32);

/// The bucket `nanos` falls in.
///
/// The small half of the range is **exact**: every value below `2^SUB_BUCKETS` (256 ns)
/// has its own bucket, because the lower octaves have fewer than [`SUB_BUCKETS`]
/// values in them and rounding a 40 ns body into a 72 ns bucket would report a floor
/// the rig cannot see. Above that, each doubling carries [`SUB_BUCKETS`] buckets of
/// width `2^(octave - SUB_BITS)`, so the relative error never exceeds
/// `1 / SUB_BUCKETS` at any magnitude.
///
/// Two facts make the map total: every `u64` below [`CEILING_NANOS`] has a bucket, and
/// every `u64` at or above it lands in the top one.
#[must_use]
pub fn slot_of(nanos: u64) -> usize {
    let value = nanos.min(CEILING_NANOS);
    if value < SUB_BUCKETS as u64 {
        return value as usize;
    }
    let octave = 63 - value.leading_zeros();
    let shift = octave - SUB_BITS;
    let index = (value >> shift) & (SUB_BUCKETS as u64 - 1);
    SUB_BUCKETS + (shift as usize) * SUB_BUCKETS + (index as usize)
}

/// The inclusive value range one bucket covers.
#[must_use]
pub fn slot_range(slot: usize) -> (u64, u64) {
    if slot < SUB_BUCKETS {
        return (slot as u64, slot as u64);
    }
    let relative = slot - SUB_BUCKETS;
    let shift = relative / SUB_BUCKETS;
    let index = relative % SUB_BUCKETS;
    let width = 1_u64 << shift;
    let low = (1_u64 << (SUB_BITS + shift as u32)) + index as u64 * width;
    (low, low + width - 1)
}

/// The value a bucket reports: its midpoint.
///
/// The midpoint rather than the bucket's upper bound, because reporting the
/// upper bound would add up to a full `1 / SUB_BUCKETS` of bias to every
/// percentile — a systematic over-report, which is the direction that makes a
/// measurement look worse than it is.
#[must_use]
pub fn slot_value(slot: usize) -> u64 {
    let (low, high) = slot_range(slot);
    low + (high - low) / 2
}

/// One shard of the latency histogram.
///
/// Relaxed atomics throughout: a sample's bucket and a sample's arrival are the
/// same event on the same thread, and the merge at the end of a window is the
/// only read that needs agreement.
#[derive(Debug)]
pub struct Recorder {
    counts: Box<[AtomicU64]>,
    total: AtomicU64,
    sum_nanos: AtomicU64,
    min_nanos: AtomicU64,
    max_nanos: AtomicU64,
    clamped: AtomicU64,
}

impl Default for Recorder {
    fn default() -> Self {
        Self::new()
    }
}

impl Recorder {
    /// A fresh, empty recorder.
    ///
    /// The bucket array is allocated once, here, and reused by every
    /// [`Self::reset`]: an instrument that allocated per window would put its own
    /// cost inside the measurement.
    #[must_use]
    pub fn new() -> Self {
        let counts = (0..SLOTS)
            .map(|_| AtomicU64::new(0))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            counts,
            total: AtomicU64::new(0),
            sum_nanos: AtomicU64::new(0),
            min_nanos: AtomicU64::new(u64::MAX),
            max_nanos: AtomicU64::new(0),
            clamped: AtomicU64::new(0),
        }
    }

    /// Record one latency in nanoseconds.
    pub fn record(&self, nanos: u64) {
        if nanos > CEILING_NANOS {
            self.clamped.fetch_add(1, Ordering::Relaxed);
        }
        let value = nanos.min(CEILING_NANOS);
        self.counts[slot_of(value)].fetch_add(1, Ordering::Relaxed);
        self.total.fetch_add(1, Ordering::Relaxed);
        // Saturating: a sum that wrapped would report a negative mean, and a
        // mean is the one statistic this rig reports as context rather than as a
        // headline. A store of a saturated sum keeps it saturated, which is the
        // same reading a `checked_add` chain gives without a retry loop on the
        // allocation path.
        let carried = self.sum_nanos.load(Ordering::Relaxed);
        self.sum_nanos
            .store(carried.saturating_add(value), Ordering::Relaxed);
        self.max_nanos.fetch_max(value, Ordering::Relaxed);
        if value == 0 {
            self.min_nanos.fetch_min(0, Ordering::Relaxed);
        } else {
            self.min_nanos.fetch_min(value, Ordering::Relaxed);
        }
    }

    /// Zero every bucket and counter, keeping the allocation.
    ///
    /// Used between recovery windows. A completion that lands *during* a reset is
    /// counted in one window or the other and never in both or neither, which is
    /// the only property the recovery reading depends on.
    pub fn reset(&self) {
        for bucket in self.counts.iter() {
            bucket.store(0, Ordering::Relaxed);
        }
        self.total.store(0, Ordering::Relaxed);
        self.sum_nanos.store(0, Ordering::Relaxed);
        self.min_nanos.store(u64::MAX, Ordering::Relaxed);
        self.max_nanos.store(0, Ordering::Relaxed);
        self.clamped.store(0, Ordering::Relaxed);
    }

    /// Merge every shard into one owned histogram.
    #[must_use]
    pub fn merged(shards: &[Arc<Self>]) -> Histogram {
        let mut counts = vec![0_u64; SLOTS];
        let mut total = 0_u64;
        let mut sum = 0_u128;
        let mut min = u64::MAX;
        let mut max = 0_u64;
        let mut clamped = 0_u64;
        for shard in shards {
            for (slot, bucket) in counts.iter_mut().zip(shard.counts.iter()) {
                *slot = slot.saturating_add(bucket.load(Ordering::Relaxed));
            }
            total = total.saturating_add(shard.total.load(Ordering::Relaxed));
            sum = sum.saturating_add(u128::from(shard.sum_nanos.load(Ordering::Relaxed)));
            let shard_min = shard.min_nanos.load(Ordering::Relaxed);
            if shard_min < min {
                min = shard_min;
            }
            max = max.max(shard.max_nanos.load(Ordering::Relaxed));
            clamped = clamped.saturating_add(shard.clamped.load(Ordering::Relaxed));
        }
        Histogram {
            counts,
            total,
            sum,
            min: if min == u64::MAX { 0 } else { min },
            max,
            clamped,
        }
    }
}

/// An owned, merged latency histogram: what a run reports.
#[derive(Debug, Clone)]
pub struct Histogram {
    counts: Vec<u64>,
    total: u64,
    sum: u128,
    min: u64,
    max: u64,
    clamped: u64,
}

impl Histogram {
    /// Samples in this histogram.
    #[must_use]
    pub fn count(&self) -> u64 {
        self.total
    }

    /// The fastest sample, in nanoseconds, exactly.
    #[must_use]
    pub fn min_nanos(&self) -> u64 {
        self.min
    }

    /// The slowest sample, in nanoseconds, exactly.
    #[must_use]
    pub fn max_nanos(&self) -> u64 {
        self.max
    }

    /// Samples that exceeded [`CEILING_NANOS`].
    ///
    /// Non-zero means the percentile below reads the top bucket, and a reader
    /// needs to know that rather than infer it from a flat-looking tail.
    #[must_use]
    pub fn clamped(&self) -> u64 {
        self.clamped
    }

    /// Mean latency in nanoseconds, for context only.
    ///
    /// A mean is reported next to the percentiles and never instead of them: it
    /// is dragged by exactly the tail this rig exists to look at.
    #[must_use]
    pub fn mean_nanos(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        (self.sum as f64) / (self.total as f64)
    }

    /// The `q` quantile in nanoseconds.
    ///
    /// `q` is clamped to `[0, 1]`; `0` is the fastest sample and `1` the slowest.
    /// An empty histogram reports `0` rather than `NaN`, so a caller printing a
    /// table never prints a token that is not a number.
    #[must_use]
    pub fn quantile_nanos(&self, q: f64) -> u64 {
        if self.total == 0 {
            return 0;
        }
        let q = q.clamp(0.0, 1.0);
        // Round the target up: the quantile is the first bucket at or past the
        // requested share, which is what makes `quantile(1.0)` the maximum
        // rather than the second-largest bucket.
        let target = (q * (self.total as f64)).ceil() as u64;
        let target = target.max(1);
        let mut seen = 0_u64;
        for (slot, count) in self.counts.iter().enumerate() {
            if *count == 0 {
                continue;
            }
            seen = seen.saturating_add(*count);
            if seen >= target {
                return slot_value(slot);
            }
        }
        self.max
    }

    /// p50, p95 and p99 together, the three this rig reports.
    #[must_use]
    pub fn percentiles_nanos(&self) -> (u64, u64, u64) {
        (
            self.quantile_nanos(0.50),
            self.quantile_nanos(0.95),
            self.quantile_nanos(0.99),
        )
    }

    /// An owned histogram of one recorder's current counts.
    ///
    /// The merge path for a single shard, so the seeded model does not have to
    /// wrap its local recorder in an `Arc` to read it back.
    #[must_use]
    pub fn of_one(recorder: &Recorder) -> Self {
        let mut counts = vec![0_u64; SLOTS];
        let mut min = u64::MAX;
        for (slot, bucket) in counts.iter_mut().zip(recorder.counts.iter()) {
            *slot = bucket.load(Ordering::Relaxed);
        }
        let recorded_min = recorder.min_nanos.load(Ordering::Relaxed);
        if recorded_min < min {
            min = recorded_min;
        }
        Self {
            counts,
            total: recorder.total.load(Ordering::Relaxed),
            sum: u128::from(recorder.sum_nanos.load(Ordering::Relaxed)),
            min: if min == u64::MAX { 0 } else { min },
            max: recorder.max_nanos.load(Ordering::Relaxed),
            clamped: recorder.clamped.load(Ordering::Relaxed),
        }
    }
}

/// The sharded recorder the live driver records through.
#[derive(Debug)]
pub struct RecorderSet {
    shards: Vec<Arc<Recorder>>,
    next: AtomicUsize,
}

/// Shards the live driver uses.
///
/// Sixteen, against fifteen cores on the reference host: enough that two tasks
/// rarely contend on one bucket's atomic, few enough that the merge at the end
/// of a window is 16 × 9,216 relaxed loads — under a millisecond.
pub const SHARDS: usize = 16;

impl Default for RecorderSet {
    fn default() -> Self {
        Self::new()
    }
}

impl RecorderSet {
    /// A recorder set with [`SHARDS`] empty shards.
    #[must_use]
    pub fn new() -> Self {
        Self {
            shards: (0..SHARDS).map(|_| Arc::new(Recorder::new())).collect(),
            next: AtomicUsize::new(0),
        }
    }

    /// The next shard, round-robin.
    ///
    /// Taken once per task at placement rather than per sample, so a body's two
    /// samples cannot land in two shards.
    #[must_use]
    pub fn take(&self) -> Arc<Recorder> {
        let index = self.next.fetch_add(1, Ordering::Relaxed) % SHARDS;
        Arc::clone(&self.shards[index])
    }

    /// Every shard, for [`Recorder::merged`].
    #[must_use]
    pub fn shards(&self) -> &[Arc<Recorder>] {
        &self.shards
    }

    /// Zero every shard, keeping the allocations.
    pub fn reset(&self) {
        for shard in &self.shards {
            shard.reset();
        }
    }
}

/// Nanoseconds between two arrivals at `rate_per_second`.
///
/// Zero is returned for a rate of zero rather than dividing by it, and the caller is
/// expected to treat that as "as fast as this process can place work", which is the
/// honest reading of an offered rate the generator cannot express.
///
/// A rate below 1,000 per second truncates to a zero nanosecond period at this
/// precision, which would turn the generator into an unbounded loop that measures its
/// own speed. [`period_picos`] is the resolution the schedule is actually built at.
#[must_use]
pub fn period_nanos(rate_per_second: u64) -> u64 {
    period_picos(rate_per_second) / 1_000
}

/// Picoseconds between two arrivals at `rate_per_second`.
///
/// The schedule's own resolution, a nanosecond finer than any arrival this rig
/// measures: at the smallest rate the simulation models (1,000/s) a nanosecond period
/// is 1,000,000 ns, and truncating to a coarser unit would make every low-rate world
/// degenerate — every arrival in the same instant, no queue, and a saturation arm that
/// proved nothing.
#[must_use]
pub fn period_picos(rate_per_second: u64) -> u64 {
    // A zero rate has no period: there is no next arrival.
    if rate_per_second == 0 {
        return 0;
    }
    // A second is 10^12 ps, which fits a `u64`, and so does every quotient of it.
    1_000_000_000_000 / rate_per_second
}

/// When arrival `index` was **intended** to start, in nanoseconds.
///
/// The whole point of the open-loop driver in one function: the intended instant is
/// `t0 + index × period`, plus at most one period of seeded jitter so a schedule can
/// be given a shape without becoming a different measurement. The live driver passes
/// `jitter = 0` (a uniform offered rate); the seeded model draws one per arrival.
///
/// A nanosecond return for both driver and model, so the schedule the simulation
/// replays is the schedule the measurement runs — the model's finer
/// [`period_picos`] resolution is divided down here rather than used for one side only.
#[must_use]
pub fn intended_arrival_nanos(index: u64, period_nanos: u64, jitter_nanos: u64) -> u64 {
    let base = index.saturating_mul(period_nanos);
    let jitter = jitter_nanos.min(period_nanos.saturating_sub(1));
    base.saturating_add(jitter)
}

/// [`intended_arrival_nanos`] as a [`Duration`], for the live driver's `Instant`.
#[must_use]
pub fn intended_arrival(index: u64, period_nanos: u64, jitter_nanos: u64) -> Duration {
    Duration::from_nanos(intended_arrival_nanos(index, period_nanos, jitter_nanos))
}

/// The word a refused arrival folds into a trace hash.
///
/// Distinct from every value a real event can carry — an intended start is a
/// nanosecond count below [`CEILING_NANOS`] — so a refusal can never be mistaken for
/// an admission that happened to end at the same nanosecond.
pub const REFUSED_MARKER: u64 = u64::MAX;

/// Fold `word` into an FNV-1a trace hash.
///
/// Used only by the seeded model's replay assertion: two runs agree iff every
/// scheduled event agrees, so the hash has to see every event's identity.
#[must_use]
pub fn fold_hash(state: u64, word: u64) -> u64 {
    (state ^ word).wrapping_mul(0x0000_0100_0000_01B3)
}

// ── The seeded model ─────────────────────────────────────────────────────────

/// One seeded open-loop world the simulation drives.
#[derive(Debug, Clone, Copy)]
pub struct SimSpec {
    /// Arrivals offered per second.
    pub offered_rate: u64,
    /// Arrivals offered in total. Fixed rather than derived from a duration, so
    /// two seeds of the same world offer the same *work* and the comparison is
    /// about timing.
    pub arrivals: u64,
    /// In-flight ceiling: how many bodies may run at once.
    pub bound: usize,
    /// Shortest body, in nanoseconds.
    pub service_min_nanos: u64,
    /// Longest body, in nanoseconds.
    pub service_max_nanos: u64,
    /// Whether the arrival schedule is jittered.
    pub jitter: bool,
    /// Whether the bound refuses an arrival rather than queueing it.
    ///
    /// This is the difference between the facade's `spawn` (awaiting a slot, so
    /// the caller queues) and its `try_spawn` (refusing, so the caller does not).
    pub refuse_at_bound: bool,
    /// Seed for arrival jitter and body length.
    pub seed: u64,
}

/// What one seeded world did, and the hash it did it under.
///
/// `p99_intended_nanos` and `p99_actual_nanos` are the same world read two ways,
/// and the gap between them *is* the coordinated-omission effect the live driver
/// is built to report: a model that measured from the actual start would report a
/// p99 that barely moves as the offered rate passes the knee, which is the
/// failure this rig exists not to commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimTrace {
    /// Arrivals offered.
    pub offered: u64,
    /// Arrivals admitted.
    pub admitted: u64,
    /// Arrivals refused at the bound.
    pub refused: u64,
    /// Arrivals whose body ran to completion.
    pub completed: u64,
    /// p50 latency from the **intended** start, in nanoseconds.
    pub p50_intended_nanos: u64,
    /// p99 latency from the intended start.
    pub p99_intended_nanos: u64,
    /// p99 latency from the **actual** start — the closed-loop figure, kept so a
    /// test can show the difference coordinated omission makes.
    pub p99_actual_nanos: u64,
    /// The slowest sample, exactly.
    pub max_nanos: u64,
    /// Most bodies running at once. Bounded by the ceiling, by construction.
    pub peak_in_flight: usize,
    /// Deepest occupancy-plus-backlog the run reached: bodies running now, plus the
    /// arrivals the generator had fallen behind by.
    ///
    /// Two facts in one figure because the live driver samples one series, and because
    /// a reader watching a queue wants both at once. `peak_in_flight` is the half the
    /// ceiling bounds.
    pub peak_queue: usize,
    /// The generator's own worst lag behind the schedule, in nanoseconds.
    pub max_generator_lag_nanos: u64,
    /// The replay identity of this world.
    pub trace_hash: u64,
}

/// Run one seeded world and report what it did.
///
/// A discrete-event model of the same two things the live driver does — schedule an
/// arrival, admit or refuse it against a ceiling of `bound`, run a body — on a logical
/// clock driven entirely by the seed. Wall time plays no part, which is what makes the
/// same seed replay exactly and a different seed diverge.
///
/// # What the clock is
///
/// The model's `now` advances to *each arrival's own* intended start before the arrival
/// is considered, exactly as the live generator is scheduled. That is not a detail: a
/// model that let `now` jump to the current arrival's *start* would let a second-deep
/// queue look empty, because each arrival would find its own start before the first
/// arrival's service had ended. The lag the model reports is therefore the lag a live
/// generator at that offered rate would really have.
///
/// # The bound is a scan
///
/// Admission picks the slot that finishes first, by scanning all `bound` of them, so
/// the model is for the small bounds a simulation can sweep exhaustively (the suite
/// uses 1, 4 and 64). It models the *schedule and admission arithmetic*, not throughput;
/// the live rig measures that.
///
/// # The trace hash sees every arrival
///
/// Every arrival folds in — admitted ones as `(index, intended, start, end)` and refused
/// ones as `(index, refused)` — so two seeds differ in the hash whenever their schedules
/// differ at all, including on a world where the two made the same admissions. Folding
/// only the admissions would make a hash that reports "seed-driven" while two seeds
/// happened to agree on every decision.
#[must_use]
pub fn simulate(spec: &SimSpec) -> SimTrace {
    let period = period_nanos(spec.offered_rate);
    let bound = spec.bound.max(1);
    let mut free_at = vec![0_u64; bound];
    let mut rng = super::async_stats::Rng::new(spec.seed);
    let service_span = spec
        .service_max_nanos
        .saturating_sub(spec.service_min_nanos)
        + 1;
    let recorder = Recorder::new();
    let recorder_actual = Recorder::new();
    let mut admitted = 0_u64;
    let mut refused = 0_u64;
    let mut completed = 0_u64;
    let mut peak_in_flight = 0_usize;
    let mut peak_queue = 0_usize;
    let mut max_lag = 0_u64;
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    // The model clock, advanced to each arrival's intended start before it is served.
    let mut now = 0_u64;

    for index in 0..spec.arrivals {
        let jitter = if spec.jitter {
            rng.next_u64() % period.max(1)
        } else {
            0
        };
        let intended = intended_arrival_nanos(index, period, jitter);
        // The schedule advances the clock to the arrival's own start, whether or not
        // the queue is empty — the same rule the live generator's sleep does.
        now = now.max(intended);

        // Every slot busy at the arrival instant is the ceiling reached, which is what
        // the facade's `try_spawn` refuses on and its `spawn` waits through.
        let busy = free_at.iter().filter(|free| **free > now).count();
        if spec.refuse_at_bound && busy >= bound {
            refused = refused.saturating_add(1);
            hash = fold_hash(fold_hash(hash, index), REFUSED_MARKER);
            continue;
        }

        // The slot whose body finishes first is the one this arrival takes.
        let mut earliest = u64::MAX;
        let mut slot = 0_usize;
        for (candidate, free) in free_at.iter().enumerate() {
            if *free < earliest {
                earliest = *free;
                slot = candidate;
            }
        }
        let start = earliest.max(now);
        let end = start.saturating_add(spec.service_min_nanos + rng.next_u64() % service_span);
        free_at[slot] = end;

        // Two separate facts, which an earlier version of this model conflated into one
        // number and got backwards: how many bodies are *running* (bounded by the
        // ceiling, and what the in-flight bound is about), and how many arrivals are
        // *waiting* (the generator's own lag divided by the arrival period, which is the
        // backlog the offered rate built and exactly what the live driver's queue sample
        // measures). A wider ceiling runs more bodies at once, so it can hold a *larger*
        // occupancy while queueing less — and a single "queue depth" column would have
        // reported the ceiling's capacity as if it were its backlog.
        let in_flight = free_at.iter().filter(|free| **free > now).count();
        peak_in_flight = peak_in_flight.max(in_flight);
        let lag = start.saturating_sub(intended);
        max_lag = max_lag.max(lag);
        let backlog = lag / period.max(1);
        let queue = (in_flight as u64).saturating_add(backlog);
        peak_queue = peak_queue.max(queue as usize);

        admitted = admitted.saturating_add(1);
        completed = completed.saturating_add(1);
        recorder.record(end.saturating_sub(intended));
        recorder_actual.record(end.saturating_sub(start));
        hash = fold_hash(
            fold_hash(fold_hash(fold_hash(hash, index), intended), start),
            end,
        );
    }

    let histogram = Histogram::of_one(&recorder);
    let actual = Histogram::of_one(&recorder_actual);
    SimTrace {
        offered: spec.arrivals,
        admitted,
        refused,
        completed,
        p50_intended_nanos: histogram.quantile_nanos(0.50),
        p99_intended_nanos: histogram.quantile_nanos(0.99),
        p99_actual_nanos: actual.quantile_nanos(0.99),
        max_nanos: histogram.max_nanos(),
        peak_in_flight,
        peak_queue,
        max_generator_lag_nanos: max_lag,
        trace_hash: hash,
    }
}
