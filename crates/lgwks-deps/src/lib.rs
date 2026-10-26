#![doc = include_str!("lexicon.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]
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
//! The supported interface is `script!` (`lgwks_bot::script!`). The gate, its
//! register types and the storefront re-exports are the support code under it:
//! each item is `#[doc(hidden)]`, still reachable by the sibling crates, by the
//! `lgwks-deps` binaries and by this crate's own tests, and kept out of the
//! documented surface by #526. This page keeps the prose that explains the gate.
//!
//! ## Enforcement boundary
//!
//! The same `lgwks-deps check` command is an explicit first job in local and
//! remote CI. It reads `cargo metadata --no-deps`, not the transitive lockfile
//! closure, because only metadata preserves the package that authored an edge.
//! Embedders call `check_dependencies` for the identical verdict.
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
//! `Refusal::AdoptionModeRefusals` reporting how many violations the posture
//! stood down, so `check` exits non-zero identically under `enforce = true` and
//! `enforce = false`. The verdict is a function of the refusals alone, which is
//! what makes the reviewable one-token diff that adoption mode depends on
//! incapable of changing a build's result (#204).
//!
//! ## No name-based exemption
//!
//! There is no package whose name alone escapes the audit. An edge is exempt
//! when Cargo's own `workspace_members` list names its target — an internal edge
//! is exempt because it is *this* workspace's code, not because of how it is
//! spelled — or when it is an **estate edge**: a crate in the estate's published
//! `lgwks_` namespace *and* resolved from the canonical crates.io registry, which
//! is rung 2 of the ladder and the `estate` tier (#701). Both halves are
//! required, so a path or Git package that calls itself `lgwks_std` — or
//! `lgwks-std`, which Cargo folds to the same name — is an ordinary external
//! edge and must be approved like any other; the refusal keeps the source it came
//! from so the two cannot be confused.
//!
//! The gate is not gated by itself, but that is a consequence of the build
//! graph rather than an exemption: the `lgwks_deps` → `lgwks_std` edge in this
//! repository targets a workspace member, and a consumer that reaches a
//! *published* `lgwks_std` declares a real registry edge, which rung 2 admits for
//! every such crate at once rather than one register entry per consumer.
//! Building the checker and interpreting a subject's metadata are different
//! operations, so auditing a same-named package creates no cycle.
//!
//! ## Storefront
//!
//! `lgwks_deps` is also the install-and-select surface for third-party
//! engines: enable a storefront feature and import its engine through this
//! crate. The re-exports are `#[doc(hidden)]`: reachable and audited, but
//! outside the supported interface (#526). For example, `bevy-app` exposes `lgwks_deps::bevy_app::App`,
//! `bevy-time` exposes `lgwks_deps::bevy_time::Time`, and `bevy-state`
//! exposes Bevy's state API.
//! `tokio` is the async engine behind `lgwks_bot::rt` (`use
//! lgwks_deps::tokio::...` only when bypassing the bot facade), and `gpui` is
//! the GPU desktop UI. The default selection is empty, and that is a decision
//! rather than an omission (ADR 0003): this crate's no-flag build is a required
//! portability claim for every declared target, `wasm32-wasip1` among them, and
//! every storefront group is either native (`gpui`, `appcui`) or a driver with
//! no wasm surface (`tokio-net`, `tokio-process`, `tokio-signal`). A default
//! that cannot build on a declared target is not a default, so selection is one
//! flag away and `lgwks-deps catalog` prints every group with its feature, its
//! owner and its measured cost, so nothing is hidden behind a guess. The
//! zero-gate source scan lives in `lgwks_ast` (feature `scan`, `lgwks-ast scan`;
//! ADR 0002), so a consumer takes exactly the engine it needs and no Rust
//! grammar rides along with a runtime edge.
//!
//! Do NOT `cargo add tokio` / `cargo add gpui` directly: the gate refuses any
//! second edge, and the facade (`lgwks_bot::rt`, `lgwks_deps::tokio`) is the
//! single entry this workspace audits.
//!
//! ## Feature table
//!
//! The default selection is empty (ADR 0003, #732) because the no-flag build is
//! a required portability claim on every declared target; each group below is
//! one flag away, and `lgwks-deps catalog --json` prints the whole table with
//! each group's default state and its recorded reason. `docs.rs` builds this
//! page with `tokio-full`, `appcui`, `bevy-app`, `bevy-time` and `bevy-state`.
//!
//! | Feature | Enables |
//! |---|---|
//! | `macro` | `proc-macro2` and `quote`, for a first-party proc-macro crate |
//! | `script` | `proc-macro2` with span locations, for the `script!` parser |
//! | `tokio` | the async engine behind `lgwks_bot`, runtime only |
//! | `tokio-time`, `tokio-sync`, `tokio-macros` | one tokio driver each, on top of `tokio` |
//! | `tokio-io` | the reader and writer traits and their extensions, and stdio |
//! | `tokio-net` | sockets; implies `tokio-io` |
//! | `tokio-process`, `tokio-fs`, `tokio-signal` | child processes, files, OS signals |
//! | `tokio-full` | every `tokio-*` driver above |
//! | `bevy-ecs` | Bevy's ECS, the state substrate |
//! | `bevy-app`, `bevy-time`, `bevy-state` | one Bevy runtime crate each, on top of `bevy-ecs` |
//! | `gpui` | Zed's GPU desktop UI framework (Apple targets only) |
//! | `appcui` | the AppCUI terminal UI framework |
//! | `ml-candle` | Candle tensors, layers and the transformer registry |
//! | `ml-candle-metal` | `ml-candle` plus Candle's Metal backend (macOS) |
//! | `ml-tokenizers` | the Hugging Face subword tokenizer |

// Lint contract (missing_docs deny, unsafe_code forbid, broken intra-doc
// links deny) comes from the workspace root.

/// The capability catalog (#732): the estate's one register of what it owns,
/// read from `contract/CATALOG.toml` and rendered as `docs/catalog.md`.
#[doc(hidden)]
pub mod catalog;
/// The approval register: parsing and lookup for `contract/APPROVED.toml`.
pub mod contract;
/// The doc-citation check's one implementation (#695): `lgwks_deps` owns the
/// rule, the `check-doc-citations` binary is a thin shim over the shared
/// entry, and `logical_ci`'s `docs-refs` gate is its designated library
/// consumer through [`doc_citations::execute`], so a changed document's
/// citations still resolve under one rule with one owner.
#[doc(hidden)]
pub mod doc_citations;
/// The optional authored invariant register and its repository-aware validator.
#[doc(hidden)]
pub mod invariants;
/// The register: parsing and approval lookup for `contract/APPROVED.toml`.
#[doc(hidden)]
pub mod lock;
/// Cargo metadata edges: who authored which external dependency.
pub mod metadata;
/// The requirements check's one implementation (#695): `lgwks_deps` owns the
/// rule, the `check-requirements` binary is a thin shim over the shared
/// entry, and `logical_ci`'s `governance` gate is its designated library
/// consumer through [`requirements::execute`], so a requirement is
/// superseded, never edited, under one rule with one owner.
#[doc(hidden)]
pub mod requirements;
/// The spec-owner check's one implementation (#695): `lgwks_deps` owns the
/// rule, the `check-spec-owners` binary is a thin shim over the shared entry,
/// and `logical_ci`'s `governance` gate is its designated library consumer
/// through [`spec_owners::execute`], so every checked requirement has a
/// falsifier row under one rule with one owner.
#[doc(hidden)]
pub mod spec_owners;
/// Vendor-tree coverage: binding the lockfile to the shared `vendor/` tree.
#[doc(hidden)]
pub mod vendor;

/// The audit itself: every direct edge judged against the register.
mod audit;
/// The estate's five published crates: rung 2 of the ladder, admitted by
/// provenance (#701).
mod estate;
/// `[patch]`, `[replace]` and source replacement: overrides the register cannot see.
mod overrides;
/// What the gate refuses and why it could not reach a verdict.
mod refusal;
/// The origin and label predicates, model-checked against a reference.
#[cfg(test)]
mod sim_edge;
/// The closed estate list, swept over seeded name draws (#701).
#[cfg(test)]
mod sim_estate;

#[doc(hidden)]
pub use audit::{audit_direct, declared_scope};
/// The verdict and the failures that keep one from being reached.
pub use refusal::{GateError, Refusal};

#[cfg(any(feature = "macro", feature = "script"))]
#[doc(hidden)]
pub use proc_macro2;
#[cfg(feature = "macro")]
#[doc(hidden)]
pub use quote;
/// The one authored `tokio` edge, re-exported for the storefront.
///
/// `lgwks_deps` owns this edge so no other crate declares `tokio` directly
/// (`INV-DEP-EDGE-OWNED`). `lgwks_bot` enables the `tokio` storefront feature
/// and reaches the engine through this re-export; a consumer that wants the
/// engine without the bot enables the feature here.
#[cfg(feature = "tokio")]
#[doc(hidden)]
pub use tokio;

/// The Bevy ECS substrate, selected through the storefront.
///
/// `lgwks_deps` owns this edge so no other crate declares `bevy_ecs` directly
/// (`INV-DEP-EDGE-OWNED`). `lgwks_bot` enables the `bevy-ecs` storefront feature
/// and reaches the crate through this re-export, which is the same rule the
/// `tokio` edge follows.
#[cfg(feature = "bevy-ecs")]
#[doc(hidden)]
pub use bevy_ecs;

/// The Bevy application layer: the `App` that owns schedules and plugins.
///
/// Re-exported for the same reason as [`bevy_ecs`]. It is a separate feature
/// from `bevy-ecs` because it is a separate decision: a consumer running ECS
/// systems inside its own loop needs the substrate and not the application
/// layer, and the two carry different transitive cost.
///
/// The consumer in `tests/fixtures/bevy-app` builds an `App` through this path, and the
/// integration test `bevy_app_facade_is_selected_only` compiles it with the feature on and
/// refuses it with the feature off.
#[cfg(feature = "bevy-app")]
#[doc(hidden)]
pub use bevy_app;

/// Bevy's time sources, including the virtual clock.
///
/// The consumer in `tests/fixtures/bevy-time` names `Time::<Real>` and `TimePlugin` through this
/// path, and the integration test `bevy_time_facade_is_selected_only` compiles it with the
/// feature on and refuses it with the feature off.
#[cfg(feature = "bevy-time")]
#[doc(hidden)]
pub use bevy_time;

/// Bevy's state machines and their run conditions.
///
/// Derive macros resolve the upstream crate name from the consumer manifest.
/// Alias this facade module at the consumer crate root before deriving. The consumer in
/// `tests/fixtures/bevy-state` does exactly that (`use lgwks_deps::bevy_state as bevy_state;`,
/// then a `States` derive on a `Screen` enum), and the integration test
/// `bevy_state_facade_is_selected_only` compiles it with the feature on and refuses it with the
/// feature off.
#[cfg(feature = "bevy-state")]
#[doc(hidden)]
pub use bevy_state;

/// The optional GPUI desktop UI framework selected through the storefront.
/// Re-exported so consumers name the storefront, never the crate.
#[cfg(feature = "gpui")]
#[doc(hidden)]
pub use gpui;

/// Native terminal widgets and input-driven drawing, selected explicitly.
///
/// The `appcui` example of `tests/fixtures/storefront-matrix` builds a `layout!` through this path,
/// and the integration test `storefront_appcui_compiles_alone` compiles it with only that feature on.
#[cfg(feature = "appcui")]
#[doc(hidden)]
pub use appcui;

/// Minimalist tensor compute and safetensors loading, selected explicitly.
///
/// The storefront owns this edge so no consumer declares `candle-core`
/// directly. Weights are loaded from local files; the feature is default-off.
#[cfg(feature = "ml-candle")]
#[doc(hidden)]
pub use candle_core;

/// Neural network layers and parameter containers built on `candle-core`.
#[cfg(feature = "ml-candle")]
#[doc(hidden)]
pub use candle_nn;

/// Reference transformer model implementations built on `candle-core`.
///
/// Selecting this feature also compiles `hf-hub`, which is network-capable;
/// the runtime reads a local checkpoint and does not call the hub.
#[cfg(feature = "ml-candle")]
#[doc(hidden)]
pub use candle_transformers;

/// Vocabulary-driven subword tokenisation matching published checkpoints.
#[cfg(feature = "ml-tokenizers")]
#[doc(hidden)]
pub use tokenizers;

/// Interim zoned time (jiff): DST-aware calendar arithmetic and IANA zones
/// until ELIMINATE lands in `lgwks_std::time`. `lgwks_std::time` stays the
/// UTC RFC 3339 core; reach zoned work only through here.
#[cfg(feature = "time-jiff")]
#[doc(hidden)]
pub use jiff;

/// The supported storefront: enable a feature and reach its engine here.
///
/// Every path below is one audited edge this crate owns, so a consumer never
/// declares the upstream crate itself. No feature is on by default; the
/// feature table on the crate page names what each one enables.
pub mod storefront {
    #[cfg(feature = "appcui")]
    pub use crate::appcui;
    #[cfg(feature = "bevy-app")]
    pub use crate::bevy_app;
    #[cfg(feature = "bevy-ecs")]
    pub use crate::bevy_ecs;
    #[cfg(feature = "bevy-state")]
    pub use crate::bevy_state;
    #[cfg(feature = "bevy-time")]
    pub use crate::bevy_time;
    #[cfg(feature = "gpui")]
    pub use crate::gpui;
    #[cfg(feature = "time-jiff")]
    pub use crate::jiff;
    #[cfg(feature = "ml-tokenizers")]
    pub use crate::tokenizers;
    #[cfg(feature = "tokio")]
    pub use crate::tokio;
    #[cfg(feature = "ml-candle")]
    pub use crate::{candle_core, candle_nn, candle_transformers};
}

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
    let overrides = overrides::refusals(root)?;
    let edges = metadata::read_with_members(root).map_err(GateError::Metadata)?;
    Ok(edges.map(|(edges, members)| {
        let mut refusals = audit_direct(&edges, &register);
        refusals.extend(audit::surface_refusals(&members, &register));
        refusals.extend(overrides);
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
#[doc(hidden)]
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
