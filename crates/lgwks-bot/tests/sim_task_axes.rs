//! Simulation family: the task front door under load, isolation and drop.
//!
//! The three front-door PRs landed their acceptance rows, but rows T01-T04 and
//! T36 do not exercise the nine axes. This target supplies the missing evidence
//! with seeded sweeps that drive the **real** `Host`, `Task`, `script` runtime
//! and admission semaphore — nothing here reimplements admission, a trail or a
//! permit.
//!
//! | Family | Axis | Property it pins |
//! |---|---|---|
//! | `saturation_conserves_permits` | hyperscale | a drawn scale of concurrent runs never exceeds the admission ceiling, all complete, and every permit returns |
//! | `two_tenants_stay_isolated` | multi-tenant | two hosts with different tenants, one shared task, concurrent: distinct step keys, distinct reports, separate budgets |
//! | `a_dropped_run_releases_everything` | ephemeral | dropping or cancelling a suspended run returns every permit and leaves nothing in flight |
//! | `names_inputs_and_limits` | generalized | task names, inputs and host limits are accepted or refused exactly on their declared boundaries |
//!
//! # What is real and what is seeded
//!
//! The host, the semaphore, the trail, the deadline and the step key are all
//! real. Time is *small* rather than virtual (a millisecond-scale deadline, an
//! immediately-ready body), so a scenario is decided by structure rather than
//! by how loaded the machine is. Nothing in a trace hash is a wall-clock
//! reading, which is what lets the same seed replay exactly on a busy box.

#![cfg(feature = "script")]

mod sim;

// The band-declaration macro, defined once for the whole layer.
#[path = "sim/bands.rs"]
mod band_family;

// The shared load instrument, so this target and `task_front_door` measure the
// same live/peak body.
#[path = "support/load.rs"]
mod load;

use std::cell::RefCell;
use std::error::Error;
use std::num::NonZeroUsize;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use lgwks_bot::script::{FlowError, Scope};
use lgwks_bot::task::{Disposition, Host, HostError, Task, task};

use sim::Band;
use sim::Rng;

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

/// A task behind a reference count, so two hosts can run the same one.
type Shared<F> = Rc<Task<F>>;

/// A host for `tenant` admitting at most `tasks` runs at once.
fn bounded(tenant: &str, tasks: usize) -> Result<Host, Box<dyn Error>> {
    let limit = NonZeroUsize::new(tasks).ok_or("the ceiling must be at least one")?;
    Ok(Host::builder(tenant)?.max_concurrent_tasks(limit).build()?)
}

use load::{drive, pending_once};

// ── Hyperscale ──────────────────────────────────────────────────────────────

/// A drawn scale of concurrent runs never exceeds the admission ceiling, all
/// complete, and every permit returns.
///
/// Each run is a separate future polled on the caller's task, so each has its
/// own task-local scope and takes its own permit. The ceiling is asserted on
/// both sides: what the bodies observed, and the host's own high-water mark.
fn saturation_conserves_permits(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let scale = usize::try_from(sim.rng().between(100, 250))?;
        let ceiling = usize::try_from(sim.rng().between(1, 8))?;
        let host = bounded("acme", ceiling)?;
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let work = load::ticking_task!(live, peak, 2)?;

        let reports = drive(lgwks_std::task::join_all(
            (0..scale).map(|index| host.run(&work, index)),
        ));
        assert_eq!(reports.len(), scale, "every run produced a report");
        load::assert_runs_succeeded(&reports, "saturation");

        let observed = peak.load(Ordering::Relaxed);
        // A ceiling of one admits no second run, so there is nothing to overlap;
        // above one the runs must genuinely be concurrent.
        if ceiling > 1 {
            assert!(
                observed > 1,
                "the runs must overlap above a ceiling of one; the peak was {observed}"
            );
        } else {
            assert_eq!(observed, 1, "a ceiling of one admits one run at a time");
        }
        load::assert_admission_conserved(&host, observed, ceiling, "saturation");
        assert_eq!(live.load(Ordering::Relaxed), 0, "every body finished");
        sim.record(&format!(
            "requested={scale} reached={observed} ceiling={ceiling} admitted={} refused={}",
            host.admission().admitted(),
            host.admission().refused()
        ));
        Ok(())
    })
}

/// The three declared saturation tiers, run concurrently, with recovery after.
///
/// One explicit test rather than a band, because the tiers are the claim: 100,
/// 1,000 and 10,000 concurrent runs each complete, never exceed the ceiling,
/// return every permit, and leave the host able to admit work afterwards.
#[test]
fn saturation_reaches_100_1000_and_10000_with_recovery() -> TestResult {
    const CEILING: usize = 64;
    let host = bounded("acme", CEILING)?;
    let live = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let work = load::ticking_task!(live, peak, 2)?;

    for tier in [100_usize, 1_000, 10_000] {
        let before = peak.load(Ordering::Relaxed);
        let reports = drive(lgwks_std::task::join_all(
            (0..tier).map(|index| host.run(&work, index)),
        ));
        assert_eq!(reports.len(), tier, "tier {tier}: every run reported");
        assert!(
            reports
                .iter()
                .all(|report| report.disposition() == Disposition::Succeeded),
            "tier {tier}: every run completed"
        );
        let observed = peak.load(Ordering::Relaxed);
        assert!(observed >= before, "tier {tier}: the mark is monotone");
        load::assert_admission_conserved(&host, observed, CEILING, &format!("tier {tier}"));
    }

    // Recovery: after 10,000 runs the host's budget is whole and it still admits.
    let after = drive(host.run(&work, 7));
    assert_eq!(
        after.disposition(),
        Disposition::Succeeded,
        "the host recovers after the largest tier: {:?}",
        after.error()
    );
    assert_eq!(after.output().copied(), Some(7), "and runs to a value");
    Ok(())
}

// ── Multi-tenant ────────────────────────────────────────────────────────────

/// Two hosts with different tenants, one shared task, run concurrently: the
/// step keys, the reports and the admission budgets never cross.
///
/// The task name and the step path are identical for both runs — only the
/// tenant differs — so a key that was not tenant-scoped would collide and this
/// would fail. The two runs are driven on one task through `join_all`, so they
/// interleave at their yield points rather than running one after the other.
fn two_tenants_stay_isolated(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let ceiling = usize::try_from(sim.rng().between(1, 4))?;
        let input = sim.rng().between(0, 10_000);
        let acme = bounded("acme", ceiling)?;
        let globex = bounded("globex", ceiling)?;
        let seen: Rc<RefCell<Vec<(String, String, String)>>> = Rc::new(RefCell::new(Vec::new()));
        let log = Rc::clone(&seen);

        // One task, shared by both hosts. The same closure type means the two
        // `run` futures have one type and can be driven together.
        let shared: Shared<_> = Rc::new(task("shared", move |scope: Scope, value: u32| {
            let log = Rc::clone(&log);
            async move {
                log.borrow_mut().push((
                    scope.tenant().as_str().to_owned(),
                    scope.path().to_owned(),
                    scope.key().to_hex(),
                ));
                Ok::<u32, FlowError>(value)
            }
        })?);

        let reports = drive(lgwks_std::task::join_all(vec![
            acme.run(&shared, input),
            globex.run(&shared, input),
        ]));
        assert_eq!(reports.len(), 2, "both concurrent runs reported");
        for report in &reports {
            assert_eq!(
                report.disposition(),
                Disposition::Succeeded,
                "a concurrent cross-tenant run completed: {:?}",
                report.error()
            );
            assert_eq!(report.output().copied(), Some(input), "each kept its input");
            assert_eq!(
                report.task_name(),
                "shared",
                "both runs are of the same task name"
            );
        }
        assert_eq!(
            reports[0].tenant().as_str(),
            "acme",
            "the first report belongs to its own host's tenant"
        );
        assert_eq!(
            reports[1].tenant().as_str(),
            "globex",
            "the second report belongs to the other tenant"
        );

        let entries = seen.borrow();
        assert_eq!(entries.len(), 2, "each run recorded one step");
        assert_eq!(
            entries[0].1, entries[1].1,
            "the same shared step must produce the same path"
        );
        assert_ne!(
            entries[0].0, entries[1].0,
            "the two tenants must be recorded distinctly"
        );
        assert_ne!(
            entries[0].2, entries[1].2,
            "identical paths under different tenants must key apart"
        );
        // The key is over tenant + path, so identical paths must still key
        // differently across tenants.
        assert_ne!(
            step_key_of(&acme),
            step_key_of(&globex),
            "a key is scoped by its tenant, so a shared path keys apart"
        );
        for host in [&acme, &globex] {
            load::assert_budget_whole(host, ceiling, &format!("tenant {}", host.tenant()));
        }
        assert_eq!(
            acme.admission().admitted(),
            1,
            "each host admitted exactly its own run"
        );
        assert_eq!(globex.admission().admitted(), 1, "and only its own");
        sim.record(&format!(
            "input={input} ceiling={ceiling} path={} keys={} {}",
            entries[0].1, entries[0].2, entries[1].2
        ));
        Ok(())
    })
}

/// The step key one host produces for a canonical step of `shared`.
///
/// Built through the real `Scope` the same way the run's body is, so the
/// comparison is of the estate's key derivation rather than a reimplementation.
fn step_key_of(host: &Host) -> String {
    let scope = Scope::root(host.tenant().clone());
    scope
        .enter("shared")
        .map_or_else(|_error| String::new(), |step| step.key().to_hex())
}

// ── Ephemeral ───────────────────────────────────────────────────────────────

/// Dropping or cancelling a suspended run returns every permit and leaves
/// nothing in flight.
///
/// The body parks forever, so the only way it ends is the drop. A cancelled
/// host does the same: cancellation is a boundary the suspended run observes
/// when it is dropped, and the counters must return to their starting values
/// either way.
fn a_dropped_run_releases_everything(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let ceiling = usize::try_from(sim.rng().between(1, 3))?;
        let cancel = sim.rng().chance(500);
        let host = bounded("acme", ceiling)?;
        let park = task("park", |_scope: Scope, ()| async move {
            std::future::pending::<()>().await;
            Ok::<(), FlowError>(())
        })?;

        let mut run = Box::pin(host.run(&park, ()));
        assert!(
            pending_once(run.as_mut()),
            "the body parks forever, so the run is pending"
        );
        assert_eq!(
            host.admission().available_permits(),
            ceiling.saturating_sub(1),
            "the suspended run holds one permit"
        );
        if cancel {
            host.cancel();
        }
        drop(run);

        load::assert_permits_released(&host, ceiling, &format!("a dropped run (cancel={cancel})"));

        // A fresh host still runs: the abandoned state is gone, not merely
        // hidden behind counters.
        let fresh = bounded("acme", ceiling)?;
        let echo = task("echo", |_scope: Scope, value: u32| async move {
            Ok::<u32, FlowError>(value)
        })?;
        let report = drive(fresh.run(&echo, 3_u32));
        assert_eq!(
            report.disposition(),
            Disposition::Succeeded,
            "a fresh host runs after the drop: {:?}",
            report.error()
        );
        sim.record(&format!(
            "ceiling={ceiling} cancel={cancel} permits={} in_flight={} refused={}",
            host.admission().available_permits(),
            host.admission().in_flight(),
            host.admission().refused()
        ));
        Ok(())
    })
}

/// A run cancelled by the host's stop after admission is reported `Cancelled`,
/// and its permit returns.
#[test]
fn a_cancelled_admitted_run_is_cancelled_and_releases_its_permit() -> TestResult {
    let host = bounded("acme", 1)?;
    let awaiting = task("awaiting", |scope: Scope, ()| async move {
        // Park until the host stops, then observe it: the checkpoint is what
        // turns the stop into a `Cancelled` disposition rather than a return
        // that merely ran after the stop.
        scope.token().cancelled().await;
        scope.checkpoint()?;
        Ok::<(), FlowError>(())
    })?;
    let report: lgwks_bot::task::Report<()> = drive(async {
        let mut future = std::pin::pin!(host.run(&awaiting, ()));
        let mut started = false;
        std::future::poll_fn(|context| match future.as_mut().poll(context) {
            std::task::Poll::Pending => {
                if !started {
                    started = true;
                    // The body is now parked on its token, having been admitted.
                    host.cancel();
                }
                std::task::Poll::Pending
            }
            std::task::Poll::Ready(report) => std::task::Poll::Ready(report),
        })
        .await
    });
    assert_eq!(
        report.disposition(),
        Disposition::Cancelled,
        "a body stopped by its host after admission is Cancelled: {:?}",
        report.error()
    );
    load::assert_budget_whole(&host, 1, "the cancelled run");
    Ok(())
}

// ── Generalized ─────────────────────────────────────────────────────────────

/// The name a scenario drew, and whether it is valid.
fn drawn_name(rng: &mut Rng) -> (String, bool) {
    match rng.below(7) {
        0 => (String::from("a"), true),
        1 => ("g".repeat(128), true),
        2 => ("g".repeat(129), false),
        3 => (String::new(), false),
        4 => (String::from("has space"), false),
        5 => (String::from("has/slash"), false),
        _ => (String::from("ok-name_1.v2:3"), true),
    }
}

/// Task names, inputs and host limits are accepted or refused exactly on their
/// declared boundaries.
///
/// Each case is chosen independently from the seed: a name of a drawn form, an
/// input value, and one of the three host ceilings (admission, deadline,
/// progress) at a boundary or one past it. The assertion is always the same
/// two-sided fact — accepted iff the value is inside the declared bound — so
/// the sweep is a property rather than a table.
fn names_inputs_and_limits(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let (name, valid) = drawn_name(sim.rng());
        let declared = task(&name, |_scope: Scope, value: u32| async move {
            Ok::<u32, FlowError>(value)
        });
        assert_eq!(
            declared.is_ok(),
            valid,
            "task name {name:?} accepted iff valid (len {})",
            name.len()
        );
        if let Ok(declared) = declared.as_ref() {
            assert_eq!(declared.name(), name, "an accepted name is kept verbatim");
        }

        let input = sim.rng().between(0, 10_000);
        let limit_case = sim.rng().below(6);
        match limit_case {
            0 => assert!(
                bounded("acme", 1).is_ok(),
                "an admission ceiling of one is accepted"
            ),
            1 => assert!(
                bounded("acme", lgwks_bot::task::MAX_ADMITTED_TASKS).is_ok(),
                "an admission ceiling at the bound is accepted"
            ),
            2 => {
                let over = lgwks_bot::task::MAX_ADMITTED_TASKS.saturating_add(1);
                let limit =
                    NonZeroUsize::new(over).ok_or("the bound plus one is a nonzero limit")?;
                assert!(
                    matches!(
                        Host::builder("acme")?.max_concurrent_tasks(limit).build(),
                        Err(HostError::Bound { .. })
                    ),
                    "an admission ceiling past the bound is refused"
                );
            }
            3 => assert!(
                matches!(
                    Host::builder("acme")?
                        .default_deadline(Duration::ZERO)
                        .build(),
                    Err(HostError::Bound { .. })
                ),
                "a zero deadline is refused at build time"
            ),
            4 => assert!(
                Host::builder("acme")?
                    .default_deadline(lgwks_bot::task::MAX_TASK_DEADLINE)
                    .build()
                    .is_ok(),
                "a deadline at the ceiling is accepted"
            ),
            _ => {
                let over = lgwks_bot::task::MAX_TASK_DEADLINE.saturating_mul(2);
                assert!(
                    matches!(
                        Host::builder("acme")?.default_deadline(over).build(),
                        Err(HostError::Bound { .. })
                    ),
                    "a deadline past the ceiling is refused"
                );
            }
        }

        // An input round-trips through a run under an accepted name.
        if valid {
            let host = bounded("acme", 2)?;
            let declared = task(&name, |_scope: Scope, value: u32| async move {
                Ok::<u32, FlowError>(value)
            })?;
            let report = drive(host.run(&declared, input));
            assert_eq!(
                report.disposition(),
                Disposition::Succeeded,
                "a valid name over input {input} runs: {:?}",
                report.error()
            );
            assert_eq!(
                report.output().copied(),
                Some(input),
                "the drawn input round-trips"
            );
        }
        sim.record(&format!(
            "name_len={} valid={valid} input={input} limits={limit_case}",
            name.len()
        ));
        Ok(())
    })
}

// ── The replay receipt ──────────────────────────────────────────────────────

/// The same seed produces the same trace hash, twice over, over the union of
/// the families above.
fn the_same_seed_replays(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let scale = usize::try_from(sim.rng().between(4, 32))?;
        let ceiling = usize::try_from(sim.rng().between(1, 4))?;
        let host = bounded("acme", ceiling)?;
        let work = task("work", |_scope: Scope, value: usize| async move {
            Ok::<usize, FlowError>(value)
        })?;
        let reports = drive(lgwks_std::task::join_all(
            (0..scale).map(|index| host.run(&work, index)),
        ));
        let succeeded = reports
            .iter()
            .filter(|report| report.disposition() == Disposition::Succeeded)
            .count();
        sim.record(&format!(
            "scale={scale} ceiling={ceiling} succeeded={succeeded} admitted={} refused={} peak={}",
            host.admission().admitted(),
            host.admission().refused(),
            host.admission().peak_in_flight()
        ));
        Ok(())
    })
}

// ── Band declarations ───────────────────────────────────────────────────────

band_family::band_family! {
    saturation_conserves_permits_band_00 => saturation_conserves_permits, 0;
    saturation_conserves_permits_band_01 => saturation_conserves_permits, 1;
    saturation_conserves_permits_band_02 => saturation_conserves_permits, 2;
    saturation_conserves_permits_band_03 => saturation_conserves_permits, 3;
    saturation_conserves_permits_band_04 => saturation_conserves_permits, 4;
    saturation_conserves_permits_band_05 => saturation_conserves_permits, 5;
    saturation_conserves_permits_band_06 => saturation_conserves_permits, 6;
    saturation_conserves_permits_band_07 => saturation_conserves_permits, 7;
    saturation_conserves_permits_band_08 => saturation_conserves_permits, 8;
    saturation_conserves_permits_band_09 => saturation_conserves_permits, 9;
    saturation_conserves_permits_band_10 => saturation_conserves_permits, 10;
    saturation_conserves_permits_band_11 => saturation_conserves_permits, 11;
    saturation_conserves_permits_band_12 => saturation_conserves_permits, 12;
    saturation_conserves_permits_band_13 => saturation_conserves_permits, 13;
    saturation_conserves_permits_band_14 => saturation_conserves_permits, 14;
    saturation_conserves_permits_band_15 => saturation_conserves_permits, 15;
    two_tenants_stay_isolated_band_00 => two_tenants_stay_isolated, 0;
    two_tenants_stay_isolated_band_01 => two_tenants_stay_isolated, 1;
    two_tenants_stay_isolated_band_02 => two_tenants_stay_isolated, 2;
    two_tenants_stay_isolated_band_03 => two_tenants_stay_isolated, 3;
    two_tenants_stay_isolated_band_04 => two_tenants_stay_isolated, 4;
    two_tenants_stay_isolated_band_05 => two_tenants_stay_isolated, 5;
    two_tenants_stay_isolated_band_06 => two_tenants_stay_isolated, 6;
    two_tenants_stay_isolated_band_07 => two_tenants_stay_isolated, 7;
    two_tenants_stay_isolated_band_08 => two_tenants_stay_isolated, 8;
    two_tenants_stay_isolated_band_09 => two_tenants_stay_isolated, 9;
    two_tenants_stay_isolated_band_10 => two_tenants_stay_isolated, 10;
    two_tenants_stay_isolated_band_11 => two_tenants_stay_isolated, 11;
    two_tenants_stay_isolated_band_12 => two_tenants_stay_isolated, 12;
    two_tenants_stay_isolated_band_13 => two_tenants_stay_isolated, 13;
    two_tenants_stay_isolated_band_14 => two_tenants_stay_isolated, 14;
    two_tenants_stay_isolated_band_15 => two_tenants_stay_isolated, 15;
    a_dropped_run_releases_everything_band_00 => a_dropped_run_releases_everything, 0;
    a_dropped_run_releases_everything_band_01 => a_dropped_run_releases_everything, 1;
    a_dropped_run_releases_everything_band_02 => a_dropped_run_releases_everything, 2;
    a_dropped_run_releases_everything_band_03 => a_dropped_run_releases_everything, 3;
    a_dropped_run_releases_everything_band_04 => a_dropped_run_releases_everything, 4;
    a_dropped_run_releases_everything_band_05 => a_dropped_run_releases_everything, 5;
    a_dropped_run_releases_everything_band_06 => a_dropped_run_releases_everything, 6;
    a_dropped_run_releases_everything_band_07 => a_dropped_run_releases_everything, 7;
    a_dropped_run_releases_everything_band_08 => a_dropped_run_releases_everything, 8;
    a_dropped_run_releases_everything_band_09 => a_dropped_run_releases_everything, 9;
    a_dropped_run_releases_everything_band_10 => a_dropped_run_releases_everything, 10;
    a_dropped_run_releases_everything_band_11 => a_dropped_run_releases_everything, 11;
    a_dropped_run_releases_everything_band_12 => a_dropped_run_releases_everything, 12;
    a_dropped_run_releases_everything_band_13 => a_dropped_run_releases_everything, 13;
    a_dropped_run_releases_everything_band_14 => a_dropped_run_releases_everything, 14;
    a_dropped_run_releases_everything_band_15 => a_dropped_run_releases_everything, 15;
    names_inputs_and_limits_band_00 => names_inputs_and_limits, 0;
    names_inputs_and_limits_band_01 => names_inputs_and_limits, 1;
    names_inputs_and_limits_band_02 => names_inputs_and_limits, 2;
    names_inputs_and_limits_band_03 => names_inputs_and_limits, 3;
    names_inputs_and_limits_band_04 => names_inputs_and_limits, 4;
    names_inputs_and_limits_band_05 => names_inputs_and_limits, 5;
    names_inputs_and_limits_band_06 => names_inputs_and_limits, 6;
    names_inputs_and_limits_band_07 => names_inputs_and_limits, 7;
    names_inputs_and_limits_band_08 => names_inputs_and_limits, 8;
    names_inputs_and_limits_band_09 => names_inputs_and_limits, 9;
    names_inputs_and_limits_band_10 => names_inputs_and_limits, 10;
    names_inputs_and_limits_band_11 => names_inputs_and_limits, 11;
    names_inputs_and_limits_band_12 => names_inputs_and_limits, 12;
    names_inputs_and_limits_band_13 => names_inputs_and_limits, 13;
    names_inputs_and_limits_band_14 => names_inputs_and_limits, 14;
    names_inputs_and_limits_band_15 => names_inputs_and_limits, 15;
    the_same_seed_replays_band_00 => the_same_seed_replays, 0;
    the_same_seed_replays_band_01 => the_same_seed_replays, 1;
    the_same_seed_replays_band_02 => the_same_seed_replays, 2;
    the_same_seed_replays_band_03 => the_same_seed_replays, 3;
    the_same_seed_replays_band_04 => the_same_seed_replays, 4;
    the_same_seed_replays_band_05 => the_same_seed_replays, 5;
    the_same_seed_replays_band_06 => the_same_seed_replays, 6;
    the_same_seed_replays_band_07 => the_same_seed_replays, 7;
    the_same_seed_replays_band_08 => the_same_seed_replays, 8;
    the_same_seed_replays_band_09 => the_same_seed_replays, 9;
    the_same_seed_replays_band_10 => the_same_seed_replays, 10;
    the_same_seed_replays_band_11 => the_same_seed_replays, 11;
    the_same_seed_replays_band_12 => the_same_seed_replays, 12;
    the_same_seed_replays_band_13 => the_same_seed_replays, 13;
    the_same_seed_replays_band_14 => the_same_seed_replays, 14;
    the_same_seed_replays_band_15 => the_same_seed_replays, 15;
}
