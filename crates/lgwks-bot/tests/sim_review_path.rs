//! Deterministic simulation of the **real** review path, across many seeds.
//!
//! [`sim_review_pr`](./sim_review_pr.rs) simulates the decision the
//! reconciliation makes, with no process anywhere in the picture. This file
//! simulates the same decisions *through the path a caller runs*: every family
//! here drives [`lgwks_bot::review::review_pr`] on a [`Host`], with the adapter
//! bound to a scripted `gh` on `PATH`. Nothing is mocked inside the process —
//! the code under test admits, forks, captures, applies its deadline and reaps
//! exactly as it would for the real client, and the fixture records the argv it
//! received.
//!
//! One seed drives the whole family: which fault runs. The same seed produces
//! the same trace hash *and* the same argv, which is asserted rather than
//! assumed, so a failure printed with its seed replays as a replay rather than
//! an approximation.
//!
//! The families are exactly the states a caller has to be able to tell apart:
//!
//! - `verified_and_not_observed_r32`: publication observed against publication
//!   not observed, over one shared fault space.
//! - `gh_exit_failures_r32`: every way the client can fail and answer nothing.
//! - `deadline_stop_r32`: a client stopped mid-call by the real adapter deadline.
//! - `head_moved_between_snapshot_and_publish_r32`: the refusal, at every seed.
//! - `malformed_and_oversized_answers_r32`: answers that are not the review
//!   history, and must be refused rather than read as a short one.
//! - `saturation_r32`: 100, 1,000 and 10,000 concurrent runs on one pull
//!   request, each checked at the receiver.
//! - `two_tenants_on_one_pull_request_r32`: two identities on one pull request,
//!   each verifying only its own review.
//! - `retention_is_bounded_by_the_declared_ceiling`: what the adapter holds is
//!   bounded by its declaration, not by what the child wrote.
//!
//! `sim_review_pr.rs` and this file split the work deliberately: the first
//! checks the *logic* exhaustively and cheaply, this one checks that the logic
//! survives contact with a real process boundary. A defect that only appears
//! once a child has actually run — an argument vector that does not reach it, a
//! deadline that does not stop it, a payload truncated on the way — is invisible
//! to the first and visible only to the second.

// `ephemeral` is required, not incidental: a staged publication payload needs a
// filename no two concurrent publications share, and without that feature the
// binding refuses to publish rather than fall back to a name the OS recycles.
// The `full` lane runs this suite, so the refusal is exercised rather than
// assumed.
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
use lgwks_bot::review::{ReviewOutcome, ReviewRequest};
use lgwks_bot::script::Scope;
use lgwks_bot::task::{Host, Report, Task, task};

#[path = "support/fake_gh.rs"]
mod fake_gh;

mod sim;

// The band-declaration macro, defined once for the whole layer.
#[path = "sim/bands.rs"]
mod band_family;

use fake_gh::{FakeGh, Scenario};

/// What a simulation reports when a precondition did not hold.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The head a clean family reads.
const HEAD: &str = "1111111111111111111111111111111111111111";

/// The head a scenario reports after the first read.
const MOVED: &str = "2222222222222222222222222222222222222222";

/// The body the shared families publish, so a read-back compares like with like.
const BODY: &str = "two findings";

/// Seeds each family runs.
const SEEDS: u64 = 32;

/// The capture ceiling the families run with: large enough that every family's
/// answer fits whole, and small enough that the oversized family can exceed it
/// with the fixture's own padding.
const CAPTURE: usize = 64 * 1024;

/// The first review id the scripted receiver hands out.
///
/// The receiver numbers accepted creates from `next_review_id`, which
/// [`fake_gh::Scenario`] sets to 9001, and this family's two runs take 9001 and
/// 9002. Naming the range here rather than writing the numbers into an
/// assertion keeps the comparison honest if the fixture's base ever moves: a
/// run that reported an id from outside the receiver's own range would be
/// reporting something the receiver never applied.
const FIRST_REVIEW_ID: u64 = 9_001;

/// The last review id this family's two runs can receive.
const LAST_REVIEW_ID: u64 = 9_002;

/// The scenario space, one entry per fault the client can inject.
///
/// Named rather than an index so a family's fault and its expectation are read
/// together. The list is exhaustive over the journey's states: every way a call
/// can fail to produce an independent observation.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Fault {
    /// Everything works; the create answers and the read-back finds it.
    Clean,
    /// The create applied the review and its response was lost.
    ResponseLost,
    /// The create was refused before it reached the receiver.
    CreateRefused,
    /// The client was refused on every read, so nothing could be observed.
    ReadsRefused,
    /// The head moved between the snapshot and the freshness read.
    HeadMoved,
    /// The client named a program that does not exist.
    ClientAbsent,
    /// The review list is longer than the adapter's declared ceiling.
    ListOverCeiling,
    /// The review list is not JSON at all.
    ListNotJson,
    /// The review list is JSON whose closing bracket was lost.
    ListTruncated,
    /// The answer is larger than the capture ceiling, so it cannot be decoded.
    AnswerOversized,
}

impl Fault {
    /// How many faults the family contains.
    const VARIANTS: u64 = 10;

    /// Every fault's label, so a coverage test does not re-list the enum.
    const LABELS: [&'static str; 10] = [
        "clean",
        "response-lost",
        "create-refused",
        "reads-refused",
        "head-moved",
        "client-absent",
        "list-over-ceiling",
        "list-not-json",
        "list-truncated",
        "answer-oversized",
    ];

    /// The fault for `index` at `offset`, drawn from the whole family.
    fn for_index(index: u64, offset: u64) -> Self {
        match index.wrapping_add(offset) % Self::VARIANTS {
            0 => Self::Clean,
            1 => Self::ResponseLost,
            2 => Self::CreateRefused,
            3 => Self::ReadsRefused,
            4 => Self::HeadMoved,
            5 => Self::ClientAbsent,
            6 => Self::ListOverCeiling,
            7 => Self::ListNotJson,
            8 => Self::ListTruncated,
            _ => Self::AnswerOversized,
        }
    }

    /// A short label, for the trace and for the failure message.
    const fn label(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::ResponseLost => "response-lost",
            Self::CreateRefused => "create-refused",
            Self::ReadsRefused => "reads-refused",
            Self::HeadMoved => "head-moved",
            Self::ClientAbsent => "client-absent",
            Self::ListOverCeiling => "list-over-ceiling",
            Self::ListNotJson => "list-not-json",
            Self::ListTruncated => "list-truncated",
            Self::AnswerOversized => "answer-oversized",
        }
    }

    /// Whether a review can reach the receiver at all under this fault.
    ///
    /// The receiver's own record decides this, not the create's exit code.
    /// `ReadsRefused` is the subtle one and is worth stating: the fixture
    /// refuses the *review* reads, so the create still lands and there is a real
    /// effect on the receiver. Reading "every read failed" as "nothing was
    /// written" would classify a live effect as a non-effect, which is the
    /// direction of error this module exists to avoid.
    const fn applies(self) -> bool {
        !matches!(self, Self::HeadMoved | Self::ClientAbsent)
    }

    /// Whether a run under this fault can report a verified publication.
    ///
    /// The whole invariant the families assert, stated once so each family tests
    /// it rather than re-deriving it. It is conservative in the direction that
    /// matters: every fault outside this list leaves the publication
    /// unobservable by construction.
    ///
    /// `ListOverCeiling` is deliberately on neither side, and that is its whole
    /// point. The create landed and the review really exists — but the
    /// read-back that would confirm it is refused, so the run *cannot* verify.
    /// Reporting `Published` off the create's own success is precisely the
    /// defect the review ceiling exists to prevent, and the run must stay
    /// `Unknown`: the effect is real and its verification is not available.
    const fn can_be_verified(self) -> bool {
        matches!(self, Self::Clean | Self::ResponseLost)
    }
}

// ── The run ─────────────────────────────────────────────────────────────────

/// A run of the review journey, with what the receiver actually observed.
struct Run {
    /// The report the host returned.
    report: Report<ReviewOutcome>,
    /// How many creates the receiver was asked for.
    creates: usize,
    /// Every argv line the receiver recorded, joined for hashing.
    argv: String,
}

/// One run's observable conclusion.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Trace {
    /// The report's disposition.
    disposition: String,
    /// The outcome, with any identity it carries hashed to a short label.
    outcome: String,
    /// How many creates the receiver saw.
    creates: usize,
    /// Every argv line the receiver recorded.
    argv: String,
}

impl Run {
    /// The run reduced to what a caller acts on.
    ///
    /// Reduced rather than carried, because a trace must be comparable across
    /// families and hold no borrowed payloads. The disposition is included:
    /// "published" from a *failed* run is a different fact from "published"
    /// from a succeeded one, and dropping it would make a trace that a broken
    /// disposition could still match.
    fn trace(&self) -> Trace {
        Trace {
            disposition: String::from(self.report.disposition().label()),
            outcome: self.report.output().map_or_else(
                || String::from("none"),
                |outcome| match *outcome {
                    ReviewOutcome::Published {
                        review_id,
                        ref commit_id,
                        verified,
                    } => text_hash(&format!("published:{review_id}:{commit_id}:{verified}"))
                        .to_string(),
                    ReviewOutcome::Unknown { ref commit_id, .. } => {
                        text_hash(&format!("unknown:{commit_id}")).to_string()
                    }
                    ReviewOutcome::TargetMoved {
                        ref reviewed,
                        ref current,
                    } => text_hash(&format!("moved:{reviewed}:{current}")).to_string(),
                    ReviewOutcome::Refused { .. } => String::from("refused"),
                    ReviewOutcome::Incomplete { .. } => String::from("incomplete"),
                    ReviewOutcome::Pending { .. } => String::from("pending"),
                    ReviewOutcome::Partial { .. } => String::from("partial"),
                    ReviewOutcome::Unverified { .. } => String::from("unverified"),
                    // A state this journey does not model yet. Named rather
                    // than collapsed, because the two facts a caller most needs
                    // to tell apart are "something else happened" and "nothing
                    // happened".
                    _ => String::from("unmodelled"),
                },
            ),
            creates: self.creates,
            argv: self.argv.clone(),
        }
    }
}

/// A stable hash of a trace, so a replay is checkable rather than asserted.
fn trace_hash(trace: &Trace) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    trace.disposition.hash(&mut hasher);
    trace.outcome.hash(&mut hasher);
    trace.creates.hash(&mut hasher);
    trace.argv.hash(&mut hasher);
    hasher.finish()
}

/// The argv the receiver recorded, normalized for replay.
///
/// The staged payload's name is the one thing about a run that is *meant* to
/// differ every time — it is random, per `INV-BOT-80`, precisely so two
/// concurrent publications cannot race for one file. Hashing it raw would make
/// every replay differ and the replay family vacuous; erasing it would make two
/// runs that reused a name look identical. Normalizing to a shape marker keeps
/// the call sequence — the thing a duplicate-post defect changes — and drops
/// only the identity that is supposed to vary.
fn normalized_argv(calls: &[Vec<String>]) -> String {
    calls
        .iter()
        .map(|call| {
            let mut rendered: Vec<String> = Vec::with_capacity(call.len());
            let mut after_input = false;
            for arg in call {
                if after_input {
                    // The staged payload's name: the one thing meant to vary.
                    rendered.push(String::from("<staged-payload>"));
                    after_input = false;
                    continue;
                }
                after_input = arg == "--input";
                rendered.push(arg.clone());
            }
            rendered.join(" ")
        })
        .collect::<Vec<String>>()
        .join("\n")
}

/// A stable hash of plain text, for a label carrying an identity.
fn text_hash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

// ── The harness ─────────────────────────────────────────────────────────────

/// A host sized for a family's worst case, with a deadline generous enough that
/// a test fails on a real hang rather than on the clock.
fn host() -> Result<Host, Box<dyn std::error::Error>> {
    Ok(Host::builder("reviewer-sim")?
        .max_concurrent_tasks(NonZeroUsize::new(64).ok_or("a non-zero ceiling")?)
        .default_deadline(Duration::from_secs(60))
        .build()?)
}

/// The `Gh` binding pointed at `fake`, with a capture ceiling the family chose.
///
/// `capture` is a per-family parameter because the oversized family needs a
/// ceiling it can exceed and every other family needs one it cannot; a single
/// shared value would make one of them untestable.
fn gh_for(fake: &FakeGh, capture: usize) -> Result<Gh, Box<dyn std::error::Error>> {
    let path = fake.search_path()?;
    Ok(Gh::new(Repository::new("acme/widgets")?)
        .program(fake.program())
        .capture_limit(NonZeroUsize::new(capture).ok_or("a non-zero limit")?)
        .deadline(Some(Duration::from_secs(20)))
        .env("PATH", path))
}

/// The future a task body returns.
type ReviewFuture = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<ReviewOutcome, lgwks_bot::script::FlowError>>>,
>;

/// The task every family runs: the journey body with a fixed analysis stage.
///
/// One definition rather than one per family, because the shape of a run *is*
/// the thing under test — a family that quietly built its own task body would be
/// testing a different journey.
fn review_task() -> Result<Task<ReviewBody>, Box<dyn std::error::Error>> {
    let body: ReviewBody = review_body;
    Ok(task("review-pr", body)?)
}

/// The closure shape [`review_task`] builds.
type ReviewBody = fn(Scope, (Gh, ReviewRequest)) -> ReviewFuture;

/// The task body: the journey itself, with the shared analysis stage.
///
/// Boxed and `!Send`, which is the shape a caller on the calling task gets:
/// [`Host::run`] polls a body where it stands, so a caller is asked for neither
/// a trait object nor a `'static` bound. The concurrent family below cannot use
/// this one, and that asymmetry is the point — spawning is the case where
/// `Send` becomes the caller's obligation, and the two shapes must still be the
/// same journey, so the boxed body delegates to the `Send` one rather than
/// restating the sequence.
fn review_body(scope: Scope, (gh, request): (Gh, ReviewRequest)) -> ReviewFuture {
    Box::pin(review_body_send(scope, (gh, request)))
}

/// The same journey as an `async fn`, whose future is `Send`.
async fn review_body_send(
    scope: Scope,
    (gh, request): (Gh, ReviewRequest),
) -> Result<ReviewOutcome, lgwks_bot::script::FlowError> {
    lgwks_bot::review::review_pr(scope, gh, request, |_snapshot, _scope| {
        Ok(String::from(BODY))
    })
    .await
}

/// The concurrent form of [`review_task`], for the saturation family.
///
/// A separate task because this is the one shape that must be `Send`: a spawned
/// task crosses a thread boundary, and a caller who wants concurrency pays that
/// bound. The body delegates to [`review_body_send`], so the two forms run the
/// same journey rather than two things that look alike.
fn review_task_send() -> Result<Task<SendBody>, Box<dyn std::error::Error>> {
    let body: SendBody = async_body;
    Ok(task("review-pr", body)?)
}

/// The `Send` body as a plain function pointer.
///
/// A named function rather than an `async fn` item, because a `Task` is generic
/// over its body's *type* and two spellings of the same body are two types.
fn async_body(
    scope: Scope,
    input: (Gh, ReviewRequest),
) -> std::pin::Pin<
    Box<
        dyn std::future::Future<Output = Result<ReviewOutcome, lgwks_bot::script::FlowError>>
            + Send,
    >,
> {
    Box::pin(review_body_send(scope, input))
}

/// The closure shape [`review_task_send`] builds: an `async fn`, so `Send`.
type SendBody = fn(Scope, (Gh, ReviewRequest)) -> SendFuture;

/// The future an `async fn` body returns, named rather than written out twice.
type SendFuture = std::pin::Pin<
    Box<
        dyn std::future::Future<Output = Result<ReviewOutcome, lgwks_bot::script::FlowError>>
            + Send,
    >,
>;

/// Why a run could not establish its publication, from wherever it recorded it.
///
/// Two places, and the difference is the point of `INV-BOT-80`: a run that
/// *failed* carries a located [`lgwks_bot::script::FlowError`], while a run that
/// reconciled but could not conclude carries `ReviewOutcome::Unknown` and is a
/// **successful** run whose report has no error at all. A family that only read
/// `report.error()` would see an empty reason for the second case and conclude
/// the run reported nothing — which is the opposite of what it did.
fn why_unknown(report: &Report<ReviewOutcome>) -> String {
    let unknown = report
        .output()
        .filter(|outcome| matches!(outcome, ReviewOutcome::Unknown { .. }));
    match unknown {
        // A `#[non_exhaustive]` outcome behind a shared reference cannot be
        // destructured without either moving out of it or binding through `ref`,
        // and this crate forbids the `&` pattern that would satisfy the pattern
        // lints. Matching on the variant and then reading the field through its
        // own accessor is the third way, and it is the one a consumer uses.
        Some(outcome) => String::from(
            outcome
                .unknown_reason()
                .unwrap_or("an unknown outcome with no reason"),
        ),
        None => report.error().map_or_else(
            || String::from("the run reported neither an outcome nor a failure"),
            ToString::to_string,
        ),
    }
}

/// The pull request every family reviews.
fn pull(number: u64) -> Result<PullRequest, Box<dyn std::error::Error>> {
    Ok(PullRequest::new(Repository::new("acme/widgets")?, number))
}

/// A request for `number` with the shared body.
fn request(number: u64) -> Result<ReviewRequest, Box<dyn std::error::Error>> {
    Ok(ReviewRequest::new(pull(number)?, "COMMENT", BODY).with_marker("lgwks-sim"))
}

/// The capture ceiling a fault runs with.
///
/// A per-fault value rather than one shared ceiling, because two different
/// bounds are under test and they must not be confused for one another. The
/// oversized family needs a ceiling its own padding can exceed. The
/// over-ceiling family needs a ceiling its 1,001 records fit *underneath* — a
/// list refused by the byte ceiling would be a truncation, which is a different
/// refusal from the review ceiling and would make the family vacuous.
fn capture_for(fault: Fault) -> usize {
    match fault {
        Fault::AnswerOversized => 1024,
        // Roughly 8 MiB against a ~101 KB body: comfortably above, so the list
        // is refused for its *length* and not for its bytes.
        Fault::ListOverCeiling => 8 * 1024 * 1024,
        _ => CAPTURE,
    }
}

/// The scenario a fault describes, with `head` as the pull request's head.
fn scenario_for(fault: Fault, head: &str) -> Scenario {
    let scenario = Scenario::new(head);
    match fault {
        Fault::Clean | Fault::ClientAbsent => scenario,
        Fault::ResponseLost => scenario.accept_then_drop().created_body(BODY),
        Fault::CreateRefused => scenario.refuse_creates(),
        Fault::ReadsRefused => scenario.fail_reads(2),
        Fault::HeadMoved => scenario.head_moves_to(MOVED),
        Fault::ListOverCeiling => scenario.created_body(BODY).with_filler_reviews(
            u32::try_from(lgwks_bot::domain::gh::MAX_REVIEWS_PER_PULL)
                .unwrap_or(1)
                .saturating_add(1),
        ),
        Fault::ListNotJson => scenario.answers_reviews_with("garbage"),
        Fault::ListTruncated => scenario.answers_reviews_with("truncated"),
        Fault::AnswerOversized => scenario.created_body(BODY).floods(400),
    }
}

/// Whether `fault` runs no client at all.
const fn needs_no_fixture(fault: Fault) -> bool {
    matches!(fault, Fault::ClientAbsent)
}

/// Run one seed's fault through the real path and return what it observed.
///
/// The whole family is this one function: install the fixture, configure it for
/// the fault, run the journey through a host, and read back what the receiver
/// actually saw. A family that assembled its own run would be a family testing
/// its own harness.
fn run_seed(index: u64, offset: u64) -> Result<(Fault, Run), Box<dyn std::error::Error>> {
    let fault = Fault::for_index(index, offset);
    let host = host()?;
    let job = review_task()?;

    if needs_no_fixture(fault) {
        // No fixture is installed, so the binding names a program that cannot
        // be resolved and the platform refuses the fork.
        let gh = Gh::new(Repository::new("acme/widgets")?)
            .program("lgwks-no-such-github-client-on-this-path")
            .deadline(Some(Duration::from_secs(5)));
        let report = host.block_on(&job, (gh, request(7)?))?;
        return Ok((
            fault,
            Run {
                report,
                creates: 0,
                argv: String::from("<no client was ever started>"),
            },
        ));
    }

    let fake = FakeGh::install(fault.label(), HEAD)?;
    fake.configure(scenario_for(fault, HEAD))?;
    let report = host.block_on(&job, (gh_for(&fake, capture_for(fault))?, request(7)?))?;
    let argv = normalized_argv(&fake.calls()?);
    Ok((
        fault,
        Run {
            report,
            creates: fake.creates()?,
            argv,
        },
    ))
}

/// The property every family asserts, in one place.
///
/// It is here rather than written out four times because four copies of a safety
/// argument is four chances for one of them to be the one that drifted, and a
/// copy that drifted would be a test that passes for the wrong reason.
fn assert_the_invariant(fault: Fault, run: &Run) -> Result<(), Box<dyn std::error::Error>> {
    let trace = run.trace();

    // One. A publication is reported only when an independent read observed it.
    // Checked from both directions: the positive, and the negative — a fault
    // that cannot produce an observation must never report one.
    if let Some(outcome) = run.report.output() {
        match *outcome {
            ReviewOutcome::Published { verified, .. } => {
                if !fault.can_be_verified() {
                    return Err(format!(
                        "{} reported a publication it could not have observed: {outcome:?}\n{}",
                        fault.label(),
                        trace.argv
                    )
                    .into());
                }
                if !verified {
                    return Err("a Published outcome must carry verified = true".into());
                }
            }
            ReviewOutcome::Unknown { ref commit_id, .. } => {
                if fault.can_be_verified() {
                    return Err(format!(
                        "{} could have been observed but was reported unknown at {commit_id}: {outcome:?}",
                        fault.label()
                    )
                    .into());
                }
            }
            ReviewOutcome::TargetMoved { .. } => {
                // A refusal, not a publication. It must name the commit that
                // was read, so a moved head cannot be reported against a
                // commit nobody read, and it must never appear for a fault
                // whose head never moved.
                if fault != Fault::HeadMoved {
                    return Err(format!(
                        "{} reported a moved head, but only a head that changed \
                         can produce one: {outcome:?}",
                        fault.label()
                    )
                    .into());
                }
            }
            ReviewOutcome::Refused { .. } => {
                // Draft-only and similar profiles. No run in this family holds
                // one, so seeing it means a state machine reached a branch the
                // journey does not name.
                return Err(format!(
                    "{} reported a draft-only refusal, which no fault here produces",
                    fault.label()
                )
                .into());
            }
            ref other => {
                return Err(format!(
                    "{} reported {other:?}, which no run in this family produces",
                    fault.label()
                )
                .into());
            }
        }
    } else if run.report.disposition().label() == "Succeeded" {
        return Err(format!(
            "{} reported a successful run with no outcome at all",
            fault.label()
        )
        .into());
    }

    // Two. At most one create per run, whatever the fault. This is the number a
    // duplicate-post defect doubles, and it is measured at the receiver rather
    // than inferred from the outcome.
    if run.creates > 1 {
        return Err(format!(
            "{} issued {} creates: a run publishes at most once\n{}",
            fault.label(),
            run.creates,
            trace.argv
        )
        .into());
    }

    // Three. A fault that never reaches the create lands nothing, so the
    // receiver's record must agree with the fault's applicability.
    if !fault.applies() && run.creates != 0 {
        return Err(format!(
            "{} must publish nothing, but {} creates were issued\n{}",
            fault.label(),
            run.creates,
            trace.argv
        )
        .into());
    }

    Ok(())
}

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
            report.error().map_or_else(String::new, ToString::to_string)
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
                report.error().map_or_else(String::new, ToString::to_string)
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
        fake.configure(scenario_for(fault, HEAD))?;
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
        fake.configure(scenario_for(Fault::ListOverCeiling, HEAD))?;
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
        let (first_fault, first) = run_seed(index, 0)?;
        let (second_fault, second) = run_seed(index, 0)?;
        assert_eq!(
            first_fault, second_fault,
            "seed {index} must select the same fault twice"
        );
        assert_eq!(
            trace_hash(&first.trace()),
            trace_hash(&second.trace()),
            "seed {index} ({}) must replay exactly: {:?} vs {:?}",
            first_fault.label(),
            first.trace(),
            second.trace()
        );
        // The argv comparison is the stronger claim: the receiver was told the
        // same things, in the same order, by both runs.
        assert_eq!(
            first.argv,
            second.argv,
            "seed {index} ({}) must issue the same calls twice",
            first_fault.label()
        );
    }
    Ok(())
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

/// The saturation family: many concurrent runs, each bounded.
///
/// Three levels rather than one, because a bound that holds at 100 and leaks at
/// 10,000 is not a bound. The runs are genuinely concurrent — a bounded
/// [`lgwks_bot::rt::task::join_all_bounded`] pipeline, not a loop of sequential
/// `block_on` calls — because "concurrent" is the property under test and a
/// sequential loop would measure admission arithmetic rather than the
/// receiver's behaviour under load.
///
/// The observation is the receiver's record: N runs on one pull request produce
/// at most N creates (a duplicate-post defect produces more) and at least as
/// many as the runs that reported a publication (a lost-write defect produces
/// fewer), with at least one read-back per create (a publication reported
/// without an independent observation produces fewer).
#[test]
fn saturation_r32() -> TestResult {
    for concurrency in [100usize, 1_000, 10_000] {
        let fake = FakeGh::install("saturation", HEAD)?;
        fake.configure(Scenario::new(HEAD).created_body(BODY))?;
        let host = Host::builder("reviewer-saturation")?
            .max_concurrent_tasks(NonZeroUsize::new(concurrency).ok_or("a non-zero ceiling")?)
            .default_deadline(Duration::from_secs(300))
            .build()?;
        // The pipeline's own bound, held well below the run count so the join
        // set is provably bounded rather than merely large.
        let in_flight = concurrency.min(64);

        let started = std::time::Instant::now();
        // Every input is built up front and owned, because a spawned task is
        // `Send + 'static`: a borrowed fixture could not cross into one, and
        // building the inputs outside the task also means a setup failure is
        // reported here rather than swallowed as a missing result.
        let job = std::sync::Arc::new(review_task_send()?);
        let work: Vec<(Gh, ReviewRequest)> = (0..concurrency)
            .map(|_| Ok((gh_for(&fake, CAPTURE)?, request(7)?)))
            .collect::<Result<Vec<(Gh, ReviewRequest)>, Box<dyn std::error::Error>>>()?;
        let host = std::sync::Arc::new(host);
        let futures = work.into_iter().map(move |(gh, request)| {
            let host = std::sync::Arc::clone(&host);
            let job = std::sync::Arc::clone(&job);
            async move { host.run(&job, (gh, request)).await }
        });
        let reports = lgwks_bot::Runtime::new()?
            .block_on(lgwks_bot::rt::task::join_all_bounded(in_flight, futures));
        let elapsed = started.elapsed();

        let mut published = 0usize;
        let mut unknown = 0usize;
        let mut failed = 0usize;
        let mut finished = 0usize;
        for report in &reports {
            finished = finished.saturating_add(1);
            if report.output().is_some_and(ReviewOutcome::is_published) {
                published = published.saturating_add(1);
            } else if report.output().is_some_and(ReviewOutcome::is_unknown) {
                unknown = unknown.saturating_add(1);
            } else if report.disposition().label() == "Failed" {
                failed = failed.saturating_add(1);
            }
        }
        assert_eq!(
            finished, concurrency,
            "concurrency {concurrency}: the owner must join every run it started, \
             not drop the ones still in flight"
        );
        let creates = fake.creates()?;
        let reads = fake.reads_of("/pulls/7/reviews")?;

        assert!(
            creates <= concurrency,
            "concurrency {concurrency}: {creates} creates for {concurrency} runs \
             means at least one run published twice"
        );
        assert!(
            creates >= published,
            "concurrency {concurrency}: {creates} creates cannot account for only \
             {published} reported publications — a create went missing"
        );
        assert!(
            reads >= creates,
            "concurrency {concurrency}: {reads} read-backs cannot account for \
             {creates} creates — a publication was reported without an \
             independent read"
        );
        lgwks_std::trace::info!(
            concurrency = concurrency,
            in_flight = in_flight,
            published = published,
            unknown = unknown,
            failed = failed,
            creates = creates,
            reads = reads,
            "saturation: the receiver's record at this concurrency"
        );
        lgwks_std::trace::info!(
            concurrency = concurrency,
            elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
            "saturation: wall time for every run at this concurrency"
        );
    }
    Ok(())
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
        let fake = FakeGh::install("tenants", HEAD)?;
        fake.configure(Scenario::new(HEAD))?;
        let host = host()?;
        let path = fake.search_path()?;

        let run_tenant = |body: &'static str| -> Result<ReviewOutcome, Box<dyn std::error::Error>> {
            // One body per identity, bound at declaration: the closure is
            // what a real consumer supplies, so the cross-talk question is
            // "does this identity verify its own review", not "does the
            // harness remember which body it passed".
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
                .deadline(Some(Duration::from_secs(20)))
                .env("PATH", &path);
            let request = ReviewRequest::new(pull(7)?, "COMMENT", body).with_marker(body);
            let report = host.block_on(&job, (gh, request))?;
            report
                .output()
                .cloned()
                .ok_or_else(|| "each identity must produce an outcome".into())
        };

        let first = run_tenant("first identity")?;
        let second = run_tenant("second identity")?;

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
        let failure = report.error().map_or_else(String::new, ToString::to_string);
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
                report.error().map_or_else(String::new, ToString::to_string)
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
        assert!(
            first_id.is_some() && second_id.is_some(),
            "seed {index}: both runs must name a review: {first_id:?} {second_id:?}"
        );
        // Both ids must lie in the range the receiver actually handed out
        // (`next_review_id` and up), so a run that invented one — or reported
        // the id from some other world — would fall outside it.
        let first = first_id.unwrap_or_default();
        let second = second_id.unwrap_or_default();
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
                    .deadline(Some(Duration::from_secs(20)))
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

// ── Subject, coverage and partial-submission family (T31, T33, T34) ─────────

/// The fault family the subject/coverage/partial/permission rows sweep.
///
/// Named rather than indexed so a family's fault and its expectation read
/// together, and exhaustive over the states those rows name: a rename, an
/// unavailable diff, each diff ceiling, an unsubmitted draft, a partial
/// submission, and a lost read permission.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum SubjectFault {
    /// Everything works; the create answers and the read-back finds it whole.
    Clean,
    /// The repository answers with a renamed-repository redirect.
    Moved,
    /// The server declines to render the diff.
    DiffUnavailable,
    /// The changed-file inventory is past its file-count ceiling.
    DiffOverFiles,
    /// The changed-file patch text is past its byte ceiling.
    DiffOverBytes,
    /// The create landed as an unsubmitted draft and its response was lost.
    PendingDraft,
    /// The create landed with fewer inline comments than intended, response lost.
    PartialComments,
    /// The create landed and the read-back lost permission.
    ReadDenied,
}

impl SubjectFault {
    /// How many faults the family contains.
    const VARIANTS: u64 = 8;

    /// Every fault's label, so a coverage test does not re-list the enum.
    const LABELS: [&'static str; 8] = [
        "clean",
        "moved",
        "diff-unavailable",
        "diff-over-files",
        "diff-over-bytes",
        "pending-draft",
        "partial-comments",
        "read-denied",
    ];

    /// The fault for `index`.
    const fn for_index(index: u64) -> Self {
        match index % Self::VARIANTS {
            0 => Self::Clean,
            1 => Self::Moved,
            2 => Self::DiffUnavailable,
            3 => Self::DiffOverFiles,
            4 => Self::DiffOverBytes,
            5 => Self::PendingDraft,
            6 => Self::PartialComments,
            _ => Self::ReadDenied,
        }
    }

    /// A short label, for the trace and for the failure message.
    const fn label(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Moved => "moved",
            Self::DiffUnavailable => "diff-unavailable",
            Self::DiffOverFiles => "diff-over-files",
            Self::DiffOverBytes => "diff-over-bytes",
            Self::PendingDraft => "pending-draft",
            Self::PartialComments => "partial-comments",
            Self::ReadDenied => "read-denied",
        }
    }

    /// Whether a run under this fault may report a verified publication.
    ///
    /// Only the clean world can: every other fault is a refusal, a coverage
    /// decision or an unverified effect, and a run that reported `Published`
    /// under one would be reporting an effect it did not observe.
    const fn can_be_verified(self) -> bool {
        matches!(self, Self::Clean)
    }

    /// How much of a call's answer the family must retain for this fault.
    ///
    /// The two ceiling faults build an answer larger than the default capture,
    /// so their envelope is raised: the refusal under test is the *diff*
    /// ceiling, not the capture bound.
    const fn capture(self) -> usize {
        match self {
            Self::DiffOverFiles | Self::DiffOverBytes => 4 * 1024 * 1024,
            _ => CAPTURE,
        }
    }
}

/// The two inline comments a partial-submission run intends.
fn comments(count: u32) -> Vec<ReviewComment> {
    (1..=count)
        .map(|line| {
            ReviewComment::new(
                format!("src/f{line}.rs"),
                u64::from(line),
                format!("comment {line}"),
            )
        })
        .collect()
}

/// What one seed draws beyond its fault.
///
/// The fault is `index % 8`, so a band of eight seeds reaches every fault; the
/// draw is what makes two seeds with one fault two different worlds — a diff one
/// file or sixty-four files past the ceiling, a partial submission that landed
/// none of five comments or four of them — rather than one scenario replayed.
#[derive(Debug, Clone, Copy)]
struct SubjectDraw {
    /// Files past `MAX_DIFF_FILES_PER_PULL` the receiver reports.
    files_over: u32,
    /// Patch bytes past `MAX_DIFF_BYTES` the receiver reports.
    bytes_over: u32,
    /// Inline comments the run intends.
    intended: u32,
    /// How many of them the receiver says landed; always fewer than intended.
    applied: u32,
}

impl SubjectDraw {
    /// The draw for seed `index`, from the shared generator.
    fn for_seed(index: u64) -> Self {
        let mut rng = sim::Rng::new(index);
        let intended = rng.between(2, 6);
        Self {
            files_over: rng.between(1, 64),
            bytes_over: rng.between(1, 4096),
            intended,
            applied: rng.below(intended),
        }
    }
}

/// The scenario a subject fault describes.
fn subject_scenario(fault: SubjectFault, head: &str, draw: SubjectDraw) -> Scenario {
    let scenario = Scenario::new(head);
    match fault {
        SubjectFault::Clean => scenario,
        SubjectFault::Moved => scenario.repository_moved(),
        SubjectFault::DiffUnavailable => scenario.diff_unavailable(),
        SubjectFault::DiffOverFiles => scenario.with_filler_files(
            u32::try_from(lgwks_bot::domain::gh::MAX_DIFF_FILES_PER_PULL)
                .unwrap_or(0)
                .saturating_add(draw.files_over),
        ),
        SubjectFault::DiffOverBytes => scenario.with_diff_bytes(
            u32::try_from(lgwks_bot::domain::gh::MAX_DIFF_BYTES)
                .unwrap_or(0)
                .saturating_add(draw.bytes_over),
        ),
        SubjectFault::PendingDraft => scenario.records_pending_draft(),
        SubjectFault::PartialComments => scenario.records_partial_comments(draw.applied),
        SubjectFault::ReadDenied => scenario.deny_reads(),
    }
}

/// Run one seed's subject fault through the real path.
fn run_subject_seed(index: u64) -> Result<(SubjectFault, Run), Box<dyn std::error::Error>> {
    run_subject_fault(SubjectFault::for_index(index), index)
}

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
            return Err(format!(
                "seed {index} ({}): a reported refusal, not a run failure: {}",
                fault.label(),
                run.report
                    .error()
                    .map_or_else(String::new, ToString::to_string)
            )
            .into());
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
                    return Err(format!(
                        "seed {index}: a submission with fewer comments than intended \
                         is Partial: {outcome:?}"
                    )
                    .into());
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
        let (first_fault, first) = run_subject_seed(index)?;
        let (second_fault, second) = run_subject_seed(index)?;
        assert_eq!(
            first_fault, second_fault,
            "seed {index} selects one fault twice"
        );
        assert_eq!(
            trace_hash(&first.trace()),
            trace_hash(&second.trace()),
            "seed {index} ({}) must replay exactly: {:?} vs {:?}",
            first_fault.label(),
            first.trace(),
            second.trace()
        );
        assert_eq!(
            first.argv,
            second.argv,
            "seed {index} ({}) must issue the same calls twice",
            first_fault.label()
        );
    }
    Ok(())
}

/// Two identities on one pull request each verify only their own review.
fn two_identities_subject(band: sim::Band) -> TestResult {
    for index in band.seeds() {
        let fake = FakeGh::install("subject-tenants", HEAD)?;
        fake.configure(Scenario::new(HEAD))?;
        let host = host()?;
        let path = fake.search_path()?;

        let run_tenant = |body: &'static str| -> Result<ReviewOutcome, Box<dyn std::error::Error>> {
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
                .deadline(Some(Duration::from_secs(20)))
                .env("PATH", &path);
            let request = ReviewRequest::new(pull(7)?, "COMMENT", body).with_marker(body);
            let report = host.block_on(&job, (gh, request))?;
            report
                .output()
                .cloned()
                .ok_or_else(|| "each identity must produce an outcome".into())
        };

        let first = run_tenant("first identity")?;
        let second = run_tenant("second identity")?;
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

/// The over-ceiling draws a family sweeps, shared by every arm family below.
///
/// One helper rather than a per-family literal: the arm families differ in what
/// they assert about a draw, not in how a draw is produced, and four copies of
/// the generator would be four chances for one arm's fault space to drift from
/// the others'.
fn over_ceiling_draw(index: u64) -> SubjectDraw {
    SubjectDraw::for_seed(index)
}

/// Run one seed under one subject fault and return the receiver's record.
///
/// [`run_subject_seed`] is this with the seed's own fault, so an arm family
/// cannot be testing a different run than the sweep does. The fault is named
/// rather than taken from the seed so an arm family sweeps *one* arm across
/// many draws, which is the sampling the arm properties are about.
fn run_subject_fault(
    fault: SubjectFault,
    index: u64,
) -> Result<(SubjectFault, Run), Box<dyn std::error::Error>> {
    let draw = over_ceiling_draw(index);
    let host = host()?;
    let job = review_task()?;
    let fake = FakeGh::install(fault.label(), HEAD)?;
    fake.configure(subject_scenario(fault, HEAD, draw))?;

    let mut request = request(7)?;
    if fault == SubjectFault::PartialComments {
        request = request.with_comments(comments(draw.intended));
    }
    let report = host.block_on(&job, (gh_for(&fake, fault.capture())?, request))?;
    let argv = normalized_argv(&fake.calls()?);
    Ok((
        fault,
        Run {
            report,
            creates: fake.creates()?,
            argv,
        },
    ))
}

/// The reason a run recorded, wherever it recorded it.
fn reason_of(run: &Run) -> String {
    run.report
        .output()
        .filter(|outcome| !matches!(outcome, ReviewOutcome::Published { .. }))
        .map_or_else(
            || {
                run.report
                    .error()
                    .map_or_else(String::new, ToString::to_string)
            },
            |outcome| match outcome.unknown_reason() {
                Some(reason) => String::from(reason),
                None => format!("{outcome:?}"),
            },
        )
}

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
        let ceiling = u32::try_from(lgwks_bot::domain::gh::MAX_DIFF_BYTES).unwrap_or(u32::MAX);
        assert!(
            draw.bytes_over > 0,
            "seed {index}: the byte arm needs a draw past the byte ceiling"
        );
        assert!(
            draw.files_over
                <= u32::try_from(lgwks_bot::domain::gh::MAX_DIFF_FILES_PER_PULL)
                    .unwrap_or(u32::MAX),
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
/// each creating at most once and publishing only what it observed.
///
/// The concurrency counterpart to the arm families above: a per-seed arm is one
/// world at a time, and a bound that holds for one run says nothing about a
/// host running a hundred. The observation is the receiver's record — N runs
/// produce at most N creates (a duplicate-post defect produces more) and at
/// least as many read-backs as creates (a publication reported without an
/// independent observation produces fewer).
#[test]
fn subject_saturation_conserves_creates() -> TestResult {
    for index in 0..16u64 {
        let fake = FakeGh::install("subject-saturation", HEAD)?;
        fake.configure(Scenario::new(HEAD).created_body(BODY))?;
        let host = host()?;
        let job = std::sync::Arc::new(review_task_send()?);
        let work: Vec<(Gh, ReviewRequest)> = (0..64)
            .map(|_| Ok((gh_for(&fake, CAPTURE)?, request(7)?)))
            .collect::<Result<Vec<(Gh, ReviewRequest)>, Box<dyn std::error::Error>>>()?;
        let host = std::sync::Arc::new(host);
        let futures = work.into_iter().map(move |(gh, request)| {
            let host = std::sync::Arc::clone(&host);
            let job = std::sync::Arc::clone(&job);
            async move { host.run(&job, (gh, request)).await }
        });
        let reports =
            lgwks_bot::Runtime::new()?.block_on(lgwks_bot::rt::task::join_all_bounded(16, futures));

        let published = reports
            .iter()
            .filter(|report| report.output().is_some_and(ReviewOutcome::is_published))
            .count();
        let creates = fake.creates()?;
        let reads = fake.reads_of("/pulls/7/reviews")?;
        assert_eq!(
            reports.len(),
            64,
            "seed {index}: the owner must join every run it started"
        );
        assert!(
            creates <= 64,
            "seed {index}: {creates} creates for 64 runs means at least one run \
             published twice"
        );
        assert!(
            creates >= published,
            "seed {index}: {creates} creates cannot account for only {published} \
             reported publications — a create went missing"
        );
        assert!(
            reads >= creates,
            "seed {index}: {reads} read-backs cannot account for {creates} creates — \
             a publication was reported without an independent read"
        );
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
        ))?;

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
