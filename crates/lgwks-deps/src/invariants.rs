//! Authored invariant register and its refusal rules.
//!
//! The register deliberately goes through `crate::contract::parse_register`
//! rather than acquiring a TOML dependency or copying the dependency reader.
//! Parsing is shared; this module owns only the invariant schema and the
//! repository-aware checks that give each claim a real enforcement boundary.
//!
//! ## Four separate questions, four separate answers
//!
//! An invariant register can be asked four different things, and conflating
//! them is how a metadata check came to print `enforced`:
//!
//! 1. **Registration validity** — is the register well formed? Answered by
//!    [`Register::parse`], which refuses an unknown enforcement kind, an
//!    unconfined `enforced_by`, or an evidence block that is not a complete
//!    revision/invocation/result triple.
//! 2. **Reference resolution** — does each claim name a mechanism that exists
//!    in *this* repository? Answered by [`audit`]: a lint must be declared and
//!    active in the workspace's own `[lints]` tables, a file must exist inside
//!    the repository and declare an active test item, and a module scope must
//!    resolve to a source file some `mod` item actually reaches.
//! 3. **Execution** — did the named mechanism run and fail on a broken tree?
//!    *This module never answers this.* It executes nothing: no authored
//!    command string is run, and no lint is invoked. [`SCOPE`] states exactly
//!    that limit wherever a verdict is printed, because a doctor that reports
//!    more than it did is the defect this module exists to repair.
//! 4. **Verified outcome** — was a run recorded against the reviewed revision?
//!    Answered only by [`Evidence`], which is what separates [`Status::Attested`]
//!    from [`Status::Resolved`].
//!
//! Human approval is a fifth thing again, and it stays a string: a reviewer's
//! name is a trust decision recorded in a diff, and this module does not
//! pretend that a name is a measurement.
//!
//! [`Register::parse`]: crate::invariants::Register::parse
//! [`audit`]: crate::invariants::audit
//! [`Evidence`]: crate::invariants::Evidence
//! [`Status::Attested`]: crate::invariants::Status::Attested
//! [`Status::Resolved`]: crate::invariants::Status::Resolved
//! [`SCOPE`]: crate::invariants::SCOPE

use std::collections::BTreeSet;
use std::error::Error;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use crate::contract::{self, RawEntry};

/// Register location relative to the repository root.
pub const INVARIANTS_PATH: &str = "contract/INVARIANTS.toml";

/// Keys accepted by an `[[invariant]]` block.
const ENTRY_FIELDS: [&str; 12] = [
    "id",
    "statement",
    "scope",
    "owner",
    "enforcement",
    "enforced_by",
    "approved_by",
    "approved_on",
    "review",
    "evidence_revision",
    "evidence_invocation",
    "evidence_result",
];

/// The fields required for an invariant entry.
///
/// `enforced_by` is deliberately absent: its absence is a semantic refusal
/// ([`Refusal::MissingEnforcedBy`]) with its own diagnostic rather than a
/// generic parse failure, and the evidence fields are conditional on the
/// enforcement kind.
const REQUIRED_FIELDS: [&str; 8] = [
    "id",
    "statement",
    "scope",
    "owner",
    "enforcement",
    "approved_by",
    "approved_on",
    "review",
];

/// The one required field whose value is a member of a closed grammar.
///
/// `enforcement = ""` has supplied the field and supplied a value no run can
/// act on, which [`Enforcement::parse`] reports as a value outside the grammar.
/// Reporting it as absent would name a different defect with a different repair.
const ENFORCEMENT_FIELD: &str = "enforcement";

/// The three keys a recorded run is made of, in diagnostic order.
const EVIDENCE_FIELDS: [&str; 3] = [
    "evidence_revision",
    "evidence_invocation",
    "evidence_result",
];

/// The one observed result this gate admits as evidence.
const PASSING_RESULT: &str = "pass";

/// Shortest revision prefix accepted as naming a revision.
///
/// Seven is Git's own abbreviation floor, which is the shortest string a human
/// can be expected to copy out of `git log` without reaching for a tool.
const MIN_REVISION: usize = 7;

/// Longest revision string accepted, covering a full SHA-256 object name.
const MAX_REVISION: usize = 64;

// ── Enforcement kind ────────────────────────────────────────────────────────

/// How an invariant claims it is enforced.
///
/// A closed set parsed at load rather than a string matched at use, so a
/// register that writes `review` or `intent` is refused with a typed diagnostic
/// instead of being carried to a comparison that silently treats it as neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Enforcement {
    /// A repository check: a declared lint, or a file that declares a test.
    StaticCheck,
    /// A periodic monitor, whose only possible proof is a recorded run.
    Monitor,
}

impl Enforcement {
    /// Parses the authored spelling, or `None` when it is outside the grammar.
    fn parse(value: &str) -> Option<Self> {
        match value {
            "static-check" => Some(Self::StaticCheck),
            "monitor" => Some(Self::Monitor),
            _ => None,
        }
    }

    /// Stable contract spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StaticCheck => "static-check",
            Self::Monitor => "monitor",
        }
    }

    /// Whether a recorded run is the only thing that can attest this kind.
    ///
    /// A monitor has no repository artifact to resolve against — it runs on a
    /// schedule — so its entry is incomplete without evidence. A static check
    /// may still carry evidence, and then it is attested rather than resolved.
    #[must_use]
    pub const fn requires_evidence(self) -> bool {
        matches!(self, Self::Monitor)
    }
}

impl fmt::Display for Enforcement {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

// ── Typed enforcer references ───────────────────────────────────────────────

/// A lint namespace this gate knows how to look up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum LintNamespace {
    /// A Clippy lint, declared under `[lints.clippy]`.
    Clippy,
    /// A `rustc` lint, declared under `[lints.rust]`.
    Rustc,
    /// A `rustdoc` lint, declared under `[lints.rustdoc]`.
    Rustdoc,
}

impl LintNamespace {
    /// Parses the selector prefix a register writes.
    fn parse(value: &str) -> Option<Self> {
        match value {
            "clippy" => Some(Self::Clippy),
            "rustc" => Some(Self::Rustc),
            "rustdoc" => Some(Self::Rustdoc),
            _ => None,
        }
    }

    /// Parses the `[lints]` table name Cargo declares this namespace under.
    ///
    /// The `rustc` selector is `rustc::` but its table is `[lints.rust]`, so
    /// this mapping is not the identity and is kept as its own function.
    fn parse_table(value: &str) -> Option<Self> {
        match value {
            "clippy" => Some(Self::Clippy),
            "rust" => Some(Self::Rustc),
            "rustdoc" => Some(Self::Rustdoc),
            _ => None,
        }
    }

    /// Stable selector prefix.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Clippy => "clippy",
            Self::Rustc => "rustc",
            Self::Rustdoc => "rustdoc",
        }
    }
}

impl fmt::Display for LintNamespace {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Why an authored `enforced_by` is not a reference this gate will resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PathRefusal {
    /// The path is absolute, so it names a file outside the repository.
    Absolute,
    /// The path walks up out of the repository with `..`.
    ParentDirectory,
}

impl fmt::Display for PathRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Absolute => formatter.write_str("it is an absolute path"),
            Self::ParentDirectory => {
                formatter.write_str("it walks out of the repository with `..`")
            }
        }
    }
}

/// A repository-relative path that cannot name a file outside the repository.
///
/// The constructor is the only way to build one, so confinement is a property
/// of the type rather than a check a later reader has to remember. It is
/// lexical: nothing here follows a symlink, which is why the audit still joins
/// the path against the root and tests the result before believing it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub struct RepoPath(PathBuf);

impl RepoPath {
    /// Parses an authored path, refusing anything that escapes the repository.
    fn parse(value: &str) -> Result<Self, PathRefusal> {
        let path = Path::new(value);
        if path.is_absolute() {
            return Err(PathRefusal::Absolute);
        }
        for component in path.components() {
            match component {
                Component::ParentDir => return Err(PathRefusal::ParentDirectory),
                Component::RootDir | Component::Prefix(_) => return Err(PathRefusal::Absolute),
                Component::CurDir | Component::Normal(_) => {}
            }
        }
        Ok(Self(path.to_path_buf()))
    }

    /// The path as authored, relative to the repository root.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl fmt::Display for RepoPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0.display())
    }
}

/// The mechanism an invariant names as its enforcement.
///
/// Only two shapes resolve: a lint selector, and a file inside the repository.
/// An authored string that is neither is refused at load with
/// [`ErrorKind::EnforcerNotConfined`] rather than carried here, so every value
/// of this type is a reference the audit can actually go and look up.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EnforcedBy {
    /// A `clippy::`/`rustc::`/`rustdoc::` lint selector.
    Lint {
        /// Namespace the lint is declared under.
        namespace: LintNamespace,
        /// Lint name, without the namespace.
        name: String,
    },
    /// A file inside the repository.
    File(RepoPath),
}

impl EnforcedBy {
    /// Parses an authored selector, or `None` when it is neither a lint nor a
    /// confined path.
    ///
    /// A lint namespace wins over the path reading: `clippy::x` is a valid
    /// relative path syntactically, and letting it fall through to the file
    /// branch would report "no such file" for a claim about a lint.
    fn parse(value: &str) -> Result<Self, PathRefusal> {
        if let Some((namespace, name)) = value.split_once("::")
            && let Some(namespace) = LintNamespace::parse(namespace)
            && valid_lint_name(name)
        {
            return Ok(Self::Lint {
                namespace,
                name: name.to_owned(),
            });
        }
        RepoPath::parse(value).map(Self::File)
    }

    /// The exact spelling an execution record must name to be about this
    /// reference.
    ///
    /// `None` only for a value this type cannot hold; it exists so callers bind
    /// evidence without re-deriving the spelling from a second source.
    #[must_use]
    pub fn selector(&self) -> String {
        match *self {
            Self::Lint {
                namespace,
                ref name,
            } => format!("{namespace}::{name}"),
            Self::File(ref path) => path.to_string(),
        }
    }
}

impl fmt::Display for EnforcedBy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.selector())
    }
}

/// Whether `name` uses the alphabet a lint name is written in.
fn valid_lint_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

// ── Evidence ────────────────────────────────────────────────────────────────

/// A recorded run, bound to a revision, an invocation, and a result.
///
/// Constructed only by `build_evidence`, which refuses an incomplete triple,
/// a revision that is not a Git-style object name, a result that is not
/// `pass`, or an invocation that does not name the entry's own enforcer. What
/// is stored is therefore already bound to the reviewed revision and the exact
/// selector, which is what makes [`Status::Attested`] mean something.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Evidence {
    /// Revision the run was observed at.
    revision: String,
    /// Command that produced the observed result.
    invocation: String,
}

impl Evidence {
    /// Revision the recorded run was observed at.
    #[must_use]
    pub fn revision(&self) -> &str {
        &self.revision
    }

    /// Command that produced the observed result.
    #[must_use]
    pub fn invocation(&self) -> &str {
        &self.invocation
    }
}

/// Whether a recorded revision names a revision at all.
fn valid_revision(revision: &str) -> bool {
    (MIN_REVISION..=MAX_REVISION).contains(&revision.len())
        && revision
            .chars()
            .all(|character| character.is_ascii_hexdigit())
}

// ── Entry and register ──────────────────────────────────────────────────────

/// One declared, reviewed invariant.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Entry {
    /// Stable `INV-<SCOPE>-<SLUG>` identifier.
    pub id: String,
    /// Human-readable claim the repository is meant to preserve.
    pub statement: String,
    /// Workspace crate or Rust module path bound by the claim.
    pub scope: String,
    /// Workspace crate responsible for preserving the claim.
    pub owner: String,
    /// Enforcement kind, parsed from the closed two-word grammar.
    pub enforcement: Enforcement,
    /// The mechanism named as enforcement, or `None` when the entry names one
    /// and it is absent. Resolution against the repository happens in
    /// [`audit`], not here.
    pub enforced_by: Option<EnforcedBy>,
    /// Recorded run attesting the claim, when the entry carries one.
    pub evidence: Option<Evidence>,
    /// Human who approved the claim.
    pub approved_by: String,
    /// ISO date of the review.
    pub approved_on: String,
    /// Path or URL to the review evidence.
    pub review: String,
    /// One-based line where the entry opened.
    pub line: usize,
}

/// The parsed invariant register.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Register {
    /// Whether invariant refusals are enforcement failures.
    pub enforce: bool,
    /// Invariants in source order.
    pub entries: Vec<Entry>,
}

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
    pub id: String,
    /// What the audit established about it.
    pub status: Status,
    /// One-based line where the invariant opened.
    pub line: usize,
}

/// The result of resolving a register against one repository.
///
/// Fields are private and reached through accessors so a consumer cannot
/// mutate a verdict after the fact; `#[non_exhaustive]` keeps a later count or
/// classification additive.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct Audit {
    /// Refusals, in deterministic display order.
    refusals: Vec<Refusal>,
    /// One outcome per registered invariant, in source order.
    outcomes: Vec<Outcome>,
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
        /// Invariant identifier, or `<unnamed>` when `id` is absent.
        id: String,
        /// The absent field.
        field: &'static str,
    },
    /// The review date is not shaped as `YYYY-MM-DD`.
    BadDate {
        /// Invariant identifier.
        id: String,
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
        /// Selector the invocation was required to contain.
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
            Self::MissingField { ref id, field } => {
                write!(
                    formatter,
                    "invariant {id:?} is missing required field {field:?}"
                )
            }
            Self::BadDate { ref id, ref value } => write!(
                formatter,
                "invariant {id:?} has approved_on {value:?}, want YYYY-MM-DD"
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

// ── Loading ─────────────────────────────────────────────────────────────────

impl Register {
    /// Parses an invariant register through the dependency register's shared
    /// fail-closed line reader.
    pub fn parse(text: &str) -> Result<Self, ErrorKind> {
        let raw = contract::parse_register(text, "[[invariant]]", &ENTRY_FIELDS)
            .map_err(map_contract_error)?;
        let mut entries = Vec::with_capacity(raw.entries.len());
        for raw_entry in &raw.entries {
            entries.push(build(raw_entry)?);
        }
        Ok(Self {
            enforce: raw.enforce,
            entries,
        })
    }
}

/// Builds an invariant entry, refusing anything the schema cannot represent.
///
/// Order matters for diagnostics: the closed grammars are parsed before the
/// evidence that depends on them, so an entry that writes both a bad
/// enforcement kind and an unrelated evidence block is told about the
/// enforcement kind.
fn build(raw: &RawEntry) -> Result<Entry, ErrorKind> {
    let id = raw
        .get("id")
        .map_or_else(|| "<unnamed>".to_owned(), str::to_owned);
    for field in REQUIRED_FIELDS {
        match raw.get(field) {
            None => return Err(ErrorKind::MissingField { id, field }),
            // A blank value is a missing value for every free-text field, and
            // for `enforcement` it is a value outside the closed grammar — the
            // repair is different, so the diagnostic has to be.
            Some(value) if value.trim().is_empty() && field != ENFORCEMENT_FIELD => {
                return Err(ErrorKind::MissingField { id, field });
            }
            Some(_) => {}
        }
    }
    let approved_on = raw
        .get("approved_on")
        .map_or_else(String::new, str::to_owned);
    if !contract::is_iso_date(&approved_on) {
        return Err(ErrorKind::BadDate {
            id,
            value: approved_on,
        });
    }
    let statement = raw.get("statement").map_or_else(String::new, str::to_owned);
    if let Some(reason) = non_monitorable_reason(&statement) {
        return Err(ErrorKind::NonMonitorable { id, reason });
    }
    let authored_kind = raw
        .get("enforcement")
        .map_or_else(String::new, str::to_owned);
    let Some(enforcement) = Enforcement::parse(&authored_kind) else {
        return Err(ErrorKind::UnsupportedEnforcement {
            id,
            enforcement: authored_kind,
        });
    };
    let enforced_by = match raw.get("enforced_by") {
        None => None,
        Some(value) if value.trim().is_empty() => None,
        Some(value) => {
            Some(
                EnforcedBy::parse(value).map_err(|reason| ErrorKind::EnforcerNotConfined {
                    id: id.clone(),
                    spelling: value.to_owned(),
                    reason,
                })?,
            )
        }
    };
    let evidence = build_evidence(raw, &id, enforcement, enforced_by.as_ref())?;
    Ok(Entry {
        id,
        statement,
        owner: raw.get("owner").map_or_else(String::new, str::to_owned),
        scope: raw.get("scope").map_or_else(String::new, str::to_owned),
        enforcement,
        enforced_by,
        evidence,
        approved_by: raw
            .get("approved_by")
            .map_or_else(String::new, str::to_owned),
        approved_on,
        review: raw.get("review").map_or_else(String::new, str::to_owned),
        line: raw.line(),
    })
}

/// Validates and assembles the optional recorded run.
///
/// The triple is all-or-nothing: a revision with no invocation is a note, not a
/// run, and admitting it would put a revision beside a verdict no execution
/// produced. `enforcer` is the entry's own resolved reference, and a non-empty
/// invocation must contain its spelling — a passing run of some *other* check
/// is exactly the false comfort this rule exists to refuse.
fn build_evidence(
    raw: &RawEntry,
    id: &str,
    enforcement: Enforcement,
    enforcer: Option<&EnforcedBy>,
) -> Result<Option<Evidence>, ErrorKind> {
    let present: Vec<&str> = EVIDENCE_FIELDS
        .iter()
        .copied()
        .filter(|field| raw.get(field).is_some_and(|value| !value.trim().is_empty()))
        .collect();
    if present.is_empty() {
        if enforcement.requires_evidence() {
            return Err(ErrorKind::MonitorNeedsEvidence { id: id.to_owned() });
        }
        return Ok(None);
    }
    for field in EVIDENCE_FIELDS {
        if !present.contains(&field) {
            return Err(ErrorKind::EvidenceIncomplete {
                id: id.to_owned(),
                missing: field,
            });
        }
    }
    let field = |name: &str| {
        raw.get(name)
            .map_or_else(String::new, |value| value.trim().to_owned())
    };
    let revision = field("evidence_revision");
    if !valid_revision(&revision) {
        return Err(ErrorKind::EvidenceRevisionShape {
            id: id.to_owned(),
            revision,
        });
    }
    let result = field("evidence_result");
    if result != PASSING_RESULT {
        return Err(ErrorKind::EvidenceNotPassing {
            id: id.to_owned(),
            result,
        });
    }
    let invocation = field("evidence_invocation");
    if let Some(selector) = enforcer.map(EnforcedBy::selector)
        && !invocation.contains(&selector)
    {
        return Err(ErrorKind::EvidenceDoesNotNameEnforcer {
            id: id.to_owned(),
            invocation,
            selector,
        });
    }
    Ok(Some(Evidence {
        revision,
        invocation,
    }))
}

/// Returns a load-time diagnostic for constraints that cannot be decided from
/// any finite monitor prefix. The register intentionally keeps this language
/// small: modal eventuality and unbounded quantification are documentation,
/// not executable enforcement.
fn non_monitorable_reason(statement: &str) -> Option<String> {
    let compact = statement
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase()
        .replace('→', "->");
    if compact.contains("g(") && compact.contains("->f") {
        return Some("G(r -> F a) has no finite good or bad prefix".to_owned());
    }
    if compact.contains("forall(")
        || compact.contains("exists(")
        || compact.contains('∀')
        || compact.contains('∃')
    {
        return Some("unbounded quantification is outside the bounded scalar domain".to_owned());
    }
    None
}

/// Maps the shared parser's structural errors into this register's diagnostic
/// namespace without giving invariants a second parser.
fn map_contract_error(error: contract::ContractError) -> ErrorKind {
    match error {
        contract::ContractError::Malformed { line, text } => ErrorKind::Malformed { line, text },
        contract::ContractError::UnknownKey { line, key } => ErrorKind::UnknownKey { line, key },
        contract::ContractError::OrphanKey { line, key } => ErrorKind::OrphanKey { line, key },
        contract::ContractError::MissingField { krate, field } => {
            ErrorKind::MissingField { id: krate, field }
        }
        contract::ContractError::BadTier { line, value } => ErrorKind::Malformed {
            line,
            text: format!("tier = {value:?}"),
        },
        contract::ContractError::BadDate { krate, value } => {
            ErrorKind::BadDate { id: krate, value }
        }
        contract::ContractError::ThinReason { krate } => ErrorKind::Malformed {
            line: 0,
            text: format!("reason for {krate:?} is not an invariant field"),
        },
        contract::ContractError::DuplicateEntry { krate, line } => ErrorKind::Malformed {
            line,
            text: format!("duplicate dependency approval {krate:?}"),
        },
        contract::ContractError::BadPolicyValue { line, key, value } => {
            ErrorKind::BadPolicyValue { line, key, value }
        }
        contract::ContractError::DuplicatePolicyKey { line, key } => {
            ErrorKind::DuplicatePolicyKey { line, key }
        }
        contract::ContractError::DuplicatePolicySection { line } => {
            ErrorKind::DuplicatePolicySection { line }
        }
    }
}

// ── Resolution ──────────────────────────────────────────────────────────────

/// Whether a register id follows the workspace's `INV-<SCOPE>-<SLUG>` form.
fn valid_id(id: &str) -> bool {
    let mut parts = id.split('-');
    parts.next() == Some("INV")
        && parts.next().is_some_and(valid_id_word)
        && parts.next().is_some_and(valid_id_word)
        && parts.all(valid_id_word)
}

/// Whether one scope or slug component uses the uppercase identifier alphabet.
fn valid_id_word(word: &str) -> bool {
    !word.is_empty()
        && word
            .chars()
            .all(|character| character.is_ascii_uppercase() || character.is_ascii_digit())
}

/// Cargo treats hyphens and underscores as equivalent in package names.
fn normalise(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace('-', "_")
}

/// One lint a manifest declares, with the level it declares.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DeclaredLint {
    /// Namespace the lint belongs to, from the `[lints]` table name.
    namespace: LintNamespace,
    /// Lint name, without the namespace.
    name: String,
    /// Level the manifest declares, verbatim.
    level: String,
}

/// Whether a declared level can fail a build.
///
/// `allow` cannot, and neither can a level this gate does not recognise —
/// fail-closed reads an unknown level as unable to fail, which is the reading
/// that refuses rather than the one that passes.
fn is_active_level(level: &str) -> bool {
    matches!(level, "forbid" | "deny" | "warn")
}

/// Collects every lint declared by the root manifest and every member manifest.
///
/// Lints are collected from the whole workspace rather than from the scoped
/// package alone, because Cargo's `[lints] workspace = true` inheritance means
/// the root table applies to members, and a member's own table applies to
/// itself. A manifest that cannot be read contributes nothing: an unreadable
/// manifest therefore makes its lints *not* declared, which refuses rather than
/// passes.
fn declared_lints(root: &Path, members: &[crate::metadata::Member]) -> Vec<DeclaredLint> {
    let mut lints = Vec::new();
    let mut manifests = vec![root.join("Cargo.toml")];
    for member in members {
        manifests.push(member.manifest_dir.join("Cargo.toml"));
    }
    for manifest in manifests {
        if let Ok(text) = std::fs::read_to_string(&manifest) {
            collect_lints(&text, &mut lints);
        }
    }
    lints
}

/// Reads `[lints.<namespace>]` and `[workspace.lints.<namespace>]` tables.
fn collect_lints(text: &str, out: &mut Vec<DeclaredLint>) {
    let mut namespace = None;
    for line in text.lines() {
        let trimmed = strip_comment(line).trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(header) = trimmed
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            namespace = lint_table(header.trim());
            continue;
        }
        let Some(namespace) = namespace else { continue };
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        let name = key.trim();
        if !valid_lint_name(name) {
            continue;
        }
        out.push(DeclaredLint {
            namespace,
            name: name.to_owned(),
            level: lint_level(value.trim()),
        });
    }
}

/// Maps a table header onto the lint namespace it declares.
fn lint_table(header: &str) -> Option<LintNamespace> {
    let rest = header.strip_prefix("workspace.").unwrap_or(header);
    LintNamespace::parse_table(rest.strip_prefix("lints.")?)
}

/// Extracts a declared level from either the shorthand or the table form.
///
/// `unwrap_used = "forbid"` and `unwrap_used = { level = "forbid" }` are both
/// legal Cargo, and a reader that understood only the first would report a
/// declared lint as missing. An unrecognised shape yields the empty string,
/// which [`is_active_level`] reads as unable to fail.
fn lint_level(value: &str) -> String {
    let unquoted = value.trim().trim_matches('"');
    let Some(table) = unquoted.strip_prefix('{') else {
        return unquoted.to_owned();
    };
    let Some(after) = table.split_once("level") else {
        return String::new();
    };
    let Some((_, tail)) = after.1.split_once('=') else {
        return String::new();
    };
    let tail = tail.trim();
    let end = tail.find([',', '}']).unwrap_or(tail.len());
    tail[..end].trim().trim_matches('"').to_owned()
}

/// Returns `line` up to a `#` that is outside a quoted string.
fn strip_comment(line: &str) -> &str {
    let mut quoted = false;
    for (index, character) in line.char_indices() {
        match character {
            '"' => quoted = !quoted,
            '#' if !quoted => return &line[..index],
            _ => {}
        }
    }
    line
}

/// Resolves each registered invariant against the repository.
///
/// This is a reference-resolution pass and nothing more. It executes no lint
/// and no test; see [`SCOPE`] for the sentence every caller must report beside
/// a verdict.
#[must_use]
pub fn audit(register: &Register, root: &Path, members: &[crate::metadata::Member]) -> Audit {
    let lints = declared_lints(root, members);
    let mut refusals = Vec::new();
    let mut outcomes = Vec::with_capacity(register.entries.len());
    let mut seen_ids = BTreeSet::new();
    for entry in &register.entries {
        let before = refusals.len();
        if !valid_id(&entry.id) {
            refusals.push(Refusal::MalformedId {
                id: entry.id.clone(),
                line: entry.line,
            });
        }
        if !seen_ids.insert(entry.id.clone()) {
            refusals.push(Refusal::DuplicateId {
                id: entry.id.clone(),
                line: entry.line,
            });
        }
        check_scope(entry, members, &mut refusals);
        check_enforcer(entry, root, &lints, &mut refusals);
        let status = match refusals.len() {
            len if len > before => Status::Refused,
            _ if entry.evidence.is_some() => Status::Attested,
            _ => Status::Resolved,
        };
        outcomes.push(Outcome {
            id: entry.id.clone(),
            status,
            line: entry.line,
        });
    }
    refusals.sort_by_key(ToString::to_string);
    Audit { refusals, outcomes }
}

/// Refuses a scope that is not a workspace package, or names a module no `mod`
/// item in that package reaches.
fn check_scope(entry: &Entry, members: &[crate::metadata::Member], refusals: &mut Vec<Refusal>) {
    let mut segments = entry.scope.split("::");
    let crate_name = segments.next().unwrap_or(entry.scope.as_str());
    let Some(member) = members
        .iter()
        .find(|member| normalise(&member.name) == normalise(crate_name))
    else {
        refusals.push(Refusal::ScopeNotInWorkspace {
            id: entry.id.clone(),
            scope: entry.scope.clone(),
            line: entry.line,
        });
        return;
    };
    let modules: Vec<&str> = segments.filter(|segment| !segment.is_empty()).collect();
    if modules.is_empty() {
        return;
    }
    if resolve_module(&member.manifest_dir, &modules).is_none() {
        refusals.push(Refusal::ScopeModuleNotFound {
            id: entry.id.clone(),
            scope: entry.scope.clone(),
            module: modules.join("::"),
            line: entry.line,
        });
    }
}

/// Refuses an enforcement reference that does not resolve to a real mechanism.
fn check_enforcer(entry: &Entry, root: &Path, lints: &[DeclaredLint], refusals: &mut Vec<Refusal>) {
    match entry.enforced_by {
        None => refusals.push(Refusal::MissingEnforcedBy {
            id: entry.id.clone(),
            line: entry.line,
        }),
        Some(EnforcedBy::Lint {
            namespace,
            ref name,
        }) => {
            let selector = format!("{namespace}::{name}");
            let declared = lints
                .iter()
                .find(|lint| lint.namespace == namespace && lint.name == *name);
            match declared {
                None => refusals.push(Refusal::LintNotDeclared {
                    id: entry.id.clone(),
                    lint: selector,
                    line: entry.line,
                }),
                Some(lint) if !is_active_level(&lint.level) => {
                    refusals.push(Refusal::LintDisabled {
                        id: entry.id.clone(),
                        lint: selector,
                        level: lint.level.clone(),
                        line: entry.line,
                    });
                }
                Some(_) => {}
            }
        }
        Some(EnforcedBy::File(ref relative)) => {
            let path = root.join(relative.as_path());
            if !path.is_file() {
                refusals.push(Refusal::EnforcedByNotFound {
                    id: entry.id.clone(),
                    enforced_by: relative.to_string(),
                    path,
                    line: entry.line,
                });
            } else if entry.enforcement == Enforcement::StaticCheck {
                // A file is only a check if it declares a test item a runner
                // would select. A monitor is proved by its recorded run, which
                // `build_evidence` has already required.
                let declares = std::fs::read_to_string(&path)
                    .is_ok_and(|source| declares_active_test(&source));
                if !declares {
                    refusals.push(Refusal::EnforcerIsNotATest {
                        id: entry.id.clone(),
                        path,
                        line: entry.line,
                    });
                }
            }
        }
    }
}

/// Resolves `segments` to the source file backing them, or `None`.
///
/// The crate root must exist, segment zero must be a module of the crate root,
/// and each later segment a module of the one before it. Requiring the `mod`
/// declaration as well as the file is the point: a stray `.rs` file that
/// nothing reaches is not a module a scope can claim authority over.
fn resolve_module(member_dir: &Path, segments: &[&str]) -> Option<PathBuf> {
    let mut current = crate_root(member_dir)?;
    for (index, segment) in segments.iter().enumerate() {
        let directory = if index == 0 {
            member_dir.join("src")
        } else {
            module_directory(&current)
        };
        let candidate = module_file(&directory, segment)?;
        let declaring = std::fs::read_to_string(&current).ok()?;
        if !declares_module(&declaring, segment) {
            return None;
        }
        current = candidate;
    }
    Some(current)
}

/// The crate root source file, preferring the library over the binary.
fn crate_root(member_dir: &Path) -> Option<PathBuf> {
    ["src/lib.rs", "src/main.rs"]
        .iter()
        .map(|relative| member_dir.join(relative))
        .find(|path| path.is_file())
}

/// The directory a module's own children live in.
fn module_directory(source: &Path) -> PathBuf {
    let parent = source.parent().map_or_else(PathBuf::new, Path::to_path_buf);
    match source.file_stem() {
        Some(stem) if stem != OsStr::new("mod") => parent.join(stem),
        _ => parent,
    }
}

/// The file backing a child module, when one exists.
fn module_file(directory: &Path, segment: &str) -> Option<PathBuf> {
    let flat = directory.join(format!("{segment}.rs"));
    if flat.is_file() {
        return Some(flat);
    }
    let nested = directory.join(segment).join("mod.rs");
    if nested.is_file() {
        return Some(nested);
    }
    None
}

/// Whether `source` declares `name` as a module.
fn declares_module(source: &str, name: &str) -> bool {
    source.lines().any(|line| {
        let trimmed = strip_item_prefix(line.trim());
        let Some(rest) = trimmed.strip_prefix("mod") else {
            return false;
        };
        if !rest.starts_with(char::is_whitespace) {
            return false;
        }
        let Some(rest) = rest.trim_start().strip_prefix(name) else {
            return false;
        };
        let rest = rest.trim_start();
        rest.starts_with(';') || rest.starts_with('{')
    })
}

/// Strips the attributes and item qualifiers a declaration may carry.
///
/// `pub mod verb;` and `pub(crate) fn helper()` are ordinary Rust, and a scan
/// that required the bare keyword would report an enforcing module as absent.
/// `#[path = "x.rs"] mod ghost;` is the same defect one attribute further out,
/// so a leading attribute group is stepped over too. String and byte-string
/// literals are stepped over so a `pub` inside one does not confuse the walk.
fn strip_item_prefix(line: &str) -> &str {
    let mut rest = line.trim_start();
    loop {
        if let Some((_, after)) = split_attribute(rest) {
            rest = after.trim_start();
            continue;
        }
        if let Some(after) = rest.strip_prefix("pub") {
            let after = after.trim_start();
            if let Some(inside) = after.strip_prefix('(') {
                match inside.find(')') {
                    Some(close) => {
                        rest = inside[close.saturating_add(1)..].trim_start();
                        continue;
                    }
                    None => return rest,
                }
            }
            rest = after;
            continue;
        }
        if let Some((after, width)) = skip_keyword(rest, "extern") {
            rest = skip_literal(after).unwrap_or(after).trim_start();
            let _ = width;
            continue;
        }
        let mut advanced = false;
        for keyword in ["unsafe", "async", "const", "default", "move"] {
            if let Some(after) = rest.strip_prefix(keyword)
                && after.starts_with(char::is_whitespace)
            {
                rest = after.trim_start();
                advanced = true;
            }
        }
        if !advanced {
            return rest;
        }
    }
}

/// Splits `keyword` off `rest` when it is a whole word, with its byte length.
fn skip_keyword<'a>(rest: &'a str, keyword: &str) -> Option<(&'a str, usize)> {
    let after = rest.strip_prefix(keyword)?;
    if after.starts_with(char::is_whitespace) {
        Some((after.trim_start(), keyword.len()))
    } else {
        None
    }
}

/// Steps over a leading string or byte-string literal.
fn skip_literal(rest: &str) -> Option<&str> {
    let body = rest
        .strip_prefix('"')
        .or_else(|| rest.strip_prefix("r\""))?;
    let close = body.find('"')?;
    Some(&body[close.saturating_add(1)..])
}

/// Whether a file declares a test item a runner would actually select.
///
/// A `#[test]` under `#[ignore]` never runs, and an attribute group that
/// introduces an item other than a function — the `#[cfg(test)] mod tests`
/// wrapper itself — is not a test. A condition the gate cannot evaluate is
/// treated as unsatisfied: `#[cfg(feature = "never")]` above or beside the
/// `#[test]` may compile the function out of every configuration CI builds, so
/// the honest answer is that no run is known to select it. A file with no
/// selectable test item is not a check that can fail, which is what makes it
/// refuse rather than pass.
fn declares_active_test(source: &str) -> bool {
    let lines: Vec<&str> = source.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        let Some(marker) = attribute_group(line) else {
            continue;
        };
        if !is_test_marker(&marker) {
            continue;
        }
        let mut unselected = is_unselected_marker(&marker);
        // Conditions stacked above the marker decide selection just as the ones
        // below it do: `#[cfg(feature = "never")]` is conventionally written
        // immediately above the item it gates.
        let mut above = index;
        while let Some(previous) = above.checked_sub(1).and_then(|at| lines.get(at)) {
            let Some(group) = attribute_group(previous) else {
                break;
            };
            if is_unselected_marker(&group) {
                unselected = true;
            }
            above = above.saturating_sub(1);
        }
        let mut cursor = index.saturating_add(1);
        while let Some(next) = lines.get(cursor).and_then(|next| attribute_group(next)) {
            if is_ignore_marker(&next) || is_unselected_marker(&next) {
                unselected = true;
            }
            cursor = cursor.saturating_add(1);
        }
        let Some(item) = lines.get(cursor) else {
            continue;
        };
        if unselected {
            continue;
        }
        let item = strip_item_prefix(item.trim());
        if item.starts_with("fn") && item["fn".len()..].starts_with(char::is_whitespace) {
            return true;
        }
    }
    false
}

/// The text inside a leading `#[...]` attribute, brackets balanced.
fn attribute_group(line: &str) -> Option<String> {
    split_attribute(line).map(|(inner, _)| inner.trim().to_owned())
}

/// Splits a leading `#[...]` attribute group off `line`.
///
/// Returns the group's body and the text after the closing bracket, so a caller
/// that must step over an attribute and one that must read it share one
/// bracket-depth-aware scanner rather than two that can drift.
fn split_attribute(line: &str) -> Option<(&str, &str)> {
    let body = line.trim_start().strip_prefix("#[")?;
    let mut depth = 1usize;
    for (offset, character) in body.char_indices() {
        match character {
            '[' => depth = depth.saturating_add(1),
            ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let inner = body.get(..offset).unwrap_or("");
                    let rest = body.get(offset.saturating_add(1)..).unwrap_or("");
                    return Some((inner, rest));
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether an attribute group marks a test item.
fn is_test_marker(text: &str) -> bool {
    text == "test"
        || text.ends_with("::test")
        || (text.starts_with("cfg(") && text.contains("test"))
}

/// Whether an attribute group gates an item on a condition this gate cannot
/// evaluate.
///
/// Only the predicate `test` is known to hold — the test profile is what runs
/// the item — and it is accepted bare or as a single-argument `all`/`any`.
/// Everything else is read as unsatisfied, including predicates that merely
/// mention `test`: `cfg(not(test))` is the *opposite* of selected, and a
/// feature named `test` is a feature. Reading the whole predicate rather than a
/// token inside it is what makes those distinguishable. The price is a
/// conjunctive predicate such as `cfg(all(test, unix))`, refused although a
/// Unix test profile would select it; refusing is the direction this gate is
/// allowed to err in.
fn is_unselected_marker(text: &str) -> bool {
    let Some(predicate) = text
        .strip_prefix("cfg(")
        .and_then(|rest| rest.strip_suffix(')'))
    else {
        return false;
    };
    let compact: String = predicate
        .chars()
        .filter(|character: &char| !character.is_whitespace())
        .collect();
    !matches!(compact.as_str(), "test" | "all(test)" | "any(test)")
}

/// Whether an attribute group keeps a test item out of a default run.
fn is_ignore_marker(text: &str) -> bool {
    text == "ignore"
        || text.ends_with("::ignore")
        || text.starts_with("ignore(")
        || text.starts_with("ignore=")
}

// ── Entry point ─────────────────────────────────────────────────────────────

/// Audits the optional invariant register at `root`.
///
/// Absence is deliberately `Ok(None)`: adding this register must not change
/// the existing dependency gate for repositories that have not authored one.
pub fn check(root: &Path) -> Result<Option<(Register, Audit)>, InvariantError> {
    let path = root.join(INVARIANTS_PATH);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(cause) => {
            return Err(InvariantError::Unreadable { path, cause });
        }
    };
    let register = Register::parse(&text)?;
    let members = crate::metadata::workspace_members(root).map_err(InvariantError::Metadata)?;
    let audit = audit(&register, root, &members);
    Ok(Some((register, audit)))
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn Error>>;

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn members() -> Vec<crate::metadata::Member> {
        vec![
            crate::metadata::Member {
                name: "lgwks_bot".to_owned(),
                manifest_dir: workspace_root().join("crates/lgwks-bot"),
            },
            crate::metadata::Member {
                name: "lgwks_deps".to_owned(),
                manifest_dir: workspace_root().join("crates/lgwks-deps"),
            },
        ]
    }

    fn entry(id: &str, scope: &str, enforced_by: &str) -> String {
        format!(
            "[[invariant]]\n\
             id = \"{id}\"\n\
             statement = \"The implementation preserves this reviewed contract.\"\n\
             scope = \"{scope}\"\n\
             owner = \"{scope}\"\n\
             enforcement = \"static-check\"\n\
             enforced_by = \"{enforced_by}\"\n\
             approved_by = \"maintainer\"\n\
             approved_on = \"2026-09-20\"\n\
             review = \"crates/lgwks-deps/src/invariants.rs\"\n"
        )
    }

    fn audit_text(text: &str) -> Result<Audit, ErrorKind> {
        let register = Register::parse(text)?;
        Ok(audit(&register, &workspace_root(), &members()))
    }

    fn refusals(text: &str) -> Result<Vec<Refusal>, ErrorKind> {
        Ok(audit_text(text)?.refusals)
    }

    // ── Registration validity ───────────────────────────────────────────────

    #[test]
    fn an_enforcement_kind_outside_the_grammar_is_refused_at_load() {
        for kind in ["review", "intent", "static_check", "Monitor", ""] {
            let text = entry("INV-BOT-FOUR-VERBS", "lgwks_bot", "clippy::unwrap_used").replace(
                "enforcement = \"static-check\"",
                &format!("enforcement = \"{kind}\""),
            );
            assert!(
                matches!(
                    Register::parse(&text),
                    Err(ErrorKind::UnsupportedEnforcement { .. })
                ),
                "enforcement {kind:?} is outside the closed grammar"
            );
        }
    }

    #[test]
    fn an_absolute_or_parent_relative_enforced_by_is_refused_at_load() {
        for spelling in ["/etc/passwd", "../outside/check.rs", "a/../../b.rs"] {
            let text = entry("INV-BOT-FOUR-VERBS", "lgwks_bot", spelling);
            assert!(
                matches!(
                    Register::parse(&text),
                    Err(ErrorKind::EnforcerNotConfined { .. })
                ),
                "enforced_by {spelling:?} must not be readable as a reference"
            );
        }
    }

    #[test]
    fn an_empty_evidence_triple_is_admitted_but_a_broken_one_is_refused() -> TestResult {
        let complete = entry("INV-BOT-FOUR-VERBS", "lgwks_bot", "clippy::unwrap_used");
        assert!(
            Register::parse(&complete)?.entries[0].evidence.is_none(),
            "a static check without a recorded run is a valid entry"
        );
        let partial = format!("{complete}evidence_revision = \"deadbeef\"\n");
        assert!(matches!(
            Register::parse(&partial),
            Err(ErrorKind::EvidenceIncomplete {
                missing: "evidence_invocation",
                ..
            })
        ));
        Ok(())
    }

    #[test]
    fn evidence_must_name_a_revision_the_enforcer_and_a_pass() -> TestResult {
        let base = entry("INV-BOT-FOUR-VERBS", "lgwks_bot", "clippy::unwrap_used");
        let with = |revision: &str, invocation: &str, result: &str| {
            format!(
                "{base}evidence_revision = \"{revision}\"\n\
                 evidence_invocation = \"{invocation}\"\n\
                 evidence_result = \"{result}\"\n"
            )
        };
        assert!(matches!(
            Register::parse(&with("abc", "cargo clippy -- clippy::unwrap_used", "pass")),
            Err(ErrorKind::EvidenceRevisionShape { .. })
        ));
        assert!(matches!(
            Register::parse(&with("deadbeef", "cargo clippy", "pass")),
            Err(ErrorKind::EvidenceDoesNotNameEnforcer { .. })
        ));
        assert!(matches!(
            Register::parse(&with(
                "deadbeef",
                "cargo clippy -- clippy::unwrap_used",
                "warn"
            )),
            Err(ErrorKind::EvidenceNotPassing { .. })
        ));
        let register = Register::parse(&with(
            "deadbeef",
            "cargo clippy -- clippy::unwrap_used",
            "pass",
        ))?;
        let evidence = register.entries[0]
            .evidence
            .as_ref()
            .ok_or("the complete triple must be recorded")?;
        assert_eq!(evidence.revision(), "deadbeef");
        assert_eq!(evidence.invocation(), "cargo clippy -- clippy::unwrap_used");
        Ok(())
    }

    #[test]
    fn a_monitor_without_a_recorded_run_is_refused_at_load() {
        let text = entry("INV-BOT-FOUR-VERBS", "lgwks_bot", "clippy::unwrap_used").replace(
            "enforcement = \"static-check\"",
            "enforcement = \"monitor\"",
        );
        assert!(matches!(
            Register::parse(&text),
            Err(ErrorKind::MonitorNeedsEvidence { .. })
        ));
    }

    // ── Reference resolution: negative controls ─────────────────────────────

    /// Control 1: a lint name that cannot exist. The previous audit accepted
    /// any `rustc::name` by shape alone, which is the issue's counterexample.
    #[test]
    fn a_lint_the_repository_does_not_declare_is_refused() -> TestResult {
        let refusals = refusals(&entry(
            "INV-BOT-FOUR-VERBS",
            "lgwks_bot",
            "rustc::this_lint_does_not_exist",
        ))?;
        assert!(
            matches!(refusals.as_slice(), [Refusal::LintNotDeclared { .. }]),
            "an undeclared lint cannot fire, so it is not enforcement; got {refusals:?}"
        );
        Ok(())
    }

    /// Control 2: a lint that exists but is declared `allow`.
    #[test]
    fn a_lint_declared_at_allow_is_refused() -> TestResult {
        // Under `target/`, which is build output rather than source, and
        // rewritten on every run so a leftover fixture cannot change the
        // verdict the way a stale one would.
        let root = workspace_root().join("target/lgwks-deps-lint-fixture");
        std::fs::create_dir_all(&root)?;
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = []\n\n[workspace.lints.clippy]\nunwrap_used = \"allow\"\n",
        )?;
        let register = Register::parse(&entry(
            "INV-BOT-FOUR-VERBS",
            "lgwks_bot",
            "clippy::unwrap_used",
        ))?;
        let audit = audit(&register, &root, &members());
        assert!(
            matches!(
                audit.refusals(),
                [Refusal::LintDisabled { level, .. }] if level == "allow"
            ),
            "an allowed lint cannot fail a build; got {:?}",
            audit.refusals()
        );
        Ok(())
    }

    /// Control 3: a scope naming a module no `mod` item reaches.
    #[test]
    fn a_scope_naming_a_module_that_does_not_exist_is_refused() -> TestResult {
        let refusals = refusals(&entry(
            "INV-BOT-NEVER-FAILS",
            "lgwks_bot::this_module_does_not_exist",
            "clippy::unwrap_used",
        ))?;
        assert!(
            matches!(refusals.as_slice(), [Refusal::ScopeModuleNotFound { .. }]),
            "the package prefix existing is not the module existing; got {refusals:?}"
        );
        Ok(())
    }

    /// Control 4: a real file that declares no test — the issue's `README.md`
    /// case, with the file chosen from this repository rather than invented.
    #[test]
    fn a_file_declaring_no_test_is_refused() -> TestResult {
        for spelling in ["README.md", "Cargo.toml", "clippy.toml"] {
            let refusals = refusals(&entry("INV-BOT-FOUR-VERBS", "lgwks_bot", spelling))?;
            assert!(
                matches!(refusals.as_slice(), [Refusal::EnforcerIsNotATest { .. }]),
                "{spelling} exists but nothing in it can fail; got {refusals:?}"
            );
        }
        Ok(())
    }

    /// Control 5: a file that declares tests, but only ones a run would skip.
    #[test]
    fn an_ignored_or_unselected_test_is_not_enforcement() -> TestResult {
        assert!(!declares_active_test(
            "#[cfg(test)]\nmod tests {\n    #[test]\n    #[ignore]\n    fn skipped() {}\n}\n"
        ));
        assert!(!declares_active_test(
            "#[test]\n#[cfg(feature = \"never\")]\nfn unselected() {}\n"
        ));
        // The predicate that only *mentions* `test`, and the one that negates
        // it: neither is the test profile, and the second is its opposite.
        assert!(!declares_active_test(
            "#[test]\n#[cfg(not(test))]\nfn inverted() {}\n"
        ));
        assert!(!declares_active_test(
            "#[cfg(feature = \"test\")]\n#[test]\nfn feature_named_test() {}\n"
        ));
        assert!(!declares_active_test(
            "#[test]\n#[cfg(all(test, unix))]\nfn conjunctive() {}\n"
        ));
        assert!(declares_active_test(
            "#[cfg(test)]\n#[test]\nfn explicit_profile() {}\n"
        ));
        assert!(!declares_active_test("pub fn helper() {}\n"));
        assert!(!declares_active_test("#[cfg(test)]\nmod tests {\n}\n"));
        assert!(declares_active_test("#[test]\nfn runs() {}\n"));
        assert!(declares_active_test(
            "#[cfg(test)]\nmod tests {\n    #[test]\n    #[should_panic]\n    fn runs() {}\n}\n"
        ));
        assert!(declares_active_test("#[tokio::test]\nasync fn runs() {}\n"));
        Ok(())
    }

    /// Control 6: an escape to an existing file outside the repository. The
    /// path is rejected at load, so it can never reach the filesystem at all.
    #[test]
    fn an_external_path_is_refused_before_the_filesystem_is_consulted() {
        let text = entry("INV-BOT-FOUR-VERBS", "lgwks_bot", "/etc/hosts");
        assert!(matches!(
            Register::parse(&text),
            Err(ErrorKind::EnforcerNotConfined {
                reason: PathRefusal::Absolute,
                ..
            })
        ));
    }

    /// Control 7: a real, passing, unrelated test is not evidence about this
    /// statement — the recorded invocation must name the entry's own enforcer.
    #[test]
    fn a_passing_run_of_something_else_is_not_evidence() {
        let text = format!(
            "{}evidence_revision = \"deadbeef\"\n\
             evidence_invocation = \"cargo test -p lgwks_deps --lib tests::a_valid_register_passes\"\n\
             evidence_result = \"pass\"\n",
            entry(
                "INV-BOT-FOUR-VERBS",
                "lgwks_bot",
                "crates/lgwks-bot/src/verb.rs"
            )
        );
        assert!(matches!(
            Register::parse(&text),
            Err(ErrorKind::EvidenceDoesNotNameEnforcer { .. })
        ));
    }

    // ── Reference resolution: positive controls ─────────────────────────────

    #[test]
    fn a_declared_active_lint_resolves() -> TestResult {
        let refusals = refusals(&entry(
            "INV-DEP-EDGE-OWNED",
            "lgwks_deps",
            "clippy::unwrap_used",
        ))?;
        assert!(
            refusals.is_empty(),
            "clippy::unwrap_used is declared at forbid in this workspace; got {refusals:?}"
        );
        Ok(())
    }

    #[test]
    fn a_declared_lint_namespace_is_read_from_the_matching_table() -> TestResult {
        let register = Register::parse(&entry(
            "INV-BOT-FOUR-VERBS",
            "lgwks_bot",
            "rustc::missing_docs",
        ))?;
        let audit = audit(&register, &workspace_root(), &members());
        assert!(
            audit.refusals().is_empty(),
            "rustc::missing_docs lives in [workspace.lints.rust] at deny; got {:?}",
            audit.refusals()
        );
        Ok(())
    }

    #[test]
    fn a_resolved_entry_without_a_run_is_resolved_not_attested() -> TestResult {
        let audit = audit_text(&entry(
            "INV-BOT-FOUR-VERBS",
            "lgwks_bot",
            "crates/lgwks-deps/src/invariants.rs",
        ))?;
        assert_eq!(audit.resolved(), 1, "a resolved reference is not a run");
        assert_eq!(audit.attested(), 0);
        assert_eq!(audit.refused(), 0);
        assert_eq!(audit.outcomes()[0].status, Status::Resolved);
        Ok(())
    }

    #[test]
    fn a_resolved_entry_with_a_recorded_run_is_attested() -> TestResult {
        let text = format!(
            "{}evidence_revision = \"5e1b437\"\n\
             evidence_invocation = \"cargo test --all-targets -- crates/lgwks-deps/src/invariants.rs\"\n\
             evidence_result = \"pass\"\n",
            entry(
                "INV-BOT-FOUR-VERBS",
                "lgwks_bot",
                "crates/lgwks-deps/src/invariants.rs"
            )
        );
        let audit = audit_text(&text)?;
        assert_eq!(audit.attested(), 1);
        assert_eq!(audit.resolved(), 0);
        assert_eq!(audit.outcomes()[0].status, Status::Attested);
        Ok(())
    }

    /// A module scope with a real `mod` behind it resolves.
    #[test]
    fn a_scope_naming_a_real_module_resolves() -> TestResult {
        let refusals = refusals(&entry(
            "INV-BOT-FOUR-VERBS",
            "lgwks_bot::verb",
            "crates/lgwks-deps/src/invariants.rs",
        ))?;
        assert!(
            refusals.is_empty(),
            "lib.rs declares `pub mod verb;`; got {refusals:?}"
        );
        Ok(())
    }

    #[test]
    fn a_stray_file_no_mod_reaches_is_not_a_module() -> TestResult {
        assert!(!declares_module("// mod ghost;\n", "ghost"));
        assert!(!declares_module("pub fn ghost() {}\n", "ghost"));
        assert!(declares_module("pub mod ghost;\n", "ghost"));
        assert!(declares_module("pub(crate) mod ghost {}\n", "ghost"));
        assert!(declares_module("#[path = \"x.rs\"] mod ghost;\n", "ghost"));
        Ok(())
    }

    // ── The pre-existing rules ──────────────────────────────────────────────

    #[test]
    fn an_invariant_without_enforcement_is_refused() -> TestResult {
        let text = concat!(
            "[[invariant]]\n",
            "id = \"INV-BOT-FOUR-VERBS\"\n",
            "statement = \"The implementation preserves this reviewed contract.\"\n",
            "scope = \"lgwks_bot\"\n",
            "owner = \"lgwks_bot\"\n",
            "enforcement = \"static-check\"\n",
            "approved_by = \"maintainer\"\n",
            "approved_on = \"2026-09-20\"\n",
            "review = \"crates/lgwks-deps/src/invariants.rs\"\n",
        );
        assert!(matches!(
            audit_text(text)?.refusals(),
            [Refusal::MissingEnforcedBy { .. }]
        ));
        Ok(())
    }

    #[test]
    fn an_enforcement_path_that_does_not_exist_is_refused() -> TestResult {
        let refusals = refusals(&entry(
            "INV-BOT-FOUR-VERBS",
            "lgwks_bot",
            "crates/lgwks-bot/src/does-not-exist.rs",
        ))?;
        assert!(matches!(
            refusals.as_slice(),
            [Refusal::EnforcedByNotFound { .. }]
        ));
        Ok(())
    }

    #[test]
    fn an_invariant_scoped_to_an_unknown_crate_is_refused() -> TestResult {
        let refusals = refusals(&entry(
            "INV-BOGUS-NOT-IN-WORKSPACE",
            "not_a_workspace_crate",
            "clippy::missing_docs",
        ))?;
        assert!(
            refusals
                .iter()
                .any(|refusal| matches!(refusal, Refusal::ScopeNotInWorkspace { .. })),
            "an unknown package must be refused; got {refusals:?}"
        );
        Ok(())
    }

    #[test]
    fn duplicate_ids_are_refused_at_the_second_entry() -> TestResult {
        let text = format!(
            "{}\n{}",
            entry(
                "INV-BOT-FOUR-VERBS",
                "lgwks_bot",
                "crates/lgwks-deps/src/invariants.rs"
            ),
            entry(
                "INV-BOT-FOUR-VERBS",
                "lgwks_bot",
                "crates/lgwks-deps/src/invariants.rs"
            )
        );
        let refusals = refusals(&text)?;
        assert!(matches!(refusals.as_slice(), [Refusal::DuplicateId { .. }]));
        Ok(())
    }

    #[test]
    fn malformed_ids_are_refused() -> TestResult {
        let refusals = refusals(&entry(
            "bad-invariant-id",
            "lgwks_bot",
            "crates/lgwks-deps/src/invariants.rs",
        ))?;
        assert!(matches!(refusals.as_slice(), [Refusal::MalformedId { .. }]));
        Ok(())
    }

    /// The rule the issue's own `a_valid_register_passes` test demonstrated was
    /// missing: the four-verbs statement was bound to this file, which has no
    /// relationship to the bot's verb traits. A valid *reference* is no longer
    /// a pass; it is a distinct `Resolved` status, and only a recorded run
    /// makes it `Attested`.
    #[test]
    fn a_valid_register_resolves_and_is_never_called_enforced() -> TestResult {
        let register = Register::parse(&entry(
            "INV-BOT-FOUR-VERBS",
            "lgwks_bot",
            "crates/lgwks-deps/src/invariants.rs",
        ))?;
        let audit = audit(&register, &workspace_root(), &members());
        assert!(audit.refusals().is_empty(), "the entry is well formed");
        assert_eq!(audit.attested(), 0, "nothing was executed for this entry");
        assert_eq!(audit.outcomes()[0].status.as_str(), "resolved");
        Ok(())
    }

    #[test]
    fn missing_register_is_not_a_failure() -> TestResult {
        let root = workspace_root().join("target/lgwks-deps-no-invariant-register");
        assert!(check(&root)?.is_none());
        Ok(())
    }
}
