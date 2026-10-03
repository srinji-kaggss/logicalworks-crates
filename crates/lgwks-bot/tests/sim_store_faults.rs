//! Seeded store faults: a read that did not answer, and a version this build
//! does not read, each swept against the real run path.
//!
//! Two of this branch's claims are properties of a *decision made from a store
//! that could not answer*, so both are exercised here across every seed a band
//! cuts rather than at one point. A single point would pass for a check that
//! happened to answer its own case; a sweep fails for the one seed whose store
//! shape or version byte the check was not written for.
//!
//! - **INV-BOT-59**: a store whose next *step* read is failed with
//!   [`RunStore::fail_next_index_read`] must reach the caller as
//!   `FlowError::Store` carrying the store's own `StoreError::Storage`, never as
//!   a drift and never as a rendered `Failed` string — and a reopen must recover,
//!   because the records themselves were never touched.
//! - **The format version** ([`StoreError::FormatVersion`]): only `\x02` is
//!   admitted. A store written by a real run and then re-stamped with any other
//!   version byte is refused naming both versions, and its bytes do not move.
//!
//! # What is real and what is virtual
//!
//! The real [`Host`], the real [`RunStore`] over a real file, the real
//! `remember`, the real frame codec and the real refusals. Only the seed is
//! virtualised: it draws the store's shape, how many replays run before the
//! fault, and the version byte. Nothing here reads a run id or a path into the
//! trace — both are minted per attempt — so a seed replays to the same hash.
//!
//! The fault is aimed at the durable step rather than at the host's admission
//! pre-flight by the injector's own placement (a `RunRecords` door, reached only
//! from `Scope::remember`), which is what makes this family a test of the step's
//! refusal rather than of the host's.

#![cfg(all(feature = "script", feature = "ephemeral"))]

mod sim;

#[path = "support/resume.rs"]
mod shared;

use std::error::Error;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use lgwks_bot::script::{FlowError, Scope, remember};
use lgwks_bot::task::{DefinitionIdentity, Disposition, Host, RunStore, StoreError, Task, task};

use shared::{Scratch, format_version, ran, record_run, store_refusal};

type TestResult = Result<(), Box<dyn Error>>;

/// The task every scenario here runs, and the name its definition declares.
const TASK: &str = "staged";
/// The durable-value schema this file's runs declare.
const CODEC: &str = "lgwks.bot.schema.v1.sim-store-faults";
/// The two tenants a band sweeps, so a fault for one is never the other's.
const TENANTS: [&str; 2] = ["acme", "widget"];

// ── The durable task ────────────────────────────────────────────────────────

/// The future the task body returns.
///
/// `Pin<Box<dyn Future>>` because `Task` stores its body's own future type and
/// the point of this file is what the store does under fault, not what the
/// unboxed form looks like.
type BodyFuture = Pin<Box<dyn Future<Output = Result<u32, FlowError>>>>;

/// The task type [`staged_task`] builds: a named function pointer returning a
/// named future, so one body has one type every call site can pass.
type StagedTask = Task<fn(Scope, Staged) -> BodyFuture>;

/// What one run is given: how many durable steps it has, and where the steps
/// write the markers that say their body ran.
///
/// A struct rather than two parameters, because the task's input type is what
/// the `Task` body takes and a pair would force every call site to name both in
/// the right order.
#[derive(Clone, Debug)]
struct Staged {
    /// Where each step's marker lives.
    dir: PathBuf,
    /// How many durable steps the definition declares.
    steps: u32,
}

/// The task body: `steps` durable steps whose values sum.
///
/// A step's marker is written *inside* its body, so a marker count that did not
/// move across a resume is the filesystem's own evidence that the body was never
/// entered — which is the claim a store fault rests on.
async fn staged_body(scope: Scope, plan: Staged) -> Result<u32, FlowError> {
    let mut total: u32 = 0;
    for index in 0..plan.steps {
        let name = format!("s{index}");
        let child = scope.enter(&name)?;
        let dir = plan.dir.clone();
        let value = remember(&child, "value", move || async move {
            record_run(&dir, &format!("s{index}"))?;
            Ok::<_, FlowError>(index)
        })
        .await?;
        total = total.saturating_add(value);
    }
    Ok(total)
}

/// The task this file drives.
fn staged_task() -> Result<StagedTask, FlowError> {
    task(TASK, boxed_staged)
}

/// The same body behind its nameable return type.
fn boxed_staged(scope: Scope, plan: Staged) -> BodyFuture {
    Box::pin(staged_body(scope, plan))
}

/// The sum every uninterrupted `steps`-step run produces.
fn uninterrupted(steps: u32) -> u32 {
    (0..steps).fold(0u32, |acc, index| acc.saturating_add(index))
}

/// The content digest of one integer, as [`Host::definition`] is given it.
///
/// A blake3 of the little-endian bytes rather than the identity trait's framing:
/// the digest is opaque to the store, and the only property this file needs is
/// that the same `steps` gives the same digest on the first attempt and the
/// resume.
fn digest_of(input: u32) -> lgwks_std::hash::Digest {
    lgwks_std::hash::blake3(&input.to_le_bytes())
}

/// The definition identity a run of `steps` steps records under.
fn identity_for(host: &Host, steps: u32) -> DefinitionIdentity {
    host.definition(
        TASK,
        1,
        Some(digest_of(steps)),
        usize::try_from(steps).unwrap_or(1),
    )
    .with_codec(CODEC)
}

/// A host for `tenant` whose store is `<dir>/<tenant>.runstore`.
fn host_for(tenant: &str, dir: &Path) -> Result<Host, Box<dyn Error>> {
    Ok(Host::builder(tenant)?.run_store(dir)?.build()?)
}

/// Every step's marker count, in step order.
fn markers(dir: &Path, steps: u32) -> Result<Vec<u32>, Box<dyn Error>> {
    (0..steps)
        .map(|index| ran(dir, &format!("s{index}")))
        .collect()
}

// ── INV-BOT-59: a read failure reaches the caller as the store ───────────────

/// A failed step read reaches the report as the store's own error, and a reopen
/// recovers.
///
/// The seed draws the store's shape (how many durable steps the run recorded) and
/// how many *unarmed* replays run before the fault, so the sweep carries the
/// control beside the fault rather than only the fault. The fault itself is
/// armed on the real `RunStore` and the resume goes through `Host::resume_under`
/// over a *reopened* file, so the check under test is the one the run path
/// actually makes.
///
/// Three things must hold for every seed. The armed resume is `Failed` and its
/// error is `FlowError::Store` wrapping `StoreError::Storage` — never
/// [`FlowError::Incompatible`], which is the defect this family exists to catch:
/// the old check turned a device error into `false` and rendered that as a claim
/// about a definition. No step body was constructed while the store could not
/// answer, because a store that cannot say whether it holds a record must not run
/// the work as though it held none. And a fresh reopen then replays every step,
/// which is the whole of "the records themselves were never touched".
fn read_fault_reaches_the_report_as_the_store(band: sim::Band) -> TestResult {
    let body = |sim: &mut sim::Sim| -> TestResult {
        for tenant in TENANTS {
            let steps = sim.rng().between(1, 4);
            let warm = sim.rng().below(steps.saturating_add(1));
            let scratch = Scratch::new("sim-read-fault")?;
            let dir = scratch.join("store");
            let plan = Staged {
                dir: dir.clone(),
                steps,
            };
            let expected = uninterrupted(steps);

            let host = host_for(tenant, &dir)?;
            let identity = identity_for(&host, steps);
            let first =
                lgwks_bot::block_on(host.run_under(&identity, &staged_task()?, plan.clone()));
            assert_eq!(
                first.disposition(),
                Disposition::Succeeded,
                "{tenant}: the first attempt must succeed before a read can fail"
            );
            assert_eq!(
                first.output(),
                Some(&expected),
                "{tenant}: the first attempt's own output"
            );
            let run = first.run_id().ok_or("a stored run must name its run id")?;
            drop(first);
            drop(host);
            assert_eq!(
                markers(&dir, steps)?,
                vec![1u32; usize::try_from(steps).unwrap_or(1)],
                "{tenant}: every step's body ran exactly once on the first attempt"
            );

            // The replays before the fault: a compatible resume returns every
            // recorded value without entering a body.
            for replay in 0..warm {
                let replaying = host_for(tenant, &dir)?;
                let report = lgwks_bot::block_on(replaying.resume_under(
                    run,
                    &identity,
                    &staged_task()?,
                    plan.clone(),
                ));
                assert_eq!(
                    report.disposition(),
                    Disposition::Succeeded,
                    "{tenant}: warm replay {replay} must succeed, got {:?}",
                    report.error()
                );
            }
            let before = markers(&dir, steps)?;

            // The armed resume: the store's next step read fails.
            let armed = host_for(tenant, &dir)?;
            armed
                .run_store()
                .ok_or("a stored host keeps a store")?
                .fail_next_index_read();
            let refused = lgwks_bot::block_on(armed.resume_under(
                run,
                &identity,
                &staged_task()?,
                plan.clone(),
            ));
            assert_eq!(
                refused.disposition(),
                Disposition::Failed,
                "{tenant}: a read failure is a failure, not a compatibility and not a replay"
            );
            let error = refused
                .error()
                .ok_or("a store that cannot be read must not succeed")?;
            assert!(
                !matches!(error, FlowError::Incompatible { .. }),
                "{tenant}: a read failure must not be reported as a definition drift, got: {error}"
            );
            let refusal = store_refusal(error).ok_or(
                "a read failure must reach the caller as FlowError::Store carrying the store's own error",
            )?;
            assert!(
                matches!(refusal, StoreError::Storage { .. }),
                "{tenant}: the wrapped refusal must be the device's own storage error, got: {refusal:?}"
            );
            assert_eq!(
                markers(&dir, steps)?,
                before,
                "{tenant}: a refused step entered its body, so the fault did not stop it"
            );
            drop(armed);

            // The reopen: a fresh handle over the untouched bytes replays.
            let recovered = host_for(tenant, &dir)?;
            let after =
                lgwks_bot::block_on(recovered.resume_under(run, &identity, &staged_task()?, plan));
            assert_eq!(
                after.disposition(),
                Disposition::Succeeded,
                "{tenant}: a reopened store must recover, got {:?}",
                after.error()
            );
            assert_eq!(
                after.output(),
                Some(&expected),
                "{tenant}: the recovered resume replays the recorded sum"
            );
            assert_eq!(
                markers(&dir, steps)?,
                before,
                "{tenant}: the recovering resume re-ran a durable step"
            );

            sim.record(tenant);
            sim.trace.record_u64("steps", u64::from(steps));
            sim.trace.record_u64("warm", u64::from(warm));
            sim.trace
                .record_u64("refused", shared::disposition_code(Disposition::Failed));
            sim.trace
                .record_u64("recovered", shared::disposition_code(after.disposition()));
        }
        Ok(())
    };
    sim::assert_replays(band, body)
}

// ── The store's format version: only \x02 is admitted ────────────────────────

/// The version byte's position in the store's header, which is the byte that
/// differs between formats.
const VERSION_AT: usize = 15;
/// The version this build reads.
///
/// Spelled out rather than imported from the crate: the claim under test is that
/// the other bytes are *not* this one, so a check that read both from the crate
/// would agree with a revert. The literal is what makes the refusal specific.
const CURRENT_FORMAT: u8 = 2;
/// The version bytes a seed draws from, including the one this build reads.
///
/// `\x02` is in the table on purpose: without it the family would only ever
/// observe refusals, and a store that refused *everything* would satisfy every
/// assertion but the control.
const VERSIONS: [u8; 6] = [1, 2, 3, 0, 7, 255];

/// A real store re-stamped with a drawn version byte: only `\x02` is admitted.
///
/// The store is produced by the shipped writer, so every frame in it is a real
/// frame a real run committed; only the version byte is rewritten, which is
/// exactly the edit that turns today's file into one an earlier build wrote. The
/// seed draws that byte, so a band sweeps both the admitted control and the
/// refusal. A drawn non-current byte must be refused as
/// [`StoreError::FormatVersion`] naming both versions — not `NotAStore`, which
/// would tell an operator their own data was never theirs — and the bytes must
/// not move, because a refusal that trims what it cannot read is how a re-run
/// loses a system.
fn only_the_current_format_is_admitted(band: sim::Band) -> TestResult {
    let body = |sim: &mut sim::Sim| -> TestResult {
        for tenant in TENANTS {
            let steps = sim.rng().between(1, 3);
            let drawn = VERSIONS[usize::try_from(sim.rng().below(6)).unwrap_or(0)];
            let scratch = Scratch::new("sim-format")?;
            let dir = scratch.join("store");
            let plan = Staged {
                dir: dir.clone(),
                steps,
            };

            let host = host_for(tenant, &dir)?;
            let identity = identity_for(&host, steps);
            let first =
                lgwks_bot::block_on(host.run_under(&identity, &staged_task()?, plan.clone()));
            assert_eq!(
                first.disposition(),
                Disposition::Succeeded,
                "{tenant}: the store must be written before its version is rewritten"
            );
            let run = first.run_id().ok_or("a stored run must name its run id")?;
            let path = host
                .run_store()
                .ok_or("a stored host keeps a store")?
                .path()
                .to_path_buf();
            drop(first);
            drop(host);

            let mut bytes = std::fs::read(&path)?;
            assert_eq!(
                bytes[VERSION_AT], CURRENT_FORMAT,
                "{tenant}: the shipped store must be written at the current version before it is re-stamped"
            );
            bytes[VERSION_AT] = drawn;
            std::fs::write(&path, &bytes)?;
            let before = std::fs::read(&path)?;

            match RunStore::open(&path) {
                Ok(store) => {
                    assert_eq!(
                        drawn, CURRENT_FORMAT,
                        "{tenant}: a store stamped with version {drawn} must not open"
                    );
                    assert_eq!(
                        store.record_count(run),
                        usize::try_from(steps).unwrap_or(1),
                        "{tenant}: the current version reads every record back"
                    );
                }
                Err(error) => {
                    assert_ne!(
                        drawn, CURRENT_FORMAT,
                        "{tenant}: the current version was refused: {error}"
                    );
                    let (found, expected) = format_version(&error).ok_or_else(|| -> Box<dyn Error> {
                        format!("{tenant}: an off-version store must be a version refusal, got: {error}")
                            .into()
                    })?;
                    assert_eq!(
                        found, drawn,
                        "{tenant}: the refusal must name the version byte found"
                    );
                    assert_eq!(
                        expected, CURRENT_FORMAT,
                        "{tenant}: the refusal must name the version this build reads"
                    );
                    assert_eq!(
                        std::fs::read(&path)?,
                        before,
                        "{tenant}: the refusal moved bytes in a store it could not read"
                    );
                }
            }

            sim.record(tenant);
            sim.trace.record_u64("drawn", u64::from(drawn));
            sim.trace
                .record_u64("admitted", u64::from(drawn == CURRENT_FORMAT));
            sim.trace.record_u64("steps", u64::from(steps));
        }
        Ok(())
    };
    sim::assert_replays(band, body)
}

/// The same seed produces the same trace, and a different seed a different one.
///
/// The replay receipt, asserted directly as well as through
/// [`sim::assert_replays`] so the two halves are separate failures: a run that is
/// internally inconsistent shows up here, and a run that is consistently *wrong*
/// shows up in the families above.
fn same_seed_same_trace_hash(band: sim::Band) -> TestResult {
    let one_run = |sim: &mut sim::Sim| -> TestResult {
        let steps = sim.rng().between(1, 4);
        let warm = sim.rng().below(steps.saturating_add(1));
        let drawn = VERSIONS[usize::try_from(sim.rng().below(6)).unwrap_or(0)];
        sim.trace.record_u64("steps", u64::from(steps));
        sim.trace.record_u64("warm", u64::from(warm));
        sim.trace.record_u64("drawn", u64::from(drawn));
        Ok(())
    };
    sim::assert_replays(band, one_run)?;

    // A different seed must reach a different trace, or the hash is a constant
    // and proves nothing about replay.
    let first = sim::sweep(sim::Band::new(1, 1), one_run)?;
    let second = sim::sweep(sim::Band::new(2, 1), one_run)?;
    assert_ne!(
        first, second,
        "two seeds produced the same trace hash, so the hash is not a receipt of anything this scenario decided"
    );
    Ok(())
}

// The tests below are written out rather than declared through `band_family!`,
// and that is deliberate: the gate that counts simulation evidence reads `#[test]`
// attributes out of the sources, so a family declared through a macro is real,
// runs, and is invisible to it. Each one names the band it sweeps.

#[test]
fn read_fault_reaches_the_report_as_the_store_band_00() -> TestResult {
    read_fault_reaches_the_report_as_the_store(sim::band_of(0))
}

#[test]
fn read_fault_reaches_the_report_as_the_store_band_01() -> TestResult {
    read_fault_reaches_the_report_as_the_store(sim::band_of(1))
}

#[test]
fn read_fault_reaches_the_report_as_the_store_band_02() -> TestResult {
    read_fault_reaches_the_report_as_the_store(sim::band_of(2))
}

#[test]
fn read_fault_reaches_the_report_as_the_store_band_03() -> TestResult {
    read_fault_reaches_the_report_as_the_store(sim::band_of(3))
}

#[test]
fn only_the_current_format_is_admitted_band_04() -> TestResult {
    only_the_current_format_is_admitted(sim::band_of(4))
}

#[test]
fn only_the_current_format_is_admitted_band_05() -> TestResult {
    only_the_current_format_is_admitted(sim::band_of(5))
}

#[test]
fn only_the_current_format_is_admitted_band_06() -> TestResult {
    only_the_current_format_is_admitted(sim::band_of(6))
}

#[test]
fn only_the_current_format_is_admitted_band_07() -> TestResult {
    only_the_current_format_is_admitted(sim::band_of(7))
}

#[test]
fn same_seed_same_trace_hash_band_08() -> TestResult {
    same_seed_same_trace_hash(sim::band_of(8))
}

#[test]
fn same_seed_same_trace_hash_band_09() -> TestResult {
    same_seed_same_trace_hash(sim::band_of(9))
}
