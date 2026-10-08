//! Reference solution — `recovery`, SCRIPT arm (the `script!` language).
//!
//! The units loop is a flow: `for` over the world's width, each unit awaited
//! in turn, the first failure returning early with its index. The effect
//! applies once by reading the ledger before applying, in plain Rust beside
//! the flow. The plain `recover` keeps the derived run identity, the host and
//! the run-then-resume orchestration; the flow keeps the per-unit work.
//!
//! Two deliberate differences from the `new` arm, both load-bearing. The
//! failure index travels in the flow's own outcome enum rather than a shared
//! `Option` cell, and the "no failure recorded" sentinel is `u32::MAX`
//! rather than `None`: a second spelling of the same contract, so a defect in
//! one spelling shows up as a divergence rather than a shared blind spot.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ai_task_support::recovery::{Ledger, World};
use lgwks_bot::effect::RunId;
use lgwks_bot::script::Scope;
use lgwks_bot::task::{Disposition, Host, task};

/// The task's fixed recovery error.
pub use ai_task_support::recovery::RecoveryError;

/// The tenant every recovered run is booked under.
const TENANT: &str = "recovery";

/// The effect name the ledger is asked to apply exactly once.
const EFFECT: &str = "recover";

/// The run identity, derived rather than minted.
///
/// Any stable non-zero 32-hex value works: both attempts derive the same run
/// so the second finds the first attempt's records. A freshly minted identity
/// per call would derive a different run every time and the resume would find
/// nothing.
const RUN: &str = "7e4b1c90d2f848632a7e5b4c9d1f3a01";

/// Which unit failed, or that none has: `u32::MAX` means no failure recorded.
type FailedUnit = Arc<Mutex<u32>>;

/// Note the first unit that failed, leaving a recorded failure alone.
fn note_failure(failed: &Mutex<u32>, index: u32) {
    let mut guard = match failed.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if *guard == u32::MAX {
        *guard = index;
    }
}

lgwks_bot::script! {
    /// Run every unit in turn, stopping at the first failure.
    flow work_units(world: ai_task_support::recovery::World, failed: std::sync::Arc<std::sync::Mutex<u32>>) -> u64:
        let mut total = 0u64
        for index in 0..world.width():
            let outcome = world.unit(&scope, index).await
            if outcome.is_err():
                note_failure(&failed, index)
                fail with "unit failed"
            total = total.saturating_add(outcome.or_fail()?)
        let ledger = world.ledger().clone()
        if !ledger.applied("recover"):
            ledger.apply("recover")
        give back total
}

/// Read one attempt's outcome into the caller's error type.
fn settled(report: &lgwks_bot::task::Report<u64>, failed: &FailedUnit) -> Result<u64, RecoveryError> {
    if report.disposition().is_success() {
        return report.output().copied().ok_or(RecoveryError::NoLedger);
    }
    if report.disposition() == Disposition::DeadlineExceeded {
        return Err(RecoveryError::Deadline);
    }
    let recorded = match failed.lock() {
        Ok(guard) => *guard,
        Err(poisoned) => *poisoned.into_inner(),
    };
    if recorded == u32::MAX {
        return Err(RecoveryError::NoStore);
    }
    Err(RecoveryError::Unit { index: recorded })
}

pub async fn recover(
    world: World,
    store_dir: PathBuf,
    deadline: Duration,
) -> Result<u64, RecoveryError> {
    let run = RunId::from_hex(RUN).map_err(|_| RecoveryError::NoStore)?;
    let host = Host::builder(TENANT)
        .map_err(|_| RecoveryError::NoStore)?
        .default_deadline(deadline)
        .run_store(&store_dir)
        .map_err(|_| RecoveryError::NoStore)?
        .build()
        .map_err(|_| RecoveryError::NoStore)?;
    let failed: FailedUnit = Arc::new(Mutex::new(u32::MAX));
    let failed_for_settled = Arc::clone(&failed);
    let work = task("flow-units", move |scope: Scope, _attempt: u32| {
        let world = world.clone();
        let failed = Arc::clone(&failed);
        async move { work_units(&scope, world, failed).await }
    })
    .map_err(|_| RecoveryError::NoStore)?;

    // The first attempt. Whatever it reaches, its records are already on the
    // disk, so the resume below can find them.
    let first = host.run(&work, 0u32).await;
    let done = settled(&first, &failed_for_settled);
    if done.is_ok() {
        return done;
    }

    // The second attempt, under the same derived run: recorded units replay
    // without their bodies running again, and the units the first attempt did
    // not reach run now.
    let resumed = host.resume(run, &work, 0u32).await;
    settled(&resumed, &failed_for_settled)
}
