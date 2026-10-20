//! The open-loop generator: one arrival schedule, a bounded queue-depth trace,
//! the arrival both engines consume, and the run that offers one side's load.
//!
//! Moved verbatim from `main.rs` for #639; the items below are unchanged.

use super::*;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::engine::{Admission, BaselineEngine, Engine, FacadeEngine, Side};
use crate::gate::SideRun;
use crate::openloop::{Recorder, RecorderSet, intended_arrival, period_nanos};
use crate::support::{body_unit, nanos, narrow};
/// Queue-depth samples kept per open-loop run.
///
/// Bounded, because a run is a measurement and a measurement's own storage is a cost
/// it must not impose on what it measures. 65,536 samples covers a 65-second run at
/// one sample per millisecond; samples past the cap are counted, not kept.
const DEPTH_SAMPLE_CAP: usize = 65_536;

/// One open-loop run: an offered rate, a window, and a ceiling.
#[derive(Clone, Copy, Debug)]
pub(crate) struct OpenLoopSpec {
    /// How long each body takes, in microseconds, on top of its work unit.
    ///
    /// A declared parameter rather than an accident of the workload, because the knee is
    /// a joint property of the offered rate *and* the body's cost, and a curve that only
    /// varies one of them measures the other by accident. At zero the body is a counter
    /// bump and a yield — about a microsecond — and a ceiling of 64 then admits roughly
    /// 64 million arrivals a second, which an in-process generator cannot offer, so the
    /// ceiling never binds and the "refuse" door never refuses. Any figure about refusal,
    /// drain or recovery is therefore meaningless unless the body is declared.
    pub(crate) body_micros: u64,
    /// Arrivals offered per second.
    pub(crate) offered_rate: u64,
    /// How long arrivals are offered for.
    ///
    /// A ceiling on the run, never the run's definition: the *arrival count* below is.
    /// Offering "for one second" lets a faster side place more arrivals than a slower
    /// one, and the difference would be a property of the clock rather than of the
    /// engine — which is exactly what the fairness gate exists to refuse, and it did
    /// refuse it, naming `placed`. So a run offers the arrivals the rate and the window
    /// define and stops on the count, and the window only bounds how long a slow side may
    /// take to offer them.
    pub(crate) window: Duration,
    /// Arrivals to offer, or `None` to offer until the window closes.
    pub(crate) arrivals: Option<u64>,
    /// The in-flight ceiling.
    pub(crate) bound: usize,
    /// Report every `drop_every`-th arrival as placed without placing it.
    ///
    /// Zero on every honest run. This is the negative control the conservation gate
    /// has to catch: a driver that counts an admission it never made reports a
    /// latency for work that never ran, the same class of defect `--mutant-check`
    /// guards for the closed-loop rig.
    pub(crate) drop_every: u64,
}

/// A bounded queue-depth trace, plus the two facts a reader needs from it.
#[derive(Debug, Default)]
pub(crate) struct DepthTrace {
    pub(crate) samples: Vec<usize>,
    pub(crate) dropped: u64,
    pub(crate) peak: usize,
}

impl DepthTrace {
    /// A trace with room for a first window's worth of samples.
    pub(crate) fn new() -> Self {
        Self {
            samples: Vec::with_capacity(4_096),
            dropped: 0,
            peak: 0,
        }
    }

    /// Record one queue depth, in arrivals.
    pub(crate) fn push(&mut self, depth: usize) {
        self.peak = self.peak.max(depth);
        if self.samples.len() < DEPTH_SAMPLE_CAP {
            self.samples.push(depth);
        } else {
            self.dropped = self.dropped.saturating_add(1);
        }
    }

    /// Mean depth over the samples kept.
    pub(crate) fn mean(&self) -> f64 {
        if self.samples.is_empty() {
            return 0.0;
        }
        let total: u64 = self.samples.iter().map(|depth| widen(*depth)).sum();
        (total as f64) / (self.samples.len() as f64)
    }
}

/// Wait until `intended`, unless it has already passed.
///
/// A generator that sleeps unconditionally pays a timer round-trip per arrival, and
/// at a million arrivals per second that timer *is* the offered rate. Past saturation
/// the generator is supposed to fall behind, and falling behind means offering as fast
/// as it can — so the wait is skipped exactly when it is already late, and the lag is
/// charged to the latency instead.
pub(crate) async fn wait_until(intended: Instant) {
    let now = Instant::now();
    if now < intended {
        tokio::time::sleep_until(tokio::time::Instant::from_std(intended)).await;
    }
}

/// Sample the queue after one arrival and return the generator's lag.
///
/// The queue depth is "bodies running now, plus arrivals that are due but not yet
/// running". The second term is the generator's own lag divided by the arrival period,
/// so it reads as an arrival count rather than as a duration, and it is what a
/// backpressure door accumulates when it falls behind: the caller's own loop *is* the
/// queue, and this is its depth.
pub(crate) fn sample_queue(
    now: Instant,
    next_intended: Instant,
    period: u64,
    in_flight: u64,
    depth: &mut DepthTrace,
) -> Duration {
    let lag = now.saturating_duration_since(next_intended);
    let waiting = nanos(lag) / period.max(1);
    depth.push(narrow(waiting.saturating_add(in_flight)));
    lag
}

/// Record one arrival's admission outcome.
pub(crate) fn count_arrival(placed: bool, admitted: &mut u64, refused: &mut u64) {
    if placed {
        *admitted = admitted.saturating_add(1);
    } else {
        *refused = refused.saturating_add(1);
    }
}

/// Nanoseconds from `intended` to now.
///
/// A `u128` from `elapsed()` into a `u64` cannot overflow in practice — a stalled run
/// of 584 years would — and the recorder clamps past its own ceiling, so the
/// conversion is stated rather than assumed.
fn latency_nanos(intended: Instant) -> u64 {
    nanos(intended.elapsed())
}

/// The intended instant for arrival `index` of a schedule that began at `t0`.
pub(crate) fn intended_at(t0: Instant, index: u64, period: u64) -> Instant {
    t0 + intended_arrival(index, period, 0)
}

/// One arrival's identity: what its body runs on, and where its latency starts.
///
/// Bundled rather than threaded as four parameters, because the same four travelled
/// through the [`Engine`] trait, both of its implementations, the baseline's spawn helper
/// and the generator — six signatures that were each an identical list of four names.
/// Six copies of a list is six chances for the two engines to end up with two different
/// definitions of what an arrival is, and the whole comparison rests on them sharing one.
#[derive(Clone, Debug)]
pub(crate) struct Arrival {
    /// The counter this body's work unit bumps.
    pub(crate) counter: Arc<AtomicU64>,
    /// The recorder shard this body's one latency lands in.
    ///
    /// Taken once, here, and moved into the body — so the shard a body's single sample
    /// lands in cannot differ between two samples of the same body, which is what makes
    /// the merge across shards an addition rather than a puzzle.
    pub(crate) shard: Arc<Recorder>,
    /// The instant this arrival was **intended** to start, not the instant it was placed.
    pub(crate) intended: Instant,
    /// This body's own service cost, in microseconds, on top of its work unit.
    pub(crate) body_micros: u64,
}

impl Arrival {
    /// Mint an arrival intended for `intended`, recording into `shard`.
    pub(crate) fn mint(
        work: &Arc<AtomicU64>,
        recorders: &RecorderSet,
        intended: Instant,
        body_micros: u64,
    ) -> Self {
        Self {
            counter: Arc::clone(work),
            shard: recorders.take(),
            intended,
            body_micros,
        }
    }

    /// Destructure into the counter, the shard and the intended instant.
    ///
    /// One destructuring for every caller that has to add its own handle to an arrival
    /// (the in-flight tiers' gate), so the three names are named once rather than matched
    /// by three separate patterns that could drift apart.
    pub(crate) fn into_parts(self) -> (Arc<AtomicU64>, Arc<Recorder>, Instant) {
        (self.counter, self.shard, self.intended)
    }
}

/// The body an open-loop arrival runs: the same work unit every side runs, then its
/// latency from the **intended** start.
///
/// Measuring from the intended instant rather than from the body's own first poll is
/// the whole coordinated-omission correction: a generator that fell behind reports the
/// queue it created rather than the service it managed to reach.
pub(crate) async fn recorded_body_unit(arrival: Arrival) {
    let Arrival {
        counter,
        shard,
        intended,
        body_micros,
    } = arrival;
    body_unit(counter).await;
    if body_micros > 0 {
        tokio::time::sleep(Duration::from_micros(body_micros)).await;
    }
    shard.record(latency_nanos(intended));
}

/// The resident set size of this process, and how it was read.
///
/// Linux reads its own `/proc/self/status`, which is a file the process already has
/// and a high-water mark rather than a sample. macOS has no in-process equivalent
/// without a `getrusage` edge, so the reading is a `ps` sample of the process's
/// **current** RSS, taken at the moment the run is at its peak. The two are different
/// measurements and each is labelled: a peak figure reported under the name of the
/// other is a false claim, and an unavailable one reported as a small one is worse
/// than an absent one.
pub(crate) fn resident_bytes() -> (Option<u64>, &'static str) {
    #[cfg(target_os = "linux")]
    {
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            for line in status.lines() {
                if let Some(value) = line.strip_prefix("VmHWM:")
                    && let Some(kib) = value.split_whitespace().next()
                    && let Ok(kib) = kib.parse::<u64>()
                {
                    return (
                        Some(kib.saturating_mul(1024)),
                        "/proc/self/status VmHWM (peak)",
                    );
                }
            }
        }
        (None, "/proc/self/status VmHWM unreadable")
    }
    #[cfg(not(target_os = "linux"))]
    {
        // `ps` rather than `getrusage`: the first is a system utility this instrument
        // may run, the second is a C edge no crate here may author.
        let sample = std::process::Command::new("/bin/ps")
            .args(["-o", "rss=", "-p"])
            .arg(std::process::id().to_string())
            .output();
        let read = match sample {
            Ok(output) => String::from_utf8_lossy(&output.stdout)
                .split_whitespace()
                .next()
                .and_then(|kib| kib.parse::<u64>().ok())
                .map(|kib| kib.saturating_mul(1024)),
            Err(_) => None,
        };
        match read {
            Some(bytes) => (Some(bytes), "/bin/ps -o rss= (current, at peak)"),
            None => (None, "/bin/ps -o rss= unreadable"),
        }
    }
}

/// Offer `spec.offered_rate` for `spec.window` to one engine, and report what it did.
///
/// The generator is one loop: join what has finished, sleep to the arrival's intended
/// instant, offer it through the engine's door, then sample the queue depth. The
/// latency is measured inside the body from the **intended** instant, which is what
/// makes the p99 comparable across offered rates and across engines.
async fn open_loop_run<E: Engine>(
    engine: &mut E,
    side: Side,
    admission: Admission,
    spec: &OpenLoopSpec,
) -> Result<SideRun, String> {
    let work = Arc::new(AtomicU64::new(0));
    let recorders = RecorderSet::new();
    let period = period_nanos(spec.offered_rate);
    let t0 = Instant::now();
    let mut offered = 0_u64;
    let mut admitted = 0_u64;
    let mut refused = 0_u64;
    let mut depth = DepthTrace::new();
    let mut max_lag = Duration::ZERO;

    loop {
        if let Some(limit) = spec.arrivals
            && offered >= limit
        {
            break;
        }
        if spec.arrivals.is_none() && t0.elapsed() >= spec.window {
            break;
        }
        engine.reap();
        let intended = intended_at(t0, offered, period);
        wait_until(intended).await;
        offered = offered.saturating_add(1);
        // The negative control's decision, made once per arrival so an honest run
        // pays one predictable branch.
        let place = spec.drop_every == 0 || !offered.is_multiple_of(spec.drop_every);
        let placed = if place {
            engine
                .admit(Arrival::mint(&work, &recorders, intended, spec.body_micros))
                .await
        } else {
            // The control's defect in one line: the arrival is counted as offered and
            // reported as admitted, and nothing runs.
            true
        };
        count_arrival(placed, &mut admitted, &mut refused);
        let next_intended = intended_at(t0, offered, period);
        max_lag = max_lag.max(sample_queue(
            Instant::now(),
            next_intended,
            period,
            engine.in_flight(),
            &mut depth,
        ));
    }

    let offered_at_end = Instant::now();
    let (rss_bytes, rss_source) = resident_bytes();
    let drain_started = Instant::now();
    engine.drain().await;
    let drain = drain_started.elapsed();
    let (completed, cancelled, aborted) = engine.terminals();
    Ok(SideRun {
        side,
        bound: spec.bound,
        offered_rate: spec.offered_rate,
        body_micros: spec.body_micros,
        admission,
        offered,
        admitted,
        refused,
        completed,
        cancelled,
        aborted,
        work_units: work.load(Ordering::SeqCst),
        in_flight_at_end: engine.in_flight(),
        depth,
        max_lag,
        histogram: Recorder::merged(recorders.shards()),
        wall: offered_at_end.duration_since(t0),
        drain,
        rss_bytes,
        rss_source,
    })
}

/// Build the engine for one side at one door and run the generator against it.
pub(crate) async fn open_loop_side(
    side: Side,
    admission: Admission,
    spec: &OpenLoopSpec,
) -> Result<SideRun, String> {
    match side {
        Side::Facade => {
            let mut engine = FacadeEngine::new(spec.bound, admission);
            open_loop_run(&mut engine, side, admission, spec).await
        }
        Side::Baseline => {
            let mut engine = BaselineEngine::new(spec.bound, admission);
            open_loop_run(&mut engine, side, admission, spec).await
        }
    }
}
