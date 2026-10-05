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
///
/// Public so a surface freeze can be expressed in the register's own vocabulary
/// rather than as a second list of crate names maintained beside it; see
/// [`crate::audit_direct`].
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
    /// SPDX expression the approval was granted under, verbatim as Cargo
    /// reports it for the package.
    ///
    /// Required rather than optional: an approval that does not say which
    /// licence was reviewed cannot be re-checked when the licence changes
    /// upstream, which is the drift this field exists to catch (#208).
    pub(crate) license: String,
    /// Admitted Cargo source class: `registry`, `git`, or `path`.
    pub(crate) source: String,
    /// Admitted origin identity for the source class: a complete Cargo
    /// registry source, a Git repository plus its admitted revision/reference
    /// policy, or an external path authority.
    ///
    /// `None` is a legacy class-only approval. It grants no exact-origin
    /// assurance beyond the one deterministic registry this estate uses, and a
    /// git or path edge requires an authored origin before it can be admitted.
    pub(crate) origin: Option<String>,
    /// Workspace crates permitted to declare this edge directly.
    pub(crate) allowed_consumers: Vec<String>,
    /// Permitted edge kinds: `normal`, `build`, and/or `dev`.
    pub(crate) allowed_kinds: Vec<String>,
    /// The complete set of upstream features this edge may enable, when the
    /// entry constrains them. `None` grandfathered the dimension: the entry
    /// predates feature policy and does not refuse a feature it never named.
    pub(crate) features: Option<Vec<String>>,
    /// Features the edge must enable, a subset of [`features`](Self::features)
    /// when that is also authored. `None` requires nothing.
    pub(crate) required_features: Option<Vec<String>>,
    /// The exact authored `default-features` bit this edge must carry, when the
    /// entry constrains it.
    pub(crate) uses_default_features: Option<bool>,
    /// The exact authored optionality this edge must carry, when constrained.
    pub(crate) optional: Option<bool>,
    /// The exact target `cfg(…)` this edge must be scoped to, when
    /// constrained; `""` requires an unconditional declaration.
    pub(crate) target: Option<String>,
    /// Additional accepted spellings of this crate's Cargo package name.
    ///
    /// An explicit, collision-checked compatibility alias: the entry admits an
    /// observed package whose name is `krate` or one of these. It exists because
    /// Cargo folds `-`/`_` when it decides two packages collide, but that fold
    /// is not package identity; an approval written in one spelling must say so
    /// rather than have the fold silently exempt a different package.
    pub(crate) aliases: Vec<String>,
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

impl Entry {
    /// The ladder tier this approval sits in.
    ///
    /// Public for the same reason [`Tier`] is: the gate reads a tier off
    /// an entry to decide whether a surface may be frozen, and an audit that had
    /// to reach into `pub(crate)` fields could not be written at all.
    #[must_use]
    pub fn tier(&self) -> Tier {
        self.tier
    }

    /// Workspace crate that is responsible for this capability.
    ///
    /// Public because the surface freeze in [`crate::audit_direct`] decides who
    /// a re-tiered approval belongs to by owner, and a decision a caller cannot
    /// reach is a decision it cannot make.
    #[must_use]
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// Package name as `Cargo.lock` spells it.
    ///
    /// The companion of [`Entry::owner`] for the same reason: a refusal that
    /// reports a violated freeze has to say which edge it is about.
    #[must_use]
    pub fn krate(&self) -> &str {
        &self.krate
    }

    /// SPDX expression this approval was granted under.
    ///
    /// Public because the licence audit is a decision about a *comparison*: it
    /// asks whether this string still says what the package's own manifest
    /// says. A caller holding only an opaque `Entry` could report that some
    /// approval drifted without being able to say which expression was
    /// expected.
    #[must_use]
    pub fn license(&self) -> &str {
        &self.license
    }

    /// Whether this approval admits the observed Cargo package name `name`.
    ///
    /// Byte-exact against the Cargo-authored identity, plus any explicitly
    /// authored [`aliases`](Self::aliases). There is no implicit `-`/`_` fold:
    /// a fold would let two distinct packages share one authority.
    #[must_use]
    pub(crate) fn admits(&self, name: &str) -> bool {
        self.krate == name || self.aliases.iter().any(|alias| alias == name)
    }
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
    /// Register schema version. Absent means version 1 (a class-only register);
    /// the committed register authors version 2, which adds the exact-origin and
    /// capability-policy keys and the explicit alias list.
    pub(crate) schema: u32,
    /// A stable fingerprint of the register text this `Contract` was parsed
    /// from, so a CLI receipt can bind a run to the exact contract revision it
    /// read. See [`Contract::digest`] for what this does and does not claim.
    pub(crate) digest: String,
    /// The repository's own policy, read from `[policy]`. Nothing in it has a
    /// built-in default: this crate is published, so any value compiled into it
    /// would bind every repository that runs the gate to one repository's
    /// choices, with no way to change them short of a new release.
    pub(crate) policy: Policy,
    /// Every approved dependency.
    pub(crate) entries: Vec<Entry>,
}

/// What a register declares about its own repository, beyond the approvals.
///
/// Each field is the register's decision, and an absent field is a decision
/// not made — never a fallback to some other repository's answer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Policy {
    /// SPDX licence identifiers an external dependency may be under
    /// (`accepted_licenses`). `None` means the register has not said, and the
    /// licence audit refuses rather than guesses (`LicensePolicyUndeclared`).
    pub(crate) accepted_licenses: Option<Vec<String>>,
    /// The closed set of workspace members and approval owners this repository
    /// has (`surfaces`). `None` means the repository keeps no closed set, and no
    /// member or owner is refused for its name.
    pub(crate) surfaces: Option<Vec<String>>,
    /// Members whose approved edge set is closed (`frozen_surfaces`), and the
    /// tier every approval they own must keep claiming (`frozen_tier`). The two
    /// are declared together or not at all.
    pub(crate) frozen: Option<Frozen>,
}

/// The frozen-surface half of [`Policy`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Frozen {
    /// Members whose edges may not grow or be re-tiered.
    pub(crate) surfaces: Vec<String>,
    /// The tier their approvals must keep.
    pub(crate) tier: Tier,
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
    /// A `[policy]` key that only means something beside another was written
    /// without it: `frozen_surfaces` names what is frozen and `frozen_tier` what
    /// it is frozen at, and either alone is half a rule.
    IncompletePolicy {
        /// The key that was written.
        present: &'static str,
        /// The key it needs beside it.
        missing: &'static str,
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
    /// A register schema version this build does not implement.
    UnsupportedSchema {
        /// One-based line of the `schema` assignment.
        line: usize,
        /// The version that was written.
        value: String,
    },
    /// An alias a second identity also claims.
    ///
    /// An alias is a compatibility spelling, so exactly one Cargo package may
    /// own it; two packages sharing one alias would share one authority, which
    /// is the defect the explicit alias exists to avoid.
    AliasCollision {
        /// The contested spelling.
        alias: String,
        /// The package that already owns it.
        owner: String,
        /// The one-based line where the entry claiming it again opened.
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

/// Renders `ContractError::UnsupportedSchema`, naming the versions this build
/// can read so the repair is "migrate or downgrade", not "guess".
fn fmt_unsupported_schema(
    formatter: &mut fmt::Formatter<'_>,
    line: usize,
    value: &str,
) -> fmt::Result {
    write!(
        formatter,
        "line {line}: register schema {value:?} is not supported; this build reads schema 1 and 2"
    )
}

/// Renders `ContractError::AliasCollision`, naming both the contested spelling
/// and the package that already owns it.
fn fmt_alias_collision(
    formatter: &mut fmt::Formatter<'_>,
    alias: &str,
    owner: &str,
    line: usize,
) -> fmt::Result {
    write!(
        formatter,
        "line {line}: alias {alias:?} is already claimed by {owner:?}; a compatibility alias names exactly one Cargo package"
    )
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
            Self::IncompletePolicy { present, missing } => write!(
                formatter,
                "[policy] declares `{present}` without `{missing}`; the two keys are one rule \
                 and are written together"
            ),
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
            Self::UnsupportedSchema { line, ref value } => {
                fmt_unsupported_schema(formatter, line, value)
            }
            Self::AliasCollision {
                ref alias,
                ref owner,
                line,
            } => fmt_alias_collision(formatter, alias, owner, line),
        }
    }
}

impl Error for ContractError {}

// ── Parsing ─────────────────────────────────────────────────────────────────

/// Every key an `[[approved]]` block must carry, in the order the parser checks
/// them. The order is observable: `validate_required_fields` reports the first
/// absent key, so a register missing several is told about the earliest one and
/// the message is stable across runs.
const REQUIRED: [&str; 13] = [
    "crate",
    "tier",
    "version",
    "owner",
    "capability",
    "license",
    "source",
    "allowed_consumers",
    "allowed_kinds",
    "reason",
    "approved_by",
    "approved_on",
    "review",
];

/// Every key an `[[approved]]` block may carry: the required set plus the
/// optional exact-origin identity and the optional admitted-capability policy.
/// Keeping it separate from `REQUIRED` is what lets an existing class-only,
/// policy-free block keep parsing while a block that writes `origin`,
/// `features` or `target` has it validated and compared.
const ENTRY_KEYS: [&str; 20] = [
    "crate",
    "tier",
    "version",
    "owner",
    "capability",
    "license",
    "source",
    "origin",
    "features",
    "required_features",
    "uses_default_features",
    "optional",
    "target",
    "aliases",
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
pub(crate) struct RawField {
    /// Key selected from the register-specific schema.
    pub(crate) key: &'static str,
    /// Decoded TOML basic-string value.
    value: String,
    /// One-based source line.
    pub(crate) line: usize,
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
    /// Register schema version, defaulting to 1 when `[policy] schema` is absent.
    schema: u32,
    /// Repository-policy declarations, decoded later by the register that owns them.
    policy_fields: Vec<RawField>,
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
            let refusal = Err(ContractError::DuplicatePolicySection { line: line_no });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "handle_section_header: returning an error to the caller");
            return refusal;
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

/// Every scalar key the `[policy]` block defines.
const POLICY_KEYS: [&str; 3] = ["enforce", "repository", "schema"];

/// The repository-policy keys a dependency register may declare in `[policy]`.
///
/// Read as strings and decoded by [`Contract::parse`], because what they mean
/// is the dependency register's: the invariant register shares the line reader
/// and refuses every one of them.
const POLICY_DECLARATION_KEYS: [&str; 4] = [
    "accepted_licenses",
    "surfaces",
    "frozen_surfaces",
    "frozen_tier",
];

/// The register schema versions this build reads.
///
/// `1` is a register that authorises a source class and nothing finer; it is
/// still read so an older committed register does not fail to parse. `2` adds
/// the exact-origin and admitted-capability-policy keys and the explicit alias
/// list. An unknown future version is refused rather than read as either.
const SUPPORTED_SCHEMAS: [u32; 2] = [1, 2];

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

/// Decodes `[policy] schema` as an unsigned integer this build implements.
///
/// The token is read bare (an integer, not a string). A version this build does
/// not implement is [`ContractError::UnsupportedSchema`] rather than a default,
/// so a register written for a later schema cannot be read under the older
/// rules and silently lose the keys that schema added.
fn decode_schema(value: &str, line_no: usize) -> Result<u32, ContractError> {
    let parsed = value.parse::<u32>().ok();
    match parsed {
        Some(version) if SUPPORTED_SCHEMAS.contains(&version) => Ok(version),
        _ => Err(ContractError::UnsupportedSchema {
            line: line_no,
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
        let refusal = Err(invalid("expected a double-quoted basic string"));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "decode_string: returning an error to the caller");
        return refusal;
    };
    let mut decoded = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '"' {
            if chars.peek().is_some() {
                let refusal = Err(invalid("trailing tokens after the closing quote"));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "decode_string: returning an error to the caller");
                return refusal;
            }
            if decoded.chars().any(char::is_control) {
                let refusal = Err(invalid("decoded control characters are not supported"));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "decode_string: returning an error to the caller");
                return refusal;
            }
            return Ok(decoded);
        }
        if ch.is_control() {
            let refusal = Err(invalid("control characters are not supported"));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "decode_string: returning an error to the caller");
            return refusal;
        }
        if ch != '\\' {
            decoded.push(ch);
            continue;
        }
        let Some(escaped) = chars.next() else {
            let refusal = Err(invalid("incomplete escape sequence"));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "decode_string: returning an error to the caller");
            return refusal;
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
                        let refusal = Err(invalid("incomplete Unicode escape"));
                        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "decode_string: returning an error to the caller");
                        return refusal;
                    };
                    let Some(hex) = digit.to_digit(16) else {
                        let refusal = Err(invalid("invalid hexadecimal Unicode escape"));
                        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "decode_string: returning an error to the caller");
                        return refusal;
                    };
                    scalar = scalar
                        .checked_mul(16)
                        .and_then(|value| value.checked_add(hex))
                        .ok_or_else(|| invalid("Unicode escape is outside the scalar range"))?;
                }
                let Some(decoded_char) = char::from_u32(scalar) else {
                    let refusal = Err(invalid("Unicode escape is not a scalar value"));
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "decode_string: returning an error to the caller");
                    return refusal;
                };
                decoded.push(decoded_char);
            }
            _ => {
                let refusal = Err(invalid("unsupported escape sequence"));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "decode_string: returning an error to the caller");
                return refusal;
            }
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
    reader: &mut Reader<'_>,
) -> Result<(), ContractError> {
    let seen_keys = &mut reader.policy_keys;
    let declaration = POLICY_DECLARATION_KEYS
        .iter()
        .copied()
        .find(|candidate| *candidate == key);
    if !POLICY_KEYS.contains(&key) && declaration.is_none() {
        let refusal = Err(ContractError::UnknownKey {
            line: line_no,
            key: key.to_owned(),
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "apply_policy_pair: returning an error to the caller");
        return refusal;
    }
    if seen_keys.iter().any(|seen| seen == key) {
        let refusal = Err(ContractError::DuplicatePolicyKey {
            line: line_no,
            key: key.to_owned(),
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "apply_policy_pair: returning an error to the caller");
        return refusal;
    }
    if let Some(declared) = declaration {
        let decoded = decode_string(value, key, line_no)?;
        seen_keys.push(key.to_owned());
        reader.policy_fields.push(RawField {
            key: declared,
            value: decoded,
            line: line_no,
        });
        return Ok(());
    }
    match key {
        "enforce" => {
            let decoded = decode_enforce(value, line_no)?;
            seen_keys.push(key.to_owned());
            reader.enforce = decoded;
            Ok(())
        }
        "schema" => {
            let decoded = decode_schema(value, line_no)?;
            seen_keys.push(key.to_owned());
            reader.schema = decoded;
            Ok(())
        }
        _ => {
            let decoded = decode_string(value, key, line_no)?;
            seen_keys.push(key.to_owned());
            reader.repository = Some(decoded);
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
        let refusal = Err(ContractError::DuplicateEntryKey {
            key: key.to_owned(),
            entry_header: draft.entry_header.clone(),
            entry_index: draft.index,
            entry_line: draft.line,
            first_line: previous.line,
            duplicate_line: line_no,
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "apply_entry_pair: returning an error to the caller");
        return refusal;
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
        Section::Policy => apply_policy_pair(key, value, line_no, reader),
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
    /// Register schema version; 1 when absent.
    pub(crate) schema: u32,
    /// `[policy]` declarations from [`POLICY_DECLARATION_KEYS`], in source order.
    pub(crate) policy_fields: Vec<RawField>,
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
        schema: 1,
        policy_fields: Vec::new(),
        entries: Vec::new(),
    };

    for (index, raw) in text.lines().enumerate() {
        process_contract_line(raw, index, &mut reader)?;
    }

    Ok(RawRegister {
        enforce: reader.enforce,
        repository: reader.repository,
        schema: reader.schema,
        policy_fields: reader.policy_fields,
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
    // Identity is byte-exact. Folding `-`/`_` here would let `foo-bar` and
    // `foo_bar` collide into one entry, which is the conflation the exact match
    // in `admits` exists to prevent.
    let duplicate = entries.iter().any(|existing| {
        existing.krate == entry.krate
            && existing.owner == entry.owner
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

/// Refuses an alias that two distinct Cargo packages would share.
///
/// Every entry's exact `krate` is registered first as its own owner, so an
/// alias that equals another package's real name is caught as a collision, and
/// two entries claiming the same alias are caught too. The alias must also be a
/// valid identifier and must not be the entry's own crate name.
fn check_aliases(entries: &[Entry]) -> Result<(), ContractError> {
    let mut owners: std::collections::BTreeMap<&str, &str> = std::collections::BTreeMap::new();
    for entry in entries {
        owners
            .entry(entry.krate.as_str())
            .or_insert(entry.krate.as_str());
    }
    for entry in entries {
        for alias in &entry.aliases {
            match owners.get(alias.as_str()) {
                Some(owner) if *owner != entry.krate => {
                    let refusal = Err(ContractError::AliasCollision {
                        alias: alias.clone(),
                        owner: (*owner).to_owned(),
                        line: entry.line,
                    });
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "check_aliases: returning an error to the caller");
                    return refusal;
                }
                Some(_) => {}
                None => {
                    owners.insert(alias.as_str(), entry.krate.as_str());
                }
            }
        }
    }
    Ok(())
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
        let raw = parse_register(text, "[[approved]]", &ENTRY_KEYS)?;
        let mut entries: Vec<Entry> = Vec::new();
        for draft in &raw.entries {
            let entry = build(draft)?;
            check_duplicate_entry(&entries, &entry, draft.line)?;
            entries.push(entry);
        }
        check_aliases(&entries)?;
        let policy = build_policy(&raw.policy_fields)?;
        Ok(Self {
            enforce: raw.enforce,
            repository: raw.repository,
            schema: raw.schema,
            digest: fingerprint(text),
            policy,
            entries,
        })
    }

    /// The SPDX licence identifiers this register accepts, or `None` when it
    /// has not declared any.
    #[must_use]
    pub fn accepted_licenses(&self) -> Option<&[String]> {
        self.policy.accepted_licenses.as_deref()
    }

    /// The register schema version this contract was written under.
    #[must_use]
    pub const fn schema(&self) -> u32 {
        self.schema
    }

    /// A stable fingerprint of the register text this contract was parsed from.
    ///
    /// It binds a receipt to the exact contract revision a run read: two
    /// different registers fingerprint differently, so a receipt naming a
    /// digest that does not match the committed register is visibly stale.
    ///
    /// This is an identity fingerprint, not an adversarial integrity claim.
    /// `lgwks_std::hash` (BLAKE3) is the estate's cryptographic primitive and is
    /// deliberately not used here: it sits behind a `lgwks_std` feature that
    /// enabling would add a `blake3` edge to the gate's own dependency graph,
    /// which INV-DEP-EDGE-OWNED refuses. A register is human-authored and
    /// reviewable, and this digest detects accidental drift, not a forger who
    /// can already rewrite the validator.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Finds the approval for a resolved Cargo package name.
    ///
    /// Matching is byte-exact against the Cargo-authored package name, plus any
    /// explicitly authored alias. Returns the first match only; a package that
    /// backs several capabilities has several entries, so callers that need all
    /// of them use `approvals_for`.
    #[must_use]
    pub fn approval_for(&self, krate: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.admits(krate))
    }

    /// Returns every semantic approval for an upstream package. A package may
    /// legitimately back distinct capabilities with different owners.
    ///
    /// Order follows the register, so for a given register the sequence is
    /// deterministic and diffable.
    pub fn approvals_for<'a>(&'a self, krate: &'a str) -> impl Iterator<Item = &'a Entry> {
        self.entries.iter().filter(move |entry| entry.admits(krate))
    }

    /// Every approval in the register, in the order it was written.
    ///
    /// The register-wide counterpart of [`Contract::approvals_for`], for a check
    /// that is about the register's whole posture rather than one package's
    /// edges: the surface freeze reads every entry once.
    pub fn approvals(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter()
    }
}

/// A 128-bit FNV-1a fingerprint of `text`, as `fnv1a128:<32 hex>`.
///
/// Chosen over a std `Hasher` because `DefaultHasher`'s algorithm is explicitly
/// unspecified and may change between releases, which would make a stored
/// receipt digest meaningless across toolchains. FNV-1a is a fixed,
/// well-defined function of the bytes and costs one multiply per byte over a
/// register of a few kilobytes.
fn fingerprint(text: &str) -> String {
    const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;
    let mut hash = OFFSET;
    for byte in text.as_bytes() {
        hash ^= u128::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("fnv1a128:{hash:032x}")
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

/// Tests whether `value` is a legible SPDX licence expression.
///
/// Deliberately structural rather than semantic. The gate does not ship an
/// SPDX parser, and inventing one would make the accepted-licence set a
/// judgement call in code; what it *can* check without a table is the shape a
/// licence field must have before it is worth comparing byte-for-byte against
/// what Cargo reports (#208):
///
/// - at least one identifier;
/// - `AND` / `OR` / `WITH` in upper case only, and never leading or trailing;
/// - no empty operand, so `OR MIT`, `MIT OR` and `MIT  OR` are all refused;
/// - parenthesised groups kept together, so `(MIT OR Apache-2.0) AND
///   Unicode-3.0` parses the way a reader parses it;
/// - identifiers made of letters, digits, `.`, `-` and `_`, which covers every
///   SPDX identifier and licence-reference suffix this repository can meet.
///
/// Whether the identifiers are *accepted* is a separate question, answered by
/// [`Contract::accepted_licenses`] in the audit rather than here: an expression can
/// be perfectly legible and still name a licence this repository does not take.
fn is_spdx_expression(value: &str) -> bool {
    // SPDX separates tokens with exactly one space; a doubled, padded or
    // tab-separated expression is a different string from the one the manifest
    // declares, so it cannot be recorded as if the two compared equal.
    if value != value.trim()
        || value.contains("  ")
        || value
            .chars()
            .any(|ch| ch.is_ascii_whitespace() && ch != ' ')
    {
        return false;
    }
    let mut operands = 0_usize;
    let mut depth = 0_usize;
    let mut previous_operand = false;
    let spaced = value.replace('(', " ( ").replace(')', " ) ");
    for token in spaced.split_ascii_whitespace() {
        if token == "(" {
            if previous_operand {
                return false;
            }
            depth = depth.saturating_add(1);
        } else if token == ")" {
            if !previous_operand || depth == 0 {
                return false;
            }
            depth = depth.saturating_sub(1);
            // A closed group is an operand, so `(MIT)(Apache-2.0)` is refused at
            // the second `(` and `(MIT OR Apache-2.0)` is whole.
            previous_operand = true;
        } else if matches!(token, "AND" | "OR" | "WITH") {
            if !previous_operand {
                return false;
            }
            previous_operand = false;
        } else {
            if previous_operand {
                return false;
            }
            let identifier = token;
            if identifier.is_empty()
                || !identifier
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_'))
            {
                return false;
            }
            previous_operand = true;
            operands = operands.saturating_add(1);
        }
    }
    operands > 0 && depth == 0 && previous_operand
}

/// Whether an authored `origin` names a supported origin identity.
///
/// Three shapes are admitted: a Cargo registry source (`registry+`), a Cargo
/// sparse-registry source (`sparse+`), and a Cargo Git source (`git+`). A
/// scheme-free, non-empty string is an external path authority. Everything
/// else — in particular an unknown scheme such as `svn+…` — is refused at load
/// rather than stored as an origin that would later compare as an ordinary
/// admitted string.
fn is_supported_origin(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return false;
    }
    if value.starts_with("registry+") || value.starts_with("sparse+") || value.starts_with("git+") {
        return true;
    }
    !value.contains("://") && !value.contains('+')
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

/// Reads an optional Boolean field under the closed `true`/`false` grammar.
///
/// A value outside the grammar is a typed field refusal, never a default: an
/// unknown spelling must not silently stand a policy down.
fn optional_bool(
    draft: &RawEntry,
    key: &'static str,
    krate: &str,
) -> Result<Option<bool>, ContractError> {
    match draft.get(key) {
        None => Ok(None),
        Some("true") => Ok(Some(true)),
        Some("false") => Ok(Some(false)),
        Some(value) => Err(ContractError::InvalidField {
            krate: krate.to_owned(),
            field: key,
            line: draft.field_line(key).unwrap_or(draft.line),
            value: value.to_owned(),
            expected: "expected true or false",
        }),
    }
}

/// Reads an optional comma-separated list of Cargo feature names.
///
/// Presence is meaningful: an absent key leaves the dimension unconstrained
/// (grandfathered), while a present key admits exactly the features it lists.
fn optional_feature_list(
    draft: &RawEntry,
    key: &'static str,
    krate: &str,
) -> Result<Option<Vec<String>>, ContractError> {
    let Some(raw) = draft.get(key) else {
        return Ok(None);
    };
    let items = split_csv(raw);
    validate_closed_value(
        draft,
        krate,
        key,
        &items.join(","),
        !items.is_empty() && items.iter().all(|item| is_feature_name(item)),
        "expected a nonempty comma-separated list of Cargo feature names",
    )?;
    Ok(Some(items))
}

/// Checks a Cargo feature spelling: a non-empty run of name characters.
///
/// Feature names may be plain (`std`), versioned-dependency features
/// (`serde/derive`), or explicit dependency activations (`dep:syn`), so the
/// admit set is alphanumerics plus the separators Cargo uses.
fn is_feature_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '+'))
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
    let license = draft.require("license", &krate)?.to_owned();
    let source = draft.require("source", &krate)?.to_owned();
    let origin = draft.get("origin").map(str::to_owned);
    let features = optional_feature_list(draft, "features", &krate)?;
    let required_features = optional_feature_list(draft, "required_features", &krate)?;
    let uses_default_features = optional_bool(draft, "uses_default_features", &krate)?;
    let optional = optional_bool(draft, "optional", &krate)?;
    let target = draft.get("target").map(str::to_owned);
    let aliases = draft.get("aliases").map(split_csv).unwrap_or_default();
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
        "license",
        &license,
        is_spdx_expression(&license),
        "expected an SPDX licence expression, for example `MIT OR Apache-2.0`",
    )?;
    validate_closed_value(
        draft,
        &krate,
        "source",
        &source,
        matches!(source.as_str(), "registry" | "git" | "path"),
        "expected one of registry, git or path",
    )?;
    if let Some(origin) = origin.as_deref() {
        validate_closed_value(
            draft,
            &krate,
            "origin",
            origin,
            is_supported_origin(origin),
            "expected a registry+ / sparse+ / git+ Cargo source, or a scheme-free path authority",
        )?;
    }
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
    if let Some(target) = target.as_deref() {
        // An empty target is meaningful: it requires an unconditional
        // declaration. Any non-empty value is the `cfg(…)` or triple Cargo
        // reported, compared byte-for-byte at admission.
        validate_closed_value(
            draft,
            &krate,
            "target",
            target,
            !target.chars().any(char::is_control),
            "expected a target cfg expression, possibly empty for unconditional",
        )?;
    }
    validate_closed_value(
        draft,
        &krate,
        "aliases",
        &aliases.join(","),
        aliases
            .iter()
            .all(|alias| is_identifier(alias) && alias != &krate),
        "expected comma-separated package identifiers, none equal to crate",
    )?;
    if let (Some(allowed), Some(required)) = (features.as_ref(), required_features.as_ref()) {
        validate_closed_value(
            draft,
            &krate,
            "required_features",
            &required.join(","),
            required.iter().all(|needed| allowed.contains(needed)),
            "expected a subset of the allowed features",
        )?;
    }
    let approved_by = draft.require("approved_by", &krate)?.to_owned();
    let review = draft.require("review", &krate)?.to_owned();

    Ok(Entry {
        krate,
        tier,
        version,
        owner,
        capability,
        license,
        source,
        origin,
        features,
        required_features,
        uses_default_features,
        optional,
        target,
        aliases,
        allowed_consumers,
        allowed_kinds,
        reason,
        approved_by,
        approved_on,
        review,
        line: draft.line,
    })
}

/// Decodes the repository-policy declarations of a dependency register.
///
/// Every list is a comma-separated basic string, refused whole when a member is
/// empty, repeated, or not in its vocabulary — a list that silently dropped a
/// malformed member would admit less, or more, than the reviewer read.
fn build_policy(fields: &[RawField]) -> Result<Policy, ContractError> {
    let mut policy = Policy::default();
    let mut frozen_surfaces: Option<(Vec<String>, usize)> = None;
    let mut frozen_tier: Option<Tier> = None;
    for field in fields {
        let decoded = decode_policy_field(field);
        let value = match decoded {
            Ok(value) => value,
            Err(error) => {
                let refusal = Err(error);
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build_policy: returning an error to the caller");
                return refusal;
            }
        };
        match value {
            PolicyValue::Licences(list) => policy.accepted_licenses = Some(list),
            PolicyValue::Surfaces(list) => policy.surfaces = Some(list),
            PolicyValue::Frozen(list) => frozen_surfaces = Some((list, field.line)),
            PolicyValue::Tier(tier) => frozen_tier = Some(tier),
        }
    }
    let missing = match (frozen_surfaces.is_some(), frozen_tier.is_some()) {
        (true, false) => Some(("frozen_surfaces", "frozen_tier")),
        (false, true) => Some(("frozen_tier", "frozen_surfaces")),
        _ => None,
    };
    if let Some((present, missing)) = missing {
        let refusal = Err(ContractError::IncompletePolicy { present, missing });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build_policy: returning an error to the caller");
        return refusal;
    }
    if let (Some((surfaces, line)), Some(tier)) = (frozen_surfaces, frozen_tier) {
        let checked = check_frozen_are_surfaces(&surfaces, policy.surfaces.as_deref(), line);
        checked?;
        policy.frozen = Some(Frozen { surfaces, tier });
    }
    Ok(policy)
}

/// One decoded `[policy]` declaration, before the freeze's two halves are
/// paired.
enum PolicyValue {
    /// `accepted_licenses`.
    Licences(Vec<String>),
    /// `surfaces`.
    Surfaces(Vec<String>),
    /// `frozen_surfaces`.
    Frozen(Vec<String>),
    /// `frozen_tier`.
    Tier(Tier),
}

/// Decodes one declaration against its own vocabulary.
fn decode_policy_field(field: &RawField) -> Result<PolicyValue, ContractError> {
    match field.key {
        "accepted_licenses" => policy_list(field, is_spdx_term).map(PolicyValue::Licences),
        "surfaces" => policy_list(field, is_identifier).map(PolicyValue::Surfaces),
        "frozen_surfaces" => policy_list(field, is_identifier).map(PolicyValue::Frozen),
        _ => Tier::parse(&field.value)
            .map(PolicyValue::Tier)
            .ok_or_else(|| ContractError::BadTier {
                line: field.line,
                value: field.value.clone(),
            }),
    }
}

/// A frozen surface the register's own closed surface set does not name is a
/// contradiction in one block, refused at load rather than audited around.
fn check_frozen_are_surfaces(
    frozen: &[String],
    surfaces: Option<&[String]>,
    line: usize,
) -> Result<(), ContractError> {
    let Some(surfaces) = surfaces else {
        return Ok(());
    };
    match frozen.iter().find(|name| !surfaces.contains(name)) {
        Some(outside) => Err(ContractError::BadPolicyValue {
            line,
            key: "frozen_surfaces".to_owned(),
            value: format!("{outside} (not one of the declared surfaces)"),
        }),
        None => Ok(()),
    }
}

/// Splits one policy list and refuses it whole on an empty, repeated or
/// out-of-vocabulary member.
fn policy_list(field: &RawField, member: fn(&str) -> bool) -> Result<Vec<String>, ContractError> {
    let refuse = || ContractError::BadPolicyValue {
        line: field.line,
        key: field.key.to_owned(),
        value: field.value.clone(),
    };
    let items = split_csv(&field.value);
    let malformed = items.is_empty()
        || items.iter().enumerate().any(|(index, item)| {
            items.iter().take(index).any(|earlier| earlier == item) || !member(item)
        });
    if malformed {
        let refusal = Err(refuse());
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "policy_list: returning an error to the caller");
        return refusal;
    }
    Ok(items)
}

/// Tests whether `term` is one SPDX licence term: an identifier, or an
/// identifier `WITH` an exception identifier.
///
/// An identifier is ASCII letters, digits, `.`, `-` and `+`, which spans every
/// SPDX list identifier and `LicenseRef-*`. This is the shape, not the SPDX
/// list: which identifiers a repository accepts is exactly the decision this
/// list records, so the reader admits any well-formed one.
fn is_spdx_term(term: &str) -> bool {
    let identifier = |word: &str| {
        !word.is_empty()
            && word
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '+'))
            && !matches!(word, "AND" | "OR" | "WITH")
    };
    let mut words = term.split(' ');
    match (words.next(), words.next(), words.next(), words.next()) {
        (Some(single), None, None, None) => identifier(single),
        (Some(licence), Some("WITH"), Some(exception), None) => {
            identifier(licence) && identifier(exception)
        }
        _ => false,
    }
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
    fold_for_prose(trimmed.trim_end_matches('.')) != fold_for_prose(krate)
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

/// Folds a string for the *prose* check in [`is_a_sentence`] and nothing else.
///
/// Trims, lowercases, and maps `-` to `_`. This is deliberately **not** used
/// for package identity: Cargo's `-`/`_` fold decides whether two published
/// names collide, but it is not the identity a register approves. Approval
/// matching is byte-exact (see [`Entry::admits`]); the only place the fold
/// survives is deciding whether a `reason` merely restates the crate name,
/// where a near-miss in prose is not an authority question.
fn fold_for_prose(name: &str) -> String {
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
                "license = \"MIT OR Apache-2.0\"\n",
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
                     license = \"MIT OR Apache-2.0\"\n\
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
        assert_eq!(parsed.license, "MIT OR Apache-2.0");
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
            ("license", "\"MIT OR Apache-2.0\"", "\"MIT\""),
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
        let from_original = Contract::parse(&original)?;
        let from_reordered = Contract::parse(&reordered)?;
        // The text fingerprint is a property of the bytes, so it differs by
        // construction; the admitted meaning — policy and every entry — must
        // not.
        assert_eq!(
            (
                from_original.enforce,
                from_original.schema,
                &from_original.entries
            ),
            (
                from_reordered.enforce,
                from_reordered.schema,
                &from_reordered.entries
            ),
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
                        line: 12,
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
                    line: 12,
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
                9,
            ),
            (
                "allowed_kinds = \"normal\"",
                "allowed_kinds = \",,,\"",
                "allowed_kinds",
                10,
            ),
            (
                "allowed_kinds = \"normal\"",
                "allowed_kinds = \"normal,unknown\"",
                "allowed_kinds",
                10,
            ),
            ("source = \"registry\"", "source = \"unknown\"", "source", 8),
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
                    Err(ContractError::BadDate { line: 13, .. })
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

    /// One final comma on a list field is a delimiter, not an empty element.
    ///
    /// Both list fields of an entry carry the same rule, so the assertion is
    /// written once and told what the single surviving element should be.
    fn assert_single_trailing_comma_parsed(actual: &[String], expected: &str) {
        assert_eq!(
            actual,
            [expected],
            "one final comma remains a delimiter rather than an empty element"
        );
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
        assert_single_trailing_comma_parsed(&parsed.entries[0].allowed_consumers, "lgwks_std");
        assert_single_trailing_comma_parsed(&parsed.entries[0].allowed_kinds, "normal");
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
                line: 15,
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
                line: 13,
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
                line: 16
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

    /// Lookup is byte-exact: an approval for one spelling does not admit a
    /// package whose name is a `-`/`_` fold of it, and a caller that wants the
    /// fold must author an explicit alias.
    #[test]
    fn lookup_is_exact_and_only_an_explicit_alias_is_tolerated() -> TestResult {
        let exact = Contract::parse(&entry(""))?;
        assert!(exact.approval_for("serde").is_some());
        assert!(
            exact.approval_for("ser-de").is_none(),
            "a fold-alike spelling must not borrow the approval"
        );
        assert!(
            exact.approval_for("SERDE").is_none(),
            "identity is case-sensitive, not folded"
        );

        let aliased = Contract::parse(&entry("aliases = \"ser-de\"\n"))?;
        assert!(
            aliased.approval_for("ser-de").is_some(),
            "an authored alias admits the observed spelling"
        );
        assert!(
            aliased.approval_for("serde").is_some(),
            "the canonical name stays admitted alongside its alias"
        );
        Ok(())
    }

    /// Two distinct spellings are two distinct approvals, not one — and a
    /// compatibility alias a second package also needs is a collision.
    #[test]
    fn aliases_collide_rather_than_share_authority() -> TestResult {
        let distinct = Contract::parse(&format!(
            "{}\n{}",
            entry(""),
            entry("").replace("crate = \"serde\"", "crate = \"ser-de\"")
        ))?;
        assert_eq!(distinct.entry_count(), 2);
        assert!(distinct.approval_for("serde").is_some());
        assert!(distinct.approval_for("ser-de").is_some());

        let contested = Contract::parse(&format!(
            "{}\n{}",
            entry("aliases = \"other\"\n"),
            entry("").replace("crate = \"serde\"", "crate = \"other\"")
        ));
        assert!(
            matches!(contested, Err(ContractError::AliasCollision { ref alias, .. }) if alias == "other"),
            "an alias equal to another package's name is a collision"
        );

        let shared = Contract::parse(&format!(
            "{}\n{}",
            entry("aliases = \"shared\"\n"),
            entry("")
                .replace("crate = \"serde\"", "crate = \"alias_second\"")
                .replace(
                    "review = \"docs/ADMISSION.md\"",
                    "review = \"docs/ADMISSION.md\"\naliases = \"shared\""
                )
        ));
        assert!(
            matches!(shared, Err(ContractError::AliasCollision { ref alias, .. }) if alias == "shared"),
            "two packages may not share one alias"
        );
        Ok(())
    }

    /// An unsupported register schema is refused rather than read under the
    /// older rules, and both supported versions load.
    #[test]
    fn an_unsupported_schema_is_refused() -> TestResult {
        assert!(matches!(
            Contract::parse("[policy]\nschema = 3\n"),
            Err(ContractError::UnsupportedSchema { line: 2, ref value }) if value == "3"
        ));
        assert!(matches!(
            Contract::parse("[policy]\nschema = \"2\"\n"),
            Err(ContractError::UnsupportedSchema { line: 2, .. })
        ));
        assert_eq!(Contract::parse("[policy]\nenforce = true\n")?.schema(), 1);
        assert_eq!(Contract::parse("[policy]\nschema = 2\n")?.schema(), 2);
        Ok(())
    }

    /// Issue #208: `license` is a required field, and the register parser
    /// refuses an expression it cannot read before the audit ever compares it
    /// with Cargo. The accepted-*licence* question is the audit's; this is only
    /// about the shape of the string.
    #[test]
    fn an_absent_license_field_is_refused() {
        let without = entry("").replace("license = \"MIT OR Apache-2.0\"\n", "");
        assert_eq!(
            Contract::parse(&without),
            Err(ContractError::MissingField {
                krate: "serde".into(),
                field: "license",
            })
        );
    }

    /// Every spelling a human writes that is not an SPDX expression is refused
    /// at the field, so a licence field cannot be a free-text note that silently
    /// never matches what Cargo reports.
    #[test]
    fn an_unreadable_license_expression_is_refused_at_its_field() {
        for value in [
            "MIT OR",
            "OR MIT",
            "MIT AND",
            "MIT OR OR Apache-2.0",
            "MIT OR  Apache-2.0",
            "(MIT OR Apache-2.0",
            "MIT OR Apache-2.0)",
            "(MIT)(Apache-2.0)",
            "MIT or Apache-2.0",
            "MIT/Apache-2.0",
            "see LICENSE for terms",
            "MIT, Apache-2.0",
        ] {
            let input = entry("").replace(
                "license = \"MIT OR Apache-2.0\"",
                &format!("license = \"{value}\""),
            );
            assert!(
                matches!(
                    Contract::parse(&input),
                    Err(ContractError::InvalidField {
                        field: "license",
                        line: 7,
                        value: ref got,
                        ..
                    }) if got == value
                ),
                "{value:?} is not an SPDX expression and must be refused, not recorded"
            );
        }
    }

    /// The positive half of the same grammar: every expression this repository
    /// actually approves today, plus the parenthesised and `WITH` forms a
    /// vendored licence statement can carry.
    #[test]
    fn a_readable_spdx_expression_is_kept_verbatim() -> TestResult {
        for value in [
            "MIT",
            "Apache-2.0",
            "MIT OR Apache-2.0",
            "CC0-1.0 OR Apache-2.0 OR Apache-2.0 WITH LLVM-exception",
            "Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT",
            "(MIT OR Apache-2.0) AND Unicode-3.0",
            "LicenseRef-Proprietary",
        ] {
            let input = entry("").replace(
                "license = \"MIT OR Apache-2.0\"",
                &format!("license = \"{value}\""),
            );
            let parsed = Contract::parse(&input)?;
            assert_eq!(parsed.entries[0].license(), value);
        }
        Ok(())
    }
}
