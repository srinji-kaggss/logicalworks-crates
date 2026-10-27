//! `lifecycle` owns the lifecycle mortgage ledger: replacement, removal
//! trigger and review date for every approved edge, keyed by crate.
//!
//! The register names the edge; this companion names its exit. It lives at
//! `contract/LIFECYCLE.toml` in the repo being gated and is read with the same
//! fail-closed line reader as the approval register, so taking a TOML crate to
//! police it would be the same self-refutation the register avoids. One
//! `[[lifecycle]]` block per approved crate; an approval with no row is
//! [`Refusal::MissingLifecycle`].
//!
//! The companion exists because the mortgage is review metadata, not admission
//! identity: the approval says what the edge is, the mortgage says how it
//! ends. Keeping it beside the register keeps both in the same commit, and
//! keeps the register's schema stable while the mortgage evolves every
//! quarter.
//!
//! The ledger is opt-in like the deferral catalog and the supply-chain
//! ledger: a repository without one is audited exactly as before. A row
//! without a replacement, a trigger or a review date is a load refusal, never
//! a silent skip.

use std::path::Path;

use crate::{
    LIFECYCLE_PATH, Refusal,
    contract::{
        Contract, ContractError, RawEntry, check_field_present, is_iso_date, parse_register,
    },
};

// ── Mortgage ────────────────────────────────────────────────────────────────

/// One row of the lifecycle ledger: how an approved edge ends.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub(crate) struct Mortgage {
    /// Crate this row mortgages, as `Cargo.lock` spells it.
    pub(crate) krate: String,
    /// What replaces the edge: the estate module or upstream contribution.
    pub(crate) replacement: String,
    /// The measured condition that retires the edge.
    pub(crate) removal_trigger: String,
    /// ISO date the edge was last reviewed.
    pub(crate) last_reviewed: String,
    /// Path or URL to the evidence behind the mortgage.
    pub(crate) review: String,
}

impl Mortgage {
    /// What replaces the edge.
    #[must_use]
    pub(crate) fn replacement(&self) -> &str {
        &self.replacement
    }

    /// The measured condition that retires the edge.
    #[must_use]
    pub(crate) fn removal_trigger(&self) -> &str {
        &self.removal_trigger
    }

    /// ISO date the edge was last reviewed.
    #[must_use]
    pub(crate) fn last_reviewed(&self) -> &str {
        &self.last_reviewed
    }
}

/// The parsed lifecycle ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Lifecycle {
    /// Every mortgage row, in the order written.
    pub(crate) entries: Vec<Mortgage>,
}

impl Lifecycle {
    /// The row mortgaging `krate`, when the ledger names one.
    #[must_use]
    pub(crate) fn row_for(&self, krate: &str) -> Option<&Mortgage> {
        self.entries.iter().find(|row| row.krate == krate)
    }

    /// Parses a ledger. Fail-closed: an unrecognised line is an error, and a
    /// row missing its replacement, trigger or review date is refused rather
    /// than recorded as an open-ended mortgage.
    pub(crate) fn parse(text: &str) -> Result<Self, ContractError> {
        const KEYS: [&str; 5] = [
            "crate",
            "replacement",
            "removal_trigger",
            "last_reviewed",
            "review",
        ];
        let raw = parse_register(text, "[[lifecycle]]", &KEYS)?;
        let mut entries: Vec<Mortgage> = Vec::new();
        for draft in &raw.entries {
            let entry = build(draft)?;
            check_duplicate(&entries, &entry, draft.line())?;
            entries.push(entry);
        }
        Ok(Self { entries })
    }
}

/// Rejects the first required field that is absent or blank, in the order the
/// caller lists them.
///
/// One definition for every ledger reader: the deferral catalog, the
/// supply-chain ledger and this one all refuse a blank field the same way, so
/// the bar cannot drift between the registers it judges.
pub(crate) fn validate_required_fields(
    draft: &RawEntry,
    krate: Option<&str>,
    fields: &[&'static str],
) -> Result<(), ContractError> {
    for field in fields {
        check_field_present(draft, krate, field)?;
    }
    Ok(())
}

/// Whether `value` states a mortgage a reviewer can act on: the same sentence
/// bar as an approval reason, so a replacement that does not say what and a
/// trigger that does not say when are refused rather than recorded.
///
/// Shared with the deferral and supply-chain ledgers, whose rows carry the
/// same bar: one definition, three readers, so the bar cannot drift between
/// the registers it judges.
pub(crate) fn is_mortgage_sentence(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.len() >= 24 && trimmed.ends_with('.') && trimmed.split_whitespace().count() >= 4
}

/// Refuses a field whose decoded value is outside its vocabulary, at the
/// field's own line when the block wrote it and at the block's line otherwise.
/// Shared with the deferral and supply-chain ledgers.
pub(crate) fn invalid(
    draft: &RawEntry,
    krate: &str,
    field: &'static str,
    value: String,
    expected: &'static str,
) -> ContractError {
    let line = field_line(draft, field);
    ContractError::InvalidField {
        krate: krate.to_owned(),
        field,
        line,
        value,
        expected,
    }
}

/// The crate a block is about, or the refusal naming the block that names none.
///
/// Every ledger reader starts here: a block that declares no crate has no
/// identity to admit, and a placeholder name would send a reader chasing a
/// crate the file does not mention.
pub(crate) fn require_crate(draft: &RawEntry) -> Result<String, ContractError> {
    match draft.get("crate").map(str::to_owned) {
        Some(krate) => Ok(krate),
        None => {
            let refusal = Err(ContractError::MissingField {
                krate: None,
                field: "crate",
                line: draft.line(),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), line = draft.line(), "require_crate: a ledger block names no crate");
            refusal
        }
    }
}

/// Turns a fully-read draft into a `Mortgage`, validating every field rule.
fn build(draft: &RawEntry) -> Result<Mortgage, ContractError> {
    let krate = require_crate(draft)?;
    validate_required_fields(
        draft,
        Some(&krate),
        &[
            "crate",
            "replacement",
            "removal_trigger",
            "last_reviewed",
            "review",
        ],
    )?;
    for field in ["replacement", "removal_trigger"] {
        let value = draft.require(field, Some(&krate))?;
        if !is_mortgage_sentence(value) {
            let refusal = Err(invalid(
                draft,
                &krate,
                field,
                value.to_owned(),
                "expected a sentence of four or more words ending in a full stop",
            ));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build: returning an error to the caller");
            return refusal;
        }
    }
    let last_reviewed = draft.require("last_reviewed", Some(&krate))?.to_owned();
    if !is_iso_date(&last_reviewed) {
        let refusal = Err(invalid(
            draft,
            &krate,
            "last_reviewed",
            last_reviewed,
            "expected a real YYYY-MM-DD date",
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build: returning an error to the caller");
        return refusal;
    }
    let replacement = draft.require("replacement", Some(&krate))?.to_owned();
    let removal_trigger = draft.require("removal_trigger", Some(&krate))?.to_owned();
    let review = draft.require("review", Some(&krate))?.to_owned();
    Ok(Mortgage {
        replacement,
        removal_trigger,
        last_reviewed,
        review,
        krate,
    })
}

/// Refuses a row the entry kind requires but the block does not carry, at the
/// row's own line. Shared with the supply-chain ledger.
pub(crate) fn missing(draft: &RawEntry, krate: &str, field: &'static str) -> ContractError {
    ContractError::MissingField {
        krate: Some(krate.to_owned()),
        field,
        line: draft.line(),
    }
}

/// The line a diagnostic about `field` names: the field's own line when the
/// block wrote it, else the block's opening line. Shared with the deferral
/// and supply-chain ledgers.
pub(crate) fn field_line(draft: &RawEntry, field: &str) -> usize {
    match draft.field_line(field) {
        Some(line) => line,
        None => draft.line(),
    }
}

/// Refuses a second row for the same crate: one crate has one mortgage, so a
/// second row would split the exit path.
fn check_duplicate(
    entries: &[Mortgage],
    entry: &Mortgage,
    line: usize,
) -> Result<(), ContractError> {
    if entries.iter().any(|existing| existing.krate == entry.krate) {
        Err(ContractError::DuplicateEntry {
            krate: entry.krate.clone(),
            line,
        })
    } else {
        Ok(())
    }
}

/// Loads the ledger beside `root`, or `Ok(None)` when the repository carries
/// none: the ledger is opt-in, and a repository without one is audited exactly
/// as before.
///
/// A file that exists but cannot be read or parsed is a refusal, never a
/// silent skip.
pub fn load(root: &Path) -> Result<Option<Lifecycle>, Refusal> {
    let path = root.join(LIFECYCLE_PATH);
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).map_err(|cause| Refusal::LedgerUnreadable {
        file: path.to_string_lossy().into_owned(),
        reason: format!("cannot read {}: {cause}", path.display()),
    })?;
    Lifecycle::parse(&text)
        .map(Some)
        .map_err(|error| Refusal::LedgerUnreadable {
            file: path.to_string_lossy().into_owned(),
            reason: format!("{}: {error}", path.display()),
        })
}

/// Every approved edge the lifecycle ledger does not mortgage, as one refusal
/// each.
///
/// The mortgage is what makes an approval more than a name on a list: the
/// refusal names the crate the review page must gain a row for.
#[must_use]
pub fn refusals(register: &Contract, lifecycle: Option<&Lifecycle>) -> Vec<Refusal> {
    let Some(lifecycle) = lifecycle else {
        return Vec::new();
    };
    let mut found: Vec<Refusal> = Vec::new();
    for entry in register.approvals() {
        if lifecycle.row_for(entry.krate()).is_none() {
            found.push(Refusal::MissingLifecycle {
                krate: entry.krate().to_owned(),
            });
        }
    }
    found.sort_by_key(|refusal| refusal.to_string());
    found
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

    fn row(extra: &str) -> String {
        format!(
            concat!(
                "[[lifecycle]]\n",
                "crate = \"axum\"\n",
                "replacement = \"An estate HTTP facade in lgwks_std covering routing with equal p99.\"\n",
                "removal_trigger = \"The estate router covers the same routes with equal p99.\"\n",
                "last_reviewed = \"2026-10-26\"\n",
                "review = \"https://github.com/srinji-kaggss/logicalworks-crates/issues/721\"\n",
                "{}"
            ),
            extra
        )
    }

    fn register(crate_name: &str) -> String {
        format!(
            concat!(
                "[[approved]]\n",
                "crate = \"{name}\"\n",
                "tier = \"boundary\"\n",
                "version = \"^0.8\"\n",
                "owner = \"lgwks_deps\"\n",
                "capability = \"http.server\"\n",
                "license = \"MIT\"\n",
                "source = \"registry\"\n",
                "allowed_consumers = \"lgwks_deps\"\n",
                "allowed_kinds = \"normal\"\n",
                "reason = \"Revenue slice HTTP routing needs Tower-compatible routing the standard library does not provide.\"\n",
                "approved_by = \"maintainer\"\n",
                "approved_on = \"2026-10-26\"\n",
                "review = \"https://github.com/srinji-kaggss/logicalworks-crates/issues/721\"\n",
            ),
            name = crate_name
        )
    }

    #[test]
    fn a_complete_row_parses() -> TestResult {
        let ledger = Lifecycle::parse(&row(""))?;
        assert_eq!(ledger.entries.len(), 1);
        let parsed = ledger.entries.first().ok_or("the ledger parsed no row")?;
        assert_eq!(parsed.krate, "axum");
        assert_eq!(
            parsed.replacement(),
            "An estate HTTP facade in lgwks_std covering routing with equal p99."
        );
        assert_eq!(
            parsed.removal_trigger(),
            "The estate router covers the same routes with equal p99."
        );
        assert_eq!(parsed.last_reviewed(), "2026-10-26");
        assert_eq!(
            parsed.review,
            "https://github.com/srinji-kaggss/logicalworks-crates/issues/721"
        );
        Ok(())
    }

    #[test]
    fn an_unmortgaged_edge_is_refused_by_name() -> TestResult {
        let mortgaged = Contract::parse(&register("axum"))?;
        let approved = Contract::parse(&register("tower-http"))?;
        let ledger = Lifecycle::parse(&row(""))?;
        let found = refusals(&mortgaged, Some(&ledger));
        assert_eq!(found.len(), 0);
        let missing = refusals(&approved, Some(&ledger));
        assert_eq!(missing.len(), 1);
        match missing.into_iter().next() {
            Some(Refusal::MissingLifecycle { krate }) => {
                assert_eq!(krate, "tower-http");
            }
            other => return Err(format!("expected a MissingLifecycle, found {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn an_absent_ledger_refuses_nothing() -> TestResult {
        let mortgaged = Contract::parse(&register("axum"))?;
        assert_eq!(refusals(&mortgaged, None).len(), 0);
        Ok(())
    }

    #[test]
    fn a_mortgage_that_states_nothing_is_refused() -> TestResult {
        let input = row("").replace(
            "removal_trigger = \"The estate router covers the same routes with equal p99.\"",
            "removal_trigger = \"later\"",
        );
        assert!(
            matches!(
                Lifecycle::parse(&input),
                Err(ContractError::InvalidField {
                    field: "removal_trigger",
                    ..
                })
            ),
            "\"when we have time\" wearing a trigger key must be refused, not recorded"
        );
        Ok(())
    }

    #[test]
    fn a_row_without_a_review_date_is_refused() -> TestResult {
        let input = row("").replace("last_reviewed = \"2026-10-26\"\n", "");
        assert!(
            matches!(
                Lifecycle::parse(&input),
                Err(ContractError::MissingField {
                    field: "last_reviewed",
                    ..
                })
            ),
            "a mortgage with no review date must be refused at load"
        );
        Ok(())
    }
}
