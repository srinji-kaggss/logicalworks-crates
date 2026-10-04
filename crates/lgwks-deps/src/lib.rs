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
/// Safe process-group existence observation for the supervised process facade.
#[cfg(feature = "process-group-probe")]
pub mod process_group;
/// Rust source scan: the zero-gate detectors behind `lgwks-deps scan` (feature `scan`).
#[cfg(feature = "scan")]
pub mod scan;
/// Vendor-tree coverage: binding the lockfile to the shared `vendor/` tree.
pub mod vendor;

#[cfg(feature = "macro")]
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

use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use contract::Contract;
use metadata::DirectEdge;

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
        /// Admitted value of `default-features`.
        approved: bool,
        /// Authored value of `default-features`.
        declared: bool,
    },
    /// The package's authored optionality differs from the policy.
    OptionalityDrift {
        /// Package declaring the edge.
        consumer: String,
        /// External package name.
        krate: String,
        /// Admitted optionality.
        approved: bool,
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
    /// A frozen surface's external edge was re-tiered out of the register's own
    /// approved tier, which is how `lgwks_ast` is refused the growth
    /// INV-DEP-1 forbids.
    ///
    /// `invariants.rs` reads INV-DEP-1 as *the finished, standalone `lgwks_ast`*:
    /// it is a closed surface whose register entries must keep saying `boundary`.
    /// A growth attempt that promotes the new edge to `vendor` — or demotes it
    /// out of the register entirely, which this crate separately refuses as
    /// [`Refusal::UnregisteredEdge`] — is how the freeze is expressed. No name
    /// is hardcoded and no exemption is invented: the freeze covers exactly the
    /// edges a register says a surface owns, and it covers no more.
    FrozenSurfaceTier {
        /// Frozen surface package that owns the edge.
        consumer: String,
        /// External package name whose approval was re-tiered.
        krate: String,
        /// Tier the approval now claims.
        tier: String,
    },
    /// The licence the package's own manifest declares is not the licence the
    /// register approved, or is not one this repository accepts.
    ///
    /// Two refusals share one variant because they are the same question asked
    /// in two directions — *is the approved licence still true, and is it one we
    /// accept?* — and a reader who got one answer deserves the same report
    /// shape for the other. `approved` is what the register recorded and
    /// `declared` is what `cargo metadata` reports: when they differ, upstream
    /// changed the licence without a re-approval; when they agree and still
    /// name something outside [`accepted_licenses`], the approval itself is out
    /// of policy. `"<none>"` stands for a manifest that declares no `license`
    /// key at all, so an absence reads as an absence and not as a permission.
    LicenseNotAccepted {
        /// Package whose declared licence was refused.
        krate: String,
        /// Licence expression the register approved.
        approved: String,
        /// Licence expression the package's manifest declares.
        declared: String,
        /// Licence identifiers outside this repository's accepted set, in the
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

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::ForeignWorkspaceMember {
                ref consumer,
                ref krate,
                ref declared_repository,
                ref expected_repository,
            } => write!(
                formatter,
                "{consumer} embeds workspace package {krate} from {declared_repository}; consume its published crate instead (workspace authority is {expected_repository})"
            ),
            Self::UnregisteredEdge {
                ref consumer,
                ref krate,
                ref requirement,
                ref source,
                ref kind,
            } => write!(
                formatter,
                "{consumer} declares unowned {kind} edge {krate} {requirement} from {source}"
            ),
            Self::ConsumerNotAllowed {
                ref consumer,
                ref krate,
            } => write!(
                formatter,
                "{consumer} declares {krate}, but no approval allows that consumer"
            ),
            Self::RequirementDrift {
                ref consumer,
                ref krate,
                ref approved,
                ref declared,
            } => write!(
                formatter,
                "{consumer} declares {krate} requirement {declared}, contract approves {approved}"
            ),
            Self::SourceDrift {
                ref consumer,
                ref krate,
                ref approved,
                ref declared,
            } => write!(
                formatter,
                "{consumer} declares {krate} from {declared}, contract approves {approved}"
            ),
            Self::OriginDrift {
                ref consumer,
                ref krate,
                ref approved,
                ref declared,
            } => write!(
                formatter,
                "{consumer} declares {krate} from {declared}, contract approves origin {approved}"
            ),
            Self::KindNotAllowed {
                ref consumer,
                ref krate,
                ref kind,
            } => write!(
                formatter,
                "{consumer} declares {krate} as {kind}, but that edge kind is not approved"
            ),
            Self::FeatureDrift {
                ref consumer,
                ref krate,
                ref approved,
                ref declared,
            } => write!(
                formatter,
                "{consumer} declares {krate} with features [{declared}], contract admits [{approved}]"
            ),
            Self::DefaultFeaturesDrift {
                ref consumer,
                ref krate,
                approved,
                declared,
            } => write!(
                formatter,
                "{consumer} declares {krate} with default-features = {declared}, contract admits {approved}"
            ),
            Self::OptionalityDrift {
                ref consumer,
                ref krate,
                approved,
                declared,
            } => write!(
                formatter,
                "{consumer} declares {krate} optional = {declared}, contract admits {approved}"
            ),
            Self::TargetDrift {
                ref consumer,
                ref krate,
                ref approved,
                ref declared,
            } => write!(
                formatter,
                "{consumer} declares {krate} for target {declared}, contract admits {approved}"
            ),
            Self::UnusedApproval {
                ref krate,
                ref owner,
                ref capability,
            } => write!(
                formatter,
                "unused approval for {krate} capability {capability} owned by {owner}"
            ),
            Self::FrozenSurfaceTier {
                ref consumer,
                ref krate,
                ref tier,
            } => write!(
                formatter,
                "{consumer} is a frozen surface and its {krate} edge is tiered {tier}; \
                 a frozen surface's edges stay boundary, and the surface's approved set \
                 is closed"
            ),
            Self::LicenseNotAccepted {
                ref krate,
                ref approved,
                ref declared,
                ref rejected,
            } => {
                let refused = rejected.join(", ");
                write!(
                    formatter,
                    "{krate} declares licence {declared}, which the register's \
                     approved {approved} does not admit ({refused}); re-approve the \
                     licence or remove the edge"
                )
            }
            Self::VendorTierConflict {
                ref krate,
                ref source,
            } => write!(
                formatter,
                "{krate} is approved at the vendor tier but its edge resolves from \
                 {source}; vendor means audited source checked into this workspace, \
                 not a registry edge"
            ),
            Self::AdoptionModeRefusals { refusals } => write!(
                formatter,
                "[policy] enforce = false stood down {refusals} dependency-edge \
                 violations; adoption mode reports refusals, it does not make them \
                 pass — register the edges or re-enable enforcement"
            ),
        }
    }
}

impl Refusal {
    /// The crate this refusal is about.
    ///
    /// An adoption-mode refusal is not about any one crate: it is about the
    /// register's own `[policy]` block standing down a count of edge
    /// violations, so it carries no crate name and answers `"<policy>"`. A
    /// caller that wants to distinguish the two shapes matches on the variant
    /// rather than on this label.
    #[must_use]
    pub fn krate(&self) -> &str {
        match *self {
            Self::ForeignWorkspaceMember { ref krate, .. }
            | Self::UnregisteredEdge { ref krate, .. }
            | Self::ConsumerNotAllowed { ref krate, .. }
            | Self::RequirementDrift { ref krate, .. }
            | Self::SourceDrift { ref krate, .. }
            | Self::OriginDrift { ref krate, .. }
            | Self::KindNotAllowed { ref krate, .. }
            | Self::FeatureDrift { ref krate, .. }
            | Self::DefaultFeaturesDrift { ref krate, .. }
            | Self::OptionalityDrift { ref krate, .. }
            | Self::TargetDrift { ref krate, .. }
            | Self::UnusedApproval { ref krate, .. }
            | Self::FrozenSurfaceTier { ref krate, .. }
            | Self::LicenseNotAccepted { ref krate, .. }
            | Self::VendorTierConflict { ref krate, .. } => krate,
            Self::AdoptionModeRefusals { .. } => "<policy>",
        }
    }

    /// Whether this refusal names the register's enforcement policy rather
    /// than a dependency edge.
    ///
    /// Kept beside [`Refusal::krate`] because the two must not drift: every
    /// variant that returns the `"<policy>"` label has to answer `true` here,
    /// or a consumer filtering by scope would find a crate-shaped refusal
    /// carrying a policy-shaped name.
    #[must_use]
    pub const fn is_policy(&self) -> bool {
        matches!(*self, Self::AdoptionModeRefusals { .. })
    }
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

/// Whether `entry` names `consumer` among its allowed consumers.
///
/// Byte-exact against the Cargo-authored workspace package name. There is no
/// implicit `-`/`_` fold: a consumer is a package identity, and folding two
/// spellings together would let one workspace package stand in for another.
fn allows_consumer(entry: &contract::Entry, consumer: &str) -> bool {
    entry
        .allowed_consumers
        .iter()
        .any(|allowed| allowed == consumer)
}

/// The one registry a legacy class-only approval admits.
///
/// The two Cargo spellings — the Git index and the sparse index — name this one
/// registry, so both compare equal to it and to each other. Any other registry
/// source is a different authority and needs its own authored `origin`.
const CRATES_IO_INDEX: &str = "registry+https://github.com/rust-lang/crates.io-index";
/// The sparse transport spelling of the crates.io registry.
const CRATES_IO_SPARSE: &str = "sparse+https://index.crates.io/";

/// Whether `detail` names the crates.io registry through either Cargo spelling.
fn is_crates_io(detail: &str) -> bool {
    detail == CRATES_IO_INDEX
        || detail == CRATES_IO_SPARSE
        || detail == "sparse+https://index.crates.io"
}

/// The repository portion of a Cargo Git source, before any revision policy.
fn git_repository(detail: &str) -> &str {
    match detail.find(['?', '#']) {
        Some(index) => &detail[..index],
        None => detail,
    }
}

/// The revision/reference policy of a Cargo Git source: the `?rev=…`/`?branch=…`
/// query and the `#…` resolved-revision fragment, or empty when none is pinned.
fn git_policy(detail: &str) -> &str {
    match detail.find(['?', '#']) {
        Some(index) => &detail[index..],
        None => "",
    }
}

/// Whether an authored origin admits an observed Cargo source exactly.
///
/// Registry identity is the whole source string, except that the two spellings
/// of crates.io name one registry. Git identity is the repository plus its
/// admitted revision/reference policy, so pinning a different revision inside
/// the approved repository is still a drift. Path identity is the whole
/// authority string.
fn same_origin(approved: &str, observed: &str) -> bool {
    if approved == observed {
        return true;
    }
    if approved.starts_with("registry+") || approved.starts_with("sparse+") {
        return is_crates_io(approved) && is_crates_io(observed);
    }
    if approved.starts_with("git+") && observed.starts_with("git+") {
        return git_repository(approved) == git_repository(observed)
            && git_policy(approved) == git_policy(observed);
    }
    false
}

/// Whether `entry`'s approval admits `edge`'s origin.
///
/// An authored `origin` is compared by [`same_origin`]. A legacy entry with no
/// `origin` is exact only for the one deterministic registry this estate uses;
/// a git or path edge needs an authored origin before it can be admitted, so a
/// class-only entry is insufficient for exact-origin assurance rather than
/// implicit approval of every origin in its class.
fn origin_matches(entry: &contract::Entry, edge: &DirectEdge) -> bool {
    match entry.origin.as_deref() {
        Some(approved) => same_origin(approved, edge.source.detail()),
        None => entry.source == "registry" && is_crates_io(edge.source.detail()),
    }
}

/// The approved origin to name in an [`Refusal::OriginDrift`].
fn approved_origin(entry: &contract::Entry, edge: &DirectEdge) -> String {
    match entry.origin.as_deref() {
        Some(approved) => approved.to_owned(),
        None if entry.source == "registry" => CRATES_IO_INDEX.to_owned(),
        None => format!(
            "{} (class-only: an exact origin is required)",
            edge.source.class()
        ),
    }
}

/// Whether `entry`'s admitted feature policy admits `edge`'s enabled features.
///
/// An absent `features` list (or `required_features` list) leaves that half
/// grandfathered: the entry predates the dimension and does not refuse a feature
/// it never named. A present list is enforced exactly — every enabled feature
/// must be allowed, and every required feature must be enabled.
fn feature_policy_matches(entry: &contract::Entry, edge: &DirectEdge) -> bool {
    if entry.features.as_ref().is_some_and(|allowed| {
        edge.features
            .iter()
            .any(|enabled| !allowed.contains(enabled))
    }) {
        return false;
    }
    if entry.required_features.as_ref().is_some_and(|required| {
        required
            .iter()
            .any(|needed| !edge.features.contains(needed))
    }) {
        return false;
    }
    true
}

/// Whether `entry`'s admitted `default-features` policy admits the edge.
fn default_features_matches(entry: &contract::Entry, edge: &DirectEdge) -> bool {
    entry
        .uses_default_features
        .is_none_or(|approved| approved == edge.uses_default_features)
}

/// Whether `entry`'s admitted optionality policy admits the edge.
fn optionality_matches(entry: &contract::Entry, edge: &DirectEdge) -> bool {
    entry
        .optional
        .is_none_or(|approved| approved == edge.optional)
}

/// Whether `entry`'s admitted target scope admits the edge.
///
/// An unauthored policy admits any scope; an authored `""` requires an
/// unconditional declaration, and any other string must equal the edge's target
/// `cfg(…)` exactly.
fn target_matches(entry: &contract::Entry, edge: &DirectEdge) -> bool {
    match entry.target.as_deref() {
        None => true,
        Some(approved) => approved == edge.target.as_deref().unwrap_or(""),
    }
}

/// The feature policy as it is named in a refusal.
fn approved_features(entry: &contract::Entry) -> String {
    let allowed = entry
        .features
        .as_ref()
        .map(|list| list.join(","))
        .unwrap_or_default();
    match entry.required_features.as_ref() {
        Some(required) if !required.is_empty() => {
            format!("{allowed} (required: {})", required.join(","))
        }
        _ => allowed,
    }
}

/// Whether an approval admits this exact edge.
///
/// Every axis must hold: the consumer is allowed, the requirement string is
/// identical, the source class and origin are identical, the dependency kind is
/// listed, and each admitted capability policy (features, default-features,
/// optionality, target) is satisfied. A partial match is not a weak admission:
/// it is a refusal with a named axis, which is why `audit_direct` re-tests each
/// axis to report *which* one drifted.
fn edge_matches(entry: &contract::Entry, edge: &DirectEdge) -> bool {
    allows_consumer(entry, &edge.consumer)
        && entry.version == edge.requirement
        && entry.source == edge.source.class()
        && origin_matches(entry, edge)
        && entry
            .allowed_kinds
            .iter()
            .any(|kind| kind == edge.kind.as_str())
        && feature_policy_matches(entry, edge)
        && default_features_matches(entry, edge)
        && optionality_matches(entry, edge)
        && target_matches(entry, edge)
}

/// Surfaces whose external edges are frozen: the finished, standalone crates
/// INV-DEP-1 closes.
///
/// This is *not* a list of surfaces to audit — that question is answered
/// generically, from Cargo's own `workspace_members`, so a newly declared
/// member is audited from its first commit and nothing has to be listed here to
/// be classified. It is only the subset whose approved edge set may not grow.
/// The registry of what is frozen is the register: a crate is frozen once some
/// `[[approved]]` entry names it as `owner`, and freezing therefore cannot be
/// smuggled past this gate by editing this array.
const FROZEN_SURFACES: [&str; 1] = ["lgwks_ast"];

/// The tier an approval for a frozen surface must keep claiming.
const FROZEN_TIER: contract::Tier = contract::Tier::Boundary;

// ── Licence audit (#208) ────────────────────────────────────────────────────

/// What a manifest with no `license` key is reported as.
///
/// A distinct spelling, not an empty string, so the refusal says *absent* rather
/// than rendering a gap in the middle of a sentence that otherwise reads like a
/// permission.
const NO_LICENSE: &str = "<none>";

/// The SPDX identifiers this repository admits an external dependency under.
///
/// Provenance, because this table is not a preference: it is the closed set
/// spanned by what the repository already does.
///
/// - Every entry is a licence one of the 32 approved packages already declares
///   under `cargo metadata`. Widening the set would be a decision the register
///   has not made; narrowing it would refuse a tree the maintainers shipped.
/// - `Apache-2.0` and `MIT` are the licences four of the five workspace crates
///   declare (`crates/*/Cargo.toml`), and `MPL-2.0` is the fifth
///   (`crates/lgwks-bot`), so these are the licences this repository has already
///   chosen to publish under.
/// - `Zlib` and `BSD-3-Clause` are added for one reason: Cargo *requires* every
///   identifier in an SPDX expression to be known, so a package offering
///   `MIT OR Apache-2.0 OR Zlib` cannot be approved at all without it. Both are
///   permissive, non-copyleft licences, so admitting them adds no obligation a
///   repository publishing under Apache-2.0 has not already taken on.
///
/// Not accepted, and refused: `GPL-*` and `AGPL-*` (reciprocal terms that would
/// reach this repository's own files through the link), `MPL-2.0` as an
/// *inbound* dependency licence (file-level copyleft imposed on us by an
/// upstream is not a decision this table makes), `Unicode-3.0` and
/// `CDLA-Permissive-2.0` (they appear in the transitive closure, and a
/// transitive edge is not a register entry), and `LicenseRef-*` (a reference to
/// a licence text this repository has never read).
///
/// **This set is not ratified policy.** `LICENSING.md` records what this
/// repository publishes *itself* under and `docs/dependency-doctrine.md` names
/// licence obligations exactly once, in its preamble; neither states which
/// licences an inbound dependency may carry. The table above is therefore the
/// set the repository currently satisfies, adopted so the audit has something
/// to refuse with — an explicit, documented and minimal reading rather than a
/// silent one. Widening it (to admit `BSD-3-Clause`-only or `Unlicense`
/// packages, for instance) is a Director decision; the honest way to record one
/// is an edit to this constant plus the register entries it admits.
const ACCEPTED_LICENSES: [&str; 7] = [
    "0BSD",
    "Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "BSD-3-Clause",
    "CC0-1.0",
    "MIT",
    "Zlib",
];

/// Licence identifiers this repository accepts, as a sorted, borrowable slice.
///
/// The accessor exists so the table above has one reader in this module and
/// every other reader — refusals, tests, an embedder asking what it may admit —
/// goes through the same name.
#[must_use]
pub fn accepted_licenses() -> &'static [&'static str] {
    &ACCEPTED_LICENSES
}

/// The licence identifiers in `expression` that this repository does not
/// accept, in the order they appear.
///
/// The expression is scanned for SPDX *identifiers*, not split on whitespace.
/// The difference decides whether two real licences pass. `Apache-2.0 WITH
/// LLVM-exception` is one identifier with one exception attached; splitting it
/// yields a bare `LLVM-exception`, which is not a licence anyone can be under,
/// so `blake3` and `rustix` were refused for carrying exactly the terms the
/// register had approved for them. `WITH` binds to the identifier on its left
/// and is not a conjunction, so it never separates two alternatives.
///
/// `AND` and `OR` are operators and are consumed as part of the scan rather
/// than reported: refusing `OR` would refuse every compound expression ever
/// written. The register has already refused an expression that is not valid
/// SPDX, so what reaches here from Cargo metadata is reported whole rather than
/// parsed into a grammar this gate would then have to keep in step with the
/// specification.
///
/// An identifier that is an accepted one passes even when the expression pairs
/// it with an exception this repository does not name, because the accepted
/// table lists `Apache-2.0 WITH LLVM-exception` in full and matching it is what
/// "accepted" means; a bare `Apache-2.0 WITH GPL-exception` is refused on the
/// `GPL-exception` token, which is the case the exception arm exists to catch.
fn rejected_license_identifiers(expression: &str) -> Vec<String> {
    let mut rejected = Vec::new();
    // Walk left to right, taking the longest accepted identifier that matches
    // here. Longest-first is what lets `Apache-2.0 WITH LLVM-exception` win over
    // the bare `Apache-2.0` that also matches at this position.
    let tokens = expression
        .split_ascii_whitespace()
        .map(|token| token.trim_matches(['(', ')']))
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    let mut index = 0;
    while index < tokens.len() {
        let rest = tokens[index..].join(" ");
        if let Some(accepted) = ACCEPTED_LICENSES
            .iter()
            .filter(|candidate| rest.starts_with(**candidate))
            .max_by_key(|candidate| candidate.len())
        {
            // Consume exactly the words this identifier occupies.
            let consumed = accepted.split_ascii_whitespace().count();
            index = index.saturating_add(consumed);
            continue;
        }
        let token = tokens[index];
        if !matches!(token, "AND" | "OR" | "WITH") {
            rejected.push(token.to_owned());
        }
        index = index.saturating_add(1);
    }
    rejected
}

/// Every approval whose recorded licence no longer says what the package
/// declares, or names a licence outside [`ACCEPTED_LICENSES`].
///
/// One pass over the approvals, and each approval is asked about the *declared*
/// licence of its target rather than the licence the register recorded: the
/// question the audit exists to answer is what the code in the tree is actually
/// under, and a register that disagrees is the drift (#208). Reporting the
/// declared expression even when it equals the approved one is deliberate —
/// there is no second refusal to find, and one refusal that names the licence
/// is the whole report.
///
/// When two edges of the same package resolve to two different declared
/// expressions, every one of them is refused, because a single-version
/// dependency cannot honestly be approved under two licences and the gate has
/// no evidence to pick one.
fn license_refusals(edges: &[DirectEdge], register: &Contract) -> Vec<Refusal> {
    register
        .approvals()
        .filter_map(|entry| {
            // Distinct observed licences for this package, `None` standing for a
            // target that declares none. Order follows the edge order, so the
            // refusal names the same expression on every run.
            let mut observed: Vec<Option<&str>> = Vec::new();
            for edge in edges
                .iter()
                .filter(|edge| !edge.workspace && entry.admits(&edge.package))
            {
                if !observed.contains(&edge.license()) {
                    observed.push(edge.license());
                }
            }
            let Some(first) = observed.first().copied() else {
                // An approval whose package Cargo does not report — an inactive
                // optional edge with no resolved package, for instance — carries
                // nothing to compare. `UnusedApproval` is what says so, and
                // refusing here would make a licence report restate it.
                return None;
            };
            let declared = declared_value(first).to_owned();
            let drift = observed.iter().copied().any(|candidate| {
                !declared_value(candidate)
                    .trim()
                    .eq_ignore_ascii_case(entry.license().trim())
            });
            let rejected = observed
                .iter()
                .copied()
                .flat_map(|candidate| rejected_license_identifiers(declared_value(candidate)))
                .collect::<Vec<_>>();
            if !drift && rejected.is_empty() {
                return None;
            }
            Some(Refusal::LicenseNotAccepted {
                krate: entry.krate().to_owned(),
                approved: entry.license().to_owned(),
                declared,
                rejected,
            })
        })
        .collect()
}

/// The text a declared licence is compared and rendered as.
fn declared_value(observed: Option<&str>) -> &str {
    match observed {
        Some(expression) => expression,
        None => NO_LICENSE,
    }
}

// ── Tier audit (#210) ───────────────────────────────────────────────────────

/// The source class an approval at the [`contract::Tier::Vendor`] tier may
/// resolve from.
///
/// `vendor` means audited source checked into the workspace rather than a
/// registry edge. This repository checks third-party source into `vendor/` for
/// *offline builds* while still resolving those crates from a registry, so
/// "there is a vendored directory" is not evidence about an edge's source class
/// — and `vendor.rs`'s [`crate::vendor::Report`] binds on `Cargo.lock` hashes
/// precisely so the tree cannot stand in for a source class. What can be read
/// from an edge is where it came from, so `registry` and `git` — the two classes
/// Cargo resolves at build time from somewhere else — contradict the tier. See
/// [`vendor_tier_refusals`] for why `path` is deliberately not refused.
const VENDOR_SOURCE_CLASSES: [&str; 2] = ["registry", "git"];

/// Every approval that claims the vendor tier while its edge still resolves from
/// a source the tier rules out.
///
/// The one claim `vendor` makes that is checkable from the edge itself. It is
/// deliberately *not* the claim a reader might want it to make: nothing in this
/// repository says whether a given crate's code was audited before it was
/// checked in — `vendor.rs` records coverage and hash equality, not a review —
/// so requiring proof of review would mean inventing an approval artefact that
/// does not exist, and refusing everything at the tier would make the tier
/// unusable and thereby leave it unexercised rather than meaningful.
///
/// Nor does this refuse `tier = "vendor"` on a `path` edge. A vendored crate
/// normally *is* a path dependency, and a path edge is exactly what "checked
/// into the workspace" looks like to Cargo; the check that belongs on that
/// shape is INV-VENDOR-SINGLE-TREE, which `vendor.rs` already enforces against
/// the lockfile. This rule is added to cover the one hole the existing gate left
/// open: `Tier::parse` accepted `vendor` at load and nothing downstream
/// distinguished it, so a boundary dependency could be re-declared as audited
/// vendored source and no refusal followed.
fn vendor_tier_refusals(edges: &[DirectEdge], register: &Contract) -> Vec<Refusal> {
    register
        .approvals()
        .filter(|entry| entry.tier() == VENDOR_TIER)
        .filter_map(|entry| {
            let source = edges
                .iter()
                .filter(|edge| !edge.workspace && entry.admits(&edge.package))
                .map(|edge| edge.source.class())
                .find(|class| VENDOR_SOURCE_CLASSES.contains(class))?;
            Some(Refusal::VendorTierConflict {
                krate: entry.krate().to_owned(),
                source: (*source).to_owned(),
            })
        })
        .collect()
}

/// The tier a vendored approval must be judged against.
///
/// Named rather than written inline so the refusal and this reader cannot
/// disagree about which tier is being policed.
const VENDOR_TIER: contract::Tier = contract::Tier::Vendor;

/// Every approval that re-tiers a frozen surface's own edge away from
/// [`FROZEN_TIER`], as one refusal each.
///
/// The second, structural half of INV-DEP-1's *never grow `lgwks_ast`*: adding
/// the edge at all is already refused as [`Refusal::UnregisteredEdge`], and
/// re-approving it as `vendor` rather than `boundary` is what a growth attempt
/// would reach for next. Both refusals name the surface, so a refusal says which
/// surface is in violation rather than only which crate is unowned.
fn frozen_surface_tier_refusals(register: &Contract) -> Vec<Refusal> {
    register
        .approvals()
        .filter(|entry| entry.tier() != FROZEN_TIER)
        .filter(|entry| FROZEN_SURFACES.contains(&entry.owner()))
        .map(|entry| Refusal::FrozenSurfaceTier {
            consumer: entry.owner().to_owned(),
            krate: entry.krate().to_owned(),
            tier: entry.tier().to_string(),
        })
        .collect()
}

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
                approved: entry.uses_default_features.unwrap_or(true),
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
                approved: entry.optional.unwrap_or(false),
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
                approved: entry
                    .target
                    .clone()
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| "<none>".to_owned()),
                declared: edge.target.clone().unwrap_or_else(|| "<none>".to_owned()),
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
            edge.target.as_deref().unwrap_or(""),
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
    let contract_path = contract_path.map_or_else(|| root.join(CONTRACT_PATH), Path::to_path_buf);
    ensure_contract_file(&contract_path)?;
    let register = Contract::parse(&read(&contract_path)?).map_err(GateError::Contract)?;
    // Parsed, not merely read. Reading proved the file exists; parsing is what
    // makes its contents auditable. A `[[package]]` block that names no package
    // would otherwise shrink the graph this gate reasons about without saying
    // so -- the failure `lock::parse` exists to refuse, reached only by the two
    // commands that happened to call it.
    lock::parse(&read(&lock_path)?).map_err(GateError::Lock)?;
    let edges = metadata::read(root).map_err(GateError::Metadata)?;
    Ok(edges.map(|edges| {
        let refusals = audit_direct(&edges, &register);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// What every test here returns.
    ///
    /// The workspace forbids `unwrap`/`expect` outright, with no test exemption,
    /// so a test propagates its failure with `?` instead of aborting the run.
    /// The message is carried in the error rather than in an added `.expect`.
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const REGISTER: &str = concat!(
        "[policy]\nenforce = true\n\n",
        "[[approved]]\n",
        "crate = \"serde\"\n",
        "tier = \"boundary\"\n",
        "version = \"1.0\"\n",
        "owner = \"lgwks_std\"\n",
        "capability = \"json.serialization\"\n",
        "license = \"MIT OR Apache-2.0\"\n",
        "source = \"registry\"\n",
        "allowed_consumers = \"lgwks_std\"\n",
        "allowed_kinds = \"normal\"\n",
        "reason = \"Derive-based serialization needs compiler introspection std lacks.\"\n",
        "approved_by = \"reviewer\"\n",
        "approved_on = \"2026-08-19\"\n",
        "review = \"docs/ADMISSION.md\"\n",
    );

    fn edge(consumer: &str, package: &str, requirement: &str) -> DirectEdge {
        DirectEdge {
            consumer: consumer.into(),
            package: package.into(),
            requirement: requirement.into(),
            kind: metadata::DependencyKind::Normal,
            source: metadata::DependencySource::Registry(
                "registry+https://github.com/rust-lang/crates.io-index".into(),
            ),
            optional: false,
            workspace: false,
            target_repository: None,
            features: Vec::new(),
            uses_default_features: true,
            target: None,
            rename: None,
            license: Some("MIT OR Apache-2.0".into()),
            license_file: None,
        }
    }

    /// One approval for `engine`, owned by `app`, with the given source class
    /// and optional exact origin.
    ///
    /// `origin` is `None` for a legacy class-only entry, which is the shape the
    /// origin tests must show is insufficient rather than permissive.
    fn register_with(
        source: &str,
        origin: Option<&str>,
    ) -> Result<Contract, contract::ContractError> {
        let origin_line = origin
            .map(|value| format!("origin = \"{value}\"\n"))
            .unwrap_or_default();
        Contract::parse(&format!(
            concat!(
                "[policy]\nenforce = true\n\n",
                "[[approved]]\n",
                "crate = \"engine\"\n",
                "tier = \"boundary\"\n",
                "version = \"1.0\"\n",
                "owner = \"app\"\n",
                "capability = \"engine.core\"\n",
                "source = \"{source}\"\n",
                "{origin}",
                "allowed_consumers = \"app\"\n",
                "allowed_kinds = \"normal\"\n",
                "reason = \"The engine supplies a capability the standard library cannot express.\"\n",
                "approved_by = \"reviewer\"\n",
                "approved_on = \"2026-09-30\"\n",
                "review = \"tests/origin_binding.rs\"\n",
            ),
            source = source,
            origin = origin_line,
        ))
    }

    /// The `app` → `engine` edge with a chosen source.
    fn app_edge(source: metadata::DependencySource) -> DirectEdge {
        let mut edge = edge("app", "engine", "1.0");
        edge.source = source;
        edge
    }

    #[test]
    fn direct_edge_requires_an_allowed_consumer() -> TestResult {
        let register = Contract::parse(REGISTER)?;
        assert_eq!(
            audit_direct(&[edge("braid-cli", "serde", "1.0")], &register),
            vec![
                Refusal::ConsumerNotAllowed {
                    consumer: "braid-cli".into(),
                    krate: "serde".into(),
                },
                Refusal::UnusedApproval {
                    krate: "serde".into(),
                    owner: "lgwks_std".into(),
                    capability: "json.serialization".into(),
                },
            ]
        );
        Ok(())
    }

    #[test]
    fn exact_owned_direct_edge_passes() -> TestResult {
        let register = Contract::parse(REGISTER)?;
        assert!(audit_direct(&[edge("lgwks_std", "serde", "1.0")], &register).is_empty());
        Ok(())
    }

    #[test]
    fn unregistered_path_copy_is_refused() -> TestResult {
        let register = Contract::parse(REGISTER)?;
        let mut copied = edge("app", "braid-ir", "*");
        copied.source = metadata::DependencySource::Path("../copied-braid-ir".into());
        assert!(matches!(
            audit_direct(&[copied], &register).first(),
            Some(Refusal::UnregisteredEdge { source, .. }) if source.starts_with("path:")
        ));
        Ok(())
    }

    #[test]
    fn copied_foreign_workspace_member_is_refused() -> TestResult {
        // `repository` goes *inside* the register's one `[policy]` block: the
        // reader refuses a second `[policy]`, which is the point of the
        // duplicate-section rule.
        let register = Contract::parse(&REGISTER.replace(
            "[policy]\nenforce = true",
            "[policy]\nrepository = \"https://example.invalid/consumer\"\nenforce = true",
        ))?;
        let mut copied = edge("app", "braid-ir", "*");
        copied.source = metadata::DependencySource::Path("vendor/braid-ir".into());
        copied.workspace = true;
        copied.target_repository = Some("https://github.com/srinji-kaggss/Braid".into());
        assert!(matches!(
            audit_direct(&[copied], &register).first(),
            Some(Refusal::ForeignWorkspaceMember { .. })
        ));
        Ok(())
    }

    /// The gate's own foundation is exempt because Cargo says it is a member of
    /// this workspace, not because of what it is called. This is the positive
    /// control for the name-only exemption the audit no longer has.
    #[test]
    fn a_workspace_member_is_exempt_from_external_admission() -> TestResult {
        let register = Contract::parse(REGISTER)?;
        let mut foundation = edge("lgwks_deps", "lgwks_std", "^0.6.6");
        foundation.source = metadata::DependencySource::Path("../lgwks-std".into());
        foundation.workspace = true;
        let refusals = audit_direct(&[foundation], &register);
        assert!(
            !refusals
                .iter()
                .any(|refusal| refusal.krate() == "lgwks_std"),
            "a validated workspace member needs no external approval; got {refusals:?}"
        );
        Ok(())
    }

    /// A package that is *not* a workspace member cannot buy admission with a
    /// name. Both the spelling the issue used and Cargo's `-`/`_` equivalent
    /// must reach the same refusal.
    #[test]
    fn an_external_package_claiming_a_self_exempt_name_is_refused() -> TestResult {
        let register = Contract::parse(REGISTER)?;
        for name in ["lgwks_std", "lgwks-std", "lgwks_deps", "LGWKS-STD"] {
            let mut impostor = edge("consumer", name, "*");
            impostor.source =
                metadata::DependencySource::Path("/outside-workspace/unreviewed".into());
            let refusals = audit_direct(&[impostor], &register);
            assert!(
                matches!(
                    refusals.first(),
                    Some(Refusal::UnregisteredEdge { source, .. })
                        if source == "path:/outside-workspace/unreviewed"
                ),
                "an external {name} must be refused with the source it came from; got {refusals:?}"
            );
        }
        Ok(())
    }

    /// The same name from a Git origin is refused too, and the refusal carries
    /// the Git URL so an approval cannot be satisfied by the spelling.
    #[test]
    fn an_external_git_package_claiming_a_self_exempt_name_is_refused() -> TestResult {
        let register = Contract::parse(REGISTER)?;
        let mut impostor = edge("consumer", "lgwks_std", "*");
        impostor.source = metadata::DependencySource::Git(
            "git+https://example.invalid/not-logical-works?rev=deadbeef".into(),
        );
        assert!(
            matches!(
                audit_direct(&[impostor], &register).first(),
                Some(Refusal::UnregisteredEdge { source, .. })
                    if source == "git:git+https://example.invalid/not-logical-works?rev=deadbeef"
            ),
            "a same-named Git source is an ordinary external edge"
        );
        Ok(())
    }

    /// Renaming the manifest key does not help either: metadata reports the
    /// upstream package name, which is what the audit reads.
    #[test]
    fn a_manifest_renamed_edge_is_refused_under_its_upstream_name() -> TestResult {
        let register = Contract::parse(REGISTER)?;
        let mut impostor = edge("consumer", "lgwks_std", "*");
        impostor.source =
            metadata::DependencySource::Registry("registry+https://example.invalid".into());
        assert!(matches!(
            audit_direct(&[impostor], &register).first(),
            Some(Refusal::UnregisteredEdge { krate, .. }) if krate == "lgwks_std"
        ));
        Ok(())
    }

    #[test]
    fn registry_to_git_source_drift_is_refused() -> TestResult {
        let register = Contract::parse(REGISTER)?;
        let mut git = edge("lgwks_std", "serde", "1.0");
        git.source =
            metadata::DependencySource::Git("git+https://example.invalid/serde?rev=abc#abc".into());
        assert!(
            audit_direct(&[git], &register)
                .iter()
                .any(|refusal| matches!(refusal, Refusal::SourceDrift { .. }))
        );
        Ok(())
    }

    #[test]
    fn unused_approval_is_refused() -> TestResult {
        let register = Contract::parse(REGISTER)?;
        assert!(matches!(
            audit_direct(&[], &register).as_slice(),
            [Refusal::UnusedApproval { .. }]
        ));
        Ok(())
    }

    /// An authored Git origin admits its own repository and refuses another,
    /// with both identities in the refusal.
    #[test]
    fn an_approved_git_origin_admits_only_that_repository() -> TestResult {
        let register = register_with("git", Some("git+https://approved.example/engine?rev=abc"))?;
        let approved = app_edge(metadata::DependencySource::Git(
            "git+https://approved.example/engine?rev=abc".into(),
        ));
        assert!(
            audit_direct(&[approved], &register).is_empty(),
            "the approved Git origin must pass"
        );

        let substituted = app_edge(metadata::DependencySource::Git(
            "git+https://different.example/engine?rev=abc".into(),
        ));
        let refusals = audit_direct(&[substituted], &register);
        assert!(
            matches!(
                refusals.first(),
                Some(Refusal::OriginDrift { approved, declared, .. })
                    if approved.contains("approved.example") && declared.contains("different.example")
            ),
            "a substituted Git repository must be an OriginDrift naming both origins: {refusals:?}"
        );
        Ok(())
    }

    /// A Git revision/reference policy change inside the approved repository is
    /// a drift: the repository is admitted, the pin is not.
    #[test]
    fn a_git_revision_policy_change_is_an_origin_drift() -> TestResult {
        let register = register_with("git", Some("git+https://repo.example/engine?rev=abc"))?;
        for substituted in [
            "git+https://repo.example/engine?rev=def",
            "git+https://repo.example/engine?branch=main",
            "git+https://repo.example/engine",
        ] {
            let edge = app_edge(metadata::DependencySource::Git(substituted.into()));
            let refusals = audit_direct(&[edge], &register);
            assert!(
                matches!(refusals.first(), Some(Refusal::OriginDrift { declared, .. }) if declared == substituted),
                "a changed revision policy ({substituted}) must be an OriginDrift: {refusals:?}"
            );
        }
        Ok(())
    }

    /// A different registry is refused even though the source class matches.
    #[test]
    fn an_approved_registry_origin_refuses_a_different_registry() -> TestResult {
        let register = register_with("registry", Some("registry+https://approved.example/index"))?;
        let approved = app_edge(metadata::DependencySource::Registry(
            "registry+https://approved.example/index".into(),
        ));
        assert!(audit_direct(&[approved], &register).is_empty());

        let substituted = app_edge(metadata::DependencySource::Registry(
            "registry+https://different.example/index".into(),
        ));
        let refusals = audit_direct(&[substituted], &register);
        assert!(
            matches!(
                refusals.first(),
                Some(Refusal::OriginDrift { declared, .. }) if declared.contains("different.example")
            ),
            "a different registry must be an OriginDrift: {refusals:?}"
        );
        Ok(())
    }

    /// A legacy class-only registry entry is exact for crates.io and nothing
    /// else: it is not implicit approval of every registry.
    #[test]
    fn a_class_only_registry_approval_admits_crates_io_only() -> TestResult {
        let register = register_with("registry", None)?;
        let crates_io = app_edge(metadata::DependencySource::Registry(CRATES_IO_INDEX.into()));
        assert!(
            audit_direct(&[crates_io], &register).is_empty(),
            "the one deterministic registry stays admitted by a class-only entry"
        );
        let sparse = app_edge(metadata::DependencySource::Registry(
            CRATES_IO_SPARSE.into(),
        ));
        assert!(
            audit_direct(&[sparse], &register).is_empty(),
            "the sparse spelling names the same crates.io registry"
        );

        let other = app_edge(metadata::DependencySource::Registry(
            "registry+https://different.example/index".into(),
        ));
        let refusals = audit_direct(&[other], &register);
        assert!(
            matches!(
                refusals.first(),
                Some(Refusal::OriginDrift { approved, .. }) if approved == CRATES_IO_INDEX
            ),
            "a class-only registry entry must admit crates.io only: {refusals:?}"
        );
        Ok(())
    }

    /// A class-only Git entry cannot grant exact-origin assurance: every Git
    /// origin is refused until an origin is authored.
    #[test]
    fn a_class_only_git_approval_is_insufficient_for_exact_origin() -> TestResult {
        let register = register_with("git", None)?;
        let edge = app_edge(metadata::DependencySource::Git(
            "git+https://any.example/engine?rev=abc".into(),
        ));
        let refusals = audit_direct(&[edge], &register);
        assert!(
            matches!(
                refusals.first(),
                Some(Refusal::OriginDrift { approved, .. }) if approved.contains("class-only")
            ),
            "a class-only Git entry must be insufficient, not permissive: {refusals:?}"
        );
        Ok(())
    }

    /// An authored external path authority admits only that path.
    #[test]
    fn an_approved_path_origin_refuses_a_different_path() -> TestResult {
        let register = register_with("path", Some("../vendor/engine"))?;
        let approved = app_edge(metadata::DependencySource::Path("../vendor/engine".into()));
        assert!(audit_direct(&[approved], &register).is_empty());

        let substituted = app_edge(metadata::DependencySource::Path("../vendor/evil".into()));
        let refusals = audit_direct(&[substituted], &register);
        assert!(
            matches!(
                refusals.first(),
                Some(Refusal::OriginDrift { approved, declared, .. })
                    if approved == "../vendor/engine" && declared == "../vendor/evil"
            ),
            "a different external path must be an OriginDrift: {refusals:?}"
        );
        Ok(())
    }

    /// An unknown scheme is not an ordinary admitted origin: it cannot be
    /// authored, and an edge carrying it does not match a registry approval.
    #[test]
    fn an_unknown_scheme_is_not_an_admitted_origin() -> TestResult {
        assert!(
            matches!(
                register_with("registry", Some("svn+https://example.invalid/x")),
                Err(contract::ContractError::InvalidField {
                    field: "origin",
                    ..
                })
            ),
            "an unknown origin scheme must be refused at register load"
        );
        let register = register_with("registry", None)?;
        let edge = app_edge(metadata::DependencySource::Other(
            "svn+https://example.invalid/x".into(),
        ));
        let refusals = audit_direct(&[edge], &register);
        assert!(
            matches!(refusals.first(), Some(Refusal::SourceDrift { .. })),
            "an unknown scheme is a class drift, never an ordinary admitted origin: {refusals:?}"
        );
        Ok(())
    }

    /// With several approvals for one crate, the reported drift is the one on
    /// the approval that admits the edge's source class, not the first source
    /// mismatch from an unrelated class.
    #[test]
    fn multiple_approvals_report_the_relevant_failed_dimension() -> TestResult {
        let text = concat!(
            "[policy]\nenforce = true\n\n",
            "[[approved]]\n",
            "crate = \"engine\"\ntier = \"boundary\"\nversion = \"1.0\"\nowner = \"app\"\n",
            "capability = \"engine.git\"\nsource = \"git\"\n",
            "origin = \"git+https://repo.example/engine\"\n",
            "allowed_consumers = \"app\"\nallowed_kinds = \"normal\"\n",
            "reason = \"The engine supplies a capability the standard library cannot express.\"\n",
            "approved_by = \"reviewer\"\napproved_on = \"2026-09-30\"\nreview = \"tests\"\n\n",
            "[[approved]]\n",
            "crate = \"engine\"\ntier = \"boundary\"\nversion = \"1.0\"\nowner = \"app\"\n",
            "capability = \"engine.registry\"\nsource = \"registry\"\n",
            "origin = \"registry+https://approved.example/index\"\n",
            "allowed_consumers = \"app\"\nallowed_kinds = \"normal\"\n",
            "reason = \"The engine supplies a capability the standard library cannot express.\"\n",
            "approved_by = \"reviewer\"\napproved_on = \"2026-09-30\"\nreview = \"tests\"\n",
        );
        let register = Contract::parse(text)?;

        // The edge's class is registry and the registry approval is present, so
        // the relevant drift is the kind, never the unrelated git source.
        let mut dev = app_edge(metadata::DependencySource::Registry(
            "registry+https://approved.example/index".into(),
        ));
        dev.kind = metadata::DependencyKind::Dev;
        let refusals = audit_direct(&[dev], &register);
        assert!(
            matches!(refusals.first(), Some(Refusal::KindNotAllowed { .. })),
            "the relevant kind drift must be reported, not the unrelated git source: {refusals:?}"
        );

        // A wrong requirement on the registry edge is likewise reported against
        // the registry approval, not the git half.
        let mut wrong_version = app_edge(metadata::DependencySource::Registry(
            "registry+https://approved.example/index".into(),
        ));
        wrong_version.requirement = "2.0".into();
        let refusals = audit_direct(&[wrong_version], &register);
        assert!(
            matches!(
                refusals.first(),
                Some(Refusal::RequirementDrift { approved, declared, .. })
                    if approved == "1.0" && declared == "2.0"
            ),
            "the registry approval's requirement must be the one compared: {refusals:?}"
        );
        Ok(())
    }

    /// INV-STDPLUS-APPROVED-ONLY: lgwks-std may depend on vetted leaf crates
    /// whose transitive trees bottom out at zero external deps. The gate itself
    /// stays zero-dep (self-refuting otherwise). This test enforces both.
    #[test]
    fn deps_are_approved_leaves() -> TestResult {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or("crates/lgwks-deps has no parent")?
            .parent()
            .ok_or("crates/ has no parent")?;

        // The deps storefront may depend only on the workspace facade it enforces,
        // plus the reviewed scan exception, plus optional storefront features
        // that the end user selects. The default build stays zero-dependency:
        // every third-party edge other than lgwks_std must be `optional = true`,
        // so `cargo build`/`cargo install` with no features pulls nothing.
        {
            let manifest = std::fs::read_to_string(workspace.join("crates/lgwks-deps/Cargo.toml"))?;
            let after = manifest
                .split("[dependencies]")
                .nth(1)
                .ok_or("gate declares [dependencies]")?;
            let declared: Vec<&str> = after
                .lines()
                .map(str::trim)
                .take_while(|line| !line.starts_with('['))
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .collect();
            let names: Vec<&str> = declared
                .iter()
                .map(|line| line.split('=').next().unwrap_or("").trim())
                .collect();
            assert_eq!(
                names,
                [
                    "lgwks_std",
                    "syn",
                    "proc-macro2",
                    "quote",
                    "gpui",
                    "appcui",
                    "bevy_ecs",
                    "bevy_app",
                    "bevy_time",
                    "bevy_state",
                    "tokio",
                    "candle-core",
                    "candle-nn",
                    "candle-transformers",
                    "tokenizers",
                ],
                "unexpected gate dependencies: {declared:?}"
            );
            for line in &declared {
                if !line.starts_with("lgwks_std") {
                    assert!(
                        line.contains("optional = true"),
                        "storefront dependency must stay optional so the default build is \
                         zero-dependency: {line}"
                    );
                }
            }
            assert!(declared[0].starts_with("lgwks_std ="));
        }

        // lgwks-std may only declare deps on this approved list.
        {
            const APPROVED: &[&str] = &[
                "blake3",
                "getrandom",
                "iri-string",
                "regex",
                "rkyv",
                "ron",
                "rustix",
                "serde",
                "serde_json",
                "tracing",
                "tracing-subscriber",
                "ureq",
            ];

            let manifest = std::fs::read_to_string(workspace.join("crates/lgwks-std/Cargo.toml"))?;
            let after = manifest
                .split("[dependencies]")
                .nth(1)
                .ok_or("std declares [dependencies]")?;
            let declared: Vec<&str> = after
                .lines()
                .map(str::trim)
                .take_while(|line| !line.starts_with('['))
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .collect();
            for line in &declared {
                let name = line.split('=').next().unwrap_or("").trim();
                assert!(
                    APPROVED.contains(&name),
                    "lgwks-std declares unapproved dependency `{name}` — \
                     add it to APPROVED in this test after review"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn a_repository_root_is_the_nearest_ancestor_holding_a_lock() -> TestResult {
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        let root = repository_root(here)?;
        assert!(root.join("Cargo.lock").is_file());
        Ok(())
    }
}
