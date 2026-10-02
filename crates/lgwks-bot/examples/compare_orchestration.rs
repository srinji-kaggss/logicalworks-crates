//! One workload, three ways to orchestrate it in Rust, measured the same way.
//!
//! ```text
//! cargo run --release -p lgwks_bot --example compare_orchestration -- <way> <scenario>
//! ```
//!
//! `<way>` is `script` (`script!`), `join_all` (`join_all_bounded` with the
//! retry and deadline written by hand) or `joinset` (a `JoinSet` gated by a
//! `Semaphore`, the pattern most tokio code reaches for). `<scenario>` is one
//! of `throughput`, `failfast`, `cancel`, `storm` and `deadline`, specified in
//! `bench/orchestration/README.md` and shared with the Python, Go and Node
//! versions beside it. Every way runs the same site model below; only the
//! orchestration differs. One JSON line is printed; the runner adds peak RSS.
//!
//! The hand-written ways are written as a careful author would write them:
//! they bound the fan-out, retry only retryable failures, apply the deadline
//! per attempt, stop on the first failure and honour cancellation. They build
//! their idempotency key as `tenant/item` by hand.

use std::collections::HashSet;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use lgwks_bot::rt::sync::{CancellationToken, Semaphore};
use lgwks_bot::rt::task::{JoinSet, join_all_bounded};
use lgwks_bot::rt::time::{sleep, timeout};
use lgwks_bot::script::{FlowError, Scope, Tenant};

/// Which workload runs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scenario {
    /// Two tenants x 10,000 one-millisecond items; one in 97 fails once.
    Throughput,
    /// 2,000 items of 1-20 ms; item 500 fails permanently.
    FailFast,
    /// 2,000 items of 50 ms; the caller cancels after 10 ms.
    Cancel,
    /// 1,000 items that always fail transiently, 100 at once, 5 attempts each.
    Storm,
    /// One item of 2 s under a 100 ms deadline.
    Deadline,
}

/// The numbers a scenario fixes.
struct Spec {
    /// Which workload.
    scenario: Scenario,
    /// Tenants run concurrently.
    tenants: u32,
    /// Items per tenant.
    items: u32,
    /// Items in flight per tenant.
    bound: usize,
    /// Attempts per item.
    attempts: u32,
    /// Wait between attempts.
    wait: Duration,
    /// Deadline per attempt.
    deadline: Duration,
    /// When the caller cancels, if it does.
    cancel_after: Option<Duration>,
}

impl Spec {
    /// The specification for `scenario` on this machine.
    fn of(scenario: Scenario) -> Self {
        let machine = std::thread::available_parallelism()
            .map_or(1, std::num::NonZeroUsize::get)
            .saturating_mul(64);
        let (tenants, items, bound, attempts) = match scenario {
            Scenario::Throughput => (2, 10_000, machine, 3),
            Scenario::FailFast | Scenario::Cancel => (1, 2_000, machine, 1),
            Scenario::Storm => (1, 1_000, 100, 5),
            Scenario::Deadline => (1, 1, 1, 1),
        };
        Self {
            scenario,
            tenants,
            items,
            bound,
            attempts,
            wait: if scenario == Scenario::Throughput {
                Duration::from_millis(5)
            } else {
                Duration::ZERO
            },
            deadline: if scenario == Scenario::Deadline {
                Duration::from_millis(100)
            } else {
                Duration::from_secs(1)
            },
            cancel_after: (scenario == Scenario::Cancel).then_some(Duration::from_millis(10)),
        }
    }
}

/// What every way reports, counted the same way.
#[derive(Default)]
struct Probe {
    /// Bodies started and not yet finished or dropped.
    live: AtomicU64,
    /// Most bodies live at once.
    peak: AtomicU64,
    /// Bodies that ran to their end.
    finished: AtomicU64,
    /// Calls into the site, first attempts and retries.
    attempts: AtomicU64,
    /// Keys the site served twice.
    duplicates: AtomicU64,
    /// Per-body wall time, microseconds.
    latencies: Mutex<Vec<u64>>,
    /// Keys served, and items whose first attempt failed.
    served: Mutex<(HashSet<String>, HashSet<String>)>,
}

/// A body's presence; dropping it unfinished leaves `finished` alone.
struct Live {
    /// Where to report.
    probe: Arc<Probe>,
    /// When the body started.
    started: Instant,
}

impl Live {
    /// Count a body in.
    fn enter(probe: &Arc<Probe>) -> Self {
        let live = probe.live.fetch_add(1, Ordering::Relaxed).saturating_add(1);
        probe.peak.fetch_max(live, Ordering::Relaxed);
        Self {
            probe: Arc::clone(probe),
            started: Instant::now(),
        }
    }

    /// The body reached its end.
    fn finish(self) {
        self.probe.finished.fetch_add(1, Ordering::Relaxed);
        let micros = u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX);
        self.probe
            .latencies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(micros);
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.probe.live.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The shared site: one call of one attempt for one item.
struct Ctx {
    /// The numbers.
    spec: Spec,
    /// The counters.
    probe: Arc<Probe>,
}

impl Ctx {
    /// One attempt at `item` under idempotency key `key`.
    async fn attempt(&self, key: String, item: u32) -> Result<u64, FlowError> {
        lgwks_std::trace::warn!(
            operation = "attempt",
            "operation refused its request; the typed error carries the facts"
        );
        self.probe.attempts.fetch_add(1, Ordering::Relaxed);
        let pause = match self.spec.scenario {
            Scenario::Throughput | Scenario::Storm => 1,
            Scenario::FailFast => u64::from(item)
                .wrapping_mul(7_919)
                .checked_rem(20)
                .unwrap_or(0)
                .saturating_add(1),
            Scenario::Cancel => 50,
            Scenario::Deadline => 2_000,
        };
        sleep(Duration::from_millis(pause)).await;
        let mut served = self
            .probe
            .served
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match self.spec.scenario {
            Scenario::Storm => return Err(FlowError::transient("the upstream is down")),
            Scenario::FailFast if item == 500 => {
                return Err(FlowError::failed("malformed record"));
            }
            Scenario::Throughput if item.is_multiple_of(97) && served.1.insert(key.clone()) => {
                return Err(FlowError::transient("the site was busy"));
            }
            _ => {}
        }
        if !served.0.insert(key) {
            self.probe.duplicates.fetch_add(1, Ordering::Relaxed);
        }
        Ok(u64::from(item))
    }
}

// BEGIN script
lgwks_bot::script! {
    /// Every item, bounded, retried and deadlined, keyed by tenant and path.
    flow run_items(ctx: &Ctx, items: Vec<u32>) -> u64:
        let values = each item in items, at most (ctx.spec.bound) at once:
            let live = Live::enter(&ctx.probe)
            let value = retry up to (ctx.spec.attempts) times, waiting (ctx.spec.wait):
                within (ctx.spec.deadline):
                    ctx.attempt(scope.key().to_hex(), item).await?
            live.finish()
            value
        give back values.iter().sum()
}
// END script

// BEGIN hand
/// Retry `item` by hand: per-attempt deadline, retryable failures only.
async fn by_hand(ctx: Arc<Ctx>, tenant: Arc<str>, item: u32) -> Result<u64, FlowError> {
    lgwks_std::trace::warn!(
        operation = "by_hand",
        "operation refused its request; the typed error carries the facts"
    );
    let live = Live::enter(&ctx.probe);
    let key = format!("{tenant}/{item}");
    let mut attempt: u32 = 1;
    let value = loop {
        let error = match timeout(ctx.spec.deadline, ctx.attempt(key.clone(), item)).await {
            Ok(Ok(value)) => break value,
            Ok(Err(error)) => error,
            Err(_elapsed) => FlowError::transient("timed out"),
        };
        if !error.is_retryable() || attempt >= ctx.spec.attempts {
            return Err(error);
        }
        attempt = attempt.saturating_add(1);
        sleep(ctx.spec.wait).await;
    };
    live.finish();
    Ok(value)
}
// END hand

// BEGIN join_all
/// `join_all_bounded` over hand-retried items.
async fn by_join_all(ctx: Arc<Ctx>, tenant: Arc<str>, items: Vec<u32>) -> Result<u64, FlowError> {
    let futures = items
        .into_iter()
        .map(|item| by_hand(Arc::clone(&ctx), Arc::clone(&tenant), item));
    let mut total: u64 = 0;
    for outcome in join_all_bounded(ctx.spec.bound, futures).await {
        total = total.saturating_add(outcome?);
    }
    Ok(total)
}
// END join_all

// BEGIN joinset
/// A `JoinSet` gated by a `Semaphore`, stopping at the first failure.
async fn by_joinset(ctx: Arc<Ctx>, tenant: Arc<str>, items: Vec<u32>) -> Result<u64, FlowError> {
    let permits = Arc::new(Semaphore::new(ctx.spec.bound));
    let mut set = JoinSet::new();
    let mut total: u64 = 0;
    let settle = |joined: Result<Result<u64, FlowError>, _>| match joined {
        Ok(outcome) => outcome,
        Err(error) => Err(FlowError::failed(format!("task failed: {error}"))),
    };
    for item in items {
        let permit = Arc::clone(&permits)
            .acquire_owned()
            .await
            .map_err(|_closed| FlowError::failed("semaphore closed"))?;
        let (ctx, tenant) = (Arc::clone(&ctx), Arc::clone(&tenant));
        set.spawn(async move {
            let _permit = permit;
            by_hand(ctx, tenant, item).await
        });
        while let Some(joined) = set.try_join_next() {
            total = total.saturating_add(settle(joined)?);
        }
    }
    while let Some(joined) = set.join_next().await {
        total = total.saturating_add(settle(joined)?);
    }
    Ok(total)
}
// END joinset

/// Run one tenant's items the chosen way, stopping when `stop` fires.
///
/// Owns its inputs so it can be spawned: every way runs each tenant as its own
/// task on the multi-threaded runtime, as a server would.
async fn tenant_run(
    way: Arc<str>,
    ctx: Arc<Ctx>,
    tenant: Arc<str>,
    stop: CancellationToken,
) -> Result<u64, FlowError> {
    let items: Vec<u32> = (0..ctx.spec.items).collect();
    let run = async {
        match &*way {
            "script" => {
                let scope = Scope::with_token(Tenant::new(&tenant)?, stop.child_token());
                run_items(&scope, &ctx, items).await
            }
            "join_all" => by_join_all(Arc::clone(&ctx), Arc::clone(&tenant), items).await,
            _ => by_joinset(Arc::clone(&ctx), Arc::clone(&tenant), items).await,
        }
    };
    stop.run_until_cancelled(run)
        .await
        .unwrap_or_else(|| Err(FlowError::failed("cancelled")))
}

/// The value at `permille` of sorted `values`, as `milliseconds.micros`.
fn quantile(values: &[u64], permille: usize) -> String {
    let rank = values
        .len()
        .saturating_sub(1)
        .saturating_mul(permille)
        .checked_div(1_000)
        .unwrap_or(0);
    let micros = values.get(rank).copied().unwrap_or(0);
    format!(
        "{}.{:03}",
        micros.checked_div(1_000).unwrap_or(0),
        micros.checked_rem(1_000).unwrap_or(0)
    )
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    lgwks_std::trace::warn!(
        operation = "main",
        "operation refused its request; the typed error carries the facts"
    );
    let args: Vec<String> = std::env::args().collect();
    let way = args.get(1).map_or("script", String::as_str).to_owned();
    let scenario = match args.get(2).map_or("throughput", String::as_str) {
        "throughput" => Scenario::Throughput,
        "failfast" => Scenario::FailFast,
        "cancel" => Scenario::Cancel,
        "storm" => Scenario::Storm,
        "deadline" => Scenario::Deadline,
        other => return Err(format!("unknown scenario {other:?}").into()),
    };
    let ctx = Arc::new(Ctx {
        spec: Spec::of(scenario),
        probe: Arc::new(Probe::default()),
    });
    let runtime = lgwks_bot::Runtime::new()?;
    let stop = CancellationToken::new();
    let started = Instant::now();
    let way: Arc<str> = Arc::from(way.as_str());
    let outcome: Result<u64, FlowError> = runtime.block_on(async {
        let mut tenants = JoinSet::new();
        for tenant in ["acme", "globex"]
            .iter()
            .take(usize::try_from(ctx.spec.tenants).unwrap_or(1))
        {
            tenants.spawn(tenant_run(
                Arc::clone(&way),
                Arc::clone(&ctx),
                Arc::from(*tenant),
                stop.clone(),
            ));
        }
        if let Some(after) = ctx.spec.cancel_after {
            sleep(after).await;
            stop.cancel();
        }
        let mut total: u64 = 0;
        while let Some(joined) = tenants.join_next().await {
            let outcome =
                joined.map_err(|error| FlowError::failed(format!("tenant task: {error}")))?;
            total = total.saturating_add(outcome?);
        }
        Ok(total)
    });
    let elapsed = started.elapsed();
    let probe = &ctx.probe;
    let live_at_return = probe.live.load(Ordering::Relaxed);
    let finished_at_return = probe.finished.load(Ordering::Relaxed);
    runtime.block_on(sleep(Duration::from_millis(200)));
    let live_after_grace = probe.live.load(Ordering::Relaxed);
    let finished_after_return = probe
        .finished
        .load(Ordering::Relaxed)
        .saturating_sub(finished_at_return);
    let mut latencies = probe
        .latencies
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    latencies.sort_unstable();
    let attempts = probe.attempts.load(Ordering::Relaxed);
    let (error, total) = match outcome {
        Ok(total) => (String::new(), total),
        Err(error) => (error.to_string(), 0),
    };
    let items = u64::from(ctx.spec.items).saturating_mul(u64::from(ctx.spec.tenants));
    let elapsed_micros = u64::try_from(elapsed.as_micros())
        .unwrap_or(u64::MAX)
        .max(1);
    let per_s = if error.is_empty() {
        items
            .saturating_mul(1_000_000)
            .checked_div(elapsed_micros)
            .unwrap_or(0)
    } else {
        0
    };
    writeln!(
        std::io::stdout().lock(),
        "{{\"way\":\"rust-{way}\",\"scenario\":\"{}\",\"ok\":{},\"items\":{items},\"total\":{total},\
         \"wall_ms\":{},\"per_s\":{per_s},\"p50_ms\":{},\"p95_ms\":{},\"p99_ms\":{},\
         \"bound\":{},\"peak_in_flight\":{},\"attempts\":{attempts},\"duplicates\":{},\
         \"live_at_return\":{live_at_return},\"live_after_grace\":{live_after_grace},\
         \"finished_after_return\":{finished_after_return},\"error\":{:?}}}",
        args.get(2).map_or("throughput", String::as_str),
        error.is_empty(),
        quantile(&[elapsed_micros], 0),
        quantile(&latencies, 500),
        quantile(&latencies, 950),
        quantile(&latencies, 990),
        ctx.spec.bound,
        probe.peak.load(Ordering::Relaxed),
        probe.duplicates.load(Ordering::Relaxed),
        error,
    )?;
    Ok(())
}
