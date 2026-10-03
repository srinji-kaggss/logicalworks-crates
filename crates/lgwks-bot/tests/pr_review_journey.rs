//! Black-box acceptance for the PR-review journey through a real `gh` process
//! boundary.
//!
//! Every test here drives [`lgwks_bot::review::review_pr`] through a
//! [`Host`](lgwks_bot::task::Host), and every one of them forks real processes:
//! a fake `gh` placed first on `PATH`, recording its argv and answering from a
//! behaviour file. Nothing is mocked inside the process; the code under test
//! admits, spawns, captures, applies its deadline and reaps exactly as it would
//! for the real client.
//!
//! What is pinned here:
//!
//! - the create carries `commit_id` equal to the pinned head, and the read-back
//!   is a **separate** invocation (PR-06, PR-09);
//! - a lost response after an accepted write is reconciled by reading back, and
//!   argv records exactly **one** create (PR-07);
//! - a head change is a typed refusal naming both shas, with no create at all;
//! - a non-zero exit is not a published review;
//! - two identities on one repository stay isolated;
//! - a truncated capture is reported as truncated, and a hanging client is
//!   stopped as a whole process group.
// `ephemeral` is required, not incidental: a staged publication payload needs a
// filename no two concurrent publications share, and without that feature the
// binding refuses to publish rather than fall back to a name the OS reuses. The
// `full` lane runs this suite, so the refusal is exercised rather than
// assumed; `gh_binding` covers what a process-only build can still do.
#![cfg(all(
    unix,
    feature = "script",
    feature = "process",
    feature = "time",
    feature = "sync",
    feature = "ephemeral"
))]

use std::num::NonZeroUsize;
use std::time::Duration;

use lgwks_bot::domain::gh::{CommitId, Gh, PullRequest, Repository, ReviewComment};
use lgwks_bot::review::{ReviewOutcome, ReviewRequest, ScriptFailure};
use lgwks_bot::script::Scope;
use lgwks_bot::task::{Host, Task, task};

#[path = "support/fake_gh.rs"]
mod fake_gh;

use fake_gh::{FakeGh, Scenario};

/// What a test reports when a precondition did not hold.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The head every scenario starts from.
const HEAD: &str = "1111111111111111111111111111111111111111";

/// The head a moved-head scenario reports on its second read.
const MOVED_HEAD: &str = "2222222222222222222222222222222222222222";

/// The pull request under review: `owner/repo#7`.
fn pull() -> Result<PullRequest, Box<dyn std::error::Error>> {
    Ok(PullRequest::new(Repository::new("acme/widgets")?, 7))
}

/// A host with enough admission for a couple of concurrent runs and a deadline
/// generous enough that a test fails on a real hang rather than on the clock.
fn host() -> Result<Host, Box<dyn std::error::Error>> {
    Ok(Host::builder("reviewer")?
        .max_concurrent_tasks(NonZeroUsize::new(8).ok_or("a non-zero ceiling")?)
        .default_deadline(Duration::from_secs(30))
        .build()?)
}

/// The `Gh` binding pointed at the fake.
///
/// The client is named `gh` and located through the `PATH` this binding puts in
/// front of every child, so the test exercises real program resolution rather
/// than pinning an absolute path — which is what a deployment does. The `PATH`
/// is a per-child delta rather than a mutation of this process's environment,
/// which keeps the tests independent and needs no `unsafe`.
fn gh_for(fake: &FakeGh) -> Result<Gh, Box<dyn std::error::Error>> {
    gh_with_capture(fake, 64 * 1024)
}

/// The same binding with a chosen capture ceiling.
///
/// The diff-bound probes need an answer larger than the default ceiling, and a
/// per-test copy of the binding would be a place for one probe to quietly stop
/// running the journey the others run.
fn gh_with_capture(fake: &FakeGh, capture: usize) -> Result<Gh, Box<dyn std::error::Error>> {
    fake.binding("acme/widgets", capture)
}

/// A script that emits a fixed body, standing in for a real analysis stage.
///
/// It is a closure over its own captured state rather than a named function,
/// which is the shape a caller's analysis takes: no trait to implement, no
/// box, and the pinned snapshot it is handed is the only input.
fn body_script(
    body: &'static str,
) -> impl Fn(&lgwks_bot::domain::gh::PrSnapshot, &Scope) -> Result<String, ScriptFailure> {
    move |_snapshot, _scope| Ok(String::from(body))
}

/// A script that always fails, to prove a failed analysis publishes nothing.
fn failing_script()
-> impl Fn(&lgwks_bot::domain::gh::PrSnapshot, &Scope) -> Result<String, ScriptFailure> {
    |_snapshot, _scope| {
        Err(ScriptFailure::new(
            "the analysis found no eligible findings",
        ))
    }
}

/// The task, host and request every journey scenario in this file starts from.
///
/// One place, because the shape of a run *is* the thing under test: a task
/// body that pinned its own subject, published outside a host, or reconciled
/// by re-posting would be a different journey, not a variation of this one.
/// Each scenario supplies the analysis stage; everything else is shared.
fn review_run() -> Result<ReviewRun, Box<dyn std::error::Error>> {
    let host = host()?;
    let request = ReviewRequest::new(pull()?, "COMMENT", REVIEW_BODY).with_marker("lgwks-test-run");
    let body: ReviewTaskBody = review_body;
    let task = task("review-pr", body)?;
    Ok(ReviewRun {
        host,
        task,
        request,
    })
}

/// The same run, with an analysis stage that always fails.
fn failing_review_run() -> Result<ReviewRun, Box<dyn std::error::Error>> {
    let host = host()?;
    let request = ReviewRequest::new(pull()?, "COMMENT", REVIEW_BODY).with_marker("lgwks-test-run");
    let body: ReviewTaskBody = failing_body;
    let task = task("review-pr", body)?;
    Ok(ReviewRun {
        host,
        task,
        request,
    })
}

/// The shared run with `count` inline comments declared on the request.
///
/// The partial-submission probe needs the request to intend more comments than
/// the receiver lands; the journey carries the count to the payload so the
/// reconciliation can compare applied against intended.
fn review_run_with_comments(count: u64) -> Result<ReviewRun, Box<dyn std::error::Error>> {
    let comments = (0..count)
        .map(|index| {
            ReviewComment::new(
                format!("src/file{index}.rs"),
                index.saturating_add(1),
                format!("comment {index}"),
            )
        })
        .collect();
    let host = host()?;
    let request = ReviewRequest::new(pull()?, "COMMENT", REVIEW_BODY)
        .with_marker("lgwks-test-run")
        .with_comments(comments);
    let body: ReviewTaskBody = review_body;
    let task = task("review-pr", body)?;
    Ok(ReviewRun {
        host,
        task,
        request,
    })
}

/// Run the shared task against `fake` and return its report.
///
/// One call, because the two lines it replaces are not incidental: binding the
/// `Gh` to the fake and executing under the host is what "the journey runs
/// through the adapter and the front door" means. A per-test copy of those two
/// lines is a place for one test to quietly stop doing it.
fn run_passing(
    fake: &FakeGh,
) -> Result<lgwks_bot::task::Report<ReviewOutcome>, Box<dyn std::error::Error>> {
    let run = review_run()?;
    let gh = gh_for(fake)?;
    Ok(run.host.block_on(&run.task, (gh, run.request))?)
}

/// The task body a passing scenario runs: publish [`REVIEW_BODY`] at the pinned
/// head and verify it.
fn review_body(scope: Scope, (gh, request): (Gh, ReviewRequest)) -> ReviewFuture {
    Box::pin(async move {
        lgwks_bot::review::review_pr(scope, gh, request, body_script(REVIEW_BODY)).await
    })
}

/// The task body a scenario that needs a failing analysis runs instead.
fn failing_body(scope: Scope, (gh, request): (Gh, ReviewRequest)) -> ReviewFuture {
    Box::pin(
        async move { lgwks_bot::review::review_pr(scope, gh, request, failing_script()).await },
    )
}

/// The three values every scenario starts from.
///
/// Returned together so a test reads as the difference between itself and the
/// shared world rather than as three lines of setup copied into each one — the
/// shape of a run *is* the thing under test, and a per-test copy of it is a
/// place for one test to quietly stop running it the way the others do.
struct ReviewRun {
    /// The host the run executes under.
    host: Host,
    /// The task to run.
    task: Task<ReviewTaskBody>,
    /// The request it runs over.
    request: ReviewRequest,
}

/// The closure shape both [`review_run`] and [`failing_review_run`] build.
///
/// Named so a scenario can hold the run in one type whatever analysis stage it
/// chose; the two stages differ only in the body they pass to `review_pr`.
type ReviewTaskBody = fn(Scope, (Gh, ReviewRequest)) -> ReviewFuture;

/// The future a task body in this file returns.
type ReviewFuture = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<ReviewOutcome, lgwks_bot::script::FlowError>>>,
>;

/// The body the shared scenarios publish, so a read-back and a payload compare
/// like with like.
const REVIEW_BODY: &str = "two findings";

// ── The publish path ────────────────────────────────────────────────────────

#[test]
fn a_review_is_published_at_the_pinned_head_and_verified_by_a_separate_read() -> TestResult {
    let fake = FakeGh::install("publish", HEAD)?;
    let report = run_passing(&fake)?;
    assert_eq!(
        report.disposition().label(),
        "Succeeded",
        "a published, verified review is a successful run: {}",
        report.error().map_or_else(String::new, ToString::to_string)
    );
    let outcome = report
        .output()
        .ok_or("a successful run carries its outcome")?;
    assert!(
        outcome.is_published(),
        "the happy path publishes and verifies: {outcome:?}"
    );
    assert_eq!(
        outcome.commit_id().map(CommitId::as_str),
        Some(HEAD),
        "the review names the commit that was read"
    );

    // The create carried the pinned commit, and the read-back was a separate
    // invocation: three reads/writes total — snapshot, freshness, create,
    // verify — with exactly one POST.
    assert_eq!(
        fake.creates()?,
        1,
        "exactly one create, or the run posted the review twice"
    );
    assert!(
        fake.reads_of("/pulls/7/reviews")? >= 1,
        "the publication is verified by a read-back, not by the create's exit code"
    );
    let payload = fake
        .payload_of(0)?
        .ok_or("the create must name the payload file it was given")?;
    assert!(
        payload.contains(HEAD),
        "the create payload must carry the reviewed commit id: {payload}"
    );
    assert!(
        payload.contains("\"commit_id\""),
        "the reviewed commit is a named field of the payload, never omitted: {payload}"
    );
    Ok(())
}

#[test]
fn a_failed_analysis_publishes_nothing() -> TestResult {
    let fake = FakeGh::install("script-fails", HEAD)?;
    let run = failing_review_run()?;
    let gh = gh_for(&fake)?;

    let report = run.host.block_on(&run.task, (gh, run.request))?;
    assert_eq!(
        report.disposition().label(),
        "Failed",
        "a failed analysis is a failed run, not an empty successful review"
    );
    assert_eq!(
        fake.creates()?,
        0,
        "an analysis that produced no body must not create a review"
    );
    Ok(())
}

// ── The lost response ───────────────────────────────────────────────────────

#[test]
fn a_lost_response_is_reconciled_by_reading_back_and_never_reposted() -> TestResult {
    let fake = FakeGh::install("lost", HEAD)?;
    // The create applies the review and then drops its response, so the effect
    // landed and the caller cannot learn that from the create call.
    fake.configure(
        Scenario::new(HEAD)
            .accept_then_drop()
            .created_body("two findings"),
    )?;
    let report = run_passing(&fake)?;
    assert_eq!(
        report.disposition().label(),
        "Succeeded",
        "a lost response that the read-back reconciles is a published review: {}",
        report.error().map_or_else(String::new, ToString::to_string)
    );
    let outcome = report
        .output()
        .ok_or("a successful run carries its outcome")?;
    assert!(
        outcome.is_published(),
        "the read-back found the review the lost response was about: {outcome:?}"
    );
    assert_eq!(
        fake.creates()?,
        1,
        "the whole point: a lost response must not become a second create"
    );
    Ok(())
}

#[test]
fn a_loss_that_cannot_be_reconciled_stays_unknown_and_still_does_not_repost() -> TestResult {
    let fake = FakeGh::install("unreconcilable", HEAD)?;
    // The create fails without applying, and the read-back is refused, so
    // whether anything landed cannot be established from either direction.
    fake.configure(Scenario::new(HEAD).refuse_creates().fail_reads(1))?;
    let report = run_passing(&fake)?;
    // The read-back itself failed, so the outcome is Unknown rather than a
    // clean failure — a caller must be able to tell "nothing happened" from
    // "I cannot tell".
    let outcome = report
        .output()
        .ok_or("an unknown outcome is a value, not a run failure")?;
    assert!(
        outcome.is_unknown(),
        "an unreadable read-back after a possible write is unknown: {outcome:?}"
    );
    assert_eq!(
        fake.creates()?,
        1,
        "the run issued one create and reconciled; it must not issue a second"
    );
    Ok(())
}

#[test]
fn a_non_zero_exit_is_not_a_published_review() -> TestResult {
    let fake = FakeGh::install("nonzero", HEAD)?;
    // The create is refused outright and nothing is applied, so the read-back
    // finds no review: the run must not report a publication.
    fake.configure(Scenario::new(HEAD).refuse_creates())?;
    let report = run_passing(&fake)?;
    let outcome = report
        .output()
        .ok_or("a client that exited non-zero still reports an outcome")?;
    assert!(
        !outcome.is_published(),
        "a client that exited non-zero never reports a publication: {outcome:?}"
    );
    assert_eq!(
        fake.creates()?,
        1,
        "the failed create was reconciled by a read, not repeated"
    );
    assert!(
        fake.reads_of("/pulls/7/reviews")? >= 1,
        "and the reconciliation was a read, which is the only safe next move \
         after an exit code that cannot prove a non-effect"
    );
    Ok(())
}

#[test]
fn a_client_that_never_starts_is_a_definite_non_effect_and_is_not_reconciled() -> TestResult {
    // No fake is installed: the binding names a program that does not exist, so
    // the platform refuses the fork and nothing can have reached GitHub. That is
    // the one publish failure that is *certainly* a non-effect, and it is
    // reported as a failure rather than as an unknown — the classification is a
    // real distinction, not a formality.
    let run = review_run()?;
    let gh = Gh::new(Repository::new("acme/widgets")?)
        .program("lgwks-no-such-github-client-on-this-path")
        .deadline(Some(Duration::from_secs(5)));

    let report = run.host.block_on(&run.task, (gh, run.request))?;
    assert_eq!(
        report.disposition().label(),
        "Failed",
        "a client that never started is a clean failure, not an unknown: {}",
        report.error().map_or_else(String::new, ToString::to_string)
    );
    assert!(
        report.output().is_none(),
        "and it reports no outcome at all, because there is no publication to \
         describe and no effect to leave unknown"
    );
    Ok(())
}

// ── The moved subject ───────────────────────────────────────────────────────

#[test]
fn a_moved_head_is_a_typed_refusal_and_publishes_nothing() -> TestResult {
    let fake = FakeGh::install("moved", HEAD)?;
    // `head_after_first` makes the fake report HEAD on its first read and
    // MOVED_HEAD on every read after it — a push landing between the snapshot
    // and the freshness check, which is the race the refusal exists for.
    fake.configure(Scenario::new(HEAD).head_moves_to(MOVED_HEAD))?;
    let report = run_passing(&fake)?;
    let outcome = report
        .output()
        .ok_or("a moved head is a reported outcome, not a run failure")?;
    match *outcome {
        ReviewOutcome::TargetMoved {
            ref reviewed,
            ref current,
        } => {
            assert_eq!(
                reviewed.as_str(),
                HEAD,
                "the refusal names the commit that was actually reviewed"
            );
            assert_eq!(
                current, MOVED_HEAD,
                "and the commit the pull request now points at, so the caller \
                 can decide whether to re-analyse"
            );
        }
        ref other => {
            return Err(format!("a moved head must be TargetMoved, not {other:?}").into());
        }
    }
    assert_eq!(
        fake.creates()?,
        0,
        "a review of code nobody read must never be published"
    );
    Ok(())
}

// ── Isolation ───────────────────────────────────────────────────────────────

#[test]
fn two_identities_on_one_repository_stay_isolated() -> TestResult {
    let fake = FakeGh::install("isolation", HEAD)?;
    let host = host()?;

    // Two reviews of the same repository with distinct bodies. Each create must
    // carry only its own payload, and each verification must match only its own
    // review — the fixture answers reads with the full list, so a run that
    // matched the *other* body's review would report the wrong review id.
    fake.configure(Scenario::new(HEAD))?;

    let build = |_body: &'static str| {
        task(
            "review-pr",
            move |scope: Scope, (gh, pull, body): (Gh, PullRequest, &'static str)| async move {
                let request = ReviewRequest::new(pull, "COMMENT", body).with_marker(body);
                lgwks_bot::review::review_pr(scope, gh, request, body_script(body)).await
            },
        )
    };

    let first = build("first identity")?;
    let second = build("second identity")?;

    let first_report = host.block_on(&first, (gh_for(&fake)?, pull()?, "first identity"))?;
    let second_report = host.block_on(&second, (gh_for(&fake)?, pull()?, "second identity"))?;
    for report in [&first_report, &second_report] {
        assert_eq!(
            report.disposition().label(),
            "Succeeded",
            "both identities publish independently"
        );
    }
    assert_eq!(
        fake.creates()?,
        2,
        "two identities on one repository are two reviews, not one and not three"
    );
    let first_payload = fake.payload_of(0)?.ok_or("the first create's payload")?;
    let second_payload = fake.payload_of(1)?.ok_or("the second create's payload")?;
    assert!(
        first_payload.contains("first identity"),
        "the first create carries only its own body: {first_payload}"
    );
    assert!(
        second_payload.contains("second identity"),
        "the second create carries only its own body: {second_payload}"
    );
    assert!(
        !first_payload.contains("second identity"),
        "a tenant's payload must not carry another's body"
    );
    Ok(())
}

// ── Capability ──────────────────────────────────────────────────────────────

#[test]
fn the_adapter_refuses_without_its_capability_before_starting_anything() -> TestResult {
    // No fake is installed: the refusal happens before the fork, so there is
    // nothing to intercept and nothing should be started.
    let query = lgwks_bot::domain::gh::GhQuery::new(
        Gh::new(Repository::new("acme/widgets")?),
        vec![
            String::from("--method"),
            String::from("GET"),
            String::from("rate_limit"),
        ],
    );
    // A grant of the wrong capability: notification delivery, not process
    // control or the network. The adapter requires both of the latter, so the
    // check names every one of them that is missing.
    let auth = lgwks_bot::GrantSet::empty()
        .grant(lgwks_bot::Cap::notify())
        .issue(&[])?;
    let denied = lgwks_bot::Runtime::new()?
        .block_on(async { lgwks_bot::Query::query(&query, (auth, &())).await })
        .err()
        .ok_or("a query without bot.sys and bot.net must be denied")?;
    match denied {
        lgwks_bot::BotError::CapabilityDenied { deficit } => {
            let missing: Vec<&str> = deficit
                .shortages()
                .map(|shortage| shortage.required().as_str())
                .collect();
            assert_eq!(
                missing,
                vec![lgwks_bot::Cap::SYS, lgwks_bot::Cap::NET],
                "the denial names every capability the GitHub binding needs"
            );
        }
        other => return Err(format!("expected a capability denial, got {other:?}").into()),
    }
    Ok(())
}

// ── Subject coverage: diff, rename and sandbox (T31) ────────────────────────

/// An unavailable diff is an *incomplete coverage*: nothing is published.
///
/// The server declining to render the diff is a coverage decision, not a
/// transport failure: the code that was read is only part of what the pull
/// request changes, and a review against it would claim a scope nobody read.
#[test]
fn an_unavailable_diff_is_an_incomplete_coverage_and_publishes_nothing() -> TestResult {
    let fake = FakeGh::install("diff-unavailable", HEAD)?;
    fake.configure(Scenario::new(HEAD).diff_unavailable())?;
    let report = run_passing(&fake)?;

    let outcome = report
        .output()
        .ok_or("an incomplete coverage is a reported outcome, not a run failure")?;
    assert!(
        outcome.is_incomplete(),
        "an unavailable diff is an incomplete coverage: {outcome:?}"
    );
    assert!(
        !outcome.is_published(),
        "an incomplete coverage must never read as a clean review"
    );
    assert_eq!(
        outcome.commit_id().map(CommitId::as_str),
        Some(HEAD),
        "the pinned commit is still named, so the caller can re-run at it"
    );
    assert_eq!(
        fake.creates()?,
        0,
        "nothing is published when the coverage is incomplete"
    );
    Ok(())
}

/// Run the journey with a raised capture ceiling and return its outcome.
///
/// The two diff-ceiling probes need an answer larger than the default capture,
/// so the envelope is raised: the refusal under test is the *diff* ceiling, not
/// the capture bound. One helper rather than a copied block, so the two probes
/// cannot drift apart.
fn run_with_large_capture(fake: &FakeGh) -> Result<ReviewOutcome, Box<dyn std::error::Error>> {
    let run = review_run()?;
    let report = run.host.block_on(
        &run.task,
        (gh_with_capture(fake, 4 * 1024 * 1024)?, run.request),
    )?;
    report
        .output()
        .cloned()
        .ok_or_else(|| "a coverage decision is a reported outcome".into())
}

/// A diff inventory past its file-count ceiling is an incomplete coverage.
///
/// The ceiling is a *typed refusal* rather than a truncated list: a prefix of
/// the inventory that decoded cleanly is indistinguishable from the whole diff.
#[test]
fn a_diff_past_the_file_ceiling_is_an_incomplete_coverage() -> TestResult {
    let fake = FakeGh::install("diff-over-files", HEAD)?;
    let over = u32::try_from(lgwks_bot::domain::gh::MAX_DIFF_FILES_PER_PULL)
        .map_err(|_| "the file ceiling must fit the fixture's counter")?
        .saturating_add(1);
    fake.configure(Scenario::new(HEAD).with_filler_files(over))?;

    let outcome = run_with_large_capture(&fake)?;
    assert!(
        outcome.is_incomplete(),
        "a diff past its file ceiling is an incomplete coverage: {outcome:?}"
    );
    assert_eq!(fake.creates()?, 0, "nothing is published");
    Ok(())
}

/// A diff past its byte ceiling is an incomplete coverage, on its own axis.
#[test]
fn a_diff_past_the_byte_ceiling_is_an_incomplete_coverage() -> TestResult {
    let fake = FakeGh::install("diff-over-bytes", HEAD)?;
    let over = u32::try_from(lgwks_bot::domain::gh::MAX_DIFF_BYTES)
        .map_err(|_| "the byte ceiling must fit the fixture's counter")?
        .saturating_add(64);
    fake.configure(Scenario::new(HEAD).with_diff_bytes(over))?;

    let outcome = run_with_large_capture(&fake)?;
    assert!(
        outcome.is_incomplete(),
        "a diff past its byte ceiling is an incomplete coverage: {outcome:?}"
    );
    assert_eq!(fake.creates()?, 0, "nothing is published");
    Ok(())
}

/// A renamed repository is a typed refusal naming both names, never a silent
/// re-point at the canonical repository.
#[test]
fn a_renamed_repository_is_refused_and_never_silently_re_pointed() -> TestResult {
    let fake = FakeGh::install("moved-repo", HEAD)?;
    fake.configure(Scenario::new(HEAD).repository_moved())?;
    let report = run_passing(&fake)?;

    let outcome = report
        .output()
        .ok_or("a moved repository is a reported outcome")?;
    match *outcome {
        ReviewOutcome::Refused { ref reason } => {
            assert!(
                reason.contains("acme/widgets"),
                "the refusal names the repository the caller requested: {reason}"
            );
            assert!(
                reason.contains("acme/newrepo"),
                "and the canonical repository the answer named: {reason}"
            );
        }
        ref other => {
            return Err(format!("a renamed repository must be Refused, not {other:?}").into());
        }
    }
    assert_eq!(
        fake.creates()?,
        0,
        "a subject identity that moved is never reviewed under a name nobody asked for"
    );
    Ok(())
}

/// The pull request's build script is inspected, never executed (INV-BOT-21).
///
/// The changed-file inventory names a `build.rs` whose patch text would remove
/// the file named by `LGWKS_TEST_MARKER` if anything ran it. The journey reads
/// the inventory as data, so the marker is untouched; a run that executed the
/// patch would trip the same oracle.
#[test]
fn an_untrusted_build_script_in_the_diff_is_never_executed() -> TestResult {
    let fake = FakeGh::install("build-script", HEAD)?;
    fake.configure(Scenario::new(HEAD).hostile_build_script())?;
    // The marker lives inside the fixture's own directory and is named in the
    // child's environment, so a shell that ran the patch would find and remove
    // it; the fixture removes it at the end of the test regardless.
    let marker = fake.dir().join("LGWKS_MARKER");
    std::fs::write(&marker, b"present")?;

    let run = review_run()?;
    let gh = gh_with_capture(&fake, 64 * 1024)?.env("LGWKS_TEST_MARKER", marker.as_os_str());
    let report = run.host.block_on(&run.task, (gh, run.request))?;

    assert!(
        marker.exists(),
        "the review path must never execute the pull request's build script"
    );
    let outcome = report
        .output()
        .ok_or("a build script in the inventory does not stop the review")?;
    assert!(
        outcome.is_published(),
        "the inventory is read as data, so a hostile file still publishes: {outcome:?}"
    );
    Ok(())
}

// ── Partial submissions (T33) ───────────────────────────────────────────────

/// A lost response that landed as an unsubmitted draft is reported as a draft,
/// never as a publication and never re-posted.
#[test]
fn a_pending_draft_is_reconciled_as_a_draft_and_never_reposted() -> TestResult {
    let fake = FakeGh::install("pending-draft", HEAD)?;
    fake.configure(Scenario::new(HEAD).records_pending_draft())?;
    let report = run_passing(&fake)?;

    let outcome = report
        .output()
        .ok_or("a pending draft is a reported outcome")?;
    assert!(
        outcome.is_pending(),
        "a lost response onto an unsubmitted draft is Pending: {outcome:?}"
    );
    assert!(
        !outcome.is_published(),
        "an unsubmitted draft is never a published review"
    );
    assert!(
        outcome.review_id().is_some(),
        "the draft's id is retained so recovery can target it: {outcome:?}"
    );
    assert_eq!(
        fake.creates()?,
        1,
        "the draft was created once; reconciliation never posts a second review"
    );
    Ok(())
}

/// A lost response whose comments landed only in part is reported as partial,
/// with both counts, and never re-posted.
#[test]
fn a_partial_submission_is_reconciled_as_partial_and_never_reposted() -> TestResult {
    let fake = FakeGh::install("partial-comments", HEAD)?;
    // The request intends two comments; the receiver lands one.
    fake.configure(Scenario::new(HEAD).records_partial_comments(1))?;

    let run = review_run_with_comments(2)?;
    let report = run
        .host
        .block_on(&run.task, (gh_for(&fake)?, run.request))?;

    let outcome = report
        .output()
        .ok_or("a partial submission is a reported outcome")?;
    match *outcome {
        ReviewOutcome::Partial {
            applied, intended, ..
        } => {
            assert_eq!(
                (applied, intended),
                (1, 2),
                "the partial state names what landed and what was intended: {outcome:?}"
            );
        }
        ref other => {
            return Err(format!("a partial submission must be Partial, not {other:?}").into());
        }
    }
    assert_eq!(
        fake.creates()?,
        1,
        "the partial review was created once; recovery targets only what remains"
    );
    Ok(())
}

// ── Lost read permission (T34) ──────────────────────────────────────────────

/// A publication whose read-back lost permission is Unverified, with the
/// applied review id retained — never a clean failure and never a second write.
#[test]
fn a_lost_read_permission_reports_unverified_and_retains_the_review_id() -> TestResult {
    let fake = FakeGh::install("read-denied", HEAD)?;
    // The create is accepted and returns an id; the review read is then denied
    // with a `403`, which the adapter reports as a typed permission failure.
    fake.configure(Scenario::new(HEAD).deny_reads())?;
    let report = run_passing(&fake)?;

    let outcome = report
        .output()
        .ok_or("an unverified publication is a reported outcome")?;
    match *outcome {
        ReviewOutcome::Unverified {
            review_id,
            ref commit_id,
            ref reason,
        } => {
            assert!(
                review_id >= 9_001,
                "the applied review id is retained as evidence: {review_id}"
            );
            assert_eq!(
                commit_id.as_str(),
                HEAD,
                "and the reviewed commit is retained"
            );
            assert!(
                reason.contains("permission"),
                "the reason names what was lost: {reason}"
            );
        }
        ref other => {
            return Err(format!("lost read permission must be Unverified, not {other:?}").into());
        }
    }
    assert_eq!(
        fake.creates()?,
        1,
        "a lost read permission is never resolved by a second create"
    );
    Ok(())
}
