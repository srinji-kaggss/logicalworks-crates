//! Authored invariant register and its refusal rules.
//!
//! The register deliberately goes through `crate::contract::parse_register`
//! rather than acquiring a TOML dependency or copying the dependency reader.
//! Parsing is shared; this module owns only the invariant schema and the
//! repository-aware checks that give each claim a real enforcement boundary.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::contract::{self, RawEntry};

/// Register location relative to the repository root.
pub const INVARIANTS_PATH: &str = "contract/INVARIANTS.toml";

/// Keys accepted by an `[[invariant]]` block.
const ENTRY_FIELDS: [&str; 9] = [
    "id",
    "statement",
    "scope",
    "owner",
    "enforcement",
    "enforced_by",
    "approved_by",
    "approved_on",
    "review",
];

/// The fields required for an invariant entry.
///
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
    /// Enforcement kind: `static-check` or `monitor`.
    pub enforcement: String,
    /// Test path or lint name that enforces the claim.
    pub enforced_by: String,
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
    /// The invariant names no workspace package or module rooted in one.
    ScopeNotInWorkspace {
        /// Invariant identifier.
        id: String,
        /// Authored scope.
        scope: String,
        /// One-based line where the invariant opened.
        line: usize,
    },
    /// The entry names an enforcement kind outside the two executable kinds.
    UnsupportedEnforcement {
        /// Invariant identifier.
        id: String,
        /// Authored enforcement kind.
        enforcement: String,
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
            | Self::ScopeNotInWorkspace { ref id, .. }
            | Self::UnsupportedEnforcement { ref id, .. }
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
            Self::ScopeNotInWorkspace {
                ref id,
                ref scope,
                line,
            } => write!(
                formatter,
                "line {line}: invariant {id:?} scope {scope:?} is not a workspace package or module"
            ),
            Self::UnsupportedEnforcement {
                ref id,
                ref enforcement,
                line,
            } => write!(
                formatter,
                "line {line}: invariant {id:?} uses enforcement {enforcement:?}; want static-check or monitor"
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

/// Builds an invariant entry, leaving `enforced_by` present-but-empty for the
/// semantic refusal pass. That distinction gives the missing-enforcement rule
/// its own diagnostic instead of hiding it as a generic parse failure.
fn build(raw: &RawEntry) -> Result<Entry, ErrorKind> {
    let id = raw
        .get("id")
        .map_or_else(|| "<unnamed>".to_owned(), str::to_owned);
    for field in REQUIRED_FIELDS {
        if raw.get(field).is_none_or(|value| value.trim().is_empty()) {
            return Err(ErrorKind::MissingField { id, field });
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
    let scope = raw.get("scope").map_or_else(String::new, str::to_owned);
    Ok(Entry {
        id,
        statement,
        owner: raw.get("owner").map_or_else(String::new, str::to_owned),
        scope,
        enforcement: raw
            .get("enforcement")
            .map_or_else(String::new, str::to_owned),
        enforced_by: raw
            .get("enforced_by")
            .map_or_else(String::new, str::to_owned),
        approved_by: raw
            .get("approved_by")
            .map_or_else(String::new, str::to_owned),
        approved_on,
        review: raw.get("review").map_or_else(String::new, str::to_owned),
        line: raw.line(),
    })
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

/// A Rust lint name is a namespaced identifier, not a repository path.
fn is_lint_name(value: &str) -> bool {
    let Some((namespace, name)) = value.split_once("::") else {
        return false;
    };
    matches!(namespace, "clippy" | "rustc" | "rustdoc")
        && !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

/// Returns every refusal in deterministic display order.
pub fn audit(register: &Register, root: &Path, workspace_packages: &[String]) -> Vec<Refusal> {
    let mut refusals = Vec::new();
    let mut seen_ids = BTreeSet::new();
    for entry in &register.entries {
        if !matches!(entry.enforcement.as_str(), "static-check" | "monitor") {
            refusals.push(Refusal::UnsupportedEnforcement {
                id: entry.id.clone(),
                enforcement: entry.enforcement.clone(),
                line: entry.line,
            });
        }
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
        if entry.enforced_by.trim().is_empty() {
            refusals.push(Refusal::MissingEnforcedBy {
                id: entry.id.clone(),
                line: entry.line,
            });
        } else if !is_lint_name(&entry.enforced_by) && !root.join(&entry.enforced_by).is_file() {
            refusals.push(Refusal::EnforcedByNotFound {
                id: entry.id.clone(),
                enforced_by: entry.enforced_by.clone(),
                path: root.join(&entry.enforced_by),
                line: entry.line,
            });
        }
        let scope_crate = entry
            .scope
            .split("::")
            .next()
            .unwrap_or(entry.scope.as_str());
        if !workspace_packages
            .iter()
            .any(|package| normalise(package) == normalise(scope_crate))
        {
            refusals.push(Refusal::ScopeNotInWorkspace {
                id: entry.id.clone(),
                scope: entry.scope.clone(),
                line: entry.line,
            });
        }
    }
    refusals.sort_by_key(ToString::to_string);
    refusals
}

/// Audits the optional invariant register at `root`.
///
/// Absence is deliberately `Ok(None)`: adding this register must not change
/// the existing dependency gate for repositories that have not authored one.
pub fn check(root: &Path) -> Result<Option<(Register, Vec<Refusal>)>, InvariantError> {
    let path = root.join(INVARIANTS_PATH);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(cause) => {
            return Err(InvariantError::Unreadable { path, cause });
        }
    };
    let register = Register::parse(&text)?;
    let workspace_packages =
        crate::metadata::workspace_package_names(root).map_err(InvariantError::Metadata)?;
    let refusals = audit(&register, root, &workspace_packages);
    Ok(Some((register, refusals)))
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn Error>>;

    fn workspace_root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn workspace_packages() -> Vec<String> {
        vec!["lgwks_bot".to_owned(), "lgwks_deps".to_owned()]
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

    fn audit_fixture(text: &str) -> Result<Vec<Refusal>, ErrorKind> {
        let register = Register::parse(text)?;
        Ok(audit(&register, &workspace_root(), &workspace_packages()))
    }

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
            audit_fixture(text)?.as_slice(),
            [Refusal::MissingEnforcedBy { .. }]
        ));
        Ok(())
    }

    #[test]
    fn an_enforcement_path_that_does_not_exist_is_refused() -> TestResult {
        let refusals = audit_fixture(&entry(
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
        let refusals = audit_fixture(&entry(
            "INV-BOGUS-NOT-IN-WORKSPACE",
            "not_a_workspace_crate",
            "clippy::missing_docs",
        ))?;
        assert!(matches!(
            refusals.as_slice(),
            [Refusal::ScopeNotInWorkspace { .. }]
        ));
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
        let refusals = audit_fixture(&text)?;
        assert!(matches!(refusals.as_slice(), [Refusal::DuplicateId { .. }]));
        Ok(())
    }

    #[test]
    fn malformed_ids_are_refused() -> TestResult {
        let refusals = audit_fixture(&entry(
            "bad-invariant-id",
            "lgwks_bot",
            "crates/lgwks-deps/src/invariants.rs",
        ))?;
        assert!(matches!(refusals.as_slice(), [Refusal::MalformedId { .. }]));
        Ok(())
    }

    #[test]
    fn a_valid_register_passes() -> TestResult {
        let register = Register::parse(&entry(
            "INV-BOT-FOUR-VERBS",
            "lgwks_bot",
            "crates/lgwks-deps/src/invariants.rs",
        ))?;
        assert!(audit(&register, &workspace_root(), &workspace_packages()).is_empty());
        Ok(())
    }

    #[test]
    fn missing_register_is_not_a_failure() -> TestResult {
        let root = workspace_root().join("target/lgwks-deps-no-invariant-register");
        assert!(check(&root)?.is_none());
        Ok(())
    }
}
