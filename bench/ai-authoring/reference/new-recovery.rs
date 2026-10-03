//! Reference solution — `recovery` (NEW API: `lgwks_bot::task` + `script`).
//!
//! There is no OLD-API arm for this task, and the reason is architectural
//! rather than a gap in the harness: the old `lgwks_bot::rt` surface has no
//! durable run store, no `remember`, no run identity and no resume. Nothing in
//! it can express "finish the work without redoing a completed unit", because
//! it keeps no record of a completed unit to consult. A hand-rolled file
//! standing in for that store would measure the fixture the model wrote rather
//! than the crate.
//!
//! Four things have to be right, and this file shows each:
//!
//! 1. **A run identity that is stable across attempts.** [`RunId::from_hex`] over
//!    a digest of the tenant, so every attempt derives the same run and a second
//!    call finds the first call's records. A freshly minted identity per call
//!    would derive a *different* run every time and the resume would find
//!    nothing — the plausible first mistake this task exists to catch.
//! 2. **A durable store installed on the host**, so `remember` has somewhere to
//!    land and `Host::resume` has records to replay.
//! 3. **A helper that builds the task body once**, reused by both attempts. The
//!    step key is derived from the step *path*, so two attempts that spelled the
//!    step differently would record under different keys and never replay.
//! 4. **The effect applied once**, by reading [`Ledger::applied`] before applying.
//!
//! The interruption itself is the harness's business. This code never learns that
//! a first attempt was killed — it is simply correct when `recover` is called
//! again.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use ai_task_support::recovery::{Ledger, UnitError, World};
use lgwks_bot::effect::RunId;
use lgwks_bot::script::{FlowError, Scope};
use lgwks_bot::task::{Disposition, Host, task};

#[derive(Debug)]
pub enum RecoveryError {
    Unit { index: u32 },
    Deadline,
    NoStore,
    NoLedger,
}

/// The tenant every recovered run is booked under.
const TENANT: &str = "recovery";

/// The effect name the ledger is asked to apply exactly once.
const EFFECT: &str = "recover";

/// The run identity, derived rather than minted.
///
/// `RunId::mint` needs the estate's entropy source, and a fresh value would be
/// exactly wrong here: a second attempt has to derive the *same* run to find the
/// first attempt's records. So the identity is a digest of the tenant alone,
/// spelled out as a constant rather than recomputed at each call — which also
/// means this file carries no hashing for a solution to get subtly wrong.
const RUN: &str = "0f9d3a17c25b4e806b1de4372f5a9c60";

/// The body every unit runs under.
///
/// `World::unit` enters its own child scope per index, and a scope's key is its
/// *path*, so each unit is already independently keyed. This name is the parent
/// both attempts share, which is what makes the replay find what the first
/// attempt wrote.
const STEP: &str = "units";

/// Which unit failed, read back out of the body's own record.
///
/// Kept beside the failure rather than inside the error type: a task body returns
/// `FlowError`, so the unit index has to travel out-of-band for `recover` to name
/// it in `RecoveryError::Unit { index }`.
type Failed = Arc<Mutex<Option<u32>>>;

/// The future a task body returns, named once so both the task's type parameter
/// and the body that builds it are spelled against the same type.
type Body = std::pin::Pin<Box<dyn std::future::Future<Output = Result<u64, FlowError>>>>;

/// The helper: the one task body both attempts run.
///
/// Built by a function rather than written twice, because "the same task on both
/// attempts" is a property of this function returning the same task. Two
/// hand-written bodies that happen to agree today are two bodies that can drift
/// tomorrow, and that drift would show up as a solution that mysteriously fails
/// to replay rather than as a compile error.
fn recovery_task(
    world: World,
    failed: Failed,
) -> Result<lgwks_bot::task::Task<impl Fn(Scope, u32) -> Body>, RecoveryError> {
    let width = world.width();
    task(STEP, move |scope: Scope, _attempt: u32| -> Body {
        let world = world.clone();
        let failed = Arc::clone(&failed);
        Box::pin(async move {
            let mut total: u64 = 0;
            for index in 0..width {
                match world.unit(&scope, index).await {
                    Ok(value) => total = total.saturating_add(value),
                    Err(UnitError::Unit { index }) => {
                        *failed.lock().unwrap_or_else(PoisonError::into_inner) = Some(index);
                        return Err(FlowError::failed(format!("unit {index} failed")));
                    }
                }
            }
            // The exactly-once half. Read the ledger before applying: a replayed
            // run finds the effect already applied and skips it, so the count
            // stays at one across any number of attempts.
            let ledger: Ledger = world.ledger().clone();
            if !ledger.applied(EFFECT) {
                ledger.apply(EFFECT);
            }
            Ok(total)
        })
    })
    .map_err(|_| RecoveryError::NoStore)
}

/// A host over `store_dir` for `TENANT`, carrying `deadline`.
///
/// One function so both attempts build the same host over the same directory:
/// the store is opened — and its existing records replayed — at
/// [`HostBuilder::run_store`](lgwks_bot::task::HostBuilder::run_store), so the
/// second attempt is the one that finds the first attempt's records.
fn recovery_host(store_dir: &std::path::Path, deadline: Duration) -> Result<Host, RecoveryError> {
    Host::builder(TENANT)
        .map_err(|_| RecoveryError::NoStore)?
        .default_deadline(deadline)
        .run_store(store_dir)
        .map_err(|_| RecoveryError::NoStore)?
        .build()
        .map_err(|_| RecoveryError::NoStore)
}

/// Read the outcome of one attempt into the caller's own error type.
fn into_result(report: &lgwks_bot::task::Report<u64>, failed: &Failed) -> Result<u64, RecoveryError> {
    match report.disposition() {
        Disposition::Succeeded => report
            .output()
            .copied()
            .ok_or(RecoveryError::NoLedger),
        Disposition::DeadlineExceeded => Err(RecoveryError::Deadline),
        _ => {
            let index = failed.lock().unwrap_or_else(PoisonError::into_inner);
            Err(match *index {
                Some(index) => RecoveryError::Unit { index },
                None => RecoveryError::NoStore,
            })
        }
    }
}

pub async fn recover(
    world: World,
    store_dir: PathBuf,
    deadline: Duration,
) -> Result<u64, RecoveryError> {
    let run = RunId::from_hex(RUN).map_err(|_| RecoveryError::NoStore)?;
    let host = recovery_host(&store_dir, deadline)?;
    let failed: Failed = Arc::new(Mutex::new(None));
    // The world is cloned into the task and kept here too: the caller reads the
    // ledger from the world it handed us, so this frame still owns one.
    let work = recovery_task(world.clone(), Arc::clone(&failed))?;

    // The first attempt. Whatever it reaches, its records are already on the disk
    // by the time `remember` returned, so the resume below can find them.
    let first = host.run(&work, 0u32).await;
    let settled = into_result(&first, &failed);
    if settled.is_ok() {
        return settled;
    }

    // The second attempt, under the same derived run: every unit the first
    // attempt recorded replays without its body running again, and the units it
    // did not reach run now. The task, the scope and the run identity are the
    // same on both attempts, which is the whole reason this compiles into a
    // resume rather than a fresh run.
    let resumed = host.resume(run, &work, 0u32).await;
    let settled = into_result(&resumed, &failed);
    settled?;
    // The ledger is the harness's, so the caller reads it there; here it is only
    // reached to keep the borrow of `world` in this frame.
    let _ = world.ledger().applies();
    Ok(resumed.output().copied().ok_or(RecoveryError::NoLedger)?)
}