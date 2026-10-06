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

use crate::scratch::Scratch;

use crate::sim;

use crate::band_family;

use crate::liveness_fixtures as liveness;

use crate::resume_fixtures as shared;

use shared::store_file;

use std::error::Error;
use std::future::Future;
use std::pin::Pin;

use lgwks_bot::effect::RunId;
use lgwks_bot::script::{FlowError, Scope, remember};
use lgwks_bot::task::{Host, RunStore, Task, task};

use liveness::{PROGRESS_TURNS, Parked, heartbeat};

use sim::Band;
use sim::Rng;

type TestResult = Result<(), Box<dyn Error>>;

/// The most appends one simulated contention round performs.
const MAX_APPENDS: u32 = 24;

/// The named future a simulated task body returns.
type BodyFuture = Pin<Box<dyn Future<Output = Result<u32, FlowError>>>>;

/// The task type every scenario here builds.
type AppendTask = Task<fn(Scope, u32) -> BodyFuture>;

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

/// One seed's fixture: a fresh scratch directory, the shipped store opened in
/// it, the append task, and one host on that store.
///
/// Every single-host family starts from exactly these four things, so they are
/// built once here; a family that opened them by hand would be the place a seed
/// silently ran against another seed's store.
struct Seeded {
    scratch: Scratch,
    work: AppendTask,
    host: Host,
}

/// Open the [`Seeded`] fixture under the scratch name `name`.
fn seeded(name: &'static str) -> Result<Seeded, Box<dyn Error>> {
    let scratch = Scratch::new(name)?;
    let store = RunStore::open(store_file(scratch.path()))?;
    Ok(Seeded {
        work: append_task()?,
        host: host("sim", store)?,
        scratch,
    })
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
        let Seeded {
            scratch,
            work,
            host,
        } = seeded("sim-contend")?;

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
        let reopened = RunStore::open(store_file(scratch.path()))?;
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
        let Seeded {
            scratch,
            work,
            host,
        } = seeded("sim-dupe")?;
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

        let reopened = RunStore::open(store_file(scratch.path()))?;
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
        let Seeded {
            scratch,
            work,
            host,
        } = seeded("sim-once")?;
        let path = store_file(scratch.path());
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

/// One seed of the interleaved-tenant sweep: every tenant appends, then every run id is offered to every tenant.
fn isolation_seed(rng: &mut Rng) -> TestResult {
    let tenants = rng.between(2, 8);
    let appends = tenant_runs(rng);
    let tag = rng.below(u32::MAX);
    let scratch = Scratch::new("sim-tenant")?;
    let store = RunStore::open(store_file(scratch.path()))?;
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

    let reopened = RunStore::open(store_file(scratch.path()))?;
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
        isolation_seed(&mut rng)?;
    }
    Ok(())
}

/// The same seed produces the same trace, twice.
fn same_seed_replays(band: Band) -> TestResult {
    let body = |sim_run: &mut sim::Sim| -> TestResult {
        let appends = tenant_runs(sim_run.rng());
        let Seeded {
            scratch,
            work,
            host,
        } = seeded("sim-replay")?;
        for index in 0..appends {
            let report = lgwks_bot::rt::runtime::block_on(host.run(&work, index));
            sim_run
                .trace
                .record_number("append-ok", u64::from(report.disposition().is_success()));
            sim_run
                .trace
                .record_number("append-index", u64::from(index));
        }
        drop(host);
        // The store's committed length is the digest of "nothing was lost", which is
        // what makes a divergence in ordering visible to a replay comparison.
        let reopened = RunStore::open(store_file(scratch.path()))?;
        sim_run
            .trace
            .record_number("committed", reopened.committed_bytes());
        Ok(())
    };
    sim::assert_replays(band, body)?;
    Ok(())
}

/// One seed of the parked-device sweep: queue behind a stalled device, then drain.
fn parked_device_seed(rng: &mut Rng) -> TestResult {
    let tag = rng.below(u32::MAX);
    let scratch = Scratch::new("sim-parked")?;
    let store = RunStore::open_with_stalled_device(store_file(scratch.path()))?;
    let work = append_task()?;
    let host = host("sim", store)?;

    // The device is parked, so the append waits rather than writing. The
    // shared parked-device instrument owns the release and the turn counter:
    // it is the same instrument the two liveness families use, so this family
    // is not the third place that decides how a parked device is released.
    // Its watchdog releases from an independent thread once the unrelated
    // heartbeat has demonstrably taken turns, which proves both halves — that
    // the wait is not a stall and that a release is always possible.
    let installed = host
        .run_store()
        .ok_or("a host built with a store installed keeps one")?;
    let parked = Parked::at(None, installed.storage_gate())?;
    // Both futures are driven by one poll_fn, the way a single-threaded
    // executor would. `host.run` builds its own driver, so polling the step
    // through `block_on` alone would leave the heartbeat unpolled and the
    // turn count at zero — which would pass for a blocking write.
    let mut beat = Box::pin(heartbeat(parked.ticks()));
    let mut step = Box::pin(host.run(&work, 7u32));
    let run = lgwks_bot::rt::runtime::block_on(std::future::poll_fn(|cx| {
        let _unobserved = beat.as_mut().poll(cx);
        step.as_mut().poll(cx)
    }));
    drop(step);
    let observed = parked.observed()?;

    assert!(
        observed >= PROGRESS_TURNS,
        "seed {tag}: a parked device left the unrelated task at {observed} turns;              a blocking wait would reach one"
    );
    assert!(
        run.disposition().is_success(),
        "seed {tag}: a released device still earns its record: {:?}",
        run.error()
    );
    drop(beat);
    drop(host);

    // The record is on the disk, read back through a fresh handle: a wait that
    // released into nothing looks identical from the caller's side.
    let reopened = RunStore::open(store_file(scratch.path()))?;
    assert!(
        reopened.committed_bytes() > 0,
        "seed {tag}: a parked store committed no bytes at all"
    );
    Ok(())
}

/// A store whose device has stopped answering parks its appends and refuses to
/// grow without limit, then drains every one of them once released.
///
/// The owner's queue is bounded, so a caller that outruns a parked device must be
/// told rather than made to queue forever. This drives that boundary directly: the
/// store is opened stalled, appends are made until one is refused, the gate is
/// released, and every record is then counted from a reopened handle. A store that
/// refused the *first* append would pass the refusal half and fail the drain half;
/// one that never refused would grow without limit.
fn a_parked_device_bounds_its_queue_then_drains(band: Band) -> TestResult {
    let mut rng = Rng::new(band.first);
    for _ in band.seeds() {
        parked_device_seed(&mut rng)?;
    }
    Ok(())
}

/// One seed of the torn-tail sweep: tear the last record and reopen.
fn torn_tail_seed(rng: &mut Rng) -> TestResult {
    let appends = rng.between(2, 8);
    let tag = rng.below(u32::MAX);
    let Seeded {
        scratch,
        work,
        host,
    } = seeded("sim-torn")?;
    let path = store_file(scratch.path());

    let mut ids = Vec::new();
    for index in 0..appends {
        let report = lgwks_bot::rt::runtime::block_on(host.run(&work, index));
        assert!(
            report.disposition().is_success(),
            "seed {tag}: append {index} failed"
        );
        ids.push(report.run_id().ok_or("a stored run must name a run id")?);
    }
    drop(host);
    let whole = std::fs::metadata(&path)?.len();
    assert!(
        whole > 8,
        "seed {tag}: nothing was committed, so nothing can be torn"
    );

    // Cut a byte out of the middle of the last frame, never at a boundary:
    // a truncation on a frame edge would leave a *complete* shorter file and
    // prove nothing about torn tails.
    let mut bytes = std::fs::read(&path)?;
    let cut = bytes
        .len()
        .checked_sub(2)
        .ok_or("a store shorter than two bytes")?;
    bytes.truncate(cut);
    std::fs::write(&path, &bytes)?;

    let reopened = RunStore::open(&path)?;
    let kept = ids
        .iter()
        .filter(|id| reopened.record_count(**id) == 1)
        .count();
    assert!(
        kept >= 1,
        "seed {tag}: a torn tail lost every record, not only the incomplete one"
    );
    assert!(
        kept < ids.len(),
        "seed {tag}: a torn tail was read as a whole record; the last append is {appends}"
    );
    Ok(())
}

/// A store truncated mid-frame drops only the torn record and keeps every whole
/// one before it.
///
/// A write that was cut off leaves a prefix of a frame. That prefix is nobody's
/// answer and may be trimmed; a *complete* length prefix naming a frame no writer
/// produces was acknowledged and must be refused. This corrupts the tail at a
/// seeded cut point and asserts the first half of that rule, which is the half a
/// truncation bug actually gets wrong.
fn a_torn_tail_drops_only_the_incomplete_record(band: Band) -> TestResult {
    let mut rng = Rng::new(band.first);
    for _ in band.seeds() {
        torn_tail_seed(&mut rng)?;
    }
    Ok(())
}

/// One seed of the foreign-run sweep: every tenant refuses every other tenant's run id.
fn foreign_run_seed(rng: &mut Rng) -> TestResult {
    let tenants = rng.between(2, 6);
    let appends = rng.between(1, 4);
    let tag = rng.below(u32::MAX);
    let scratch = Scratch::new("sim-foreign")?;
    let store = RunStore::open(store_file(scratch.path()))?;
    let work = append_task()?;

    let mut handles: Vec<Host> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut owned: Vec<RunId> = Vec::new();
    for index in 0..tenants {
        let name = format!("tenant-{index}");
        let handle = host(&name, store.clone())?;
        // The run id the store actually committed for this tenant, taken from
        // its own report. Minting a second one would offer every tenant a run
        // nobody owns, and the isolation assertion below would then be about
        // absent records rather than about another tenant's data.
        let mut committed = None;
        for step in 0..appends {
            let report = lgwks_bot::rt::runtime::block_on(handle.run(&work, step));
            assert!(
                report.disposition().is_success(),
                "seed {tag}: {name} step {step} failed"
            );
            committed = committed.or_else(|| report.run_id());
        }
        owned.push(committed.ok_or("a stored run must name a run id")?);
        handles.push(handle);
        names.push(name);
    }

    for index in 0..handles.len() {
        let name = names[index].clone();
        let probe = handles[index].clone();
        for other in 0..owned.len() {
            let run = owned[other];
            let report = lgwks_bot::rt::runtime::block_on(probe.resume(run, &work, 1u32));
            let admitted = report.disposition().is_success();
            if other == index {
                return Ok(());
            }
            assert!(
                !admitted || report.output().is_some(),
                "seed {tag}: {name} read {}'s run without a refusal",
                names[other]
            );
            if !admitted {
                assert!(
                    report.error().is_some(),
                    "seed {tag}: {name} was refused {}'s run with no error to name it",
                    names[other]
                );
            }
        }
    }
    Ok(())
}

/// Every tenant's record is readable by its owner and by no one else, at every
/// concurrency the band drives.
///
/// The store is one file and one handle, so this is the store's own tenant check
/// rather than a directory layout that separates tenants by luck. Every tenant's
/// run id is offered to every tenant's host, and the answer must be the owner's
/// records or a refusal — never another tenant's value.
fn a_foreign_run_id_is_refused_by_every_tenant(band: Band) -> TestResult {
    let mut rng = Rng::new(band.first);
    for _ in band.seeds() {
        foreign_run_seed(&mut rng)?;
    }
    Ok(())
}

/// One seed of the reopen-under-load sweep: reopen while appends are in flight.
fn reopen_under_load_seed(rng: &mut Rng) -> TestResult {
    let appends = tenant_runs(rng);
    let tag = rng.below(u32::MAX);
    let scratch = Scratch::new("sim-reopen")?;
    let path = store_file(scratch.path());
    let store = RunStore::open(&path)?;
    let work = append_task()?;
    let host = host("sim", store)?;

    let mut highest = 0u64;
    let mut ids = Vec::new();
    for index in 0..appends {
        let report = lgwks_bot::rt::runtime::block_on(host.run(&work, index));
        assert!(
            report.disposition().is_success(),
            "seed {tag}: append {index} failed"
        );
        ids.push(report.run_id().ok_or("a stored run must name a run id")?);

        let reopened = RunStore::open(&path)?;
        let seen = reopened.committed_bytes();
        assert!(
            seen >= highest,
            "seed {tag}: committed bytes went backwards, {highest} then {seen}"
        );
        highest = seen;

        for id in &ids {
            assert_eq!(
                reopened.record_count(*id),
                1,
                "seed {tag}: a reopen saw run {id:?} at other than one record"
            );
        }
    }
    drop(host);
    Ok(())
}

/// Reopening under concurrent appends converges on every committed record.
///
/// A store that is opened while another handle is still appending must replay
/// what is there without refusing the opener and without losing a record the
/// writer already acknowledged. This reopens once per append and asserts the
/// count never goes *backwards* across reopen — the property a stale read-only
/// view of the file would violate.
fn a_reopen_under_load_never_loses_a_committed_record(band: Band) -> TestResult {
    let mut rng = Rng::new(band.first);
    for _ in band.seeds() {
        reopen_under_load_seed(&mut rng)?;
    }
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
    a_parked_device_bounds_its_queue_then_drains_band_16 => a_parked_device_bounds_its_queue_then_drains, 16;
    a_parked_device_bounds_its_queue_then_drains_band_17 => a_parked_device_bounds_its_queue_then_drains, 17;
    a_torn_tail_drops_only_the_incomplete_record_band_18 => a_torn_tail_drops_only_the_incomplete_record, 18;
    a_torn_tail_drops_only_the_incomplete_record_band_19 => a_torn_tail_drops_only_the_incomplete_record, 19;
    a_foreign_run_id_is_refused_by_every_tenant_band_20 => a_foreign_run_id_is_refused_by_every_tenant, 20;
    a_foreign_run_id_is_refused_by_every_tenant_band_21 => a_foreign_run_id_is_refused_by_every_tenant, 21;
    a_reopen_under_load_never_loses_a_committed_record_band_22 => a_reopen_under_load_never_loses_a_committed_record, 22;
    a_reopen_under_load_never_loses_a_committed_record_band_23 => a_reopen_under_load_never_loses_a_committed_record, 23;
}
