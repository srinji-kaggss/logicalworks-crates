//! `lgwks_deps` owns dependency admission and enforces
//! INV-DEP-EDGE-OWNED: every external dependency authored by a workspace
//! package names its semantic owner, capability, source, requirement, allowed
//! consumers, and allowed dependency kinds.
//!
//! The Director's rule in one line: *if a library is not in `std` or `std+`,
//! it is not an approved dependency, and the code does not compile until a
//! human has registered it in the semantic contract.* Everything here exists to
//! make the second half of that sentence mechanically true rather than a
//! convention people remember.
//!
//! ## Enforcement boundary
//!
//! The same `lgwks-deps check` command is an explicit first lane in local and
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
//! register itself — a reviewable diff carrying a human's name, never an
//! environment variable a build can set for itself.
//!
//! ## Storefront
//!
//! `lgwks_deps` is also the estate's install-and-select surface for
//! third-party engines: enable `tokio` for the async runtime behind
//! `lgwks_bot::rt` (`use lgwks_deps::tokio::...` only when bypassing the bot
//! facade) or `gpui` for the GPU desktop UI. Capability features are
//! default-off; the `scan` gate-tool feature is the default-on exception for
//! `cargo install` CLI use. Library consumers take
//! `default-features = false` plus exactly the engine they need so the Rust
//! parser never rides along with a runtime edge.
//!
//! Do NOT `cargo add tokio` / `cargo add gpui` directly: the gate refuses any
//! second edge, and the facade (`lgwks_bot::rt`, `lgwks_deps::tokio`) is the
//! single entry the estate audits.

// Lint contract (missing_docs deny, unsafe_code forbid, broken intra-doc
// links deny) comes from the workspace root.

/// The approval register: parsing and lookup for `contract/APPROVED.toml`.
pub mod contract;
/// The register: parsing and approval lookup for `contract/APPROVED.toml`.
pub mod lock;
/// Cargo metadata edges: who authored which external dependency.
pub mod metadata;
/// Rust source scan: the keel zero-gate detectors behind `lgwks-deps scan` (feature `scan`).
#[cfg(feature = "scan")]
pub mod scan;
/// Vendor-tree coverage: binding the lockfile to the shared `vendor/` tree.
pub mod vendor;

/// The estate's one authored `tokio` edge, re-exported for the storefront.
///
/// `lgwks_deps` owns this edge so no other crate declares `tokio` directly
/// (`INV-DEP-EDGE-OWNED`). `lgwks_bot` enables the `tokio` storefront feature
/// and reaches the engine through this re-export; a consumer that wants the
/// engine without the bot enables the feature here.
#[cfg(feature = "tokio")]
pub use tokio;

/// The optional GPUI desktop UI framework selected through the storefront.
/// Re-exported so a consumer names the storefront, never the crate: `lgwks_bot`
/// reaches `bevy_ecs` through `lgwks_deps::bevy_ecs`, which is the same rule the
/// `tokio` edge follows.
#[cfg(feature = "bevy-ecs")]
pub use bevy_ecs;
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
/// the estate runtime reads a local checkpoint and does not call the hub.
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

/// Crates that are the gate, and so cannot be gated by it.
const SELF_EXEMPT: [&str; 2] = ["lgwks_std", "lgwks_deps"];

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
    /// The package is registered, but not for this dependency kind.
    KindNotAllowed {
        /// Package declaring the edge.
        consumer: String,
        /// External package name.
        krate: String,
        /// Authored dependency kind.
        kind: String,
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
            Self::KindNotAllowed {
                ref consumer,
                ref krate,
                ref kind,
            } => write!(
                formatter,
                "{consumer} declares {krate} as {kind}, but that edge kind is not approved"
            ),
            Self::UnusedApproval {
                ref krate,
                ref owner,
                ref capability,
            } => write!(
                formatter,
                "unused approval for {krate} capability {capability} owned by {owner}"
            ),
        }
    }
}

impl Refusal {
    /// The crate this refusal is about.
    #[must_use]
    pub fn krate(&self) -> &str {
        match *self {
            Self::ForeignWorkspaceMember { ref krate, .. }
            | Self::UnregisteredEdge { ref krate, .. }
            | Self::ConsumerNotAllowed { ref krate, .. }
            | Self::RequirementDrift { ref krate, .. }
            | Self::SourceDrift { ref krate, .. }
            | Self::KindNotAllowed { ref krate, .. }
            | Self::UnusedApproval { ref krate, .. } => krate,
        }
    }
}

/// Why the gate could not reach a verdict. Every variant is a refusal, not a
/// pass — see the fail-closed note on this module.
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
    /// The lock file could not be read.
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
            Self::Lock(ref error) => Some(error),
            Self::Metadata(ref error) => Some(error),
            _ => None,
        }
    }
}

// ── The audit ───────────────────────────────────────────────────────────────

/// Folds a package name to its comparison form: ASCII-lowercased, with `-`
/// rewritten to `_`.
///
/// Cargo treats `foo-bar` and `foo_bar` as the same package, so a register that
/// wrote one spelling must not read as absent merely because a manifest used
/// the other. The transform is deliberately ASCII-only: package names are ASCII
/// by Cargo's own rules, and a Unicode-aware fold would make the verdict depend
/// on locale.
fn normalise(name: &str) -> String {
    name.to_ascii_lowercase().replace('-', "_")
}

/// Whether `entry` names `consumer` among its allowed consumers.
///
/// Both sides pass through [`normalise`], so a register entry and a manifest
/// that spell the same package differently still compare equal.
fn allows_consumer(entry: &contract::Entry, consumer: &str) -> bool {
    entry
        .allowed_consumers
        .iter()
        .any(|allowed| normalise(allowed) == normalise(consumer))
}

/// Whether an approval admits this exact edge.
///
/// All four axes must hold: the consumer is allowed, the requirement string is
/// identical, the source class is identical, and the dependency kind is listed.
/// A partial match is not a weak admission — it is a refusal with a named axis,
/// which is why `audit_direct` re-tests each axis to report *which* one drifted.
fn edge_matches(entry: &contract::Entry, edge: &DirectEdge) -> bool {
    allows_consumer(entry, &edge.consumer)
        && entry.version == edge.requirement
        && entry.source == edge.source.class()
        && entry
            .allowed_kinds
            .iter()
            .any(|kind| kind == edge.kind.as_str())
}

/// Audits authored direct dependency edges against semantic ownership.
pub fn audit_direct(edges: &[DirectEdge], register: &Contract) -> Vec<Refusal> {
    let mut refusals = Vec::new();
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
    let external: Vec<&DirectEdge> = edges
        .iter()
        .filter(|edge| !edge.workspace && !is_self_exempt(&edge.package))
        .collect();
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
        let consumer_approvals: Vec<&&contract::Entry> = approvals
            .iter()
            .filter(|entry| allows_consumer(entry, &edge.consumer))
            .collect();
        if consumer_approvals.is_empty() {
            refusals.push(Refusal::ConsumerNotAllowed {
                consumer: edge.consumer.clone(),
                krate: edge.package.clone(),
            });
        } else if let Some(entry) = consumer_approvals
            .iter()
            .find(|entry| entry.source != edge.source.class())
        {
            refusals.push(Refusal::SourceDrift {
                consumer: edge.consumer.clone(),
                krate: edge.package.clone(),
                approved: entry.source.clone(),
                declared: edge.source.class().to_owned(),
            });
        } else if let Some(entry) = consumer_approvals
            .iter()
            .find(|entry| entry.version != edge.requirement)
        {
            refusals.push(Refusal::RequirementDrift {
                consumer: edge.consumer.clone(),
                krate: edge.package.clone(),
                approved: entry.version.clone(),
                declared: edge.requirement.clone(),
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
            normalise(&edge.package) == normalise(&entry.krate)
                && normalise(&edge.consumer) == normalise(&entry.owner)
                && edge_matches(entry, edge)
        });
        if !used {
            refusals.push(Refusal::UnusedApproval {
                krate: entry.krate.clone(),
                owner: entry.owner.clone(),
                capability: entry.capability.clone(),
            });
        }
    }
    refusals.sort_by_key(ToString::to_string);
    refusals
}

/// Whether `name` is the gate itself or the substrate it is built on.
///
/// The gate cannot be gated by itself — auditing `lgwks_deps` against a register
/// it parses is circular — and `lgwks_std` is the facade this crate is compiled
/// against, so an edge to it is the crate's own foundation rather than an
/// admission decision. Everything else, including an unnamed path copy, is
/// audited.
fn is_self_exempt(name: &str) -> bool {
    let normalised = name.to_ascii_lowercase().replace('-', "_");
    SELF_EXEMPT.contains(&normalised.as_str())
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

/// Audits the repository rooted at `root`, reading its lock file and register.
pub fn check_dependencies(root: &Path) -> Result<(Contract, Vec<Refusal>), GateError> {
    check_dependencies_against(root, &root.join(CONTRACT_PATH))
}

/// Audits `root` against a register held elsewhere. This exists for the `check
/// --contract` diagnosis path, where a repo is audited *before* it carries a
/// register of its own. `enforce` never calls it: a build always reads the
/// register committed beside the code it is building, so no build can be
/// pointed at a more permissive contract than the one in its own tree.
pub fn check_dependencies_against(
    root: &Path,
    contract_path: &Path,
) -> Result<(Contract, Vec<Refusal>), GateError> {
    let lock_path = root.join("Cargo.lock");
    let contract_path = contract_path.to_path_buf();
    ensure_contract_file(&contract_path)?;
    let register = Contract::parse(&read(&contract_path)?).map_err(GateError::Contract)?;
    read(&lock_path)?;
    let edges = metadata::read(root).map_err(GateError::Metadata)?;
    let refusals = audit_direct(&edges, &register);
    Ok((register, refusals))
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
        "source = \"registry\"\n",
        "allowed_consumers = \"lgwks_std\"\n",
        "allowed_kinds = \"normal\"\n",
        "reason = \"Derive-based serialization needs compiler introspection std lacks.\"\n",
        "approved_by = \"Director\"\n",
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
        }
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
        let register = Contract::parse(&format!(
            "[policy]\nrepository = \"https://example.invalid/consumer\"\n{REGISTER}"
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

        // The deps storefront may depend only on the estate facade it enforces,
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
                     add it to APPROVED in this test after Director review"
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
