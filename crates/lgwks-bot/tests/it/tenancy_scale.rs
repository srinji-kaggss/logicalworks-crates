//! Measured multi-tenant scale: 5,000 tenants provisioned at each declared
//! in-flight tier (#268).
//!
//! The test drives the shipped `Supervisor` through its public surface and
//! reports admission waits per tier with peak RSS. The numbers in
//! `docs/production-readiness.md` §4.8 come from these lines, quoted from a run
//! rather than asserted from a model.
//!
//! The adversarial neighbour's admission-vs-scheduling split (#351) is no
//! longer a test here: a wall-clock ratio on a shared host read the host, so
//! the claim is gated exactly in `sim_tenancy_flood` (virtual time through the
//! shipped round) and the wall-clock split is reported by
//! `bench/async -- --flood-split` (#375).
//!
//! Admission takes `&mut Supervisor`, so one caller submits for every tenant.
//! Bodies park on a gate until the test completes them — controlled
//! completions, never slept-for load — and every submission is awaited inside a
//! wedge timeout, so a parked submission no completion resolves is a named
//! failure rather than a hung suite.

#![cfg(all(feature = "rt", feature = "time", feature = "sync", feature = "script"))]

use std::error::Error;
use std::io::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use lgwks_bot::Runtime;
use lgwks_bot::rt::supervise::Supervisor;
use lgwks_bot::rt::tenancy::TenancyPolicy;
use lgwks_bot::script::Tenant;

use crate::tenancy_harness::{Gate, percentile, submit_parked, wide};

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

/// The slowest sample, or zero for no samples.
fn slowest(samples: &[Duration]) -> Duration {
    let mut max = Duration::ZERO;
    for sample in samples {
        max = max.max(*sample);
    }
    max
}

/// The process's resident set in kilobytes, or `None` where nothing reports
/// it.
///
/// Linux reads the high-water mark directly; the BSDs and macOS sample the
/// current set through `ps`, so the sampler that calls this keeps its own
/// maximum. A machine that cannot answer reports `None` rather than a number
/// it did not measure.
#[cfg(target_os = "linux")]
fn sample_resident_kb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

/// The process's resident set in kilobytes on the BSDs and macOS, read from
/// `ps`, or `None` when it cannot be read.
#[cfg(all(unix, not(target_os = "linux")))]
fn sample_resident_kb() -> Option<u64> {
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

/// No sampler outside Unix: the question is not answered rather than answered
/// with a zero.
#[cfg(not(unix))]
fn sample_resident_kb() -> Option<u64> {
    None
}

/// The peak resident set observed while the tier runs, sampled every
/// [`RSS_QUANTUM`].
struct RssPeak {
    /// Tells the sampler to stop; joined before the peak is read.
    stop: Arc<AtomicBool>,
    /// The sampler's thread, joined exactly once.
    sampler: Option<std::thread::JoinHandle<u64>>,
}

/// How often the sampler reads the resident set.
const RSS_QUANTUM: Duration = Duration::from_millis(5);

impl RssPeak {
    /// Start sampling the resident set.
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let sampler_stop = Arc::clone(&stop);
        // `park_timeout`, not `sleep`: the sampler owns its thread and waits
        // for nothing but the next sample, which is what the sync replacement
        // is for.
        let sampler = std::thread::Builder::new()
            .name(String::from("tenancy-scale-rss"))
            .spawn(move || {
                let mut peak = 0_u64;
                while !sampler_stop.load(Ordering::Relaxed) {
                    if let Some(kib) = sample_resident_kb() {
                        peak = peak.max(kib);
                    }
                    std::thread::park_timeout(RSS_QUANTUM);
                }
                if let Some(kib) = sample_resident_kb() {
                    peak = peak.max(kib);
                }
                peak
            })
            .ok();
        Self { stop, sampler }
    }

    /// Stop sampling and report the peak in kilobytes, or `None` when nothing
    /// reported a number for the whole tier.
    fn stop(mut self) -> Option<u64> {
        self.stop.store(true, Ordering::Relaxed);
        match self.sampler.take() {
            Some(sampler) => match sampler.join() {
                Ok(peak) if peak > 0 => Some(peak),
                Ok(_) | Err(_) => None,
            },
            None => None,
        }
    }
}

/// Render an RSS reading the way the readiness section quotes it.
fn render_rss(peak_kb: Option<u64>) -> String {
    match peak_kb {
        Some(kib) => format!("{kib} KiB"),
        None => String::from("unavailable on this platform"),
    }
}

/// Five thousand tenants each submit work at every declared in-flight tier, and
/// every admission resolves inside the progress bound.
///
/// The tier is the pool: at tiers below the fleet's size most submissions park
/// in their own tenant's queue until controlled completions free permits, and
/// at tiers above it every submission admits at once. Bodies park on a gate —
/// the in-flight shape is created, not raced — and the submitter completes one
/// held body per parked submission, so the fleet drains only through the
/// round's grants.
#[test]
fn every_tenant_is_provisioned_at_each_in_flight_tier() -> TestResult {
    /// The in-flight tiers: the issue's 100/1,000/10,000 and 100,000.
    const TIERS: [usize; 4] = [100, 1_000, 10_000, 100_000];
    /// How many tenants submit work at once.
    const FLEET: usize = 5_000;
    /// How long one admission may take before the tier fails.
    ///
    /// A ceiling on a failure, not a budget the work must finish inside: the
    /// measured waits are milliseconds, and a wait past this bound means the
    /// round stopped serving the tenant, which is the defect this bound
    /// catches.
    const PROGRESS_BOUND: Duration = Duration::from_secs(5);
    /// How many waiting admissions one tenant may park: the most tasks any one
    /// tenant submits at any tier.
    const QUEUE_PER_TENANT: usize = 32;

    let runtime = Runtime::new()?;
    for tier in TIERS {
        // Enough tasks per tenant to reach the tier's in-flight count across
        // the fleet: at tiers below the fleet one task each already contends,
        // and at 100,000 twenty tasks each hold the pool open.
        let per_tenant = tier.div_ceil(FLEET);
        let mut named: Vec<Tenant> = Vec::with_capacity(FLEET);
        for index in 0..FLEET {
            named.push(Tenant::new(&format!("fleet-{index}"))?);
        }
        let gate = Gate::new();
        let peak = RssPeak::start();
        let measured = runtime.block_on(async {
            let mut supervisor =
                Supervisor::with_tenancy(tier, TenancyPolicy::new(tier, QUEUE_PER_TENANT));
            let mut waits: Vec<Duration> = Vec::new();
            let mut admitted = 0_usize;
            let mut refused = 0_usize;
            for tenant in &named {
                for _ in 0..per_tenant {
                    let started = Instant::now();
                    match submit_parked(&mut supervisor, tenant, &gate).await? {
                        Ok(()) => {
                            waits.push(started.elapsed());
                            admitted = admitted.saturating_add(1);
                        }
                        Err(_) => {
                            refused = refused.saturating_add(1);
                        }
                    }
                }
            }
            gate.release();
            let report = supervisor.shutdown().await;
            Ok::<_, String>((admitted, refused, waits, report))
        })?;
        let (admitted, refused, waits, report) = measured;
        let peak_kb = peak.stop();
        let submitted = FLEET.saturating_mul(per_tenant);
        assert_eq!(
            admitted.saturating_add(refused),
            submitted,
            "at tier {tier} every submission is either admitted or refused by name"
        );
        assert_eq!(
            refused, 0,
            "at tier {tier} no tenant is past its {QUEUE_PER_TENANT}-deep queue"
        );
        assert_eq!(
            admitted, submitted,
            "at tier {tier} every one of {FLEET} tenants' {per_tenant} tasks was admitted"
        );
        assert_eq!(
            report.stats().completed,
            wide(admitted)?,
            "at tier {tier} every admitted task reached a terminal outcome"
        );
        let max = slowest(&waits);
        assert!(
            max < PROGRESS_BOUND,
            "at tier {tier} the slowest of {submitted} admissions took {max:?}, past the \
             {PROGRESS_BOUND:?} progress bound"
        );
        // The measured line the readiness section quotes.
        let mut line = std::io::stdout().lock();
        let _written = writeln!(
            line,
            "tier {tier}: tenants {FLEET} tasks-per-tenant {per_tenant} admitted {admitted} \
             refused {refused} admission-wait p50 {:?} p99 {:?} max {max:?} peak-rss {}",
            percentile(&waits, 50),
            percentile(&waits, 99),
            render_rss(peak_kb),
        );
    }
    Ok(())
}
