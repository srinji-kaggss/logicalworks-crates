//! More than one million task executions in flight at once on one node (#152 §4).
//!
//! The subject is stated before the number, because the issue is explicit that
//! a million inert structs, a queue of a million requests or an extrapolation
//! from a smaller run do not count. Every one of the 1,048,576 subjects here is
//! an **admitted, executing** `Host::run`: it holds its host's admission permit,
//! its body has been entered, and it is suspended at an `await` inside that
//! body. The run waits until every subject has been observed inside its body at
//! the same time, and only then releases them; each subject must then finish
//! with its own input doubled, and every host must get every permit back.
//!
//! One `Host` admits at most `MAX_ADMITTED_TASKS` (65,536) runs: a host is one
//! tenant's admission authority, and that ceiling is deliberate. A million on
//! one node is therefore sixteen tenants, each saturated to its own ceiling —
//! which is also the shape a multi-tenant node actually has, and it makes the
//! tenant arithmetic part of the claim: no host may ever admit past its own
//! ceiling, and no tenant's run may report another tenant's value.
//!
//! The measurement is opt-in (`LGWKS_MILLION=1`) because it holds gigabytes of
//! suspended state: a default `nextest` run reports that it did not run rather
//! than a bound nobody checked (the INV-BOT-16 rule). Its numbers are written to
//! `LGWKS_SCALE_OUT` when set. Peak RSS is taken by the operator's
//! `/usr/bin/time -l` (macOS) or `/usr/bin/time -v` (Linux) around the binary, an
//! observer outside the process whose memory is being measured.
#![cfg(feature = "script")]

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use lgwks_bot::rt::sync::watch;
use lgwks_bot::rt::task::JoinSet;
use lgwks_bot::script::{FlowError, Scope};
use lgwks_bot::task::{Disposition, Host, MAX_ADMITTED_TASKS, task};

#[path = "support/journal.rs"]
mod shared;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Tenants on the node; each is one host saturated to its own ceiling.
const TENANTS: usize = 16;

/// What one subject reports: its input doubled, and how long after the release
/// its body resumed.
type Outcome = (u64, Duration);

/// Nearest-rank percentile, in per-mille, of an already sorted sample.
fn percentile(sorted: &[Duration], per_mille: usize) -> Duration {
    let rank = sorted
        .len()
        .saturating_mul(per_mille)
        .checked_div(1000)
        .unwrap_or_default();
    sorted
        .get(rank.min(sorted.len().saturating_sub(1)))
        .copied()
        .unwrap_or_default()
}

#[test]
fn more_than_a_million_admitted_runs_are_in_flight_at_once() -> TestResult {
    if std::env::var_os("LGWKS_MILLION").is_none() {
        shared::record_measurement("million: not run (set LGWKS_MILLION=1)")?;
        return Ok(());
    }
    let total = TENANTS.saturating_mul(MAX_ADMITTED_TASKS);
    let ceiling = NonZeroUsize::new(MAX_ADMITTED_TASKS).ok_or("the ceiling is non-zero")?;

    let mut hosts = Vec::with_capacity(TENANTS);
    for tenant in 0..TENANTS {
        hosts.push(
            Host::builder(&format!("tenant-{tenant}"))?
                .max_concurrent_tasks(ceiling)
                .build()?,
        );
    }

    // Level-triggered, so a body that subscribes after the release still sees
    // it: the release cannot be lost to a subscription race.
    let (release, gate) = watch::channel::<Option<Instant>>(None);
    let entered = Arc::new(AtomicUsize::new(0));
    // One task shared by every subject, as one declared task is shared by every
    // run of it on a real host; `Task` is not `Clone`, so the share is an `Arc`.
    let hold = Arc::new({
        let entered = Arc::clone(&entered);
        task("hold", move |_scope: Scope, value: u64| {
            let entered = Arc::clone(&entered);
            let mut gate = gate.clone();
            async move {
                entered.fetch_add(1, Ordering::AcqRel);
                let released = gate.wait_for(Option::is_some).await.map_err(|_| {
                    FlowError::failed("the release gate was dropped before the release")
                })?;
                let resumed = released.map(|at| at.elapsed()).unwrap_or_default();
                Ok::<Outcome, FlowError>((value.saturating_mul(2), resumed))
            }
        })?
    });

    let started = Instant::now();
    let (admitted_at, mut outcomes) = lgwks_bot::block_on(async {
        let mut set: JoinSet<(usize, u64, lgwks_bot::task::Report<Outcome>)> = JoinSet::new();
        for index in 0..total {
            let tenant = index % TENANTS;
            let host = hosts.get(tenant).cloned().ok_or("a host per tenant")?;
            let hold = Arc::clone(&hold);
            let value = u64::try_from(index)?;
            set.spawn(async move { (tenant, value, host.run(&hold, value).await) });
        }
        // Every subject must be inside its body at the same time before the
        // release: this is the concurrency claim, observed rather than inferred.
        while entered.load(Ordering::Acquire) < total {
            lgwks_bot::rt::task::yield_now().await;
        }
        let admitted_at = started.elapsed();
        for (tenant, host) in hosts.iter().enumerate() {
            assert_eq!(
                host.admission().in_flight(),
                MAX_ADMITTED_TASKS,
                "tenant {tenant} holds exactly its ceiling while every subject is suspended"
            );
        }
        release.send_replace(Some(Instant::now()));

        let mut outcomes = Vec::with_capacity(total);
        while let Some(joined) = set.join_next().await {
            let (tenant, value, report) = joined?;
            assert_eq!(
                report.disposition(),
                Disposition::Succeeded,
                "tenant {tenant} run {value}: {:?}",
                report.error()
            );
            let (doubled, resumed) = report
                .output()
                .copied()
                .ok_or("a succeeded run has output")?;
            assert_eq!(
                doubled,
                value.saturating_mul(2),
                "run {value} kept its own input"
            );
            outcomes.push(resumed);
        }
        Ok::<_, Box<dyn std::error::Error>>((admitted_at, outcomes))
    })?;
    let finished = started.elapsed();

    assert_eq!(outcomes.len(), total, "every subject reported");
    for (tenant, host) in hosts.iter().enumerate() {
        assert_eq!(
            host.admission().in_flight(),
            0,
            "tenant {tenant}: nothing left in flight"
        );
        assert_eq!(
            host.admission().available_permits(),
            MAX_ADMITTED_TASKS,
            "tenant {tenant}: every permit came back"
        );
        assert!(
            host.admission().peak_in_flight() <= MAX_ADMITTED_TASKS,
            "tenant {tenant}: never admitted past its ceiling"
        );
    }

    outcomes.sort_unstable();
    shared::record_measurement(&format!(
        "million: subjects={total} tenants={TENANTS} per_host={MAX_ADMITTED_TASKS} \
         all_in_flight_after={admitted_at:?} total={finished:?} \
         release_to_resume p50={:?} p95={:?} p99={:?} max={:?}",
        percentile(&outcomes, 500),
        percentile(&outcomes, 950),
        percentile(&outcomes, 990),
        outcomes.last().copied().unwrap_or_default(),
    ))?;
    Ok(())
}
