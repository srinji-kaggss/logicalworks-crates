//! `patches` owns the supply-chain ledger: upstream pins that never become
//! forks, and sealed TLS provider exceptions with expiry dates.
//!
//! When an OSS crate does not work, the answer is an upstream patch, not a
//! permanent fork (#723). The ledger lives at `contract/PATCHES.toml` in the
//! repo being gated and is read with the same fail-closed line reader as the
//! approval register. One `[[patch]]` block per pin or exception; a pin whose
//! expiry passes is [`Refusal::PatchExpired`], and a blocked TLS provider
//! (`ring`, `webpki-roots`) present in the resolved lockfile without a live
//! exception is [`Refusal::ProviderBlocked`] (#728).
//!
//! The ledger is opt-in: a repository without one is audited exactly as
//! before. A row that exists but cannot be read or parsed is a refusal, never
//! a silent skip.

use std::path::Path;

use crate::{
    PATCHES_PATH, Refusal,
    contract::{ContractError, RawEntry, is_iso_date, parse_register},
    lifecycle,
    lock::Resolved,
};

// ── Ledger ──────────────────────────────────────────────────────────────────

/// The two things the ledger records. An upstream pin is a temporary,
/// expiring excuse to resolve a crate from a pinned source while the fix
/// travels upstream; a provider exception is a sealed, owned, dated excuse
/// for a blocked provider the resolved tree cannot yet avoid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PinKind {
    /// A pinned upstream source with an open PR and an expiry date.
    UpstreamPin,
    /// A sealed exception for a blocked provider with a blast radius.
    ProviderException,
}

impl PinKind {
    /// Decodes the register spelling of a pin kind.
    fn parse(value: &str) -> Option<Self> {
        match value {
            "upstream-pin" => Some(Self::UpstreamPin),
            "provider-exception" => Some(Self::ProviderException),
            _ => None,
        }
    }
}

/// One row of the supply-chain ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Pin {
    /// Crate this row is about, as `Cargo.lock` spells it.
    pub(crate) krate: String,
    /// Which ledger row this is.
    pub(crate) kind: PinKind,
    /// Workspace crate or team that owns the row.
    pub(crate) owner: String,
    /// One sentence naming why the row exists.
    pub(crate) reason: String,
    /// Path or URL to the evidence: the upstream PR, or the exception record.
    pub(crate) review: String,
    /// ISO date the row was recorded.
    pub(crate) recorded_on: String,
    /// ISO date the row stops being honored. Expired rows are refusals.
    pub(crate) expires_on: String,
    /// Pinned source version. Required for upstream pins.
    pub(crate) version: Option<String>,
    /// Upstream issue URL. Required for upstream pins.
    pub(crate) upstream_issue: Option<String>,
    /// Upstream PR URL. Required for upstream pins.
    pub(crate) upstream_pr: Option<String>,
    /// Digest of the pinned source. Required for upstream pins.
    pub(crate) patch_digest: Option<String>,
    /// What the exception touches and what it cannot leak into. Required for
    /// provider exceptions.
    pub(crate) blast_radius: Option<String>,
    /// The measured condition that removes the exception. Required for
    /// provider exceptions.
    pub(crate) removal_trigger: Option<String>,
}

impl Pin {
    /// Crate this row is about.
    #[must_use]
    pub(crate) fn krate(&self) -> &str {
        &self.krate
    }

    /// Which ledger row this is.
    #[must_use]
    pub(crate) fn kind(&self) -> PinKind {
        self.kind
    }

    /// ISO date the row stops being honored.
    #[must_use]
    pub(crate) fn expires_on(&self) -> &str {
        &self.expires_on
    }

    /// Whether the row is still honored on ISO date `today`.
    ///
    /// ISO dates compare lexicographically, so no calendar arithmetic is
    /// needed and the comparison is total: `expires_on >= today` is live.
    #[must_use]
    pub(crate) fn is_live(&self, today: &str) -> bool {
        self.expires_on.as_str() >= today
    }
}

/// The parsed supply-chain ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Ledger {
    /// Every pin and exception, in the order written.
    pub(crate) entries: Vec<Pin>,
}

impl Ledger {
    /// Every pin and exception, in the order written.
    #[must_use]
    pub(crate) fn entries(&self) -> &[Pin] {
        &self.entries
    }

    /// Parses a ledger. Fail-closed: an unrecognised line is an error, and a
    /// row missing the fields its kind requires is refused rather than honored
    /// halfway.
    pub(crate) fn parse(text: &str) -> Result<Self, ContractError> {
        const KEYS: [&str; 13] = [
            "crate",
            "kind",
            "owner",
            "reason",
            "review",
            "recorded_on",
            "expires_on",
            "version",
            "upstream_issue",
            "upstream_pr",
            "patch_digest",
            "blast_radius",
            "removal_trigger",
        ];
        let raw = parse_register(text, "[[patch]]", &KEYS)?;
        let mut entries: Vec<Pin> = Vec::new();
        for draft in &raw.entries {
            let entry = build(draft)?;
            check_duplicate(&entries, &entry, draft.line())?;
            entries.push(entry);
        }
        Ok(Self { entries })
    }
}

/// The crate names the TLS path refuses in-process without a sealed exception.
///
/// `ring` carries `Apache-2.0 AND ISC` and `webpki-roots` carries
/// `CDLA-Permissive-2.0`; neither term is in any register's accepted set, so a
/// direct edge to either is already refused by the licence audit. This list is
/// the transitive half of #728: the resolved tree may still carry them behind
/// an admitted edge (`lgwks_std` http via `ureq` → `rustls`), and then only a
/// live provider exception excuses them.
pub(crate) const BLOCKED_PROVIDERS: [&str; 2] = ["ring", "webpki-roots"];

/// Turns a fully-read draft into a `Pin`, validating every field rule for the
/// row's kind. Field validation lives in the lifecycle ledger beside the other
/// registers: one definition for every ledger reader.
fn build(draft: &RawEntry) -> Result<Pin, ContractError> {
    let krate = crate::lifecycle::require_crate(draft)?;
    lifecycle::validate_required_fields(
        draft,
        Some(&krate),
        &[
            "crate",
            "kind",
            "owner",
            "reason",
            "review",
            "recorded_on",
            "expires_on",
        ],
    )?;
    let kind_text = draft.require("kind", Some(&krate))?;
    let Some(kind) = PinKind::parse(kind_text) else {
        let refusal = Err(lifecycle::invalid(
            draft,
            &krate,
            "kind",
            kind_text.to_owned(),
            "expected upstream-pin or provider-exception",
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build: returning an error to the caller");
        return refusal;
    };
    let reason = draft.require("reason", Some(&krate))?;
    if !lifecycle::is_mortgage_sentence(reason) {
        let refusal = Err(lifecycle::invalid(
            draft,
            &krate,
            "reason",
            reason.to_owned(),
            "expected a sentence of four or more words ending in a full stop",
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build: returning an error to the caller");
        return refusal;
    }
    for field in ["recorded_on", "expires_on"] {
        let value = draft.require(field, Some(&krate))?;
        if !is_iso_date(value) {
            let refusal = Err(lifecycle::invalid(
                draft,
                &krate,
                field,
                value.to_owned(),
                "expected a real YYYY-MM-DD date",
            ));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build: returning an error to the caller");
            return refusal;
        }
    }
    if draft.require("expires_on", Some(&krate))? < draft.require("recorded_on", Some(&krate))? {
        let refusal = Err(lifecycle::invalid(
            draft,
            &krate,
            "expires_on",
            draft.require("expires_on", Some(&krate))?.to_owned(),
            "expected an expiry on or after the recorded date",
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build: returning an error to the caller");
        return refusal;
    }
    let version = draft.get("version").map(str::to_owned);
    let upstream_issue = draft.get("upstream_issue").map(str::to_owned);
    let upstream_pr = draft.get("upstream_pr").map(str::to_owned);
    let patch_digest = draft.get("patch_digest").map(str::to_owned);
    let blast_radius = draft.get("blast_radius").map(str::to_owned);
    let removal_trigger = draft.get("removal_trigger").map(str::to_owned);
    match kind {
        PinKind::UpstreamPin => {
            for field in ["version", "upstream_issue", "upstream_pr", "patch_digest"] {
                if draft.get(field).is_none_or(|value| value.trim().is_empty()) {
                    let refusal = Err(lifecycle::missing(draft, &krate, field));
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build: returning an error to the caller");
                    return refusal;
                }
            }
        }
        PinKind::ProviderException => {
            for field in ["blast_radius", "removal_trigger"] {
                if draft.get(field).is_none_or(|value| value.trim().is_empty()) {
                    let refusal = Err(lifecycle::missing(draft, &krate, field));
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build: returning an error to the caller");
                    return refusal;
                }
            }
            let Some(trigger) = removal_trigger.as_deref() else {
                let refusal = Err(lifecycle::missing(draft, &krate, "removal_trigger"));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build: returning an error to the caller");
                return refusal;
            };
            if !lifecycle::is_mortgage_sentence(trigger) {
                let refusal = Err(lifecycle::invalid(
                    draft,
                    &krate,
                    "removal_trigger",
                    trigger.to_owned(),
                    "expected a sentence of four or more words ending in a full stop",
                ));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build: returning an error to the caller");
                return refusal;
            }
        }
    }
    let owner = draft.require("owner", Some(&krate))?.to_owned();
    let reason = reason.to_owned();
    let review = draft.require("review", Some(&krate))?.to_owned();
    let recorded_on = draft.require("recorded_on", Some(&krate))?.to_owned();
    let expires_on = draft.require("expires_on", Some(&krate))?.to_owned();
    Ok(Pin {
        kind,
        owner,
        reason,
        review,
        recorded_on,
        expires_on,
        version,
        upstream_issue,
        upstream_pr,
        patch_digest,
        blast_radius,
        removal_trigger,
        krate,
    })
}

/// Refuses a second row for the same crate: one crate has one ledger entry, so
/// a crate that needs both a pin and an exception is a repair to write as one
/// row, not two authorities.
fn check_duplicate(entries: &[Pin], entry: &Pin, line: usize) -> Result<(), ContractError> {
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
pub fn load(root: &Path) -> Result<Option<Ledger>, Refusal> {
    let path = root.join(PATCHES_PATH);
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).map_err(|cause| Refusal::LedgerUnreadable {
        file: path.to_string_lossy().into_owned(),
        reason: format!("cannot read {}: {cause}", path.display()),
    })?;
    Ledger::parse(&text)
        .map(Some)
        .map_err(|error| Refusal::LedgerUnreadable {
            file: path.to_string_lossy().into_owned(),
            reason: format!("{}: {error}", path.display()),
        })
}

/// The ISO date today, in UTC, for expiry comparison.
///
/// Computed from the wall clock at the call site through the estate calendar:
/// the gate runs on today's tree, while tests inject fixed dates and never
/// touch the clock, so the comparison stays deterministic where it is
/// asserted. A clock the estate cannot read fails closed: the far-future date
/// lapses every pin, so a broken clock refuses rather than honors.
///
/// Estate reuse: `lgwks_std::time::calendar::civil_from_days` owns the
/// proleptic Gregorian conversion, and `div_euclid` with `saturating_add`
/// keeps the narrowing total without an `as` cast.
#[must_use]
pub fn today_utc() -> String {
    let elapsed = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(elapsed) => elapsed.as_secs().div_euclid(86_400),
        // A clock the estate cannot read fails closed: the far-future date
        // lapses every pin, so a broken clock refuses rather than honors.
        Err(_) => u64::MAX,
    };
    let Ok(days) = i64::try_from(elapsed) else {
        return civil_date(i64::MAX);
    };
    civil_date(days)
}

/// `days` since the epoch as an ISO date, through the estate calendar.
fn civil_date(days: i64) -> String {
    let (year, month, day) = lgwks_std::time::calendar::civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Every expired row and every uncovered blocked provider in the resolved
/// lockfile, as one refusal each.
///
/// `today` is an ISO date the caller supplies: production passes
/// [`today_utc`], tests pass fixed dates, so the verdict never depends on the
/// machine's clock where it is asserted. A blocked provider the ledger excuses
/// with a live exception is covered and refused nowhere; one it does not is
/// refused with the dependency chain that pulled it in.
#[must_use]
pub fn refusals(ledger: Option<&Ledger>, locked: &[Resolved], today: &str) -> Vec<Refusal> {
    let Some(ledger) = ledger else {
        return Vec::new();
    };
    let mut found: Vec<Refusal> = Vec::new();
    for pin in &ledger.entries {
        if !pin.is_live(today) {
            let upstream = match pin.upstream_pr.as_deref() {
                Some(pr) => pr.to_owned(),
                None => pin.review.clone(),
            };
            found.push(Refusal::PatchExpired {
                krate: pin.krate.clone(),
                expiry: pin.expires_on.clone(),
                upstream,
            });
        }
    }
    for blocked in BLOCKED_PROVIDERS {
        let Some(package) = locked.iter().find(|item| item.name() == blocked) else {
            continue;
        };
        let covered = ledger.entries.iter().any(|pin| {
            pin.kind == PinKind::ProviderException && pin.krate == *blocked && pin.is_live(today)
        });
        if !covered {
            found.push(Refusal::ProviderBlocked {
                krate: package.name().to_owned(),
                version: package.version().to_owned(),
                via: via_chain(locked, package.name()),
            });
        }
    }
    found.sort_by_key(|refusal| refusal.to_string());
    found
}

/// One dependency chain from `leaf` up to the undepended package that pulled
/// it in, as `leaf → … → root`.
///
/// The chain follows the first dependent in lock order, bounded to eight
/// links: it names the seam to record in the exception, not the whole graph,
/// and a cycle or a deep tree cannot make it unbounded. Shared with the
/// ledger report, which prints the same chain a refusal would carry.
pub(crate) fn via_chain(locked: &[Resolved], leaf: &str) -> String {
    fn dep_name(spec: &str) -> &str {
        // A split always yields its first item; the fallback names the whole
        // spec rather than substituting a default.
        match spec.split([' ', '(']).next() {
            Some(first) => first,
            None => spec,
        }
    }
    let mut chain: Vec<&str> = vec![leaf];
    while chain.len() < 8 {
        let current = chain[chain.len().saturating_sub(1)];
        let Some(parent) = locked.iter().find(|item| {
            item.dependencies()
                .iter()
                .any(|spec| dep_name(spec) == current)
        }) else {
            break;
        };
        if chain.contains(&parent.name()) {
            break;
        }
        chain.push(parent.name());
    }
    chain.join(" → ")
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

    fn pin(extra: &str) -> String {
        format!(
            concat!(
                "[[patch]]\n",
                "crate = \"llm-guard\"\n",
                "kind = \"upstream-pin\"\n",
                "owner = \"lgwks_deps\"\n",
                "reason = \"The scanner misses a seeded corpus case the estate must keep failing on.\"\n",
                "review = \"https://github.com/srinji-kaggss/logicalworks-crates/issues/723\"\n",
                "recorded_on = \"2026-10-26\"\n",
                "expires_on = \"2027-01-26\"\n",
                "version = \"0.2.0\"\n",
                "upstream_issue = \"https://example.invalid/upstream/issues/1\"\n",
                "upstream_pr = \"https://example.invalid/upstream/pull/2\"\n",
                "patch_digest = \"sha256:0000000000000000000000000000000000000000000000000000000000000000\"\n",
                "{}"
            ),
            extra
        )
    }

    fn exception(extra: &str) -> String {
        format!(
            concat!(
                "[[patch]]\n",
                "crate = \"ring\"\n",
                "kind = \"provider-exception\"\n",
                "owner = \"lgwks_std\"\n",
                "reason = \"The http feature resolves ring transitively through ureq and rustls.\"\n",
                "review = \"https://github.com/srinji-kaggss/logicalworks-crates/issues/728\"\n",
                "recorded_on = \"2026-10-26\"\n",
                "expires_on = \"2027-04-26\"\n",
                "blast_radius = \"Default-off http feature only; no in-process TLS in the day-0 path.\"\n",
                "removal_trigger = \"HTTP proofs pass with equal p99 and no ring in the resolved tree.\"\n",
                "{}"
            ),
            extra
        )
    }

    fn resolved(
        name: &str,
        version: &str,
        deps: &[&str],
    ) -> Result<Resolved, Box<dyn std::error::Error>> {
        // A `Resolved` is built the same way the lock reader builds one: the
        // test names the fields the scan reads and nothing else.
        let mut text = format!("[[package]]\nname = \"{name}\"\nversion = \"{version}\"\n");
        if !deps.is_empty() {
            text.push_str("dependencies = [\n");
            for dep in deps {
                text.push_str(" \"");
                text.push_str(dep);
                text.push_str("\",\n");
            }
            text.push_str("]\n");
        }
        let parsed = crate::lock::parse(&text).map_err(|error| format!("fixture lock: {error}"))?;
        assert_eq!(parsed.len(), 1);
        let Some(first) = parsed.into_iter().next() else {
            return Err("fixture lock parsed to no package".into());
        };
        Ok(first)
    }

    #[test]
    fn an_upstream_pin_parses_with_its_provenance() -> TestResult {
        let ledger = Ledger::parse(&pin(""))?;
        assert_eq!(ledger.entries.len(), 1);
        let parsed = &ledger.entries[0];
        assert_eq!(parsed.krate(), "llm-guard");
        assert_eq!(parsed.kind(), PinKind::UpstreamPin);
        assert!(parsed.is_live("2026-10-26"));
        assert!(!parsed.is_live("2027-01-27"));
        Ok(())
    }

    #[test]
    fn an_expired_pin_is_refused_with_its_upstream() -> TestResult {
        let ledger = Ledger::parse(&pin(""))?;
        let found = refusals(Some(&ledger), &[], "2027-01-27");
        assert_eq!(found.len(), 1);
        match found.into_iter().next() {
            Some(Refusal::PatchExpired {
                krate,
                expiry,
                upstream,
            }) => {
                assert_eq!(krate, "llm-guard");
                assert_eq!(expiry, "2027-01-26");
                assert_eq!(upstream, "https://example.invalid/upstream/pull/2");
            }
            other => return Err(format!("expected a PatchExpired, found {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn a_live_exception_covers_its_provider() -> TestResult {
        let ledger = Ledger::parse(&exception(""))?;
        let locked = vec![
            resolved("lgwks_std", "3.1.0", &["ureq"])?,
            resolved("ureq", "3.2.0", &["rustls"])?,
            resolved("rustls", "0.23.46", &["ring"])?,
            resolved("ring", "0.17.14", &[])?,
        ];
        assert_eq!(refusals(Some(&ledger), &locked, "2026-10-26").len(), 0);
        let lapsed = refusals(Some(&ledger), &locked, "2027-04-27");
        assert_eq!(lapsed.len(), 2);
        let mut uncovered: Option<String> = None;
        for refusal in lapsed {
            if let Refusal::ProviderBlocked { krate, .. } = refusal {
                uncovered = Some(krate);
            }
        }
        assert_eq!(
            uncovered.as_deref(),
            Some("ring"),
            "a lapsed exception must uncover its provider"
        );
        Ok(())
    }

    #[test]
    fn an_uncovered_provider_is_refused_with_its_chain() -> TestResult {
        let ledger = Ledger::parse(&pin(""))?;
        let locked = vec![
            resolved("lgwks_std", "3.1.0", &["ureq"])?,
            resolved("ureq", "3.2.0", &["rustls"])?,
            resolved("rustls", "0.23.46", &["ring"])?,
            resolved("ring", "0.17.14", &[])?,
        ];
        let found = refusals(Some(&ledger), &locked, "2026-10-26");
        assert_eq!(found.len(), 1);
        match found.into_iter().next() {
            Some(Refusal::ProviderBlocked {
                krate,
                version,
                via,
            }) => {
                assert_eq!(krate, "ring");
                assert_eq!(version, "0.17.14");
                assert_eq!(via, "ring → rustls → ureq → lgwks_std");
            }
            other => return Err(format!("expected a ProviderBlocked, found {other:?}").into()),
        }
        Ok(())
    }
}
