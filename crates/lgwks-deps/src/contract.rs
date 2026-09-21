//! `contract` owns the human-approved dependency register and enforces
//! INV-APPROVAL-IS-SEMANTIC: an entry is not an approval unless it says *what
//! the standard library cannot do*. A name on a list is a whitelist; a name
//! with a reason, a pin, an approver, a date, and a link to the evidence is a
//! contract, and only the second one is admissible here.
//!
//! The register lives at `contract/APPROVED.toml` in the repo being gated. It
//! is valid TOML so an editor or a human can read it, but it is parsed by a
//! line-oriented reader in this module rather than by a TOML crate: taking a
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
#[non_exhaustive]
pub enum Tier {
    /// Out of scope for reimplementation; kept as a direct dependency.
    Boundary,
    /// Kept as audited upstream source under `vendor/`, not as a registry edge.
    Vendor,
}

impl Tier {
    /// Decodes the register spelling of a tier.
    ///
    /// Rejects everything else, including the `eliminate` and `consolidate`
    /// tiers: those are answered by an `lgwks_std` module rather than by an
    /// approval, so a register naming one has made a category error and must
    /// fail rather than fall back to a default tier. Matching is exact and
    /// case-sensitive, so `Boundary` is not accepted for `boundary`.
    fn parse(value: &str) -> Option<Self> {
        match value {
            "boundary" => Some(Self::Boundary),
            "vendor" => Some(Self::Vendor),
            _ => None,
        }
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `Tier` is `Copy` and both variants are unit variants, so matching on
        // the dereferenced value moves nothing.
        formatter.write_str(match *self {
            Self::Boundary => "boundary",
            Self::Vendor => "vendor",
        })
    }
}

/// One approved dependency, with the evidence that justified it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
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
#[non_exhaustive]
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
#[non_exhaustive]
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
    /// A `[policy]` value was outside its closed grammar.
    ///
    /// `enforce` admits exactly `true` and exactly `false`. Every other token,
    /// including `True`, `"true"` and `1`, is this refusal: the earlier reader
    /// read any unrecognised spelling as `false`, which let a typo stand the
    /// whole gate down without a diagnostic.
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
    /// `approved_on` was not an ISO `YYYY-MM-DD` date.
    BadDate {
        /// The entry's crate name.
        krate: String,
        /// The offending value.
        value: String,
    },
    /// `reason` did not name what the standard library cannot do. A reason must
    /// be a sentence (at least four words, at least 24 characters, ending in a
    /// full stop) and must not merely restate the crate's name.
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

/// Renders `ContractError::Malformed`. The offending text is quoted with `{:?}`
/// so trailing whitespace and an empty line are both visible in the message.
fn fmt_malformed(formatter: &mut fmt::Formatter<'_>, line: usize, text: &str) -> fmt::Result {
    write!(formatter, "line {line}: cannot parse {text:?}")
}

/// Renders `ContractError::UnknownKey`, naming the key the schema does not
/// define so the register can be corrected without re-reading this module.
fn fmt_unknown_key(formatter: &mut fmt::Formatter<'_>, line: usize, key: &str) -> fmt::Result {
    write!(formatter, "line {line}: unknown key {key:?}")
}

/// Renders `ContractError::OrphanKey`, which is what a key/value pair written
/// above the first section header produces.
fn fmt_orphan_key(formatter: &mut fmt::Formatter<'_>, line: usize, key: &str) -> fmt::Result {
    write!(
        formatter,
        "line {line}: key {key:?} appears before any section header"
    )
}

/// Renders `ContractError::MissingField`. The entry is named by crate because
/// the field's line is not known once the draft has been closed.
fn fmt_missing_field(formatter: &mut fmt::Formatter<'_>, krate: &str, field: &str) -> fmt::Result {
    write!(
        formatter,
        "approval for {krate:?} is missing required field {field:?}"
    )
}

/// Renders `ContractError::BadTier`, restating the only two admissible values
/// so the message is actionable on its own.
fn fmt_bad_tier(formatter: &mut fmt::Formatter<'_>, line: usize, value: &str) -> fmt::Result {
    write!(
        formatter,
        "line {line}: tier {value:?} is not 'boundary' or 'vendor'"
    )
}

/// Renders `ContractError::BadPolicyValue`. A quoted `"true"` is shown with its
/// quotes so the reader can see that the register carried a string where the
/// grammar requires a Boolean.
fn fmt_bad_policy_value(
    formatter: &mut fmt::Formatter<'_>,
    line: usize,
    key: &str,
    value: &str,
) -> fmt::Result {
    write!(
        formatter,
        "line {line}: [policy] {key} = {value:?} is not a Boolean; write exactly true or false"
    )
}

/// Renders `ContractError::DuplicatePolicyKey`, naming the repeated key so the
/// second assignment can be deleted without re-reading this module.
fn fmt_duplicate_policy_key(
    formatter: &mut fmt::Formatter<'_>,
    line: usize,
    key: &str,
) -> fmt::Result {
    write!(
        formatter,
        "line {line}: [policy] {key} is written more than once; keep one assignment"
    )
}

/// Renders `ContractError::DuplicatePolicySection` at the second `[policy]`, so
/// the two declarations can be merged rather than guessed between.
fn fmt_duplicate_policy_section(formatter: &mut fmt::Formatter<'_>, line: usize) -> fmt::Result {
    write!(
        formatter,
        "line {line}: [policy] is declared more than once; a second declaration would \
         silently override the first"
    )
}

/// Renders `ContractError::BadDate`, printing the required layout explicitly.
fn fmt_bad_date(formatter: &mut fmt::Formatter<'_>, krate: &str, value: &str) -> fmt::Result {
    write!(
        formatter,
        "approval for {krate:?} has approved_on {value:?}, want YYYY-MM-DD"
    )
}

/// Renders `ContractError::ThinReason`, spelling out the sentence rule so the
/// refusal explains itself rather than pointing back at the source.
fn fmt_thin_reason(formatter: &mut fmt::Formatter<'_>, krate: &str) -> fmt::Result {
    write!(
        formatter,
        "approval for {krate:?} needs a reason naming what std cannot do — \
         a sentence of four or more words ending in a full stop"
    )
}

/// Renders `ContractError::DuplicateEntry` at the line the second approval
/// opened, not at the line of the first.
fn fmt_duplicate(formatter: &mut fmt::Formatter<'_>, line: usize, krate: &str) -> fmt::Result {
    write!(formatter, "line {line}: {krate:?} is already approved")
}

impl fmt::Display for ContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `ContractError` is not `Copy`, so the borrowed payloads are bound by
        // `ref`; matching the dereferenced value keeps the pattern types equal
        // to the variant types without cloning a single `String`.
        match *self {
            Self::Malformed { line, ref text } => fmt_malformed(formatter, line, text),
            Self::UnknownKey { line, ref key } => fmt_unknown_key(formatter, line, key),
            Self::OrphanKey { line, ref key } => fmt_orphan_key(formatter, line, key),
            Self::MissingField { ref krate, field } => fmt_missing_field(formatter, krate, field),
            Self::BadTier { line, ref value } => fmt_bad_tier(formatter, line, value),
            Self::BadPolicyValue {
                line,
                ref key,
                ref value,
            } => fmt_bad_policy_value(formatter, line, key, value),
            Self::DuplicatePolicyKey { line, ref key } => {
                fmt_duplicate_policy_key(formatter, line, key)
            }
            Self::DuplicatePolicySection { line } => fmt_duplicate_policy_section(formatter, line),
            Self::BadDate {
                ref krate,
                ref value,
            } => fmt_bad_date(formatter, krate, value),
            Self::ThinReason { ref krate } => fmt_thin_reason(formatter, krate),
            Self::DuplicateEntry { line, ref krate } => fmt_duplicate(formatter, line, krate),
        }
    }
}

impl Error for ContractError {}

// ── Parsing ─────────────────────────────────────────────────────────────────

/// Every key an `[[approved]]` block must carry, in the order the parser checks
/// them. The order is observable: `validate_required_fields` reports the first
/// absent key, so a register missing several is told about the earliest one and
/// the message is stable across runs.
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

/// One repeated register block as it is being read, before validation.
///
/// Fields are kept in the order they appeared rather than in a map, because a
/// repeated key is deliberately allowed to overwrite its earlier value and the
/// parser must stay allocation-light: this module may not take a dependency on
/// a TOML or map crate.
#[derive(Default)]
pub(crate) struct RawEntry {
    /// One-based line where the `[[approved]]` header opened. Diagnostics for
    /// the whole block point here, since individual fields carry no lines.
    line: usize,
    /// Raw key/value pairs, unquoted but otherwise unvalidated.
    fields: Vec<(&'static str, String)>,
}

impl RawEntry {
    /// Returns the last value written for `key`, or `None` if the block never
    /// carried it. An empty string is returned as `Some("")`: presence and
    /// non-emptiness are separate questions, and `check_field_present` is what
    /// rejects the blank case.
    pub(crate) fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|field| field.0 == key)
            .map(|field| field.1.as_str())
    }

    /// Returns a field that `validate_required_fields` has already proven
    /// present, or the typed `MissingField` refusal for its absence.
    ///
    /// The absent branch is unreachable from `build`, which validates first;
    /// the method exists so the invariant is carried by the return type rather
    /// than by an `expect` that would abort the process on a parser bug.
    pub(crate) fn require(&self, key: &'static str, krate: &str) -> Result<&str, ContractError> {
        self.get(key).ok_or_else(|| ContractError::MissingField {
            krate: krate.to_owned(),
            field: key,
        })
    }

    /// One-based line where this entry opened.
    pub(crate) const fn line(&self) -> usize {
        self.line
    }
}

/// Which part of the register the reader is currently inside, so a key can be
/// dispatched to the policy block or to the open approval block.
#[derive(PartialEq, Eq)]
enum Section {
    /// Before any header. A key here is an orphan and is refused.
    None,
    /// Inside `[policy]`.
    Policy,
    /// Inside the register's repeated entry block, which the reader has
    /// already pushed a [`RawEntry`] for.
    Entry,
}

/// Mutable state for one pass through either register schema.
struct Reader<'a> {
    /// Repeated table header accepted by this register.
    entry_header: &'a str,
    /// Entry keys accepted by this register.
    allowed_keys: &'a [&'static str],
    /// Current section.
    section: Section,
    /// Whether a `[policy]` header has already been read.
    policy_declared: bool,
    /// Policy keys already assigned, so a repeat is a refusal.
    policy_keys: Vec<String>,
    /// Policy enforcement flag, defaulting to true.
    enforce: bool,
    /// Optional repository authority.
    repository: Option<String>,
    /// Raw entries in source order.
    entries: Vec<RawEntry>,
}

/// Recognises a section header, opening a new raw entry for `entry_header`.
///
/// Returns `Ok(true)` when the line was a header the reader consumed, `Ok(false)`
/// when it is an ordinary key/value line, and `Malformed` for a bracketed line
/// that names no known section: a typo like `[[aproved]]` must not silently
/// fall through and become an unenforced entry.
fn handle_section_header(
    line: &str,
    line_no: usize,
    entry_header: &str,
    section: &mut Section,
    policy_declared: &mut bool,
    drafts: &mut Vec<RawEntry>,
) -> Result<bool, ContractError> {
    if line == "[policy]" {
        // A second `[policy]` is refused rather than merged. The two blocks are
        // read by the same reader, so a later one silently overwrites whatever
        // the earlier one set; refusing is what keeps the register's effective
        // enforcement flag the one a reviewer read.
        if *policy_declared {
            return Err(ContractError::DuplicatePolicySection { line: line_no });
        }
        *policy_declared = true;
        *section = Section::Policy;
        Ok(true)
    } else if line == entry_header {
        *section = Section::Entry;
        drafts.push(RawEntry {
            line: line_no,
            fields: Vec::new(),
        });
        Ok(true)
    } else if line.starts_with('[') {
        Err(ContractError::Malformed {
            line: line_no,
            text: line.to_owned(),
        })
    } else {
        Ok(false)
    }
}

/// Every key the `[policy]` block defines.
const POLICY_KEYS: [&str; 2] = ["enforce", "repository"];

/// Decodes `[policy] enforce` under a closed grammar: exactly `true` or exactly
/// `false`, and nothing else.
///
/// The token is the raw pair value after comment stripping and trimming, so a
/// quoted `"true"` still arrives with its quotes and is refused as a string
/// where the grammar requires a Boolean. This is the difference the previous
/// reader could not make: it read *any* unrecognised spelling as `false`, so
/// `enforce = True`, `enforce = "true"` and `enforce = 1` each stood the whole
/// gate down without a diagnostic. A gate a typo can disable is a gate a typo
/// does disable.
fn decode_enforce(value: &str, line_no: usize) -> Result<bool, ContractError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(ContractError::BadPolicyValue {
            line: line_no,
            key: "enforce".to_owned(),
            value: value.to_owned(),
        }),
    }
}

/// Applies one key/value pair written inside `[policy]`.
///
/// A repeated key is refused rather than overwritten: "last write wins" would
/// make the effective enforcement flag depend on line order, which is exactly
/// the ambiguity this block exists to remove. The quote wrapper is stripped
/// from `repository` because the reader does not implement TOML escapes, so an
/// escaped quote inside the value is not accepted.
fn apply_policy_pair(
    key: &str,
    value: &str,
    line_no: usize,
    seen_keys: &mut Vec<String>,
    enforce: &mut bool,
    repository: &mut Option<String>,
) -> Result<(), ContractError> {
    if !POLICY_KEYS.contains(&key) {
        return Err(ContractError::UnknownKey {
            line: line_no,
            key: key.to_owned(),
        });
    }
    if seen_keys.iter().any(|seen| seen == key) {
        return Err(ContractError::DuplicatePolicyKey {
            line: line_no,
            key: key.to_owned(),
        });
    }
    match key {
        "enforce" => {
            let decoded = decode_enforce(value, line_no)?;
            seen_keys.push(key.to_owned());
            *enforce = decoded;
            Ok(())
        }
        _ => {
            seen_keys.push(key.to_owned());
            *repository = Some(unquote(value).to_owned());
            Ok(())
        }
    }
}

/// Applies one key/value pair to the repeated entry currently being read.
///
/// Only keys listed in `REQUIRED` are accepted, so a misspelled field is a hard
/// refusal and cannot become an unenforced entry. A repeated key overwrites its
/// earlier value rather than erroring: the last write wins, which keeps the
/// parser's behaviour identical to the TOML reader a human might expect.
fn apply_entry_pair(
    key: &str,
    value: &str,
    line_no: usize,
    draft: &mut RawEntry,
    allowed_keys: &[&'static str],
) -> Result<(), ContractError> {
    let known = allowed_keys
        .iter()
        .find(|candidate| **candidate == key)
        .ok_or_else(|| ContractError::UnknownKey {
            line: line_no,
            key: key.to_owned(),
        })?;
    draft.fields.push((known, unquote(value).to_owned()));
    Ok(())
}

/// Dispatches a key/value pair according to the section the reader is inside.
///
/// `drafts` is a slice rather than a `Vec` because this function never grows the
/// list: only `handle_section_header` may push, and it runs first.
fn process_pair(
    reader: &mut Reader<'_>,
    key: &str,
    value: &str,
    line_no: usize,
) -> Result<(), ContractError> {
    // `Section` is a fieldless enum, so matching the dereferenced value copies
    // nothing and needs no `ref` bindings.
    match reader.section {
        Section::None => Err(ContractError::OrphanKey {
            line: line_no,
            key: key.to_owned(),
        }),
        Section::Policy => apply_policy_pair(
            key,
            value,
            line_no,
            &mut reader.policy_keys,
            &mut reader.enforce,
            &mut reader.repository,
        ),
        Section::Entry => {
            // `Section::Entry` is only ever entered by `handle_section_header`
            // pushing a draft, and a draft is never popped, so this is a failure
            // the type system cannot express away. Resolving it as an orphan key
            // rather than asserting keeps the reader fail-closed on a parser bug
            // instead of aborting the process mid-register.
            let draft = reader
                .entries
                .last_mut()
                .ok_or_else(|| ContractError::OrphanKey {
                    line: line_no,
                    key: key.to_owned(),
                })?;
            apply_entry_pair(key, value, line_no, draft, reader.allowed_keys)
        }
    }
}

/// Reads one raw line of the register: strips any trailing comment, skips a
/// blank or comment-only line, consumes a section header, and otherwise requires
/// a well-formed `key = value` pair.
///
/// A line that is neither is `Malformed` rather than skipped, which is the
/// property that makes the register fail-closed. `index` is the zero-based
/// position from `enumerate`; the reported number is one-based.
fn process_contract_line(
    raw: &str,
    index: usize,
    reader: &mut Reader<'_>,
) -> Result<(), ContractError> {
    // Diagnostics are one-based. `index` comes from `enumerate` over a string's
    // lines, so it is strictly less than the input length and cannot be
    // `usize::MAX`; saturating rather than wrapping guarantees the reported line
    // number stays monotonic even on that impossible input.
    let line_no = index.saturating_add(1);
    let line = strip_comment(raw).trim();
    if line.is_empty() {
        return Ok(());
    }
    if handle_section_header(
        line,
        line_no,
        reader.entry_header,
        &mut reader.section,
        &mut reader.policy_declared,
        &mut reader.entries,
    )? {
        return Ok(());
    }
    let (key, value) = split_pair(line).ok_or_else(|| ContractError::Malformed {
        line: line_no,
        text: line.to_owned(),
    })?;
    process_pair(reader, key, value, line_no)
}

/// The syntax-only result shared by the dependency and invariant registers.
///
/// The line reader owns TOML's small common subset; each register then applies
/// its own schema and repository-aware semantic checks to these raw entries.
pub(crate) struct RawRegister {
    /// Whether the register asks the caller to make refusals fatal.
    pub(crate) enforce: bool,
    /// Optional repository authority carried by the dependency register.
    pub(crate) repository: Option<String>,
    /// Repeated blocks in source order.
    pub(crate) entries: Vec<RawEntry>,
}

/// Parses the shared register shape without applying an entry schema.
///
/// `entry_header` selects the repeated TOML table and `allowed_keys` is the
/// register-specific schema. Keeping both parameters here means the invariant
/// register cannot silently grow a second line parser while the dependency
/// register retains its existing fail-closed behaviour.
pub(crate) fn parse_register(
    text: &str,
    entry_header: &str,
    allowed_keys: &[&'static str],
) -> Result<RawRegister, ContractError> {
    let mut reader = Reader {
        entry_header,
        allowed_keys,
        section: Section::None,
        policy_declared: false,
        policy_keys: Vec::new(),
        enforce: true,
        repository: None,
        entries: Vec::new(),
    };

    for (index, raw) in text.lines().enumerate() {
        process_contract_line(raw, index, &mut reader)?;
    }

    Ok(RawRegister {
        enforce: reader.enforce,
        repository: reader.repository,
        entries: reader.entries,
    })
}

/// Refuses a second approval for the same `(crate, owner, capability)` triple.
///
/// The comparison normalises crate and owner names through `normalise` but
/// compares capability byte-for-byte: `-`/`_` drift is a Cargo naming artefact
/// and is tolerated, while a capability is a semantic name this module controls
/// and a near-miss there is a real difference.
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
        let raw = parse_register(text, "[[approved]]", &REQUIRED)?;
        let mut entries: Vec<Entry> = Vec::new();
        for draft in &raw.entries {
            let entry = build(draft)?;
            check_duplicate_entry(&entries, &entry, draft.line)?;
            entries.push(entry);
        }
        Ok(Self {
            enforce: raw.enforce,
            repository: raw.repository,
            entries,
        })
    }

    /// Finds the approval for a resolved package, tolerating `-`/`_` spelling
    /// drift between a manifest and a lock file.
    ///
    /// Returns the first match only. A package that legitimately backs several
    /// capabilities has several entries, so callers that need all of them must
    /// use `approvals_for`; this one exists for the single-owner question.
    #[must_use]
    pub fn approval_for(&self, krate: &str) -> Option<&Entry> {
        let wanted = normalise(krate);
        self.entries
            .iter()
            .find(|entry| normalise(&entry.krate) == wanted)
    }

    /// Returns every semantic approval for an upstream package. A package may
    /// legitimately back distinct capabilities with different owners.
    ///
    /// Order follows the register, so for a given register the sequence is
    /// deterministic and diffable.
    pub fn approvals_for<'a>(&'a self, krate: &'a str) -> impl Iterator<Item = &'a Entry> {
        let wanted = normalise(krate);
        self.entries
            .iter()
            .filter(move |entry| normalise(&entry.krate) == wanted)
    }
}

/// Requires a field to be present and non-blank.
///
/// Whitespace only counts as blank, so `owner = "   "` is refused exactly as a
/// missing `owner` would be: a placeholder is not evidence.
pub(crate) fn check_field_present(
    draft: &RawEntry,
    krate: &str,
    field: &'static str,
) -> Result<(), ContractError> {
    if draft
        .get(field)
        .is_some_and(|value| !value.trim().is_empty())
    {
        Ok(())
    } else {
        Err(ContractError::MissingField {
            krate: krate.to_owned(),
            field,
        })
    }
}

/// Rejects the first required field that is absent or blank, in `REQUIRED`
/// order. `krate` is only used to name the offending block in the message.
fn validate_required_fields(draft: &RawEntry, krate: &str) -> Result<(), ContractError> {
    for field in REQUIRED {
        check_field_present(draft, krate, field)?;
    }
    Ok(())
}

/// Decodes `tier`, refusing anything outside `boundary` / `vendor`.
///
/// Callers run this after `validate_required_fields`, so an absent `tier` is
/// reported as `MissingField` before it can reach `Tier::parse`; the absent
/// branch here is typed rather than asserted so a reordering bug surfaces as a
/// refusal instead of a panic.
fn validate_tier(draft: &RawEntry, krate: &str) -> Result<Tier, ContractError> {
    let tier_text = draft.require("tier", krate)?;
    Tier::parse(tier_text).ok_or_else(|| ContractError::BadTier {
        line: draft.line,
        value: tier_text.to_owned(),
    })
}

/// Requires `approved_on` to be an ISO `YYYY-MM-DD` date.
///
/// Shape only: the calendar is not consulted, so `2026-13-45` is as acceptable
/// to this reader as `2026-01-01`. The field exists to make an approval
/// attributable to a review, not to schedule anything.
fn validate_date(approved_on: &str, krate: &str) -> Result<(), ContractError> {
    if is_iso_date(approved_on) {
        Ok(())
    } else {
        Err(ContractError::BadDate {
            krate: krate.to_owned(),
            value: approved_on.to_owned(),
        })
    }
}

/// Requires `reason` to clear the `is_a_sentence` bar, refusing the two shapes
/// that make an approval a whitelist entry: a one-word reason and a reason that
/// merely restates the crate's name.
fn validate_reason(reason: &str, krate: &str) -> Result<(), ContractError> {
    if is_a_sentence(reason, krate) {
        Ok(())
    } else {
        Err(ContractError::ThinReason {
            krate: krate.to_owned(),
        })
    }
}

/// Turns a fully-read draft into an `Entry`, validating every field rule.
///
/// The checks run in a fixed order (required fields, then tier, then date,
/// then reason), so a register with several defects always reports the same
/// one. `crate` falls back to `<unnamed>` for diagnostics only; a block whose
/// `crate` is absent still fails `validate_required_fields` immediately after.
fn build(draft: &RawEntry) -> Result<Entry, ContractError> {
    let krate = draft.get("crate").unwrap_or("<unnamed>").to_owned();
    validate_required_fields(draft, &krate)?;
    let tier = validate_tier(draft, &krate)?;
    let approved_on = draft.require("approved_on", &krate)?.to_owned();
    validate_date(&approved_on, &krate)?;
    let reason = draft.require("reason", &krate)?.to_owned();
    validate_reason(&reason, &krate)?;

    // Every remaining field is read while `krate` is still borrowable; the
    // struct literal below moves `krate`, so nothing may borrow it afterwards.
    let version = draft.require("version", &krate)?.to_owned();
    let owner = draft.require("owner", &krate)?.to_owned();
    let capability = draft.require("capability", &krate)?.to_owned();
    let source = draft.require("source", &krate)?.to_owned();
    let allowed_consumers = split_csv(draft.require("allowed_consumers", &krate)?);
    let allowed_kinds = split_csv(draft.require("allowed_kinds", &krate)?);
    let approved_by = draft.require("approved_by", &krate)?.to_owned();
    let review = draft.require("review", &krate)?.to_owned();

    Ok(Entry {
        krate,
        tier,
        version,
        owner,
        capability,
        source,
        allowed_consumers,
        allowed_kinds,
        reason,
        approved_by,
        approved_on,
        review,
        line: draft.line,
    })
}

/// Splits a comma-separated list field, trimming each item and dropping the
/// empty ones.
///
/// Dropping empties is what lets `allowed_kinds = "normal,"` mean `["normal"]`
/// rather than a list containing an unparseable blank. An entirely empty value
/// therefore yields an empty list, which `check_field_present` has already
/// refused, so no admitted entry can carry one.
fn split_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

// ── Field rules ─────────────────────────────────────────────────────────────

/// Tests whether `reason` is a sentence that says something.
///
/// Three rules, all of which must hold: at least 24 characters after trimming,
/// a trailing full stop, at least four whitespace-separated words, and, after
/// stripping the full stop and normalising, the text must not simply repeat the
/// crate's name. The floor is deliberately low enough to pass any honest
/// justification and high enough to fail `reason = "needed"` or
/// `reason = "serde"`.
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

/// Tests the `YYYY-MM-DD` shape byte by byte.
///
/// The length is checked before any index, so the fixed offsets below cannot
/// panic; only the separator positions and digits are examined, and no calendar
/// validation is attempted. The parameter is named `value` because the same
/// check is applied to whatever `approved_on` held.
pub(crate) fn is_iso_date(value: &str) -> bool {
    let date_bytes = value.as_bytes();
    date_bytes.len() == 10
        && date_bytes[4] == b'-'
        && date_bytes[7] == b'-'
        && [0, 1, 2, 3, 5, 6, 8, 9]
            .iter()
            .all(|&position| date_bytes[position].is_ascii_digit())
}

/// Cargo treats `-` and `_` as interchangeable in package names; so does this.
///
/// Trims, lowercases, and maps `-` to `_`. Used for crate names and owners, not
/// for capability strings, which are compared verbatim.
fn normalise(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace('-', "_")
}

// ── Line reading ────────────────────────────────────────────────────────────

/// Drops a trailing `#` comment, respecting quotes so a `#` inside a reason or
/// a review URL survives.
///
/// Quote tracking is deliberately shallow: a backslash escapes the next byte
/// only while inside quotes, so an odd number of quotes leaves the rest of the
/// line quoted and the `#` is kept rather than truncated. Erring toward keeping
/// the `#` is the safe direction: the value then fails validation visibly
/// instead of being silently cut short.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_quotes = false;
    let mut escaped = false;
    for (index, &byte) in bytes.iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        match byte {
            b'\\' if in_quotes => escaped = true,
            b'"' => in_quotes = !in_quotes,
            b'#' if !in_quotes => return &line[..index],
            _ => {}
        }
    }
    line
}

/// Splits a `key = value` line, returning `None` for anything else.
///
/// The key must be non-empty and made only of ASCII alphanumerics and `_`, which
/// is what makes a malformed line a refusal rather than a silently ignored key.
/// Splitting is on the first `=`, so a value may itself contain `=`: a review
/// URL or an extra constraint needs no escaping.
fn split_pair(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.split_once('=')?;
    let key = key.trim();
    if key.is_empty()
        || !key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return None;
    }
    Some((key, value.trim()))
}

/// Removes one layer of double quotes if, and only if, the value is wrapped in
/// them.
///
/// A value with only a leading or only a trailing quote is returned unchanged,
/// so the malformed form is visible in the refusal message rather than silently
/// half-stripped. No escape sequences are interpreted.
fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|quoted| quoted.strip_suffix('"'))
        .unwrap_or(value)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// What every test here returns.
    ///
    /// The workspace forbids `unwrap`/`expect` outright, with no test exemption,
    /// so a test propagates its failure with `?` rather than aborting the run.
    type TestResult = Result<(), Box<dyn std::error::Error>>;

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
                "approved_by = \"reviewer\"\n",
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
    fn a_complete_entry_parses() -> TestResult {
        let contract = Contract::parse(&entry(""))?;
        assert_eq!(contract.entries.len(), 1);
        let parsed = &contract.entries[0];
        assert_eq!(parsed.krate, "serde");
        assert_eq!(parsed.tier, Tier::Boundary);
        assert_eq!(parsed.version, "1.0");
        assert_eq!(parsed.owner, "lgwks_std");
        assert_eq!(parsed.capability, "json.serialization");
        assert_eq!(parsed.allowed_consumers, ["lgwks_std"]);
        assert!(parsed.reason.ends_with('.'));
        assert_eq!(parsed.approved_by, "reviewer");
        assert_eq!(parsed.approved_on, "2026-08-19");
        assert_eq!(parsed.review, "docs/ADMISSION.md");
        Ok(())
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() -> TestResult {
        let input = concat!(
            "# A top-level comment\n",
            "\n",
            "[policy] # inline\n",
            "enforce = true\n",
            "\n",
        );
        let contract = Contract::parse(&complete(input))?;
        assert!(contract.enforce);
        assert!(contract.entries.is_empty());
        Ok(())
    }

    #[test]
    fn a_hash_inside_a_quoted_value_survives() -> TestResult {
        let input = concat!(
            "[[approved]]\n",
            "crate = \"serde\"\n",
            "tier = \"boundary\"\n",
            "version = \"1.0\"\n",
            "reason = \"Derive-based serialization needs compiler introspection std does not expose.\"\n",
            "approved_by = \"reviewer\"\n",
            "approved_on = \"2026-08-19\"\n",
            "review = \"https://example.com/pr#123\"\n",
        );
        let contract = Contract::parse(&complete(input))?;
        assert_eq!(contract.entries[0].review, "https://example.com/pr#123");
        Ok(())
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
            "approved_by = \"reviewer\"\n",
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
            "approved_by = \"reviewer\"\n",
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
            "approved_by = \"reviewer\"\n",
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
            "approved_by = \"reviewer\"\n",
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
            "approved_by = \"reviewer\"\n",
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
    fn enforcement_defaults_to_on_when_no_policy_is_written() -> TestResult {
        let contract = Contract::parse(&entry(""))?;
        assert!(contract.enforce);
        Ok(())
    }

    #[test]
    fn policy_can_stand_enforcement_down_for_adoption() -> TestResult {
        let input = format!("[policy]\nenforce = false\n\n{}", entry(""));
        let contract = Contract::parse(&input)?;
        assert!(!contract.enforce);
        Ok(())
    }

    /// The exact spellings the closed grammar admits. `true` and `false` are
    /// the only two tokens; everything else is refused below.
    #[test]
    fn the_boolean_grammar_admits_exactly_true_and_false() -> TestResult {
        let on = Contract::parse("[policy]\nenforce = true\n")?;
        assert!(on.enforce, "`enforce = true` must leave enforcement on");
        let off = Contract::parse("[policy]\nenforce = false\n")?;
        assert!(!off.enforce, "`enforce = false` is adoption mode");
        Ok(())
    }

    /// Issue 45's three literal inputs plus the neighbouring spellings a human
    /// actually writes. Each one used to be read as `false`, standing the gate
    /// down without a diagnostic; each must now be a typed refusal that names
    /// the line.
    #[test]
    fn a_malformed_enforce_value_is_refused_rather_than_read_as_false() {
        // Surrounding whitespace is the reader's, not the value's: `split_pair`
        // trims it, so `enforce =   true` is the legal spelling with padding and
        // is covered by the positive test above.
        for token in [
            "True",
            "FALSE",
            "\"true\"",
            "\"false\"",
            "1",
            "0",
            "yes",
            "on",
            "",
            "true.",
            "true;",
        ] {
            let input = format!("[policy]\nenforce = {token}\n");
            assert_eq!(
                Contract::parse(&input),
                Err(ContractError::BadPolicyValue {
                    line: 2,
                    key: "enforce".into(),
                    value: token.into(),
                }),
                "`enforce = {token}` must be refused, not silently read as false"
            );
        }
    }

    /// A trailing comment is stripped before the token is read, so the two
    /// legal spellings keep working with one written beside them.
    #[test]
    fn a_trailing_comment_does_not_change_the_policy_token() -> TestResult {
        let contract = Contract::parse("[policy]\nenforce = false # adoption\n")?;
        assert!(!contract.enforce);
        Ok(())
    }

    #[test]
    fn a_repeated_policy_key_is_refused() {
        assert_eq!(
            Contract::parse("[policy]\nenforce = true\nenforce = false\n"),
            Err(ContractError::DuplicatePolicyKey {
                line: 3,
                key: "enforce".into(),
            })
        );
    }

    #[test]
    fn a_repeated_policy_section_is_refused() {
        assert_eq!(
            Contract::parse("[policy]\nenforce = true\n[policy]\nenforce = false\n"),
            Err(ContractError::DuplicatePolicySection { line: 3 })
        );
    }

    #[test]
    fn a_policy_block_after_the_entries_is_still_read() -> TestResult {
        let input = format!("{}\n[policy]\nenforce = false\n", entry(""));
        let contract = Contract::parse(&input)?;
        assert!(!contract.enforce);
        Ok(())
    }

    #[test]
    fn lookup_tolerates_hyphen_underscore_drift() -> TestResult {
        let contract = Contract::parse(&entry(""))?;
        assert!(contract.approval_for("serde").is_some());
        Ok(())
    }
}
