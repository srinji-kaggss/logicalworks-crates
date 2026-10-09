//! `--flood-split`: the wall-clock half of INV-BOT-151, measured where timing
//! evidence belongs (#375).
//!
//! The gate on the claim is deterministic — `sim_tenancy_flood` checks, in
//! virtual time through the shipped round, that a flood neither delays the
//! neighbour nor buys the round extra work per event. What a clock can add is
//! the size of the effect on a real runtime, and that is a report: a ratio of
//! two arms on a host shared with other work reads the host as much as the code
//! (one CI run spread its paired ratios from 170‰ to 34,985‰), so it is printed
//! with its spread and never asserted.
//!
//! Each paired round runs both arms back to back on identically shaped
//! supervisors, alternating which goes first. One arm is the neighbour alone;
//! the other puts [`FLOOD`] fail-at-once attacker submissions before each of the
//! neighbour's. Two numbers come from each arm, both the neighbour's own:
//!
//! - **throughput** — the time the neighbour spent inside its own `spawn_for`
//!   calls, which with its ceiling reached is the rate it is served at;
//! - **decision** — the round's arrival-decision time charged to the
//!   neighbour's own admissions, read from `Supervisor::admission_cost` around
//!   each neighbour call. One caller submits for both tenants, so the counters'
//!   movement across a neighbour call is that call's decision and nothing else.
//!   Mixing the attacker's own decisions into the attacked arm's mean, as the
//!   retired test did, compared two different populations.

use std::time::{Duration, Instant};

use lgwks_bot::rt::supervise::Supervisor;
use lgwks_bot::rt::tenancy::TenancyPolicy;
use lgwks_bot::script::Tenant;
use lgwks_std::trace::info;

/// The pool both arms run under; each tenant's ceiling is half of it, so the
/// attacker can never hold a permit the neighbour's own ceiling would reach.
const POOL: usize = 64;
/// The neighbour's tasks per arm.
const TASKS: usize = 2_000;
/// Fail-at-once attacker submissions before each neighbour submission.
const FLOOD: usize = 4;

/// One arm's neighbour-only measurements.
struct Arm {
    /// Time inside the neighbour's own `spawn_for` calls.
    wall: Duration,
    /// The neighbour's own arrival decisions.
    decisions: u64,
    /// Their charged time, in nanoseconds.
    decision_nanos: u64,
}

impl Arm {
    /// The mean decision in nanoseconds, refused for an arm with none.
    fn mean_decision(&self) -> Result<u128, String> {
        u128::from(self.decision_nanos)
            .checked_div(u128::from(self.decisions))
            .ok_or_else(|| String::from("an arm took no neighbour decisions to average"))
    }
}

/// The neighbour's measured body: fixed arithmetic, sunk so it is not removed.
async fn neighbour_work() {
    let mut accumulator: u64 = 0;
    for step in 0..2_000_u64 {
        accumulator = accumulator.wrapping_add(step.wrapping_mul(2_654_435_761));
    }
    std::hint::black_box(accumulator);
}

/// One arm: `flood` attacker submissions before each neighbour submission.
async fn arm(neighbour: &Tenant, attacker: &Tenant, flood: usize) -> Result<Arm, String> {
    let mut supervisor =
        Supervisor::with_tenancy(POOL, TenancyPolicy::new(POOL.div_euclid(2), POOL));
    let mut measured = Arm {
        wall: Duration::ZERO,
        decisions: 0,
        decision_nanos: 0,
    };
    for _ in 0..TASKS {
        for _ in 0..flood {
            supervisor
                .spawn_for(attacker, |_token| async {})
                .await
                .map_err(|refusal| format!("the attacker was refused: {refusal}"))?;
        }
        let (arrivals_before, nanos_before, _, _) = supervisor.admission_cost();
        let started = Instant::now();
        supervisor
            .spawn_for(neighbour, |_token| neighbour_work())
            .await
            .map_err(|refusal| format!("the neighbour was refused: {refusal}"))?;
        measured.wall = measured.wall.saturating_add(started.elapsed());
        let (arrivals_after, nanos_after, _, _) = supervisor.admission_cost();
        measured.decisions = measured
            .decisions
            .saturating_add(arrivals_after.saturating_sub(arrivals_before));
        measured.decision_nanos = measured
            .decision_nanos
            .saturating_add(nanos_after.saturating_sub(nanos_before));
    }
    let report = supervisor.shutdown().await;
    let submitted = u64::try_from(TASKS.saturating_mul(flood.saturating_add(1)))
        .map_err(|error| format!("the submission count does not fit in u64: {error}"))?;
    if report.stats().spawned != submitted || report.stats().completed != submitted {
        let refusal = format!(
            "the arm spawned {} and completed {} of {submitted} submitted tasks",
            report.stats().spawned,
            report.stats().completed
        );
        info!(error = %refusal, "flood split: an arm lost tasks");
        return Err(refusal);
    }
    Ok(measured)
}

/// `part` per mille of `whole`, refused for a whole of zero.
fn per_mille(part: u128, whole: u128) -> Result<u128, String> {
    part.saturating_mul(1_000)
        .checked_div(whole)
        .ok_or_else(|| String::from("a ratio against an arm that took no time"))
}

/// The nearest-rank `nth` percentile of `sorted`, refused for no samples.
fn rank(sorted: &[u128], nth: usize) -> Result<u128, String> {
    let index = nth
        .saturating_mul(sorted.len())
        .div_ceil(100)
        .saturating_sub(1)
        .min(sorted.len().saturating_sub(1));
    sorted
        .get(index)
        .copied()
        .ok_or_else(|| String::from("no paired round to rank"))
}

/// Run `rounds` paired rounds and report both ratios' p50/p95/p99 and range.
pub async fn flood_split(rounds: usize) -> Result<(), Box<dyn std::error::Error>> {
    let neighbour = Tenant::new("neighbour")?;
    let attacker = Tenant::new("attacker")?;
    let mut throughput = Vec::with_capacity(rounds);
    let mut decision = Vec::with_capacity(rounds);
    for round in 0..rounds {
        let (quiet, flooded) = if round % 2 == 0 {
            let quiet = arm(&neighbour, &attacker, 0).await?;
            (quiet, arm(&neighbour, &attacker, FLOOD).await?)
        } else {
            let flooded = arm(&neighbour, &attacker, FLOOD).await?;
            (arm(&neighbour, &attacker, 0).await?, flooded)
        };
        throughput.push(per_mille(flooded.wall.as_nanos(), quiet.wall.as_nanos())?);
        decision.push(per_mille(flooded.mean_decision()?, quiet.mean_decision()?)?);
    }
    throughput.sort_unstable();
    decision.sort_unstable();
    for (name, ratios) in [("throughput", &throughput), ("decision", &decision)] {
        info!(
            ratio = name,
            rounds,
            p50 = %rank(ratios, 50)?,
            p95 = %rank(ratios, 95)?,
            p99 = %rank(ratios, 99)?,
            min = %rank(ratios, 0)?,
            max = %rank(ratios, 100)?,
            "flood split: the neighbour's flooded arm per mille of its arm alone"
        );
    }
    Ok(())
}
