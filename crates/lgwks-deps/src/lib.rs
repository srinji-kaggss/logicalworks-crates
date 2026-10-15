//! `lgwks_deps` owns dependency admission and enforces
//! INV-DEP-EDGE-OWNED: every external dependency authored by a workspace
//! package names its semantic owner, capability, source, requirement, allowed
//! consumers, and allowed dependency kinds.
//!
//! The rule in one line: *if a library is not in `core`, it is not an approved
//! dependency, and the code does not compile until a human has registered it in
//! the semantic contract.* Everything here exists to
//! make the second half of that sentence mechanically true rather than a
//! convention people remember.
//!
//! ## Enforcement boundary
//!
//! The same `lgwks-deps check` command is an explicit first job in local and
//! remote CI. It reads `cargo metadata --no-deps`, not the transitive lockfile
//! closure, because only metadata preserves the package that authored an edge.
//! Embedders call [`check_dependencies`] for the identical verdict.
//!
//! ## Fail-closed
//!
//! A missing register, unparseable metadata, an unparseable register, and an unreadable lock file are
//! all refusals. A gate that passes when it cannot find its own contract is a
//! gate that reports success for the one condition it exists to catch. The only
//! way to stand enforcement down is `enforce = false` under `[policy]` in the
//! register itself: a reviewable diff carrying a human's name, never an
//! environment variable a build can set for itself.
//!
//! Adoption mode is a posture, not an off switch. It is supported over a tree
//! this gate admits, and a clean tree still passes under it. It cannot stand a
//! violating tree down: `enforce = false` over refusals adds a named
//! [`Refusal::AdoptionModeRefusals`] reporting how many violations the posture
//! stood down, so `check` exits non-zero identically under `enforce = true` and
//! `enforce = false`. The verdict is a function of the refusals alone, which is
//! what makes the reviewable one-token diff that adoption mode depends on
//! incapable of changing a build's result (#204).
//!
//! ## No name-based exemption
//!
//! There is no package whose name alone escapes the audit. An edge is exempt
//! only when Cargo's own `workspace_members` list names its target, so an
//! internal edge is exempt because it is *this* workspace's code and not
//! because of how it is spelled. A path or Git package that calls itself
//! `lgwks_std` — or `lgwks-std`, which Cargo folds to the same name — is an
//! ordinary external edge and must be approved like any other; the refusal
//! keeps the source it came from so the two cannot be confused.
//!
//! The gate is not gated by itself, but that is a consequence of the build
//! graph rather than an exemption: the `lgwks_deps` → `lgwks_std` edge in this
//! repository targets a workspace member, and a consumer that reaches a
//! *published* `lgwks_std` declares a real registry edge, which the register
//! must review. Building the checker and interpreting a subject's metadata are
//! different operations, so auditing a same-named package creates no cycle.
//!
//! ## Storefront
//!
//! `lgwks_deps` is also the install-and-select surface for third-party
//! engines: enable a storefront feature and import its engine through this
//! crate. For example, `bevy-app` exposes `lgwks_deps::bevy_app::App`,
//! `bevy-time` exposes `lgwks_deps::bevy_time::Time`, and `bevy-state`
//! exposes Bevy's state API.
//! `tokio` is the async engine behind `lgwks_bot::rt` (`use
//! lgwks_deps::tokio::...` only when bypassing the bot facade), and `gpui` is
//! the GPU desktop UI. Capability features are default-off; `scan`, the
//! gate-tool feature, is the default-on exception for `cargo install` CLI use.
//! Library consumers take `default-features = false` plus exactly the engine
//! they need so the Rust parser never rides along with a runtime edge.
//!
//! Do NOT `cargo add tokio` / `cargo add gpui` directly: the gate refuses any
//! second edge, and the facade (`lgwks_bot::rt`, `lgwks_deps::tokio`) is the
//! single entry this workspace audits.

// Lint contract (missing_docs deny, unsafe_code forbid, broken intra-doc
// links deny) comes from the workspace root.

/// The approval register: parsing and lookup for `contract/APPROVED.toml`.
pub mod contract;
/// The optional authored invariant register and its repository-aware validator.
pub mod invariants;
/// The register: parsing and approval lookup for `contract/APPROVED.toml`.
pub mod lock;
/// Cargo metadata edges: who authored which external dependency.
pub mod metadata;
/// Rust source scan: the zero-gate detectors behind `lgwks-deps scan` (feature `scan`).
#[cfg(feature = "scan")]
pub mod scan;
/// Vendor-tree coverage: binding the lockfile to the shared `vendor/` tree.
pub mod vendor;

/// The audit itself: every direct edge judged against the register.
mod audit;
/// What the gate refuses and why it could not reach a verdict.
mod refusal;

pub use audit::{audit_direct, declared_scope};
pub use refusal::{GateError, Refusal};

#[cfg(any(feature = "macro", feature = "script"))]
pub use proc_macro2;
#[cfg(feature = "macro")]
pub use quote;
#[cfg(feature = "macro")]
pub use syn;
/// The one authored `tokio` edge, re-exported for the storefront.
///
/// `lgwks_deps` owns this edge so no other crate declares `tokio` directly
/// (`INV-DEP-EDGE-OWNED`). `lgwks_bot` enables the `tokio` storefront feature
/// and reaches the engine through this re-export; a consumer that wants the
/// engine without the bot enables the feature here.
#[cfg(feature = "tokio")]
pub use tokio;

/// The Bevy ECS substrate, selected through the storefront.
///
/// `lgwks_deps` owns this edge so no other crate declares `bevy_ecs` directly
/// (`INV-DEP-EDGE-OWNED`). `lgwks_bot` enables the `bevy-ecs` storefront feature
/// and reaches the crate through this re-export, which is the same rule the
/// `tokio` edge follows.
#[cfg(feature = "bevy-ecs")]
pub use bevy_ecs;

/// The Bevy application layer: the `App` that owns schedules and plugins.
///
/// Re-exported for the same reason as [`bevy_ecs`]. It is a separate feature
/// from `bevy-ecs` because it is a separate decision: a consumer running ECS
/// systems inside its own loop needs the substrate and not the application
/// layer, and the two carry different transitive cost.
///
/// ```
/// use lgwks_deps::bevy_app::App;
///
/// let _app = App::new();
/// ```
#[cfg(feature = "bevy-app")]
pub use bevy_app;

/// Bevy's time sources, including the virtual clock.
///
/// ```
/// use lgwks_deps::bevy_time::{Time, TimePlugin};
///
/// let _time = Time::<lgwks_deps::bevy_time::Real>::default();
/// let _plugin = TimePlugin;
/// ```
#[cfg(feature = "bevy-time")]
pub use bevy_time;

/// Bevy's state machines and their run conditions.
///
/// Derive macros resolve the upstream crate name from the consumer manifest.
/// Alias this facade module at the consumer crate root before deriving:
///
/// ```
/// use lgwks_deps::bevy_state as bevy_state;
/// use lgwks_deps::bevy_state::prelude::States;
///
/// #[derive(Clone, Debug, Default, Eq, Hash, PartialEq, States)]
/// enum Screen { #[default] Loading, Ready }
///
/// let _initial = Screen::default();
/// ```
#[cfg(feature = "bevy-state")]
pub use bevy_state;

/// The optional GPUI desktop UI framework selected through the storefront.
/// Re-exported so consumers name the storefront, never the crate.
#[cfg(feature = "gpui")]
pub use gpui;

/// Native terminal widgets and input-driven drawing, selected explicitly.
///
/// ```
/// use lgwks_deps::appcui;
/// use appcui::prelude::*;
/// let _layout = layout!("x:0,y:0,w:20,h:5");
/// ```
#[cfg(feature = "appcui")]
pub use appcui;

/// Minimalist tensor compute and safetensors loading, selected explicitly.
///
/// The storefront owns this edge so no consumer declares `candle-core`
/// directly. Weights are loaded from local files; the feature is default-off.
#[cfg(feature = "ml-candle")]
pub use candle_core;

/// Neural network layers and parameter containers built on `candle-core`.
#[cfg(feature = "ml-candle")]
pub use candle_nn;

/// Reference transformer model implementations built on `candle-core`.
///
/// Selecting this feature also compiles `hf-hub`, which is network-capable;
/// the runtime reads a local checkpoint and does not call the hub.
#[cfg(feature = "ml-candle")]
pub use candle_transformers;

/// Vocabulary-driven subword tokenisation matching published checkpoints.
#[cfg(feature = "ml-tokenizers")]
pub use tokenizers;

use std::path::{Path, PathBuf};

use contract::Contract;
use metadata::DirectEdge;

/// Register location relative to the repository root.
pub const CONTRACT_PATH: &str = "contract/APPROVED.toml";

// ── Filesystem entry points ─────────────────────────────────────────────────

/// Walks up from `start` to the nearest directory holding a `Cargo.lock`.
pub fn repository_root(start: &Path) -> Result<PathBuf, GateError> {
    let mut cursor = Some(start);
    while let Some(dir) = cursor {
        if dir.join("Cargo.lock").is_file() {
            return Ok(dir.to_path_buf());
        }
        cursor = dir.parent();
    }
    Err(GateError::LockNotFound {
        from: start.to_path_buf(),
    })
}

/// Refuses when the register file is absent.
///
/// This is the fail-closed hinge: a repo with no register has not been audited,
/// and reporting "no refusals" for it would make the one condition the gate
/// exists to catch the one condition it passes. Callers reach here before any
/// audit runs, so a missing register is `GateError::ContractNotFound` rather
/// than an empty refusal list.
fn ensure_contract_file(path: &Path) -> Result<(), GateError> {
    if !path.is_file() {
        Err(GateError::ContractNotFound {
            path: path.to_path_buf(),
        })
    } else {
        Ok(())
    }
}

/// The exact metadata subject an audit ran against.
///
/// A receipt that names only the root cannot tell two runs over the same tree
/// with a changed manifest apart. The subject fingerprint binds the direct-edge
/// identities Cargo reported, so the receipt names the graph as audited.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Subject {
    /// Stable fingerprint over every direct edge's identity.
    digest: String,
    /// How many direct edges the fingerprint covers.
    edges: usize,
}

impl Subject {
    /// The stable fingerprint of the audited direct-edge graph.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// How many direct edges the fingerprint covers.
    #[must_use]
    pub const fn edges(&self) -> usize {
        self.edges
    }
}

/// One complete gate verdict: the register read, the subject it was audited
/// against, and the refusals.
///
/// This is what a receipt binds: `contract` and `subject` are the two identities
/// the CLI stamps beside the verdict, and the policy mode is the caller's
/// (committed enforcement or `--contract` diagnosis).
#[derive(Debug)]
#[non_exhaustive]
pub struct Verdict {
    /// The parsed register the subject was audited against.
    register: Contract,
    /// Every refusal, sorted deterministically.
    refusals: Vec<Refusal>,
    /// The exact metadata subject.
    subject: Subject,
}

impl Verdict {
    /// The approved-dependency register this verdict was judged against, as parsed from `contract/APPROVED.toml`.
    #[must_use]
    pub fn register(&self) -> &Contract {
        &self.register
    }

    /// Every refusal, sorted.
    #[must_use]
    pub fn refusals(&self) -> &[Refusal] {
        &self.refusals
    }

    /// The exact metadata subject.
    #[must_use]
    pub fn subject(&self) -> &Subject {
        &self.subject
    }
}

/// A stable fingerprint of the audited direct-edge graph.
///
/// Every identity the audit distinguishes is framed into the digest, so a
/// changed feature, target, rename or requirement moves it. See
/// [`Contract::digest`] for why this is FNV-1a rather than the estate's BLAKE3.
fn subject_fingerprint(edges: &[DirectEdge]) -> String {
    const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;
    let mut hash = OFFSET;
    for edge in edges {
        let line = format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}:{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\n",
            edge.consumer,
            edge.package,
            edge.requirement,
            edge.kind.as_str(),
            edge.source.class(),
            edge.source.detail(),
            edge.features.join(","),
            edge.uses_default_features,
            edge.optional,
            declared_scope(edge.target.as_deref()),
        );
        for byte in line.as_bytes() {
            hash ^= u128::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
    }
    format!("fnv1a128:{hash:032x}")
}

/// Audits the repository rooted at `root`, reading its lock file and register.
///
/// The verdict comes back inside a [`metadata::Collected`]: a Cargo capture
/// file that could not be removed after a complete read is reported beside the
/// verdict rather than replacing it.
pub fn check_dependencies(
    root: &Path,
) -> Result<metadata::Collected<(Contract, Vec<Refusal>)>, GateError> {
    check_dependencies_against(root, &root.join(CONTRACT_PATH))
}

/// Audits `root`, returning the full receipt-bearing [`Verdict`].
///
/// The register is `contract_path` when `Some`, else the committed
/// `contract/APPROVED.toml` beside `root`. The distinction is the receipt's
/// policy mode: a committed register is enforcement, an override is diagnosis.
pub fn check_verdict(
    root: &Path,
    contract_path: Option<&Path>,
) -> Result<metadata::Collected<Verdict>, GateError> {
    let lock_path = root.join("Cargo.lock");
    let contract_path = match contract_path {
        Some(path) => path.to_path_buf(),
        None => root.join(CONTRACT_PATH),
    };
    ensure_contract_file(&contract_path)?;
    let register = Contract::parse(&read(&contract_path)?).map_err(GateError::Contract)?;
    // Parsed, not merely read. Reading proved the file exists; parsing is what
    // makes its contents auditable. A `[[package]]` block that names no package
    // would otherwise shrink the graph this gate reasons about without saying
    // so -- the failure `lock::parse` exists to refuse, reached only by the two
    // commands that happened to call it.
    lock::parse(&read(&lock_path)?).map_err(GateError::Lock)?;
    let edges = metadata::read_with_members(root).map_err(GateError::Metadata)?;
    Ok(edges.map(|(edges, members)| {
        let mut refusals = audit_direct(&edges, &register);
        refusals.extend(audit::surface_refusals(&members, &register));
        Verdict {
            register,
            refusals,
            subject: Subject {
                digest: subject_fingerprint(&edges),
                edges: edges.len(),
            },
        }
    }))
}

/// Audits `root` against a register held elsewhere. This exists for the `check
/// --contract` diagnosis path, where a repo is audited *before* it carries a
/// register of its own. `enforce` never calls it: a build always reads the
/// register committed beside the code it is building, so no build can be
/// pointed at a more permissive contract than the one in its own tree.
pub fn check_dependencies_against(
    root: &Path,
    contract_path: &Path,
) -> Result<metadata::Collected<(Contract, Vec<Refusal>)>, GateError> {
    Ok(check_verdict(root, Some(contract_path))?
        .map(|verdict| (verdict.register, verdict.refusals)))
}

/// Resolves the optional invariant register beside `root` against the
/// repository.
///
/// `Ok(None)` is the compatibility path for a repository that has not authored
/// `contract/INVARIANTS.toml`; its dependency-register verdict is unchanged.
///
/// The second half of the result is an [`invariants::Audit`], not a refusal
/// list: an audit carries a per-entry status, so a caller can tell an entry
/// whose references resolved (`Resolved`) from one a recorded run attests
/// (`Attested`). Neither is proof that the invariant holds — this crate
/// executes nothing — and [`invariants::SCOPE`] is the sentence that says so.
pub fn check_invariants(
    root: &Path,
) -> Result<Option<metadata::Collected<(invariants::Register, invariants::Audit)>>, GateError> {
    invariants::check(root).map_err(GateError::Invariant)
}

/// Reads a file to a `String`, naming the path in the failure.
///
/// Only UTF-8 text is accepted: the register and the lock file are both text,
/// and a lossy read would let a malformed byte sequence silently change what the
/// gate parsed. The underlying `io::Error` is preserved as [`GateError::Unreadable`]'s
/// source so the caller can distinguish absent from unreadable.
fn read(path: &Path) -> Result<String, GateError> {
    std::fs::read_to_string(path).map_err(|cause| GateError::Unreadable {
        path: path.to_path_buf(),
        cause,
    })
}

// ── Tests ───────────────────────────────────────────────────────────────────

/// The audit's unit tests, kept at `lgwks_deps::tests` because INVARIANTS.md
/// names them by that path.
#[cfg(test)]
mod tests;
