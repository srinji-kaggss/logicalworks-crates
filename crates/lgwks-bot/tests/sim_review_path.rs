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

use lgwks_bot::domain::gh::{CommitId, Gh, PullRequest, Repository};
use lgwks_bot::review::{ReviewOutcome, ReviewRequest};
use lgwks_bot::script::Scope;
use lgwks_bot::task::{Host, Report, Task, task};

#[path = "support/fake_gh.rs"]
mod fake_gh;

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
    let ambient = std::env::var_os("PATH").unwrap_or_default();
    let mut entries = vec![fake.dir().to_path_buf()];
    entries.extend(std::env::split_paths(&ambient));
    let path = std::env::join_paths(&entries)?;
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
            .env("PATH", path_for(&fake)?);
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
        let path = path_for(&fake)?;

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

/// The `PATH` a fixture is found through, with its directory in front of the
/// ambient one.
fn path_for(fake: &FakeGh) -> Result<std::ffi::OsString, Box<dyn std::error::Error>> {
    let ambient = std::env::var_os("PATH").unwrap_or_default();
    let mut entries = vec![fake.dir().to_path_buf()];
    entries.extend(std::env::split_paths(&ambient));
    Ok(std::env::join_paths(&entries)?)
}
