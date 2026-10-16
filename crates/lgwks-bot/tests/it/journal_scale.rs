//! Append-path scale: latency tails and concurrent tenant appends.
//!
//! Three measurements against the real [`FileJournal`] on real files: the
//! p50/p95/p99 of one acknowledged append (the shipped path, where the write
//! and `sync_all` run on the storage owner thread, not the caller's), and a
//! tiered sweep of concurrent tenant journals at 100, 1,000 and 10,000.
//!
//! # The tier ceiling
//!
//! Each tenant journal costs two OS threads — the tenant's own driver thread
//! and the journal's storage-owner thread — and two open descriptors. A tier
//! whose level a host cannot reach is clamped to the level the process really
//! can, and the requested, reached and ceiling levels are recorded together, so
//! a reader is never told a concurrency number nobody ran (INV-BOT-16's rule,
//! applied here). The ceiling is the smaller of the descriptor budget
//! `ulimit -n` names and the thread budget this process can actually spawn,
//! measured by a parked-thread probe rather than assumed, less a reserve for
//! the harness.
//!
//! # Where the numbers go
//!
//! The tail and the tier lines are appended to the file named by
//! `LGWKS_SCALE_OUT` when that variable is set, so the report can carry a
//! measured value rather than an assertion that merely passed. Nothing is
//! printed: the workspace forbids it.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use lgwks_bot::journal::{DurabilityPromise, EffectEvent, EffectJournal, FileJournal};

use crate::journal_fixtures as shared;

use shared::{TempGuard, key, percentile, record_measurement, scratch};

/// How many acknowledged appends the latency family times.
const SAMPLES: usize = 1_024;

/// The concurrency levels the contract names, in the order it names them.
const TIERS: [usize; 3] = [100, 1_000, 10_000];

/// Events one tenant journal writes: a two-rung identity, so a lost or a
/// duplicated append changes the count and the reopened sequence.
const EVENTS_PER_TENANT: usize = 2;

/// The attempt-number stride between tenants, so no two tenants ever write the
/// same effect key and a key collision cannot masquerade as cross-tenant bleed.
const ATTEMPT_STRIDE: u64 = 1_000;

/// OS threads one tenant journal costs: its driver thread and its journal's
/// storage-owner thread. The architecture is one storage owner per journal.
const THREADS_PER_TENANT: u64 = 2;

/// Open descriptors one tenant journal holds: the writer and the fence view.
const FDS_PER_TENANT: u64 = 2;

/// Threads held back from the measured budget for the harness, the sampler and
/// nextest's own runner, so the real run does not sit exactly on the ceiling.
const THREAD_RESERVE: u64 = 256;

/// The most parked threads the probe creates: what the largest tier needs
/// (10,000 tenants x [`THREADS_PER_TENANT`]) plus [`THREAD_RESERVE`], and no more.
///
/// The probe used to spawn until the OS refused a thread. On Linux that limit is
/// per *user*, so while the probe sat at exhaustion the test runner itself could
/// not fork the next test (`Resource temporarily unavailable (os error 11)`,
/// main CI run 37091933076). Measuring only up to the level a tier can use
/// answers the same question without taking the host's last thread.
const MAX_PROBE_THREADS: usize = 20_256;

/// The ceiling assumed when neither `ulimit -n` nor the thread probe answers:
/// the level round two proved, kept so the tier still runs rather than folding
/// to zero on a host that hides its limits.
const FALLBACK_TENANTS: usize = 100;

/// How often the RSS sampler reads the process's resident set, in
/// milliseconds.
const RSS_SAMPLE_MS: u64 = 10;

type TestResult = Result<(), Box<dyn Error>>;

/// The attempt number tenant `tenant` writes for `step`.
fn attempt_of(tenant: usize, step: usize) -> Result<u64, std::num::TryFromIntError> {
    let stride = u64::try_from(tenant)?.saturating_mul(ATTEMPT_STRIDE);
    Ok(stride.saturating_add(u64::try_from(step)?))
}

/// The exact events tenant `tenant` must hold after its appends.
fn tenant_events(tenant: usize) -> Result<Vec<EffectEvent>, Box<dyn Error>> {
    let mut events = Vec::with_capacity(EVENTS_PER_TENANT);
    for step in 1..=EVENTS_PER_TENANT {
        events.push(EffectEvent::IntentAdmitted {
            key: key(attempt_of(tenant, step)?)?,
        });
    }
    Ok(events)
}

/// One tenant's whole append path, on its own journal.
///
/// Opens the journal, writes the whole two-rung ladder as one acknowledged
/// batch and times it, then returns how many events the live handle holds so
/// the caller can see no other tenant's events landed in it. The one append per
/// tenant is what keeps ten thousand tenants inside a gate's time budget; the
/// single-append tail is the dedicated latency family's subject.
fn append_tenant(
    path: &Path,
    tenant: usize,
    sink: &Arc<Mutex<Vec<u128>>>,
) -> Result<usize, String> {
    let mut journal = FileJournal::open(path).map_err(|error| error.to_string())?;
    let events = tenant_events(tenant).map_err(|error| error.to_string())?;
    let start = Instant::now();
    let acks = journal
        .compare_and_append_all(&events)
        .map_err(|error| error.to_string())?;
    let elapsed = start.elapsed().as_micros();
    if acks.len() != EVENTS_PER_TENANT {
        let refusal = Err("the batch did not acknowledge every rung".to_owned());
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "append_tenant: returning an error to the caller");
        return refusal;
    }
    let held = journal.events().count();
    sink.lock()
        .map_err(|error| format!("the latency sink was poisoned: {error}"))?
        .push(elapsed);
    Ok(held)
}

/// The host's measured tenant ceiling and the inputs it came from.
struct Ceiling {
    /// How many concurrent tenant journals the host can hold.
    tenants: usize,
    /// The threads the parked-thread probe actually created, if it measured.
    measured_threads: Option<u64>,
    /// The soft descriptor limit `ulimit -n`, if the shell answered.
    fd_limit: Option<u64>,
}

/// Measure the host's tenant ceiling from its descriptor and thread budgets.
fn host_tenant_ceiling() -> Ceiling {
    let fd_limit = read_ulimit("-n");
    let measured_threads = measure_thread_capacity();
    let fd_ceiling = fd_limit.and_then(|limit| limit.checked_div(FDS_PER_TENANT));
    let thread_ceiling = measured_threads.and_then(|threads| {
        threads
            .saturating_sub(THREAD_RESERVE)
            .checked_div(THREADS_PER_TENANT)
    });
    let tenants = match (fd_ceiling, thread_ceiling) {
        (Some(from_fds), Some(from_threads)) => from_fds.min(from_threads),
        (Some(from_fds), None) => from_fds,
        (None, Some(from_threads)) => from_threads,
        // Neither budget answered: keep the level round two proved, which is
        // what the fallback constant is for, rather than folding to zero on a
        // host that hides its limits.
        (None, None) => {
            return Ceiling {
                tenants: FALLBACK_TENANTS,
                measured_threads,
                fd_limit,
            };
        }
    };
    Ceiling {
        // A budget this host cannot address is clamped by its own address space,
        // which is then the real ceiling: neither `ulimit -n` nor the thread
        // probe answers anywhere near that, so the arm is the host's own limit
        // rather than a substituted number.
        tenants: match usize::try_from(tenants) {
            Ok(tenants) => tenants.max(1),
            Err(_too_wide) => usize::MAX,
        },
        measured_threads,
        fd_limit,
    }
}

/// How many OS threads this process can really create, by creating them.
///
/// The probe spawns parked threads until the OS refuses one, then wakes and
/// joins every one of them, so it measures the reachable level rather than
/// reading a limit that a host may not enforce. A join that fails means a probe
/// thread panicked, which makes the measurement meaningless, so the answer is
/// then `None`.
fn measure_thread_capacity() -> Option<u64> {
    let mut parked: Vec<std::thread::JoinHandle<()>> = Vec::new();
    while parked.len() < MAX_PROBE_THREADS {
        match std::thread::Builder::new()
            .name("thread-probe".to_owned())
            .spawn(std::thread::park)
        {
            Ok(handle) => parked.push(handle),
            Err(_) => break,
        }
    }
    let reached = parked.len();
    for handle in parked {
        handle.thread().unpark();
        if handle.join().is_err() {
            return None;
        }
    }
    if reached == 0 {
        return None;
    }
    // A level this host cannot name as a `u64` is not one any tier here can
    // reach, so the answer is "not measured" rather than the largest number there
    // is, which would read as a ceiling nobody hit.
    let Ok(reached) = u64::try_from(reached) else {
        return None;
    };
    Some(reached)
}

/// A soft resource limit `ulimit <flag>` reports, in the shell that inherits
/// this process's limits.
#[cfg(unix)]
fn read_ulimit(flag: &str) -> Option<u64> {
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("ulimit {flag}"))
        .output()
        .ok()?;
    std::str::from_utf8(&output.stdout)
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// No shell limit to read off Unix; the ceiling falls back to the thread probe.
#[cfg(not(unix))]
fn read_ulimit(_flag: &str) -> Option<u64> {
    None
}

/// The process's resident set in kilobytes, or `None` when it cannot be read.
///
/// Linux already tracks the high-water mark, so `VmHWM` is the peak directly;
/// the sampler that calls this still takes its maximum so the same code is
/// correct on both platforms.
#[cfg(target_os = "linux")]
fn read_rss_kb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

/// The process's resident set in kilobytes, read from `ps` on the BSDs and
/// macOS, where there is no `VmHWM`.
///
/// The shell's `$PPID` is this test process, because the shell is spawned
/// directly by it. That addresses the stat read without `std::process::id`,
/// whose value is an OS-reused identifier rather than an identity and is
/// therefore refused by the `std`-first gate.
#[cfg(all(unix, not(target_os = "linux")))]
fn read_rss_kb() -> Option<u64> {
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg("ps -o rss= -p $PPID")
        .output()
        .ok()?;
    std::str::from_utf8(&output.stdout)
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// No resident-set reading on a non-Unix host.
#[cfg(not(unix))]
fn read_rss_kb() -> Option<u64> {
    None
}

/// The larger of the peak so far and a new reading; the reading alone when
/// nothing was read before it.
fn larger_reading(peak: Option<u64>, reading: u64) -> u64 {
    match peak {
        Some(was) => was.max(reading),
        None => reading,
    }
}

/// A background sampler that tracks the process's peak resident set.
///
/// Sampling rather than a single end-of-run read, because the peak is the
/// concurrency claim and by the time a tier ends the journals are closed. The
/// thread is owned: it is joined on [`RssPeak::stop`], and [`Drop`] stops it
/// too, so an early return cannot leak a spinning sampler.
struct RssPeak {
    /// Set when the sampler should stop.
    stop: Arc<AtomicBool>,
    /// The sampler thread, yielding the peak it read or `None` if it read none.
    handle: Option<std::thread::JoinHandle<Option<u64>>>,
}

impl RssPeak {
    /// Start sampling the process's resident set.
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("rss-peak".to_owned())
            .spawn(move || {
                // The peak is the largest reading this host actually took. A
                // sample it could not read leaves the peak alone rather than
                // lowering it, and a host with no reading at all reports none.
                let mut peak: Option<u64> = None;
                while !flag.load(Ordering::Relaxed) {
                    if let Some(kb) = read_rss_kb() {
                        peak = Some(larger_reading(peak, kb));
                    }
                    std::thread::park_timeout(Duration::from_millis(RSS_SAMPLE_MS));
                }
                if let Some(kb) = read_rss_kb() {
                    peak = Some(larger_reading(peak, kb));
                }
                peak
            })
            .ok();
        Self { stop, handle }
    }

    /// Stop sampling and report the peak, if anything was sampled.
    fn stop(mut self) -> Option<u64> {
        self.stop.store(true, Ordering::Relaxed);
        let handle = self.handle.take()?;
        handle.join().ok().flatten()
    }
}

impl Drop for RssPeak {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            drop(handle.join());
        }
    }
}

/// One tier's measured result.
struct TierReport {
    /// The level the contract names.
    requested: usize,
    /// The level actually run.
    reached: usize,
    /// The wanted level the host's ceiling allows.
    ceiling: usize,
    /// The 50th percentile append latency, microseconds, if the tier sampled any.
    p50: Option<u128>,
    /// The 95th percentile append latency, microseconds, if the tier sampled any.
    p95: Option<u128>,
    /// The 99th percentile append latency, microseconds, if the tier sampled any.
    p99: Option<u128>,
    /// The process's peak resident set during the tier, kilobytes.
    peak_rss_kb: Option<u64>,
}

impl TierReport {
    /// Append this tier's line to the measurement file.
    fn record(&self) -> std::io::Result<()> {
        record_measurement(&format!(
            "tier requested={} reached={} ceiling={} p50_us={} p95_us={} p99_us={} peak_rss_kb={}",
            self.requested,
            self.reached,
            self.ceiling,
            display_limit(self.p50),
            display_limit(self.p95),
            display_limit(self.p99),
            display_limit(self.peak_rss_kb),
        ))
    }
}

/// Render an optional measurement for the report, naming what was not measured.
///
/// One renderer for every optional reading in this file, so "unmeasured" means
/// the same thing for a percentile and for a resident set.
fn display_limit<T: std::fmt::Display>(value: Option<T>) -> String {
    match value {
        Some(number) => number.to_string(),
        None => String::from("unmeasured"),
    }
}

/// Run one tier at the level the ceiling allows and prove its properties.
///
/// Opens `min(requested, ceiling)` concurrent tenant journals, each on its own
/// file and its own storage owner, appends the two-rung ladder to each, then
/// reopens every file and requires its exact own events. That is isolation, zero
/// lost and zero duplicated at once: a lost append shortens the sequence and a
/// duplicated one lengthens it, and a foreign event is not in the expected
/// sequence.
fn run_tier(requested: usize, ceiling: usize) -> Result<TierReport, Box<dyn Error>> {
    let target = requested.min(ceiling);
    let dir = scratch(&format!("tier-{requested}"));
    std::fs::create_dir_all(&dir)?;
    let _guard = TempGuard(dir.clone());

    let peak = RssPeak::start();
    let latencies: Arc<Mutex<Vec<u128>>> = Arc::new(Mutex::new(Vec::new()));
    let mut handles = Vec::with_capacity(target);
    let mut paths: Vec<PathBuf> = Vec::with_capacity(target);
    for tenant in 0..target {
        let path = dir.join(format!("tenant-{tenant}.jrnl"));
        let thread_path = path.clone();
        let sink = Arc::clone(&latencies);
        match std::thread::Builder::new()
            .name(format!("tenant-{tenant}"))
            .spawn(move || append_tenant(&thread_path, tenant, &sink))
        {
            Ok(handle) => {
                handles.push(handle);
                paths.push(path);
            }
            Err(error) => {
                let refusal = Err(format!(
                    "the host refused a tenant thread at {tenant} of {target} with a \
                 measured ceiling of {ceiling}: {error}"
                )
                .into());
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "run_tier: returning an error to the caller");
                return refusal;
            }
        }
    }

    for handle in handles {
        let count = handle.join().map_err(|panic| {
            std::io::Error::other(format!("a tenant thread panicked: {panic:?}"))
        })??;
        assert_eq!(
            count, EVENTS_PER_TENANT,
            "a tenant journal held another tenant's events"
        );
    }

    let mut samples = latencies
        .lock()
        .map_err(|error| std::io::Error::other(format!("the latency sink was poisoned: {error}")))?
        .clone();
    samples.sort_unstable();
    let p50 = percentile(&samples, 500);
    let p95 = percentile(&samples, 950);
    let p99 = percentile(&samples, 990);
    // The peak is the concurrent phase's claim, and it is over: every tenant has
    // joined. The sampler is stopped before the reopen below because on macOS it
    // spawns `ps` through a shell, and a spawned child holds a copy of every
    // descriptor the process has open until its exec closes them — including a
    // journal's locked one. A reopen landing in that window was refused
    // `Locked` by a lock no writer held, and a loaded host widens the window.
    let peak_rss_kb = peak.stop();

    for (tenant, path) in paths.iter().enumerate() {
        let journal = FileJournal::open(path)?;
        let held: Vec<EffectEvent> = journal.events().copied().collect();
        let expected = tenant_events(tenant)?;
        assert_eq!(
            held, expected,
            "tenant {tenant} did not reopen to exactly its own {EVENTS_PER_TENANT} events, \
             so an append was lost, duplicated, or crossed a tenant boundary"
        );
    }

    Ok(TierReport {
        requested,
        reached: target,
        ceiling,
        p50,
        p95,
        p99,
        peak_rss_kb,
    })
}

/// The size of the bare write beside each append, near one framed event.
const CONTROL_BYTES: usize = 128;

/// A bare durable write that is reached and answered the way an append is.
///
/// An append is handed to its storage owner's thread and answered back: the owner
/// wakes, writes and syncs, and the caller wakes again, so a host with a long run
/// queue charges it a wait at each wake. A bare write on the caller's own thread
/// is charged none of those waits, so on a loaded host it read about a quarter of
/// the path (run 38938628208: append p50 13,488us against 3,251us bare, where an
/// idle host reads 78us against 68us) and every stall the run queue dealt landed
/// on the path alone. This control makes the same two hand-offs around the same
/// write and `sync_all`, so the two differ by what the path adds and not by how
/// many times each waits to be scheduled.
struct HoppedWrite {
    /// What the caller asked for and what the writer answered.
    slot: Arc<Slot>,
    /// The writer's thread, joined on drop so it never outlives the test.
    writer: Option<std::thread::JoinHandle<()>>,
}

/// The hand-off state and the one condition variable both sides wait on, as the
/// storage owner's request slot has them.
struct Slot {
    /// One request in flight, one answer waiting.
    state: Mutex<Handoff>,
    /// Woken on every change of `state`.
    signal: Condvar,
}

/// The control's hand-off state: one request in flight, one answer waiting.
#[derive(Default)]
struct Handoff {
    /// A write was asked for and the writer has not taken it yet.
    asked: bool,
    /// The outcome of the last write, until the caller takes it.
    answered: Option<std::io::Result<()>>,
    /// The caller is done with the writer.
    stop: bool,
}

/// A poisoned hand-off slot, as the error a test reports.
fn poisoned<T>(error: std::sync::PoisonError<T>) -> String {
    format!("the control's hand-off slot was poisoned: {error}")
}

impl HoppedWrite {
    /// Start the writer thread on `file`.
    fn start(mut file: std::fs::File) -> std::io::Result<Self> {
        use std::io::Write as _;

        let slot = Arc::new(Slot {
            state: Mutex::new(Handoff::default()),
            signal: Condvar::new(),
        });
        let theirs = Arc::clone(&slot);
        let writer = std::thread::Builder::new()
            .name(String::from("control-writer"))
            .spawn(move || {
                loop {
                    let Ok(held) = theirs.state.lock() else {
                        return;
                    };
                    let Ok(mut held) = theirs
                        .signal
                        .wait_while(held, |state| !state.asked && !state.stop)
                    else {
                        return;
                    };
                    if held.stop {
                        return;
                    }
                    held.asked = false;
                    drop(held);
                    let outcome = file
                        .write_all(&[0xA5; CONTROL_BYTES])
                        .and_then(|()| file.sync_all());
                    let Ok(mut held) = theirs.state.lock() else {
                        return;
                    };
                    held.answered = Some(outcome);
                    drop(held);
                    theirs.signal.notify_all();
                }
            })?;
        Ok(Self {
            slot,
            writer: Some(writer),
        })
    }

    /// One durable write, in microseconds from the request to the answer.
    fn timed(&self) -> Result<u128, Box<dyn Error>> {
        let start = Instant::now();
        let mut held = self.slot.state.lock().map_err(poisoned)?;
        held.asked = true;
        self.slot.signal.notify_all();
        let mut held = self
            .slot
            .signal
            .wait_while(held, |state| state.answered.is_none())
            .map_err(poisoned)?;
        let outcome = held
            .answered
            .take()
            .ok_or("the control writer gave no answer")?;
        drop(held);
        outcome?;
        Ok(start.elapsed().as_micros())
    }
}

impl Drop for HoppedWrite {
    fn drop(&mut self) {
        if let Ok(mut held) = self.slot.state.lock() {
            held.stop = true;
        }
        self.slot.signal.notify_all();
        if let Some(writer) = self.writer.take() {
            drop(writer.join());
        }
    }
}

/// A durable write that took this many microseconds or more is a stall: a
/// quarter of a second blocked on the device, which a healthy host never costs.
pub(crate) const STALL_US: u128 = 250_000;

/// How many times the control's median cost the path's median may be.
///
/// The path is the control's two hand-offs and write behind a fence and a frame:
/// 1.02 to 1.04 times the control at the median on an idle host (84us against
/// 81us), and 1.04 to 1.07 times beside sixty loops forking and renaming files
/// as fast as the kernel lets them, where the path's p99 was 0.1 to 1.7 times the
/// control's. So eight is the margin of a defect (an extra flush per append reads
/// as a multiple of the one the control makes) and not of a busy host.
pub(crate) const TYPICAL_RATIO: u128 = 8;

/// How many times as often as the bare writes beside them (counting one more
/// than they took) the appends may stall before the stalls are called the path's.
///
/// A stall hits the one write in flight, and the two alternate and wait to be
/// scheduled the same number of times ([`HoppedWrite`]), so a stall that belongs
/// to the host lands on each about equally: the odds that the bare writes take
/// none of eleven stalls the host dealt both are one in 2,048. On a host
/// whose bare writes never stall the rule reads exactly as the 250 ms p99 bound
/// it replaces.
pub(crate) const STALL_RATIO: usize = 8;

/// The median of `sample`, by nearest rank.
fn median(sample: &[u128]) -> Result<u128, String> {
    let mut sorted = sample.to_vec();
    sorted.sort_unstable();
    percentile(&sorted, 500).ok_or_else(|| String::from("a sweep sampled nothing at all"))
}

/// How many samples of `sample` are [`STALL_US`] or more.
pub(crate) fn stalls(sample: &[u128]) -> usize {
    sample.iter().filter(|us| **us >= STALL_US).count()
}

/// Whether a sweep of acknowledged appends is pathological, judged against the
/// bare durable writes timed beside them in the same instants (microseconds).
///
/// A latency bound written in absolute microseconds measures the host as much as
/// the path: main CI run 38929540150 started thirteen jobs in one second and this
/// path read p50 2.6 ms and p99 453 ms with nothing in it changed. So the path is
/// judged against the host's own durable write, on two counts:
///
/// - **typical cost:** the median append is at most [`TYPICAL_RATIO`] times the
///   median bare write, which a host slowed throughout slows both of;
/// - **stalls:** more than 1% of the appends stalling is the old absolute bound
///   (the nearest-rank p99 is a stall exactly when more than 1% of the samples
///   are), and it is the path's only when it stalls [`STALL_RATIO`] times as often
///   as the bare writes did, so a host that stalls on its own is not charged to it.
///
/// # Errors
///
/// The reason the sweep is pathological, or that a sample was empty.
pub(crate) fn append_verdict(path: &[u128], bare: &[u128]) -> Result<(), String> {
    let (path_p50, bare_p50) = (median(path)?, median(bare)?);
    let (path_stalls, bare_stalls) = (stalls(path), stalls(bare));
    let slow_throughout = path_p50 > bare_p50.max(1).saturating_mul(TYPICAL_RATIO);
    let stalls_alone = path_stalls > path.len().div_euclid(100)
        && path_stalls >= STALL_RATIO.saturating_mul(bare_stalls.saturating_add(1));
    match (slow_throughout, stalls_alone) {
        (true, _) => Err(format!(
            "the median append took {path_p50}us, more than {TYPICAL_RATIO}x the {bare_p50}us of \
             the bare durable write beside it"
        )),
        (false, true) => Err(format!(
            "{path_stalls} of {} appends took {STALL_US}us or more, so the p99 is a stall, and the \
             bare durable writes beside them stalled {bare_stalls} times: the stalls are the \
             path's, not the host's",
            path.len()
        )),
        (false, false) => Ok(()),
    }
}

/// One acknowledged append's latency tails on the real path, against the host.
///
/// Every sample is a full `compare_and_append`: fence, frame, write and
/// `sync_all` on the storage owner, then the acknowledgment. Straight after each
/// one the test times a bare `write` and `sync_all` on a sibling file behind the
/// same two hand-offs ([`HoppedWrite`]), so the host is measured in the same
/// instants, and with the same waits to be scheduled, as the path.
///
/// The claim is that the path is not pathological on this host, which
/// [`append_verdict`] decides against the bare writes: a host shared with
/// thirteen CI jobs that all start in the same second stalls both alike (run
/// 38929540150 read this path at p99 453 ms with nothing in it changed), and a
/// bound in absolute microseconds there measures the neighbours. The tails of
/// both are in the report.
#[test]
fn append_latency_tails_are_bounded() -> TestResult {
    let dir = scratch("latency");
    std::fs::create_dir_all(&dir)?;
    let _guard = TempGuard(dir.clone());
    let path = dir.join("journal.log");
    let mut journal = FileJournal::open(&path)?;
    let control = HoppedWrite::start(
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("control.log"))?,
    )?;

    let mut samples: Vec<u128> = Vec::with_capacity(SAMPLES);
    let mut bare: Vec<u128> = Vec::with_capacity(SAMPLES);
    for index in 0..SAMPLES {
        let attempt = u64::try_from(index)?.saturating_add(1);
        let event = EffectEvent::IntentAdmitted { key: key(attempt)? };
        let start = Instant::now();
        let ack = journal.compare_and_append(journal.tail(), &event)?;
        samples.push(start.elapsed().as_micros());
        assert_eq!(
            ack.promise(),
            DurabilityPromise::ProcessCrash,
            "every sampled append must earn the durable promise"
        );
        bare.push(control.timed()?);
    }
    let verdict = append_verdict(&samples, &bare);
    samples.sort_unstable();
    bare.sort_unstable();
    let p50 = percentile(&samples, 500).ok_or("the latency sweep sampled nothing at all")?;
    let p95 = percentile(&samples, 950).ok_or("the latency sweep sampled nothing at all")?;
    let p99 = percentile(&samples, 990).ok_or("the latency sweep sampled nothing at all")?;
    let bare_p50 = percentile(&bare, 500).ok_or("the control sampled nothing at all")?;
    let bare_p95 = percentile(&bare, 950).ok_or("the control sampled nothing at all")?;
    let bare_p99 = percentile(&bare, 990).ok_or("the control sampled nothing at all")?;
    let max = match samples.last() {
        Some(worst) => worst.to_string(),
        // An empty sample has no slowest value, and the percentile fields beside
        // it say `unmeasured`; a zero here would claim a run that finished
        // inside the clock.
        None => String::from("unmeasured"),
    };
    record_measurement(&format!(
        "append p50_us={p50} p95_us={p95} p99_us={p99} max_us={max} samples={SAMPLES} \
         bare_p50_us={bare_p50} bare_p95_us={bare_p95} bare_p99_us={bare_p99}"
    ))?;
    assert_eq!(journal.events().count(), SAMPLES, "every append committed");
    verdict.map_err(|reason| {
        format!(
            "append latency is pathological on this host: {reason} (append p50={p50} p95={p95} \
             p99={p99} max={max}; bare p50={bare_p50} p95={bare_p95} p99={bare_p99})"
        )
    })?;
    Ok(())
}

/// Concurrent tenant appends at 100, 1,000 and 10,000 stay isolated and lose
/// and duplicate nothing.
///
/// Each tenant owns its own file and its own storage-owner thread, so the
/// appends genuinely overlap. A tier the host cannot reach is clamped to the
/// level it can, and the requested, reached and ceiling levels go into the
/// report together — a scale claim that quietly shrank to fit is the one
/// failure this test has to rule out.
#[test]
fn concurrent_tenant_appends_scale_with_isolation() -> TestResult {
    let ceiling = host_tenant_ceiling();
    record_measurement(&format!(
        "ceiling tenants={} measured_threads={} fd_limit={} reserve_threads={} threads_per_tenant={THREADS_PER_TENANT}",
        ceiling.tenants,
        display_limit(ceiling.measured_threads),
        display_limit(ceiling.fd_limit),
        THREAD_RESERVE,
    ))?;

    for requested in TIERS {
        let report = run_tier(requested, ceiling.tenants)?;
        report.record()?;
    }
    Ok(())
}
