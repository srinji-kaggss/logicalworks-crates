//! The sweeps and per-arm properties of the subject, coverage and
//! partial-submission family (T31, T33, T34).
//!
//! Split out of `sim_review_path.rs` by responsibility (the size law). The fault
//! space and the run these assert on live in `subject.rs`.

use lgwks_bot::domain::gh::{CommitId, Gh, PullRequest};
use lgwks_bot::review::ReviewOutcome;
use lgwks_bot::script::Scope;
use lgwks_bot::task::task;

use super::fake_gh::{FakeGh, Scenario};
use super::families::{assert_replays, two_identities};
use super::saturation::run_saturation_tier;
use super::subject::{
    SubjectDraw, SubjectFault, over_ceiling_draw, reason_of, run_subject_fault, run_subject_seed,
    subject_scenario,
};
use super::{
    CAPTURE, FIRST_REVIEW_ID, HEAD, LAST_REVIEW_ID, TestResult, band_family, error_text, gh_for,
    host, pull, request, review_task, sim,
};

/// The subject family: every fault reaches its own distinct outcome, and none
/// but the clean world reports a publication.
fn subject_coverage_and_partial_faults(band: sim::Band) -> TestResult {
    let mut seen = std::collections::HashSet::new();
    for index in band.seeds() {
        let (fault, run) = run_subject_seed(index)?;
        seen.insert(fault.label());

        assert!(
            run.creates <= 1,
            "seed {index} ({}): a run publishes at most once, never a duplicate post\n{}",
            fault.label(),
            run.argv
        );

        let Some(outcome) = run.report.output() else {
            let failure = error_text(&run.report);
            let refusal = Err(format!(
                "seed {index} ({}): a reported refusal, not a run failure: {failure}",
                fault.label()
            )
            .into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "subject_coverage_and_partial_faults: returning an error to the caller");
            return refusal;
        };
        assert_eq!(
            outcome.is_published(),
            fault.can_be_verified(),
            "seed {index} ({}): a verified publication is reported exactly when the \
             fault allows an independent read to succeed: {outcome:?}",
            fault.label()
        );
        match fault {
            SubjectFault::Clean => {}
            SubjectFault::Moved => assert!(
                matches!(outcome, ReviewOutcome::Refused { .. }),
                "seed {index}: a rename is a refusal: {outcome:?}"
            ),
            SubjectFault::DiffUnavailable
            | SubjectFault::DiffOverFiles
            | SubjectFault::DiffOverBytes => assert!(
                outcome.is_incomplete(),
                "seed {index} ({}): a diff that cannot be read whole is an incomplete \
                 coverage: {outcome:?}",
                fault.label()
            ),
            SubjectFault::PendingDraft => assert!(
                outcome.is_pending(),
                "seed {index}: a lost response onto a draft is Pending: {outcome:?}"
            ),
            SubjectFault::PartialComments => {
                let draw = SubjectDraw::for_seed(index);
                let &ReviewOutcome::Partial {
                    applied, intended, ..
                } = outcome
                else {
                    let refusal = Err(format!(
                        "seed {index}: a submission with fewer comments than intended \
                     is Partial: {outcome:?}"
                    )
                    .into());
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "subject_coverage_and_partial_faults: returning an error to the caller");
                    return refusal;
                };
                assert_eq!(
                    (applied, intended),
                    (
                        usize::try_from(draw.applied)?,
                        usize::try_from(draw.intended)?
                    ),
                    "seed {index}: Partial reports the counts that landed and were intended"
                );
            }
            SubjectFault::ReadDenied => assert!(
                outcome.is_unverified(),
                "seed {index}: a lost read permission after a create is Unverified: {outcome:?}"
            ),
        }
    }
    for expected in SubjectFault::LABELS {
        assert!(
            seen.contains(expected),
            "the family must reach {expected:?}; it reached {seen:?}"
        );
    }
    Ok(())
}

/// The same seed must produce the same subject trace, through the real path.
fn same_seed_same_trace_hash_subject(band: sim::Band) -> TestResult {
    for index in band.seeds() {
        assert_replays(
            index,
            SubjectFault::label,
            run_subject_seed(index)?,
            run_subject_seed(index)?,
        );
    }
    Ok(())
}

/// One seed of the two-identity sweep: each identity publishes and verifies only its own review.
fn identities_seed(index: u64) -> TestResult {
    let (fake, first, second) = two_identities("subject-tenants")?;
    assert!(
        first.is_published() && second.is_published(),
        "seed {index}: both identities publish independently: {first:?} {second:?}"
    );
    assert_ne!(
        first.review_id(),
        second.review_id(),
        "seed {index}: two identities must verify two distinct reviews: {first:?} {second:?}"
    );
    let payloads = fake.received()?;
    assert_eq!(
        payloads
            .iter()
            .filter(|payload| payload.contains("first identity"))
            .count(),
        1,
        "seed {index}: exactly one payload carries the first identity's body"
    );
    assert_eq!(
        payloads
            .iter()
            .filter(|payload| payload.contains("second identity"))
            .count(),
        1,
        "seed {index}: exactly one payload carries the second identity's body"
    );
    Ok(())
}

/// Two identities on one pull request each verify only their own review.
fn two_identities_subject(band: sim::Band) -> TestResult {
    for index in band.seeds() {
        identities_seed(index)?;
    }
    Ok(())
}

// ── Band declarations ────────────────────────────────────────────────────────
//
// Eight seeds a band, each band starting on a multiple of eight, so every
// subject band reaches all eight faults with its own draws.

band_family::band_family! {
    subject_coverage_and_partial_faults_band_00 => subject_coverage_and_partial_faults, 0;
    subject_coverage_and_partial_faults_band_01 => subject_coverage_and_partial_faults, 1;
    subject_coverage_and_partial_faults_band_02 => subject_coverage_and_partial_faults, 2;
    subject_coverage_and_partial_faults_band_03 => subject_coverage_and_partial_faults, 3;
    subject_coverage_and_partial_faults_band_04 => subject_coverage_and_partial_faults, 4;
    subject_coverage_and_partial_faults_band_05 => subject_coverage_and_partial_faults, 5;
    subject_coverage_and_partial_faults_band_06 => subject_coverage_and_partial_faults, 6;
    subject_coverage_and_partial_faults_band_07 => subject_coverage_and_partial_faults, 7;
    same_seed_same_trace_hash_subject_band_08 => same_seed_same_trace_hash_subject, 8;
    same_seed_same_trace_hash_subject_band_09 => same_seed_same_trace_hash_subject, 9;
    same_seed_same_trace_hash_subject_band_10 => same_seed_same_trace_hash_subject, 10;
    same_seed_same_trace_hash_subject_band_11 => same_seed_same_trace_hash_subject, 11;
    two_identities_subject_band_12 => two_identities_subject, 12;
    two_identities_subject_band_13 => two_identities_subject, 13;
}
// ── Per-arm properties of the subject, coverage and partial families ───────
//
// The families above sweep the fault space and ask one question of each seed:
// does every fault reach its own outcome? Each family below asks a *different*
// question of the same seeded worlds, one property per arm. The distinction is
// the point. "every fault reaches its own outcome" is satisfied by a world in
// which the wrong arm is reached for the wrong reason, and it is satisfied by a
// run that published a review nobody asked for, because the assertion is about
// the outcome's *shape* and not about what the receiver holds.
//
// Every family below draws its own seed from the shared generator, so a fault's
// severity — a diff one file or sixty-four files past the ceiling, a partial
// submission that landed none of five comments or four of them — is a draw
// rather than a fixed scenario. A single fixed scenario proves one world.

/// An unavailable diff is a coverage decision, and it publishes nothing.
///
/// Distinct from the sweep above, which only checks that this fault produced
/// *some* non-published outcome: this checks that nothing reached the receiver
/// at all. A run that refused the diff and then published anyway would satisfy
/// the sweep's shape assertion and violate T31's coverage claim, which is a
/// defect that publishes a review against code nobody read.
#[test]
fn an_unavailable_diff_publishes_nothing() -> TestResult {
    let band = sim::Band::new(14, 16);
    for index in band.seeds() {
        let (fault, run) = run_subject_fault(SubjectFault::DiffUnavailable, index)?;
        assert!(
            run.creates == 0,
            "seed {index} ({}): an unreadable diff must create nothing, because a \
             review against a partial scope claims coverage nobody had\n{}",
            fault.label(),
            run.argv
        );
        assert!(
            run.report
                .output()
                .is_some_and(ReviewOutcome::is_incomplete),
            "seed {index}: an unavailable diff is an incomplete coverage: {:?}",
            run.report.output()
        );
    }
    Ok(())
}

/// A diff past the file ceiling is a coverage decision, and it publishes nothing.
#[test]
fn a_diff_past_the_file_ceiling_publishes_nothing() -> TestResult {
    let band = sim::Band::new(15, 16);
    for index in band.seeds() {
        let draw = over_ceiling_draw(index);
        let (_fault, run) = run_subject_fault(SubjectFault::DiffOverFiles, index)?;
        assert!(
            run.creates == 0,
            "seed {index} ({} files past): a refused inventory must create nothing\n{}",
            draw.files_over,
            run.argv
        );
        assert!(
            run.report
                .output()
                .is_some_and(ReviewOutcome::is_incomplete),
            "seed {index} ({draw:?}): a refused inventory is an incomplete coverage: {:?}",
            run.report.output()
        );
        let reason = reason_of(&run);
        assert!(
            reason.contains("file") || reason.contains("ceiling"),
            "seed {index}: the reason must name the bound the run hit, so a reader \
             knows the scope was refused rather than reviewed: {reason}"
        );
    }
    Ok(())
}

/// A diff past the byte ceiling is a coverage decision, and it publishes nothing.
///
/// On its own axis from the file ceiling: an implementation that charged both
/// against one bound would pass the file family and fail this one, and an
/// implementation that refused a heavy-but-short diff would pass both. The draw
/// puts one enormous file over the byte bound with the file count far under its
/// own, which is the only way to tell the two bounds apart.
#[test]
fn a_diff_past_the_byte_ceiling_publishes_nothing() -> TestResult {
    let band = sim::Band::new(16, 16);
    for index in band.seeds() {
        let draw = over_ceiling_draw(index);
        let ceiling = u32::try_from(lgwks_bot::domain::gh::MAX_DIFF_BYTES)?;
        assert!(
            draw.bytes_over > 0,
            "seed {index}: the byte arm needs a draw past the byte ceiling"
        );
        assert!(
            draw.files_over <= u32::try_from(lgwks_bot::domain::gh::MAX_DIFF_FILES_PER_PULL)?,
            "seed {index}: the byte arm must stay under the *file* ceiling, or the \
             refusal would be the other bound's and this family would prove nothing \
             about the byte axis"
        );
        let (_fault, run) = run_subject_fault(SubjectFault::DiffOverBytes, index)?;
        assert!(
            run.creates == 0,
            "seed {index} ({} bytes past): a refused inventory must create nothing\n{}",
            ceiling.saturating_add(draw.bytes_over),
            run.argv
        );
        assert!(
            run.report
                .output()
                .is_some_and(ReviewOutcome::is_incomplete),
            "seed {index} ({draw:?}): a heavy diff is an incomplete coverage: {:?}",
            run.report.output()
        );
    }
    Ok(())
}

/// The two diff ceilings are refused *separately*: the reason names the bound
/// that fired, never the other one.
///
/// The property the two families above cannot express between them. A single
/// combined "refused the diff" assertion is satisfied by an implementation that
/// charges bytes against the file bound — it refuses everything past either
/// ceiling and every assertion above still passes. What makes the two axes
/// distinct is that each refusal *says which one* it is.
#[test]
fn the_two_diff_ceilings_are_refused_separately() -> TestResult {
    let band = sim::Band::new(17, 16);
    for index in band.seeds() {
        let (files_fault, files_run) = run_subject_fault(SubjectFault::DiffOverFiles, index)?;
        let (bytes_fault, bytes_run) = run_subject_fault(SubjectFault::DiffOverBytes, index)?;
        let files_reason = reason_of(&files_run);
        let bytes_reason = reason_of(&bytes_run);
        assert!(
            files_reason.to_lowercase().contains("file"),
            "seed {index}: the file-ceiling refusal names files: {files_reason}"
        );
        assert!(
            bytes_reason.to_lowercase().contains("byte"),
            "seed {index}: the byte-ceiling refusal names bytes: {bytes_reason}"
        );
        assert_eq!(
            files_fault,
            SubjectFault::DiffOverFiles,
            "seed {index}: the first run really was the file arm"
        );
        assert_eq!(
            bytes_fault,
            SubjectFault::DiffOverBytes,
            "seed {index}: the second run really was the byte arm"
        );
    }
    Ok(())
}

/// A renamed repository is a refusal naming both names, and it publishes
/// nothing.
///
/// Distinct from the sweep's shape assertion: this checks the refusal *carries
/// the canonical name*, which is the fact a caller needs to decide what to do.
/// A refusal that said "the repository moved" and stopped there would satisfy
/// the sweep and leave the caller with nothing to act on, and a run that
/// followed the redirect would publish against code it never read.
#[test]
fn a_renamed_repository_is_refused_naming_both() -> TestResult {
    let band = sim::Band::new(18, 16);
    for index in band.seeds() {
        let (fault, run) = run_subject_fault(SubjectFault::Moved, index)?;
        let outcome = run.report.output().ok_or_else(|| {
            format!("seed {index}: a rename is a reported outcome, not a failure")
        })?;
        assert!(
            matches!(outcome, ReviewOutcome::Refused { .. }),
            "seed {index}: a rename is a refusal, never a re-pointed subject: {outcome:?}"
        );
        let reason = reason_of(&run);
        assert!(
            reason.contains("acme/widgets"),
            "seed {index}: the refusal names the repository the caller asked for: {reason}"
        );
        assert!(
            reason.contains("acme/newrepo"),
            "seed {index}: and the canonical name the answer reported, so a caller \
             can decide rather than re-run: {reason}"
        );
        assert!(
            run.creates == 0,
            "seed {index} ({}): a renamed repository publishes nothing\n{}",
            fault.label(),
            run.argv
        );
    }
    Ok(())
}

/// A credential the receiver refuses stops the review with its repair, and
/// publishes nothing (row 3 of #278 on the shipped review path).
///
/// Each seed draws the status the receiver refuses with — an expired token's
/// `401`, a lost scope's `403`, an invisible resource's `404` — so the band is
/// three refusals the adapter must each read as a credential, not one replayed.
/// The run must end as `FlowError::Bot` carrying `CredentialRejected` with that
/// status and the re-grant of the network authority, never as the generic
/// domain failure a retry policy would repeat, and never with a create.
#[test]
fn a_refused_credential_stops_the_review_with_its_repair() -> TestResult {
    use lgwks_bot::cap::Cap;
    use lgwks_bot::error::BotError;
    use lgwks_bot::script::FlowError;
    use lgwks_bot::spec::NeedSet;

    const REFUSALS: [u16; 3] = [401, 403, 404];
    let band = sim::Band::new(40, 16);
    let mut reached = std::collections::HashSet::new();
    for index in band.seeds() {
        let drawn = usize::try_from(sim::Rng::new(index).below(3))?;
        let status = *REFUSALS
            .get(drawn)
            .ok_or("a refusal index drawn below the list's length")?;
        let host = host()?;
        let job = review_task()?;
        let fake = FakeGh::install("credential-refused", HEAD)?;
        fake.configure(Scenario::new(HEAD).deny_credential(status))?;
        let report = host.block_on(&job, (gh_for(&fake, CAPTURE)?, request(7)?))?;
        assert_eq!(
            fake.creates()?,
            0,
            "seed {index} ({status}): a refused credential publishes nothing"
        );
        let failure = report.error().ok_or_else(|| {
            format!(
                "seed {index} ({status}): a refused credential is a run failure, not {:?}",
                report.output()
            )
        })?;
        let FlowError::Bot { ref source, .. } = *failure else {
            let refusal = Err(format!(
                "seed {index} ({status}): the verb's typed error, not {failure}"
            )
            .into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "a_refused_credential_stops_the_review_with_its_repair: returning an error to the caller");
            return refusal;
        };
        let BotError::CredentialRejected {
            status: reported,
            ref needs,
            ..
        } = **source
        else {
            let refusal =
                Err(format!("seed {index} ({status}): CredentialRejected, not {source}").into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "a_refused_credential_stops_the_review_with_its_repair: returning an error to the caller");
            return refusal;
        };
        assert_eq!(
            reported, status,
            "seed {index}: the rejection carries the status the receiver named"
        );
        assert_eq!(
            needs,
            &NeedSet::expired_credentials("gh", &[Cap::net()]),
            "seed {index} ({status}): and the repair re-grants the network authority"
        );
        assert_eq!(
            source.retry_class(),
            lgwks_bot::RetryClass::Never,
            "seed {index} ({status}): the same credential is never retried"
        );
        reached.insert(status);
    }
    assert_eq!(
        reached.len(),
        REFUSALS.len(),
        "the band must reach every credential status, not one replayed: {reached:?}"
    );
    Ok(())
}

/// An untrusted build script in the changed-file inventory is never executed.
///
/// The oracle is a file the inventory's patch text names: the fake emits a
/// `build.rs` whose patch is `rm -f $LGWKS_TEST_MARKER`, and the test writes
/// that marker into the fixture's own directory and names it in the child's
/// environment. A review path that executed the inventory — compiling,
/// shelling, or handing the patch to anything — would remove the file. The
/// review itself still publishes, because a hostile file in the inventory is
/// data, not a defect in the review.
#[test]
fn an_untrusted_build_script_is_never_executed() -> TestResult {
    let band = sim::Band::new(19, 8);
    for index in band.seeds() {
        let fake = FakeGh::install("build-script", HEAD)?;
        fake.configure(Scenario::new(HEAD).hostile_build_script())?;
        let marker = fake.dir().join(format!("LGWKS_MARKER-{index}"));
        std::fs::write(&marker, b"present")?;

        let host = host()?;
        let job = review_task()?;
        let gh = gh_for(&fake, CAPTURE)?.env("LGWKS_TEST_MARKER", marker.as_os_str());
        let report = host.block_on(&job, (gh, request(7)?))?;

        assert!(
            marker.exists(),
            "seed {index}: the review path executed the pull request's build \
             script; the inventory is data and must never be run"
        );
        assert!(
            report.output().is_some_and(ReviewOutcome::is_published),
            "seed {index}: a hostile file in the inventory is data, so the review \
             still publishes: {:?}",
            report.output()
        );
    }
    Ok(())
}

/// A lost response that landed as an unsubmitted draft is `Pending`, and the
/// receiver sees exactly one create.
///
/// Distinct from the sweep, which checks the *variant*; this checks that the
/// draft exists at the receiver and that reconciliation found it by reading
/// rather than by posting again. A `Pending` reported from a run that created
/// twice would satisfy the sweep and violate T33's no-duplicate-post claim.
#[test]
fn a_pending_draft_is_never_reposted() -> TestResult {
    let band = sim::Band::new(20, 16);
    for index in band.seeds() {
        let fake = FakeGh::install("pending", HEAD)?;
        fake.configure(Scenario::new(HEAD).records_pending_draft())?;
        let host = host()?;
        let job = review_task()?;
        let report = host.block_on(&job, (gh_for(&fake, CAPTURE)?, request(7)?))?;

        let outcome = report
            .output()
            .ok_or_else(|| format!("seed {index}: a pending draft is a reported outcome"))?;
        assert!(
            outcome.is_pending(),
            "seed {index}: a lost response onto a draft is Pending: {outcome:?}"
        );
        assert!(
            !outcome.is_published(),
            "seed {index}: an unsubmitted draft is never a publication: {outcome:?}"
        );
        assert_eq!(
            fake.creates()?,
            1,
            "seed {index}: the draft was created once and reconciliation issued no \
             second create — a repost here is the duplicate T33 names"
        );
        assert!(
            outcome.review_id().is_some(),
            "seed {index}: the draft's id is retained, so recovery targets it: {outcome:?}"
        );
        assert!(
            fake.reads_of("/pulls/7/reviews")? >= 1,
            "seed {index}: the draft was found by an independent read, not assumed"
        );
    }
    Ok(())
}

/// A partial submission is reported with the counts that actually landed.
///
/// Distinct from the sweep, which checks the outcome's *variant*. This checks
/// that the two counts are the draw's counts: a `Partial` reporting the
/// intended count twice would satisfy the sweep and tell a caller nothing was
/// missing, which is the failure mode T33's recovery case exists to prevent.
#[test]
fn a_partial_submission_reports_both_counts() -> TestResult {
    let band = sim::Band::new(21, 16);
    for index in band.seeds() {
        let draw = over_ceiling_draw(index);
        let (fault, run) = run_subject_fault(SubjectFault::PartialComments, index)?;
        let outcome = run
            .report
            .output()
            .ok_or_else(|| format!("seed {index}: a partial submission is a reported outcome"))?;
        let &ReviewOutcome::Partial {
            applied, intended, ..
        } = outcome
        else {
            return Err(format!(
                "seed {index} ({}): a submission with fewer comments than intended is \
                 Partial, not {outcome:?}",
                fault.label()
            )
            .into());
        };
        assert_eq!(
            (applied, intended),
            (
                usize::try_from(draw.applied)?,
                usize::try_from(draw.intended)?
            ),
            "seed {index}: Partial reports the draw's counts — what landed and what \
             was intended — so a caller can target only what remains"
        );
        assert!(
            applied < intended,
            "seed {index}: a *partial* submission landed fewer comments than it \
             intended ({applied} of {intended})"
        );
        assert_eq!(
            run.creates, 1,
            "seed {index}: a partial submission is reconciled by a read, never a repost"
        );
    }
    Ok(())
}

/// A lost read permission after a create is `Unverified`, and the applied
/// review id survives.
///
/// Distinct from the sweep, which checks the variant. The id is the applied
/// effect evidence T34 names: a `Unverified` without it would force a caller to
/// re-post to find out whether anything landed, which is exactly the duplicate
/// the module exists to prevent.
#[test]
fn an_unverified_effect_retains_its_review_id() -> TestResult {
    let band = sim::Band::new(22, 16);
    for index in band.seeds() {
        let fake = FakeGh::install("unverified", HEAD)?;
        fake.configure(Scenario::new(HEAD).deny_reads())?;
        let host = host()?;
        let job = review_task()?;
        let report = host.block_on(&job, (gh_for(&fake, CAPTURE)?, request(7)?))?;

        let outcome = report
            .output()
            .ok_or_else(|| format!("seed {index}: a lost permission is a reported outcome"))?;
        assert!(
            outcome.is_unverified(),
            "seed {index}: a create whose read-back lost permission is Unverified, not \
             Unknown — the effect is applied, only its verification is blocked: {outcome:?}"
        );
        let review_id = outcome.review_id().ok_or_else(|| {
            format!("seed {index}: an Unverified outcome retains the id: {outcome:?}")
        })?;
        assert_eq!(
            fake.creates()?,
            1,
            "seed {index}: the effect really landed, and no second create was issued"
        );
        assert!(
            (FIRST_REVIEW_ID..=LAST_REVIEW_ID).contains(&review_id),
            "seed {index}: the retained id must come from the receiver's own range \
             {FIRST_REVIEW_ID}..={LAST_REVIEW_ID}, not be invented: {review_id}"
        );
        assert_eq!(
            outcome.commit_id().map(CommitId::as_str),
            Some(HEAD),
            "seed {index}: the retained id is about the commit that was read"
        );
    }
    Ok(())
}

/// `Unauthorized` and `Transport` are different faults, and the journey tells
/// them apart.
///
/// T34's row is "lost read permission reports unverified/blocking while
/// retaining applied-effect evidence"; a transport failure is a different fact
/// and must stay `Unknown`. This checks the adapter's classification directly,
/// across a seeded sweep of both: a `403` answer is `Unauthorized` and a
/// dropped connection is `Transport`, never the reverse, and neither is
/// collapsed into the other.
#[test]
fn unauthorized_and_transport_stay_distinct() -> TestResult {
    let band = sim::Band::new(23, 16);

    // One task body, used for both reads: the question is which *error* each
    // read produced, so the read has to be reached the way a caller reaches
    // it — through the host — and both faults must travel the same path for the
    // comparison to mean anything.
    let job = task(
        "read-reviews",
        |scope: Scope, (gh, pr): (Gh, PullRequest)| async move {
            let step = scope.enter("reviews")?;
            let read = gh.read_reviews(&pr).await;
            drop(step);
            read.map_err(lgwks_bot::script::FlowError::from)
        },
    )?;

    for index in band.seeds() {
        // The read-back loses permission: a real `403` from the receiver.
        let fake = FakeGh::install("denied", HEAD)?;
        fake.configure(Scenario::new(HEAD).deny_reads())?;
        let denial = host()?.block_on(&job, (gh_for(&fake, CAPTURE)?, pull(7)?))?;

        // The same read fails for a reason that names no HTTP status at all:
        // the receiver refuses with a bare non-zero exit, which is the shape a
        // dropped connection takes and the shape a permission answer never
        // takes.
        let other = FakeGh::install("transported", HEAD)?;
        let refused = sim::Rng::new(index).between(1, 4);
        other.configure(Scenario::new(HEAD).fail_reads(refused))?;
        let outage = host()?.block_on(&job, (gh_for(&other, CAPTURE)?, pull(7)?))?;

        let denial = denial
            .error()
            .ok_or("a denied read must report why it failed")?
            .to_string();
        let outage = outage
            .error()
            .ok_or("a refused read must report why it failed")?
            .to_string();

        // What a caller can actually observe: the permission arm names the HTTP
        // status and the credential that could not reach the resource, and the
        // transport arm names the child's exit and no status at all. A caller
        // deciding between "fix my token" and "retry later" reads exactly these
        // two facts, so collapsing the arms would remove the decision.
        assert!(
            denial.contains("HTTP 403") && denial.contains("credential"),
            "seed {index}: a permission refusal names its HTTP status and the \
             credential that could not reach the resource: {denial}"
        );
        assert!(
            outage.contains("exit ") && !outage.contains("HTTP"),
            "seed {index} ({refused} refusals): a failure naming no HTTP status is a \
             transport failure that reports the child's exit, not a permission one \
             that reports a status: {outage}"
        );
        // The arms are disjoint, not merely differently worded: neither message
        // claims the other's evidence.
        assert!(
            !outage.contains("credential"),
            "seed {index}: a transport failure must not be reported as a permission \
             loss, which would tell a caller to fix a credential that is fine: \
             {outage}"
        );
        assert!(
            !denial.contains("exit "),
            "seed {index}: a permission refusal must not be reported as a transport \
             failure, which would hide a credential problem behind a network one \
             and make a healthy credential look like a flaky link: {denial}"
        );
    }
    Ok(())
}

/// The same two faults reach different outcomes through the whole journey.
///
/// The classification above is the adapter's; this is the journey's. A run
/// whose read-back lost permission reports `Unverified` with the id retained,
/// while a run whose read-back failed for a transport reason reports `Unknown`.
/// Merging them would either discard applied-effect evidence or over-report a
/// blocking state for a failure nobody can attribute to permissions.
#[test]
fn permission_loss_and_transport_outcome_differ() -> TestResult {
    let band = sim::Band::new(24, 8);
    for index in band.seeds() {
        let denied = FakeGh::install("outcome-denied", HEAD)?;
        denied.configure(Scenario::new(HEAD).deny_reads())?;
        let host = host()?;
        let job = review_task()?;
        let denied_report = host.block_on(&job, (gh_for(&denied, CAPTURE)?, request(7)?))?;

        let outage = FakeGh::install("outcome-transported", HEAD)?;
        outage.configure(Scenario::new(HEAD).fail_reads(4))?;
        let outage_report = host.block_on(&job, (gh_for(&outage, CAPTURE)?, request(7)?))?;

        assert!(
            denied_report
                .output()
                .is_some_and(ReviewOutcome::is_unverified),
            "seed {index}: a lost permission after a create is Unverified: {:?}",
            denied_report.output()
        );
        assert!(
            outage_report
                .output()
                .is_some_and(ReviewOutcome::is_unknown),
            "seed {index}: a transport failure leaves the effect unobserved, so it \
             is Unknown — not Unverified, which would claim a permission loss \
             nobody observed: {:?}",
            outage_report.output()
        );
        assert_eq!(
            denied.creates()?,
            1,
            "seed {index}: the denied run applied exactly one create"
        );
    }
    Ok(())
}

/// Saturation over the subject path: many concurrent runs on one pull request,
/// each receiver's own record asserted.
///
/// The concurrency counterpart to the arm families above: a per-seed arm is one
/// world at a time, and a bound that holds for one run says nothing about a host
/// running many. The runs go through [`run_saturation_tier`], so the receiver is
/// sharded at [`RUNS_PER_RECEIVER`] and the equalities are the strict ones — with
/// no fault configured, creates *equal* runs and every run verifies — rather
/// than the weak `creates <= runs` a degenerate world satisfies just as readily.
#[test]
fn subject_saturation_conserves_creates() -> TestResult {
    for _ in 0..16u64 {
        run_saturation_tier(64, 16, "subject-saturation")?;
    }
    Ok(())
}

/// Two tenants' coverage decisions on one pull request stay apart.
///
/// A host that leaked a diff verdict between runs would tell one tenant its
/// coverage was complete when the other tenant's oversized diff was the reason
/// it was refused. The two tenants' inventories differ in size — one clean, one
/// past the file ceiling — so a crossed verdict would be visible.
#[test]
fn two_tenants_coverage_stays_isolated() -> TestResult {
    let band = sim::Band::new(25, 16);
    for index in band.seeds() {
        let clean = FakeGh::install("tenant-clean", HEAD)?;
        clean.configure(Scenario::new(HEAD))?;
        let refused = FakeGh::install("tenant-refused", HEAD)?;
        refused.configure(subject_scenario(
            SubjectFault::DiffOverFiles,
            HEAD,
            over_ceiling_draw(index),
        )?)?;

        let host = host()?;
        let job = review_task()?;
        let clean_report = host.block_on(&job, (gh_for(&clean, CAPTURE)?, request(7)?))?;
        let refused_report = host.block_on(
            &job,
            (
                gh_for(&refused, fault_capture(SubjectFault::DiffOverFiles))?,
                request(7)?,
            ),
        )?;

        assert!(
            clean_report
                .output()
                .is_some_and(ReviewOutcome::is_published),
            "seed {index}: the clean tenant's coverage is complete and it publishes: {:?}",
            clean_report.output()
        );
        assert!(
            refused_report
                .output()
                .is_some_and(ReviewOutcome::is_incomplete),
            "seed {index}: the over-ceiling tenant's coverage is incomplete and it \
             publishes nothing — the two verdicts must not cross: {:?}",
            refused_report.output()
        );
        assert_eq!(
            refused.creates()?,
            0,
            "seed {index}: the refused tenant's receiver saw no create"
        );
        assert_eq!(
            clean.creates()?,
            1,
            "seed {index}: the clean tenant's receiver saw exactly one create"
        );
    }
    Ok(())
}

/// The capture ceiling the file-ceiling arm runs with.
///
/// One helper because the byte arm shares it and a per-family literal is a
/// place for the two to drift: a family running the over-ceiling inventory
/// under a capture ceiling that refused it for the *wrong* reason would report
/// an incomplete coverage for a capture truncation and prove nothing about the
/// file bound.
fn fault_capture(fault: SubjectFault) -> usize {
    fault.capture()
}

/// The review is pinned to the commit that was read, across every arm.
///
/// Every arm above checks a *refusal* arm. This one checks the success arm and
/// the pinning that T31/T33/T34 all rest on: a publication is reported at the
/// commit the run read and nowhere else. It runs through the whole path, so a
/// run that somehow re-pointed the subject at the canonical name after a rename
/// would publish at a commit it never read.
#[test]
fn a_publication_is_pinned_to_the_read_commit() -> TestResult {
    let band = sim::Band::new(26, 16);
    for index in band.seeds() {
        let fake = FakeGh::install("pinned", HEAD)?;
        fake.configure(Scenario::new(HEAD))?;
        let host = host()?;
        let job = review_task()?;
        let report = host.block_on(&job, (gh_for(&fake, CAPTURE)?, request(7)?))?;

        let outcome = report
            .output()
            .ok_or_else(|| format!("seed {index}: a clean run reports an outcome"))?;
        assert!(
            outcome.is_published(),
            "seed {index}: a clean run publishes: {outcome:?}"
        );
        assert_eq!(
            outcome.commit_id().map(CommitId::as_str),
            Some(HEAD),
            "seed {index}: the publication is pinned to the commit that was read, \
             never re-pointed: {outcome:?}"
        );
        assert_eq!(
            fake.creates()?,
            1,
            "seed {index}: a clean run creates exactly once"
        );
    }
    Ok(())
}
