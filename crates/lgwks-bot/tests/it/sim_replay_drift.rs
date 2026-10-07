//! Seeded drift: a run recorded under one definition, resumed under another.
//!
//! T15 asks for a typed incompatibility *before* any new effect, and for a
//! replayed model result to incur no new request. Both are properties of a
//! decision made from a definition identity, so both are exercised here across
//! the four ways a definition can move — its revision, its input, its shape, and
//! its durable-value schema — for two tenants over separate stores, at every seed
//! a band cuts.
//!
//! Why a simulation rather than four named assertions. The property is a
//! *refusal*: the interesting failure is a drift kind that slips through a check
//! written for the others, and a sweep over seeds catches that by construction
//! where four tests would each pass against a check that only answers its own
//! case. The trace hash is what makes the sweep cheap: two runs of one seed must
//! agree on every refusal, so a check that consulted the clock, the OS or an
//! unseeded source shows up as a hash difference rather than as an
//! intermittently green assertion.
//!
//! What is real and what is virtual follows the shared harness: the real
//! [`Host`], the real [`RunStore`] over a real file, the real
//! [`DefinitionIdentity`] and the real refusal. Only the seed is virtualised, and
//! it is the only thing the scenario draws from. Nothing here reads a run id or
//! a path into the trace — both are minted per attempt — so a seed replays to the
//! same hash.
//!
//! Two things are deliberately *not* claimed, and both are stated in the family
//! that would otherwise imply them:
//!
//! - Two tenants here are two **stores**, one file per tenant, which is what
//!   `HostBuilder::run_store` builds. Cross-tenant isolation over one shared file
//!   is `task_resume.rs`'s row and this file does not restate it.
//! - "Step order" is the *shape* of the flow — how many durable steps the
//!   definition declares — because that is the only form of it a step key cannot
//!   see. Two adjacent steps permuted inside the same shape keep every path and
//!   every recorded value, so nothing there is a drift and pretending otherwise
//!   would be refusing a sound resume.
//! - The store's *format* version is not a definition axis at all: a `\x01` store
//!   has no records to disagree with, so it is refused at open rather than
//!   refused as a drift. `a_pre_version_store_is_refused_naming_both_versions`
//!   keeps that separate on purpose — the refusal says "wrong version", not
//!   "wrong definition", because those two send an operator to different places.

// A run store needs the `script` surface to be durable at all and `ephemeral`
// to mint the run identity its records are keyed by, so this target is gated on
// exactly those two — the same gate `task_resume.rs` and `sim_task_resume.rs`
// carry, and for the same reason.
#![cfg(all(feature = "script", feature = "ephemeral"))]

use crate::sim;

use crate::band_family;

use std::error::Error;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::resume_fixtures as shared;

use shared::{ran, record_run};

use lgwks_bot::effect::InputIdentity;
use lgwks_bot::script::{FlowError, Scope, remember};
use lgwks_bot::task::{
    DefinitionIdentity, Disposition, Drift, Host, RunStore, StoreError, Task, task,
};

type TestResult = Result<(), Box<dyn Error>>;

/// The task every scenario here runs, and the one its definition names.
const TASK: &str = "drift";
/// The definition revision the first attempt declares.
const FIRST_REVISION: u64 = 7;
/// The durable-value schema the first attempt declares.
const FIRST_CODEC: &str = "lgwks.bot.schema.v1.model-answer";
/// The schema a later attempt declares instead.
const SECOND_CODEC: &str = "lgwks.bot.schema.v2.model-answer";
/// How many durable steps the definition declares, under both attempts.
const STEPS: usize = 2;
/// The input the first attempt is given, in the families that fix it.
const FIRST_INPUT: u32 = 3;
/// The two tenants this file sweeps, so a refusal for one is never the other's.
const TENANTS: [&str; 2] = ["acme", "widget"];

// ── The task ────────────────────────────────────────────────────────────────

/// The future the task body returns.
///
/// `Pin<Box<dyn Future>>` because `Task` stores its body's own future type and
/// the point of this file is what the store and the report do, not what the
/// unboxed form looks like; the front door's own test covers that.
type BodyFuture = Pin<Box<dyn Future<Output = Result<u32, FlowError>>>>;

/// The task type [`drift_task`] builds: a named function pointer returning a
/// named future, so one body has one type every call site can pass.
type DriftTask = Task<fn(Scope, Job) -> BodyFuture>;

/// How many times a step's body has been constructed.
///
/// What stands in for "a new model request". The counter moves inside
/// `remember`'s closure, which is only reached *after* the record lookup, so a
/// replay that returned its recorded value never moves it — and a body that was
/// constructed but never polled also does not, which is what separates "the
/// value was replayed" from "the body was rebuilt and thrown away".
static BODY_CONSTRUCTIONS: AtomicU64 = AtomicU64::new(0);

/// The count of bodies constructed since the process started.
fn constructions() -> u64 {
    BODY_CONSTRUCTIONS.load(Ordering::SeqCst)
}

/// What one task run is given: the input, and where its step markers live.
///
/// A struct rather than two parameters, because the task's input type is what the
/// `Task` body takes and a pair would force every call site to name both in the
/// right order.
#[derive(Debug, Clone)]
struct Job {
    /// The value the task was asked to run over.
    input: u32,
    /// Where the steps write their run markers.
    dir: PathBuf,
}

/// One durable step.
async fn durable_step(
    scope: &Scope,
    step: &'static str,
    value: u32,
    dir: &Path,
) -> Result<u32, FlowError> {
    let dir = dir.to_path_buf();
    remember(scope, step, || async move {
        BODY_CONSTRUCTIONS.fetch_add(1, Ordering::SeqCst);
        record_run(&dir, step)?;
        Ok::<_, FlowError>(value)
    })
    .await
}

/// The task body: two durable steps whose values sum.
fn drift_body(scope: Scope, job: Job) -> BodyFuture {
    Box::pin(async move {
        let first = durable_step(&scope, "alpha", job.input, &job.dir).await?;
        let second = durable_step(&scope, "beta", job.input.saturating_add(1), &job.dir).await?;
        Ok(first.saturating_add(second))
    })
}

/// The task this file drives.
fn drift_task() -> Result<DriftTask, FlowError> {
    task(TASK, drift_body)
}

// ── One attempt, and the resume that must refuse it ─────────────────────────

/// How a definition moved between the two attempts.
///
/// One arm per axis, because each names a different repair and a drift check
/// written for the others would not catch it. The order is fixed so a seed's
/// draws stay aligned with its recorded trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DriftKind {
    /// The definition revision the caller declares changed.
    Revision,
    /// The input the run was given changed.
    Input,
    /// The shape of the flow changed: a different number of durable steps.
    Order,
    /// The declared schema of the durable values changed.
    Schema,
    /// Nothing changed, which is the control every other arm is measured against.
    None,
}

impl DriftKind {
    /// The four arms that move an axis, in a fixed order.
    const DRIFTED: [Self; 4] = [Self::Revision, Self::Input, Self::Order, Self::Schema];

    /// Every arm, in a fixed order.
    const ALL: [Self; 5] = [
        Self::None,
        Self::Revision,
        Self::Input,
        Self::Order,
        Self::Schema,
    ];

    /// The tag recorded in the trace, stable under any change to `Debug`.
    const fn tag(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Revision => "revision",
            Self::Input => "input",
            Self::Order => "order",
            Self::Schema => "schema",
        }
    }

    /// The axis a resume under this kind must report, or `None` for the control.
    ///
    /// The expectation is stated here rather than read back from what the run
    /// produced, so a check that stopped discriminating would fail against this
    /// table rather than against itself.
    const fn expects(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Revision => Some("definition"),
            Self::Input => Some("input"),
            Self::Order => Some("order"),
            Self::Schema => Some("schema"),
        }
    }

    /// The identity the resume declares, given the one the first attempt used.
    fn declare(self, first: &DefinitionIdentity) -> DefinitionIdentity {
        match self {
            Self::None => first.clone(),
            Self::Revision => DefinitionIdentity::new(
                first.name(),
                first.revision().saturating_add(1),
                first.input(),
                first.steps(),
            )
            .with_codec(first.codec()),
            Self::Input => DefinitionIdentity::new(
                first.name(),
                first.revision(),
                input_digest(first.input(), true),
                first.steps(),
            )
            .with_codec(first.codec()),
            Self::Order => DefinitionIdentity::new(
                first.name(),
                first.revision(),
                first.input(),
                first.steps().saturating_add(1),
            )
            .with_codec(first.codec()),
            Self::Schema => DefinitionIdentity::new(
                first.name(),
                first.revision(),
                first.input(),
                first.steps(),
            )
            .with_codec(SECOND_CODEC),
        }
    }
}

/// The content digest of an input, as [`Host::definition`] is given it.
///
/// `bumped` is how the `Input` arm changes one axis and no other: the same
/// integer shifted by one, so the digest differs and nothing else does.
fn input_digest(input: lgwks_std::hash::Digest, bumped: bool) -> lgwks_std::hash::Digest {
    if !bumped {
        return input;
    }
    // A different integer, and nothing else. Written through the same one
    // constructor as every other digest here, so the two differ in the value and
    // in nothing about how it was framed.
    input_digest_value(u32::MAX)
}

/// What one scenario observed, as values rather than prose.
#[derive(Debug)]
struct Observed {
    /// How many records the store holds for the run.
    records: usize,
    /// The resumed attempt's disposition.
    disposition: Disposition,
    /// The axis the refusal named, if it refused.
    drift: Option<&'static str>,
    /// How many durable bodies the resume constructed.
    constructions: u64,
    /// How many times each step's body ran during the resume.
    markers: (u32, u32),
}

/// A scratch directory removed when the test ends, however it ends.
struct Scratch(std::path::PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.0));
    }
}

/// A host for `tenant`, whose store is `<dir>/<tenant>.runstore`.
///
/// The builder takes the *directory* and names the file after the tenant, which
/// is what makes two tenants pointed at one directory share no file. The path is
/// therefore recomputed from the directory wherever the file itself is read, and
/// the two must not drift — hence the [`store_path`] helper rather than a second
/// spelling of the same filename.
fn host_for(tenant: &str, dir: &Path) -> Result<Host, Box<dyn Error>> {
    Ok(Host::builder(tenant)?.run_store(dir)?.build()?)
}

/// Where the store for `tenant` lives under `dir`.
fn store_path(dir: &Path, tenant: &str) -> PathBuf {
    dir.join(format!("{tenant}.runstore"))
}

/// The identity the first attempt records under.
fn first_identity(
    host: &Host,
    input: u32,
    codec: &str,
) -> Result<DefinitionIdentity, Box<dyn Error>> {
    Ok(host
        .definition(TASK, FIRST_REVISION, Some(input_digest_value(input)), STEPS)
        .with_codec(codec))
}

/// The content digest of one integer, as [`Host::definition`] is given it.
fn input_digest_value(input: u32) -> lgwks_std::hash::Digest {
    let mut hasher = lgwks_std::hash::Hasher::new();
    input.write_identity(&mut hasher);
    hasher.finalize()
}

/// The drift kind this seed chose, drawn from `table` by `sim`.
///
/// One draw for every family that draws one, because two draws spelled out are
/// two places the draw order could move — and a draw order that moves silently
/// renumbers every other family's trace without failing anything. A draw past
/// the table is refused with its seed rather than read as some other arm.
fn drawn(sim: &mut sim::Sim, table: &[DriftKind]) -> Result<DriftKind, Box<dyn Error>> {
    let drawn = usize::try_from(sim.rng().below(u32::try_from(table.len())?))?;
    table
        .get(drawn)
        .copied()
        .ok_or_else(|| format!("seed {} drew arm {drawn} of {}", sim.seed, table.len()).into())
}

/// A stable number per axis, so a trace compares runs rather than rendered text.
fn axis_id(axis: &str) -> u64 {
    match axis {
        "definition" => 1,
        "input" => 2,
        "order" => 3,
        "schema" => 4,
        _ => 0,
    }
}

/// Run the first attempt under `input`, then resume it under `drift`, and
/// observe both.
fn attempt(
    dir: &Path,
    tenant: &str,
    input: u32,
    codec: &str,
    drift: DriftKind,
) -> Result<Observed, Box<dyn Error>> {
    let host = host_for(tenant, dir)?;
    let work = drift_task()?;
    let first = first_identity(&host, input, codec)?;
    let second = drift.declare(&first);
    let job = Job {
        input,
        dir: dir.to_path_buf(),
    };

    let before = constructions();
    let initial = lgwks_bot::block_on(host.run_under(&first, &work, job.clone()));
    if initial.disposition() != Disposition::Succeeded {
        let refusal = Err(format!(
            "the first attempt must succeed for the row to mean anything: {:?}",
            initial.error()
        )
        .into());
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "attempt: returning an error to the caller");
        return refusal;
    }
    assert_eq!(
        constructions().saturating_sub(before),
        u64::try_from(STEPS)?,
        "the first attempt constructs every durable step exactly once"
    );
    let run = initial
        .run_id()
        .ok_or("a stored run must name its run id")?;
    drop(initial);
    drop(host);

    // The resume is on a *fresh* host over a *fresh* handle: the claim is about a
    // restart, and a resume on the handle that wrote the records proves only that
    // a map still had its entries.
    let resumed_host = host_for(tenant, dir)?;
    let before = constructions();
    let markers_before = (ran(dir, "alpha")?, ran(dir, "beta")?);
    let resumed = lgwks_bot::block_on(resumed_host.resume_under(run, &second, &work, job));
    let built = constructions().saturating_sub(before);
    let markers_after = (ran(dir, "alpha")?, ran(dir, "beta")?);

    let records = RunStore::open(store_path(dir, tenant))?.record_count(run);
    Ok(Observed {
        records,
        disposition: resumed.disposition(),
        drift: resumed.error().and_then(drift_axis),
        constructions: built,
        markers: (
            markers_after.0.saturating_sub(markers_before.0),
            markers_after.1.saturating_sub(markers_before.1),
        ),
    })
}

/// The typed `Drift` a refusal carries, or `None` for any other error.
///
/// The exact variant, cloned out of the refusal, so a caller asserts against the
/// crate's own vocabulary rather than against a rendering of it. `match *error`
/// with a `ref` binding is the spelling the workspace's `pattern_type_mismatch`
/// rule requires: matching a `&FlowError` with an owned pattern is a lint error,
/// and the clone is the one the clone-free rule permits because the value has to
/// leave the borrow to be compared by value.
fn drift_of(error: &FlowError) -> Option<Drift> {
    match *error {
        FlowError::Incompatible { ref drift, .. } => Some(drift.clone()),
        _ => None,
    }
}

/// The axis a refusal named, as its stable tag, read from the typed drift.
///
/// [`Drift::kind`] rather than the `Display` text: the tag is the crate's own
/// statement of which axis moved, and a check that read a rendered sentence could
/// pass on a message that merely mentioned another axis's word — which a
/// `contains` over the whole message once did, reporting `definition` for an
/// order drift. A refusal that named no axis renders no tag, so a check that had
/// collapsed into one generic refusal is `None` here, which is the failure the
/// drift families exist to catch.
fn drift_axis(error: &FlowError) -> Option<&'static str> {
    drift_of(error).as_ref().map(Drift::kind)
}

/// Assert `drift` is `kind`'s axis *and* carries the two values that axis moved.
///
/// The axis tag alone is satisfied by a refusal that names the right axis with
/// the wrong numbers, so each arm destructures the typed `Drift` and compares
/// both payload fields against the values this scenario declared. The `match` is
/// over an owned `Drift`, which is what lets the `String` field of the schema arm
/// be read without a borrowed-pattern lint.
///
/// # Errors
///
/// When the refusal carries a different axis, or the same axis with different
/// values, or when `kind` is the control that must not have been refused at all.
fn assert_exact_drift(
    tenant: &str,
    kind: DriftKind,
    drift: Drift,
    recorded_input: lgwks_std::hash::Digest,
) -> TestResult {
    match (kind, drift) {
        (DriftKind::Revision, Drift::Definition { declared, recorded }) => assert_eq!(
            (declared, recorded),
            (FIRST_REVISION.saturating_add(1), FIRST_REVISION),
            "{tenant}: the definition axis must name both revisions"
        ),
        (DriftKind::Input, Drift::Input { named, recorded }) => assert_eq!(
            (named, recorded),
            (input_digest_value(u32::MAX), recorded_input),
            "{tenant}: the input axis must name both input digests"
        ),
        (DriftKind::Order, Drift::Order { declared, recorded }) => assert_eq!(
            (declared, recorded),
            (STEPS.saturating_add(1), STEPS),
            "{tenant}: the order axis must name both durable-step counts"
        ),
        (DriftKind::Schema, Drift::Schema { named, recorded }) => assert_eq!(
            (named.as_str(), recorded.as_str()),
            (SECOND_CODEC, FIRST_CODEC),
            "{tenant}: the schema axis must name both schema ids"
        ),
        (kind, other) => {
            let refusal = Err(format!(
                "{tenant}/{}: refused with the wrong typed drift: {other:?}",
                kind.tag()
            )
            .into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "assert_exact_drift: returning an error to the caller");
            return refusal;
        }
    }
    Ok(())
}

// ── The families ───────────────────────────────────────────────────────────

/// Every drift kind is refused with its own typed axis, and the control is not.
fn drift_kinds_are_refused_typed(band: sim::Band) -> TestResult {
    let body = |sim: &mut sim::Sim| -> TestResult {
        for tenant in TENANTS {
            let kind = drawn(sim, &DriftKind::DRIFTED)?;
            let dir = Scratch(sim.scratch("drift")?);
            let observed = attempt(&dir.0, tenant, FIRST_INPUT, FIRST_CODEC, kind)?;

            sim.record(&format!("{tenant}:{}", kind.tag()));
            sim.trace.record_number(
                "disposition",
                shared::disposition_code(observed.disposition),
            );

            assert_eq!(
                observed.disposition,
                Disposition::Refused,
                "{tenant}/{}: a drifted resume must be refused, not run",
                kind.tag()
            );
            assert_eq!(
                observed.drift,
                kind.expects(),
                "{tenant}/{}: the refusal must name this axis, saw {:?}",
                kind.tag(),
                observed.drift
            );
            assert_eq!(
                observed.records,
                STEPS,
                "{tenant}/{}: the refusal wrote a record",
                kind.tag()
            );
            assert_eq!(
                observed.constructions,
                0,
                "{tenant}/{}: a refused resume constructed a step body, so it was not refused before the body",
                kind.tag()
            );
            assert_eq!(
                observed.markers,
                (0, 0),
                "{tenant}/{}: a refused resume ran a step",
                kind.tag()
            );
        }
        Ok(())
    };
    sim::assert_replays(band, body)
}

/// One tenant of the compatible-resume sweep.
fn compatible_tenant(sim: &mut sim::Sim, tenant: &str) -> TestResult {
    let input = sim.rng().below(64);
    let dir = Scratch(sim.scratch("compatible")?);
    let host = host_for(tenant, &dir.0)?;
    let work = drift_task()?;
    let identity = first_identity(&host, input, FIRST_CODEC)?;
    let expected = input.saturating_add(input.saturating_add(1));
    let job = Job {
        input,
        dir: dir.0.clone(),
    };

    let first = lgwks_bot::block_on(host.run_under(&identity, &work, job.clone()));
    assert_eq!(
        first.disposition(),
        Disposition::Succeeded,
        "{tenant}: the first attempt must succeed"
    );
    assert_eq!(
        first.output(),
        Some(&expected),
        "{tenant}: the first attempt's own output"
    );
    let run = first.run_id().ok_or("a stored run must name its run id")?;
    assert_eq!(
        (ran(&dir.0, "alpha")?, ran(&dir.0, "beta")?),
        (1, 1),
        "{tenant}: each durable step ran exactly once"
    );
    drop(first);
    drop(host);

    let reopened = host_for(tenant, &dir.0)?;
    let before = constructions();
    let replayed = lgwks_bot::block_on(reopened.resume_under(run, &identity, &work, job));

    assert_eq!(
        replayed.disposition(),
        Disposition::Succeeded,
        "{tenant}: a compatible resume must succeed"
    );
    assert_eq!(
        replayed.output(),
        Some(&expected),
        "{tenant}: the resumed output is the recorded one, not a re-run's"
    );
    assert_eq!(
        constructions().saturating_sub(before),
        0,
        "{tenant}: a compatible resume constructed a step body, so a durable step was re-requested"
    );
    assert_eq!(
        (ran(&dir.0, "alpha")?, ran(&dir.0, "beta")?),
        (1, 1),
        "{tenant}: a compatible resume re-ran a durable step"
    );
    assert_eq!(
        RunStore::open(store_path(&dir.0, tenant))?.record_count(run),
        STEPS,
        "{tenant}: a compatible resume added a record"
    );

    sim.record(tenant);
    sim.trace.record_number("input", u64::from(input));
    sim.trace.record_number("output", u64::from(expected));
    Ok(())
}

/// A compatible resume replays every recorded step and runs no body.
///
/// The second half of T15: replayed model results incur no new request. The
/// evidence is the construction counter and the marker count, both untouched by
/// a resume whose every step is recorded — and the output must still be the sum
/// the first attempt produced, which is what proves the *values* were replayed
/// rather than the bodies having run and coincidentally matched.
fn compatible_resume_replays_without_a_new_request(band: sim::Band) -> TestResult {
    let body = |sim: &mut sim::Sim| -> TestResult {
        for tenant in TENANTS {
            compatible_tenant(sim, tenant)?;
        }
        Ok(())
    };
    sim::assert_replays(band, body)
}

/// Two tenants over two stores drift independently, through every kind.
///
/// The isolation claim is that one tenant's refusal is about its own records: a
/// check that consulted the other tenant's store would either succeed on a
/// compatible resume or refuse on a drifted one for the wrong reason. Both stores
/// live in one directory, which is the arrangement `HostBuilder::run_store`
/// builds — one file per tenant, named after it.
fn tenants_drift_independently(band: sim::Band) -> TestResult {
    let body = |sim: &mut sim::Sim| -> TestResult {
        let shared = Scratch(sim.scratch("tenants")?);
        for tenant in TENANTS {
            for kind in DriftKind::ALL {
                let observed = attempt(&shared.0, tenant, FIRST_INPUT, FIRST_CODEC, kind)?;
                sim.record(&format!("{tenant}:{}", kind.tag()));
                sim.trace
                    .record_number("drift", observed.drift.map_or(255, axis_id));

                assert_eq!(
                    observed.disposition,
                    if kind.expects().is_some() {
                        Disposition::Refused
                    } else {
                        Disposition::Succeeded
                    },
                    "{tenant}/{}: the disposition does not follow from the drift",
                    kind.tag()
                );
                assert_eq!(
                    observed.drift,
                    kind.expects(),
                    "{tenant}/{}: the refusal named the wrong axis",
                    kind.tag()
                );
                assert_eq!(
                    observed.records,
                    STEPS,
                    "{tenant}/{}: this tenant's record count moved",
                    kind.tag()
                );
            }
            assert!(
                store_path(&shared.0, tenant).is_file(),
                "{tenant}: the store is one file per tenant, named after it"
            );
        }
        Ok(())
    };
    sim::assert_replays(band, body)
}

/// The four axes are distinguishable, not one refusal wearing four names.
///
/// A caller has to know whether it needs a new run, a decision, a migration or
/// an edit, which is only possible if each kind reports its own axis.
fn every_axis_is_distinguishable(band: sim::Band) -> TestResult {
    let body = |sim: &mut sim::Sim| -> TestResult {
        let mut seen: Vec<&'static str> = Vec::new();
        for kind in DriftKind::ALL {
            let dir = Scratch(sim.scratch("axes")?);
            let observed = attempt(&dir.0, TENANTS[0], FIRST_INPUT, FIRST_CODEC, kind)?;
            if let Some(axis) = observed.drift {
                assert!(
                    !seen.contains(&axis),
                    "{axis} was reported twice: the axes are not distinguishable"
                );
                seen.push(axis);
            }
            sim.trace.record_number("axes", u64::try_from(seen.len())?);
        }
        assert_eq!(
            seen.len(),
            DriftKind::ALL.len().saturating_sub(1),
            "every drifted kind must report an axis, and each must be its own"
        );
        Ok(())
    };
    sim::assert_replays(band, body)
}

/// One tenant of the exact-replay sweep.
fn exact_tenant(sim: &mut sim::Sim, tenant: &str) -> TestResult {
    let dir = Scratch(sim.scratch("exact")?);
    let host = host_for(tenant, &dir.0)?;
    let work = drift_task()?;
    let first = first_identity(&host, FIRST_INPUT, FIRST_CODEC)?;
    let recorded_input = first.input();
    let job = Job {
        input: FIRST_INPUT,
        dir: dir.0.clone(),
    };
    let initial = lgwks_bot::block_on(host.run_under(&first, &work, job.clone()));
    assert_eq!(
        initial.disposition(),
        Disposition::Succeeded,
        "{tenant}: the first attempt must succeed before it can be drifted"
    );
    let run = initial
        .run_id()
        .ok_or("a stored run must name its run id")?;
    drop(initial);
    drop(host);

    let reopened = host_for(tenant, &dir.0)?;
    for kind in DriftKind::DRIFTED {
        let drifted = kind.declare(&first);
        let resumed = lgwks_bot::block_on(reopened.resume_under(run, &drifted, &work, job.clone()));
        let error = resumed.error().ok_or_else(|| -> Box<dyn Error> {
            format!("{tenant}/{}: a drifted resume must be refused", kind.tag()).into()
        })?;
        let drift = drift_of(error).ok_or_else(|| -> Box<dyn Error> {
            format!(
                "{tenant}/{}: the refusal must be FlowError::Incompatible",
                kind.tag()
            )
            .into()
        })?;
        sim.record(&format!("{tenant}:{}", kind.tag()));
        sim.trace.record_number("axis", axis_id(drift.kind()));
        assert_exact_drift(tenant, kind, drift, recorded_input)?;
    }
    Ok(())
}

/// Each axis is refused with its exact typed `Drift`, payloads included.
///
/// The typed half of T15 and R2. [`drift_axis`] checks *which* axis a refusal
/// named; this family checks *what it said*, by requiring
/// [`FlowError::Incompatible`] and destructuring the `Drift` down to the two
/// values the axis moved. A refusal that reported the right axis word while
/// carrying another run's revision, another input's digest, another flow's step
/// count or another codec id would satisfy an axis-only check and fail here,
/// which is what makes the payload part of the claim rather than decoration.
///
/// Every attempt goes through `Host::resume_under` over a **reopened** file
/// store — the first host is dropped and the resume is on a fresh handle over the
/// same bytes — so the typed refusal is the one a restart meets, not one a warm
/// in-memory index could have answered.
fn every_axis_is_refused_with_its_exact_drift(band: sim::Band) -> TestResult {
    let body = |sim: &mut sim::Sim| -> TestResult {
        for tenant in TENANTS {
            exact_tenant(sim, tenant)?;
        }
        Ok(())
    };
    sim::assert_replays(band, body)
}

/// One tenant of the drifted-resume sweep.
fn drifted_tenant(sim: &mut sim::Sim, tenant: &str) -> TestResult {
    let kind = drawn(sim, &DriftKind::DRIFTED)?;
    let dir = Scratch(sim.scratch("bytes")?);
    let path = store_path(&dir.0, tenant);

    let host = host_for(tenant, &dir.0)?;
    let work = drift_task()?;
    let first = first_identity(&host, FIRST_INPUT, FIRST_CODEC)?;
    let drifted = kind.declare(&first);
    let job = Job {
        input: FIRST_INPUT,
        dir: dir.0.clone(),
    };
    let initial = lgwks_bot::block_on(host.run_under(&first, &work, job.clone()));
    let run = initial
        .run_id()
        .ok_or("a stored run must name its run id")?;
    drop(initial);
    drop(host);

    let before = std::fs::read(&path)?;
    let reopened = host_for(tenant, &dir.0)?;
    let refused = lgwks_bot::block_on(reopened.resume_under(run, &drifted, &work, job));

    assert_eq!(
        refused.disposition(),
        Disposition::Refused,
        "{tenant}/{}: a drifted resume must be refused",
        kind.tag()
    );
    assert_eq!(
        refused.error().and_then(drift_axis),
        kind.expects(),
        "{tenant}/{}: the refusal must name this axis",
        kind.tag()
    );
    assert_eq!(
        std::fs::read(&path)?,
        before,
        "{tenant}/{}: the refusal moved bytes in the store",
        kind.tag()
    );

    sim.record(&format!("{tenant}:{}", kind.tag()));
    sim.trace
        .record_number("store-bytes", u64::try_from(before.len())?);
    Ok(())
}

/// A refused resume leaves the store byte-identical.
///
/// The device half of "before any new effect": a typed refusal that wrote a
/// partial frame, repaired a torn tail or moved the tail would change what a
/// restart recovers, so the file is compared against what it was before the
/// refusal rather than against its record count.
fn a_refusal_leaves_the_store_byte_identical(band: sim::Band) -> TestResult {
    let body = |sim: &mut sim::Sim| -> TestResult {
        for tenant in TENANTS {
            drifted_tenant(sim, tenant)?;
        }
        Ok(())
    };
    sim::assert_replays(band, body)
}

/// The format-version byte at the end of the header, the byte that differs
/// between formats.
const VERSION_BYTE: usize = 15;
/// The version byte a pre-version store carries, and the version this build reads.
///
/// Spelled out rather than imported: the claim under test is that the two are
/// *not* the same, so a check that read both from the crate would agree with a
/// revert. The two are literals here for the same reason `CURRENT_FORMAT` is in
/// `task_resume.rs` — the same fact, one owner each for the two questions that
/// read it (a store handle, and a host installing over a directory).
const PRE_VERSION: u8 = 1;
/// The version this build writes and reads.
const CURRENT_FORMAT: u8 = 2;

/// One tenant of the refused-resume sweep.
fn refused_tenant(sim: &mut sim::Sim, tenant: &str) -> TestResult {
    let input = sim.rng().below(64);
    let dir = Scratch(sim.scratch("format")?);
    let path = store_path(&dir.0, tenant);

    let host = host_for(tenant, &dir.0)?;
    let work = drift_task()?;
    let identity = first_identity(&host, input, FIRST_CODEC)?;
    let first = lgwks_bot::block_on(host.run_under(
        &identity,
        &work,
        Job {
            input,
            dir: dir.0.clone(),
        },
    ));
    assert_eq!(
        first.disposition(),
        Disposition::Succeeded,
        "{tenant}: the first attempt must succeed so there are real frames to downgrade"
    );
    drop(first);
    drop(host);

    // The shipped file becomes a `\x01` file: real committed frames under
    // a pre-version header, which is exactly the artifact a deployment on
    // the earlier format holds.
    let mut bytes = std::fs::read(&path)?;
    assert_eq!(
        bytes[VERSION_BYTE], CURRENT_FORMAT,
        "{tenant}: the store must be written at the current version before it is downgraded"
    );
    bytes[VERSION_BYTE] = PRE_VERSION;
    std::fs::write(&path, &bytes)?;
    let before = bytes.clone();

    let refusal = RunStore::open(&path)
        .err()
        .ok_or_else(|| -> Box<dyn Error> {
            format!("{tenant}: a pre-version store must not open").into()
        })?;
    let (found, expected) = shared::format_version(&refusal).ok_or_else(|| -> Box<dyn Error> {
        format!("{tenant}: a \\x01 store must be a version refusal, got: {refusal}").into()
    })?;
    assert_eq!(
        found, PRE_VERSION,
        "{tenant}: the refusal must name the version found"
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

    sim.record(tenant);
    sim.trace.record_number("found", u64::from(PRE_VERSION));
    sim.trace
        .record_number("expected", u64::from(CURRENT_FORMAT));
    sim.trace
        .record_number("store-bytes", u64::try_from(before.len())?);
    Ok(())
}

/// A store in an older format is refused naming both versions, and its bytes do
/// not move.
///
/// The file half of T15's "before any new effect". A record written without a
/// definition identity cannot be shown to be this build's own work, so the only
/// honest answer is a refusal — and the refusal has to distinguish "this is an
/// older version of *my* format" from "this was never my format at all", because
/// the second message is what makes an operator delete a file their system is
/// still relying on. This family writes a real store, rewrites its version byte
/// and reopens it, sweeping the tenant per seed so both stores are exercised, and
/// asserts the typed `FormatVersion { found, expected }` payload *and* that the
/// file is byte-identical afterwards.
fn a_pre_version_store_is_refused_naming_both_versions(band: sim::Band) -> TestResult {
    let body = |sim: &mut sim::Sim| -> TestResult {
        for tenant in TENANTS {
            refused_tenant(sim, tenant)?;
        }
        Ok(())
    };
    sim::assert_replays(band, body)
}

/// One tenant of the same-seed replay sweep.
fn replay_tenant(sim: &mut sim::Sim, tenant: &str) -> TestResult {
    let input = sim.rng().below(64);
    // Drawn before anything else, so the trace's first fact is the one that
    // selects the scenario rather than a value derived later.
    let armed = sim.rng().chance(500);
    let dir = Scratch(sim.scratch("read-failure")?);

    let host = host_for(tenant, &dir.0)?;
    let work = drift_task()?;
    let identity = first_identity(&host, input, FIRST_CODEC)?;
    let job = Job {
        input,
        dir: dir.0.clone(),
    };
    let first = lgwks_bot::block_on(host.run_under(&identity, &work, job.clone()));
    assert_eq!(
        first.disposition(),
        Disposition::Succeeded,
        "{tenant}: the first attempt must succeed before a read can fail"
    );
    let run = first.run_id().ok_or("a stored run must name its run id")?;
    drop(first);
    drop(host);

    // A fresh handle over a fresh replay of the file, so the resume below is
    // a restart rather than a map the first attempt left warm.
    let reopened = host_for(tenant, &dir.0)?;
    if armed {
        reopened
            .run_store()
            .ok_or("a stored host keeps a store")?
            .fail_next_index_read();
    }

    let before = constructions();
    let refused = lgwks_bot::block_on(reopened.resume_under(run, &identity, &work, job));

    if armed {
        let error = refused.error().ok_or_else(|| -> Box<dyn Error> {
            format!("{tenant}: a store that cannot be read must not succeed").into()
        })?;
        assert!(
            !matches!(error, FlowError::Incompatible { .. }),
            "{tenant}: a read failure must not be reported as a definition drift, got: \
             {error}"
        );
        // The typed half of INV-BOT-7: the refusal is the store's own,
        // reached by its variant and its wrapped kind, not by reading the
        // rendered text. A rendering that merely mentioned a device would
        // satisfy a `contains` while the payload was another arm.
        let refusal = shared::store_refusal(error).ok_or_else(|| -> Box<dyn Error> {
            format!("{tenant}: a read failure must reach the caller as FlowError::Store").into()
        })?;
        assert!(
            matches!(refusal, StoreError::Storage { .. }),
            "{tenant}: the wrapped refusal must be the device's own storage error, got: \
             {refusal:?}"
        );
        assert_eq!(
            constructions().saturating_sub(before),
            0,
            "{tenant}: a refused step constructed its body, so the fault did not stop it"
        );
    } else {
        assert_eq!(
            refused.disposition(),
            Disposition::Succeeded,
            "{tenant}: the control arm must replay, got: {:?}",
            refused.error()
        );
        assert_eq!(
            constructions().saturating_sub(before),
            0,
            "{tenant}: the control arm re-ran a durable step"
        );
    }

    sim.record(tenant);
    sim.trace.record_number("armed", u64::from(armed));
    sim.trace.record_number(
        "disposition",
        shared::disposition_code(refused.disposition()),
    );
    Ok(())
}

/// A store that cannot be read is refused as itself, never as a drift.
///
/// The seed draws whether the read fault is armed at all, so the sweep carries the
/// control beside the fault rather than only the fault. That matters here more
/// than in most families: the property INV-BOT-7 names — a read failure reaches the
/// caller as itself — is only meaningful against a store that would otherwise have
/// succeeded, and a sweep of nothing but refusals would be satisfied by a store
/// that refuses everything.
///
/// Every armed arm must agree on two things. The refusal is never
/// [`FlowError::Incompatible`], which is the defect this family exists to catch:
/// the old check turned a device error into `false` and then rendered that as a
/// claim about a definition. And the step's body was never constructed, because a
/// store that cannot say whether it holds a record must not run the work as though
/// it held none.
fn an_unreadable_store_is_refused_as_itself(band: sim::Band) -> TestResult {
    let body = |sim: &mut sim::Sim| -> TestResult {
        for tenant in TENANTS {
            replay_tenant(sim, tenant)?;
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
    // One body for both halves. The replay check and the distinct-seed check
    // differ only in what they do with the hashes afterwards, and a scenario
    // spelled out twice is two scenarios that can drift — which is how a
    // distinct-seed check ends up comparing two runs that never differed.
    let one_run = |sim: &mut sim::Sim| -> Result<(), Box<dyn Error>> {
        for tenant in TENANTS {
            let kind = drawn(sim, &DriftKind::ALL)?;
            let dir = Scratch(sim.scratch("replay")?);
            let observed = attempt(&dir.0, tenant, FIRST_INPUT, FIRST_CODEC, kind)?;
            sim.record(&format!("{tenant}:{}", kind.tag()));
            sim.trace.record_number("records", observed.records);
            sim.trace
                .record_number("constructions", observed.constructions);
            sim.trace
                .record_number("alpha", u64::from(observed.markers.0));
            sim.trace
                .record_number("beta", u64::from(observed.markers.1));
        }
        Ok(())
    };

    // The replay receipt: two sweeps of one band must agree on every hash.
    sim::assert_replays(band, one_run)?;

    // A different seed must reach a different trace, or the hash is a constant
    // and proves nothing about replay. The seed decides which arm each tenant
    // runs, so two seeds reach different traces without the scenario
    // introducing a difference of its own — which is what makes "the traces
    // differ" evidence about the seed rather than about the harness.
    let first = sim::sweep(sim::Band::new(1, 1), one_run)?;
    let second = sim::sweep(sim::Band::new(2, 1), one_run)?;
    assert_ne!(
        first, second,
        "two seeds produced the same trace hash, so the hash is not a receipt of anything this scenario decided"
    );
    Ok(())
}

band_family::band_family! {
    drift_kinds_are_refused_typed_band_00 => drift_kinds_are_refused_typed, 0;
    drift_kinds_are_refused_typed_band_01 => drift_kinds_are_refused_typed, 1;
    drift_kinds_are_refused_typed_band_02 => drift_kinds_are_refused_typed, 2;
    drift_kinds_are_refused_typed_band_03 => drift_kinds_are_refused_typed, 3;
    compatible_resume_replays_without_a_new_request_band_04 => compatible_resume_replays_without_a_new_request, 4;
    compatible_resume_replays_without_a_new_request_band_05 => compatible_resume_replays_without_a_new_request, 5;
    compatible_resume_replays_without_a_new_request_band_06 => compatible_resume_replays_without_a_new_request, 6;
    compatible_resume_replays_without_a_new_request_band_07 => compatible_resume_replays_without_a_new_request, 7;
    tenants_drift_independently_band_08 => tenants_drift_independently, 8;
    tenants_drift_independently_band_09 => tenants_drift_independently, 9;
    tenants_drift_independently_band_10 => tenants_drift_independently, 10;
    every_axis_is_distinguishable_band_11_t15 => every_axis_is_distinguishable, 11;
    a_refusal_leaves_the_store_byte_identical_band_12 => a_refusal_leaves_the_store_byte_identical, 12;
    a_refusal_leaves_the_store_byte_identical_band_13 => a_refusal_leaves_the_store_byte_identical, 13;
    a_pre_version_store_is_refused_naming_both_versions_band_16 => a_pre_version_store_is_refused_naming_both_versions, 16;
    a_pre_version_store_is_refused_naming_both_versions_band_17 => a_pre_version_store_is_refused_naming_both_versions, 17;
    an_unreadable_store_is_refused_as_itself_band_18 => an_unreadable_store_is_refused_as_itself, 18;
    an_unreadable_store_is_refused_as_itself_band_19 => an_unreadable_store_is_refused_as_itself, 19;
    every_axis_is_refused_with_its_exact_drift_band_20 => every_axis_is_refused_with_its_exact_drift, 20;
    every_axis_is_refused_with_its_exact_drift_band_21 => every_axis_is_refused_with_its_exact_drift, 21;
    same_seed_same_trace_hash_band_14 => same_seed_same_trace_hash, 14;
    same_seed_same_trace_hash_band_15 => same_seed_same_trace_hash, 15;
}
