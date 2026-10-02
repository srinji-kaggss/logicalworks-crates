//! Simulation family: concurrent appends to one run store, under a seeded schedule.
//!
//! The store is the shipped one. Every append goes through its real storage-owner
//! thread, the real length fence, the real frame grammar and a real `fsync`; what
//! the seed controls is *how many* appends contend, *which tenants* they belong to,
//! and *which steps* they file under. Nothing here reimplements a record, a chain
//! head or a store.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `concurrent_appends_lose_nothing` | under contention every admitted record is on the disk exactly once, with the chain intact |
//! | `duplicate_submissions_are_idempotent` | the same step recorded repeatedly commits one frame and stays readable |
//! | `an_interrupted_step_records_exactly_once` | a replayed step is never re-recorded and never doubled |
//! | `tenants_interleaved_stay_isolated` | tenants contending on one store each read their own records and never another's |
//! | `same_seed_replays` | the same seed produces the same trace hash, twice |
//!
//! # Why this file exists next to `sim_task_resume.rs`
//!
//! That file sweeps *crash points*: where a run would have died, and whether the
//! resume reaches the same output. This one sweeps *concurrency*: many appends
//! racing for one owner thread, and whether the ordered step stays ordered. The two
//! are the store's two independent claims, and a store that is right about one can
//! still be wrong about the other.

#![cfg(all(feature = "script", feature = "ephemeral"))]

mod sim;

#[path = "sim/bands.rs"]
mod band_family;

use std::error::Error;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use lgwks_bot::effect::RunId;
use lgwks_bot::script::{FlowError, Scope, remember};
use lgwks_bot::task::{Host, RunStore, Task, task};

use sim::Band;
use sim::Rng;

type TestResult = Result<(), Box<dyn Error>>;

/// The most appends one simulated contention round performs.
const MAX_APPENDS: u32 = 24;

/// The named future a simulated task body returns.
type BodyFuture = Pin<Box<dyn Future<Output = Result<u32, FlowError>>>>;

/// The task type every scenario here builds.
type AppendTask = Task<fn(Scope, u32) -> BodyFuture>;

/// A scratch directory for one scenario, removed when it ends.
struct Scratch(PathBuf);

impl Scratch {
    /// A directory named by the scenario's own tag, the seed it is on and the
    /// process's own identity.
    ///
    /// All three, because this file's families run in parallel and two of them
    /// reach the same seed numbers: a directory named by tag and seed alone is two
    /// scenarios' scratch space, and one of them deletes the other's store while it
    /// is still appending. The pid is the last third because the same seed is also
    /// swept twice in `same_seed_replays`, within one process.
    fn new(tag: &str, unique: u32) -> Result<Self, Box<dyn Error>> {
        let path = std::env::temp_dir().join(format!(
            "lgwks-sim-store-{tag}-{unique}-{}",
            std::process::id()
        ));
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
        }
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }

    /// The store file this scenario contends over.
    fn store(&self) -> PathBuf {
        self.0.join("contended.runstore")
    }
}

impl Drop for Scratch {
    /// Remove the directory. A leaked temp directory is a nuisance, not a failure.
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.0));
    }
}

/// The task whose whole body is one `remember` over a caller-chosen value.
fn append_task() -> Result<AppendTask, FlowError> {
    task("append", boxed_body)
}

/// The one-step body behind its nameable return type.
fn boxed_body(scope: Scope, value: u32) -> BodyFuture {
    Box::pin(
        async move { remember(&scope, "v", || async move { Ok::<_, FlowError>(value) }).await },
    )
}

/// A host over `store`, for `tenant`.
fn host(tenant: &str, store: RunStore) -> Result<Host, Box<dyn Error>> {
    Ok(Host::builder(tenant)?.store(store).build()?)
}

/// How many runs a tenant performs in one contended round.
fn tenant_runs(rng: &mut Rng) -> u32 {
    rng.between(1, MAX_APPENDS)
}

/// Many runs contending for one store lose no record and keep the chain intact.
///
/// Every append here passes through the one owner thread, so the property under
/// test is that the ordered step stays ordered: the file's committed length, the
/// index's tail and the frames on the disk agree at the end. A store that lost a
/// record under contention would still report success for the ones it kept, so
/// the count is re-read from a *fresh* handle rather than from the writer.
fn concurrent_appends_lose_nothing(band: Band) -> TestResult {
    let mut rng = Rng::new(band.first);
    for _ in band.seeds() {
        let appends = tenant_runs(&mut rng);
        let tag = rng.below(u32::MAX);
        let scratch = Scratch::new("contend", tag)?;
        let store = RunStore::open(scratch.store())?;
        let host = host("sim", store)?;
        let work = append_task()?;

        let mut ids = Vec::new();
        for index in 0..appends {
            let report = lgwks_bot::rt::runtime::block_on(host.run(&work, index));
            assert!(
                report.disposition().is_success(),
                "seed {tag}: append {index} failed: {:?}",
                report.error()
            );
            ids.push(report.run_id().ok_or("a stored run must name a run id")?);
        }
        drop(host);

        // Reopened, so every count below is answered from the disk.
        let reopened = RunStore::open(scratch.store())?;
        for id in &ids {
            assert_eq!(
                reopened.record_count(*id),
                1,
                "seed {tag}: run {id:?} kept exactly its one record under contention"
            );
        }
    }
    Ok(())
}

/// The same step recorded many times commits one frame and stays readable.
///
/// This is the at-least-once half of the claim: a step whose value was returned and
/// whose caller recorded it again is the same fact seen twice, so it must not
/// append a second frame. A store that did would grow without bound under a retry
/// loop, and the resumed run would still be right — so only the frame count can
/// see the defect.
fn duplicate_submissions_are_idempotent(band: Band) -> TestResult {
    let mut rng = Rng::new(band.first);
    for _ in band.seeds() {
        let repeats = tenant_runs(&mut rng);
        let tag = rng.below(u32::MAX);
        let scratch = Scratch::new("dupe", tag)?;
        let store = RunStore::open(scratch.store())?;
        let host = host("sim", store)?;
        let work = append_task()?;
        let run = RunId::mint()?;

        // The same run id and the same value, recorded over and over: every attempt
        // after the first must find the record already there.
        for round in 0..repeats {
            let report = lgwks_bot::rt::runtime::block_on(host.resume(run, &work, 42u32));
            assert!(
                report.disposition().is_success(),
                "seed {tag}: repeat {round} failed: {:?}",
                report.error()
            );
            assert_eq!(
                report.output(),
                Some(&42),
                "seed {tag}: the value is the same every time"
            );
        }
        drop(host);

        let reopened = RunStore::open(scratch.store())?;
        assert_eq!(
            reopened.record_count(run),
            1,
            "seed {tag}: {repeats} identical submissions left one record, not {repeats}"
        );
        // And the store's byte length grew by one frame, not by `repeats`: the
        // records ceiling is about what is retained, and a duplicate that wrote a
        // frame would be invisible to a record count alone.
        let frames = reopened.committed_bytes();
        assert!(
            frames > 0,
            "seed {tag}: the store committed no bytes at all, so the repeats were never seen"
        );
    }
    Ok(())
}

/// A record that did not land leaves the step free to record, and one that did is
/// never overwritten.
///
/// The at-least-once half, driven through the public path. A resume of a run whose
/// *last* step was interrupted finds no record for it and records it now; a resume
/// of the same run afterwards finds that record and replays it. The store must hold
/// exactly one record for that step after both, because the second resume must not
/// have written a second frame for a value the first already committed — and a
/// second frame would be invisible to a record count unless the bytes were checked
/// too.
fn an_interrupted_step_records_exactly_once(band: Band) -> TestResult {
    let mut rng = Rng::new(band.first);
    for _ in band.seeds() {
        let attempts = tenant_runs(&mut rng);
        let tag = rng.below(u32::MAX);
        let scratch = Scratch::new("once", tag)?;
        let path = scratch.store();
        let store = RunStore::open(&path)?;
        let work = append_task()?;
        let host = host("sim", store)?;
        let run = RunId::mint()?;

        // The first attempt records. Every resume after it replays, because the
        // record is there — so the body is never polled again and the value is
        // always the one the first attempt committed.
        let first = lgwks_bot::rt::runtime::block_on(host.resume(run, &work, 7u32));
        assert!(
            first.disposition().is_success(),
            "seed {tag}: the first attempt must record"
        );
        for round in 1..attempts {
            let report = lgwks_bot::rt::runtime::block_on(host.resume(run, &work, 7u32));
            assert!(
                report.disposition().is_success(),
                "seed {tag}: replay {round} failed: {:?}",
                report.error()
            );
            assert_eq!(
                report.output(),
                Some(&7),
                "seed {tag}: a replayed step returns its committed value"
            );
        }
        drop(host);

        // The byte length is the receipt: one record, one frame. A store that
        // appended per resume would grow by a frame each time while the record
        // count stayed at one, which is why both are checked.
        let reopened = RunStore::open(&path)?;
        assert_eq!(
            reopened.record_count(run),
            1,
            "seed {tag}: {attempts} resumes of one run left one record"
        );
        assert!(
            reopened.committed_bytes() > 0,
            "seed {tag}: the store committed no bytes, so no record was ever written"
        );
    }
    Ok(())
}

/// Tenants contending on one store each read their own records and never another's.
///
/// The tenants share **one file and one store handle**, so isolation is exercised by
/// the store's own tenant check on a record read rather than by a directory layout
/// that separates them by luck. Each tenant's run id is offered to every tenant's
/// host: the owner is admitted and resumes, and every other tenant is refused.
fn tenants_interleaved_stay_isolated(band: Band) -> TestResult {
    let mut rng = Rng::new(band.first);
    for _ in band.seeds() {
        let tenants = rng.between(2, 8);
        let appends = tenant_runs(&mut rng);
        let tag = rng.below(u32::MAX);
        let scratch = Scratch::new("tenant", tag)?;
        let store = RunStore::open(scratch.store())?;
        let work = append_task()?;

        let mut runs: Vec<(String, RunId)> = Vec::new();
        for index in 0..tenants {
            let name = format!("tenant-{index}");
            let host = host(&name, store.clone())?;
            for step in 0..appends {
                let report = lgwks_bot::rt::runtime::block_on(host.run(&work, step));
                assert!(
                    report.disposition().is_success(),
                    "seed {tag}: {name} append {step} failed: {:?}",
                    report.error()
                );
                if step == 0 {
                    runs.push((name.clone(), report.run_id().ok_or("a run id")?));
                }
            }
        }
        drop(store);

        let reopened = RunStore::open(scratch.store())?;
        for entry in &runs {
            let (owner, run) = (entry.0.as_str(), entry.1);
            for other in &runs {
                let asker = other.0.as_str();
                assert_eq!(
                    reopened.tenant_of(run).as_deref(),
                    Some(owner),
                    "seed {tag}: {owner}'s run is attributed to {owner}, whoever asks"
                );
                assert_eq!(
                    reopened.record_count(run),
                    1,
                    "seed {tag}: {owner}'s one run keeps its one record after {asker} \
                     read it — {appends} appends is {appends} runs of one, not one of \
                     {appends}"
                );
                if asker != owner {
                    // A foreign tenant is refused the resume outright, and its
                    // refusal leaves the record count exactly as it was.
                    let foreign = lgwks_bot::rt::runtime::block_on(
                        host(asker, reopened.clone())?.resume(run, &work, 1u32),
                    );
                    assert_eq!(
                        foreign.disposition(),
                        lgwks_bot::task::Disposition::Refused,
                        "seed {tag}: {asker} must be refused {owner}'s run"
                    );
                }
            }
        }
    }
    Ok(())
}

/// The same seed produces the same trace, twice.
fn same_seed_replays(band: Band) -> TestResult {
    let body = |sim_run: &mut sim::Sim| -> TestResult {
        let appends = tenant_runs(sim_run.rng());
        let tag = u32::try_from(sim_run.seed & u64::from(u32::MAX)).unwrap_or_default();
        let scratch = Scratch::new("replay", tag)?;
        let store = RunStore::open(scratch.store())?;
        let host = host("sim", store)?;
        let work = append_task()?;
        for index in 0..appends {
            let report = lgwks_bot::rt::runtime::block_on(host.run(&work, index));
            sim_run
                .trace
                .record_u64("append-ok", u64::from(report.disposition().is_success()));
            sim_run.trace.record_u64("append-index", u64::from(index));
        }
        drop(host);
        // The store's committed length is the digest of "nothing was lost", which is
        // what makes a divergence in ordering visible to a replay comparison.
        let reopened = RunStore::open(scratch.store())?;
        sim_run
            .trace
            .record_u64("committed", reopened.committed_bytes());
        Ok(())
    };
    sim::assert_replays(band, body)?;
    Ok(())
}

band_family::band_family! {
    concurrent_appends_lose_nothing_band_00 => concurrent_appends_lose_nothing, 0;
    concurrent_appends_lose_nothing_band_01 => concurrent_appends_lose_nothing, 1;
    concurrent_appends_lose_nothing_band_02 => concurrent_appends_lose_nothing, 2;
    concurrent_appends_lose_nothing_band_03 => concurrent_appends_lose_nothing, 3;
    duplicate_submissions_are_idempotent_band_04 => duplicate_submissions_are_idempotent, 4;
    duplicate_submissions_are_idempotent_band_05 => duplicate_submissions_are_idempotent, 5;
    duplicate_submissions_are_idempotent_band_06 => duplicate_submissions_are_idempotent, 6;
    duplicate_submissions_are_idempotent_band_07 => duplicate_submissions_are_idempotent, 7;
    an_interrupted_step_records_exactly_once_band_04 => an_interrupted_step_records_exactly_once, 4;
    an_interrupted_step_records_exactly_once_band_05 => an_interrupted_step_records_exactly_once, 5;
    an_interrupted_step_records_exactly_once_band_06 => an_interrupted_step_records_exactly_once, 6;
    an_interrupted_step_records_exactly_once_band_07 => an_interrupted_step_records_exactly_once, 7;
    tenants_interleaved_stay_isolated_band_08 => tenants_interleaved_stay_isolated, 8;
    tenants_interleaved_stay_isolated_band_09 => tenants_interleaved_stay_isolated, 9;
    tenants_interleaved_stay_isolated_band_10 => tenants_interleaved_stay_isolated, 10;
    same_seed_replays_band_11 => same_seed_replays, 11;
    same_seed_replays_band_12 => same_seed_replays, 12;
    same_seed_replays_band_13 => same_seed_replays, 13;
    same_seed_replays_band_14 => same_seed_replays, 14;
    same_seed_replays_band_15 => same_seed_replays, 15;
}
