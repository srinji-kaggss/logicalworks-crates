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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lgwks_bot::journal::{DurabilityPromise, EffectEvent, EffectJournal, FileJournal};

#[path = "support/journal.rs"]
mod shared;

use shared::{TempGuard, key, record_measurement, scratch};

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

/// The nearest-rank percentile of an already-sorted slice, in microseconds.
fn percentile(sorted: &[u128], percent: usize) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = sorted
        .len()
        .saturating_mul(percent)
        .saturating_add(99)
        .checked_div(100)
        .unwrap_or(1)
        .saturating_sub(1);
    sorted[rank.min(sorted.len().saturating_sub(1))]
}

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
        return Err("the batch did not acknowledge every rung".to_owned());
    }
    let held = journal.events().count();
    sink.lock()
        .map_err(|_| "the latency sink was poisoned".to_owned())?
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
        (None, None) => u64::try_from(FALLBACK_TENANTS).unwrap_or(1),
    };
    Ceiling {
        tenants: usize::try_from(tenants).unwrap_or(FALLBACK_TENANTS).max(1),
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
        None
    } else {
        Some(u64::try_from(reached).unwrap_or(u64::MAX))
    }
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

/// A background sampler that tracks the process's peak resident set.
///
/// Sampling rather than a single end-of-run read, because the peak is the
/// concurrency claim and by the time a tier ends the journals are closed. The
/// thread is owned: it is joined on [`RssPeak::stop`], and [`Drop`] stops it
/// too, so an early return cannot leak a spinning sampler.
struct RssPeak {
    /// Set when the sampler should stop.
    stop: Arc<AtomicBool>,
    /// The sampler thread; `None` if it could not be spawned.
    handle: Option<std::thread::JoinHandle<u64>>,
}

impl RssPeak {
    /// Start sampling the process's resident set.
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("rss-peak".to_owned())
            .spawn(move || {
                let mut peak = read_rss_kb().unwrap_or(0);
                while !flag.load(Ordering::Relaxed) {
                    if let Some(kb) = read_rss_kb() {
                        peak = peak.max(kb);
                    }
                    std::thread::park_timeout(Duration::from_millis(RSS_SAMPLE_MS));
                }
                if let Some(kb) = read_rss_kb() {
                    peak = peak.max(kb);
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
        handle.join().ok()
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
    /// The 50th percentile append latency, microseconds.
    p50: u128,
    /// The 95th percentile append latency, microseconds.
    p95: u128,
    /// The 99th percentile append latency, microseconds.
    p99: u128,
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
            self.p50,
            self.p95,
            self.p99,
            display_limit(self.peak_rss_kb),
        ))
    }
}

/// Render an optional measurement for the report, naming what was not measured.
fn display_limit(value: Option<u64>) -> String {
    value.map_or_else(|| "unmeasured".to_owned(), |number| number.to_string())
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
                return Err(format!(
                    "the host refused a tenant thread at {tenant} of {target} with a \
                     measured ceiling of {ceiling}: {error}"
                )
                .into());
            }
        }
    }

    for handle in handles {
        let count = handle
            .join()
            .map_err(|_| std::io::Error::other("a tenant thread panicked"))??;
        assert_eq!(
            count, EVENTS_PER_TENANT,
            "a tenant journal held another tenant's events"
        );
    }

    let mut samples = latencies
        .lock()
        .map_err(|_| std::io::Error::other("the latency sink was poisoned"))?
        .clone();
    samples.sort_unstable();
    let p50 = percentile(&samples, 50);
    let p95 = percentile(&samples, 95);
    let p99 = percentile(&samples, 99);

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
        peak_rss_kb: peak.stop(),
    })
}

/// One acknowledged append's latency tails on the real path.
///
/// Every sample is a full `compare_and_append`: fence, frame, write and
/// `sync_all` on the storage owner, then the acknowledgment. The bounds are
/// generous — the assertion is that the path is not pathological on this host,
/// and the exact tails are the measured value the report carries.
#[test]
fn append_latency_tails_are_bounded() -> TestResult {
    let dir = scratch("latency");
    std::fs::create_dir_all(&dir)?;
    let _guard = TempGuard(dir.clone());
    let path = dir.join("journal.log");
    let mut journal = FileJournal::open(&path)?;

    let mut samples: Vec<u128> = Vec::with_capacity(SAMPLES);
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
    }
    samples.sort_unstable();
    let p50 = percentile(&samples, 50);
    let p95 = percentile(&samples, 95);
    let p99 = percentile(&samples, 99);
    let max = samples.last().copied().unwrap_or(0);
    record_measurement(&format!(
        "append p50_us={p50} p95_us={p95} p99_us={p99} max_us={max} samples={SAMPLES}"
    ))?;
    assert_eq!(journal.events().count(), SAMPLES, "every append committed");
    assert!(
        p99 < 250_000,
        "append p99 {p99}us is pathological on this host (p50={p50} p95={p95} max={max})"
    );
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
