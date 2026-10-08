//! The shared harness behind the tenancy acceptance tests: the admission-wait
//! percentiles, the gate parked bodies hold on, the wedge-bounded submission,
//! and the neighbour workload both arms of a throughput comparison run.
//!
//! One copy, called from every tenancy test module, so the arms being compared
//! cannot drift apart through two spellings of the same submission.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use lgwks_bot::rt::supervise::{SpawnRefused, Supervisor};
use lgwks_bot::rt::sync::{CancellationToken, Notify};
use lgwks_bot::script::Tenant;

/// A count wide enough to compare against a `Stats` counter.
///
/// The counters a test compares against are `u64`. A count that cannot be
/// widened is refused rather than replaced by a ceiling, because a substituted
/// number is one a comparison could pass against.
pub fn wide(count: usize) -> Result<u64, String> {
    u64::try_from(count)
        .map_err(|refusal| format!("{count} does not widen to a counter: {refusal}"))
}

/// The `n`-th percentile of a latency sample, nearest-rank. Every sample is a
/// real admission wait, so the percentile of admission waits is itself one of
/// them. `div_ceil`, not `/`, which this workspace forbids.
pub fn percentile(samples: &[Duration], nth: usize) -> Duration {
    if samples.is_empty() {
        return Duration::ZERO;
    }
    let mut sorted: Vec<Duration> = samples.to_vec();
    sorted.sort_unstable();
    let rank = nth.saturating_mul(sorted.len()).div_ceil(100);
    let index = rank.saturating_sub(1).min(sorted.len().saturating_sub(1));
    match sorted.get(index).copied() {
        Some(measured) => measured,
        None => Duration::ZERO,
    }
}

/// A gate the loud tenant's bodies park on, opened by the test at a chosen
/// moment. Controlled completion: the bodies are known admitted and parked
/// until `release`, so the loud tenant's occupancy is created rather than
/// raced.
#[derive(Clone)]
pub struct Gate {
    /// Opens every parked and future body.
    open: Arc<Notify>,
    /// How many bodies have actually parked, for the test's precondition.
    parked: Arc<AtomicUsize>,
}

impl Gate {
    /// A gate nothing has entered yet.
    pub fn new() -> Self {
        Self {
            open: Arc::new(Notify::new()),
            parked: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// A body that parks here until the gate is opened.
    pub async fn wait(&self) {
        self.parked.fetch_add(1, Ordering::SeqCst);
        self.open.notified().await;
    }

    /// How many bodies are parked right now.
    pub fn parked(&self) -> usize {
        self.parked.load(Ordering::SeqCst)
    }

    /// Open the gate for every parked and future body.
    pub fn release(&self) {
        self.open.notify_waiters();
    }

    /// Complete exactly one parked body: the controlled completion a parked
    /// submission is waiting for. `notify_one` wakes one waiter, or stores the
    /// wake for the next body to park, so no completion is lost to a race
    /// between the release and the park.
    pub fn release_one(&self) {
        self.open.notify_one();
    }
}

/// Await one submission inside `bound`, naming where it was awaited.
///
/// Every `spawn_for` in a flood loop goes through this. A wedged admission — a
/// contended arrival that parks and no admitted task ever resolves it — is
/// otherwise indistinguishable from a slow host, and the first version of this
/// harness proved it: twelve minutes, every worker parked, nothing runnable,
/// and nothing to say so.
pub async fn bounded<F>(
    bound: Duration,
    where_awaited: &str,
    submission: F,
) -> Result<F::Output, String>
where
    F: std::future::Future,
{
    match lgwks_bot::rt::time::timeout(bound, submission).await {
        Ok(value) => Ok(value),
        Err(_) => Err(format!(
            "{where_awaited} never resolved within {bound:?}: a contended \
             submission parked and nothing returned a permit to it"
        )),
    }
}

/// How long a harness submission may wait before the test fails naming where.
///
/// A bound on a failure, not a budget the work must finish inside.
pub const SUBMISSION_BOUND: Duration = Duration::from_millis(2_000);

/// Submit one parking body, completing one held body if the submission parks.
///
/// The submission is polled first, so it registers in its tenant's queue
/// before anything is released; only a submission that is still pending after
/// that poll triggers its one controlled completion. The completed body returns
/// its permit to the round, the round grants it to the waiting submission, and
/// the waiter's wake re-polls this future. A submission that never resolves
/// after its completion is a lost wakeup in the round, and [`bounded`] names
/// it.
pub async fn submit_parked(
    supervisor: &mut Supervisor,
    tenant: &Tenant,
    gate: &Gate,
) -> Result<Result<(), SpawnRefused>, String> {
    let body_gate = gate.clone();
    let mut submission = std::pin::pin!(
        supervisor.spawn_for(tenant, move |_token| async move { body_gate.wait().await })
    );
    let mut released = false;
    bounded(
        SUBMISSION_BOUND,
        "a parked submission after one controlled completion",
        std::future::poll_fn(|context| {
            if let std::task::Poll::Ready(outcome) = submission.as_mut().poll(context) {
                return std::task::Poll::Ready(outcome);
            }
            if !released {
                released = true;
                gate.release_one();
            }
            std::task::Poll::Pending
        }),
    )
    .await
}

/// Submit one body for `tenant` inside [`SUBMISSION_BOUND`], naming the tenant
/// when the submission is refused.
pub async fn admit<F, Fut>(
    supervisor: &mut Supervisor,
    tenant: &Tenant,
    body: F,
) -> Result<(), String>
where
    F: FnOnce(CancellationToken) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let admitted = bounded(
        SUBMISSION_BOUND,
        "one tenant's submission",
        supervisor.spawn_for(tenant, body),
    )
    .await?;
    admitted.map_err(|refusal| format!("{tenant} was refused: {refusal}"))
}

/// The neighbour's measured body: a fixed amount of arithmetic, identical in
/// both arms of a throughput comparison, sunk into [`CONSUMED`] so the
/// optimiser cannot remove it.
pub async fn neighbour_work() {
    let mut accumulator: u64 = 0;
    for step in 0..2_000_u64 {
        accumulator = accumulator.wrapping_add(step.wrapping_mul(2_654_435_761));
    }
    let sunk = match usize::try_from(std::hint::black_box(accumulator)) {
        Ok(as_index) => as_index,
        Err(_) => usize::MAX,
    };
    CONSUMED.fetch_add(sunk, Ordering::Relaxed);
}

/// Where the neighbour's arithmetic is sunk, so the optimiser cannot remove the
/// work two runs are compared on. Wrapping add is used throughout so no run
/// panics on overflow.
static CONSUMED: AtomicUsize = AtomicUsize::new(0);
