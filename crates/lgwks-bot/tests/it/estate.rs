//! The estate front door: tiers, refusals, one run, cancel, two tenants.
//!
//! The assembly owns what it declares and refuses the rest typed: tenant validation (INV-BOT-58),
//! the admission ceiling it inherits from the host (INV-BOT-20), durable steps where a store is
//! installed (INV-BOT-54), and store directories that die with the run (INV-BOT-116).

use lgwks_bot::rt::estate::{Estate, EstateError};
use lgwks_bot::rt::runtime::Runtime;
use lgwks_bot::task::{Disposition, HostError, task};

use crate::scratch::Scratch;

type TestResult = Result<(), String>;

#[test]
fn three_tiers_declare_what_they_own() -> TestResult {
    let driver = Runtime::new().map_err(|error| error.to_string())?;

    let bare = Estate::in_process("acme").map_err(|error| error.to_string())?;
    assert!(!bare.has_runtime(), "in-process carries no runtime");
    assert!(!bare.has_supervisor(), "in-process carries no supervisor");
    assert!(bare.run_store().is_none(), "in-process carries no store");
    assert_eq!(bare.tenant(), "acme", "the estate runs for its tenant");
    assert_eq!(bare.tier(), "in-process", "the tier reads as declared");

    let supervised = Estate::supervised("acme", 8).map_err(|error| error.to_string())?;
    assert!(supervised.has_runtime(), "supervised carries a runtime");
    assert!(
        supervised.has_supervisor(),
        "supervised carries a supervisor"
    );
    assert!(
        supervised.run_store().is_none(),
        "supervised without a directory carries no store"
    );
    assert_eq!(
        supervised.tier(),
        "supervised",
        "the tier reads as declared"
    );
    let report = driver.block_on(supervised.shutdown());
    assert_eq!(report.tenant(), "acme", "the report names its tenant");
    assert_eq!(report.tier(), "supervised", "the report names its tier");

    let scratch = Scratch::new("estate").map_err(|error| error.to_string())?;
    let durable = Estate::durable("acme", 8, scratch.path()).map_err(|error| error.to_string())?;
    assert!(durable.has_runtime(), "durable carries a runtime");
    assert!(durable.has_supervisor(), "durable carries a supervisor");
    assert!(durable.run_store().is_some(), "durable carries a store");
    assert_eq!(durable.tier(), "durable", "the tier reads as declared");
    let probe = task("estate-probe", |_scope, input: u64| async move {
        Ok(input.saturating_add(1))
    })
    .map_err(|error| error.to_string())?;
    let run = durable
        .run_blocking(&probe, 41)
        .map_err(|error| error.to_string())?;
    assert!(
        matches!(run.disposition(), Disposition::Succeeded),
        "a durable run succeeds"
    );
    let report = driver.block_on(durable.shutdown());
    assert_eq!(
        report.runs_admitted(),
        1,
        "the shutdown report accounts for the durable run"
    );
    Ok(())
}

#[test]
fn an_empty_tenant_is_a_typed_refusal() -> TestResult {
    match Estate::in_process("") {
        Err(EstateError::InvalidTenant { .. }) => {}
        Err(error) => {
            return Err(format!("an empty tenant is InvalidTenant, not {error}"));
        }
        Ok(_) => return Err("an empty tenant must not build".to_owned()),
    }
    match Estate::supervised("", 8) {
        Err(EstateError::InvalidTenant { .. }) => {}
        Err(error) => {
            return Err(format!("an empty tenant is InvalidTenant, not {error}"));
        }
        Ok(_) => return Err("an empty tenant must not build".to_owned()),
    }
    Ok(())
}

#[test]
fn a_run_succeeds_and_the_shutdown_report_accounts_for_it() -> TestResult {
    let estate = Estate::supervised("acme", 8).map_err(|error| error.to_string())?;
    let probe = task("estate-probe", |_scope, input: u64| async move {
        Ok(input.saturating_add(1))
    })
    .map_err(|error| error.to_string())?;
    let run = estate
        .run_blocking(&probe, 41)
        .map_err(|error| error.to_string())?;
    assert!(
        matches!(run.disposition(), Disposition::Succeeded),
        "the run succeeds"
    );
    assert_eq!(run.output(), Some(&42u64), "the run answers its value");
    let driver = Runtime::new().map_err(|error| error.to_string())?;
    let report = driver.block_on(estate.shutdown());
    assert_eq!(report.tenant(), "acme", "the report names its tenant");
    assert_eq!(report.tier(), "supervised", "the report names its tier");
    assert_eq!(report.runs_admitted(), 1, "the host admitted the run");
    assert_eq!(report.runs_refused(), 0, "nothing was refused");
    assert_eq!(
        report.supervised().succeeded,
        0,
        "no task was placed on the supervisor itself"
    );
    Ok(())
}

#[test]
fn cancel_then_shutdown_blocking_drains_to_an_empty_terminal_record() -> TestResult {
    let estate = Estate::supervised("acme", 8).map_err(|error| error.to_string())?;
    estate.cancel();
    let report = estate
        .shutdown_blocking()
        .map_err(|error| error.to_string())?;
    let drained = report.supervised();
    assert_eq!(drained.spawned, 0, "nothing started");
    assert_eq!(drained.completed, 0, "nothing finished");
    assert_eq!(drained.succeeded, 0, "nothing succeeded");
    assert_eq!(drained.failed, 0, "nothing failed");
    assert_eq!(drained.cancelled, 0, "nothing was cancelled mid-body");
    assert_eq!(drained.refused, 0, "nothing was refused");
    assert_eq!(drained.aborted, 0, "nothing was aborted");
    assert_eq!(drained.panicked, 0, "nothing panicked");
    assert_eq!(drained.reports_dropped, 0, "nothing was dropped");
    assert_eq!(report.runs_admitted(), 0, "no run was admitted");
    assert_eq!(report.runs_refused(), 0, "no run was refused");
    Ok(())
}

#[test]
fn two_tenants_run_one_task_name_without_sharing() -> TestResult {
    let acme = Estate::in_process("acme").map_err(|error| error.to_string())?;
    let globex = Estate::in_process("globex").map_err(|error| error.to_string())?;
    let probe = task("shared-probe", |_scope, input: u64| async move {
        Ok(input.saturating_add(1))
    })
    .map_err(|error| error.to_string())?;
    let acme_run = acme
        .run_blocking(&probe, 6)
        .map_err(|error| error.to_string())?;
    let globex_run = globex
        .run_blocking(&probe, 6)
        .map_err(|error| error.to_string())?;
    for run in [&acme_run, &globex_run] {
        assert!(
            matches!(run.disposition(), Disposition::Succeeded),
            "both tenants succeed"
        );
        assert_eq!(run.output(), Some(&7u64), "both answer the same value");
    }
    assert_ne!(acme.tenant(), globex.tenant(), "the tenants stay distinct");
    Ok(())
}

#[test]
fn a_store_that_cannot_open_is_a_typed_refusal() -> TestResult {
    let scratch = Scratch::new("estate").map_err(|error| error.to_string())?;
    let blocker = scratch.path().join("blocker");
    std::fs::write(&blocker, b"a file, not a directory").map_err(|error| error.to_string())?;
    match Estate::durable("acme", 8, &blocker) {
        Err(EstateError::Store { .. }) => Ok(()),
        Err(error) => Err(format!("an unopenable store is Store, not {error}")),
        Ok(_) => Err("an unopenable store must not build".to_owned()),
    }
}

#[test]
fn a_synchronous_shutdown_inside_a_runtime_is_a_typed_refusal() -> TestResult {
    let estate = Estate::supervised("acme", 8).map_err(|error| error.to_string())?;
    let runtime = Runtime::new().map_err(|error| error.to_string())?;
    let outcome = runtime.block_on(async { estate.shutdown_blocking() });
    match outcome {
        Err(HostError::InsideRuntime) => Ok(()),
        Err(error) => Err(format!(
            "a sync shutdown inside a runtime is InsideRuntime, not {error}"
        )),
        Ok(_) => Err("a sync shutdown inside a runtime must not drain".to_owned()),
    }
}

#[test]
fn a_ceiling_past_the_hosts_bound_is_a_typed_refusal() -> TestResult {
    match Estate::supervised("acme", usize::MAX) {
        Err(EstateError::Host { .. }) => Ok(()),
        Err(error) => Err(format!("a bound past the ceiling is Host, not {error}")),
        Ok(_) => Err("a bound past the ceiling must not build".to_owned()),
    }
}

#[test]
fn a_zero_bound_is_a_typed_refusal() -> TestResult {
    match Estate::supervised("acme", 0) {
        Err(EstateError::Host { .. }) => Ok(()),
        Err(error) => Err(format!("a zero bound is Host, not {error}")),
        Ok(_) => Err("a zero bound must not build".to_owned()),
    }
}
