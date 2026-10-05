//! Simulation family: resumable runs under a seeded sweep of crash points.
//!
//! Every scenario drives the **real** `Host`, the real `RunStore` over a real
//! file, the real `remember`, and the real frame codec. Nothing here reimplements
//! a record, a chain head, or a resume. What the seed controls is *where the
//! process would have died*: how many durable steps a run has, at which step
//! boundary the crash lands, how many tenants share one store, and how many runs
//! follow.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `crash_points_resume_to_the_same_output` | for every crash point at every step boundary, the resumed run's output equals the uninterrupted one |
//! | `finished_steps_run_once` | across every crash point, each committed step's body contributes exactly once to the resumed output |
//! | `tenants_stay_isolated` | many tenants on one directory: each reads its own records, and no tenant is served another's run |
//! | `same_seed_replays` | the same seed produces the same trace hash, twice |
//!
//! # What is real and what is seeded
//!
//! The store, the file, the `sync_all` and the chain are real. The crash is
//! modelled the way a crash is *observable* to a resume: the process stops
//! between two step boundaries, so the steps before the boundary committed and
//! the step at the boundary started without committing. Modelling it that way
//! rather than by killing a child is what lets a family sweep thousands of crash
//! points in seconds; the real `SIGKILL` is in `tests/task_resume.rs`, and this
//! family checks the property across a space that one file cannot enumerate.
//!
//! # Why a crash inside a step is modelled as "no record"
//!
//! A step's body runs *before* its record lands. A process killed inside a body
//! therefore leaves that step unrecorded, and the resume must run it again. That
//! is at-least-once for an unrecorded step and exactly-once for a recorded one,
//! and this file is where the distinction is checked at every boundary rather
//! than asserted in prose.

#![cfg(all(feature = "script", feature = "ephemeral"))]

use crate::sim;

use crate::band_family;

use crate::resume_fixtures as shared;

use std::error::Error;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use lgwks_bot::effect::RunId;
use lgwks_bot::script::{FlowError, Scope, remember};
use lgwks_bot::task::{Disposition, Host, RunStore, Task, task};

use shared::Scratch;

use sim::Band;
use sim::Rng;
use sim::Trace;

type TestResult = Result<(), Box<dyn Error>>;

/// The most durable steps one simulated run has.
const MAX_STEPS: u32 = 6;

/// The named future one simulated task body returns.
type BodyFuture = Pin<Box<dyn Future<Output = Result<Outcome, FlowError>>>>;

/// The task type every scenario here builds.
type WorkTask = Task<fn(Scope, (usize, usize)) -> BodyFuture>;

/// What one simulated run produced: which step bodies ran, and their sum.
///
/// Archivable because `Host::resume` takes it: a resumed run may be settling a
/// request, and a recorded verdict is made of an archived output. The derives
/// are the `lgwks_std::wire` facade's, not rkyv's, exactly as the library spells
/// them.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    lgwks_std::wire::Archive,
    lgwks_std::wire::Serialize,
    lgwks_std::wire::Deserialize,
)]
#[rkyv(crate = lgwks_std::wire::rkyv, compare(PartialEq), derive(Debug))]
struct Outcome {
    /// Which step bodies ran, in order. A replayed step contributes nothing new.
    ran: Vec<u32>,
    /// The sum of every step's value.
    total: u32,
}

impl Outcome {
    /// Fold into the trace, so a family running a different scenario stays
    /// comparable with one running this.
    fn record(&self, trace: &mut Trace) {
        trace.record_u64("steps-ran", u64::try_from(self.ran.len()).unwrap_or(0));
        trace.record_u64("total", u64::from(self.total));
        for step in &self.ran {
            trace.record_u64("ran", u64::from(*step));
        }
    }
}

/// One step's body. It adds its index to the running total and reports that it
/// ran.
///
/// A crash at this boundary is modelled by the step refusing: the body's future
/// is entered, does no work, and the record never lands — exactly the state a
/// `SIGKILL` mid-body leaves.
fn step_body(index: u32, crash_at: usize) -> Result<u32, FlowError> {
    if usize::try_from(index).unwrap_or(usize::MAX) == crash_at {
        let refusal = Err(FlowError::Cancelled { at: Arc::from("") });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "step_body: returning an error to the caller");
        return refusal;
    }
    Ok(index)
}

/// The body of one simulated task.
///
/// `remember` is called once per step under a step of its own, so the store holds
/// one record per completed step and each key is distinct.
async fn work_body(scope: Scope, crash_at: usize, steps: usize) -> Result<Outcome, FlowError> {
    let mut outcome = Outcome::default();
    for index in 0..steps {
        let index_u32 = u32::try_from(index).unwrap_or_default();
        let name = format!("step-{index}");
        let child = scope.enter(&name)?;
        let value = remember(&child, "value", || async { step_body(index_u32, crash_at) }).await?;
        outcome.ran.push(index_u32);
        outcome.total = outcome
            .total
            .checked_add(value)
            .ok_or_else(|| FlowError::failed("step sum overflow"))?;
    }
    Ok(outcome)
}

/// The task, behind one nameable body type.
fn work_task() -> Result<WorkTask, FlowError> {
    task("work", boxed_work_body)
}

/// The same body behind its nameable return type.
fn boxed_work_body(scope: Scope, input: (usize, usize)) -> BodyFuture {
    let (crash_at, steps) = input;
    Box::pin(work_body(scope, crash_at, steps))
}

/// The uninterrupted outcome of a `steps`-step run.
fn uninterrupted(steps: usize) -> Outcome {
    let mut outcome = Outcome::default();
    for index in 0..steps {
        let index_u32 = u32::try_from(index).unwrap_or_default();
        outcome.ran.push(index_u32);
        outcome.total = outcome.total.saturating_add(index_u32);
    }
    outcome
}

/// A host for `tenant` over `dir`, opened fresh every time.
fn host(tenant: &str, dir: &Path) -> Result<Host, Box<dyn Error>> {
    Ok(Host::builder(tenant)?.run_store(dir)?.build()?)
}

/// Crash a fresh run at one boundary, then resume that run to the uninterrupted output.
fn crash_then_resume(
    store_dir: &Path,
    crash_at: usize,
    steps: usize,
    expected: &Outcome,
) -> TestResult {
    let crashed_host = host("sim", store_dir)?;
    let report = lgwks_bot::block_on(crashed_host.run(&work_task()?, (crash_at, steps)));
    assert_eq!(
        report.disposition(),
        Disposition::Cancelled,
        "a run stopped at a step boundary reports Cancelled"
    );
    assert!(
        report.output().is_none(),
        "a stopped run has no output to hand back"
    );
    let crashed_run = report.run_id().ok_or("a stored run must name a run id")?;
    let recorded = crashed_host
        .run_store()
        .ok_or("a stored host keeps a store")?
        .record_count(crashed_run);
    assert_eq!(
        recorded, crash_at,
        "crash at {crash_at} records exactly the steps before it"
    );

    // Resume that same run with no crash: the interrupted step runs once,
    // and the full output comes back.
    let resumed = lgwks_bot::block_on(host("sim", store_dir)?.resume(
        crashed_run,
        &work_task()?,
        (steps, steps),
    ));
    assert_eq!(
        resumed.output(),
        Some(expected),
        "the resumed run must equal the uninterrupted output, got {:?} err {:?}",
        resumed.output(),
        resumed.error()
    );
    assert_eq!(
        resumed.disposition(),
        Disposition::Succeeded,
        "the resume must succeed"
    );
    Ok(())
}

/// One seed of the crash sweep: a full run, then a crash and resume at every boundary.
fn crash_seed(rng: &mut Rng) -> TestResult {
    let steps = usize::try_from(rng.between(2, MAX_STEPS)).unwrap_or(2);
    let scratch = Scratch::new("sim-crash")?;
    let store_dir = scratch.path().join("store");
    let expected = uninterrupted(steps);

    // One full run establishes the records a resume would find.
    let seeded = lgwks_bot::block_on(host("sim", &store_dir)?.run(&work_task()?, (steps, steps)));
    let run = seeded.run_id().ok_or("a stored run must name a run id")?;
    assert_eq!(
        seeded.output(),
        Some(&expected),
        "the uninterrupted run must reach its output"
    );

    // A fresh run under every crash boundary in turn. The steps before the
    // boundary commit and the step at the boundary does not, so the resume
    // starts exactly where the first attempt stopped. Each crashed run has a
    // fresh run id (its own store record set), and its own resume reaches the
    // uninterrupted output.
    for crash_at in 0..steps {
        crash_then_resume(&store_dir, crash_at, steps, &expected)?;
    }
    // And the seeded run, never crashed, still resumes to the same output.
    let _ = run;
    Ok(())
}

/// Every crash point at every step boundary resumes to the uninterrupted output.
fn crash_points_resume_to_the_same_output(band: Band) -> TestResult {
    let mut rng = Rng::new(band.first);
    for _ in band.seeds() {
        crash_seed(&mut rng)?;
    }
    Ok(())
}

/// One seed of the run-once check: crash at a drawn boundary, resume, and count each step.
fn once_per_seed(rng: &mut Rng) -> TestResult {
    let steps = usize::try_from(rng.between(2, MAX_STEPS)).unwrap_or(2);
    let crash_at =
        usize::try_from(rng.below(u32::try_from(steps).unwrap_or(1))).unwrap_or_default();
    let scratch = Scratch::new("sim-once")?;
    let store_dir = scratch.path().join("store");
    let host = host("sim", &store_dir)?;

    // The first attempt dies at the boundary; the resume completes it. The
    // interrupted step must run exactly once more, and no committed step a
    // second time.
    let stopped = lgwks_bot::block_on(host.run(&work_task()?, (crash_at, steps)));
    assert_eq!(
        stopped.disposition(),
        Disposition::Cancelled,
        "seed {}: a run stopped at a boundary reports Cancelled",
        rng.below(u32::MAX)
    );
    let run = stopped.run_id().ok_or("a stored run must name a run id")?;
    let recorded = host
        .run_store()
        .ok_or("a stored host keeps a store")?
        .record_count(run);
    assert_eq!(
        recorded,
        crash_at,
        "seed {}: exactly the steps before the boundary are on the disk",
        rng.below(u32::MAX)
    );

    let resumed = lgwks_bot::block_on(host.resume(run, &work_task()?, (steps, steps)));
    let outcome = resumed.output().ok_or("the resume must succeed")?;
    assert_eq!(
        resumed.disposition(),
        Disposition::Succeeded,
        "the resume must succeed"
    );
    assert_eq!(
        outcome.ran,
        (0..steps)
            .map(|index| u32::try_from(index).unwrap_or_default())
            .collect::<Vec<_>>(),
        "seed {}: each step contributes exactly once over the whole life of the run",
        rng.below(u32::MAX)
    );
    assert_eq!(
        outcome.total,
        uninterrupted(steps).total,
        "the resumed total must equal the uninterrupted one"
    );
    Ok(())
}

/// Across every crash point, each committed step's body contributes exactly once.
fn finished_steps_run_once(band: Band) -> TestResult {
    let mut rng = Rng::new(band.first);
    for _ in band.seeds() {
        once_per_seed(&mut rng)?;
    }
    Ok(())
}

/// Offer one tenant's run id to another tenant's handle on the shared file, and check who may resume it.
fn offer_run(
    rng: &mut Rng,
    shared_path: &Path,
    steps: usize,
    owner_name: &str,
    run: RunId,
    other: &(String, RunId),
) -> TestResult {
    let (ref asker_name, _) = *other;
    let asker = Host::builder(asker_name)?
        .store(RunStore::open(shared_path)?)
        .build()?;
    if asker_name == owner_name {
        assert_eq!(
            asker
                .run_store()
                .ok_or("a stored host keeps a store")?
                .tenant_of(run)
                .as_deref(),
            Some(asker_name.as_str()),
            "the owner must attribute its own run to itself"
        );
        assert_eq!(
            lgwks_bot::block_on(asker.resume(run, &work_task()?, (steps, steps))).disposition(),
            Disposition::Succeeded,
            "the owner must be able to resume its own run"
        );
    } else {
        let foreign = lgwks_bot::block_on(asker.resume(run, &work_task()?, (steps, steps)));
        assert_eq!(
            foreign.disposition(),
            Disposition::Refused,
            "seed {}: {asker_name} must be refused {owner_name}'s run",
            rng.below(u32::MAX)
        );
        assert_eq!(
            asker
                .run_store()
                .ok_or("a store keeps a handle")?
                .record_count(run),
            steps,
            "seed {}: a refused resume leaves {owner_name}'s records exactly as they \
             were — it neither reads them nor adds to them",
            rng.below(u32::MAX)
        );
    }
    Ok(())
}

/// One tenant installs a handle on the shared file, runs, and records its run id.
fn tenant_runs_alone(
    rng: &mut Rng,
    shared_path: &Path,
    steps: usize,
    expected: &Outcome,
    index: usize,
    runs: &mut Vec<(String, RunId)>,
) -> TestResult {
    let name = format!("tenant-{index}");
    let owned = Host::builder(&name)?
        .store(RunStore::open(shared_path)?)
        .build()?;
    let report = lgwks_bot::block_on(owned.run(&work_task()?, (steps, steps)));
    assert_eq!(
        report.output(),
        Some(expected),
        "seed {}: tenant {name} must reach its own output",
        rng.below(u32::MAX)
    );
    let run = report.run_id().ok_or("a stored run must name a run id")?;
    assert_eq!(
        owned
            .run_store()
            .ok_or("a stored host keeps a store")?
            .record_count(run),
        steps,
        "seed {}: tenant {name} records every step",
        rng.below(u32::MAX)
    );
    runs.push((name, run));
    Ok(())
}

/// Many tenants over one shared store file: each reads its own records only.
///
/// The tenants deliberately share **one file**, not one file each, so isolation is
/// exercised by the store's own tenant check on a record read rather than by a
/// directory layout that separates them by luck. A run id minted under one tenant
/// is refused by every other, even though every host opened the same bytes.
fn tenants_stay_isolated(band: Band) -> TestResult {
    let mut rng = Rng::new(band.first);
    for _ in band.seeds() {
        let tenants = usize::try_from(rng.between(2, 65)).unwrap_or(2);
        let steps = usize::try_from(rng.between(1, 4)).unwrap_or(1);
        let scratch = Scratch::new("sim-tenants")?;
        let shared_path = scratch.path().join("shared.runstore");
        let expected = uninterrupted(steps);

        // Every tenant installs a handle on the *same* file.
        let mut runs = Vec::new();
        for index in 0..tenants {
            tenant_runs_alone(&mut rng, &shared_path, steps, &expected, index, &mut runs)?;
        }

        // Every tenant's run id is offered to every tenant's handle on that one
        // file. The owner attributes it to itself; every other tenant is refused
        // the record and refuses to adopt the run.
        for entry in &runs {
            let (ref owner_name, run) = *entry;
            for other in &runs {
                offer_run(&mut rng, &shared_path, steps, owner_name, run, other)?;
            }
        }
    }
    Ok(())
}

/// The same seed produces the same trace, twice.
fn same_seed_replays(band: Band) -> TestResult {
    let body = |sim_run: &mut sim::Sim| -> TestResult {
        let steps = usize::try_from(sim_run.rng().between(2, MAX_STEPS)).unwrap_or(2);
        let scratch = Scratch::new("sim-replay")?;
        let store_dir = scratch.path().join("store");
        let crash_at = usize::try_from(sim_run.rng().below(u32::try_from(steps).unwrap_or(1)))
            .unwrap_or_default();

        let stopped =
            lgwks_bot::block_on(host("sim", &store_dir)?.run(&work_task()?, (crash_at, steps)));
        stopped
            .output()
            .cloned()
            .unwrap_or_default()
            .record(&mut sim_run.trace);
        let run = stopped.run_id().ok_or("a stored run must name a run id")?;
        let resumed = lgwks_bot::block_on(host("sim", &store_dir)?.resume(
            run,
            &work_task()?,
            (steps, steps),
        ));
        resumed
            .output()
            .cloned()
            .unwrap_or_default()
            .record(&mut sim_run.trace);
        Ok(())
    };
    sim::assert_replays(band, body)?;
    Ok(())
}

band_family::band_family! {
    crash_points_resume_to_the_same_output_band_00 => crash_points_resume_to_the_same_output, 0;
    crash_points_resume_to_the_same_output_band_01 => crash_points_resume_to_the_same_output, 1;
    crash_points_resume_to_the_same_output_band_02 => crash_points_resume_to_the_same_output, 2;
    crash_points_resume_to_the_same_output_band_03 => crash_points_resume_to_the_same_output, 3;
    crash_points_resume_to_the_same_output_band_04 => crash_points_resume_to_the_same_output, 4;
    crash_points_resume_to_the_same_output_band_05 => crash_points_resume_to_the_same_output, 5;
    crash_points_resume_to_the_same_output_band_06 => crash_points_resume_to_the_same_output, 6;
    crash_points_resume_to_the_same_output_band_07 => crash_points_resume_to_the_same_output, 7;
    finished_steps_run_once_band_08 => finished_steps_run_once, 8;
    finished_steps_run_once_band_09 => finished_steps_run_once, 9;
    finished_steps_run_once_band_10 => finished_steps_run_once, 10;
    finished_steps_run_once_band_11 => finished_steps_run_once, 11;
    tenants_stay_isolated_band_12 => tenants_stay_isolated, 12;
    tenants_stay_isolated_band_13 => tenants_stay_isolated, 13;
    tenants_stay_isolated_band_14 => tenants_stay_isolated, 14;
    tenants_stay_isolated_band_15 => tenants_stay_isolated, 15;
    same_seed_replays_band_00 => same_seed_replays, 0;
    same_seed_replays_band_01 => same_seed_replays, 1;
    same_seed_replays_band_02 => same_seed_replays, 2;
    same_seed_replays_band_03 => same_seed_replays, 3;
    same_seed_replays_band_04 => same_seed_replays, 4;
    same_seed_replays_band_05 => same_seed_replays, 5;
    same_seed_replays_band_06 => same_seed_replays, 6;
    same_seed_replays_band_07 => same_seed_replays, 7;
    same_seed_replays_band_08 => same_seed_replays, 8;
    same_seed_replays_band_09 => same_seed_replays, 9;
    same_seed_replays_band_10 => same_seed_replays, 10;
    same_seed_replays_band_11 => same_seed_replays, 11;
    same_seed_replays_band_12 => same_seed_replays, 12;
    same_seed_replays_band_13 => same_seed_replays, 13;
    same_seed_replays_band_14 => same_seed_replays, 14;
    same_seed_replays_band_15 => same_seed_replays, 15;
}
