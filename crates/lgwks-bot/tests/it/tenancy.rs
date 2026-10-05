//! Black-box acceptance for per-tenant capacity on a real supervisor.
//!
//! The unit tests in `rt::tenancy` pin the scheduler's decisions against a
//! model; this file pins the *property the mechanism exists for*, measured on
//! the shipped `Supervisor` through its public surface: **a noisy tenant cannot
//! starve a quiet one**. Every test drives real async tasks through `spawn_for`
//! against real permits, with no mock and no simulated clock, and reads the
//! per-tenant counters the admission itself charges (INV-BOT-31).
//!
//! Admission takes `&mut Supervisor`, so a single test drives one caller. The
//! noisy tenant fills the pool by submitting bodies that *park* — each admitted
//! body holds its permit until the test opens a gate — so the loud tenant holds
//! its whole ceiling without any timing race. The quantity under test, the quiet
//! tenant's admission wait, is the wall time from its `spawn_for` call to the
//! moment the call returns (the body is placed and holds its permit), which is
//! bounded by the quiet tenant's own share's service rate and not by the loud
//! tenant's backlog.
#![cfg(all(feature = "rt", feature = "time", feature = "sync", feature = "script"))]

use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use lgwks_bot::Runtime;
use lgwks_bot::rt::supervise::{SpawnRefused, Supervisor};
use lgwks_bot::rt::sync::Notify;
use lgwks_bot::rt::tenancy::TenancyPolicy;
use lgwks_bot::rt::time::sleep;
use lgwks_bot::script::Tenant;

/// What a test reports when its precondition did not hold.
type TestResult = Result<(), Box<dyn Error>>;

/// The supervisor's global in-flight bound for the noisy-neighbour sweep.
const POOL: usize = 64;

/// How many tasks the quiet tenant submits.
const QUIET_TASKS: usize = 64;

/// A ceiling on a *failure*, not a budget the admitted work must finish inside.
const BUDGET: Duration = Duration::from_secs(20);

/// How long each quiet body holds its admitted slot, so the quiet tenant's
/// completions are spread over real time and its admission is a distribution
/// rather than a single instant.
const QUIET_HOLD: Duration = Duration::from_millis(2);

/// A gate the loud tenant's bodies park on, opened by the test at a chosen
/// moment. Controlled completion: the bodies are known admitted and parked until
/// `release`, so the loud tenant's occupancy is created rather than raced.
#[derive(Clone)]
struct Gate {
    /// Opens every parked and future body.
    open: Arc<Notify>,
    /// How many bodies have actually parked, for the test's precondition.
    parked: Arc<AtomicUsize>,
}

impl Gate {
    /// A gate nothing has entered yet.
    fn new() -> Self {
        Self {
            open: Arc::new(Notify::new()),
            parked: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// A body that parks here until the gate is opened.
    async fn wait(&self) {
        self.parked.fetch_add(1, Ordering::SeqCst);
        self.open.notified().await;
    }

    /// How many bodies are parked right now.
    fn parked(&self) -> usize {
        self.parked.load(Ordering::SeqCst)
    }

    /// Open the gate for every parked and future body.
    fn release(&self) {
        self.open.notify_waiters();
    }
}

/// Wait until `predicate` holds, or the budget runs out. A bounded poll, so a
/// defect that stops bodies from parking is a failure rather than a hang.
async fn until(budget: Duration, mut predicate: impl FnMut() -> bool) -> Result<(), String> {
    let start = Instant::now();
    while !predicate() {
        if start.elapsed() >= budget {
            return Err(format!("the precondition did not hold within {budget:?}"));
        }
        sleep(Duration::from_millis(1)).await;
    }
    Ok(())
}

/// The wall time after which a quiet admission wait is called a starvation.
///
/// The quiet tenant's own share frees a slot every [`QUIET_HOLD`], so admitting
/// [`QUIET_TASKS`] of them cannot exceed that count times the hold — unless the
/// quiet tenant is being served out of the loud tenant's queue, which is the
/// regression this bound catches. A generous ceiling, because it is a ceiling on
/// a *failure*; the strict fair-share numbers are the unit-tested property.
fn quiet_ceiling() -> Duration {
    QUIET_HOLD.saturating_mul(u32::try_from(QUIET_TASKS).unwrap_or(u32::MAX))
}

/// The `n`-th percentile of a latency sample, nearest-rank. Every sample is a
/// real admission wait, so the percentile of admission waits is itself one of
/// them. `div_ceil`, not `/`, which this workspace forbids.
fn percentile(samples: &[Duration], nth: usize) -> Duration {
    if samples.is_empty() {
        return Duration::ZERO;
    }
    let mut sorted: Vec<Duration> = samples.to_vec();
    sorted.sort_unstable();
    let rank = nth.saturating_mul(sorted.len()).div_ceil(100);
    let index = rank.saturating_sub(1).min(sorted.len().saturating_sub(1));
    sorted.get(index).copied().unwrap_or(Duration::ZERO)
}

/// The noisy-neighbour sweep: the loud tenant saturates its own share and parks,
/// the quiet tenant is admitted inside its own share and completes, and the
/// quiet tenant's admission wait is bounded by its own share's service rate
/// rather than by the loud tenant's backlog.
#[test]
fn a_noisy_tenant_cannot_starve_a_quiet_one() -> TestResult {
    let loud = Tenant::new("loud")?;
    let quiet = Tenant::new("quiet")?;
    // The loud tenant may hold 32 of the 64 permits; the quiet one may hold its
    // own 32. The quiet tenant's share is then unreachable by the loud one, which
    // is the property under test.
    let half = POOL.div_ceil(2);
    let gate = Gate::new();
    let runtime = Runtime::new()?;

    let (quiet_waits, quiet_p50, quiet_p99) = runtime.block_on(async {
        let mut supervisor = Supervisor::with_tenancy(POOL, TenancyPolicy::new(half, 4 * half));

        // The loud tenant submits exactly its ceiling (`half` tasks), each
        // parking on the gate and holding its permit. We stop at the ceiling
        // because a further `spawn_for` would wait on the loud tenant's queue —
        // and admission takes `&mut self`, so a single caller cannot observe
        // that wait and the quiet tenant at the same time. Saturating the loud
        // half of the pool is the precondition the isolation question needs: the
        // quiet tenant must be served out of the *other* half, which only its
        // own per-tenant ceiling guarantees.
        let mut loud_submitted = 0_usize;
        let mut loud_refused: Option<SpawnRefused> = None;
        while loud_submitted < half {
            match supervisor
                .spawn_for(&loud, {
                    let gate_for_task = gate.clone();
                    move |_token| async move { gate_for_task.wait().await }
                })
                .await
            {
                Ok(()) => loud_submitted = loud_submitted.saturating_add(1),
                Err(refusal) => {
                    loud_refused = Some(refusal);
                    break;
                }
            }
        }
        // The loud tenant now holds its whole ceiling parked on the gate: a fact
        // the test created, not a timing hope. This is the precondition the whole
        // measurement rests on.
        until(BUDGET, || gate.parked() >= half).await?;

        // The quiet tenant now submits its own tasks. Its admission wait is the
        // wall time from each `spawn_for` call to the call returning (its body is
        // placed and holds its permit). The quiet bodies hold for QUIET_HOLD, so
        // its share frees a slot every QUIET_HOLD and the wait is bounded by that
        // rate — never by the loud tenant's queue.
        let mut quiet_waits: Vec<Duration> = Vec::new();
        for _ in 0..QUIET_TASKS {
            let started = Instant::now();
            supervisor
                .spawn_for(&quiet, |_token| async move {
                    sleep(QUIET_HOLD).await;
                })
                .await
                .map_err(|refusal| format!("the quiet tenant was refused: {refusal}"))?;
            quiet_waits.push(started.elapsed());
        }

        // Open the gate and shut down: the loud tenant drains, every quiet body
        // finishes, and shutdown reports the outcomes.
        gate.release();
        let report = supervisor.shutdown().await;
        // Both tenants' tasks completed: the loud tenant's (which were queued
        // and ran once admitted) and the quiet tenant's. Nothing was lost or
        // aborted by the admission.
        assert!(
            report.stats().completed
                >= u64::try_from(loud_submitted).unwrap_or(u64::MAX)
                    + u64::try_from(QUIET_TASKS).unwrap_or(u64::MAX),
            "every admitted task, loud and quiet, ran to completion \
             (spawned={} completed={})",
            report.stats().spawned,
            report.stats().completed
        );
        // A refusal of the loud tenant at the queue bound is the loud tenant's
        // own business and names it; record whether one occurred for the report.
        if let Some(refusal) = loud_refused {
            assert!(
                matches!(refusal, SpawnRefused::TenantAtCapacity { .. }),
                "the only refusal the loud flood sees is its own queue's bound, got {refusal:?}"
            );
        }
        Ok::<(Vec<Duration>, Duration, Duration), String>((
            quiet_waits.clone(),
            percentile(&quiet_waits, 50),
            percentile(&quiet_waits, 99),
        ))
    })?;

    // Every quiet task was admitted and measured.
    assert_eq!(
        quiet_waits.len(),
        QUIET_TASKS,
        "every quiet task was admitted and measured"
    );
    assert!(
        quiet_waits.iter().all(|wait| *wait < quiet_ceiling()),
        "no quiet admission waited on the loud tenant's queue"
    );
    assert!(
        quiet_p99 < quiet_ceiling(),
        "the quiet tenant's p99 admission wait ({quiet_p99:?}, p50 {quiet_p50:?}) is \
         bounded by its own share's service rate"
    );
    Ok(())
}

/// A tenant whose bounded queue is full receives a typed refusal naming it and
/// the bound, and that refusal is scoped to that tenant — another tenant is
/// still admitted.
#[test]
fn a_tenant_past_its_queue_bound_is_refused_by_name() -> TestResult {
    let loud = Tenant::new("loud")?;
    let other = Tenant::new("other")?;
    // Two supervisors, one per arm. The first shows another tenant is admitted
    // while the loud tenant is at its ceiling; the second reaches the typed
    // refusal on the public door with a zero-length queue, which is the one
    // refusal a single caller can observe without a concurrent submitter.
    let gate = Gate::new();
    let runtime = Runtime::new()?;

    let (refusal_names_loud, other_admitted) = runtime.block_on(async {
        let mut supervisor = Supervisor::with_tenancy(4, TenancyPolicy::new(1, 1));

        // The loud tenant holds its single permit on the gate.
        supervisor
            .spawn_for(&loud, {
                let gate_for_task = gate.clone();
                move |_token| async move { gate_for_task.wait().await }
            })
            .await
            .map_err(|refusal| format!("the holding loud task was refused: {refusal}"))?;
        until(BUDGET, || gate.parked() >= 1).await?;

        // Park the loud tenant's one queued task. `spawn_for` waits for
        // admission, so this call blocks until a permit frees — which, with the
        // gate closed, it does not. We therefore cannot await it here; instead we
        // observe the *refusal* that arrives on the next arrival once the queue
        // is also full. Filling the queue with one waiting task requires a
        // concurrent submitter, which admission's `&mut self` forbids; the
        // queue-bound refusal is therefore pinned deterministically in the
        // scheduler's unit tests, and here we pin the public refusal's shape and
        // its per-tenant scope.
        //
        // We get a real refusal on the public door by driving `spawn_for` on a
        // tenant whose pool is exhausted with a *short* queue bound: with a
        // queue of 0 the next arrival is refused immediately without waiting.
        let mut zero_queue = Supervisor::with_tenancy(4, TenancyPolicy::new(1, 0));
        let hold = Gate::new();
        zero_queue
            .spawn_for(&loud, {
                let hold_for_task = hold.clone();
                move |_token| async move { hold_for_task.wait().await }
            })
            .await
            .map_err(|refusal| format!("the holding task was refused: {refusal}"))?;
        until(BUDGET, || hold.parked() >= 1).await?;
        // With a queue of 0 and the tenant at its ceiling, the next arrival is
        // refused by name without ever parking.
        let refusal = zero_queue
            .spawn_for(&loud, |_token| async move {})
            .await
            .err();
        hold.release();
        let _drained = zero_queue.shutdown().await;

        // Meanwhile the other tenant, on its own ceiling and queue, is admitted
        // and runs: the loud tenant's refusal never touches it.
        let mut other_admitted = 0_usize;
        for _ in 0..2 {
            if supervisor
                .spawn_for(&other, |_token| async move {})
                .await
                .is_ok()
            {
                other_admitted = other_admitted.saturating_add(1);
            }
        }
        gate.release();
        let _report = supervisor.shutdown().await;

        let refusal_names_loud = matches!(
            refusal,
            Some(SpawnRefused::TenantAtCapacity { ref tenant, limit: 0 })
                if tenant == &loud
        );
        Ok::<(bool, usize), String>((refusal_names_loud, other_admitted))
    })?;

    assert!(
        refusal_names_loud,
        "the refusal names the loud tenant and its zero queue bound"
    );
    assert_eq!(
        other_admitted, 2,
        "another tenant is admitted regardless of the loud tenant's queue"
    );
    Ok(())
}
/// The adversarial arm of the noisy-neighbour sweep: a tenant whose spawns all
/// fail at once does not cost its neighbours throughput.
///
/// The neighbour's throughput is measured twice on the same supervisor shape —
/// once with the adversary absent and once with it flooding — and the two are
/// compared. Both runs do identical work with identical bodies, so the only
/// difference is the adversary's presence, which is exactly the question: "does
/// a failing tenant cost its neighbours anything?"
#[test]
fn an_adversarial_tenants_spawns_do_not_cost_its_neighbour_throughput() -> TestResult {
    let attacker = Tenant::new("attacker")?;
    let neighbour = Tenant::new("neighbour")?;
    let runtime = Runtime::new()?;

    // The neighbour's measured work: each admitted body does a fixed amount of
    // arithmetic and returns. Identical in both runs.
    let work = || async {
        let mut accumulator: u64 = 0;
        for step in 0..2_000_u64 {
            accumulator = accumulator.wrapping_add(step.wrapping_mul(2_654_435_761));
        }
        // Consume the result so the loop cannot be optimised away. A supervised
        // body returns `()`, so the work is observable through a sink rather
        // than a return value.
        CONSUMED.fetch_add(
            usize::try_from(std::hint::black_box(accumulator)).unwrap_or(0),
            Ordering::Relaxed,
        );
    };

    // Baseline: the neighbour alone.
    let baseline = runtime.block_on(async {
        let mut supervisor = Supervisor::with_tenancy(POOL, TenancyPolicy::new(half(), POOL));
        let started = Instant::now();
        for _ in 0..NEIGHBOUR_TASKS {
            supervisor
                .spawn_for(&neighbour, move |_token| work())
                .await
                .map_err(|refusal| format!("the neighbour was refused at baseline: {refusal}"))?;
        }
        let report = supervisor.shutdown().await;
        Ok::<(Duration, _), String>((started.elapsed(), report))
    })?;

    // Attacked: the adversary floods with bodies that fail at once — a task that
    // returns immediately releases its permit in the same tick it took it, which
    // is the cheapest possible churn for the scheduler to absorb.
    let attacked = runtime.block_on(async {
        let mut supervisor = Supervisor::with_tenancy(POOL, TenancyPolicy::new(half(), POOL));
        // The adversary holds its own ceiling on bodies that park, so its
        // occupancy is a constant pressure rather than a burst the neighbour
        // might simply outlast.
        let hold = Gate::new();
        for _ in 0..half() {
            let gate_for_task = hold.clone();
            supervisor
                .spawn_for(&attacker, move |_token| async move {
                    gate_for_task.wait().await;
                })
                .await
                .map_err(|refusal| format!("the adversary was refused: {refusal}"))?;
        }
        until(BUDGET, || hold.parked() >= half()).await?;

        // Now the neighbour does exactly the baseline's work while the adversary
        // holds half the pool.
        let started = Instant::now();
        for _ in 0..NEIGHBOUR_TASKS {
            supervisor
                .spawn_for(&neighbour, move |_token| work())
                .await
                .map_err(|refusal| format!("the neighbour was refused under attack: {refusal}"))?;
        }
        let elapsed = started.elapsed();
        hold.release();
        let report = supervisor.shutdown().await;
        Ok::<(Duration, _), String>((elapsed, report))
    })?;

    // The neighbour lost nothing in either run: every task it submitted reached
    // a terminal outcome. A task still queued when `shutdown` lands is reported
    // cancelled *after* completing, so the accounting is `completed == spawned`,
    // and neither run refused a single neighbour task (the `?` above would have
    // returned early).
    let baseline_stats = baseline.1.stats();
    let attacked_stats = attacked.1.stats();
    assert_eq!(
        baseline_stats.completed, baseline_stats.spawned,
        "every task the neighbour submitted at baseline reached a terminal outcome"
    );
    assert_eq!(
        attacked_stats.completed, attacked_stats.spawned,
        "every task the neighbour submitted under attack reached a terminal outcome"
    );
    assert_eq!(
        baseline_stats.spawned,
        u64::try_from(NEIGHBOUR_TASKS).unwrap_or(u64::MAX),
        "the neighbour submitted every task at baseline"
    );
    // The counters are supervisor-wide, so the attacked run's total includes the
    // adversary's own `half()` tasks: the neighbour's share is the total minus
    // them, and it must be exactly what it submitted.
    let adversary = u64::try_from(half()).unwrap_or(u64::MAX);
    assert_eq!(
        attacked_stats.spawned,
        u64::try_from(NEIGHBOUR_TASKS)
            .unwrap_or(u64::MAX)
            .saturating_add(adversary),
        "the neighbour submitted every task under attack, alongside the adversary's own"
    );
    // Throughput: the attacked run may not be more than 10% slower than the
    // baseline. The neighbour's share of the pool is fixed by the policy, so the
    // adversary's presence cannot change how many of its tasks run at once.
    let budget = Duration::from_micros(
        u64::try_from(baseline.0.as_micros())
            .unwrap_or(u64::MAX)
            .saturating_div(10),
    );
    let ceiling = baseline.0.saturating_add(budget);
    assert!(
        attacked.0 <= ceiling,
        "the neighbour's admission+completion under attack took {:?}, within 10% of \
         the {:?} baseline (ceiling {:?})",
        attacked.0,
        baseline.0,
        ceiling
    );
    Ok(())
}

/// Half the pool: the per-tenant ceiling both tests hand out, and the share one
/// noisy tenant may hold.
fn half() -> usize {
    POOL.div_ceil(2)
}

/// How many tasks the neighbour runs in the adversarial arm.
const NEIGHBOUR_TASKS: usize = 2_000;

/// Where the neighbour's arithmetic is sunk, so the optimiser cannot remove the
/// work the two runs are being compared on. Wrapping add is used throughout so
/// no run panics on overflow.
static CONSUMED: AtomicUsize = AtomicUsize::new(0);
