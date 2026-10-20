//! `act gh::issue`: a `script!` flow opens a GitHub issue once per key.
//!
//! The flow is expanded by the real macro and run by a real `Host`; the action
//! is the shipped `OpenIssue`, forking a fake `gh` that holds issues through
//! the same supervised runner the real client runs through. What each test
//! asserts is what the receiver holds and how many creates were issued, which
//! is the number a duplicate doubles.
//!
//! | Journey | Test |
//! |---|---|
//! | J1: the harness opens an issue and prints its page | `a_flow_opens_an_issue_and_reads_it_back` |
//! | J2: a run without `github.issue.create` is refused naming it | `a_run_without_the_issue_grant_is_blocked_before_gh_starts` |
//! | J3: a rerun after a lost answer or a kill opens nothing new | `a_second_run_with_the_same_key_finds_the_first_issue`, `a_lost_answer_is_reconciled_by_reading_back_and_never_reposted` |

#![cfg(all(unix, feature = "script", feature = "process", feature = "ephemeral"))]

use std::error::Error;
use std::time::Duration;

use lgwks_bot::domain::gh::{
    GhError, ISSUE_CREATE, IssueDraft, OpenIssue, OpenedIssue, Settlement,
};
use lgwks_bot::script::{FlowError, Scope};
use lgwks_bot::task::{Host, Report, task};
use lgwks_bot::{BotError, Cap, GrantSet, domains};

use crate::fake_gh_issues::{calls_on, creates_on, hold, issue_on_fake};

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

domains! {
    /// `gh::issue` bound to the fake receiver.
    pub(crate) FAKE_ISSUES {
        observe {}
        execute {
            "gh::issue" => issue_on_fake,
        }
    }
}

domains! {
    /// `gh::issue` as a deployment declares it: the shipped constructor.
    pub(crate) SHIPPED_ISSUES {
        observe {}
        execute {
            "gh::issue" => OpenIssue::from_target,
        }
    }
}

lgwks_bot::script! {
    /// Open `draft` on `repo`, once for its key.
    pub(crate) flow open_issue(repo: &str, draft: IssueDraft) -> OpenedIssue:
        let opened = act gh::issue on repo with draft
        give back opened.into_output::<OpenedIssue>()?
}

/// Everything opening an issue needs: running `gh`, the network, the write.
pub(crate) fn full_grant() -> GrantSet {
    GrantSet::empty()
        .grant(Cap::sys())
        .grant(Cap::net())
        .grant(Cap::new(ISSUE_CREATE))
}

/// A host for `tenant` over the fake receiver.
pub(crate) fn fake_host(tenant: &str, grants: GrantSet) -> Result<Host, Box<dyn Error>> {
    Ok(Host::builder(tenant)?
        .domains(&FAKE_ISSUES)
        .grants(grants)
        .default_deadline(Duration::from_secs(120))
        .build()?)
}

/// Run the flow once on `host`.
pub(crate) fn open(
    host: &Host,
    repo: &str,
    draft: &IssueDraft,
) -> Result<Report<OpenedIssue>, Box<dyn Error>> {
    let opening = task(
        "opening",
        |scope: Scope, (repo, draft): (String, IssueDraft)| async move {
            open_issue(&scope, &repo, draft).await
        },
    )?;
    Ok(lgwks_bot::block_on(
        host.run(&opening, (repo.to_owned(), draft.clone())),
    ))
}

/// The issue a run settled on, or why it did not settle.
pub(crate) fn opened(report: &Report<OpenedIssue>) -> Result<&OpenedIssue, String> {
    report
        .result()
        .map_err(|failed| format!("expected an issue, got {failed:?}"))
}

/// The crate error a run failed with, or what it returned instead.
pub(crate) fn failed(report: &Report<OpenedIssue>) -> Result<&BotError, String> {
    let refusal = match report.result() {
        Err(refusal) => refusal,
        Ok(issue) => {
            lgwks_std::trace::debug!(issue = ?issue, "gh_issue: the run opened an issue");
            return Err(format!("expected a bot error, got {issue:?}"));
        }
    };
    match *refusal {
        FlowError::Bot { ref source, .. } => Ok(&**source),
        ref other => Err(format!("expected a bot error, got {other:?}")),
    }
}

/// Open `key` on `repo` from a fresh fully granted host, requiring that this
/// run created the issue.
fn open_new(repo: &str, key: &str, title: &str) -> Result<OpenedIssue, Box<dyn Error>> {
    let host = fake_host("acme", full_grant())?;
    let draft = IssueDraft::new(key, title, "Body text.")?;
    let issue = opened(&open(&host, repo, &draft)?)?.clone();
    assert_eq!(issue.settlement(), Settlement::Created, "{key} on {repo}");
    Ok(issue)
}

// ── J1 ─────────────────────────────────────────────────────────────────────

#[test]
fn a_flow_opens_an_issue_and_reads_it_back() -> TestResult {
    let issue = open_new("j1-owner/accept", "j1", "The harness opened this")?;
    assert_eq!(issue.number(), 1);
    assert_eq!(issue.url(), "https://github.com/j1-owner/accept/issues/1");
    assert_eq!(creates_on("j1-owner")?, 1, "one create");
    // Scan, create, read #1 back, see it listed: the create's own answer is
    // not the evidence, and a rerun's scan is shown to find it.
    assert_eq!(calls_on("j1-owner")?.len(), 4);
    Ok(())
}

/// The live failure of 2026-10-20: this repository's newest 100 issues read
/// back as 567,275 bytes against a 65,536-byte capture, so the first live open
/// was refused before it created anything. Two bodies near GitHub's limit put
/// an unfiltered read past the capture on their own.
#[test]
fn a_repository_of_long_issues_still_reads_back_within_the_capture() -> TestResult {
    let host = fake_host("acme", full_grant())?;
    let long = "b".repeat(60_000);
    for key in ["long-1", "long-2"] {
        let draft = IssueDraft::new(key, "A long issue", long.as_str())?;
        assert_eq!(
            opened(&open(&host, "long-owner/accept", &draft)?)?.settlement(),
            Settlement::Created
        );
    }
    let draft = IssueDraft::new("long-3", "After two long issues", "")?;
    let report = open(&host, "long-owner/accept", &draft)?;
    let issue = opened(&report)?;
    assert_eq!(issue.settlement(), Settlement::Created);
    assert_eq!(issue.number(), 3);
    assert_eq!(creates_on("long-owner")?, 3);
    Ok(())
}

/// The live failures of 2026-10-20: GitHub created #657 and the listing read
/// right after did not show it; then an immediate rerun opened #659 beside
/// #658. A create is confirmed at its number and reported only once the
/// listing shows it, so the rerun straight after finds it.
#[test]
fn a_create_waits_for_the_listing_so_an_immediate_rerun_finds_it() -> TestResult {
    let host = fake_host("acme", full_grant())?;
    let draft = IssueDraft::new("lagged", "Listed late", "")?;
    let first = open(&host, "lag-owner/lag", &draft)?;
    let again = open(&host, "lag-owner/lag", &draft)?;
    assert_eq!(opened(&first)?.settlement(), Settlement::Created);
    assert_eq!(opened(&again)?.settlement(), Settlement::Found);
    assert_eq!(
        creates_on("lag-owner")?,
        1,
        "the immediate rerun created nothing"
    );
    let calls = calls_on("lag-owner")?;
    assert!(
        calls
            .iter()
            .any(|argv| argv.iter().any(|arg| arg.ends_with("/issues/1"))),
        "the create is confirmed at the number GitHub answered: {calls:?}"
    );
    // Scan, create, read #1, two lagging listings, the one that shows it,
    // and the rerun's scan.
    assert_eq!(calls.len(), 7);
    Ok(())
}

/// The live finding of 2026-10-20: once #659 duplicated #658, the key's
/// newest match was the duplicate. A key answers the first issue opened under
/// it, so a rerun reports the original, not the duplicate.
#[test]
fn a_key_that_names_two_issues_answers_the_first() -> TestResult {
    let host = fake_host("acme", full_grant())?;
    let draft = IssueDraft::new("twice", "Opened twice", "")?;
    // The original and its duplicate, as the lagging rerun left them.
    hold("twice-owner", "accept", 1, &draft)?;
    hold("twice-owner", "accept", 2, &draft)?;
    let rerun = opened(&open(&host, "twice-owner/accept", &draft)?)?.clone();
    assert_eq!(rerun.settlement(), Settlement::Found);
    assert_eq!(rerun.number(), 1, "the first issue, not the duplicate");
    assert_eq!(creates_on("twice-owner")?, 0);
    Ok(())
}

/// A listing that never shows the issue within the settle bound is reported
/// as `Unlisted`, not as a create a rerun is safe after.
#[test]
fn a_create_the_listing_never_shows_is_reported_unlisted() -> TestResult {
    let host = fake_host("acme", full_grant())?;
    let draft = IssueDraft::new("stale", "Never listed", "")?;
    let issue = opened(&open(&host, "stale-owner/stale", &draft)?)?.clone();
    assert_eq!(issue.settlement(), Settlement::Unlisted);
    assert_eq!(issue.number(), 1);
    // Scan, create, read #1, then the eight settle reads.
    assert_eq!(calls_on("stale-owner")?.len(), 11);
    Ok(())
}

// ── J2 ─────────────────────────────────────────────────────────────────────

#[test]
fn a_run_without_the_issue_grant_is_blocked_before_gh_starts() -> TestResult {
    let host = Host::builder("acme")?
        .domains(&SHIPPED_ISSUES)
        .grants(GrantSet::empty().grant(Cap::sys()).grant(Cap::net()))
        .default_deadline(Duration::from_secs(120))
        .build()?;
    let draft = IssueDraft::new("j2", "Never opened", "")?;
    let report = open(&host, "j2-owner/accept", &draft)?;
    let refusal = match report.result() {
        Err(refusal) => refusal,
        Ok(issue) => return Err(format!("expected a blocked run, got {issue:?}").into()),
    };
    let short: Vec<Cap> = match *refusal {
        FlowError::Blocked { ref deficit, .. } => deficit
            .shortages()
            .map(|shortage| shortage.required().clone())
            .collect(),
        ref other => return Err(format!("expected a blocked run, got {other:?}").into()),
    };
    assert_eq!(short, [Cap::new(ISSUE_CREATE)], "the write, by name, alone");
    Ok(())
}

#[test]
fn the_shipped_constructor_refuses_a_target_that_is_not_a_repository() -> TestResult {
    let host = Host::builder("acme")?
        .domains(&SHIPPED_ISSUES)
        .grants(full_grant())
        .default_deadline(Duration::from_secs(120))
        .build()?;
    let draft = IssueDraft::new("j2b", "Never opened", "")?;
    let report = open(&host, "not-a-repository", &draft)?;
    assert!(
        matches!(
            *failed(&report)?,
            BotError::IncompleteSpec {
                field: "target",
                ..
            }
        ),
        "{:?}",
        report.result()
    );
    Ok(())
}

// ── J3 ─────────────────────────────────────────────────────────────────────

#[test]
fn a_second_run_with_the_same_key_finds_the_first_issue() -> TestResult {
    let host = fake_host("acme", full_grant())?;
    let draft = IssueDraft::new("j3", "Opened once", "")?;
    let first = open(&host, "j3-owner/accept", &draft)?;
    let second = open(&host, "j3-owner/accept", &draft)?;
    assert_eq!(opened(&first)?.settlement(), Settlement::Created);
    assert_eq!(opened(&second)?.settlement(), Settlement::Found);
    assert_eq!(opened(&second)?.number(), opened(&first)?.number());
    assert_eq!(creates_on("j3-owner")?, 1, "the rerun created nothing");
    Ok(())
}

#[test]
fn a_lost_answer_is_reconciled_by_reading_back_and_never_reposted() -> TestResult {
    let host = fake_host("acme", full_grant())?;
    let draft = IssueDraft::new("lost", "Answer lost", "")?;
    let first = open(&host, "lost-owner/drop", &draft)?;
    assert_eq!(opened(&first)?.settlement(), Settlement::Reconciled);
    let again = open(&host, "lost-owner/drop", &draft)?;
    assert_eq!(opened(&again)?.settlement(), Settlement::Found);
    assert_eq!(creates_on("lost-owner")?, 1, "one create across both runs");
    Ok(())
}

#[test]
fn a_create_that_landed_nowhere_is_indeterminate_not_failed() -> TestResult {
    let host = fake_host("acme", full_grant())?;
    let draft = IssueDraft::new("refused", "Refused by the receiver", "")?;
    let report = open(&host, "refused-owner/refuse", &draft)?;
    assert!(
        matches!(
            *failed(&report)?,
            BotError::EffectIndeterminate { ref domain, .. } if domain == "gh::issue"
        ),
        "{:?}",
        report.result()
    );
    // Read, create, read back: the one create, then the read that settles it.
    assert_eq!(creates_on("refused-owner")?, 1);
    assert_eq!(calls_on("refused-owner")?.len(), 3);
    Ok(())
}

// ── Credentials and keys ───────────────────────────────────────────────────

#[test]
fn a_refused_credential_is_a_typed_rejection_and_creates_nothing() -> TestResult {
    let host = fake_host("acme", full_grant())?;
    let draft = IssueDraft::new("deny", "Never opened", "")?;
    let report = open(&host, "deny-owner/deny", &draft)?;
    assert!(
        matches!(*failed(&report)?, BotError::CredentialRejected { .. }),
        "{:?}",
        report.result()
    );
    assert_eq!(creates_on("deny-owner")?, 0);
    Ok(())
}

#[test]
fn a_create_refused_for_its_credential_is_not_read_back() -> TestResult {
    let host = fake_host("acme", full_grant())?;
    let draft = IssueDraft::new("denycreate", "Never opened", "")?;
    let report = open(&host, "dc-owner/denycreate", &draft)?;
    assert!(
        matches!(*failed(&report)?, BotError::CredentialRejected { .. }),
        "{:?}",
        report.result()
    );
    // GitHub refused the write, so nothing landed and nothing is reconciled.
    assert_eq!(calls_on("dc-owner")?.len(), 2);
    Ok(())
}

#[test]
fn a_key_already_on_an_issue_with_another_title_is_refused() -> TestResult {
    let host = fake_host("acme", full_grant())?;
    let first = IssueDraft::new("shared", "First title", "")?;
    let second = IssueDraft::new("shared", "Second title", "")?;
    assert_eq!(
        opened(&open(&host, "key-owner/accept", &first)?)?.settlement(),
        Settlement::Created
    );
    let report = open(&host, "key-owner/accept", &second)?;
    assert!(
        matches!(*failed(&report)?, BotError::DomainError { ref cause, .. } if cause.contains("a key names one issue")),
        "{:?}",
        report.result()
    );
    assert_eq!(creates_on("key-owner")?, 1);
    Ok(())
}

#[test]
fn a_draft_github_would_refuse_is_refused_before_any_call() -> TestResult {
    let refused =
        |draft: Result<IssueDraft, GhError>| matches!(draft, Err(GhError::IssueDraft { .. }));
    assert!(refused(IssueDraft::new("", "t", "")));
    assert!(refused(IssueDraft::new("k".repeat(65), "t", "")));
    assert!(refused(IssueDraft::new("a--b>", "t", "")));
    assert!(refused(IssueDraft::new("k", "   ", "")));
    assert!(refused(IssueDraft::new("k", "two\nlines", "")));
    assert!(refused(IssueDraft::new("k", "t".repeat(257), "")));
    assert!(refused(IssueDraft::new("k", "t", "b".repeat(65_536))));
    // The bounds themselves are GitHub's, so each is admitted exactly.
    let key = "k".repeat(64);
    let room = 65_536 - format!("\n\n<!-- lgwks-issue:{key} -->").chars().count();
    let at_bounds = IssueDraft::new(key.as_str(), "t".repeat(256), "b".repeat(room))?;
    assert_eq!(at_bounds.body().chars().count(), 65_536);
    assert!(
        at_bounds
            .body()
            .ends_with(&format!("<!-- lgwks-issue:{key} -->"))
    );
    Ok(())
}
