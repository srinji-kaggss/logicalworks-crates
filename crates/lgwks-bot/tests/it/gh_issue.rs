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
//! | J3: a rerun after a lost answer or a kill opens nothing new | `a_second_run_with_the_same_key_finds_the_first_issue`, `a_lost_answer_is_reconciled_by_reading_back_and_never_reposted`, `a_real_kill_between_the_post_and_its_reply_leaves_one_issue` |

#![cfg(all(unix, feature = "script", feature = "process", feature = "ephemeral"))]

use std::error::Error;
use std::time::{Duration, Instant};

use lgwks_bot::domain::gh::{
    GhError, ISSUE_CREATE, IssueDraft, OpenIssue, OpenedIssue, Settlement,
};
use lgwks_bot::rt::clock::Clock;
use lgwks_bot::script::{FlowError, Scope};
use lgwks_bot::task::{Host, MAX_TASK_DEADLINE, Report, task};
use lgwks_bot::{BotError, Cap, DispatchCertainty, GrantSet, domains};

use crate::fake_gh_issues::{calls_on, creates_on, held_on, hide, hold, issue_on_fake, land_later};
use crate::sim_keyed::drive;

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

/// A host for `tenant` over the fake receiver, on a clock [`open`] drives.
///
/// Every new key waits out GitHub's in-flight bound before its create, so a
/// host on the wall clock would spend eleven real seconds per create; on this
/// clock the wait is spent by the driver and costs nothing.
pub(crate) fn fake_host(tenant: &str, grants: GrantSet) -> Result<Host, Box<dyn Error>> {
    fake_host_with_clock(tenant, grants, Clock::virtual_at(Duration::ZERO))
}

/// A host for `tenant` whose runs measure every wait on `clock`.
///
/// The deadline is the host's own day-long ceiling: a test moves the clock a
/// second at a time while a fork is outstanding, and a 120-second deadline
/// would turn that move into a timeout the action never asked for.
pub(crate) fn fake_host_with_clock(
    tenant: &str,
    grants: GrantSet,
    clock: Clock,
) -> Result<Host, Box<dyn Error>> {
    Ok(Host::builder(tenant)?
        .domains(&FAKE_ISSUES)
        .grants(grants)
        .clock(clock)
        .default_deadline(MAX_TASK_DEADLINE)
        .build()?)
}

/// Run the flow once on `host`, driving the host's own clock.
///
/// A wall-clock host is never advanced: the driver's advance is refused and
/// the run's waits, if any, are real.
pub(crate) fn open(
    host: &Host,
    repo: &str,
    draft: &IssueDraft,
) -> Result<Report<OpenedIssue>, Box<dyn Error>> {
    open_on_clock(host, &host.clock().clone(), repo, draft)
}

/// Run the flow once on `host`, advancing `clock` until the run settles.
///
/// Every wait the action makes is on this clock, so a run parked on one makes
/// no progress until [`drive`] moves it; a run waiting on a child process
/// burns no logical time.
pub(crate) fn open_on_clock(
    host: &Host,
    clock: &Clock,
    repo: &str,
    draft: &IssueDraft,
) -> Result<Report<OpenedIssue>, Box<dyn Error>> {
    let opening = task(
        "opening",
        |scope: Scope, (repo, draft): (String, IssueDraft)| async move {
            open_issue(&scope, &repo, draft).await
        },
    )?;
    Ok(drive(
        clock,
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

// ── J1 ─────────────────────────────────────────────────────────────────────

/// A new key finds nothing, waits out GitHub's in-flight bound on the flow's
/// clock, finds nothing again, and creates once.
#[test]
fn a_flow_opens_an_issue_and_reads_it_back() -> TestResult {
    let host = fake_host("acme", full_grant())?;
    let draft = IssueDraft::new("j1", "The harness opened this", "Body text.")?;
    let started = Instant::now();
    let issue = opened(&open(&host, "j1-owner/accept", &draft)?)?.clone();
    let elapsed = started.elapsed();
    assert_eq!(issue.settlement(), Settlement::Created);
    assert_eq!(issue.number(), 1);
    assert_eq!(issue.url(), "https://github.com/j1-owner/accept/issues/1");
    assert_eq!(creates_on("j1-owner")?, 1, "one create");
    // Two finds (a listing, then two 404s past it, each) around the fence,
    // and the create, confirmed from its own answer.
    assert_eq!(calls_on("j1-owner")?.len(), 7);
    assert!(
        host.clock().now() >= Duration::from_secs(11),
        "the fence waits GitHub's 10 s request bound plus 1 s on the flow's clock: {:?}",
        host.clock().now()
    );
    assert!(
        elapsed < Duration::from_secs(11),
        "the fence must not wait in real time: {elapsed:?}"
    );
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
/// #658. The rerun's listing still hides #1, so its scan reads the number past
/// the listing's newest and finds the key there, without waiting for the
/// listing to catch up.
#[test]
fn an_immediate_rerun_under_a_lagging_listing_finds_the_issue_by_number() -> TestResult {
    let host = fake_host("acme", full_grant())?;
    let draft = IssueDraft::new("lagged", "Listed late", "")?;
    let first = open(&host, "lag-owner/lag", &draft)?;
    let again = open(&host, "lag-owner/lag", &draft)?;
    assert_eq!(opened(&first)?.settlement(), Settlement::Created);
    assert_eq!(opened(&again)?.settlement(), Settlement::Found);
    assert_eq!(opened(&again)?.number(), 1);
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
        "the rerun read #1 by its number: {calls:?}"
    );
    // The first run's two finds and the create; the rerun's listing (still
    // lagging) and its read of #1.
    assert_eq!(calls.len(), 9);
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

/// A listing that never shows the newest issue cannot hide it: the create is
/// reported from its own answer, and the rerun reads past the listing by
/// number and answers the same issue.
#[test]
fn a_listing_that_never_shows_the_issue_is_read_past_by_number() -> TestResult {
    let host = fake_host("acme", full_grant())?;
    let draft = IssueDraft::new("stale", "Never listed", "")?;
    let first = opened(&open(&host, "stale-owner/stale", &draft)?)?.clone();
    let again = opened(&open(&host, "stale-owner/stale", &draft)?)?.clone();
    assert_eq!(first.settlement(), Settlement::Created);
    assert_eq!(again.settlement(), Settlement::Found);
    assert_eq!(again.number(), first.number());
    assert_eq!(creates_on("stale-owner")?, 1);
    assert_eq!(calls_on("stale-owner")?.len(), 9);
    Ok(())
}

/// A key held by an issue the listing has not reached, behind an unlisted
/// issue of another key, is still found: the scan reads every number past
/// the listing until GitHub has none.
#[test]
fn the_scan_reads_past_an_unlisted_issue_of_another_key() -> TestResult {
    let host = fake_host("acme", full_grant())?;
    let ours = IssueDraft::new("ours", "Ours", "")?;
    let theirs = IssueDraft::new("theirs", "Theirs", "")?;
    // `lag-5` hides each created issue from the next five listing reads.
    assert_eq!(
        opened(&open(&host, "past-owner/lag-5", &theirs)?)?.number(),
        1
    );
    assert_eq!(
        opened(&open(&host, "past-owner/lag-5", &ours)?)?.number(),
        2
    );
    let again = opened(&open(&host, "past-owner/lag-5", &ours)?)?.clone();
    assert_eq!(again.settlement(), Settlement::Found);
    assert_eq!(again.number(), 2);
    assert_eq!(creates_on("past-owner")?, 2);
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

/// A real `SIGKILL` between the POST and its reply, then a rerun with the same
/// key, leaves exactly one issue.
///
/// The child is the fake client, and it kills itself with `SIGKILL` after it
/// has held the issue and before it prints the answer — the same instant a
/// `kill -9` of `gh` catches. What the supervisor reports is a signal death,
/// not an exit code, so the action cannot know whether the create landed; it
/// reads back and reports `Reconciled`. The rerun then finds the key by its
/// own pre-create scan, and issues no second POST.
///
/// The settles run on a clock this test owns: the kill is real, the waits are
/// not.
#[test]
fn a_real_kill_between_the_post_and_its_reply_leaves_one_issue() -> TestResult {
    let clock = Clock::virtual_at(Duration::ZERO);
    let host = fake_host_with_clock("acme", full_grant(), clock.clone())?;
    let draft = IssueDraft::new("killed", "Killed mid-create", "")?;
    let first = open_on_clock(&host, &clock, "kill-owner/kill", &draft)?;
    assert_eq!(
        opened(&first)?.settlement(),
        Settlement::Reconciled,
        "a signal death is an unknown outcome, settled by the read-back"
    );
    let again = open_on_clock(&host, &clock, "kill-owner/kill", &draft)?;
    assert_eq!(opened(&again)?.settlement(), Settlement::Found);
    assert_eq!(
        opened(&again)?.number(),
        opened(&first)?.number(),
        "the rerun answers the issue the killed run created"
    );
    assert_eq!(creates_on("kill-owner")?, 1, "one POST across both runs");
    assert_eq!(
        held_on("kill-owner", "kill")?,
        1,
        "the killed create left one issue, and the rerun left none"
    );
    Ok(())
}

/// A lost answer whose issue never appears is indeterminate after the
/// read-back's settle, and the settle is spent on the flow's clock.
///
/// The schedule — 250 ms doubling to four seconds, 15.75 s across eight
/// scans — runs on a clock this test advances. The bound on real elapsed time
/// is below the schedule itself, so a wait that read the process timer would
/// fail it whatever the host's load.
#[test]
fn a_lost_answer_that_landed_nowhere_is_indeterminate_after_the_settle() -> TestResult {
    let clock = Clock::virtual_at(Duration::ZERO);
    let host = fake_host_with_clock("acme", full_grant(), clock.clone())?;
    let draft = IssueDraft::new("vanished", "Lost and never held", "")?;
    let started = Instant::now();
    let report = open_on_clock(&host, &clock, "vanish-owner/vanish", &draft)?;
    let elapsed = started.elapsed();
    assert!(
        matches!(
            *failed(&report)?,
            BotError::EffectIndeterminate { ref domain, .. } if domain == "gh::issue"
        ),
        "{:?}",
        report.result()
    );
    // Two finds around the fence, the create, then eight read-back finds of
    // three calls each (a listing and two 404s).
    assert_eq!(creates_on("vanish-owner")?, 1);
    assert_eq!(calls_on("vanish-owner")?.len(), 31);
    assert!(
        clock.now() >= Duration::from_millis(15_750),
        "the settle must spend its schedule on the flow's clock, which read {:?}",
        clock.now()
    );
    assert!(
        elapsed < Duration::from_millis(15_750),
        "the settle must not wait the schedule in real time: {elapsed:?}"
    );
    Ok(())
}

/// GitHub rejecting the create (`422`) landed nothing: the run reports the
/// rejection at once, reads nothing back, and a rerun may create.
#[test]
fn a_rejected_create_is_refused_at_once_and_not_read_back() -> TestResult {
    let host = fake_host("acme", full_grant())?;
    let draft = IssueDraft::new("rejected", "Rejected by the receiver", "")?;
    let report = open(&host, "rejected-owner/refuse", &draft)?;
    assert!(
        matches!(
            *failed(&report)?,
            BotError::DomainError { ref domain, ref cause, .. }
                if domain == "gh::issue" && cause.contains("rejected by GitHub (HTTP 422)")
        ),
        "{:?}",
        report.result()
    );
    assert_eq!(creates_on("rejected-owner")?, 1);
    assert_eq!(held_on("rejected-owner", "refuse")?, 0);
    assert_eq!(calls_on("rejected-owner")?.len(), 7);
    Ok(())
}

/// GitHub's other two rejections of a create, `400` and `410`, are refusals
/// too: nothing landed, nothing is read back, and the certainty a retry
/// policy reads is `Refused`.
#[test]
fn a_create_rejected_with_400_or_410_is_refused_at_once() -> TestResult {
    for status in ["400", "410"] {
        let owner = format!("rejected{status}-owner");
        let repo = format!("refuse-{status}");
        let host = fake_host("acme", full_grant())?;
        let draft = IssueDraft::new("rejected", "Rejected by the receiver", "")?;
        let report = open(&host, &format!("{owner}/{repo}"), &draft)?;
        let error = failed(&report)?;
        assert!(
            matches!(
                *error,
                BotError::DomainError { ref cause, .. }
                    if cause.contains(&format!("rejected by GitHub (HTTP {status})"))
            ),
            "{error:?}"
        );
        assert_eq!(error.dispatch_certainty(), DispatchCertainty::Refused);
        assert_eq!(held_on(&owner, &repo)?, 0);
        assert_eq!(calls_on(&owner)?.len(), 7, "no read-back after a rejection");
    }
    Ok(())
}

/// A read-back read that fails does not end the read-back: the next read
/// finds the issue the lost create left, and the run is reconciled.
#[test]
fn a_failed_read_back_read_does_not_end_the_read_back() -> TestResult {
    let host = fake_host("acme", full_grant())?;
    let draft = IssueDraft::new("flaky", "Read back through a failure", "")?;
    let report = open(&host, "flaky-owner/dropflaky", &draft)?;
    assert_eq!(opened(&report)?.settlement(), Settlement::Reconciled);
    assert_eq!(creates_on("flaky-owner")?, 1);
    assert_eq!(held_on("flaky-owner", "dropflaky")?, 1);
    Ok(())
}

// ── Finding what the listing has not shown ─────────────────────────────────

/// A Discussion's number inside the sequence answers `404` at an issue's
/// address, and does not end the find: the key's unlisted issue past it is
/// still read, and no duplicate is created.
#[test]
fn a_discussion_number_inside_the_sequence_does_not_end_the_find() -> TestResult {
    let host = fake_host("acme", full_grant())?;
    let ours = IssueDraft::new("ours", "Ours", "")?;
    let theirs = IssueDraft::new("theirs", "Theirs", "")?;
    // #1 is listed, #2 is a Discussion (no issue), #3 is ours and unlisted.
    hold("gap-owner", "accept", 1, &theirs)?;
    hold("gap-owner", "accept", 3, &ours)?;
    hide("gap-owner", "accept", 3, 99)?;
    let issue = opened(&open(&host, "gap-owner/accept", &ours)?)?.clone();
    assert_eq!(issue.settlement(), Settlement::Found);
    assert_eq!(issue.number(), 3);
    assert_eq!(creates_on("gap-owner")?, 0);
    Ok(())
}

/// An older issue can trail a newer one into the listing. The key's issue
/// below the listing's newest, not yet listed, is read by its number.
#[test]
fn an_unlisted_issue_below_a_listed_one_is_found() -> TestResult {
    let host = fake_host("acme", full_grant())?;
    let ours = IssueDraft::new("ours", "Ours", "")?;
    let theirs = IssueDraft::new("theirs", "Theirs", "")?;
    hold("below-owner", "accept", 1, &ours)?;
    hide("below-owner", "accept", 1, 99)?;
    hold("below-owner", "accept", 2, &theirs)?;
    let issue = opened(&open(&host, "below-owner/accept", &ours)?)?.clone();
    assert_eq!(issue.settlement(), Settlement::Found);
    assert_eq!(issue.number(), 1);
    assert_eq!(creates_on("below-owner")?, 0);
    Ok(())
}

/// A run killed after sending its create leaves that create in flight. A
/// rerun that finds nothing at first waits out GitHub's in-flight bound,
/// reads again, and finds the issue the killed create landed, instead of
/// creating a second.
#[test]
fn a_create_still_in_flight_from_a_killed_run_is_found_not_duplicated() -> TestResult {
    let host = fake_host("acme", full_grant())?;
    let draft = IssueDraft::new("inflight", "Sent by a killed run", "")?;
    // It lands on the fourth call: after the first find's three, before the
    // read that follows the fence is answered.
    land_later("flight-owner", "accept", &draft, 4)?;
    let issue = opened(&open(&host, "flight-owner/accept", &draft)?)?.clone();
    assert_eq!(issue.settlement(), Settlement::Found);
    assert_eq!(issue.number(), 1);
    assert_eq!(creates_on("flight-owner")?, 0, "the rerun created nothing");
    assert_eq!(held_on("flight-owner", "accept")?, 1);
    assert!(host.clock().now() >= Duration::from_secs(11));
    Ok(())
}

/// A repository whose listing trails by more than sixteen numbers refuses the
/// open rather than creating over a window it could not read.
#[test]
fn a_listing_more_than_sixteen_numbers_behind_refuses_the_open() -> TestResult {
    let host = fake_host("acme", full_grant())?;
    let theirs = IssueDraft::new("theirs", "Theirs", "")?;
    for number in 1..=17 {
        hold("behind-owner", "accept", number, &theirs)?;
        hide("behind-owner", "accept", number, 99)?;
    }
    let ours = IssueDraft::new("ours", "Ours", "")?;
    let report = open(&host, "behind-owner/accept", &ours)?;
    assert!(
        matches!(
            *failed(&report)?,
            BotError::DomainError { ref cause, .. } if cause.contains("more than 16 issue numbers")
        ),
        "{:?}",
        report.result()
    );
    assert_eq!(creates_on("behind-owner")?, 0);
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
    // GitHub refused the write, so nothing landed and nothing is read back:
    // the two finds around the fence, the create.
    assert_eq!(calls_on("dc-owner")?.len(), 7);
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
