//! `deferral` owns the deferral catalog: crates the estate does not admit on
//! day 0, each with the owner that must be fixed first and the measured
//! trigger that would admit it.
//!
//! The catalog lives at `contract/DEFERRAL.toml` in the repo being gated and
//! is read with the same fail-closed line reader as the approval register, so
//! taking a TOML crate to police it would be the same self-refutation the
//! register avoids. One `[[deferred]]` block per crate family row; a crate the
//! register approves while the catalog defers is
//! [`Refusal::DeferredAdmitted`].
//!
//! The catalog is opt-in: a repository without one is audited exactly as
//! before. A row without a trigger or an owner is a load refusal, never a
//! silent skip, because a deferral that names neither is "not now" wearing a
//! new key.

use std::path::Path;

use crate::{
    DEFERRAL_PATH, Refusal,
    contract::{Contract, ContractError, RawEntry, is_iso_date, parse_register},
    lifecycle,
};

// ── Catalog ─────────────────────────────────────────────────────────────────

/// One row of the deferral catalog: a crate family that is not day 0, the
/// estate owner that must be fixed first, and the measured trigger.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Deferred {
    /// Crate name as `Cargo.lock` would spell it.
    pub(crate) krate: String,
    /// The family the issue catalogs it under (`data-layer`, `platform`, …).
    pub(crate) family: String,
    /// The estate owner that must be fixed first, or the gap when none owns it.
    pub(crate) owner: String,
    /// The measured trigger that would admit the crate.
    pub(crate) trigger: String,
    /// What replaces the crate when the trigger is met, or the exit path.
    pub(crate) replacement: String,
    /// Path or URL to the evidence behind the deferral.
    pub(crate) review: String,
    /// ISO date the row was recorded.
    pub(crate) deferred_on: String,
}

impl Deferred {
    /// Crate name this row defers.
    #[must_use]
    pub(crate) fn krate(&self) -> &str {
        &self.krate
    }

    /// The estate owner that must be fixed first.
    #[must_use]
    pub(crate) fn owner(&self) -> &str {
        &self.owner
    }

    /// The measured trigger that would admit the crate.
    #[must_use]
    pub(crate) fn trigger(&self) -> &str {
        &self.trigger
    }

    /// What replaces the crate when the trigger is met.
    #[must_use]
    pub(crate) fn replacement(&self) -> &str {
        &self.replacement
    }

    /// Path or URL to the evidence behind the deferral.
    #[must_use]
    pub(crate) fn review(&self) -> &str {
        &self.review
    }

    /// ISO date the row was recorded.
    #[must_use]
    pub(crate) fn deferred_on(&self) -> &str {
        &self.deferred_on
    }
}

/// The parsed deferral catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Catalog {
    /// Every deferred row, in the order written.
    pub(crate) entries: Vec<Deferred>,
}

impl Catalog {
    /// Every deferred row, in the order written.
    #[must_use]
    pub(crate) fn entries(&self) -> &[Deferred] {
        &self.entries
    }

    /// How many crates the catalog defers.
    #[must_use]
    pub(crate) fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Parses a catalog. Fail-closed: an unrecognised line is an error, not a
    /// line to skip, and a row missing its owner or trigger is refused rather
    /// than recorded as an open-ended deferral.
    pub(crate) fn parse(text: &str) -> Result<Self, ContractError> {
        const KEYS: [&str; 7] = [
            "crate",
            "family",
            "owner",
            "trigger",
            "replacement",
            "review",
            "deferred_on",
        ];
        let raw = parse_register(text, "[[deferred]]", &KEYS)?;
        let mut entries: Vec<Deferred> = Vec::new();
        for draft in &raw.entries {
            let entry = build(draft)?;
            check_duplicate(&entries, &entry, draft.line())?;
            entries.push(entry);
        }
        Ok(Self { entries })
    }
}

/// Turns a fully-read draft into a `Deferred`, validating every field rule.
/// Field validation lives in the lifecycle ledger beside the other registers:
/// one definition for every ledger reader, so the bar cannot drift.
fn build(draft: &RawEntry) -> Result<Deferred, ContractError> {
    let krate = crate::lifecycle::require_crate(draft)?;
    lifecycle::validate_required_fields(
        draft,
        Some(&krate),
        &[
            "crate",
            "family",
            "owner",
            "trigger",
            "replacement",
            "review",
            "deferred_on",
        ],
    )?;
    let deferred_on = draft.require("deferred_on", Some(&krate))?.to_owned();
    if !is_iso_date(&deferred_on) {
        let refusal = Err(lifecycle::invalid(
            draft,
            &krate,
            "deferred_on",
            deferred_on,
            "expected a real YYYY-MM-DD date",
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build: returning an error to the caller");
        return refusal;
    }
    for field in ["trigger", "replacement"] {
        let value = draft.require(field, Some(&krate))?;
        if !crate::lifecycle::is_mortgage_sentence(value) {
            let refusal = Err(lifecycle::invalid(
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
    let family = draft.require("family", Some(&krate))?.to_owned();
    let owner = draft.require("owner", Some(&krate))?.to_owned();
    let trigger = draft.require("trigger", Some(&krate))?.to_owned();
    let replacement = draft.require("replacement", Some(&krate))?.to_owned();
    let review = draft.require("review", Some(&krate))?.to_owned();
    Ok(Deferred {
        family,
        owner,
        trigger,
        replacement,
        review,
        deferred_on,
        krate,
    })
}

/// Refuses a second row for the same crate: one crate has one deferral, one
/// owner and one trigger, so a second row would split the request path.
fn check_duplicate(
    entries: &[Deferred],
    entry: &Deferred,
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

/// Loads the catalog beside `root`, or `Ok(None)` when the repository carries
/// none: the catalog is opt-in, and a repository without one is audited
/// exactly as before.
///
/// A file that exists but cannot be read or parsed is a refusal, never a
/// silent skip: failing open would make the one condition the catalog exists
/// to catch the one condition it passes.
pub fn load(root: &Path) -> Result<Option<Catalog>, Refusal> {
    let path = root.join(DEFERRAL_PATH);
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).map_err(|cause| Refusal::LedgerUnreadable {
        file: path.to_string_lossy().into_owned(),
        reason: format!("cannot read {}: {cause}", path.display()),
    })?;
    Catalog::parse(&text)
        .map(Some)
        .map_err(|error| Refusal::LedgerUnreadable {
            file: path.to_string_lossy().into_owned(),
            reason: format!("{}: {error}", path.display()),
        })
}

/// Every approval the deferral catalog still defers, as one refusal each.
///
/// The refusal names the estate owner that must be fixed first and the
/// measured trigger that would admit the crate, so a rejected request is
/// closed with the trigger and the owner, not with "not now".
#[must_use]
pub fn refusals(register: &Contract, catalog: Option<&Catalog>) -> Vec<Refusal> {
    let Some(catalog) = catalog else {
        return Vec::new();
    };
    catalog
        .entries
        .iter()
        .filter(|row| register.approvals_for(&row.krate).next().is_some())
        .map(|row| Refusal::DeferredAdmitted {
            krate: row.krate.clone(),
            owner: row.owner.clone(),
            trigger: row.trigger.clone(),
        })
        .collect()
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
                "[[deferred]]\n",
                "crate = \"openraft\"\n",
                "family = \"data-layer\"\n",
                "owner = \"logical-DB\"\n",
                "trigger = \"A paid multi-node replication requirement with a measured failover test.\"\n",
                "replacement = \"openraft behind lgwks_deps with the estate log as the source of truth.\"\n",
                "review = \"https://github.com/srinji-kaggss/logicalworks-crates/issues/725\"\n",
                "deferred_on = \"2026-10-26\"\n",
                "{}"
            ),
            extra
        )
    }

    fn register_text(crate_name: &str) -> String {
        format!(
            concat!(
                "[[approved]]\n",
                "crate = \"{name}\"\n",
                "tier = \"boundary\"\n",
                "version = \"^1\"\n",
                "owner = \"lgwks_deps\"\n",
                "capability = \"test.deferred\"\n",
                "license = \"MIT\"\n",
                "source = \"registry\"\n",
                "allowed_consumers = \"lgwks_deps\"\n",
                "allowed_kinds = \"normal\"\n",
                "reason = \"A deferred edge under test that the catalog must refuse.\"\n",
                "approved_by = \"reviewer\"\n",
                "approved_on = \"2026-10-26\"\n",
                "review = \"docs/deferral.md\"\n",
            ),
            name = crate_name
        )
    }

    #[test]
    fn a_complete_row_parses() -> TestResult {
        let catalog = Catalog::parse(&row(""))?;
        assert_eq!(catalog.entry_count(), 1);
        let parsed = &catalog.entries[0];
        assert_eq!(parsed.krate(), "openraft");
        assert_eq!(parsed.owner(), "logical-DB");
        assert_eq!(
            parsed.trigger(),
            "A paid multi-node replication requirement with a measured failover test."
        );
        assert_eq!(
            parsed.replacement(),
            "openraft behind lgwks_deps with the estate log as the source of truth."
        );
        assert_eq!(
            parsed.review(),
            "https://github.com/srinji-kaggss/logicalworks-crates/issues/725"
        );
        assert_eq!(parsed.deferred_on(), "2026-10-26");
        Ok(())
    }

    #[test]
    fn an_approved_edge_the_catalog_defers_is_refused() -> TestResult {
        let register = Contract::parse(&register_text("openraft"))?;
        let catalog = Catalog::parse(&row(""))?;
        let found = refusals(&register, Some(&catalog));
        assert_eq!(found.len(), 1);
        match found.into_iter().next() {
            Some(Refusal::DeferredAdmitted {
                krate,
                owner,
                trigger,
            }) => {
                assert_eq!(krate, "openraft");
                assert_eq!(owner, "logical-DB");
                assert_eq!(
                    trigger,
                    "A paid multi-node replication requirement with a measured failover test.",
                    "the refusal must name the measured trigger"
                );
            }
            other => return Err(format!("expected a DeferredAdmitted, found {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn an_absent_catalog_refuses_nothing() -> TestResult {
        let register = Contract::parse(&register_text("openraft"))?;
        assert_eq!(refusals(&register, None).len(), 0);
        let catalog = Catalog::parse(&row(""))?;
        let clean = Contract::parse(&register_text("axum"))?;
        assert_eq!(refusals(&clean, Some(&catalog)).len(), 0);
        Ok(())
    }

    #[test]
    fn a_trigger_that_states_nothing_is_refused() -> TestResult {
        let input = row("").replace(
            "trigger = \"A paid multi-node replication requirement with a measured failover test.\"",
            "trigger = \"later\"",
        );
        assert!(
            matches!(
                Catalog::parse(&input),
                Err(ContractError::InvalidField {
                    field: "trigger",
                    ..
                })
            ),
            "\"not now\" wearing a trigger key must be refused, not recorded"
        );
        Ok(())
    }

    #[test]
    fn a_row_without_an_owner_is_refused() -> TestResult {
        let input = row("").replace("owner = \"logical-DB\"\n", "");
        assert!(
            matches!(
                Catalog::parse(&input),
                Err(ContractError::MissingField { field: "owner", .. })
            ),
            "a deferral that names no owner must be refused at load"
        );
        Ok(())
    }
}
