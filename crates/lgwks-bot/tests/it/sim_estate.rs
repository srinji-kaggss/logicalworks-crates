//! Simulation family: the estate under load, burst and replay.
//!
//! The estate assembles a host, a supervisor and a runtime, and each already has its own evidence
//! (`sim_task_axes`, `sim_supervise_wait`). This target proves the assembly: the ceiling an estate
//! declares is the ceiling its host holds under a drawn scale of runs driven on the estate's own
//! runtime, the supervisor an estate exposes counts and drains the work placed on it, and two
//! estates never lend each other a number. Nothing here reimplements admission or a counter; the
//! real `Estate`, `Host` and `Supervisor` run, and a seed draws only the scale.
//!
//! | Family | Axis | Property it pins |
//! |---|---|---|
//! | `the_ceiling_holds_at_every_drawn_scale` | hyperscale | a drawn scale of runs reaches the declared ceiling and never exceeds it, and the report accounts for every one |
//! | `a_seeded_burst_on_the_supervisor_is_counted_and_drained` | ephemeral | a drawn burst of supervised bodies is spawned, completed and drained exactly once each |
//! | `two_tenants_keep_their_own_accounts` | multi-tenant | two estates at different scales report their own admissions and no one else's |
//! | `saturation_reaches_100_1000_and_10000_through_the_estate` | hyperscale | the three declared tiers complete under a ceiling of 64, return every permit, and leave the estate able to run |
//!
//! Real-world source for the lifecycle the burst exercises: tokio's graceful-shutdown guidance (a
//! cancellation signal, then a drain of the tasks it reached), which `Estate::shutdown_blocking`
//! implements over the crate's supervisor.
//!
//! # What is real and what is seeded
//!
//! Time is not drawn: the bodies are ready, or yield a fixed number of times, so a scenario is
//! decided by structure and replays on a loaded box. A trace carries only facts that do not depend
//! on the scheduler.

#![cfg(feature = "script")]

use crate::band_family;
use crate::load;
use crate::sim;

use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use lgwks_bot::rt::estate::Estate;
use lgwks_bot::script::{FlowError, Scope};
use lgwks_bot::task::{Report, Task, task};

use sim::Band;

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

/// `scale` runs of `work`, one per index, driven together on the estate's own runtime.
fn saturate<F, Fut>(
    estate: &Estate,
    work: &Task<F>,
    scale: usize,
) -> Result<Vec<Report<usize>>, Box<dyn Error>>
where
    F: Fn(Scope, usize) -> Fut,
    Fut: Future<Output = Result<usize, FlowError>>,
{
    let handle = estate.handle().ok_or("a supervised estate has a runtime")?;
    Ok(handle.block_on(lgwks_std::task::join_all(
        (0..scale).map(|index| estate.run_task(work, index)),
    )))
}

// ── Hyperscale ──────────────────────────────────────────────────────────────

/// A drawn scale of runs reaches the declared ceiling, never exceeds it, and is all accounted for.
fn the_ceiling_holds_at_every_drawn_scale(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let scale = usize::try_from(sim.rng().between(100, 250))?;
        let ceiling = usize::try_from(sim.rng().between(1, 8))?;
        let mut estate = Estate::supervised("acme", ceiling)?;
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let work = load::ticking_task!(live, peak, 2)?;

        let reports = saturate(&estate, &work, scale)?;
        load::assert_runs_succeeded(&reports, "estate saturation");
        let observed = peak.load(Ordering::Relaxed);
        assert_eq!(
            observed, ceiling,
            "a scale far past the ceiling reaches it exactly and no further"
        );
        load::assert_admission_conserved(estate.host(), observed, ceiling, "estate saturation");

        let report = estate.shutdown_blocking()?;
        assert_eq!(
            report.runs_admitted(),
            u64::try_from(scale)?,
            "the report accounts for every run"
        );
        assert_eq!(report.runs_refused(), 0, "nothing was refused");
        sim.record(&format!(
            "requested={scale} reached={observed} ceiling={ceiling} admitted={} refused={}",
            report.runs_admitted(),
            report.runs_refused()
        ));
        Ok(())
    })
}

/// The three declared saturation tiers, run through one estate, with recovery after.
///
/// One explicit test rather than a band, because the tiers are the claim: 100, 1,000 and 10,000
/// runs each complete under a ceiling of 64 (the cap this host is asked to hold), return every
/// permit, and leave the estate able to admit work afterwards.
#[test]
fn saturation_reaches_100_1000_and_10000_through_the_estate() -> TestResult {
    const CEILING: usize = 64;
    let mut estate = Estate::supervised("acme", CEILING)?;
    let live = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let work = load::ticking_task!(live, peak, 2)?;

    let mut total: u64 = 0;
    for tier in [100_usize, 1_000, 10_000] {
        let reports = saturate(&estate, &work, tier)?;
        assert_eq!(reports.len(), tier, "tier {tier}: every run reported");
        load::assert_runs_succeeded(&reports, &format!("tier {tier}"));
        let observed = peak.load(Ordering::Relaxed);
        load::assert_admission_conserved(estate.host(), observed, CEILING, &format!("tier {tier}"));
        total = total.saturating_add(u64::try_from(tier)?);
    }

    let after = saturate(&estate, &work, 1)?;
    load::assert_runs_succeeded(&after, "recovery");
    let report = estate.shutdown_blocking()?;
    assert_eq!(
        report.runs_admitted(),
        total.saturating_add(1),
        "the report accounts for every tier and the recovery run"
    );
    assert_eq!(report.runs_refused(), 0, "nothing was refused");
    Ok(())
}

// ── Ephemeral ───────────────────────────────────────────────────────────────

/// A drawn burst of supervised bodies is spawned, completed and drained exactly once each.
///
/// The bodies return at once, so whether one returns before or after the shutdown's stop is the
/// scheduler's: the trace keeps only the counts that cannot depend on it.
fn a_seeded_burst_on_the_supervisor_is_counted_and_drained(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let bodies = usize::try_from(sim.rng().between(1, 64))?;
        let ceiling = usize::try_from(sim.rng().between(1, 8))?;
        let mut estate = Estate::supervised("acme", ceiling)?;
        let handle = estate.handle().ok_or("a supervised estate has a runtime")?;
        let supervisor = estate
            .supervisor()
            .ok_or("a supervised estate has a supervisor")?;
        handle.block_on(async {
            for _ in 0..bodies {
                supervisor.spawn(|_token| async {}).await;
            }
        });

        let report = estate.shutdown_blocking()?;
        let drained = report.supervised();
        let placed = u64::try_from(bodies)?;
        assert_eq!(drained.spawned, placed, "every body was placed");
        assert_eq!(drained.completed, placed, "and every body was drained");
        assert_eq!(
            drained
                .succeeded
                .saturating_add(drained.cancelled)
                .saturating_add(drained.aborted),
            placed,
            "each ended exactly one way"
        );
        assert_eq!(drained.panicked, 0, "none panicked");
        assert_eq!(drained.refused, 0, "none was refused");
        sim.record(&format!(
            "bodies={bodies} ceiling={ceiling} spawned={} completed={}",
            drained.spawned, drained.completed
        ));
        Ok(())
    })
}

// ── Multi-tenant ────────────────────────────────────────────────────────────

/// Two estates at different scales report their own admissions and no one else's.
///
/// The scales are disjoint ranges, so a counter shared between the estates (one static, one
/// installation) could not equal both.
fn two_tenants_keep_their_own_accounts(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let acme_runs = usize::try_from(sim.rng().between(1, 40))?;
        let globex_runs = usize::try_from(sim.rng().between(41, 80))?;
        let mut acme = Estate::supervised("acme", 4)?;
        let mut globex = Estate::supervised("globex", 4)?;
        let echo = task("echo", |_scope: Scope, value: usize| async move {
            Ok::<usize, FlowError>(value)
        })?;

        let first = saturate(&acme, &echo, acme_runs)?;
        let second = saturate(&globex, &echo, globex_runs)?;
        load::assert_runs_succeeded(&first, "acme");
        load::assert_runs_succeeded(&second, "globex");
        assert!(
            first
                .iter()
                .all(|report| report.tenant().as_str() == "acme"),
            "every acme report is acme's"
        );
        assert!(
            second
                .iter()
                .all(|report| report.tenant().as_str() == "globex"),
            "every globex report is globex's"
        );

        let acme_report = acme.shutdown_blocking()?;
        let globex_report = globex.shutdown_blocking()?;
        assert_eq!(
            acme_report.runs_admitted(),
            u64::try_from(acme_runs)?,
            "acme's report counts acme's runs"
        );
        assert_eq!(
            globex_report.runs_admitted(),
            u64::try_from(globex_runs)?,
            "globex's report counts globex's runs"
        );
        sim.record(&format!(
            "acme={acme_runs} globex={globex_runs} admitted={} {}",
            acme_report.runs_admitted(),
            globex_report.runs_admitted()
        ));
        Ok(())
    })
}

// ── Band declarations ───────────────────────────────────────────────────────

band_family::band_family! {
    the_ceiling_holds_at_every_drawn_scale_band_00 => the_ceiling_holds_at_every_drawn_scale, 0;
    the_ceiling_holds_at_every_drawn_scale_band_01 => the_ceiling_holds_at_every_drawn_scale, 1;
    the_ceiling_holds_at_every_drawn_scale_band_02 => the_ceiling_holds_at_every_drawn_scale, 2;
    the_ceiling_holds_at_every_drawn_scale_band_03 => the_ceiling_holds_at_every_drawn_scale, 3;
    a_seeded_burst_on_the_supervisor_is_counted_and_drained_band_04 => a_seeded_burst_on_the_supervisor_is_counted_and_drained, 4;
    a_seeded_burst_on_the_supervisor_is_counted_and_drained_band_05 => a_seeded_burst_on_the_supervisor_is_counted_and_drained, 5;
    a_seeded_burst_on_the_supervisor_is_counted_and_drained_band_06 => a_seeded_burst_on_the_supervisor_is_counted_and_drained, 6;
    a_seeded_burst_on_the_supervisor_is_counted_and_drained_band_07 => a_seeded_burst_on_the_supervisor_is_counted_and_drained, 7;
    two_tenants_keep_their_own_accounts_band_08 => two_tenants_keep_their_own_accounts, 8;
    two_tenants_keep_their_own_accounts_band_09 => two_tenants_keep_their_own_accounts, 9;
    two_tenants_keep_their_own_accounts_band_10 => two_tenants_keep_their_own_accounts, 10;
    two_tenants_keep_their_own_accounts_band_11 => two_tenants_keep_their_own_accounts, 11;
}
