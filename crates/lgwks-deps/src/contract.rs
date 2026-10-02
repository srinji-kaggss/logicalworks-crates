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
    pub(crate) krate: String,
    /// Which tier the approval sits in.
    pub(crate) tier: Tier,
    /// Approved Cargo manifest requirement, exactly as metadata reports it.
    pub(crate) version: String,
    /// Workspace crate responsible for this external capability.
    pub(crate) owner: String,
    /// Stable semantic capability supplied by the dependency.
    pub(crate) capability: String,
    /// Admitted Cargo source class: `registry`, `git`, or `path`.
    pub(crate) source: String,
    /// Workspace crates permitted to declare this edge directly.
    pub(crate) allowed_consumers: Vec<String>,
    /// Permitted edge kinds: `normal`, `build`, and/or `dev`.
    pub(crate) allowed_kinds: Vec<String>,
    /// One sentence naming what the standard library cannot do.
    pub(crate) reason: String,
    /// The human who approved it.
    pub(crate) approved_by: String,
    /// ISO date of approval.
    pub(crate) approved_on: String,
    /// Path or URL to the evidence behind the approval.
    pub(crate) review: String,
    /// Line where the entry opened, for diagnosis.
    pub(crate) line: usize,
}

/// The parsed register.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Contract {
    /// When false, the register declares itself to be in adoption mode.
    ///
    /// This is a reviewable posture, not an off switch: flipping it is a diff in
    /// the register itself, never an environment variable a process can set for
    /// itself. It does **not** make refusals non-fatal. `audit_direct` refuses a
    /// stand-down outright when the tree carries violations, so the recorded
    /// value reports which posture the register was read under while the verdict
    /// stays a function of the refusals (#204).
    pub enforce: bool,
    /// Canonical repository URL whose workspace members are local authority.
    pub repository: Option<String>,
    /// Every approved dependency.
    pub(crate) entries: Vec<Entry>,
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
        /// One-based line where the field was written.
        line: usize,
        /// The offending value.
        value: String,
    },
    /// A string value is outside the supported TOML basic-string subset.
    InvalidString {
        /// One-based line number.
        line: usize,
        /// The field that carried the value.
        key: String,
        /// The authored value, including quotes.
        value: String,
        /// The syntax rule that was not met.
        reason: &'static str,
    },
    /// A repeated field key makes an entry ambiguous.
    DuplicateEntryKey {
        /// Repeated field name.
        key: String,
        /// Repeated table header, such as `[[approved]]`.
        entry_header: String,
        /// One-based entry ordinal in this register.
        entry_index: usize,
        /// One-based line where the table opened.
        entry_line: usize,
        /// One-based line of the first assignment.
        first_line: usize,
        /// One-based line of the duplicate assignment.
        duplicate_line: usize,
    },
    /// A decoded field value does not satisfy the schema's closed vocabulary.
    InvalidField {
        /// The entry's crate name.
        krate: String,
        /// The field that failed validation.
        field: &'static str,
        /// One-based line where the field was written.
        line: usize,
        /// Decoded offending value.
        value: String,
        /// Accepted value shape.
        expected: &'static str,
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

/// Renders `ContractError::BadDate` at the field's source location.
fn fmt_bad_date(
    formatter: &mut fmt::Formatter<'_>,
    krate: &str,
    line: usize,
    value: &str,
) -> fmt::Result {
    write!(
        formatter,
        "line {line}: approval for {krate:?} has invalid approved_on {value:?}; want a real YYYY-MM-DD date"
    )
}

/// Renders a strict-string refusal at its source line and field.
fn fmt_invalid_string(
    formatter: &mut fmt::Formatter<'_>,
    line: usize,
    key: &str,
    value: &str,
    reason: &str,
) -> fmt::Result {
    write!(
        formatter,
        "line {line}: {key} value {value:?} is not supported TOML: {reason}"
    )
}

/// Renders both locations and the repeated table identity for an ambiguous key.
fn fmt_duplicate_entry_key(
    formatter: &mut fmt::Formatter<'_>,
    key: &str,
    entry_header: &str,
    entry_index: usize,
    entry_line: usize,
    first_line: usize,
    duplicate_line: usize,
) -> fmt::Result {
    write!(
        formatter,
        "line {duplicate_line}: duplicate key {key:?} in {entry_header} entry #{entry_index} opened at line {entry_line}; first assignment is at line {first_line}"
    )
}

/// Renders an offending decoded value with its field and source line.
fn fmt_invalid_field(
    formatter: &mut fmt::Formatter<'_>,
    krate: &str,
    field: &str,
    line: usize,
    value: &str,
    expected: &str,
) -> fmt::Result {
    write!(
        formatter,
        "line {line}: approval for {krate:?} has invalid {field} value {value:?}; {expected}"
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
                line,
                ref value,
            } => fmt_bad_date(formatter, krate, line, value),
            Self::InvalidString {
                line,
                ref key,
                ref value,
                reason,
            } => fmt_invalid_string(formatter, line, key, value, reason),
            Self::DuplicateEntryKey {
                ref key,
                ref entry_header,
                entry_index,
                entry_line,
                first_line,
                duplicate_line,
            } => fmt_duplicate_entry_key(
                formatter,
                key,
                entry_header,
                entry_index,
                entry_line,
                first_line,
                duplicate_line,
            ),
            Self::InvalidField {
                ref krate,
                field,
                line,
                ref value,
                expected,
            } => fmt_invalid_field(formatter, krate, field, line, value, expected),
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
/// Fields are kept in source order with their locations, so duplicate
/// diagnostics can name both assignments without adding a TOML or map crate.
#[derive(Default)]
pub(crate) struct RawEntry {
    /// One-based line where the `[[approved]]` header opened. Diagnostics for
    /// the whole block point here, since individual fields carry no lines.
    line: usize,
    /// One-based ordinal among repeated tables in this register.
    index: usize,
    /// Decoded key/value pairs and their source lines.
    fields: Vec<RawField>,
    /// Header that identifies this register's entry type.
    entry_header: String,
}

/// One schema-approved field with decoded value and source location.
struct RawField {
    /// Key selected from the register-specific schema.
    key: &'static str,
    /// Decoded TOML basic-string value.
    value: String,
    /// One-based source line.
    line: usize,
}

impl RawEntry {
    /// Returns the value written for `key`, or `None` if the block never
    /// carried it. An empty string is returned as `Some("")`: presence and
    /// non-emptiness are separate questions, and `check_field_present` is what
    /// rejects the blank case.
    pub(crate) fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|field| field.key == key)
            .map(|field| field.value.as_str())
    }

    /// Returns a field's source line after successful syntax admission.
    pub(crate) fn field_line(&self, key: &str) -> Option<usize> {
        self.fields
            .iter()
            .find(|field| field.key == key)
            .map(|field| field.line)
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
            index: drafts.len().saturating_add(1),
            fields: Vec::new(),
            entry_header: entry_header.to_owned(),
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

/// Decodes a single-line TOML v1.0 basic string used by either register schema.
///
/// Every TOML escape is recognised, but a register value may not contain a
/// control character, raw or decoded: `\b`, `\t`, `\n`, `\f`, `\r` and any
/// `\u`/`\U` escape naming a control character are refused with their own
/// reason rather than as unknown escapes. What a value can carry is therefore
/// printable text plus `\"`, `\\` and non-control Unicode escapes. This is
/// stricter than TOML, which admits a raw tab, and it fails closed. Literal and
/// multiline strings are outside this register subset and refused.
fn decode_string(value: &str, key: &str, line: usize) -> Result<String, ContractError> {
    let invalid = |reason| ContractError::InvalidString {
        line,
        key: key.to_owned(),
        value: value.to_owned(),
        reason,
    };
    let Some(body) = value.strip_prefix('"') else {
        return Err(invalid("expected a double-quoted basic string"));
    };
    let mut decoded = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '"' {
            if chars.peek().is_some() {
                return Err(invalid("trailing tokens after the closing quote"));
            }
            if decoded.chars().any(char::is_control) {
                return Err(invalid("decoded control characters are not supported"));
            }
            return Ok(decoded);
        }
        if ch.is_control() {
            return Err(invalid("control characters are not supported"));
        }
        if ch != '\\' {
            decoded.push(ch);
            continue;
        }
        let Some(escaped) = chars.next() else {
            return Err(invalid("incomplete escape sequence"));
        };
        match escaped {
            'b' => decoded.push('\u{0008}'),
            't' => decoded.push('\t'),
            'n' => decoded.push('\n'),
            'f' => decoded.push('\u{000c}'),
            'r' => decoded.push('\r'),
            '"' => decoded.push('"'),
            '\\' => decoded.push('\\'),
            'u' | 'U' => {
                let digits = if escaped == 'u' { 4 } else { 8 };
                let mut scalar = 0_u32;
                for _ in 0..digits {
                    let Some(digit) = chars.next() else {
                        return Err(invalid("incomplete Unicode escape"));
                    };
                    let Some(hex) = digit.to_digit(16) else {
                        return Err(invalid("invalid hexadecimal Unicode escape"));
                    };
                    scalar = scalar
                        .checked_mul(16)
                        .and_then(|value| value.checked_add(hex))
                        .ok_or_else(|| invalid("Unicode escape is outside the scalar range"))?;
                }
                let Some(decoded_char) = char::from_u32(scalar) else {
                    return Err(invalid("Unicode escape is not a scalar value"));
                };
                decoded.push(decoded_char);
            }
            _ => return Err(invalid("unsupported escape sequence")),
        }
    }
    Err(invalid("missing closing quote"))
}

/// Applies one key/value pair written inside `[policy]`.
///
/// A repeated key is refused rather than overwritten, keeping the enforcement
/// flag independent of line order. `repository` uses the shared strict string
/// decoder rather than stripping quotes without interpreting escapes.
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
            let decoded = decode_string(value, key, line_no)?;
            seen_keys.push(key.to_owned());
            *repository = Some(decoded);
            Ok(())
        }
    }
}

/// Applies one schema-approved key/value pair to the current repeated entry.
///
/// Repeated keys and unsupported string spellings are refusals, not precedence
/// rules, so line order cannot change the effective approval.
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
    if let Some(previous) = draft.fields.iter().find(|field| field.key == *known) {
        return Err(ContractError::DuplicateEntryKey {
            key: key.to_owned(),
            entry_header: draft.entry_header.clone(),
            entry_index: draft.index,
            entry_line: draft.line,
            first_line: previous.line,
            duplicate_line: line_no,
        });
    }
    let decoded = decode_string(value, key, line_no)?;
    draft.fields.push(RawField {
        key: known,
        value: decoded,
        line: line_no,
    });
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
    /// Number of reviewed dependency approvals in this register.
    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

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

/// Requires `approved_on` to be a real Gregorian `YYYY-MM-DD` date.
///
/// The field makes an approval attributable to a review, not a scheduled event.
fn validate_date(approved_on: &str, krate: &str, line: usize) -> Result<(), ContractError> {
    if is_iso_date(approved_on) {
        Ok(())
    } else {
        Err(ContractError::BadDate {
            krate: krate.to_owned(),
            line,
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

/// Checks Cargo's portable package/owner identifier subset.
fn is_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

/// Refuses a decoded field that is outside its schema vocabulary.
fn validate_closed_value(
    draft: &RawEntry,
    krate: &str,
    field: &'static str,
    value: &str,
    valid: bool,
    expected: &'static str,
) -> Result<(), ContractError> {
    if valid {
        Ok(())
    } else {
        Err(ContractError::InvalidField {
            krate: krate.to_owned(),
            field,
            line: draft.field_line(field).unwrap_or(draft.line),
            value: value.to_owned(),
            expected,
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
    validate_date(
        &approved_on,
        &krate,
        draft.field_line("approved_on").unwrap_or(draft.line),
    )?;
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
    validate_closed_value(
        draft,
        &krate,
        "crate",
        &krate,
        is_identifier(&krate),
        "expected a Cargo package identifier using ASCII letters, digits, hyphens or underscores",
    )?;
    validate_closed_value(
        draft,
        &krate,
        "owner",
        &owner,
        is_identifier(&owner),
        "expected a workspace package identifier using ASCII letters, digits, hyphens or underscores",
    )?;
    validate_closed_value(
        draft,
        &krate,
        "source",
        &source,
        matches!(source.as_str(), "registry" | "git" | "path"),
        "expected one of registry, git or path",
    )?;
    validate_closed_value(
        draft,
        &krate,
        "allowed_consumers",
        &allowed_consumers.join(","),
        !allowed_consumers.is_empty() && allowed_consumers.iter().all(|item| is_identifier(item)),
        "expected a nonempty comma-separated list of package identifiers",
    )?;
    validate_closed_value(
        draft,
        &krate,
        "allowed_kinds",
        &allowed_kinds.join(","),
        !allowed_kinds.is_empty()
            && allowed_kinds
                .iter()
                .all(|kind| matches!(kind.as_str(), "normal" | "build" | "dev")),
        "expected a nonempty list containing only normal, build or dev",
    )?;
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

/// Splits a comma-separated list, retaining empty members for schema refusal.
///
/// One final comma remains a supported legacy terminator; interior empty
/// members are never discarded because they would hide malformed lists.
fn split_csv(value: &str) -> Vec<String> {
    let mut items = value.split(',').map(str::trim).collect::<Vec<_>>();
    if value.trim_end().ends_with(',') && items.last().is_some_and(|item| item.is_empty()) {
        items.pop();
    }
    items.into_iter().map(str::to_owned).collect()
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

/// Validates a canonical Gregorian `YYYY-MM-DD` date without allocation.
///
/// Length is checked before indexing; decimal fields are bounded by the fixed
/// format, and month length follows the Gregorian leap-year rule.
pub(crate) fn is_iso_date(value: &str) -> bool {
    let date_bytes = value.as_bytes();
    if date_bytes.len() != 10
        || date_bytes[4] != b'-'
        || date_bytes[7] != b'-'
        || ![0, 1, 2, 3, 5, 6, 8, 9]
            .iter()
            .all(|&position| date_bytes[position].is_ascii_digit())
    {
        return false;
    }
    let year = decimal_pair(&date_bytes[0..4]);
    let month = decimal_pair(&date_bytes[5..7]);
    let day = decimal_pair(&date_bytes[8..10]);
    let (Some(year), Some(month), Some(day)) = (year, month, day) else {
        return false;
    };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    };
    year > 0 && day > 0 && day <= month_days
}

/// Parses an ASCII decimal byte slice without allocation.
fn decimal_pair(bytes: &[u8]) -> Option<u32> {
    bytes.iter().try_fold(0_u32, |value, byte| {
        value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u32::from(byte.checked_sub(b'0')?)))
    })
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

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as FmtWrite;

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
    fn duplicate_entry_keys_refuse_both_identical_and_conflicting_values() -> TestResult {
        let assignments = [
            ("crate", "\"serde\"", "\"another-crate\""),
            ("tier", "\"boundary\"", "\"vendor\""),
            ("version", "\"1.0\"", "\"2.0\""),
            ("owner", "\"lgwks_std\"", "\"lgwks_deps\""),
            (
                "capability",
                "\"json.serialization\"",
                "\"other.capability\"",
            ),
            ("source", "\"registry\"", "\"git\""),
            ("allowed_consumers", "\"lgwks_std\"", "\"lgwks_deps\""),
            ("allowed_kinds", "\"normal\"", "\"dev\""),
            (
                "reason",
                "\"Derive-based serialization needs compiler introspection std does not expose.\"",
                "\"A different capability has a distinct boundary.\"",
            ),
            ("approved_by", "\"reviewer\"", "\"another-reviewer\""),
            ("approved_on", "\"2026-08-19\"", "\"2026-08-20\""),
            ("review", "\"docs/ADMISSION.md\"", "\"docs/OTHER.md\""),
        ];
        for (key, identical, conflicting) in assignments {
            let original = entry("");
            let reversed = original.replace(
                &format!("{key} = {identical}"),
                &format!("{key} = {conflicting}"),
            );
            for (base, value) in [
                (original.as_str(), identical),
                (original.as_str(), conflicting),
                (reversed.as_str(), identical),
            ] {
                let first_line = base
                    .lines()
                    .position(|line| line.starts_with(&format!("{key} =")))
                    .map(|index| index + 1)
                    .ok_or_else(|| format!("fixture has no {key} assignment"))?;
                let duplicate_line = base.lines().count() + 1;
                let input = format!("{base}{key} = {value}\n");
                assert_eq!(
                    Contract::parse(&input),
                    Err(ContractError::DuplicateEntryKey {
                        key: key.to_owned(),
                        entry_header: "[[approved]]".to_owned(),
                        entry_index: 1,
                        entry_line: 1,
                        first_line,
                        duplicate_line,
                    }),
                    "duplicate {key} with {value} must refuse both positions"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn unique_fields_are_order_independent() -> TestResult {
        let original = entry("");
        let mut lines = original.lines();
        let header = lines.next().ok_or("entry fixture has no table header")?;
        let mut lines = lines.collect::<Vec<_>>();
        lines.reverse();
        let reordered = format!("{header}\n{}\n", lines.join("\n"));
        assert_eq!(
            Contract::parse(&original)?,
            Contract::parse(&reordered)?,
            "reordering unique fields must not change the admitted contract"
        );
        Ok(())
    }

    #[test]
    fn basic_string_subset_decodes_escapes_comments_unicode_and_crlf() -> TestResult {
        let input = entry("").replace(
            "review = \"docs/ADMISSION.md\"",
            r#"review = "docs/\u0041#é" # trailing comment"#,
        );
        let parsed = Contract::parse(&input)?;
        assert_eq!(
            parsed.entries[0].review, "docs/A#é",
            "basic Unicode escapes, non-ASCII text and quoted hashes must decode"
        );
        let escaped = entry("").replace(
            "review = \"docs/ADMISSION.md\"",
            r#"review = "docs/\"quoted\"\\literal""#,
        );
        assert_eq!(
            Contract::parse(&escaped)?.entries[0].review,
            "docs/\"quoted\"\\literal",
            "quote and backslash escapes must decode without reinterpretation"
        );
        let crlf = entry("").replace('\n', "\r\n");
        assert_eq!(
            Contract::parse(&crlf)?.entries.len(),
            1,
            "CRLF input must preserve the same one-entry register"
        );
        Ok(())
    }

    #[test]
    fn unsupported_string_forms_refuse_at_the_authored_field() {
        let cases = [
            ("approved_by = \"reviewer\"", "approved_by = reviewer"),
            ("approved_by = \"reviewer\"", "approved_by = \"reviewer"),
            (
                "approved_by = \"reviewer\"",
                "approved_by = \"reviewer\\q\"",
            ),
            (
                "approved_by = \"reviewer\"",
                "approved_by = \"reviewer\" trailing",
            ),
            (
                "approved_by = \"reviewer\"",
                "approved_by = \"\"\"reviewer\"\"\"",
            ),
            (
                "approved_by = \"reviewer\"",
                "approved_by = \"reviewer\\n\"",
            ),
        ];
        for (from, to) in cases {
            let input = entry("").replace(from, to);
            assert!(
                matches!(
                    Contract::parse(&input),
                    Err(ContractError::InvalidString {
                        line: 11,
                        ref key,
                        ..
                    }) if key == "approved_by"
                ),
                "unsupported string form {to:?} must give a typed field refusal"
            );
        }
        let raw_control = format!("approved_by = \"reviewer{}\"", '\u{0001}');
        let input = entry("").replace("approved_by = \"reviewer\"", &raw_control);
        assert!(
            matches!(
                Contract::parse(&input),
                Err(ContractError::InvalidString {
                    line: 11,
                    ref key,
                    ..
                }) if key == "approved_by"
            ),
            "a raw control character must be refused at its authored field"
        );
    }

    #[test]
    fn decoded_schema_values_and_calendar_dates_are_closed() -> TestResult {
        let invalid = [
            (
                "allowed_consumers = \"lgwks_std\"",
                "allowed_consumers = \",,,\"",
                "allowed_consumers",
                8,
            ),
            (
                "allowed_kinds = \"normal\"",
                "allowed_kinds = \",,,\"",
                "allowed_kinds",
                9,
            ),
            (
                "allowed_kinds = \"normal\"",
                "allowed_kinds = \"normal,unknown\"",
                "allowed_kinds",
                9,
            ),
            ("source = \"registry\"", "source = \"unknown\"", "source", 7),
            ("crate = \"serde\"", "crate = \"bad package\"", "crate", 2),
            ("owner = \"lgwks_std\"", "owner = \"bad owner\"", "owner", 5),
        ];
        for (from, to, field, line) in invalid {
            let input = entry("").replace(from, to);
            assert!(
                matches!(
                    Contract::parse(&input),
                    Err(ContractError::InvalidField { field: found, line: found_line, .. })
                        if found == field && found_line == line
                ),
                "invalid decoded {field} must be refused at line {line}"
            );
        }
        for date in ["2026-13-45", "1900-02-29", "2024-04-31", "0000-01-01"] {
            let input = entry("").replace("2026-08-19", date);
            assert!(
                matches!(
                    Contract::parse(&input),
                    Err(ContractError::BadDate { line: 12, .. })
                ),
                "impossible date {date} must be refused"
            );
        }
        for date in ["2024-02-29", "2000-02-29", "2026-08-19"] {
            let input = entry("").replace("2026-08-19", date);
            assert_eq!(
                Contract::parse(&input)?.entries[0].approved_on,
                date,
                "valid date {date} must remain supported"
            );
        }
        Ok(())
    }

    #[test]
    fn a_single_legacy_trailing_list_comma_remains_supported() -> TestResult {
        let input = entry("")
            .replace(
                "allowed_consumers = \"lgwks_std\"",
                "allowed_consumers = \"lgwks_std,\"",
            )
            .replace("allowed_kinds = \"normal\"", "allowed_kinds = \"normal,\"");
        let parsed = Contract::parse(&input)?;
        assert_eq!(
            parsed.entries[0].allowed_consumers,
            ["lgwks_std"],
            "one final comma remains a delimiter rather than an empty consumer"
        );
        assert_eq!(
            parsed.entries[0].allowed_kinds,
            ["normal"],
            "one final comma remains a delimiter rather than an empty kind"
        );
        Ok(())
    }

    #[test]
    fn invariant_register_uses_the_shared_duplicate_key_refusal() -> TestResult {
        let fields = [
            ("id", "\"INV-DEP-TEST\"", "\"INV-DEP-OTHER\""),
            (
                "statement",
                "\"The implementation preserves this reviewed contract.\"",
                "\"A different invariant has its own exact statement.\"",
            ),
            ("scope", "\"lgwks_deps\"", "\"lgwks_std\""),
            ("owner", "\"lgwks_deps\"", "\"lgwks_bot\""),
            ("enforcement", "\"static-check\"", "\"monitor\""),
            ("enforced_by", "\"src/lib.rs\"", "\"src/contract.rs\""),
            ("approved_by", "\"reviewer\"", "\"second-reviewer\""),
            ("approved_on", "\"2026-09-20\"", "\"2026-09-21\""),
            ("review", "\"src/lib.rs\"", "\"src/contract.rs\""),
            ("evidence_revision", "\"abcd1234\"", "\"1234abcd\""),
            (
                "evidence_invocation",
                "\"cargo check --locked\"",
                "\"cargo nextest run --locked\"",
            ),
            ("evidence_result", "\"pass\"", "\"fail\""),
        ];
        for (key, original, conflicting) in fields {
            let mut base = String::from("[[invariant]]\n");
            for (field, value, _) in fields {
                writeln!(base, "{field} = {value}")?;
            }
            let reversed = base.replace(
                &format!("{key} = {original}"),
                &format!("{key} = {conflicting}"),
            );
            for (document, duplicate) in [
                (base.as_str(), original),
                (base.as_str(), conflicting),
                (reversed.as_str(), original),
            ] {
                let first_line = document
                    .lines()
                    .position(|line| line.starts_with(&format!("{key} =")))
                    .map(|index| index + 1)
                    .ok_or_else(|| format!("fixture has no invariant {key}"))?;
                let duplicate_line = document.lines().count() + 1;
                let input = format!("{document}{key} = {duplicate}\n");
                let error = crate::invariants::Register::parse(&input)
                    .err()
                    .ok_or_else(|| format!("invariant parser accepted duplicate {key}"))?;
                let diagnostic = error.to_string();
                assert!(
                    diagnostic.contains(&format!("first assignment is at line {first_line}")),
                    "invariant duplicate {key} must retain the first source position: {diagnostic}"
                );
                assert!(
                    diagnostic.contains(&format!("line {duplicate_line}"))
                        && diagnostic.contains("duplicate key")
                        && diagnostic.contains(key),
                    "invariant duplicate {key} must identify the key and second position: {diagnostic}"
                );
            }
        }
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
                line: 12,
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
