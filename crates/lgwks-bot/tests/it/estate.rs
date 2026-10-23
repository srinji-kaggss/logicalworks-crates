//! The estate front door: tiers, reach, refusals, drop, cancel, two tenants.
//!
//! Each claim in `rt/estate.rs` has a test here that fails when the claim is false. The real-world
//! failure behind the lifecycle rows is tokio's "Cannot drop a runtime in a context where blocking
//! is not allowed", which a multi-thread runtime raises when it is dropped from async code; the
//! case row is the default case-insensitive volume of macOS (APFS) and Windows (NTFS), where two
//! tenant names that differ only by case name one file.
//!
//! | Test | What it pins |
//! |---|---|
//! | `three_tiers_declare_what_they_own` | each constructor owns exactly its pieces, and the estate is `Send + Sync` |
//! | `a_run_succeeds_and_the_shutdown_report_accounts_for_it` | a run is admitted by the host and counted on the report |
//! | `a_task_placed_on_the_supervisor_is_counted_and_drained` | the supervisor is reachable and its work is on the report |
//! | `cancel_stops_a_run_and_a_supervised_task` | cancel refuses a later run and stops a parked supervised body |
//! | `a_synchronous_shutdown_inside_a_runtime_is_refused_and_keeps_the_estate` | the refusal changes nothing, and `shutdown` then finishes |
//! | `an_estate_dropped_inside_a_runtime_never_panics` | in-process (with a built reactor) and supervised drop inside async |
//! | `durable::*` | the `ephemeral` tier: recorded steps, reopen, refusals inside async, case twins, modes, and the smoke binary's exit |

use lgwks_bot::rt::estate::{Estate, EstateError};
use lgwks_bot::rt::runtime::Runtime;
use lgwks_bot::task::{Disposition, HostError, task};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Prove at compile time that an estate can be shared across threads.
const fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn three_tiers_declare_what_they_own() -> TestResult {
    const { assert_send_sync::<Estate>() };
    let driver = Runtime::new()?;

    let mut bare = Estate::in_process("acme")?;
    assert!(bare.handle().is_none(), "in-process carries no runtime");
    assert!(
        bare.supervisor().is_none(),
        "in-process carries no supervisor"
    );
    assert!(bare.run_store().is_none(), "in-process carries no store");
    assert_eq!(bare.tenant(), "acme", "the estate runs for its tenant");
    assert_eq!(bare.tier(), "in-process", "the tier reads as declared");
    assert_eq!(
        bare.host().tenant().as_str(),
        "acme",
        "the host is the estate's own"
    );

    let mut supervised = Estate::supervised("acme", 8)?;
    assert!(
        supervised.handle().is_some(),
        "supervised carries a runtime"
    );
    assert!(
        supervised.supervisor().is_some(),
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
    assert_eq!(
        supervised.host().limits().max_concurrent_tasks(),
        8,
        "the host admits under the ceiling the estate declared"
    );
    let report = driver.block_on(supervised.shutdown());
    assert_eq!(report.tenant(), "acme", "the report names its tenant");
    assert_eq!(report.tier(), "supervised", "the report names its tier");
    Ok(())
}

#[test]
fn an_empty_tenant_is_a_typed_refusal() -> TestResult {
    match Estate::in_process("") {
        Err(EstateError::InvalidTenant { .. }) => {}
        Err(error) => return Err(format!("an empty tenant is InvalidTenant, not {error}").into()),
        Ok(_) => return Err("an empty tenant must not build".into()),
    }
    match Estate::supervised("", 8) {
        Err(EstateError::InvalidTenant { .. }) => Ok(()),
        Err(error) => Err(format!("an empty tenant is InvalidTenant, not {error}").into()),
        Ok(_) => Err("an empty tenant must not build".into()),
    }
}

#[test]
fn a_run_succeeds_and_the_shutdown_report_accounts_for_it() -> TestResult {
    let mut estate = Estate::supervised("acme", 8)?;
    let probe = task("estate-probe", |_scope, input: u64| async move {
        Ok(input.saturating_add(1))
    })?;
    let run = estate.run_blocking(&probe, 41)?;
    assert!(
        matches!(run.disposition(), Disposition::Succeeded),
        "the run succeeds"
    );
    assert_eq!(run.output(), Some(&42u64), "the run answers its value");
    let report = estate.shutdown_blocking()?;
    assert_eq!(report.tenant(), "acme", "the report names its tenant");
    assert_eq!(report.tier(), "supervised", "the report names its tier");
    assert_eq!(report.runs_admitted(), 1, "the host admitted the run");
    assert_eq!(report.runs_refused(), 0, "nothing was refused");
    assert_eq!(
        report.supervised().spawned,
        0,
        "a run through the host is not a task placed on the supervisor"
    );
    Ok(())
}

#[test]
fn a_task_placed_on_the_supervisor_is_counted_and_drained() -> TestResult {
    let mut estate = Estate::supervised("acme", 4)?;
    let handle = estate.handle().ok_or("a supervised estate has a runtime")?;
    let supervisor = estate
        .supervisor()
        .ok_or("a supervised estate has a supervisor")?;
    handle.block_on(supervisor.spawn(|_token| async {}));
    let report = estate.shutdown_blocking()?;
    let drained = report.supervised();
    assert_eq!(drained.spawned, 1, "the task was placed on the supervisor");
    assert_eq!(drained.completed, 1, "and the drain accounted for it");
    assert_eq!(
        drained.succeeded.saturating_add(drained.cancelled),
        1,
        "it returned on its own, before or after the stop"
    );
    assert_eq!(drained.refused, 0, "nothing was refused");
    assert_eq!(drained.panicked, 0, "nothing panicked");
    assert!(
        estate.supervisor().is_none() && estate.handle().is_none(),
        "a drained estate is closed"
    );
    Ok(())
}

#[test]
fn cancel_stops_a_run_and_a_supervised_task() -> TestResult {
    let mut estate = Estate::supervised("acme", 4)?;
    let probe = task("estate-probe", |_scope, input: u64| async move {
        Ok(input.saturating_add(1))
    })?;
    let handle = estate.handle().ok_or("a supervised estate has a runtime")?;
    let supervisor = estate
        .supervisor()
        .ok_or("a supervised estate has a supervisor")?;
    handle.block_on(supervisor.spawn(|token| async move {
        token.cancelled().await;
    }));
    estate.cancel();
    let run = estate.run_blocking(&probe, 1)?;
    assert_eq!(
        run.disposition(),
        Disposition::Refused,
        "a run asked for after the stop is refused at admission"
    );
    let report = estate.shutdown_blocking()?;
    assert_eq!(
        report.supervised().cancelled,
        1,
        "the parked body observed its token and returned because of it"
    );
    assert_eq!(report.supervised().aborted, 0, "nothing needed the abort");
    assert_eq!(report.runs_refused(), 1, "the report counts the refusal");
    assert_eq!(report.runs_admitted(), 0, "and no run was admitted");
    Ok(())
}

#[test]
fn two_tenants_run_one_task_name_without_sharing() -> TestResult {
    let acme = Estate::in_process("acme")?;
    let globex = Estate::in_process("globex")?;
    let probe = task("shared-probe", |_scope, input: u64| async move {
        Ok(input.saturating_add(1))
    })?;
    let acme_run = acme.run_blocking(&probe, 6)?;
    let globex_run = globex.run_blocking(&probe, 6)?;
    for run in [&acme_run, &globex_run] {
        assert!(
            matches!(run.disposition(), Disposition::Succeeded),
            "both tenants succeed"
        );
        assert_eq!(run.output(), Some(&7u64), "both answer the same value");
    }
    assert_eq!(acme_run.tenant().as_str(), "acme", "each report is its own");
    assert_eq!(globex_run.tenant().as_str(), "globex", "and only its own");
    assert_eq!(
        acme.host().admission().admitted(),
        1,
        "acme counted its run"
    );
    assert_eq!(
        globex.host().admission().admitted(),
        1,
        "globex counted its run"
    );
    Ok(())
}

#[test]
fn a_synchronous_shutdown_inside_a_runtime_is_refused_and_keeps_the_estate() -> TestResult {
    let mut estate = Estate::supervised("acme", 8)?;
    let runtime = Runtime::new()?;
    let (refused, kept, report) = runtime.block_on(async {
        let refused = matches!(estate.shutdown_blocking(), Err(HostError::InsideRuntime));
        let kept = estate.supervisor().is_some() && estate.handle().is_some();
        (refused, kept, estate.shutdown().await)
    });
    assert!(refused, "a sync shutdown inside a runtime is InsideRuntime");
    assert!(kept, "the refusal left the supervisor and runtime in place");
    assert_eq!(report.tier(), "supervised", "and shutdown finished the job");
    Ok(())
}

#[test]
fn an_estate_dropped_inside_a_runtime_never_panics() -> TestResult {
    let probe = task("estate-probe", |_scope, input: u64| async move {
        Ok(input.saturating_add(1))
    })?;
    // `run_blocking` on the in-process tier builds the host's own reactor: the runtime whose plain
    // drop inside async is the panic.
    let bare = Estate::in_process("acme")?;
    bare.run_blocking(&probe, 1)?;
    let supervised = Estate::supervised("acme", 4)?;
    supervised.run_blocking(&probe, 1)?;
    let runtime = Runtime::new()?;
    runtime.block_on(async move {
        drop(bare);
        drop(supervised);
    });
    Ok(())
}

#[test]
fn a_ceiling_past_the_hosts_bound_is_a_typed_refusal() -> TestResult {
    match Estate::supervised("acme", usize::MAX) {
        Err(EstateError::Host { .. }) => Ok(()),
        Err(error) => Err(format!("a bound past the ceiling is Host, not {error}").into()),
        Ok(_) => Err("a bound past the ceiling must not build".into()),
    }
}

#[test]
fn a_zero_bound_is_a_typed_refusal() -> TestResult {
    match Estate::supervised("acme", 0) {
        Err(EstateError::Host { .. }) => Ok(()),
        Err(error) => Err(format!("a zero bound is Host, not {error}").into()),
        Ok(_) => Err("a zero bound must not build".into()),
    }
}

/// The `ephemeral` tier: a durable run mints its identity from that entropy source, so this
/// constructor exists only with it.
#[cfg(feature = "ephemeral")]
mod durable {
    use lgwks_bot::rt::estate::{Estate, EstateError};
    use lgwks_bot::rt::runtime::Runtime;
    use lgwks_bot::script::{FlowError, remember};
    use lgwks_bot::task::{Disposition, task};

    use crate::scratch::Scratch;

    use super::TestResult;

    #[test]
    fn a_durable_estate_records_a_step_and_a_reopen_reads_it() -> TestResult {
        let scratch = Scratch::new("estate")?;
        let step = task("estate-step", |scope, input: u64| async move {
            remember(&scope, "double", move || async move {
                Ok::<_, FlowError>(input.saturating_mul(2))
            })
            .await
        })?;
        let mut first = Estate::durable("acme", 8, scratch.path())?;
        assert_eq!(first.tier(), "durable", "the tier reads as declared");
        let run = first.run_blocking(&step, 21)?;
        assert_eq!(
            run.disposition(),
            Disposition::Succeeded,
            "a durable run succeeds: {:?}",
            run.error()
        );
        assert_eq!(run.output(), Some(&42u64), "and answers its value");
        let id = run.run_id().ok_or("a durable run names its run")?;
        let store = first.run_store().ok_or("a durable estate has a store")?;
        assert_eq!(store.record_count(id), 1, "the step was recorded");
        let report = first.shutdown_blocking()?;
        assert_eq!(report.tier(), "durable", "the report names its tier");
        assert_eq!(report.runs_admitted(), 1, "the report accounts for the run");
        drop(first);

        let second = Estate::durable("acme", 8, scratch.path())?;
        let reopened = second.run_store().ok_or("a reopened estate has a store")?;
        assert_eq!(
            reopened.record_count(id),
            1,
            "the record survives the process that wrote it"
        );
        Ok(())
    }

    #[test]
    fn a_store_that_cannot_open_is_a_typed_refusal_even_inside_a_runtime() -> TestResult {
        let scratch = Scratch::new("estate")?;
        let blocker = scratch.path().join("blocker");
        std::fs::write(&blocker, b"a file, not a directory")?;
        let foreign = scratch.path().join("foreign");
        std::fs::create_dir(&foreign)?;
        std::fs::write(foreign.join("acme.runstore"), b"not a run store")?;
        // Inside a runtime, where dropping a runtime that a refused build had already made is the
        // panic: both refusals must come back as values.
        let runtime = Runtime::new()?;
        let (unopenable, foreign_file) = runtime.block_on(async {
            (
                Estate::durable("acme", 8, &blocker).err(),
                Estate::durable("acme", 8, &foreign).err(),
            )
        });
        match unopenable {
            Some(EstateError::Store { .. }) => {}
            Some(error) => return Err(format!("an unopenable store is Store, not {error}").into()),
            None => return Err("an unopenable store must not build".into()),
        }
        match foreign_file {
            Some(EstateError::Store { .. }) => Ok(()),
            Some(error) => Err(format!("a foreign file is Store, not {error}").into()),
            None => Err("a foreign file must not build".into()),
        }
    }

    #[test]
    fn a_durable_estate_dropped_inside_a_runtime_never_panics() -> TestResult {
        let scratch = Scratch::new("estate")?;
        let probe = task("estate-probe", |_scope, input: u64| async move {
            Ok(input.saturating_add(1))
        })?;
        let durable = Estate::durable("acme", 4, scratch.path())?;
        durable.run_blocking(&probe, 1)?;
        let runtime = Runtime::new()?;
        runtime.block_on(async move { drop(durable) });
        Ok(())
    }

    #[test]
    fn a_case_variant_tenant_never_shares_a_store_file() -> TestResult {
        let scratch = Scratch::new("estate")?;
        let lower = Estate::durable("acme", 8, scratch.path())?;
        let lower_path = lower
            .run_store()
            .ok_or("a durable estate has a store")?
            .path()
            .to_path_buf();
        assert_eq!(
            lower_path.file_name().and_then(|name| name.to_str()),
            Some("acme.runstore"),
            "the estate's idea of the store's name is `RunStore::open_in`'s"
        );
        // Whether this volume tells the two cases apart is a fact about the volume: probe it.
        std::fs::write(scratch.path().join("CASE-PROBE"), b"x")?;
        let folds = scratch.path().join("case-probe").exists();
        match Estate::durable("ACME", 8, scratch.path()) {
            Err(EstateError::Store { .. }) if folds => Ok(()),
            Ok(upper) if !folds => {
                let upper_path = upper
                    .run_store()
                    .ok_or("a durable estate has a store")?
                    .path();
                assert_ne!(
                    upper_path,
                    lower_path.as_path(),
                    "a case-sensitive volume gives each tenant its own file"
                );
                Ok(())
            }
            Err(error) => Err(format!("unexpected refusal (folds={folds}): {error}").into()),
            Ok(_) => Err("a case twin must not share the first tenant's file".into()),
        }
    }

    #[test]
    fn the_smoke_binary_proves_the_evening() -> TestResult {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_estate")).output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "the smoke exits clean: {stdout} {}",
            String::from_utf8_lossy(&output.stderr)
        );
        for line in [
            "in-process drop inside a runtime: ok",
            "durable drain: admitted=1",
            "refused: an unopenable store",
        ] {
            assert!(
                stdout.contains(line),
                "the smoke printed {line:?}: {stdout}"
            );
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_new_store_is_owner_only() -> TestResult {
        use std::os::unix::fs::PermissionsExt as _;

        let scratch = Scratch::new("estate")?;
        let dir = scratch.path().join("tenants");
        let estate = Estate::durable("acme", 8, &dir)?;
        let store = estate
            .run_store()
            .ok_or("a durable estate has a store")?
            .path()
            .to_path_buf();
        let file_mode = std::fs::metadata(&store)?.permissions().mode() & 0o777;
        let dir_mode = std::fs::metadata(&dir)?.permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600, "step values are not world-readable");
        assert_eq!(dir_mode, 0o700, "a directory the estate made is private");
        Ok(())
    }
}
