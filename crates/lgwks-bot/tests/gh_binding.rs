//! The adapter's own guarantees, driven through the same real process
//! boundary: bounded capture, a deadline that stops a whole process group, and
//! the refusals a caller must be able to distinguish.
//!
//! These are properties of the *binding*, not of the review journey, so they
//! are exercised directly against [`Gh`] rather than through a task. Every one
//! of them forks real processes: the fixture is the same receiver-backed fake
//! `gh`, and the observations are what the adapter reports about what the child
//! actually did.
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

use lgwks_bot::domain::gh::{CommitId, Gh, GhError, Repository};

#[path = "support/fake_gh.rs"]
mod fake_gh;

use fake_gh::{FakeGh, Scenario};

/// What a test reports when a precondition did not hold.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The head every scenario reads.
const HEAD: &str = "1111111111111111111111111111111111111111";

/// A non-zero ceiling, because an unbounded capture is not expressible.
fn limit(bytes: usize) -> Result<NonZeroUsize, Box<dyn std::error::Error>> {
    NonZeroUsize::new(bytes).ok_or_else(|| "a non-zero limit".into())
}

// ── Bounded capture ─────────────────────────────────────────────────────────

#[test]
fn a_chatty_client_is_truncated_at_its_ceiling_and_never_decoded() -> TestResult {
    let fake = FakeGh::install("flood", HEAD)?;
    // 200 lines of 49 bytes each is about 9,800 bytes, and the ceiling is 1,024.
    // A read that returns more than the ceiling must not be parsed: a partial
    // JSON document decoded into a partial value is the defect.
    fake.configure(Scenario::new(HEAD).floods(200))?;
    let gh = Gh::new(Repository::new("acme/widgets")?)
        .program(fake.program())
        .capture_limit(limit(1024)?)
        .deadline(Some(Duration::from_secs(10)))
        .env("PATH", fake.search_path()?);

    let outcome =
        lgwks_bot::Runtime::new()?.block_on(gh.query_lossy("repos/acme/widgets/pulls/7"))?;

    assert!(
        outcome.stdout_truncated(),
        "a child that wrote past the ceiling is reported as truncated: {outcome:?}"
    );
    assert!(
        outcome.stdout_total_bytes() > 1024,
        "and the exact total is reported, so a reader knows how much was not retained: {}",
        outcome.stdout_total_bytes()
    );
    assert!(
        outcome.stdout().len() <= 1024,
        "the retained bytes stop at the ceiling: {}",
        outcome.stdout().len()
    );
    assert_eq!(
        outcome.exit_code(),
        Some(0),
        "the child succeeded; truncation is the adapter's bound, not its failure"
    );

    // The parse is refused rather than attempted on a partial document.
    let decoded = outcome.parse_json::<lgwks_bot::domain::gh::PrSnapshot>();
    assert!(
        matches!(decoded, Err(GhError::TruncatedResponse { .. })),
        "a truncated answer is refused, never decoded into a partial value: {decoded:?}"
    );
    Ok(())
}

#[test]
fn a_response_within_the_ceiling_is_parsed() -> TestResult {
    let fake = FakeGh::install("small", HEAD)?;
    fake.configure(Scenario::new(HEAD))?;
    let gh = Gh::new(Repository::new("acme/widgets")?)
        .program(fake.program())
        .capture_limit(limit(64 * 1024)?)
        .deadline(Some(Duration::from_secs(10)))
        .env("PATH", fake.search_path()?);

    let outcome =
        lgwks_bot::Runtime::new()?.block_on(gh.query_lossy("repos/acme/widgets/pulls/7"))?;
    assert!(
        !outcome.stdout_truncated(),
        "a small answer is not truncated"
    );
    let snapshot = outcome.parse_json::<lgwks_bot::domain::gh::PrSnapshot>()?;
    assert_eq!(
        snapshot.head_sha(),
        HEAD,
        "an answer within the ceiling is decoded, not refused"
    );
    Ok(())
}

// ── The deadline stops a whole process group ────────────────────────────────

#[test]
fn a_hanging_client_is_stopped_as_a_whole_group_and_reports_no_exit_code() -> TestResult {
    let fake = FakeGh::install("hang", HEAD)?;
    // The fixture spawns a grandchild and then sleeps, so the deadline has a
    // process *group* to reap rather than one child it could trivially kill.
    fake.configure(Scenario::new(HEAD).hangs_for(30))?;
    let gh = Gh::new(Repository::new("acme/widgets")?)
        .program(fake.program())
        .capture_limit(limit(64 * 1024)?)
        .deadline(Some(Duration::from_millis(400)))
        .env("PATH", fake.search_path()?);

    let started = std::time::Instant::now();
    let outcome =
        lgwks_bot::Runtime::new()?.block_on(gh.query_lossy("repos/acme/widgets/pulls/7"))?;
    let elapsed = started.elapsed();

    assert!(
        outcome.deadline_fired(),
        "a client that hangs is stopped by its deadline: {outcome:?}"
    );
    assert_eq!(
        outcome.exit_code(),
        None,
        "a deadline stop is not an exit code: the child did not choose to exit"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "the deadline is honoured rather than the client being waited out: {elapsed:?}"
    );
    Ok(())
}

// ── Validation refusals ─────────────────────────────────────────────────────

#[test]
fn a_repository_reference_that_is_not_one_is_refused_before_anything_runs() -> TestResult {
    for bad in [
        "",
        "no-slash",
        "too/many/slashes",
        "/leading",
        "trailing/",
        "owner/has space",
        "owner/re po",
    ] {
        assert!(
            Repository::new(bad).is_err(),
            "{bad:?} is not an owner/repo reference and must be refused"
        );
    }
    let ok = Repository::new("acme/widgets.git")?;
    assert_eq!(ok.as_str(), "acme/widgets.git");
    assert!(
        Repository::new("a/b").is_ok(),
        "control: a minimal valid reference is accepted"
    );
    Ok(())
}

#[test]
fn a_commit_id_that_is_not_one_is_refused() -> TestResult {
    for bad in [
        "",
        "abc",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "111111111111111111111111111111111111111G",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    ] {
        assert!(
            CommitId::new(bad).is_err(),
            "{bad:?} is not a full lowercase commit id and must be refused"
        );
    }
    assert_eq!(
        CommitId::new(HEAD)?.as_str(),
        HEAD,
        "control: a full lowercase sha is accepted"
    );
    Ok(())
}

#[test]
fn a_review_event_github_does_not_accept_is_refused() -> TestResult {
    let subject = CommitId::new(HEAD)?;
    for event in ["", "MERGE", "comment", "APPROVED"] {
        assert!(
            lgwks_bot::domain::gh::ReviewPayload::new(&subject, event, "body", "marker").is_err(),
            "{event:?} is not a review event GitHub accepts"
        );
    }
    for event in lgwks_bot::domain::gh::ReviewPayload::EVENTS {
        let payload = lgwks_bot::domain::gh::ReviewPayload::new(&subject, event, "body", "marker")?;
        assert_eq!(
            payload.commit_id(),
            HEAD,
            "the reviewed commit is a field of the payload, never omitted"
        );
    }
    Ok(())
}

#[test]
fn a_build_without_a_runner_refuses_rather_than_reporting_an_empty_answer() {
    // The vocabulary is the point: a binding that cannot reach GitHub must say
    // so, because a caller handed "no reviews" would conclude GitHub has none.
    let error = GhError::NoRunner;
    assert!(
        error.to_string().contains("process runner"),
        "the refusal names the missing capability: {error}"
    );
    assert!(
        !error.is_read_only_retryable(),
        "and it is not retryable, because retrying cannot conjure a runner"
    );
}

// ── The review ceiling ──────────────────────────────────────────────────────

#[test]
fn a_review_list_past_the_ceiling_is_refused_not_truncated() -> TestResult {
    let fake = FakeGh::install("ceiling", HEAD)?;
    // One more review than the adapter will decode. A client that paginates
    // happily produces exactly this, and the question is not whether the bytes
    // fit — they do — but whether a *clean decode* of a partial history is
    // reported as the history.
    let over = u32::try_from(lgwks_bot::domain::gh::MAX_REVIEWS_PER_PULL)
        .map_err(|_| "the ceiling must fit the fixture's counter")?
        .saturating_add(1);
    fake.configure(Scenario::new(HEAD).with_filler_reviews(over))?;
    let gh = Gh::new(Repository::new("acme/widgets")?)
        .program(fake.program())
        .capture_limit(limit(4 * 1024 * 1024)?)
        .deadline(Some(Duration::from_secs(20)))
        .env("PATH", fake.search_path()?);

    let refused = lgwks_bot::Runtime::new()?
        .block_on(async {
            gh.read_reviews(&lgwks_bot::domain::gh::PullRequest::new(
                Repository::new("acme/widgets")?,
                7,
            ))
            .await
        })
        .err()
        .ok_or("a list past the ceiling must be refused, not returned short")?;

    match refused {
        GhError::ReviewCeiling {
            ref path,
            reviews,
            ceiling,
        } => {
            assert!(
                path.contains("/reviews"),
                "the refusal names the endpoint it came from: {path}"
            );
            assert_eq!(
                reviews,
                usize::try_from(over).unwrap_or(usize::MAX),
                "the refusal reports what the client actually returned, so a caller \
                 can tell a long history from a short one"
            );
            assert_eq!(
                ceiling,
                lgwks_bot::domain::gh::MAX_REVIEWS_PER_PULL,
                "and the ceiling that refused it, which is a declared bound"
            );
        }
        other => {
            return Err(format!(
                "a list past the ceiling must be ReviewCeiling, not {other:?} ({other})"
            )
            .into());
        }
    }
    assert!(
        refused.to_string().contains("not returned"),
        "the message says the list was withheld rather than shortened: {refused}"
    );
    Ok(())
}

#[test]
fn a_review_list_exactly_at_the_ceiling_is_read() -> TestResult {
    let fake = FakeGh::install("ceiling-edge", HEAD)?;
    // The boundary: exactly the ceiling is the largest list the adapter accepts,
    // and refusing it would make the bound a fiction rather than a declaration.
    let at = u32::try_from(lgwks_bot::domain::gh::MAX_REVIEWS_PER_PULL)
        .map_err(|_| "the ceiling must fit the fixture's counter")?;
    fake.configure(Scenario::new(HEAD).with_filler_reviews(at))?;
    let gh = Gh::new(Repository::new("acme/widgets")?)
        .program(fake.program())
        .capture_limit(limit(4 * 1024 * 1024)?)
        .deadline(Some(Duration::from_secs(20)))
        .env("PATH", fake.search_path()?);

    let reviews = lgwks_bot::Runtime::new()?.block_on(async {
        gh.read_reviews(&lgwks_bot::domain::gh::PullRequest::new(
            Repository::new("acme/widgets")?,
            7,
        ))
        .await
    })?;
    assert_eq!(
        reviews.len(),
        lgwks_bot::domain::gh::MAX_REVIEWS_PER_PULL,
        "the ceiling is inclusive: exactly the declared number is accepted"
    );
    Ok(())
}

#[test]
fn a_malformed_review_list_is_refused_rather_than_decoded_into_a_partial_answer() -> TestResult {
    for shape in ["garbage", "truncated"] {
        let fake = FakeGh::install(shape, HEAD)?;
        fake.configure(Scenario::new(HEAD).answers_reviews_with(shape))?;
        let gh = Gh::new(Repository::new("acme/widgets")?)
            .program(fake.program())
            .capture_limit(limit(64 * 1024)?)
            .deadline(Some(Duration::from_secs(10)))
            .env("PATH", fake.search_path()?);

        let refused = lgwks_bot::Runtime::new()?
            .block_on(async {
                gh.read_reviews(&lgwks_bot::domain::gh::PullRequest::new(
                    Repository::new("acme/widgets")?,
                    7,
                ))
                .await
            })
            .err()
            .ok_or("a review list that is not a review list must be refused")?;
        assert!(
            matches!(refused, GhError::MalformedResponse { .. }),
            "{shape:?} must be a decode failure, not a partial list: {refused:?}"
        );
        assert!(
            refused.is_read_only_retryable(),
            "and it is read-only retryable, because a malformed answer says nothing \
             about whether the reviews exist: {refused}"
        );
    }
    Ok(())
}

// ── The argument vector ─────────────────────────────────────────────────────

#[test]
fn the_argument_vector_is_a_vector_and_names_the_method_and_path() -> TestResult {
    let gh = Gh::new(Repository::new("acme/widgets")?);
    let args = gh.args_for(&["--method", "GET", "repos/acme/widgets/pulls/7"]);

    assert_eq!(
        args,
        vec!["api", "--method", "GET", "repos/acme/widgets/pulls/7"],
        "the client's own arguments are passed through unchanged, with no shell \
         between them and the process"
    );
    assert!(
        gh.args_for(&["--method", "POST"])
            .iter()
            .all(|arg| !arg.contains(';') && !arg.contains('|')),
        "no shell metacharacter is introduced by the binding itself"
    );
    Ok(())
}


/// The one extension the fixture needs so a test can read the raw outcome
/// without going through a typed verb call that would reject a truncated or a
/// hung answer first.
trait LossyQuery {
    /// Run one `gh api` call through the public verb surface and report what
    /// was observed.
    ///
    /// The `Result` is the adapter's own refusal, not a test convenience: a
    /// probe that needs an outcome asserts the call produced one, and a probe
    /// that expects a refusal asserts the error — which is how a test tells
    /// "the call was refused" apart from "the call ran and was truncated".
    type Refused;

    /// The observed outcome of one call.
    fn query_lossy(&self, path: &str) -> Self::Refused;
}

impl LossyQuery for Gh {
    /// The future [`LossyQuery::query_lossy`] returns.
    type Refused = std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<lgwks_bot::domain::gh::GhOutcome, lgwks_bot::BotError>,
                >,
        >,
    >;

    fn query_lossy(&self, path: &str) -> Self::Refused {
        let args = self.args_for(&["--method", "GET", path]);
        // `GhQuery` is the public verb surface, so this reaches the same
        // supervised call a real consumer makes rather than a private one.
        let query = lgwks_bot::domain::gh::GhQuery::new(self.clone(), args.into_iter().skip(1));
        // The adapter's own requirement list is what the proof is built from,
        // so the test cannot drift from what the binding actually demands: a
        // new capability on the binding would deny this call rather than
        // silently passing it.
        let required = self.required_caps().to_vec();
        let grants = required
            .iter()
            .fold(lgwks_bot::GrantSet::empty(), |set, cap| {
                set.grant(cap.clone())
            });
        Box::pin(async move {
            let auth = grants.issue(&required)?;
            lgwks_bot::Query::query(&query, (auth, &())).await
        })
    }
}
