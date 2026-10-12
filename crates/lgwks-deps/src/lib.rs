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
//! the GPU desktop UI. Every capability feature is default-off, and there is
//! no default-on exception: the zero-gate source scan that used to be one now
//! lives in `lgwks_ast` (feature `scan`, `lgwks-ast scan`), which reads Rust
//! through its one parser (ADR 0001, #408).
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
/// Vendor-tree coverage: binding the lockfile to the shared `vendor/` tree.
pub mod vendor;

mod edge;
mod license;
mod refusal;
/// The origin and label predicates, model-checked against a reference.
#[cfg(test)]
mod sim_edge;
#[cfg(test)]
mod tests;
mod tier;

pub use edge::declared_scope;

#[cfg(any(feature = "macro", feature = "script"))]
pub use proc_macro2;
#[cfg(feature = "macro")]
pub use quote;
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

use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use contract::Contract;
use edge::{
    allows_consumer, approved_features, approved_origin, default_features_matches, edge_matches,
    feature_policy_matches, optionality_matches, origin_matches, scope_label, target_matches,
};
use license::license_refusals;
use metadata::DirectEdge;
use tier::{frozen_surface_tier_refusals, surface_refusals, vendor_tier_refusals};

/// Register location relative to the repository root.
pub const CONTRACT_PATH: &str = "contract/APPROVED.toml";

// ── Refusals ────────────────────────────────────────────────────────────────

/// One dependency the register does not admit.
///
/// `#[non_exhaustive]`: the set of drift classes grows as the gate learns to
/// name them, so a consumer must carry a catch-all arm instead of pinning the
/// list. Matching a variant you know, and constructing one, are unaffected.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Refusal {
    /// A path target claims a repository outside this workspace authority.
    ForeignWorkspaceMember {
        /// Workspace package declaring the path edge.
        consumer: String,
        /// Copied or embedded package name.
        krate: String,
        /// Repository declared by the target package.
        declared_repository: String,
        /// Repository admitted by this contract.
        expected_repository: String,
    },
    /// A workspace package authored an external edge with no semantic owner.
    UnregisteredEdge {
        /// Package declaring the edge.
        consumer: String,
        /// External package name.
        krate: String,
        /// Cargo manifest requirement.
        requirement: String,
        /// Registry, Git, path, or other.
        source: String,
        /// Normal, build, or dev.
        kind: String,
    },
    /// The package is registered, but not for this workspace consumer.
    ConsumerNotAllowed {
        /// Package declaring the edge.
        consumer: String,
        /// External package name.
        krate: String,
    },
    /// The package is registered, but not at this manifest requirement.
    RequirementDrift {
        /// Package declaring the edge.
        consumer: String,
        /// External package name.
        krate: String,
        /// Approved manifest requirement.
        approved: String,
        /// Authored manifest requirement.
        declared: String,
    },
    /// The package is registered, but the edge changed origin class.
    SourceDrift {
        /// Package declaring the edge.
        consumer: String,
        /// External package name.
        krate: String,
        /// Approved source class.
        approved: String,
        /// Authored source class.
        declared: String,
    },
    /// The package's source class is approved, but the admitted origin differs:
    /// another registry, another Git repository or revision policy, or another
    /// external path. A class approval is not an origin approval.
    OriginDrift {
        /// Package declaring the edge.
        consumer: String,
        /// External package name.
        krate: String,
        /// Approved origin identity.
        approved: String,
        /// Authored origin identity.
        declared: String,
    },
    /// The package is registered, but not for this dependency kind.
    KindNotAllowed {
        /// Package declaring the edge.
        consumer: String,
        /// External package name.
        krate: String,
        /// Authored dependency kind.
        kind: String,
    },
    /// The package's feature set is outside the admitted policy: a feature the
    /// edge enables is not allowed, or a required feature is absent.
    FeatureDrift {
        /// Package declaring the edge.
        consumer: String,
        /// External package name.
        krate: String,
        /// Admitted feature policy, as authored.
        approved: String,
        /// Features the edge actually enables.
        declared: String,
    },
    /// The package's authored `default-features` bit differs from the policy.
    DefaultFeaturesDrift {
        /// Package declaring the edge.
        consumer: String,
        /// External package name.
        krate: String,
        /// Admitted value of `default-features`, which the entry authored: this
        /// refusal only exists for an authored dimension.
        approved: Option<bool>,
        /// Authored value of `default-features`.
        declared: bool,
    },
    /// The package's authored optionality differs from the policy.
    OptionalityDrift {
        /// Package declaring the edge.
        consumer: String,
        /// External package name.
        krate: String,
        /// Admitted optionality, which the entry authored: this refusal only
        /// exists for an authored dimension.
        approved: Option<bool>,
        /// Authored optionality.
        declared: bool,
    },
    /// The package's target scope differs from the admitted policy.
    TargetDrift {
        /// Package declaring the edge.
        consumer: String,
        /// External package name.
        krate: String,
        /// Admitted target scope, or `<none>` for an unconditional edge.
        approved: String,
        /// Authored target scope, or `<none>`.
        declared: String,
    },
    /// A semantic approval has no authored edge and is stale authority.
    UnusedApproval {
        /// Approved external package.
        krate: String,
        /// Crate responsible for the capability.
        owner: String,
        /// Capability the approval claims to supply.
        capability: String,
    },
    /// A frozen surface's external edge was re-tiered out of the tier the
    /// register freezes it at, which is how this repository refuses `lgwks_ast`
    /// the growth INV-DEP-1 forbids.
    ///
    /// The register names both halves: `[policy] frozen_surfaces` (here,
    /// `lgwks_ast`) and `frozen_tier` (here, `boundary`). A growth attempt that
    /// promotes the new edge out of that tier — or demotes it out of the
    /// register entirely, which this crate separately refuses as
    /// [`Refusal::UnregisteredEdge`] — is how the freeze is expressed. No name
    /// is compiled in: the freeze covers exactly the surfaces a register
    /// declares and the edges it says they own.
    FrozenSurfaceTier {
        /// Frozen surface package that owns the edge.
        consumer: String,
        /// External package name whose approval was re-tiered.
        krate: String,
        /// Tier the approval now claims.
        tier: String,
    },
    /// The register approves external edges the tree really has but declares
    /// no `[policy] accepted_licenses`, so there is no licence policy to judge
    /// them by.
    ///
    /// The gate is a published crate that audits other repositories; a set
    /// compiled into it would be one repository's choice imposed on every
    /// other. One refusal for the whole register, because the fix is one line
    /// in it.
    LicensePolicyUndeclared {
        /// Approvals with an observed edge that went unjudged.
        approvals: usize,
    },
    /// The licence the package's own manifest declares is not the licence the
    /// register approved, or is not one the register accepts.
    ///
    /// Two refusals share one variant because they are the same question asked
    /// in two directions — *is the approved licence still true, and is it one we
    /// accept?* — and a reader who got one answer deserves the same report
    /// shape for the other. `approved` is what the register recorded and
    /// `declared` is what `cargo metadata` reports: when they differ, upstream
    /// changed the licence without a re-approval; when they agree and still
    /// name something outside the register's `accepted_licenses`, the approval
    /// itself is out of policy. `"<none>"` stands for a manifest that declares no `license`
    /// key at all, so an absence reads as an absence and not as a permission.
    LicenseNotAccepted {
        /// Package whose declared licence was refused.
        krate: String,
        /// Licence expression the register approved.
        approved: String,
        /// Licence expression the package's manifest declares.
        declared: String,
        /// Licence identifiers outside the register's accepted set, in the
        /// order they appear in `declared`.
        rejected: Vec<String>,
    },
    /// An approval claims the `vendor` tier while its edge still resolves to
    /// the source class a registry edge has.
    ///
    /// `vendor` is rung 7 of the ladder — "audited upstream source checked into
    /// the workspace, not a registry edge" (`docs/dependency-doctrine.md` §1).
    /// This is the reader that tier never had. It does not ask whether the tier
    /// is the right *label* for a dependency, which is a review decision this
    /// gate has no evidence to make; it asks the one question the tier can
    /// decide on its own from data the gate already has, which is whether the
    /// tier contradicts the source class the edge actually resolves from.
    VendorTierConflict {
        /// Package whose approval claims the vendor tier.
        krate: String,
        /// Source class the authored edge actually resolves from.
        source: String,
    },
    /// A workspace member or an approval owner names a surface outside the
    /// closed set the register's `[policy] surfaces` declares (INV-DEP-1 for
    /// this repository).
    ///
    /// The gate is membership-driven, so a sixth crate would otherwise be
    /// audited like any other and an `owner = "lgwks_rogue"` approval would
    /// pass an identifier check. This is the refusal that makes the surface
    /// list a closed vocabulary rather than a sentence.
    UnknownSurface {
        /// The member package or owner name that is not a recognised surface.
        krate: String,
        /// Where it was found: `workspace member` or `approval owner`.
        role: String,
    },
    /// `[policy] enforce = false` was used to turn a tree that *actually
    /// carries refusals* into a passing build.
    ///
    /// Adoption mode is a reviewable posture, not an off switch: it may report
    /// refusals as guidance, but it cannot make them non-fatal. This refusal is
    /// added on top of the edge refusals it counted, so the verdict is the same
    /// under `enforce = true` and `enforce = false` and the boolean cannot be
    /// inverted without the build changing. The count is carried so the report
    /// says how many violations the posture tried to stand down.
    AdoptionModeRefusals {
        /// Number of dependency-edge refusals the posture stood down.
        refusals: usize,
    },
}

/// Why the gate could not reach a verdict. Every variant is a refusal, not a
/// pass; see the fail-closed note on this module.
///
/// `#[non_exhaustive]`: a new failure mode is a refusal that must be added, and
/// adding it must not be a breaking change for embedders matching on this type.
#[derive(Debug)]
#[non_exhaustive]
pub enum GateError {
    /// No `Cargo.lock` was found at or above the starting directory.
    LockNotFound {
        /// Directory the search started from.
        from: PathBuf,
    },
    /// The repository has no register.
    ContractNotFound {
        /// Path the register was expected at.
        path: PathBuf,
    },
    /// A file could not be read.
    Unreadable {
        /// Path that could not be read.
        path: PathBuf,
        /// Underlying I/O cause.
        cause: std::io::Error,
    },
    /// The register is not a valid contract.
    Contract(contract::ContractError),
    /// The optional invariant register could not be read or parsed.
    Invariant(invariants::InvariantError),
    /// `Cargo.lock` does not parse.
    ///
    /// Its own variant rather than `Unreadable`: the file read fine, and
    /// "unreadable" would send an operator looking at permissions or encoding
    /// when the actual defect is a `[[package]]` block naming no package. The
    /// gate audits a dependency graph, so a lock whose contents cannot be read
    /// is a graph whose contents cannot be audited.
    Lock(lock::LockError),
    /// Cargo's authored direct dependency graph could not be obtained.
    Metadata(metadata::MetadataError),
}

impl fmt::Display for GateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::LockNotFound { ref from } => {
                write!(formatter, "no Cargo.lock at or above {}", from.display())
            }
            Self::ContractNotFound { ref path } => write!(
                formatter,
                "no dependency register at {} — every repo the gate guards must carry one; \
                 run `lgwks-deps init` to write a fail-closed starting register",
                path.display()
            ),
            Self::Unreadable {
                ref path,
                ref cause,
            } => {
                write!(formatter, "cannot read {}: {cause}", path.display())
            }
            Self::Contract(ref error) => write!(formatter, "{CONTRACT_PATH}: {error}"),
            Self::Invariant(ref error) => {
                write!(formatter, "{}: {error}", invariants::INVARIANTS_PATH)
            }
            Self::Lock(ref error) => write!(formatter, "Cargo.lock: {error}"),
            Self::Metadata(ref error) => write!(formatter, "Cargo metadata: {error}"),
        }
    }
}

impl Error for GateError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match *self {
            Self::Unreadable { ref cause, .. } => Some(cause),
            Self::Contract(ref error) => Some(error),
            Self::Invariant(ref error) => Some(error),
            Self::Lock(ref error) => Some(error),
            Self::Metadata(ref error) => Some(error),
            _ => None,
        }
    }
}

// ── The audit ───────────────────────────────────────────────────────────────

/// Audits authored direct dependency edges against semantic ownership.
///
/// Five questions, each answered from a different evidence: is every frozen
/// surface's edge set closed (INV-DEP-1), is every edge owned, is every
/// approval still true (`UnusedApproval`), is every declared licence one this
/// repository accepts and the one the register approved (#208), and does every
/// approval's tier agree with the source its edge resolves from (#210).
pub fn audit_direct(edges: &[DirectEdge], register: &Contract) -> Vec<Refusal> {
    let mut refusals = frozen_surface_tier_refusals(register);
    refusals.extend(vendor_tier_refusals(edges, register));
    refusals.extend(license_refusals(edges, register));
    if let Some(expected) = register.repository.as_ref() {
        for edge in edges.iter().filter(|edge| edge.workspace) {
            if let Some(declared) = edge.target_repository.as_ref()
                && declared != expected
            {
                refusals.push(Refusal::ForeignWorkspaceMember {
                    consumer: edge.consumer.clone(),
                    krate: edge.package.clone(),
                    declared_repository: declared.clone(),
                    expected_repository: expected.clone(),
                });
            }
        }
    }
    // Workspace membership is the only exemption, and it is decided by
    // `workspace_members` in Cargo's own metadata, not by a package name.
    let external: Vec<&DirectEdge> = edges.iter().filter(|edge| !edge.workspace).collect();
    for edge in &external {
        let approvals: Vec<&contract::Entry> = register.approvals_for(&edge.package).collect();
        if approvals.is_empty() {
            refusals.push(Refusal::UnregisteredEdge {
                consumer: edge.consumer.clone(),
                krate: edge.package.clone(),
                requirement: edge.requirement.clone(),
                source: format!("{}:{}", edge.source.class(), edge.source.detail()),
                kind: edge.kind.to_string(),
            });
            continue;
        }
        if approvals.iter().any(|entry| edge_matches(entry, edge)) {
            continue;
        }
        let consumer_approvals: Vec<&contract::Entry> = approvals
            .iter()
            .copied()
            .filter(|entry| allows_consumer(entry, &edge.consumer))
            .collect();
        // Only approvals that admit the edge's source class are candidates for
        // the class, origin, requirement or kind drift. Reporting a source
        // mismatch from an approval of a *different* class would name an
        // irrelevant candidate and misdirect the repair.
        let class_matching: Vec<&contract::Entry> = consumer_approvals
            .iter()
            .copied()
            .filter(|entry| entry.source == edge.source.class())
            .collect();
        if consumer_approvals.is_empty() {
            refusals.push(Refusal::ConsumerNotAllowed {
                consumer: edge.consumer.clone(),
                krate: edge.package.clone(),
            });
        } else if class_matching.is_empty() {
            if let Some(entry) = consumer_approvals.first() {
                refusals.push(Refusal::SourceDrift {
                    consumer: edge.consumer.clone(),
                    krate: edge.package.clone(),
                    approved: entry.source.clone(),
                    declared: edge.source.class().to_owned(),
                });
            }
        } else if let Some(entry) = class_matching
            .iter()
            .copied()
            .find(|entry| !origin_matches(entry, edge))
        {
            refusals.push(Refusal::OriginDrift {
                consumer: edge.consumer.clone(),
                krate: edge.package.clone(),
                approved: approved_origin(entry, edge),
                declared: edge.source.detail().to_owned(),
            });
        } else if let Some(entry) = class_matching
            .iter()
            .copied()
            .find(|entry| entry.version != edge.requirement)
        {
            refusals.push(Refusal::RequirementDrift {
                consumer: edge.consumer.clone(),
                krate: edge.package.clone(),
                approved: entry.version.clone(),
                declared: edge.requirement.clone(),
            });
        } else if let Some(entry) = class_matching
            .iter()
            .copied()
            .find(|entry| !feature_policy_matches(entry, edge))
        {
            refusals.push(Refusal::FeatureDrift {
                consumer: edge.consumer.clone(),
                krate: edge.package.clone(),
                approved: approved_features(entry),
                declared: edge.features.join(","),
            });
        } else if let Some(entry) = class_matching
            .iter()
            .copied()
            .find(|entry| !default_features_matches(entry, edge))
        {
            refusals.push(Refusal::DefaultFeaturesDrift {
                consumer: edge.consumer.clone(),
                krate: edge.package.clone(),
                approved: entry.uses_default_features,
                declared: edge.uses_default_features,
            });
        } else if let Some(entry) = class_matching
            .iter()
            .copied()
            .find(|entry| !optionality_matches(entry, edge))
        {
            refusals.push(Refusal::OptionalityDrift {
                consumer: edge.consumer.clone(),
                krate: edge.package.clone(),
                approved: entry.optional,
                declared: edge.optional,
            });
        } else if let Some(entry) = class_matching
            .iter()
            .copied()
            .find(|entry| !target_matches(entry, edge))
        {
            refusals.push(Refusal::TargetDrift {
                consumer: edge.consumer.clone(),
                krate: edge.package.clone(),
                approved: scope_label(entry.target.as_deref()).to_owned(),
                declared: scope_label(edge.target.as_deref()).to_owned(),
            });
        } else {
            refusals.push(Refusal::KindNotAllowed {
                consumer: edge.consumer.clone(),
                krate: edge.package.clone(),
                kind: edge.kind.to_string(),
            });
        }
    }
    for entry in &register.entries {
        let used = external.iter().any(|edge| {
            entry.admits(&edge.package) && edge.consumer == entry.owner && edge_matches(entry, edge)
        });
        if !used {
            refusals.push(Refusal::UnusedApproval {
                krate: entry.krate.clone(),
                owner: entry.owner.clone(),
                capability: entry.capability.clone(),
            });
        }
    }
    // Adoption mode is a posture, not an off switch. When the register says
    // `enforce = false` *and* the tree actually carries edge violations, one
    // refusal names the stand-down so the build fails for the same reason it
    // would under `enforce = true`. Without this the whole verdict reduced to
    // that one boolean: a reviewable one-token diff was enough to make a tree
    // the gate had just declared in violation report success. An empty audit
    // stays empty — adoption mode on a clean tree is a legitimate posture and
    // must not be refused.
    if !register.enforce && !refusals.is_empty() {
        refusals.push(Refusal::AdoptionModeRefusals {
            refusals: refusals.len(),
        });
    }
    refusals.sort_by_key(ToString::to_string);
    refusals
}

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
        refusals.extend(surface_refusals(&members, &register));
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
