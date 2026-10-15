//! What an invariant audit can say: the verdict ladder, the semantic
//! refusals produced after a register parses, and the registration errors
//! that stop it parsing.
//!
//! Split out of `invariants.rs` by responsibility; every type is re-exported
//! there, so its public path is unchanged.

use std::error::Error;
use std::fmt;
use std::path::PathBuf;

use super::{EvidenceGap, MAX_REVISION, MIN_REVISION, PASSING_RESULT, PathRefusal};

// ── Verdicts ────────────────────────────────────────────────────────────────

/// What the audit was able to establish about one invariant.
///
/// The three states are deliberately distinct, because collapsing them is the
/// defect this module repairs: `Resolved` says a mechanism exists and nothing
/// more, and only `Attested` claims a run was observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Status {
    /// The entry was refused; it has no honest status beyond this.
    Refused,
    /// Every reference resolved to a real mechanism. Nothing was executed, so
    /// this is not evidence that the invariant holds.
    Resolved,
    /// Every reference resolved and the entry carries a complete recorded run
    /// bound to the reviewed revision and to this exact enforcer.
    Attested,
}

impl Status {
    /// Stable spelling used by the human and machine renderings.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Refused => "refused",
            Self::Resolved => "resolved",
            Self::Attested => "attested",
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One invariant's place in the verdict ladder.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Outcome {
    /// Invariant identifier.
    pub(super) id: String,
    /// What the audit established about it.
    pub status: Status,
    /// One-based line where the invariant opened.
    pub line: usize,
}

impl Outcome {
    /// Invariant identifier bound to this audit result.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
}

/// The result of resolving a register against one repository.
///
/// Fields are private to the `invariants` module and reached through
/// accessors, so a consumer cannot mutate a verdict after the fact;
/// `#[non_exhaustive]` keeps a later count or classification additive.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct Audit {
    /// Refusals, in deterministic display order.
    pub(super) refusals: Vec<Refusal>,
    /// One outcome per registered invariant, in source order.
    pub(super) outcomes: Vec<Outcome>,
}

impl Audit {
    /// Refusals, sorted by their rendered text.
    #[must_use]
    pub fn refusals(&self) -> &[Refusal] {
        &self.refusals
    }

    /// One outcome per registered invariant, in source order.
    #[must_use]
    pub fn outcomes(&self) -> &[Outcome] {
        &self.outcomes
    }

    /// Number of registered invariants.
    #[must_use]
    pub fn registered(&self) -> usize {
        self.outcomes.len()
    }

    /// Number of invariants refused.
    #[must_use]
    pub fn refused(&self) -> usize {
        self.count(Status::Refused)
    }

    /// Number of invariants whose references resolved with no run recorded.
    #[must_use]
    pub fn resolved(&self) -> usize {
        self.count(Status::Resolved)
    }

    /// Number of invariants attested by a recorded run.
    #[must_use]
    pub fn attested(&self) -> usize {
        self.count(Status::Attested)
    }

    /// Counts outcomes carrying `status`.
    fn count(&self, status: Status) -> usize {
        self.outcomes
            .iter()
            .filter(|outcome| outcome.status == status)
            .count()
    }
}

/// The precise boundary of an invariant audit's authority.
///
/// Printed beside every verdict. A metadata-only check that does not say it is
/// metadata-only is how `enforced` came to appear next to a pass.
pub const SCOPE: &str = "registration and reference resolution only: no enforcer was executed, so this is not proof \
     that an invariant holds";

/// A semantic refusal produced after an invariant register has parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Refusal {
    /// The invariant does not name a path or lint that enforces it.
    MissingEnforcedBy {
        /// Invariant identifier.
        id: String,
        /// One-based line where the invariant opened.
        line: usize,
    },
    /// The authored enforcement path is absent from the repository.
    EnforcedByNotFound {
        /// Invariant identifier.
        id: String,
        /// Authored enforcement path.
        enforced_by: String,
        /// Resolved path checked by the gate.
        path: PathBuf,
        /// One-based line where the invariant opened.
        line: usize,
    },
    /// The named file exists but declares no test item a runner would select.
    EnforcerIsNotATest {
        /// Invariant identifier.
        id: String,
        /// Resolved path that declares no active test.
        path: PathBuf,
        /// One-based line where the invariant opened.
        line: usize,
    },
    /// The invariant names no workspace package or module rooted in one.
    ScopeNotInWorkspace {
        /// Invariant identifier.
        id: String,
        /// Authored scope.
        scope: String,
        /// One-based line where the invariant opened.
        line: usize,
    },
    /// The scope names a module no `mod` item in the workspace reaches.
    ScopeModuleNotFound {
        /// Invariant identifier.
        id: String,
        /// Authored scope.
        scope: String,
        /// Module path that did not resolve.
        module: String,
        /// One-based line where the invariant opened.
        line: usize,
    },
    /// The named lint is not declared by any manifest in this repository.
    LintNotDeclared {
        /// Invariant identifier.
        id: String,
        /// Lint selector, namespace included.
        lint: String,
        /// One-based line where the invariant opened.
        line: usize,
    },
    /// The named lint is declared, but at a level that cannot fail a build.
    LintDisabled {
        /// Invariant identifier.
        id: String,
        /// Lint selector, namespace included.
        lint: String,
        /// The level the repository declares.
        level: String,
        /// One-based line where the invariant opened.
        line: usize,
    },
    /// The recorded run is not about the tree under review: its revision is
    /// unknown or foreign, or the enforcer changed after it, or Git could not say.
    EvidenceNotBound {
        /// Invariant identifier.
        id: String,
        /// The revision the evidence names.
        revision: String,
        /// Why it does not bind.
        gap: EvidenceGap,
        /// One-based line where the invariant opened.
        line: usize,
    },
    /// An identifier was authored more than once.
    DuplicateId {
        /// Repeated invariant identifier.
        id: String,
        /// One-based line where the duplicate opened.
        line: usize,
    },
    /// The identifier is not `INV-<SCOPE>-<SLUG>`.
    MalformedId {
        /// Invalid invariant identifier.
        id: String,
        /// One-based line where the invariant opened.
        line: usize,
    },
}

impl Refusal {
    /// Returns the invariant identifier named by this refusal.
    #[must_use]
    pub fn id(&self) -> &str {
        match *self {
            Self::MissingEnforcedBy { ref id, .. }
            | Self::EnforcedByNotFound { ref id, .. }
            | Self::EnforcerIsNotATest { ref id, .. }
            | Self::ScopeNotInWorkspace { ref id, .. }
            | Self::ScopeModuleNotFound { ref id, .. }
            | Self::LintNotDeclared { ref id, .. }
            | Self::LintDisabled { ref id, .. }
            | Self::EvidenceNotBound { ref id, .. }
            | Self::DuplicateId { ref id, .. }
            | Self::MalformedId { ref id, .. } => id,
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::MissingEnforcedBy { ref id, line } => write!(
                formatter,
                "line {line}: invariant {id:?} has no enforced_by path or lint"
            ),
            Self::EnforcedByNotFound {
                ref id,
                ref enforced_by,
                ref path,
                line,
            } => write!(
                formatter,
                "line {line}: invariant {id:?} names enforced_by {enforced_by:?}, but {} does not exist",
                path.display()
            ),
            Self::EnforcerIsNotATest {
                ref id,
                ref path,
                line,
            } => write!(
                formatter,
                "line {line}: invariant {id:?} names {}, but that file declares no test item, \
                 so nothing about it can fail",
                path.display()
            ),
            Self::ScopeNotInWorkspace {
                ref id,
                ref scope,
                line,
            } => write!(
                formatter,
                "line {line}: invariant {id:?} scope {scope:?} is not a workspace package or module"
            ),
            Self::ScopeModuleNotFound {
                ref id,
                ref scope,
                ref module,
                line,
            } => write!(
                formatter,
                "line {line}: invariant {id:?} scope {scope:?} names module {module:?}, which no \
                 `mod` item in that package reaches"
            ),
            Self::LintNotDeclared {
                ref id,
                ref lint,
                line,
            } => write!(
                formatter,
                "line {line}: invariant {id:?} names lint {lint}, which no manifest in this \
                 repository declares, so it can never fire"
            ),
            Self::LintDisabled {
                ref id,
                ref lint,
                ref level,
                line,
            } => write!(
                formatter,
                "line {line}: invariant {id:?} names lint {lint}, which this repository declares \
                 at {level:?}; nothing below `warn` can fail a build"
            ),
            Self::EvidenceNotBound {
                ref id,
                ref revision,
                ref gap,
                line,
            } => write!(
                formatter,
                "line {line}: invariant {id:?} records a run at {revision}, which does not bind \
                 to this tree: {gap}"
            ),
            Self::DuplicateId { ref id, line } => {
                write!(formatter, "line {line}: invariant id {id:?} is duplicated")
            }
            Self::MalformedId { ref id, line } => write!(
                formatter,
                "line {line}: invariant id {id:?} must match INV-<SCOPE>-<SLUG>"
            ),
        }
    }
}

// ── Errors ──────────────────────────────────────────────────────────────────

/// Why the optional invariant register could not produce a verdict.
#[derive(Debug)]
#[non_exhaustive]
pub enum InvariantError {
    /// The register is not valid under the shared parser or invariant schema.
    Register(ErrorKind),
    /// The register exists but could not be read.
    Unreadable {
        /// Path that could not be read.
        path: PathBuf,
        /// Underlying I/O cause.
        cause: std::io::Error,
    },
    /// Cargo workspace metadata could not be obtained.
    Metadata(crate::metadata::MetadataError),
}

impl fmt::Display for InvariantError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Register(ref error) => error.fmt(formatter),
            Self::Unreadable {
                ref path,
                ref cause,
            } => {
                write!(formatter, "cannot read {}: {cause}", path.display())
            }
            Self::Metadata(ref error) => write!(formatter, "Cargo metadata: {error}"),
        }
    }
}

impl Error for InvariantError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match *self {
            Self::Register(ref error) => Some(error),
            Self::Unreadable { ref cause, .. } => Some(cause),
            Self::Metadata(ref error) => Some(error),
        }
    }
}

impl From<ErrorKind> for InvariantError {
    fn from(error: ErrorKind) -> Self {
        Self::Register(error)
    }
}

/// Why an invariant register cannot be parsed as a register.
///
/// Everything here is a registration problem, decided without looking at the
/// repository: schema, closed grammars, confinement, and the shape of a
/// recorded run. Anything that needs to know what is on disk is a [`Refusal`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// A line was not valid register syntax.
    Malformed {
        /// One-based line number.
        line: usize,
        /// The offending text.
        text: String,
    },
    /// A key is outside the invariant schema.
    UnknownKey {
        /// One-based line number.
        line: usize,
        /// The offending key.
        key: String,
    },
    /// A key appeared before a register section.
    OrphanKey {
        /// One-based line number.
        line: usize,
        /// The offending key.
        key: String,
    },
    /// A required authored field is absent or blank.
    MissingField {
        /// Invariant identifier, absent when the block declared no `id`.
        ///
        /// An `Option` because a block that names no invariant has no name to
        /// report: a placeholder would be a name no register ever wrote. The
        /// block's line identifies it instead, as it does every other refusal in
        /// this enum.
        id: Option<String>,
        /// The absent field.
        field: &'static str,
        /// One-based line where the block that lacks the field opened.
        line: usize,
    },
    /// The review date is not shaped as `YYYY-MM-DD`.
    BadDate {
        /// Invariant identifier.
        id: String,
        /// One-based line where the field was written.
        line: usize,
        /// The invalid value.
        value: String,
    },
    /// The statement has no finite good or bad prefix for this register.
    NonMonitorable {
        /// Invariant identifier.
        id: String,
        /// Why the statement cannot be admitted.
        reason: String,
    },
    /// The entry names an enforcement kind outside the closed grammar.
    UnsupportedEnforcement {
        /// Invariant identifier.
        id: String,
        /// The authored kind.
        enforcement: String,
    },
    /// `enforced_by` is neither a lint selector nor a confined repository path.
    EnforcerNotConfined {
        /// Invariant identifier.
        id: String,
        /// The authored value, verbatim.
        spelling: String,
        /// Why it is not a confined reference.
        reason: PathRefusal,
    },
    /// A monitor carries no recorded run, which is its only possible proof.
    MonitorNeedsEvidence {
        /// Invariant identifier.
        id: String,
    },
    /// A recorded run was started but not finished.
    EvidenceIncomplete {
        /// Invariant identifier.
        id: String,
        /// The first field that is missing.
        missing: &'static str,
    },
    /// The recorded result is not the one observed result this gate admits.
    EvidenceNotPassing {
        /// Invariant identifier.
        id: String,
        /// The authored result.
        result: String,
    },
    /// The recorded revision is not shaped like a revision.
    EvidenceRevisionShape {
        /// Invariant identifier.
        id: String,
        /// The authored revision.
        revision: String,
    },
    /// The recorded invocation does not name the entry's own enforcer.
    EvidenceDoesNotNameEnforcer {
        /// Invariant identifier.
        id: String,
        /// The authored invocation.
        invocation: String,
        /// Selector the invocation was required to name as a whole token.
        selector: String,
    },
    /// A `[policy]` value was outside its closed grammar.
    BadPolicyValue {
        /// One-based line number.
        line: usize,
        /// The offending key.
        key: String,
        /// The offending value, verbatim, quotes included.
        value: String,
    },
    /// The same `[policy]` key was written more than once.
    DuplicatePolicyKey {
        /// One-based line number.
        line: usize,
        /// The repeated key.
        key: String,
    },
    /// `[policy]` was declared more than once.
    DuplicatePolicySection {
        /// One-based line number of the second declaration.
        line: usize,
    },
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Malformed { line, ref text } => {
                write!(formatter, "line {line}: cannot parse {text:?}")
            }
            Self::UnknownKey { line, ref key } => {
                write!(formatter, "line {line}: unknown invariant key {key:?}")
            }
            Self::OrphanKey { line, ref key } => write!(
                formatter,
                "line {line}: invariant key {key:?} appears before any section header"
            ),
            Self::MissingField {
                ref id,
                field,
                line,
            } => match id.as_deref() {
                Some(id) => write!(
                    formatter,
                    "line {line}: invariant {id:?} is missing required field {field:?}"
                ),
                None => write!(
                    formatter,
                    "line {line}: invariant block is missing required field {field:?}"
                ),
            },
            Self::BadDate {
                ref id,
                line,
                ref value,
            } => write!(
                formatter,
                "line {line}: invariant {id:?} has invalid approved_on {value:?}; want a real YYYY-MM-DD date"
            ),
            Self::NonMonitorable { ref id, ref reason } => {
                write!(formatter, "invariant {id:?} is not monitorable: {reason}")
            }
            Self::UnsupportedEnforcement {
                ref id,
                ref enforcement,
            } => write!(
                formatter,
                "invariant {id:?} uses enforcement {enforcement:?}; want static-check or monitor"
            ),
            Self::EnforcerNotConfined {
                ref id,
                ref spelling,
                reason,
            } => write!(
                formatter,
                "invariant {id:?} names enforced_by {spelling:?}, which is not a reference this \
                 gate can resolve: {reason}"
            ),
            Self::MonitorNeedsEvidence { ref id } => write!(
                formatter,
                "invariant {id:?} is a monitor with no recorded run; a monitor has no repository \
                 artifact to resolve, so evidence_revision, evidence_invocation and \
                 evidence_result are required"
            ),
            Self::EvidenceIncomplete { ref id, missing } => write!(
                formatter,
                "invariant {id:?} records a run but is missing {missing}; a recorded run is a \
                 revision, an invocation and a result, or none of them"
            ),
            Self::EvidenceNotPassing { ref id, ref result } => write!(
                formatter,
                "invariant {id:?} records result {result:?}; only {PASSING_RESULT:?} is evidence \
                 that the check was observed to hold"
            ),
            Self::EvidenceRevisionShape {
                ref id,
                ref revision,
            } => write!(
                formatter,
                "invariant {id:?} records revision {revision:?}; want {MIN_REVISION}..={MAX_REVISION} \
                 hex digits naming the revision the run was observed at"
            ),
            Self::EvidenceDoesNotNameEnforcer {
                ref id,
                ref invocation,
                ref selector,
            } => write!(
                formatter,
                "invariant {id:?} records invocation {invocation:?}, which does not name its own \
                 enforcer {selector:?}; a run of something else is not evidence about this claim"
            ),
            Self::BadPolicyValue {
                line,
                ref key,
                ref value,
            } => write!(
                formatter,
                "line {line}: [policy] {key} = {value:?} is not a Boolean; write exactly true or false"
            ),
            Self::DuplicatePolicyKey { line, ref key } => write!(
                formatter,
                "line {line}: [policy] {key} is written more than once; keep one assignment"
            ),
            Self::DuplicatePolicySection { line } => write!(
                formatter,
                "line {line}: [policy] is declared more than once; a second declaration would \
                 silently override the first"
            ),
        }
    }
}

impl Error for ErrorKind {}
