//! The seeded families of the review path: the fault space, the two-identity
//! and two-repository isolation families, retention, cancellation and
//! duplicate submission.
//!
//! Split out of `sim_review_path.rs` by responsibility (the size law). The fault
//! enum, the run record and the harness stay in the binary's root, and every
//! family here drives them through the same `run_seed` and `review_task`, so a
//! family cannot be testing a different journey than the others.

use std::num::NonZeroUsize;
use std::time::Duration;

use lgwks_bot::domain::gh::{CommitId, Gh, PullRequest, Repository};
use lgwks_bot::review::{ReviewOutcome, ReviewRequest};
use lgwks_bot::script::Scope;
use lgwks_bot::task::{Host, task};

use super::fake_gh::{FakeGh, HANG_BOUND, Scenario};
use super::{
    BODY, CAPTURE, FIRST_REVIEW_ID, Fault, HEAD, LAST_REVIEW_ID, MOVED, Run, SEEDS, TestResult,
    assert_the_invariant, capture_for, error_text, gh_for, host, pull, request, review_task,
    run_seed, scenario_for, trace_hash, why_unknown,
};

// ── The families ────────────────────────────────────────────────────────────

/// The verified family: publication observed against publication not observed.
///
/// The two halves run the same seeds over the same fault space and differ only
/// in the expectation, which is the point — the run that reports `Published`
/// under `clean` reports `Unknown` under a fault that cannot produce an
/// observation, and the difference is the observation, not the code path.
#[test]
fn verified_and_not_observed_r32() -> TestResult {
    for index in 0..SEEDS {
        let (fault, run) = run_seed(index, 0)?;
        assert_the_invariant(fault, &run)?;

        let published = run.report.output().is_some_and(ReviewOutcome::is_published);
        assert_eq!(
            published,
            fault.can_be_verified(),
            "seed {index} ({}): a verified publication must be reported exactly \
             when the fault allows an independent read to succeed",
            fault.label()
        );
    }
    Ok(())
}

/// The exit-failure family: every way the client can fail and answer nothing.
///
/// Distinct from the family above because it is the discrimination that matters
/// most: a client that exited non-zero must not be read as "nothing happened",
/// while a client that never started must be. Both are failures to a shell; only
/// one is a proven non-effect.
#[test]
fn gh_exit_failures_r32() -> TestResult {
    let mut seen = std::collections::HashSet::new();
    for index in 0..SEEDS {
        let (fault, run) = run_seed(index, 0)?;
        assert_the_invariant(fault, &run)?;
        if !matches!(
            fault,
            Fault::CreateRefused | Fault::ReadsRefused | Fault::ClientAbsent
        ) {
            continue;
        }
        seen.insert(fault);

        // Every one of these is a failure of the client, and none may report a
        // publication. The one that never started is a proven non-effect, so it
        // carries no outcome at all; the others left the effect unobserved and
        // must say so rather than claim a clean failure.
        let outcome = run.report.output();
        assert!(
            outcome.is_none_or(ReviewOutcome::is_unknown),
            "seed {index} ({}): a client that failed must report either nothing \
             or an unknown effect, never a publication: {outcome:?}",
            fault.label()
        );
        if fault == Fault::ClientAbsent {
            assert_eq!(
                run.report.disposition().label(),
                "Failed",
                "a client that never started is a clean failure: it cannot have \
                 written anything, so there is no effect to leave unknown"
            );
            assert!(
                !run.argv.contains("--method POST"),
                "and it must never have reached a create:\n{}",
                run.argv
            );
        } else {
            assert!(
                outcome.is_some_and(ReviewOutcome::is_unknown),
                "a client that ran and failed leaves the effect unobserved, so \
                 the run reports Unknown rather than a clean failure"
            );
        }
    }
    assert_eq!(
        seen.len(),
        3,
        "every client-failure mode the family names must be reached: {seen:?}"
    );
    Ok(())
}

/// The deadline family: a client stopped mid-call leaves the effect unobserved.
///
/// The deadline is the case where "did it write?" is genuinely unknowable — the
/// child was mid-request when the supervisor stopped it — so the run must read
/// back rather than assert either answer. This drives the real deadline, not a
/// simulated one: the fixture hangs and the supervisor stops the group.
#[test]
fn deadline_stop_r32() -> TestResult {
    for index in 0..8u64 {
        let fake = FakeGh::install("deadline", HEAD)?;
        // A hang of 30s against a 400ms adapter deadline: the deadline, not the
        // client, is what ends the call.
        fake.configure(Scenario::new(HEAD).hangs_for(30))?;
        let host = host()?;
        let job = review_task()?;
        let gh = Gh::new(Repository::new("acme/widgets")?)
            .program(fake.program())
            .capture_limit(NonZeroUsize::new(CAPTURE).ok_or("a non-zero limit")?)
            .deadline(Some(Duration::from_millis(400)))
            .env("PATH", fake.search_path()?);
        let started = std::time::Instant::now();
        let report = host.block_on(&job, (gh, request(7)?))?;
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(15),
            "seed {index}: the deadline stopped the call rather than the client \
             being waited out: {elapsed:?}"
        );
        assert_eq!(
            report.disposition().label(),
            "Failed",
            "seed {index}: a run whose client never answered is a failure, not a \
             silent success: {}",
            error_text(&report)
        );
        assert!(
            report.output().is_none_or(ReviewOutcome::is_unknown),
            "seed {index}: a hung client leaves the effect unobserved, so the \
             run reports unknown at worst: {:?}",
            report.output()
        );
        assert!(
            fake.creates()? <= 1,
            "seed {index}: a stopped run publishes at most once"
        );
    }
    Ok(())
}

/// The moved-head family: a head that changed is a refusal, at every seed.
///
/// The fault is the same at every seed and the *run* is not, so this family
/// covers the change landing between the snapshot and the freshness read rather
/// than one fixed interleaving.
#[test]
fn head_moved_between_snapshot_and_publish_r32() -> TestResult {
    for index in 0..SEEDS {
        let fake = FakeGh::install("moved-sim", HEAD)?;
        fake.configure(Scenario::new(HEAD).head_moves_to(MOVED))?;
        let host = host()?;
        let job = review_task()?;
        let report = host.block_on(&job, (gh_for(&fake, CAPTURE)?, request(7)?))?;

        let Some(outcome) = report.output() else {
            return Err(format!(
                "seed {index}: a moved head is a reported outcome, not a run failure: {}",
                error_text(&report)
            )
            .into());
        };
        match *outcome {
            ReviewOutcome::TargetMoved {
                ref reviewed,
                ref current,
            } => {
                assert_eq!(
                    reviewed.as_str(),
                    HEAD,
                    "seed {index}: the refusal names the commit that was read"
                );
                assert_eq!(
                    current, MOVED,
                    "seed {index}: and the commit the pull request now points at, \
                     so the caller can decide whether to re-analyse"
                );
            }
            _ => {
                return Err(format!("a moved head must be TargetMoved, not {outcome:?}").into());
            }
        }
        assert_eq!(
            fake.creates()?,
            0,
            "seed {index}: a review of code nobody read must never be created"
        );
    }
    Ok(())
}

/// The malformed family: an answer that is not what was asked for is refused.
///
/// Every one of these is a case where the client exited zero and said nothing
/// useful, which is precisely the case a naive implementation turns into a
/// confident "no matching review". A partial history reported as the history is
/// the same defect as an empty one reported as the truth.
#[test]
fn malformed_and_oversized_answers_r32() -> TestResult {
    let mut seen = std::collections::HashSet::new();
    for index in 0..SEEDS {
        let fault = Fault::for_index(index, 6);
        if !matches!(
            fault,
            Fault::ListOverCeiling
                | Fault::ListNotJson
                | Fault::ListTruncated
                | Fault::AnswerOversized
        ) {
            continue;
        }
        seen.insert(fault);

        let fake = FakeGh::install(fault.label(), HEAD)?;
        fake.configure(scenario_for(fault, HEAD)?)?;
        let host = host()?;
        let job = review_task()?;
        // The ceiling comes from the fault rather than from the test, so the
        // difference between "refused as oversized" and "refused as malformed"
        // is the fault and not the ceiling.
        let capture = capture_for(fault);
        let report = host.block_on(&job, (gh_for(&fake, capture)?, request(7)?))?;

        assert!(
            !report.output().is_some_and(ReviewOutcome::is_published),
            "seed {index} ({}): an answer that is not the review history must \
             never verify a publication",
            fault.label()
        );
        assert!(
            report.output().is_none_or(ReviewOutcome::is_unknown),
            "seed {index} ({}): and the effect stays unobserved rather than being \
             reported as a clean failure: {:?}",
            fault.label(),
            report.output()
        );
        let reason = why_unknown(&report);
        assert!(
            !reason.is_empty(),
            "seed {index} ({}): a run that could not establish its publication \
             must name why, wherever it recorded it",
            fault.label()
        );
    }
    assert_eq!(
        seen.len(),
        4,
        "every malformed-answer mode the family names must be reached: {seen:?}"
    );
    Ok(())
}

/// The review-ceiling family: a real publication whose *verification* is refused.
///
/// This is the case the other families cannot express. The create lands, so the
/// receiver holds the review; the read-back that would confirm it is refused,
/// because the pull request's history is longer than the adapter will decode. A
/// run that reported `Published` here would be reporting the create's own exit
/// code as proof, which is the one thing `INV-BOT-80` forbids — and it would do
/// so on a run whose effect genuinely landed, which is exactly when the mistake
/// is most tempting.
#[test]
fn a_publication_the_ceiling_cannot_verify_stays_unknown() -> TestResult {
    for index in 0..8u64 {
        let fake = FakeGh::install("ceiling-sim", HEAD)?;
        fake.configure(scenario_for(Fault::ListOverCeiling, HEAD)?)?;
        let host = host()?;
        let job = review_task()?;
        let report = host.block_on(
            &job,
            (
                gh_for(&fake, capture_for(Fault::ListOverCeiling))?,
                request(7)?,
            ),
        )?;

        // The effect really landed: the receiver was asked for exactly one
        // create, and it accepted it.
        assert_eq!(
            fake.creates()?,
            1,
            "seed {index}: the create itself is not in doubt — only its \
             verification is"
        );
        assert_eq!(
            fake.received()?.len(),
            1,
            "seed {index}: and the receiver holds the payload the adapter sent"
        );
        // The verification is unavailable, so the run must say so.
        assert!(
            report.output().is_some_and(ReviewOutcome::is_unknown),
            "seed {index}: a publication the ceiling cannot verify must stay \
             Unknown, never Published: {:?}",
            report.output()
        );
        assert!(
            !report.output().is_some_and(ReviewOutcome::is_published),
            "seed {index}: and it must never be reported as published on the \
             create's own success"
        );
        // One read was attempted, and it was the read that refused.
        assert!(
            fake.reads_of("/pulls/7/reviews")? >= 1,
            "seed {index}: the run must have tried the independent read rather \
             than concluding from the create"
        );
        let reason = why_unknown(&report);
        assert!(
            reason.contains("past the ceiling"),
            "seed {index}: the reason must name the ceiling, so a reader knows \
             the review exists and the *verification* is what is missing: {reason}"
        );
    }
    Ok(())
}

/// The same seed must produce the same trace, through the real path.
#[test]
fn same_seed_same_trace_hash_r32() -> TestResult {
    for index in 0..SEEDS {
        assert_replays(
            index,
            Fault::label,
            run_seed(index, 0)?,
            run_seed(index, 0)?,
        );
    }
    Ok(())
}

/// Two runs of seed `index` selected the same fault, hashed to the same trace
/// and issued the same argv: the replay is exact, not approximate.
pub(super) fn assert_replays<F: Copy + PartialEq + std::fmt::Debug>(
    index: u64,
    label: fn(F) -> &'static str,
    (first_fault, first): (F, Run),
    (second_fault, second): (F, Run),
) {
    assert_eq!(
        first_fault, second_fault,
        "seed {index} must select the same fault twice"
    );
    assert_eq!(
        trace_hash(&first.trace()),
        trace_hash(&second.trace()),
        "seed {index} ({}) must replay exactly: {:?} vs {:?}",
        label(first_fault),
        first.trace(),
        second.trace()
    );
    // The argv comparison is the stronger claim: the receiver was told the
    // same things, in the same order, by both runs.
    assert_eq!(
        first.argv,
        second.argv,
        "seed {index} ({}) must issue the same calls twice",
        label(first_fault)
    );
}

/// Every fault the journey names is reachable, so the families are not
/// silently covering one case each.
///
/// One seed per fault rather than a multiple of the seed width: the coverage
/// question is "is this fault reachable at all", and running the wide families
/// again would answer it more slowly, not more convincingly. Two seeds per
/// fault, so a fault that only fires on one parity cannot pass by luck.
#[test]
fn every_fault_is_reachable() -> TestResult {
    let mut faults = std::collections::HashSet::new();
    let mut outcomes = std::collections::HashSet::new();
    for fault_index in 0..Fault::VARIANTS {
        for repeat in 0..2u64 {
            let (fault, run) = run_seed(fault_index, repeat)?;
            assert_the_invariant(fault, &run)?;
            faults.insert(fault.label());
            if let Some(outcome) = run.report.output() {
                outcomes.insert(match *outcome {
                    ReviewOutcome::Published { .. } => "published",
                    ReviewOutcome::Unknown { .. } => "unknown",
                    ReviewOutcome::TargetMoved { .. } => "moved",
                    ReviewOutcome::Refused { .. } => "refused",
                    _ => "unmodelled",
                });
            }
        }
    }
    for expected in Fault::LABELS {
        assert!(
            faults.contains(expected),
            "the families must reach {expected:?}; they reached {faults:?}"
        );
    }
    for expected in ["published", "unknown", "moved"] {
        assert!(
            outcomes.contains(expected),
            "the families must reach {expected:?}; they reached {outcomes:?}"
        );
    }
    Ok(())
}

/// One identity's review of pull request 7 through `fake`: it publishes
/// `body`, marked with `body`, and returns the outcome it verified.
///
/// The body is bound at declaration: the closure is what a real consumer
/// supplies, so the cross-talk question is "does this identity verify its own
/// review", not "does the harness remember which body it passed".
fn run_tenant(
    fake: &FakeGh,
    host: &Host,
    path: &std::ffi::OsStr,
    body: &'static str,
) -> Result<ReviewOutcome, Box<dyn std::error::Error>> {
    let job = task(
        "review-pr",
        move |scope: Scope, (gh, request): (Gh, ReviewRequest)| async move {
            lgwks_bot::review::review_pr(scope, gh, request, move |_s, _scope| {
                Ok(String::from(body))
            })
            .await
        },
    )?;
    let gh = Gh::new(Repository::new("acme/widgets")?)
        .program(fake.program())
        .capture_limit(NonZeroUsize::new(CAPTURE).ok_or("a non-zero limit")?)
        .deadline(Some(HANG_BOUND))
        .env("PATH", path);
    let request = ReviewRequest::new(pull(7)?, "COMMENT", body).with_marker(body);
    let report = host.block_on(&job, (gh, request))?;
    report
        .output()
        .cloned()
        .ok_or_else(|| "each identity must produce an outcome".into())
}

/// A fresh `gh` fixture named `tag` on which the first and then the second
/// identity each review pull request 7: the fixture, for its read-back, and
/// the two outcomes, in that order.
pub(super) fn two_identities(
    tag: &str,
) -> Result<(FakeGh, ReviewOutcome, ReviewOutcome), Box<dyn std::error::Error>> {
    let fake = FakeGh::install(tag, HEAD)?;
    fake.configure(Scenario::new(HEAD))?;
    let host = host()?;
    let path = fake.search_path()?;
    let first = run_tenant(&fake, &host, &path, "first identity")?;
    let second = run_tenant(&fake, &host, &path, "second identity")?;
    Ok((fake, first, second))
}

/// The tenant family: two identities on one pull request number.
///
/// Each identity publishes a different body, so a read-back that matched the
/// *other* identity's review would report the wrong review. That is the
/// cross-talk defect, and it is only visible when the bodies differ — two
/// identical bodies would pass by accident.
#[test]
fn two_tenants_on_one_pull_request_r32() -> TestResult {
    for index in 0..SEEDS {
        let (fake, first, second) = two_identities("tenants")?;

        assert!(
            first.is_published() && second.is_published(),
            "seed {index}: both identities publish independently: {first:?} {second:?}"
        );
        assert_eq!(
            first.commit_id().map(CommitId::as_str),
            Some(HEAD),
            "seed {index}: both publish at the commit they read"
        );
        // Each identity's verification must name its own review. The two ids
        // come from two distinct records, so this compares across records rather
        // than across one.
        assert_ne!(
            first.review_id(),
            second.review_id(),
            "seed {index}: two identities must verify two distinct reviews, not \
             one review twice: {first:?} {second:?}"
        );
        let payloads = fake.received()?;
        assert_eq!(
            payloads.len(),
            2,
            "seed {index}: exactly two creates reached the receiver"
        );
        assert_eq!(
            payloads
                .iter()
                .filter(|body| body.contains("first identity"))
                .count(),
            1,
            "seed {index}: exactly one payload carries the first identity's body"
        );
        assert_eq!(
            payloads
                .iter()
                .filter(|body| body.contains("second identity"))
                .count(),
            1,
            "seed {index}: exactly one payload carries the second identity's body"
        );
    }
    Ok(())
}

/// The retention family: what the adapter holds is bounded by its declaration,
/// not by what the child wrote.
///
/// Measured against a real child rather than asserted from a constant, because
/// the claim is about a process the test does not control.
#[test]
fn retention_is_bounded_by_the_declared_ceiling() -> TestResult {
    for flood in [100u32, 1_000, 10_000] {
        let fake = FakeGh::install("flood-sim", HEAD)?;
        fake.configure(Scenario::new(HEAD).floods(flood))?;
        let host = host()?;
        let job = review_task()?;
        let capture = 1024;
        let report = host.block_on(&job, (gh_for(&fake, capture)?, request(7)?))?;

        assert!(
            !report.output().is_some_and(ReviewOutcome::is_published),
            "flood {flood}: an answer past the ceiling cannot verify anything"
        );
        let failure = error_text(&report);
        assert!(
            failure.contains("truncat") || failure.contains("could not"),
            "flood {flood}: the refusal must name the truncation, so a reader \
             knows the answer was incomplete rather than wrong: {failure}"
        );
        lgwks_std::trace::info!(
            flood = flood,
            capture = capture,
            failure = %failure,
            "retention: the refusal a past-ceiling answer produced"
        );
    }
    Ok(())
}

/// The cancellation family: a run stopped mid-flight keeps its effect knowledge.
///
/// The seed chooses *when* the stop arrives, which is the whole question: a run
/// cancelled before the create leaves nothing, and a run cancelled after it may
/// have left a review whose outcome nobody observed. Both must be reported —
/// neither may report a publication, and neither may issue a second create on
/// the way out. A cancellation that quietly looked like success is the failure
/// this family exists to catch.
#[test]
fn cancellation_under_faults_r16() -> TestResult {
    for index in 0..16u64 {
        // Alternate the phase the stop lands in: before the first read, or after
        // the create. Both are reachable by a real caller cancelling under load,
        // and they are the two answers a caller most needs to tell apart.
        let after_create = index % 2 == 0;
        let fake = FakeGh::install("cancel", HEAD)?;
        fake.configure(
            Scenario::new(HEAD)
                .created_body(BODY)
                .hangs_for(if after_create { 3 } else { 0 }),
        )?;
        let host = host()?;
        let job = review_task()?;

        if after_create {
            // Run to the publication, then stop the host: the create's response
            // is still in flight when the stop arrives.
            let report = host.block_on(&job, (gh_for(&fake, CAPTURE)?, request(7)?))?;
            host.cancel();
            assert!(
                !report.output().is_some_and(ReviewOutcome::is_published),
                "seed {index}: a run whose client hung must not report a \
                 publication: {:?}",
                report.output()
            );
            assert!(
                fake.creates()? <= 1,
                "seed {index}: a cancelled run publishes at most once"
            );
        } else {
            // Stop the host before the run is even admitted.
            host.cancel();
            let report = host.block_on(&job, (gh_for(&fake, CAPTURE)?, request(7)?))?;
            assert_ne!(
                report.disposition().label(),
                "Succeeded",
                "seed {index}: a run cancelled before it starts must not report \
                 success: {}",
                error_text(&report)
            );
            assert_eq!(
                fake.creates()?,
                0,
                "seed {index}: a run cancelled before it starts creates nothing"
            );
        }
    }
    Ok(())
}

/// The duplicate-submission family: the same request twice still creates once
/// per *run*, and each run is reported honestly.
///
/// This is the shape a retrying caller produces, and the question is not whether
/// two runs are allowed to each publish — they are, that is two reviews — but
/// whether either run ever creates twice. The counter at the receiver is the
/// only place that question has an answer.
#[test]
fn duplicate_submission_r16() -> TestResult {
    for index in 0..16u64 {
        let fake = FakeGh::install("duplicate", HEAD)?;
        fake.configure(Scenario::new(HEAD).created_body(BODY))?;
        let host = host()?;
        let job = review_task()?;

        // The same request, run twice: once cleanly, once with the create's
        // response dropped. The second is what a caller retrying after a
        // timeout would produce, and it is exactly the case the reconciliation
        // read exists for rather than a second create.
        let clean = host.block_on(&job, (gh_for(&fake, CAPTURE)?, request(7)?))?;
        fake.configure(Scenario::new(HEAD).created_body(BODY).accept_then_drop())?;
        let retried = host.block_on(&job, (gh_for(&fake, CAPTURE)?, request(7)?))?;

        assert_eq!(
            fake.creates()?,
            2,
            "seed {index}: two runs are two reviews — one each, never one run \
             posting twice"
        );
        assert!(
            clean.output().is_some_and(ReviewOutcome::is_published),
            "seed {index}: the first run publishes and verifies: {:?}",
            clean.output()
        );
        assert!(
            retried.output().is_some_and(ReviewOutcome::is_published),
            "seed {index}: the retried run reconciles its lost response and \
             reports published, not unknown: {:?}",
            retried.output()
        );
        // Both runs published, and the receiver holds two distinct records with two
        // distinct ids — that part is checked against the receiver below, not
        // inferred from the outcomes. What the *outcomes* must do is stay
        // truthful: neither may report a review the receiver never applied, and
        // reconciling a lost response onto an earlier identical review is
        // correct, not a defect — the two runs asked for the same body at the
        // same commit, so the records are interchangeable by every field the
        // verification reads.
        let first_id = clean.output().and_then(ReviewOutcome::review_id);
        let second_id = retried.output().and_then(ReviewOutcome::review_id);
        let applied = fake.received()?;
        assert_eq!(
            applied.len(),
            2,
            "seed {index}: the receiver really did apply two reviews, so a run \
             reporting one id for both would be reporting one review twice"
        );
        // Both runs must name a review, and both ids must lie in the range the
        // receiver actually handed out (`next_review_id` and up), so a run that
        // invented one — or reported the id from some other world — would fall
        // outside it.
        let first = first_id.ok_or_else(|| {
            format!("seed {index}: the first run named no review (second: {second_id:?})")
        })?;
        let second = second_id.ok_or_else(|| {
            format!("seed {index}: the second run named no review (first: {first})")
        })?;
        assert!(
            (FIRST_REVIEW_ID..=LAST_REVIEW_ID).contains(&first)
                && (FIRST_REVIEW_ID..=LAST_REVIEW_ID).contains(&second),
            "seed {index}: both ids must come from the receiver's own range \
             {FIRST_REVIEW_ID}..={LAST_REVIEW_ID}: {first} and {second}"
        );
    }
    Ok(())
}

/// The repository-isolation family: two repositories on one host stay apart.
///
/// The tenant family covers two identities on one pull request; this is the
/// other direction, and it is the one a shared host makes possible: a host that
/// leaked its `PATH` or its binding between runs would send one repository's
/// review to another's pull request, and the run would still report success.
#[test]
fn two_repositories_on_one_host_r16() -> TestResult {
    for index in 0..16u64 {
        let first_fake = FakeGh::install("repo-a", HEAD)?;
        let second_fake = FakeGh::install("repo-b", MOVED)?;
        first_fake.configure(Scenario::new(HEAD).created_body(BODY))?;
        second_fake.configure(Scenario::new(MOVED).created_body(BODY))?;
        let host = host()?;

        // One host, two bindings, two repositories. The second names the second
        // repository, so a run that resolved the wrong binding would read the
        // wrong head — and since the two heads differ, it would either publish
        // against the wrong commit or refuse.
        let job = task(
            "review-pr",
            |scope: Scope, (gh, repo, request): (Gh, String, ReviewRequest)| async move {
                lgwks_bot::review::review_pr(scope, gh, request, move |snapshot, _scope| {
                    Ok(format!("{repo}: {BODY} at {}", snapshot.head_sha()))
                })
                .await
            },
        )?;

        let run =
            |repo: &str, fake: &FakeGh| -> Result<ReviewOutcome, Box<dyn std::error::Error>> {
                let gh = Gh::new(Repository::new(repo)?)
                    .program(fake.program())
                    .capture_limit(NonZeroUsize::new(CAPTURE).ok_or("a non-zero limit")?)
                    .deadline(Some(HANG_BOUND))
                    .env("PATH", fake.search_path()?);
                let pull = PullRequest::new(Repository::new(repo)?, 7);
                let request = ReviewRequest::new(pull, "COMMENT", BODY).with_marker(repo);
                let report = host.block_on(&job, (gh, String::from(repo), request))?;
                report
                    .output()
                    .cloned()
                    .ok_or_else(|| format!("{repo} produced no outcome").into())
            };

        let first = run("acme/widgets", &first_fake)?;
        let second = run("other/gadgets", &second_fake)?;

        assert!(
            first.is_published(),
            "seed {index}: the first repository publishes: {first:?}"
        );
        assert!(
            second.is_published(),
            "seed {index}: the second repository publishes: {second:?}"
        );
        // The two heads differ, so a crossed binding would show up as one run
        // publishing at the other's commit.
        assert_eq!(
            first.commit_id().map(CommitId::as_str),
            Some(HEAD),
            "seed {index}: the first run published at its own head"
        );
        assert_eq!(
            second.commit_id().map(CommitId::as_str),
            Some(MOVED),
            "seed {index}: the second run published at its own head, not the \
             first's"
        );
        // Each receiver saw only its own create.
        assert_eq!(first_fake.creates()?, 1, "seed {index}: receiver A's count");
        assert_eq!(
            second_fake.creates()?,
            1,
            "seed {index}: receiver B's count"
        );
        assert!(
            first_fake
                .received()?
                .iter()
                .all(|payload| !payload.contains("other/gadgets")),
            "seed {index}: no payload crossed into the other repository"
        );
    }
    Ok(())
}
