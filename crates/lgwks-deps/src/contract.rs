//! `contract` owns the human-approved dependency register and enforces
//! INV-APPROVAL-IS-SEMANTIC: an entry is not an approval unless it says *what
//! the standard library cannot do*. A name on a list is a whitelist; a name
//! with a reason, a pin, an approver, a date, and a link to the evidence is a
//! contract, and only the second one is admissible here.
//!
//! The register lives at `contract/APPROVED.toml` in the repo being gated. It
//! is valid TOML so an editor or a human can read it, but it is parsed by a
//! line-oriented reader in this module rather than by a TOML crate — taking a
//! dependency in order to police dependencies would be self-refuting. The
//! reader refuses any line it does not recognise instead of skipping it, so a
//! typo cannot quietly become an unenforced entry.
//!
//! Approval is a diff. There is no command that adds an entry: a human writes
//! the block and commits it, which is what makes the approval reviewable and
//! attributable. `lgwks-deps request` only prints the block to be filled in.

use std::error::Error;
use std::fmt;

// ── The register ────────────────────────────────────────────────────────────

/// Where an approved crate sits in the ladder. Only two tiers are admissible:
/// ELIMINATE and CONSOLIDATE crates do not get entries, they get an
/// `lgwks_std` module, and an entry claiming either tier is a category error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Out of scope for reimplementation — kept as a direct dependency.
    Boundary,
    /// Kept as audited upstream source under `vendor/`, not as a registry edge.
    Vendor,
}

impl Tier {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "boundary" => Some(Self::Boundary),
            "vendor" => Some(Self::Vendor),
            _ => None,
        }
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Boundary => "boundary",
            Self::Vendor => "vendor",
        })
    }
}

/// One approved dependency, with the evidence that justified it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Package name as `Cargo.lock` spells it.
    pub krate: String,
    /// Which tier the approval sits in.
    pub tier: Tier,
    /// Approved Cargo manifest requirement, exactly as metadata reports it.
    pub version: String,
    /// Workspace crate responsible for this external capability.
    pub owner: String,
    /// Stable semantic capability supplied by the dependency.
    pub capability: String,
    /// Admitted Cargo source class: `registry`, `git`, or `path`.
    pub source: String,
    /// Workspace crates permitted to declare this edge directly.
    pub allowed_consumers: Vec<String>,
    /// Permitted edge kinds: `normal`, `build`, and/or `dev`.
    pub allowed_kinds: Vec<String>,
    /// One sentence naming what the standard library cannot do.
    pub reason: String,
    /// The human who approved it.
    pub approved_by: String,
    /// ISO date of approval.
    pub approved_on: String,
    /// Path or URL to the evidence behind the approval.
    pub review: String,
    /// Line where the entry opened, for diagnosis.
    pub line: usize,
}

/// The parsed register.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contract {
    /// When false, refusals are reported as warnings instead of failing the
    /// build. Adoption-only: flipping it is a reviewable diff in the register
    /// itself, never an environment variable a process can set for itself.
    pub enforce: bool,
    /// Canonical repository URL whose workspace members are local authority.
    pub repository: Option<String>,
    /// Every approved dependency.
    pub entries: Vec<Entry>,
}

// ── Errors ──────────────────────────────────────────────────────────────────

/// Why a register is not a contract. Each variant carries the line so the
/// message can be pasted straight into an editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractError {
    /// A line matched neither a section header, a key/value pair, nor a comment.
    Malformed {
        /// One-based line number.
        line: usize,
        /// The offending line, trimmed.
        text: String,
    },
    /// A key appeared that the schema does not define.
    UnknownKey {
        /// One-based line number.
        line: usize,
        /// The offending key.
        key: String,
    },
    /// A key/value pair appeared before any section header.
    OrphanKey {
        /// One-based line number.
        line: usize,
        /// The offending key.
        key: String,
    },
    /// A required field was absent from an entry.
    MissingField {
        /// The entry's crate name, or `<unnamed>`.
        krate: String,
        /// The absent field.
        field: &'static str,
    },
    /// `tier` held a value outside `boundary` / `vendor`.
    BadTier {
        /// One-based line number.
        line: usize,
        /// The offending value.
        value: String,
    },
    /// `approved_on` was not an ISO `YYYY-MM-DD` date.
    BadDate {
        /// The entry's crate name.
        krate: String,
        /// The offending value.
        value: String,
    },
    /// `reason` did not name what the standard library cannot do. A reason must
    /// be a sentence — at least four words, at least 24 characters, ending in a
    /// full stop — and must not merely restate the crate's name.
    ThinReason {
        /// The entry's crate name.
        krate: String,
    },
    /// The same crate/capability owner pair was approved twice.
    DuplicateEntry {
        /// The repeated crate name.
        krate: String,
        /// One-based line where the duplicate opened.
        line: usize,
    },
}

fn fmt_malformed(f: &mut fmt::Formatter<'_>, line: usize, text: &str) -> fmt::Result {
    write!(f, "line {line}: cannot parse {text:?}")
}

fn fmt_unknown_key(f: &mut fmt::Formatter<'_>, line: usize, key: &str) -> fmt::Result {
    write!(f, "line {line}: unknown key {key:?}")
}

fn fmt_orphan_key(f: &mut fmt::Formatter<'_>, line: usize, key: &str) -> fmt::Result {
    write!(
        f,
        "line {line}: key {key:?} appears before any section header"
    )
}

fn fmt_missing_field(f: &mut fmt::Formatter<'_>, krate: &str, field: &str) -> fmt::Result {
    write!(
        f,
        "approval for {krate:?} is missing required field {field:?}"
    )
}

fn fmt_bad_tier(f: &mut fmt::Formatter<'_>, line: usize, value: &str) -> fmt::Result {
    write!(
        f,
        "line {line}: tier {value:?} is not 'boundary' or 'vendor'"
    )
}

fn fmt_bad_date(f: &mut fmt::Formatter<'_>, krate: &str, value: &str) -> fmt::Result {
    write!(
        f,
        "approval for {krate:?} has approved_on {value:?}, want YYYY-MM-DD"
    )
}

fn fmt_thin_reason(f: &mut fmt::Formatter<'_>, krate: &str) -> fmt::Result {
    write!(
        f,
        "approval for {krate:?} needs a reason naming what std cannot do — \
         a sentence of four or more words ending in a full stop"
    )
}

fn fmt_duplicate(f: &mut fmt::Formatter<'_>, line: usize, krate: &str) -> fmt::Result {
    write!(f, "line {line}: {krate:?} is already approved")
}

impl fmt::Display for ContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed { line, text } => fmt_malformed(f, *line, text),
            Self::UnknownKey { line, key } => fmt_unknown_key(f, *line, key),
            Self::OrphanKey { line, key } => fmt_orphan_key(f, *line, key),
            Self::MissingField { krate, field } => fmt_missing_field(f, krate, field),
            Self::BadTier { line, value } => fmt_bad_tier(f, *line, value),
            Self::BadDate { krate, value } => fmt_bad_date(f, krate, value),
            Self::ThinReason { krate } => fmt_thin_reason(f, krate),
            Self::DuplicateEntry { krate, line } => fmt_duplicate(f, *line, krate),
        }
    }
}

impl Error for ContractError {}

// ── Parsing ─────────────────────────────────────────────────────────────────

const REQUIRED: [&str; 12] = [
    "crate",
    "tier",
    "version",
    "owner",
    "capability",
    "source",
    "allowed_consumers",
    "allowed_kinds",
    "reason",
    "approved_by",
    "approved_on",
    "review",
];

#[derive(Default)]
struct Draft {
    line: usize,
    fields: Vec<(&'static str, String)>,
}

impl Draft {
    fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(PartialEq, Eq)]
enum Section {
    None,
    Policy,
    Approved,
}

fn handle_section_header(
    line: &str,
    line_no: usize,
    section: &mut Section,
    drafts: &mut Vec<Draft>,
) -> Result<bool, ContractError> {
    if line == "[policy]" {
        *section = Section::Policy;
        Ok(true)
    } else if line == "[[approved]]" {
        *section = Section::Approved;
        drafts.push(Draft {
            line: line_no,
            fields: Vec::new(),
        });
        Ok(true)
    } else if line.starts_with('[') {
        Err(ContractError::Malformed {
            line: line_no,
            text: line.to_string(),
        })
    } else {
        Ok(false)
    }
}

fn apply_policy_pair(
    key: &str,
    value: &str,
    line_no: usize,
    enforce: &mut bool,
    repository: &mut Option<String>,
) -> Result<(), ContractError> {
    if key == "enforce" {
        *enforce = value == "true";
        Ok(())
    } else if key == "repository" {
        *repository = Some(unquote(value).to_string());
        Ok(())
    } else {
        Err(ContractError::UnknownKey {
            line: line_no,
            key: key.to_string(),
        })
    }
}

fn apply_approved_pair(
    key: &str,
    value: &str,
    line_no: usize,
    drafts: &mut [Draft],
) -> Result<(), ContractError> {
    let known = REQUIRED
        .iter()
        .find(|k| **k == key)
        .ok_or_else(|| ContractError::UnknownKey {
            line: line_no,
            key: key.to_string(),
        })?;
    let draft = drafts.last_mut().expect("approved section implies a draft");
    draft.fields.push((known, unquote(value).to_string()));
    Ok(())
}

fn process_pair(
    section: &Section,
    key: &str,
    value: &str,
    line_no: usize,
    enforce: &mut bool,
    repository: &mut Option<String>,
    drafts: &mut [Draft],
) -> Result<(), ContractError> {
    match section {
        Section::None => Err(ContractError::OrphanKey {
            line: line_no,
            key: key.to_string(),
        }),
        Section::Policy => apply_policy_pair(key, value, line_no, enforce, repository),
        Section::Approved => apply_approved_pair(key, value, line_no, drafts),
    }
}

fn process_contract_line(
    raw: &str,
    index: usize,
    section: &mut Section,
    enforce: &mut bool,
    repository: &mut Option<String>,
    drafts: &mut Vec<Draft>,
) -> Result<(), ContractError> {
    let line_no = index + 1;
    let line = strip_comment(raw).trim();
    if line.is_empty() {
        return Ok(());
    }
    if handle_section_header(line, line_no, section, drafts)? {
        return Ok(());
    }
    let (key, value) = split_pair(line).ok_or_else(|| ContractError::Malformed {
        line: line_no,
        text: line.to_string(),
    })?;
    process_pair(section, key, value, line_no, enforce, repository, drafts)
}

fn check_duplicate_entry(
    entries: &[Entry],
    entry: &Entry,
    line: usize,
) -> Result<(), ContractError> {
    let duplicate = entries.iter().any(|existing| {
        normalise(&existing.krate) == normalise(&entry.krate)
            && normalise(&existing.owner) == normalise(&entry.owner)
            && existing.capability == entry.capability
    });
    if duplicate {
        Err(ContractError::DuplicateEntry {
            krate: entry.krate.clone(),
            line,
        })
    } else {
        Ok(())
    }
}

impl Contract {
    /// Parses a register. Fail-closed: an unrecognised line is an error, not a
    /// line to skip.
    pub fn parse(text: &str) -> Result<Self, ContractError> {
        let mut enforce = true;
        let mut repository = None;
        let mut drafts: Vec<Draft> = Vec::new();
        let mut section = Section::None;

        for (index, raw) in text.lines().enumerate() {
            process_contract_line(
                raw,
                index,
                &mut section,
                &mut enforce,
                &mut repository,
                &mut drafts,
            )?;
        }

        let mut entries: Vec<Entry> = Vec::new();
        for draft in &drafts {
            let entry = build(draft)?;
            check_duplicate_entry(&entries, &entry, draft.line)?;
            entries.push(entry);
        }
        Ok(Self {
            enforce,
            repository,
            entries,
        })
    }

    /// Finds the approval for a resolved package, tolerating `-`/`_` spelling
    /// drift between a manifest and a lock file.
    pub fn approval_for(&self, krate: &str) -> Option<&Entry> {
        let wanted = normalise(krate);
        self.entries.iter().find(|e| normalise(&e.krate) == wanted)
    }

    /// Returns every semantic approval for an upstream package. A package may
    /// legitimately back distinct capabilities with different owners.
    pub fn approvals_for<'a>(&'a self, krate: &'a str) -> impl Iterator<Item = &'a Entry> {
        let wanted = normalise(krate);
        self.entries
            .iter()
            .filter(move |entry| normalise(&entry.krate) == wanted)
    }
}

fn check_field_present(
    draft: &Draft,
    krate: &str,
    field: &'static str,
) -> Result<(), ContractError> {
    if draft.get(field).is_some_and(|v| !v.trim().is_empty()) {
        Ok(())
    } else {
        Err(ContractError::MissingField {
            krate: krate.to_string(),
            field,
        })
    }
}

fn validate_required_fields(draft: &Draft, krate: &str) -> Result<(), ContractError> {
    for field in REQUIRED {
        check_field_present(draft, krate, field)?;
    }
    Ok(())
}

fn validate_tier(draft: &Draft) -> Result<Tier, ContractError> {
    let tier_text = draft.get("tier").expect("checked above");
    Tier::parse(tier_text).ok_or_else(|| ContractError::BadTier {
        line: draft.line,
        value: tier_text.to_string(),
    })
}

fn validate_date(approved_on: &str, krate: &str) -> Result<(), ContractError> {
    if !is_iso_date(approved_on) {
        Err(ContractError::BadDate {
            krate: krate.to_string(),
            value: approved_on.to_string(),
        })
    } else {
        Ok(())
    }
}

fn validate_reason(reason: &str, krate: &str) -> Result<(), ContractError> {
    if !is_a_sentence(reason, krate) {
        Err(ContractError::ThinReason {
            krate: krate.to_string(),
        })
    } else {
        Ok(())
    }
}

fn build(draft: &Draft) -> Result<Entry, ContractError> {
    let krate = draft.get("crate").unwrap_or("<unnamed>").to_string();
    validate_required_fields(draft, &krate)?;
    let tier = validate_tier(draft)?;
    let approved_on = draft.get("approved_on").expect("checked above").to_string();
    validate_date(&approved_on, &krate)?;
    let reason = draft.get("reason").expect("checked above").to_string();
    validate_reason(&reason, &krate)?;

    Ok(Entry {
        krate,
        tier,
        version: draft.get("version").expect("checked above").to_string(),
        owner: draft.get("owner").expect("checked above").to_string(),
        capability: draft.get("capability").expect("checked above").to_string(),
        source: draft.get("source").expect("checked above").to_string(),
        allowed_consumers: split_csv(draft.get("allowed_consumers").expect("checked above")),
        allowed_kinds: split_csv(draft.get("allowed_kinds").expect("checked above")),
        reason,
        approved_by: draft.get("approved_by").expect("checked above").to_string(),
        approved_on,
        review: draft.get("review").expect("checked above").to_string(),
        line: draft.line,
    })
}

fn split_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToString::to_string)
        .collect()
}

// ── Field rules ─────────────────────────────────────────────────────────────

/// A reason has to be a sentence that says something. The floor is deliberately
/// low enough to pass any honest justification and high enough to fail
/// `reason = "needed"` or `reason = "serde"`.
fn is_a_sentence(reason: &str, krate: &str) -> bool {
    let trimmed = reason.trim();
    if trimmed.len() < 24 || !trimmed.ends_with('.') {
        return false;
    }
    if trimmed.split_whitespace().count() < 4 {
        return false;
    }
    normalise(trimmed.trim_end_matches('.')) != normalise(krate)
}

fn is_iso_date(value: &str) -> bool {
    let date_bytes = value.as_bytes();
    date_bytes.len() == 10
        && date_bytes[4] == b'-'
        && date_bytes[7] == b'-'
        && [0, 1, 2, 3, 5, 6, 8, 9]
            .iter()
            .all(|&idx| date_bytes[idx].is_ascii_digit())
}

/// Cargo treats `-` and `_` as interchangeable in package names; so does this.
fn normalise(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace('-', "_")
}

// ── Line reading ────────────────────────────────────────────────────────────

/// Drops a trailing `#` comment, respecting quotes so a `#` inside a reason or
/// a review URL survives.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_quotes = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        match b {
            b'\\' if in_quotes => escaped = true,
            b'"' => in_quotes = !in_quotes,
            b'#' if !in_quotes => return &line[..i],
            _ => {}
        }
    }
    line
}

fn split_pair(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.split_once('=')?;
    let key = key.trim();
    if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some((key, value.trim()))
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(extra: &str) -> String {
        format!(
            concat!(
                "[[approved]]\n",
                "crate = \"serde\"\n",
                "tier = \"boundary\"\n",
                "version = \"1.0\"\n",
                "owner = \"lgwks_std\"\n",
                "capability = \"json.serialization\"\n",
                "source = \"registry\"\n",
                "allowed_consumers = \"lgwks_std\"\n",
                "allowed_kinds = \"normal\"\n",
                "reason = \"Derive-based serialization needs compiler introspection std does not expose.\"\n",
                "approved_by = \"Director\"\n",
                "approved_on = \"2026-08-19\"\n",
                "review = \"docs/ADMISSION.md\"\n",
                "{}"
            ),
            extra
        )
    }

    fn complete(input: &str) -> String {
        let mut output = String::new();
        for line in input.lines() {
            output.push_str(line);
            output.push('\n');
            if line.trim_start().starts_with("version =") {
                output.push_str(
                    "owner = \"lgwks_std\"\n\
                     capability = \"json.serialization\"\n\
                     source = \"registry\"\n\
                     allowed_consumers = \"lgwks_std\"\n\
                     allowed_kinds = \"normal\"\n",
                );
            }
        }
        output
    }

    #[test]
    fn a_complete_entry_parses() {
        let contract = Contract::parse(&entry("")).unwrap();
        assert_eq!(contract.entries.len(), 1);
        let e = &contract.entries[0];
        assert_eq!(e.krate, "serde");
        assert_eq!(e.tier, Tier::Boundary);
        assert_eq!(e.version, "1.0");
        assert_eq!(e.owner, "lgwks_std");
        assert_eq!(e.capability, "json.serialization");
        assert_eq!(e.allowed_consumers, ["lgwks_std"]);
        assert!(e.reason.ends_with('.'));
        assert_eq!(e.approved_by, "Director");
        assert_eq!(e.approved_on, "2026-08-19");
        assert_eq!(e.review, "docs/ADMISSION.md");
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let input = concat!(
            "# A top-level comment\n",
            "\n",
            "[policy] # inline\n",
            "enforce = true\n",
            "\n",
        );
        let contract = Contract::parse(&complete(input)).unwrap();
        assert!(contract.enforce);
        assert!(contract.entries.is_empty());
    }

    #[test]
    fn a_hash_inside_a_quoted_value_survives() {
        let input = concat!(
            "[[approved]]\n",
            "crate = \"serde\"\n",
            "tier = \"boundary\"\n",
            "version = \"1.0\"\n",
            "reason = \"Derive-based serialization needs compiler introspection std does not expose.\"\n",
            "approved_by = \"Director\"\n",
            "approved_on = \"2026-08-19\"\n",
            "review = \"https://example.com/pr#123\"\n",
        );
        let contract = Contract::parse(&complete(input)).unwrap();
        assert_eq!(contract.entries[0].review, "https://example.com/pr#123");
    }

    #[test]
    fn an_unknown_key_is_refused_rather_than_skipped() {
        let input = entry("typo = \"boom\"\n");
        assert_eq!(
            Contract::parse(&input),
            Err(ContractError::UnknownKey {
                line: 14,
                key: "typo".into()
            })
        );
    }

    #[test]
    fn a_key_before_any_section_is_refused() {
        let input = "orphan = \"value\"\n";
        assert_eq!(
            Contract::parse(&complete(input)),
            Err(ContractError::OrphanKey {
                line: 1,
                key: "orphan".into()
            })
        );
    }

    #[test]
    fn a_missing_field_is_refused() {
        let input = concat!(
            "[[approved]]\n",
            "crate = \"serde\"\n",
            "tier = \"boundary\"\n",
            "version = \"1.0\"\n",
            "approved_by = \"Director\"\n",
            "approved_on = \"2026-08-19\"\n",
            "review = \"docs/ADMISSION.md\"\n",
        );
        assert_eq!(
            Contract::parse(&complete(input)),
            Err(ContractError::MissingField {
                krate: "serde".into(),
                field: "reason"
            })
        );
    }

    #[test]
    fn an_eliminate_tier_entry_is_a_category_error() {
        let input = concat!(
            "[[approved]]\n",
            "crate = \"hex\"\n",
            "tier = \"eliminate\"\n",
            "version = \"0.4\"\n",
            "reason = \"Workspace stdlib replaces this; no external crate is admissible here.\"\n",
            "approved_by = \"Director\"\n",
            "approved_on = \"2026-08-19\"\n",
            "review = \"docs/ADMISSION.md\"\n",
        );
        assert_eq!(
            Contract::parse(&complete(input)),
            Err(ContractError::BadTier {
                line: 1,
                value: "eliminate".into()
            })
        );
    }

    #[test]
    fn a_malformed_date_is_refused() {
        let input = concat!(
            "[[approved]]\n",
            "crate = \"serde\"\n",
            "tier = \"boundary\"\n",
            "version = \"1.0\"\n",
            "reason = \"Compiler introspection needed.\"\n",
            "approved_by = \"Director\"\n",
            "approved_on = \"19-08-2026\"\n",
            "review = \"docs/ADMISSION.md\"\n",
        );
        assert_eq!(
            Contract::parse(&complete(input)),
            Err(ContractError::BadDate {
                krate: "serde".into(),
                value: "19-08-2026".into()
            })
        );
    }

    #[test]
    fn a_reason_that_is_not_a_sentence_is_refused() {
        let input = concat!(
            "[[approved]]\n",
            "crate = \"serde\"\n",
            "tier = \"boundary\"\n",
            "version = \"1.0\"\n",
            "reason = \"needed\"\n",
            "approved_by = \"Director\"\n",
            "approved_on = \"2026-08-19\"\n",
            "review = \"docs/ADMISSION.md\"\n",
        );
        assert_eq!(
            Contract::parse(&complete(input)),
            Err(ContractError::ThinReason {
                krate: "serde".into()
            })
        );
    }

    #[test]
    fn a_reason_that_restates_the_crate_name_is_refused() {
        let input = concat!(
            "[[approved]]\n",
            "crate = \"serde\"\n",
            "tier = \"boundary\"\n",
            "version = \"1.0\"\n",
            "reason = \"serde.\"\n",
            "approved_by = \"Director\"\n",
            "approved_on = \"2026-08-19\"\n",
            "review = \"docs/ADMISSION.md\"\n",
        );
        assert_eq!(
            Contract::parse(&complete(input)),
            Err(ContractError::ThinReason {
                krate: "serde".into()
            })
        );
    }

    #[test]
    fn a_duplicate_approval_is_refused() {
        let input = format!("{}\n{}", entry(""), entry(""));
        assert_eq!(
            Contract::parse(&input),
            Err(ContractError::DuplicateEntry {
                krate: "serde".into(),
                line: 15
            })
        );
    }

    #[test]
    fn enforcement_defaults_to_on_when_no_policy_is_written() {
        let contract = Contract::parse(&entry("")).unwrap();
        assert!(contract.enforce);
    }

    #[test]
    fn policy_can_stand_enforcement_down_for_adoption() {
        let input = format!("[policy]\nenforce = false\n\n{}", entry(""));
        let contract = Contract::parse(&input).unwrap();
        assert!(!contract.enforce);
    }

    #[test]
    fn lookup_tolerates_hyphen_underscore_drift() {
        let contract = Contract::parse(&entry("")).unwrap();
        assert!(contract.approval_for("serde").is_some());
    }
}
