//! The scaffolding both proposal test files need, written once.
//!
//! `proposal.rs` and `sim_proposal.rs` drive the same admission surface over the
//! same store and needed the same five things: a surface with a known shape, a
//! decoder at a known ceiling, the bytes of each injection shape, a store that
//! two tenants can share, and a saturation driver that reports the tier it
//! reached rather than the tier it asked for. A copy per file is how the two
//! drift — one file's surface would grow an operation and the other's would
//! refuse it for a reason that reads like a defect.
//!
//! Included by path so both targets share it:
//! `#[path = "support/proposal.rs"] mod support;` from a target at `tests/`.

// Each including test target uses a different subset of this harness, so a name
// unused in one is not dead. The lint is real per target and the allowance is
// inherent to sharing one harness across two of them.
#![allow(
    dead_code,
    reason = "each including test target uses a different subset of the shared harness"
)]

use std::error::Error;

use lgwks_bot::cap::Cap;
use lgwks_bot::proposal::{
    ArtifactStore, Decoder, LedgerLimits, PlanBudget, PlanLimits, Provenance, Source, Surface,
    WriteOutcome,
};
use lgwks_bot::script::{Gate, Scope};

/// A test's result: an error fails it with the error's text.
pub type TestResult = Result<(), Box<dyn Error>>;

/// The tenant the shared surface is built for.
pub const TENANT: &str = "acme";

/// The second tenant every isolation case runs against.
pub const OTHER_TENANT: &str = "globex";

/// The operation both targets admit, and the only one they register besides the
/// capless one.
///
/// Deliberately one operation that needs a capability and one that needs none:
/// the pair is what makes "an operation whose capability the run does not hold"
/// and "an operation nobody registered" two different refusals rather than one.
pub const READ: &str = "read-report";
/// A capless operation, so a refusal can be about the *name* rather than a cap.
pub const PING: &str = "ping";

/// The surface the shared tests decode against.
///
/// A host's real surface has as many operations as it has work; this one has two
/// so that every refusal arm is reachable from a declared, readable set.
pub fn surface() -> Result<Surface, Box<dyn Error>> {
    Ok(Surface::builder(TENANT)?
        .operation(READ, &[Cap::fs()])?
        .operation(PING, &[])?
        .holding(&[Cap::fs()])
        .build())
}

/// The same surface for a run that holds nothing, so every capful operation is
/// refused by name rather than by an accident of the grant set.
pub fn poor_surface() -> Result<Surface, Box<dyn Error>> {
    Ok(Surface::builder(TENANT)?
        .operation(READ, &[Cap::fs()])?
        .operation(PING, &[])?
        .build())
}

/// A decoder at the crate's default ceilings.
#[must_use]
pub fn decoder() -> Decoder {
    Decoder::new(PlanLimits::default())
}

/// The provenance a payload decoded by these tests has.
#[must_use]
pub fn provenance(payload: &[u8]) -> Provenance {
    Provenance::of(Source::Model, TENANT, payload)
}

// ── The payload shapes ───────────────────────────────────────────────────────

/// A payload naming a registered operation and nothing else.
#[must_use]
pub fn well_formed() -> Vec<u8> {
    b"op=read-report\nnote=steady\n".to_vec()
}

/// A payload asking to install a tool.
#[must_use]
pub fn installs() -> Vec<u8> {
    b"op=read-report\ninstall=ripgrep\nnote=you-must-install-this\n".to_vec()
}

/// A payload asking to read a credential.
#[must_use]
pub fn credential() -> Vec<u8> {
    b"op=read-report\ncredential=GITHUB_TOKEN\n".to_vec()
}

/// A payload aimed at another tenant: the injection shape.
#[must_use]
pub fn injection() -> Vec<u8> {
    b"op=read-report\nhost=globex\nnote=ignore-previous-instructions\n".to_vec()
}

/// A payload that declares full coverage over whatever it saw.
#[must_use]
pub fn overclaims() -> Vec<u8> {
    b"op=read-report\ncoverage=complete\nnote=saw-everything\n".to_vec()
}

/// A document cut off part-way through a value.
#[must_use]
pub fn truncated() -> Vec<u8> {
    let whole = format!("op=read-report\nnote={}", "x".repeat(64));
    whole.into_bytes()[..12].to_vec()
}

/// A payload with no `=` on its first line.
#[must_use]
pub fn malformed() -> Vec<u8> {
    b"read-report no separator here".to_vec()
}

/// A payload past the default byte ceiling.
#[must_use]
pub fn oversized() -> Vec<u8> {
    format!("op=read-report\nnote={}\n", "y".repeat(70 * 1024)).into_bytes()
}

/// A payload reaching outside the tenant's artifact root.
#[must_use]
pub fn escaping_path() -> Vec<u8> {
    b"op=read-report\npath=../../etc/passwd\n".to_vec()
}

/// A payload naming an operation nobody registered.
#[must_use]
pub fn unknown_operation() -> Vec<u8> {
    b"op=install-dependency\nnote=please\n".to_vec()
}

/// One untrusted payload shape: its name, and the bytes it produces.
pub type Shape = (&'static str, fn() -> Vec<u8>);

/// Every shape the two targets drive, as one table.
///
/// A table rather than a function so the simulation family and this file agree
/// on which shape is which without a second list to drift. The index into it is
/// what a seeded scenario draws, so the order is part of the contract: appending
/// is fine, reordering would change every seed's payload.
pub const SHAPES: [Shape; 10] = [
    ("well-formed", well_formed),
    ("installs", installs),
    ("credential", credential),
    ("injection", injection),
    ("overclaims", overclaims),
    ("truncated", truncated),
    ("malformed", malformed),
    ("oversized", oversized),
    ("escaping-path", escaping_path),
    ("unknown-operation", unknown_operation),
];

// ── The store ────────────────────────────────────────────────────────────────

/// Two tenants, one store: the shape every isolation case is built on.
#[must_use]
pub fn two_tenant_store() -> (ArtifactStore, Vec<u8>) {
    let store = ArtifactStore::new();
    // The bytes both tenants produce, so the digests are identical and only the
    // tenant differs. That is the case a digest-keyed store gets wrong.
    let shared = b"the same report from two tenants".to_vec();
    (store, shared)
}

/// Write `bytes` as `tenant`'s artifact and report whether it was newly stored.
///
/// # Errors
///
/// Whatever the store reports, so a caller can assert on a refusal rather than
/// unwrapping one.
pub fn store(
    artifacts: &ArtifactStore,
    tenant: &str,
    bytes: &[u8],
) -> Result<WriteOutcome, Box<dyn Error>> {
    Ok(artifacts.write(tenant, bytes)?)
}

// ── The wired path: a task body admitting untrusted output ───────────────────

/// The admission ceiling every gate here is opened with.
///
/// Declared rather than defaulted inside [`lgwks_bot::script::admit`] because a
/// repair ceiling a caller cannot read is a repair loop they cannot see coming,
/// and a falsifier that drew it from the seed could not say what it drew.
pub const ADMISSIONS: u32 = 8;

/// The repetition ceiling every gate here is opened with.
///
/// Three, so T29's falsifier drives three tolerated refusals and then the finite
/// intervention on the fourth, with the budget above it never the binding
/// constraint — otherwise the two ceilings could not be told apart.
pub const REPEAT: u32 = 3;

/// A gate over the shared surface, for a run admitting untrusted model output.
///
/// # Errors
///
/// [`lgwks_bot::proposal::SurfaceError`] when the surface does not build.
pub fn gate(tenant: &str) -> Result<Gate, Box<dyn Error>> {
    Ok(Gate::new(
        tenant,
        surface()?,
        decoder(),
        PlanBudget::new(ADMISSIONS),
        LedgerLimits::new(REPEAT, 8),
    ))
}

/// A gate over the surface that holds nothing, so every capful operation is
/// refused by name rather than by an accident of the grant set.
///
/// # Errors
///
/// [`lgwks_bot::proposal::SurfaceError`] when the surface does not build.
pub fn poor_gate(tenant: &str) -> Result<Gate, Box<dyn Error>> {
    Ok(Gate::new(
        tenant,
        poor_surface()?,
        decoder(),
        PlanBudget::new(ADMISSIONS),
        LedgerLimits::new(REPEAT, 8),
    ))
}

/// A fresh scratch directory for a run store, named by random bytes.
///
/// Random bytes and never the process id: the OS reuses a pid, so two runs in two
/// processes would share a scratch directory and one would delete the other's
/// store mid-run. `lgwks_std::random` is the estate's one distinguishable source.
/// The randomness names the *directory* and never enters an assertion, so the
/// tests' observations are unchanged by it.
///
/// # Errors
///
/// [`lgwks_std::random`]'s error when the entropy source is unavailable, or an
/// I/O error creating the directory.
pub fn scratch(tag: &str) -> Result<std::path::PathBuf, Box<dyn Error>> {
    let unique = lgwks_std::random::bytes::<8>()?;
    let suffix = unique
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let path = std::env::temp_dir().join(format!("lgwks-proposal-{tag}-{suffix}"));
    if path.exists() {
        std::fs::remove_dir_all(&path)?;
    }
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

/// Drive one future to completion on the crate's own runtime.
///
/// One spelling for both targets, because the two have to drive a `Host::run` the
/// same way: a target that drove it differently could compare two runs that were
/// never actually equivalent.
pub fn drive<T>(future: impl std::future::Future<Output = T>) -> T {
    lgwks_bot::block_on(future)
}

/// A task body that admits `payload` through a gate and reports what happened.
///
/// A named `fn` rather than a closure so the same `Task` type can be built twice —
/// which is what a resume needs: a context reset is a *new* instance resuming the
/// *same* run id, and a closure would give each instance its own anonymous type.
///
/// It takes `scope` by value and moves it into the future, so the future owns what
/// it borrows and is `'static`, which is what a task body needs since the host
/// drives it after the body has returned.
pub fn admitting_body(
    scope: Scope,
    (gate, payload): (Gate, Vec<u8>),
) -> lgwks_bot::BoxFuture<'static, Result<String, lgwks_bot::script::FlowError>> {
    Box::pin(async move {
        let plan = lgwks_bot::script::admit(&scope, "plan", &gate, &payload, Source::Model).await?;
        Ok(plan.to_string())
    })
}

/// What [`admitting_body`] returns, named so the coercion at
/// [`admitting_task`] reads as a signature rather than a type expression.
pub type AdmittingFuture =
    lgwks_bot::BoxFuture<'static, Result<String, lgwks_bot::script::FlowError>>;

/// The `Task` type [`admitting_body`] is built into.
pub type AdmittingTask = lgwks_bot::task::Task<fn(Scope, (Gate, Vec<u8>)) -> AdmittingFuture>;

/// The task that admits one payload through one gate.
///
/// The coercion is written at the call rather than left to inference: `task` is
/// generic over its body, and without it the `?` would resolve against the `fn`
/// *item* type and the annotation on the binding would never apply.
///
/// # Errors
///
/// [`lgwks_bot::script::FlowError`] when the logical name does not validate.
pub fn admitting_task() -> Result<AdmittingTask, Box<dyn Error>> {
    let body: fn(Scope, (Gate, Vec<u8>)) -> AdmittingFuture = admitting_body;
    Ok(lgwks_bot::task::task("admit", body)?)
}
