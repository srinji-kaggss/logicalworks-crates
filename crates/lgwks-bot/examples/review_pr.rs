// File: crates/lgwks-bot/examples/review_pr.rs (rust)
//! The canonical PR-review journey, runnable end to end.
//!
//! This is `lgwks_bot::review::review_pr` with a caller-supplied analysis
//! stage, driven through the front door: a [`Host`] runs one typed [`Task`]
//! and returns a [`Report`]. Nothing here manages a process, a pid, a retry
//! ladder or a publication queue — the host owns execution and the adapter owns
//! the process boundary, and the body below is the ordinary business code the
//! specification describes.
//!
//! ```text
//! # a read-only probe: confirm the client resolves before anything is published
//! gh --version
//!
//! # run the journey against a pull request
//! cargo run -p lgwks_bot --features process,ephemeral --example review_pr -- \
//!     acme/widgets 7 "COMMENT" "the diff is correct; one nit at line 42"
//!
//! # draft only: the same run with LGWKS_REVIEW_PUBLISH unset publishes nothing
//! LGWKS_REVIEW_PUBLISH=0 cargo run -p lgwks_bot --features process \
//!     --example review_pr -- acme/widgets 7 "COMMENT" "a draft"
//! ```
//!
//! The environment it depends on is named rather than assumed: `gh` on `PATH`
//! and, for a publication, whatever credentials `gh` itself uses. The example
//! never handles a token, so a token can never be logged by it.
//!
//! The five outcomes it prints are the five the journey file names, and each is
//! reported with the fact that produced it — the reviewed commit, the review
//! id, or the two shas of a moved head — rather than with a message a caller
//! has to interpret.

use std::io::Write;

use lgwks_bot::domain::gh::{CommitId, Gh, PullRequest, Repository};
use lgwks_bot::review::{ReviewOutcome, ReviewRequest, ScriptFailure};
use lgwks_bot::script::{Scope, Tenant};
use lgwks_bot::task::{Host, task};

/// The pull request number, repository, event and body, from the command line.
const USAGE: &str = "usage: review_pr <owner/repo> <pr-number> <EVENT> <body...>";

/// What one review run was asked to do, as the host's task input.
struct Job {
    /// The GitHub binding, already pointed at a program on `PATH`.
    gh: Gh,
    /// What to review and what to say.
    request: ReviewRequest,
    /// Whether this run may publish, or only draft.
    publish: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // An explicit locked handle, so a broken pipe (`review_pr | head`) is an
    // ordinary `Err` rather than a panic in the middle of a report.
    let mut out = std::io::stdout().lock();

    let mut args = std::env::args().skip(1);
    let (Some(repo), Some(number), Some(event)) = (args.next(), args.next(), args.next()) else {
        writeln!(std::io::stderr(), "{USAGE}")?;
        let refusal = Err(USAGE.into());
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "main: returning an error to the caller");
        return refusal;
    };
    let body = args.collect::<Vec<_>>().join(" ");
    if body.is_empty() {
        writeln!(std::io::stderr(), "{USAGE}")?;
        let refusal = Err("the review body is empty".into());
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "main: returning an error to the caller");
        return refusal;
    }

    // The capability the adapter requires is named once, here, so a reader can
    // see what this program reaches before it reaches it.
    let repository = Repository::new(&repo)?;
    let adapter = Gh::new(repository.clone());
    writeln!(
        out,
        "adapter: gh, requiring {:?} for {repository}",
        adapter.required_caps()
    )?;

    // One host, built once. Its ceilings are readable, so nothing here is an
    // unbounded wait or an unbounded fan-out.
    let host = Host::builder(Tenant::new("reviewer")?.as_str())?
        .default_deadline(std::time::Duration::from_secs(300))
        .build()?;
    writeln!(
        out,
        "host: tenant={} deadline={:?} permits={}",
        host.tenant().as_str(),
        host.limits().default_deadline(),
        host.limits().max_concurrent_tasks()
    )?;

    let pull = PullRequest::new(repository.clone(), number.parse::<u64>()?);
    let publish = std::env::var_os("LGWKS_REVIEW_PUBLISH").is_none_or(|value| value != "0");
    let request = ReviewRequest::new(pull, &event, body).with_marker("lgwks:review-pr:v1");

    // The consumer's task: a typed body and nothing else. No trait to
    // implement, no `Box`, no `Pin`, no `'static` bound in this code.
    let review = task("review-pr", |scope: Scope, job: Job| async move {
        run_review(scope, job).await
    })?;

    let report = host.block_on(
        &review,
        Job {
            gh: adapter,
            request,
            publish,
        },
    )?;

    writeln!(out, "disposition: {}", report.disposition().label())?;
    for step in report.steps() {
        writeln!(out, "  step: {step}")?;
    }
    if report.dropped_steps() > 0 {
        writeln!(out, "  ({} earlier steps dropped)", report.dropped_steps())?;
    }
    match report.output() {
        Some(outcome) => report_outcome(&mut out, outcome)?,
        None => {
            if let Some(error) = report.error() {
                writeln!(out, "failed: {error}")?;
            }
        }
    }
    writeln!(out, "effects: {:?}", report.effects())?;
    writeln!(out, "elapsed: {:?}", report.elapsed())?;
    Ok(())
}

/// The task body: analyse the pinned snapshot, then publish and verify.
///
/// This is where a caller substitutes their own analysis. The example's is a
/// named finding — it is not a semantic reviewer, and it says so, rather than
/// implying a quality claim it cannot make.
async fn run_review(scope: Scope, job: Job) -> Result<ReviewOutcome, lgwks_bot::script::FlowError> {
    if !job.publish {
        // A draft-only profile is a real mode, not a degraded one: it reads the
        // subject and reports it, and it never acquires the publication effect.
        let snapshot = job
            .gh
            .snapshot(job.request.pull())
            .await
            .map_err(lgwks_bot::script::FlowError::from)?;
        let drafted = analyse(&snapshot, job.request.pull(), job.request.body())?;
        writeln!(
            std::io::stdout(),
            "draft only: reviewed {} at {}, not published",
            snapshot.head_sha(),
            drafted
        )?;
        return Ok(ReviewOutcome::Refused {
            reason: String::from("this profile is draft-only; no publication right was granted"),
        });
    }
    let pull = job.request.pull().clone();
    let draft = job.request.body().to_owned();
    lgwks_bot::review::review_pr(scope, job.gh, job.request, move |snapshot, _scope| {
        analyse(snapshot, &pull, &draft)
    })
    .await
}

/// The analysis stage: turn a pinned snapshot into the body to publish.
///
/// A caller replaces this. It is handed the pinned snapshot, the pull request
/// it was read from and the operator's text, and it produces a body or a typed
/// failure. It performs no publication and holds no credential. The body names
/// the repository and commit the snapshot pinned, so a published review says
/// which code it is about.
fn analyse(
    snapshot: &lgwks_bot::domain::gh::PrSnapshot,
    pull: &PullRequest,
    draft: &str,
) -> Result<String, ScriptFailure> {
    let head = CommitId::new(snapshot.head_sha())
        .map_err(|source| ScriptFailure::new(format!("the head is not a commit: {source}")))?;
    Ok(format!(
        "{draft}\n\nReviewed {}#{} at {head}.",
        pull.repository(),
        snapshot.number()
    ))
}

/// The lines that name one outcome and the fact that produced it.
fn outcome_lines(outcome: &ReviewOutcome) -> Vec<String> {
    match *outcome {
        ReviewOutcome::Published {
            review_id,
            ref commit_id,
            verified,
        } => vec![
            format!("published review {review_id} at {commit_id} (verified: {verified})"),
            String::from("  the review was read back from GitHub before this was reported"),
        ],
        ReviewOutcome::Unknown {
            ref commit_id,
            ref reason,
        } => vec![
            format!("unknown: an effect at {commit_id} may or may not have landed"),
            format!("  {reason}"),
            String::from("  no second review was created; reconcile before doing anything else"),
        ],
        ReviewOutcome::TargetMoved {
            ref reviewed,
            ref current,
        } => vec![
            String::from("refused: the head moved while this review was being prepared"),
            format!("  reviewed: {reviewed}"),
            format!("  current:  {current}"),
            String::from(
                "  nothing was published; re-analysing at the new head is a separate decision",
            ),
        ],
        ReviewOutcome::Refused { ref reason } => vec![
            format!("refused: {reason}"),
            String::from("  no external effect is claimed"),
        ],
        // A new state the journey does not model yet. Named rather than
        // collapsed into a refusal, because "something else happened" and
        // "nothing happened" are the two answers a caller most needs to tell
        // apart.
        ref other => vec![
            format!("unmodelled outcome: {other:?}"),
            String::from("  no claim is made about whether an effect happened"),
        ],
    }
}

/// Print one outcome, naming the fact that produced it.
fn report_outcome(
    out: &mut impl Write,
    outcome: &ReviewOutcome,
) -> Result<(), Box<dyn std::error::Error>> {
    for line in outcome_lines(outcome) {
        writeln!(out, "{line}")?;
    }
    Ok(())
}
