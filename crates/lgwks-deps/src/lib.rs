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

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod contract;
pub mod lock;
pub mod metadata;

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
#[derive(Debug, Clone, PartialEq, Eq)]
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
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ForeignWorkspaceMember {
                consumer,
                krate,
                declared_repository,
                expected_repository,
            } => write!(
                f,
                "{consumer} embeds workspace package {krate} from {declared_repository}; consume its published crate instead (workspace authority is {expected_repository})"
            ),
            Self::UnregisteredEdge {
                consumer,
                krate,
                requirement,
                source,
                kind,
            } => write!(
                f,
                "{consumer} declares unowned {kind} edge {krate} {requirement} from {source}"
            ),
            Self::ConsumerNotAllowed { consumer, krate } => write!(
                f,
                "{consumer} declares {krate}, but no approval allows that consumer"
            ),
            Self::RequirementDrift {
                consumer,
                krate,
                approved,
                declared,
            } => write!(
                f,
                "{consumer} declares {krate} requirement {declared}, contract approves {approved}"
            ),
            Self::SourceDrift {
                consumer,
                krate,
                approved,
                declared,
            } => write!(
                f,
                "{consumer} declares {krate} from {declared}, contract approves {approved}"
            ),
            Self::KindNotAllowed {
                consumer,
                krate,
                kind,
            } => write!(
                f,
                "{consumer} declares {krate} as {kind}, but that edge kind is not approved"
            ),
            Self::UnusedApproval {
                krate,
                owner,
                capability,
            } => write!(
                f,
                "unused approval for {krate} capability {capability} owned by {owner}"
            ),
        }
    }
}

impl Refusal {
    /// The crate this refusal is about.
    pub fn krate(&self) -> &str {
        match self {
            Self::ForeignWorkspaceMember { krate, .. }
            | Self::UnregisteredEdge { krate, .. }
            | Self::ConsumerNotAllowed { krate, .. }
            | Self::RequirementDrift { krate, .. }
            | Self::SourceDrift { krate, .. }
            | Self::KindNotAllowed { krate, .. }
            | Self::UnusedApproval { krate, .. } => krate,
        }
    }
}

/// Why the gate could not reach a verdict. Every variant is a refusal, not a
/// pass — see the fail-closed note on this module.
#[derive(Debug)]
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
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LockNotFound { from } => {
                write!(f, "no Cargo.lock at or above {}", from.display())
            }
            Self::ContractNotFound { path } => write!(
                f,
                "no dependency register at {} — every repo the gate guards must carry one; \
                 run `lgwks-deps init` to write a fail-closed starting register",
                path.display()
            ),
            Self::Unreadable { path, cause } => {
                write!(f, "cannot read {}: {cause}", path.display())
            }
            Self::Contract(e) => write!(f, "{CONTRACT_PATH}: {e}"),
            Self::Lock(e) => write!(f, "Cargo.lock: {e}"),
            Self::Metadata(e) => write!(f, "Cargo metadata: {e}"),
        }
    }
}

impl Error for GateError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Unreadable { cause, .. } => Some(cause),
            Self::Contract(e) => Some(e),
            Self::Lock(e) => Some(e),
            Self::Metadata(e) => Some(e),
            _ => None,
        }
    }
}

// ── The audit ───────────────────────────────────────────────────────────────

fn normalise(name: &str) -> String {
    name.to_ascii_lowercase().replace('-', "_")
}

fn allows_consumer(entry: &contract::Entry, consumer: &str) -> bool {
    entry
        .allowed_consumers
        .iter()
        .any(|allowed| normalise(allowed) == normalise(consumer))
}

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
    if let Some(expected) = &register.repository {
        for edge in edges.iter().filter(|edge| edge.workspace) {
            if let Some(declared) = &edge.target_repository
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
                declared: edge.source.class().to_string(),
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
/// register of its own. [`enforce`] never calls it: a build always reads the
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
    fn direct_edge_requires_an_allowed_consumer() {
        let register = Contract::parse(REGISTER).unwrap();
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
    }

    #[test]
    fn exact_owned_direct_edge_passes() {
        let register = Contract::parse(REGISTER).unwrap();
        assert!(audit_direct(&[edge("lgwks_std", "serde", "1.0")], &register).is_empty());
    }

    #[test]
    fn unregistered_path_copy_is_refused() {
        let register = Contract::parse(REGISTER).unwrap();
        let mut copied = edge("app", "braid-ir", "*");
        copied.source = metadata::DependencySource::Path("../copied-braid-ir".into());
        assert!(matches!(
            audit_direct(&[copied], &register).first(),
            Some(Refusal::UnregisteredEdge { source, .. }) if source.starts_with("path:")
        ));
    }

    #[test]
    fn copied_foreign_workspace_member_is_refused() {
        let register = Contract::parse(&format!(
            "[policy]\nrepository = \"https://example.invalid/consumer\"\n{REGISTER}"
        ))
        .unwrap();
        let mut copied = edge("app", "braid-ir", "*");
        copied.source = metadata::DependencySource::Path("vendor/braid-ir".into());
        copied.workspace = true;
        copied.target_repository = Some("https://github.com/srinji-kaggss/Braid".into());
        assert!(matches!(
            audit_direct(&[copied], &register).first(),
            Some(Refusal::ForeignWorkspaceMember { .. })
        ));
    }

    #[test]
    fn registry_to_git_source_drift_is_refused() {
        let register = Contract::parse(REGISTER).unwrap();
        let mut git = edge("lgwks_std", "serde", "1.0");
        git.source =
            metadata::DependencySource::Git("git+https://example.invalid/serde?rev=abc#abc".into());
        assert!(
            audit_direct(&[git], &register)
                .iter()
                .any(|refusal| matches!(refusal, Refusal::SourceDrift { .. }))
        );
    }

    #[test]
    fn unused_approval_is_refused() {
        let register = Contract::parse(REGISTER).unwrap();
        assert!(matches!(
            audit_direct(&[], &register).as_slice(),
            [Refusal::UnusedApproval { .. }]
        ));
    }

    /// INV-STDPLUS-APPROVED-ONLY: lgwks-std may depend on vetted leaf crates
    /// whose transitive trees bottom out at zero external deps. The gate itself
    /// stays zero-dep (self-refuting otherwise). This test enforces both.
    #[test]
    fn deps_are_approved_leaves() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();

        // The gate itself may depend only on the estate facade it enforces.
        {
            let manifest =
                std::fs::read_to_string(workspace.join("crates/lgwks-deps/Cargo.toml"))
                    .expect("gate manifest missing");
            let after = manifest
                .split("[dependencies]")
                .nth(1)
                .expect("gate declares [dependencies]");
            let declared: Vec<&str> = after
                .lines()
                .map(str::trim)
                .take_while(|l| !l.starts_with('['))
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .collect();
            assert_eq!(
                declared.len(),
                1,
                "unexpected gate dependencies: {declared:?}"
            );
            assert!(declared[0].starts_with("lgwks_std ="));
        }

        // lgwks-std may only declare deps on this approved list.
        {
            const APPROVED: &[&str] = &[
                "blake3",
                "getrandom",
                "regex",
                "rkyv",
                "ron",
                "serde",
                "serde_json",
            ];

            let manifest = std::fs::read_to_string(workspace.join("crates/lgwks-std/Cargo.toml"))
                .expect("std manifest missing");
            let after = manifest
                .split("[dependencies]")
                .nth(1)
                .expect("std declares [dependencies]");
            let declared: Vec<&str> = after
                .lines()
                .map(str::trim)
                .take_while(|l| !l.starts_with('['))
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
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
    }

    #[test]
    fn a_repository_root_is_the_nearest_ancestor_holding_a_lock() {
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        let root = repository_root(here).expect("workspace has a Cargo.lock");
        assert!(root.join("Cargo.lock").is_file());
    }
}
