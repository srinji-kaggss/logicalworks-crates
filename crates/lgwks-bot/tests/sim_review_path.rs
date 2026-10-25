//! Deterministic simulation of the **real** review path, across many seeds.
//!
//! [`sim_review_pr`](./it/sim_review_pr.rs) simulates the decision the
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
//! - `saturation_r32_tier_100`/`_1000`/`_10000`: 100, 1,000 and 10,000 concurrent
//!   runs as one test per tier, each checked at its own receiver; one host, one
//!   bounded pipeline per tier, receivers sharded at 100 runs so the fixture's
//!   read-back stays linear. Splitting the tiers into separate tests lets
//!   nextest schedule them alongside the rest of the suite rather than in one
//!   serialized loop.
//! - `two_tenants_on_one_pull_request_r32`: two identities on one pull request,
//!   each verifying only its own review.
//! - `retention_is_bounded_by_the_declared_ceiling`: what the adapter holds is
//!   bounded by its declaration, not by what the child wrote.
//! - `the_run_bound_outlasts_every_call_a_run_makes`: no host in this binary
//!   declares a run bound shorter than the per-call bound times the calls a run
//!   makes, so a loaded machine can slow a run and never change its verdict.
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

use lgwks_bot::domain::gh::{Gh, PullRequest, Repository};
use lgwks_bot::review::{ReviewOutcome, ReviewRequest};
use lgwks_bot::script::Scope;
use lgwks_bot::task::{Host, MAX_TASK_DEADLINE, Report, Task, task};

// Its own binary rather than a module of `it` (#272): the saturation CI job
// runs only this file's tiers, and building the whole `it` binary for them put
// 80 s of compile on the gate's critical path.
#[path = "support/fake_gh.rs"]
mod fake_gh;

// The band arithmetic and the seeded generator, and nothing else: the rest of
// the substrate drives journals and a network this family never opens.
mod sim {
    mod band;
    mod seed;

    pub use band::{Band, band_of};
    pub use seed::{Rng, Trace};
}

// The band-declaration macro, defined once for the whole layer.
#[path = "sim/bands.rs"]
mod band_family;

use fake_gh::{FakeGh, HANG_BOUND, Scenario};

// The families, by responsibility (the size law): the fault space and the
// tenant, retention and cancellation families; the saturation machinery; the
// subject fault space; the subject sweeps and per-arm properties; and the
// ordering of the run bound against the call bound. The fault enum, the run
// record and the harness stay here and the modules reach them as descendants
// of this crate root.
#[path = "sim_review_families/bounds.rs"]
mod bounds;
#[path = "sim_review_families/families.rs"]
mod families;
#[path = "sim_review_families/saturation.rs"]
mod saturation;
#[path = "sim_review_families/subject.rs"]
mod subject;
#[path = "sim_review_families/subject_arms.rs"]
mod subject_arms;

use saturation::{IN_FLIGHT_CAP, run_saturation_tier};

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
    /// The review list is served in pages and the run's own review is on the
    /// third, which only a client that follows every page ever reads.
    ListPaginated,
}

impl Fault {
    /// How many faults the family contains.
    const VARIANTS: u64 = 11;

    /// Every fault's label, so a coverage test does not re-list the enum.
    const LABELS: [&'static str; 11] = [
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
        "list-paginated",
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
            9 => Self::AnswerOversized,
            _ => Self::ListPaginated,
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
            Self::ListPaginated => "list-paginated",
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
        matches!(self, Self::Clean | Self::ResponseLost | Self::ListPaginated)
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
struct Conclusion {
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
    fn trace(&self) -> Conclusion {
        let outcome = match self.report.output() {
            None => String::from("none"),
            Some(outcome) => match *outcome {
                ReviewOutcome::Published {
                    review_id,
                    ref commit_id,
                    verified,
                } => {
                    text_hash(&format!("published:{review_id}:{commit_id}:{verified}")).to_string()
                }
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
        };
        Conclusion {
            disposition: String::from(self.report.disposition().label()),
            outcome,
            creates: self.creates,
            argv: self.argv.clone(),
        }
    }
}

/// A stable hash of a run's conclusion, so a replay is checkable rather than
/// asserted.
///
/// Through the substrate's FNV-1a receipt rather than `DefaultHasher`, whose
/// algorithm std leaves free to change between releases: a hash recorded before
/// a toolchain bump must still compare equal after it.
fn trace_hash(conclusion: &Conclusion) -> u64 {
    let mut trace = sim::Trace::new();
    trace.record(&conclusion.disposition);
    trace.record(&conclusion.outcome);
    trace.record_number("creates", conclusion.creates);
    trace.record(&conclusion.argv);
    trace.hash()
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
///
/// Through the substrate's FNV-1a receipt rather than `DefaultHasher`, whose
/// algorithm std leaves free to change between releases: a hash recorded before
/// a toolchain bump must still compare equal after it.
fn text_hash(text: &str) -> u64 {
    let mut trace = sim::Trace::new();
    trace.record(text);
    trace.hash()
}

// ── The harness ─────────────────────────────────────────────────────────────

/// A host sized for a family's worst case, bounded only by the host's own
/// ceiling so a test fails on a real hang rather than on the clock.
fn host() -> Result<Host, Box<dyn std::error::Error>> {
    host_named("reviewer-sim", 64)
}

/// The one constructor every host in this binary comes from: `tasks` admitted at
/// once, and a run bound that is the host's own ceiling.
///
/// `script::within` arms an engine timer as the real-time watchdog for either
/// clock, so a host's deadline is a wall bound on the whole run. A run makes
/// several `gh` calls, each already bounded by [`HANG_BOUND`] and each naming what
/// it was reading when it was stopped. A run bound shorter than those calls
/// together fires on a run whose every call is inside its own bound, and on a
/// loaded host that run is slow, not hung: the same seed concluded
/// `DeadlineExceeded` on one run and `Succeeded` on its replay (main CI run
/// 40085291776, seeds 75 and 15, issue #751). The ceiling removes that verdict;
/// the per-call bound and nextest's own termination remain the hang detectors,
/// and `bounds::the_run_bound_outlasts_every_call_a_run_makes` holds the ordering.
fn host_named(name: &str, tasks: usize) -> Result<Host, Box<dyn std::error::Error>> {
    Ok(Host::builder(name)?
        .max_concurrent_tasks(NonZeroUsize::new(tasks).ok_or("a non-zero ceiling")?)
        .default_deadline(MAX_TASK_DEADLINE)
        .build()?)
}

/// The `Gh` binding pointed at `fake`, with a capture ceiling the family chose.
///
/// `capture` is a per-family parameter because the oversized family needs a
/// ceiling it can exceed and every other family needs one it cannot; a single
/// shared value would make one of them untestable.
fn gh_for(fake: &FakeGh, capture: usize) -> Result<Gh, Box<dyn std::error::Error>> {
    fake.binding("acme/widgets", capture)
}

/// The `Gh` binding the saturation tiers drive: the fake named by its absolute
/// path, with no `PATH` override.
///
/// That is the spawn a default binding makes, and it is not the spawn
/// [`gh_for`] makes. `std` hands a child to `posix_spawn` unless the caller
/// overrides `PATH` and names a bare program, in which case it must search that
/// `PATH` itself and falls back to `fork` + `execvp`. Forking a test process
/// with sixty-four runs in flight copies its page tables and, on macOS, takes
/// every malloc zone lock, once per call — five calls a run, fifty thousand at
/// the 10,000 tier — so the tier timed the harness's fork rather than the
/// adapter. `PATH` resolution itself is still exercised by every other family
/// through [`gh_for`]; the tiers measure concurrency through the adapter, which
/// this binding leaves unchanged.
fn gh_spawned_directly(fake: &FakeGh, capture: usize) -> Result<Gh, Box<dyn std::error::Error>> {
    let program = fake.dir().join(fake.program());
    let program = program
        .to_str()
        .ok_or("the fixture directory is not valid UTF-8")?;
    Ok(Gh::new(Repository::new("acme/widgets")?)
        .program(program)
        .capture_limit(NonZeroUsize::new(capture).ok_or("a non-zero limit")?)
        .deadline(Some(HANG_BOUND)))
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
        Some(outcome) => match outcome.unknown_reason() {
            Some(reason) => String::from(reason),
            None => String::from("an unknown outcome with no reason"),
        },
        None => match report.error() {
            Some(error) => error.to_string(),
            None => String::from("the run reported neither an outcome nor a failure"),
        },
    }
}

/// The failure a report carries, as text; empty for a run that recorded none.
///
/// One definition because several families print it in a failure message, and a
/// `match` written out at each of them is a place for the reading to drift.
fn error_text(report: &Report<ReviewOutcome>) -> String {
    match report.error() {
        Some(error) => error.to_string(),
        None => String::new(),
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
fn scenario_for(fault: Fault, head: &str) -> Result<Scenario, Box<dyn std::error::Error>> {
    let scenario = Scenario::new(head);
    Ok(match fault {
        Fault::Clean | Fault::ClientAbsent => scenario,
        Fault::ResponseLost => scenario.accept_then_drop().created_body(BODY),
        Fault::CreateRefused => scenario.refuse_creates(),
        Fault::ReadsRefused => scenario.fail_reads(2),
        Fault::HeadMoved => scenario.head_moves_to(MOVED),
        Fault::ListOverCeiling => scenario.created_body(BODY).with_filler_reviews(
            u32::try_from(lgwks_bot::domain::gh::MAX_REVIEWS_PER_PULL)?.saturating_add(1),
        ),
        Fault::ListNotJson => scenario.answers_reviews_with("garbage"),
        Fault::ListTruncated => scenario.answers_reviews_with("truncated"),
        Fault::AnswerOversized => scenario.created_body(BODY).floods(400),
        // 250 older reviews in pages of 100, so the run's own review is record
        // 251, on the third page: a verification that read one page would not
        // find it and would have to report the publication unobserved.
        Fault::ListPaginated => scenario.with_filler_reviews(250).with_review_pages(100),
    })
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
    fake.configure(scenario_for(fault, HEAD)?)?;
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
                    let refusal = Err(format!(
                        "{} reported a publication it could not have observed: {outcome:?}\n{}",
                        fault.label(),
                        trace.argv
                    )
                    .into());
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "assert_the_invariant: returning an error to the caller");
                    return refusal;
                }
                if !verified {
                    let refusal = Err("a Published outcome must carry verified = true".into());
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "assert_the_invariant: returning an error to the caller");
                    return refusal;
                }
            }
            ReviewOutcome::Unknown { ref commit_id, .. } => {
                if fault.can_be_verified() {
                    let refusal = Err(format!(
                    "{} could have been observed but was reported unknown at {commit_id}: {outcome:?}",
                    fault.label()
                    )
                    .into());
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "assert_the_invariant: returning an error to the caller");
                    return refusal;
                }
            }
            ReviewOutcome::TargetMoved { .. } => {
                // A refusal, not a publication. It must name the commit that
                // was read, so a moved head cannot be reported against a
                // commit nobody read, and it must never appear for a fault
                // whose head never moved.
                if fault != Fault::HeadMoved {
                    let refusal = Err(format!(
                        "{} reported a moved head, but only a head that changed \
                     can produce one: {outcome:?}",
                        fault.label()
                    )
                    .into());
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "assert_the_invariant: returning an error to the caller");
                    return refusal;
                }
            }
            ReviewOutcome::Refused { .. } => {
                // Draft-only and similar profiles. No run in this family holds
                // one, so seeing it means a state machine reached a branch the
                // journey does not name.
                let refusal = Err(format!(
                    "{} reported a draft-only refusal, which no fault here produces",
                    fault.label()
                )
                .into());
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "assert_the_invariant: returning an error to the caller");
                return refusal;
            }
            ref other => {
                let refusal = Err(format!(
                    "{} reported {other:?}, which no run in this family produces",
                    fault.label()
                )
                .into());
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "assert_the_invariant: returning an error to the caller");
                return refusal;
            }
        }
    } else if run.report.disposition().label() == "Succeeded" {
        let refusal = Err(format!(
            "{} reported a successful run with no outcome at all",
            fault.label()
        )
        .into());
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "assert_the_invariant: returning an error to the caller");
        return refusal;
    }

    // Two. At most one create per run, whatever the fault. This is the number a
    // duplicate-post defect doubles, and it is measured at the receiver rather
    // than inferred from the outcome.
    if run.creates > 1 {
        let refusal = Err(format!(
            "{} issued {} creates: a run publishes at most once\n{}",
            fault.label(),
            run.creates,
            trace.argv
        )
        .into());
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "assert_the_invariant: returning an error to the caller");
        return refusal;
    }

    // Three. A fault that never reaches the create lands nothing, so the
    // receiver's record must agree with the fault's applicability.
    if !fault.applies() && run.creates != 0 {
        let refusal = Err(format!(
            "{} must publish nothing, but {} creates were issued\n{}",
            fault.label(),
            run.creates,
            trace.argv
        )
        .into());
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "assert_the_invariant: returning an error to the caller");
        return refusal;
    }

    Ok(())
}

/// The saturation family: many concurrent runs, each bounded.
///
/// One declared test per tier rather than one test looping over three, because
/// nextest schedules *tests* concurrently while a single test runs on a single
/// worker: the three-tier loop serialized the 10,000-run tier behind the
/// 1,000-run one and held the suite's critical path for their sum. Each tier is
/// still run by [`run_saturation_tier`], so the shard, the single `Host`, the
/// one bounded [`lgwks_bot::rt::task::join_all_bounded`] pipeline and the
/// per-receiver equalities are exactly the ones the loop used.
#[test]
fn saturation_r32_tier_100() -> TestResult {
    run_saturation_tier(100, 100.min(IN_FLIGHT_CAP), "saturation-100")?;
    Ok(())
}

/// The 1,000-run tier of the saturation family (see `saturation_r32_tier_100`).
#[test]
fn saturation_r32_tier_1000() -> TestResult {
    run_saturation_tier(1_000, 1_000.min(IN_FLIGHT_CAP), "saturation-1000")?;
    Ok(())
}

/// The 10,000-run tier of the saturation family (see `saturation_r32_tier_100`).
#[test]
fn saturation_r32_tier_10000() -> TestResult {
    run_saturation_tier(10_000, 10_000.min(IN_FLIGHT_CAP), "saturation-10000")?;
    Ok(())
}
