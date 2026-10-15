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
//! The module is split by responsibility: this file owns the schema and the
//! load-time checks, `verdict` the verdict ladder and the error types, and
//! `resolve` the repository-aware resolution and the entry point. Their public
//! items are re-exported here, so every path is `crate::invariants::…`.
//!
//! [`Register::parse`]: crate::invariants::Register::parse
//! [`audit`]: crate::invariants::audit
//! [`Evidence`]: crate::invariants::Evidence
//! [`Status::Attested`]: crate::invariants::Status::Attested
//! [`Status::Resolved`]: crate::invariants::Status::Resolved
//! [`SCOPE`]: crate::invariants::SCOPE

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

mod binding;
mod resolve;
#[cfg(test)]
mod tests;
mod verdict;

pub use binding::EvidenceGap;
pub use resolve::{audit, check};
pub use verdict::{Audit, ErrorKind, InvariantError, Outcome, Refusal, SCOPE, Status};

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
            let refusal = Err(PathRefusal::Absolute);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "parse: returning an error to the caller");
            return refusal;
        }
        for component in path.components() {
            match component {
                Component::ParentDir => {
                    let refusal = Err(PathRefusal::ParentDirectory);
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "parse: returning an error to the caller");
                    return refusal;
                }
                Component::RootDir | Component::Prefix(_) => {
                    let refusal = Err(PathRefusal::Absolute);
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "parse: returning an error to the caller");
                    return refusal;
                }
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
/// `pass`, or an invocation that does not name the entry's own enforcer. That
/// fixes the *selector*. It does not fix the *revision*, which at parse time is
/// only a well-shaped hex string: [`audit`] binds it, by asking Git whether it is
/// a commit in the history of `HEAD` and whether the enforcer is unchanged since,
/// and refuses [`Status::Attested`] with [`Refusal::EvidenceNotBound`] when not.
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
    pub(crate) id: String,
    /// Human-readable claim the repository is meant to preserve.
    pub(crate) statement: String,
    /// Workspace crate or Rust module path bound by the claim.
    pub(crate) scope: String,
    /// Workspace crate responsible for preserving the claim.
    pub(crate) owner: String,
    /// Enforcement kind, parsed from the closed two-word grammar.
    pub(crate) enforcement: Enforcement,
    /// The mechanism named as enforcement, or `None` when the entry names one
    /// and it is absent. Resolution against the repository happens in
    /// [`audit`], not here.
    pub(crate) enforced_by: Option<EnforcedBy>,
    /// Recorded run attesting the claim, when the entry carries one.
    pub(crate) evidence: Option<Evidence>,
    /// Human who approved the claim.
    pub(crate) approved_by: String,
    /// ISO date of the review.
    pub(crate) approved_on: String,
    /// Path or URL to the review evidence.
    pub(crate) review: String,
    /// One-based line where the entry opened.
    pub(crate) line: usize,
}

/// The parsed invariant register.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Register {
    /// Whether invariant refusals are enforcement failures.
    pub enforce: bool,
    /// Invariants in source order.
    pub(crate) entries: Vec<Entry>,
}

impl Register {
    /// Number of invariant declarations in this register.
    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }
}

// ── Loading ─────────────────────────────────────────────────────────────────

impl Register {
    /// Parses an invariant register through the dependency register's shared
    /// fail-closed line reader.
    pub fn parse(text: &str) -> Result<Self, ErrorKind> {
        let raw = contract::parse_register(text, "[[invariant]]", &ENTRY_FIELDS)
            .map_err(map_contract_error)?;
        // The repository-policy keys are the dependency register's; the shared
        // line reader collects them, and this register has no meaning for them.
        if let Some(field) = raw.policy_fields.first() {
            let refusal = Err(ErrorKind::UnknownKey {
                line: field.line,
                key: field.key.to_owned(),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "parse: returning an error to the caller");
            return refusal;
        }
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

/// The text `raw` wrote for `key`, or the empty string for a key it did not
/// write. Every key read through here is one the required-field loop in
/// [`build`] has already proved present, or one whose absence is itself the
/// answer (an optional field), so the empty string is never a guessed value.
fn written(raw: &RawEntry, key: &str) -> String {
    match raw.get(key) {
        Some(value) => value.to_owned(),
        None => String::new(),
    }
}

/// Builds an invariant entry, refusing anything the schema cannot represent.
///
/// Order matters for diagnostics: the closed grammars are parsed before the
/// evidence that depends on them, so an entry that writes both a bad
/// enforcement kind and an unrelated evidence block is told about the
/// enforcement kind.
fn build(raw: &RawEntry) -> Result<Entry, ErrorKind> {
    // A block that names no invariant is reported at its own line. The name is
    // kept as an `Option` rather than a placeholder so the refusal can say which
    // field is missing and where the block is, instead of naming an identifier
    // no register wrote.
    let id = raw.get("id").map(str::to_owned);
    let id_for_error = id.clone();
    for field in REQUIRED_FIELDS {
        match raw.get(field) {
            None => {
                let refusal = Err(ErrorKind::MissingField {
                    id: id_for_error,
                    field,
                    line: raw.line(),
                });
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build: returning an error to the caller");
                return refusal;
            }
            // A blank value is a missing value for every free-text field, and
            // for `enforcement` it is a value outside the closed grammar — the
            // repair is different, so the diagnostic has to be.
            Some(value) if value.trim().is_empty() && field != ENFORCEMENT_FIELD => {
                let refusal = Err(ErrorKind::MissingField {
                    id: id_for_error,
                    field,
                    line: raw.line(),
                });
                // Each refusal moves the borrow; the next iteration takes it
                // again from the name the block wrote.
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build: returning an error to the caller");
                return refusal;
            }
            Some(_) => {}
        }
    }
    // The loop above proved `id` is present and non-blank, so every refusal from
    // here on can name the invariant. The arm below is the reordering guard: a
    // block that reached it without an `id` could not have passed the loop, and
    // reporting that is the direction a parser bug must surface in rather than
    // the direction it must be assumed away in.
    let Some(id) = id else {
        let refusal = Err(ErrorKind::MissingField {
            id: None,
            field: "id",
            line: raw.line(),
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), line = raw.line(), "build: a block passed the required-field loop without an id");
        return refusal;
    };
    let approved_on = written(raw, "approved_on");
    if !contract::is_iso_date(&approved_on) {
        // The required-field loop proved `approved_on` is present, so this is
        // its own line; the block's line is where the repair is if the loop is
        // ever reordered to let a blank value through.
        let line = match raw.field_line("approved_on") {
            Some(line) => line,
            None => raw.line(),
        };
        let refusal = Err(ErrorKind::BadDate {
            id,
            line,
            value: approved_on,
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build: returning an error to the caller");
        return refusal;
    }
    let statement = written(raw, "statement");
    if let Some(reason) = non_monitorable_reason(&statement) {
        let refusal = Err(ErrorKind::NonMonitorable { id, reason });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build: returning an error to the caller");
        return refusal;
    }
    let authored_kind = written(raw, "enforcement");
    let Some(enforcement) = Enforcement::parse(&authored_kind) else {
        let refusal = Err(ErrorKind::UnsupportedEnforcement {
            id,
            enforcement: authored_kind,
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build: returning an error to the caller");
        return refusal;
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
        owner: written(raw, "owner"),
        scope: written(raw, "scope"),
        enforcement,
        enforced_by,
        evidence,
        approved_by: written(raw, "approved_by"),
        approved_on,
        review: written(raw, "review"),
        line: raw.line(),
    })
}

/// Validates and assembles the optional recorded run.
///
/// The triple is all-or-nothing: a revision with no invocation is a note, not a
/// run, and admitting it would put a revision beside a verdict no execution
/// produced. `enforcer` is the entry's own resolved reference, and a non-empty
/// invocation must name its spelling as a whole token — a passing run of some
/// *other* check is exactly the false comfort this rule exists to refuse.
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
            let refusal = Err(ErrorKind::MonitorNeedsEvidence { id: id.to_owned() });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build_evidence: returning an error to the caller");
            return refusal;
        }
        return Ok(None);
    }
    for field in EVIDENCE_FIELDS {
        if !present.contains(&field) {
            let refusal = Err(ErrorKind::EvidenceIncomplete {
                id: id.to_owned(),
                missing: field,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build_evidence: returning an error to the caller");
            return refusal;
        }
    }
    // The loop above proved all three fields present and non-blank.
    let field = |name: &str| written(raw, name).trim().to_owned();
    let revision = field("evidence_revision");
    if !valid_revision(&revision) {
        let refusal = Err(ErrorKind::EvidenceRevisionShape {
            id: id.to_owned(),
            revision,
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build_evidence: returning an error to the caller");
        return refusal;
    }
    let result = field("evidence_result");
    if result != PASSING_RESULT {
        let refusal = Err(ErrorKind::EvidenceNotPassing {
            id: id.to_owned(),
            result,
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build_evidence: returning an error to the caller");
        return refusal;
    }
    let invocation = field("evidence_invocation");
    if let Some(selector) = enforcer.map(EnforcedBy::selector)
        && !names_selector(&invocation, &selector)
    {
        let refusal = Err(ErrorKind::EvidenceDoesNotNameEnforcer {
            id: id.to_owned(),
            invocation,
            selector,
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build_evidence: returning an error to the caller");
        return refusal;
    }
    Ok(Some(Evidence {
        revision,
        invocation,
    }))
}

/// Whether `invocation` names `selector` as a whole token.
///
/// Containing the selector is not naming it: Clippy has lints that are strict
/// prefixes of others (`panic` / `panic_in_result_fn`), and a path is a suffix
/// of a longer path (`vendor/tests/a.rs`) or a prefix of a sibling
/// (`tests/a.rs.orig`). A run of any of those is a run of something else, so an
/// occurrence counts only when neither neighbour could continue a lint name or
/// a path (`sim_invariant_register`, the prefix scenario).
fn names_selector(invocation: &str, selector: &str) -> bool {
    !selector.is_empty()
        && invocation.match_indices(selector).any(|(at, _)| {
            let before = invocation
                .get(..at)
                .and_then(|head| head.chars().next_back());
            let after = invocation
                .get(at.saturating_add(selector.len())..)
                .and_then(|tail| tail.chars().next());
            !before.is_some_and(continues_selector) && !after.is_some_and(continues_selector)
        })
}

/// Whether `character` could extend a lint selector or a repository path.
fn continues_selector(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '_' | '-' | '.' | '/' | '\\' | ':')
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
        // A register block that named no crate keeps its own line: the
        // invariant register reports the dependency register's refusal, and a
        // nameless block is identified by where it is rather than by a name it
        // never wrote.
        contract::ContractError::MissingField { krate, field, line } => ErrorKind::Malformed {
            line,
            text: match krate {
                Some(krate) => format!("{krate:?} is missing required field {field:?}"),
                None => format!("approval block is missing required field {field:?}"),
            },
        },
        contract::ContractError::BadTier { line, value } => ErrorKind::Malformed {
            line,
            text: format!("tier = {value:?}"),
        },
        contract::ContractError::BadDate { krate, line, value } => ErrorKind::BadDate {
            id: krate,
            line,
            value,
        },
        contract::ContractError::ThinReason { krate } => ErrorKind::Malformed {
            line: 0,
            text: format!("reason for {krate:?} is not an invariant field"),
        },
        contract::ContractError::DuplicateEntry { krate, line } => ErrorKind::Malformed {
            line,
            text: format!("duplicate dependency approval {krate:?}"),
        },
        error @ contract::ContractError::InvalidString { line, .. } => ErrorKind::Malformed {
            line,
            text: error.to_string(),
        },
        error @ contract::ContractError::DuplicateEntryKey {
            duplicate_line: line,
            ..
        } => ErrorKind::Malformed {
            line,
            text: error.to_string(),
        },
        error @ contract::ContractError::InvalidField { line, .. } => ErrorKind::Malformed {
            line,
            text: error.to_string(),
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
        // The invariant schema has neither a `schema` policy key nor aliases, so
        // these cannot arise from an invariant register; they are mapped rather
        // than panicked so a future shared-parser change stays fail-closed.
        error @ (contract::ContractError::UnsupportedSchema { line, .. }
        | contract::ContractError::AliasCollision { line, .. }) => ErrorKind::Malformed {
            line,
            text: error.to_string(),
        },
        // Only `Contract::parse` decodes the repository-policy keys, and this
        // register refuses them before that point; mapped for the same reason.
        error @ contract::ContractError::IncompletePolicy { .. } => ErrorKind::Malformed {
            line: 0,
            text: error.to_string(),
        },
    }
}
