//! Measured multi-tenant scale: 5,000 tenants provisioned at each declared
//! in-flight tier (#268), and the admission-vs-scheduling split of the
//! adversarial neighbour's wait (#351).
//!
//! Both tests drive the shipped `Supervisor` through its public surface and
//! report the numbers they measured: admission waits per tier with peak RSS,
//! and the neighbour's wall time beside the round's own decision time. The
//! numbers in `docs/production-readiness.md` §4.8 come from these lines, quoted
//! from a run rather than asserted from a model.
//!
//! | Test | What it measures |
//! |---|---|
//! | `every_tenant_is_provisioned_at_each_in_flight_tier` | 5,000 tenants submit at tiers 100/1,000/10,000/100,000; every admission resolves inside the progress bound and the tier's peak RSS is reported |
//! | `the_floods_cost_is_scheduling_not_admission` | the fail-at-once flood's extra admission-side time against the neighbour's wall; the bound stays 10% and the split names the scheduling share |
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

use crate::tenancy_harness::{Gate, admit, neighbour_work, percentile, submit_parked, wide};

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

/// The flood's admission-side cost, split from the neighbour's wall (#351).
///
/// Both arms submit the neighbour's fixed workload; the attacked arm puts four
/// fail-at-once attacker submissions before each of the neighbour's. The
/// supervisor's [`Supervisor::admission_cost`] counters report the round's own
/// lock-hold arrival time over each arm — the same lock the submitter holds
/// inside its own `spawn_for` — and the neighbour's summed `spawn_for` time is
/// the wall the issue's bound judges. Releases land on the completing tasks
/// and are counted apart, so the split compares one thread's quantities: the
/// submitter's arrival-decision time against its wall. Everything else in the
/// wall is the scheduling share — the attacker's polls and completions running
/// on the runtime beside the neighbour's submissions — which INV-BOT-151 does
/// not claim to isolate.
///
/// The bound is unchanged: the flood must not inflate the arrival decision
/// past the same 10% envelope the neighbour's throughput is judged against.
/// It is judged on the **median of the per-round ratios**, not on one ratio of
/// pooled means. Each round runs both arms back to back, alternating which goes
/// first, so a round's ratio compares two arms that ran under the same host
/// load; a pooled mean lets one arm absorb a burst the other never saw. On a
/// host shared with twelve other CI lanes the pooled ratio read 1103–1304‰
/// while the attacked arm's wall was 321–865‰ of the baseline's — the attacked
/// arm finishing faster is a host burst landing on the baseline, not an
/// admission cost. A flood that does inflate the round's decision inflates it
/// in every round, and the median of every round reads it.
#[test]
fn the_floods_cost_is_scheduling_not_admission() -> TestResult {
    /// The pool both arms run under: the neighbour's share is unreachable by
    /// the attacker, so the wait under test is service rate, not starvation.
    const POOL: usize = 64;
    /// How many tasks the neighbour runs per arm.
    const TASKS: usize = 2_000;
    /// How many fail-at-once attacker submissions land before each neighbour
    /// submission in the attacked arm.
    const FLOOD: usize = 4;
    /// How many paired rounds run. Odd, so the median is a round that ran.
    const ROUNDS: usize = 101;

    let attacker = Tenant::new("attacker")?;
    let neighbour = Tenant::new("neighbour")?;
    let runtime = Runtime::new()?;

    let mut baseline = ArmTotals::default();
    let mut attacked = ArmTotals::default();
    let mut round_ratios = Vec::with_capacity(ROUNDS);
    for round in 0..ROUNDS {
        let arm =
            |flood| runtime.block_on(neighbour_arm(&neighbour, &attacker, POOL, TASKS, flood));
        // Alternate the order, so whichever position a warm cache or a host
        // burst favours lands on both arms equally often.
        let (quiet, flooded) = if round % 2 == 0 {
            let quiet = arm(0)?;
            (quiet, arm(FLOOD)?)
        } else {
            let flooded = arm(FLOOD)?;
            (arm(0)?, flooded)
        };
        round_ratios.push(per_mille(
            mean_nanos(flooded.arrival_nanos, flooded.arrivals)?,
            u128::from(mean_nanos(quiet.arrival_nanos, quiet.arrivals)?),
        )?);
        baseline.add(&quiet);
        attacked.add(&flooded);
    }
    round_ratios.sort_unstable();
    let Some(decision_per_mille) = round_ratios.get(ROUNDS.div_euclid(2)).copied() else {
        return Err("the paired rounds produced no median ratio".into());
    };
    // Per mille, in integers: this workspace forbids the float casts a ratio
    // of durations would take. Means, not totals: the attacked arm takes five
    // times the decisions, on five times the threads, so only the per-decision
    // cost compares. The pooled ratio is reported beside the median so a run
    // shows how far host noise moved it.
    let baseline_mean = mean_nanos(baseline.arrival_nanos, baseline.arrivals)?;
    let attacked_mean = mean_nanos(attacked.arrival_nanos, attacked.arrivals)?;
    let pooled_per_mille = per_mille(attacked_mean, u128::from(baseline_mean))?;
    let wall_per_mille = match attacked
        .wall
        .as_nanos()
        .saturating_mul(1_000)
        .checked_div(baseline.wall.as_nanos().max(1))
    {
        Some(ratio) => ratio,
        None => return Err("the attacked wall has no ratio against the baseline".into()),
    };
    let (lowest, highest) = match (round_ratios.first(), round_ratios.last()) {
        (Some(lowest), Some(highest)) => (*lowest, *highest),
        _ => return Err("the paired rounds produced no ratio range".into()),
    };
    let mut line = std::io::stdout().lock();
    let _written = writeln!(
        line,
        "split over {ROUNDS} paired rounds x {TASKS} neighbour tasks: baseline wall {:?} \
         arrival mean {baseline_mean} ns over {} decisions (submitter share \
         {}\u{2030}); attacked wall {:?} arrival mean {attacked_mean} ns over \
         {} decisions (submitter share {}\u{2030}); wall ratio \
         {wall_per_mille}\u{2030}, decision-mean ratio median {decision_per_mille}\u{2030} \
         (rounds {lowest}..{highest}\u{2030}, pooled {pooled_per_mille}\u{2030})",
        baseline.wall,
        baseline.arrivals,
        submitter_share(
            baseline.arrival_nanos,
            baseline.arrivals,
            baseline.wall,
            TASKS,
            ROUNDS
        )?,
        attacked.wall,
        attacked.arrivals,
        submitter_share(
            attacked.arrival_nanos,
            attacked.arrivals,
            attacked.wall,
            TASKS,
            ROUNDS
        )?,
    );
    assert!(
        decision_per_mille <= 1_100,
        "the flood inflated the median round's arrival decision to {decision_per_mille}\u{2030} \
         of its cost alone, past the 10% envelope: the cost is admission-side and must be fixed \
         in the round, not stated away (rounds {lowest}..{highest}\u{2030}, pooled \
         {pooled_per_mille}\u{2030}, wall ratio {wall_per_mille}\u{2030})"
    );
    Ok(())
}

/// One arm's measurements summed over every round it ran.
#[derive(Default)]
struct ArmTotals {
    /// Time the neighbour spent inside its own `spawn_for` calls.
    wall: Duration,
    /// Arrival decisions taken.
    arrivals: u64,
    /// Their cumulative lock-hold time, in nanoseconds.
    arrival_nanos: u64,
}

impl ArmTotals {
    /// Fold one round's arm into the totals.
    fn add(&mut self, arm: &ArmMeasurement) {
        self.wall = self.wall.saturating_add(arm.wall);
        self.arrivals = self.arrivals.saturating_add(arm.arrivals);
        self.arrival_nanos = self.arrival_nanos.saturating_add(arm.arrival_nanos);
    }
}

/// The submitter's arrival-decision share of one neighbour submission, per
/// mille: the mean arrival decision against the mean `spawn_for` wall.
///
/// Both means are per neighbour submission, so the populations compare: the
/// arrival mean covers every submit the arm made, divided down to one, against
/// the wall of one. The remainder of each submission is the scheduling share —
/// the task spawn, the bounded reap, and the executor's polls beside it.
fn submitter_share(
    arrival_nanos: u64,
    arrivals: u64,
    wall: Duration,
    tasks: usize,
    rounds: usize,
) -> Result<u128, String> {
    let submits = u128::from(
        u64::try_from(tasks.saturating_mul(rounds))
            .map_err(|error| format!("the round count does not fit in u64: {error}"))?,
    );
    let mean_arrival = mean_nanos(arrival_nanos, arrivals)?;
    let wall_per_submit = match wall.as_nanos().checked_div(submits.max(1)) {
        Some(nanos) => nanos,
        None => {
            lgwks_std::trace::warn!(
                tasks,
                rounds,
                "tenancy_scale it: one submission has no wall to divide"
            );
            return Err(String::from("one submission has no wall"));
        }
    };
    per_mille(mean_arrival, wall_per_submit)
}

/// Per mille of `part` against `whole`, in integers.
///
/// The divisor is at least one by construction, so the division always has an
/// answer; a refusal names the share that could not be computed rather than
/// substituting one.
fn per_mille(part: u64, whole: u128) -> Result<u128, String> {
    match u128::from(part)
        .saturating_mul(1_000)
        .checked_div(whole.max(1))
    {
        Some(share) => Ok(share),
        None => {
            lgwks_std::trace::warn!(
                part,
                whole = %whole,
                "tenancy_scale it: a per-mille share has no ratio"
            );
            Err(String::from("a per-mille share has no ratio"))
        }
    }
}

/// The mean arrival-decision cost in nanoseconds: total lock-hold time over the
/// decisions that paid it. A refusal names the arm with no decisions rather
/// than dividing by zero.
fn mean_nanos(nanos: u64, decisions: u64) -> Result<u64, String> {
    if decisions == 0 {
        lgwks_std::trace::warn!("tenancy_scale it: the arm took no arrival decisions to average");
        return Err(String::from("the arm took no arrival decisions to average"));
    }
    match u128::from(nanos).checked_div(u128::from(decisions)) {
        Some(mean) => match u64::try_from(mean) {
            Ok(mean) => Ok(mean),
            Err(_) => Err(String::from("the mean decision cost does not fit in u64")),
        },
        None => Err(String::from("the mean decision cost has no ratio")),
    }
}

/// What one arm of [`the_floods_cost_is_scheduling_not_admission`] measured:
/// the neighbour's summed `spawn_for` time and the round's arrival-decision
/// counters over the arm.
struct ArmMeasurement {
    /// Time the neighbour spent inside its own `spawn_for` calls.
    wall: Duration,
    /// Arrival decisions taken during the arm.
    arrivals: u64,
    /// Their cumulative lock-hold time, in nanoseconds.
    arrival_nanos: u64,
}

/// One arm: the neighbour's fixed workload, with `flood` fail-at-once attacker
/// submissions before each of its own.
///
/// Returns the time the neighbour spent inside its own `spawn_for` calls — the
/// submitter's time on the attacker's work is the harness's, not the
/// neighbour's — and the round's decision counters over the arm.
async fn neighbour_arm(
    neighbour: &Tenant,
    attacker: &Tenant,
    pool: usize,
    tasks: usize,
    flood: usize,
) -> Result<ArmMeasurement, String> {
    let mut supervisor = Supervisor::with_tenancy(pool, TenancyPolicy::new(pool.div_ceil(2), pool));
    let (before_arrivals, before_nanos, _, _) = supervisor.admission_cost();
    let mut wall = Duration::ZERO;
    for _ in 0..tasks {
        for _ in 0..flood {
            admit(&mut supervisor, attacker, |_token| async {}).await?;
        }
        let submitting = Instant::now();
        admit(&mut supervisor, neighbour, |_token| async move {
            neighbour_work().await;
        })
        .await?;
        wall = wall.saturating_add(submitting.elapsed());
    }
    // Let the admitted bodies finish so their releases land in the counters
    // before they are read: a release still in flight would undercount the arm
    // that ran more work.
    let quiet_since = Instant::now();
    while supervisor.tenant_capacity(neighbour).0 > 0 || supervisor.tenant_capacity(attacker).0 > 0
    {
        if quiet_since.elapsed() > Duration::from_secs(10) {
            lgwks_std::trace::warn!(
                "tenancy_scale it: the arm's admitted tasks never finished within 10 s"
            );
            return Err(String::from(
                "the arm's admitted tasks never finished within 10 s",
            ));
        }
        lgwks_bot::rt::time::sleep(Duration::from_millis(1)).await;
    }
    let (after_arrivals, after_nanos, _, _) = supervisor.admission_cost();
    let report = supervisor.shutdown().await;
    let submitted = tasks.saturating_mul(flood.saturating_add(1));
    if report.stats().spawned != wide(submitted)? {
        lgwks_std::trace::warn!(
            spawned = report.stats().spawned,
            submitted,
            "tenancy_scale it: the arm admitted a different task count than submitted"
        );
        return Err(format!(
            "the arm admitted {} tasks, not the {submitted} both tenants submitted",
            report.stats().spawned
        ));
    }
    if report.stats().completed != report.stats().spawned {
        lgwks_std::trace::warn!(
            completed = report.stats().completed,
            spawned = report.stats().spawned,
            "tenancy_scale it: the arm completed fewer than the admitted tasks"
        );
        return Err(format!(
            "the arm completed {} of {} admitted tasks",
            report.stats().completed,
            report.stats().spawned
        ));
    }
    Ok(ArmMeasurement {
        wall,
        arrivals: after_arrivals.saturating_sub(before_arrivals.min(after_arrivals)),
        arrival_nanos: after_nanos.saturating_sub(before_nanos.min(after_nanos)),
    })
}
