//! The filesystem entry points: find the repository, read its register and
//! lock file, collect Cargo's metadata, and return the receipt-bearing verdict.

use std::path::{Path, PathBuf};

use crate::contract::Contract;
use crate::metadata::{self, DirectEdge};
use crate::{
    CONTRACT_PATH, GateError, Refusal, audit_direct, declared_scope, invariants, lock,
    surface_refusals,
};

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
    // No override names the committed register beside the root: enforcement mode.
    let contract_path = match contract_path {
        Some(chosen) => chosen.to_path_buf(),
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
