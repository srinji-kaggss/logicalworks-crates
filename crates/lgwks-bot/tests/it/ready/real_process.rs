//! The real-process half of the readiness journeys: a supervised `sh` child
//! whose own stdout line closes the gate. Split out of `ready.rs` under the
//! size law; the module path stays `ready::real_process`.

use super::{BUDGET, Generation, Named, Readiness, Scope, TestResult, refused};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use lgwks_bot::rt::process::ProcessSpec;
use lgwks_bot::rt::supervise::Supervisor;

/// The retained-byte ceiling for the child's stdout.
///
/// Bounded, and small enough to be a real bound: the journey's whole claim
/// is that readiness comes from the child's *output*, and an unbounded
/// capture would be a second way for a child to exhaust memory.
const CAPTURE: NonZeroUsize = match NonZeroUsize::new(64 * 1024) {
    Some(limit) => limit,
    None => NonZeroUsize::MIN,
};

/// The line a journey's child announces on stdout.
const ANNOUNCEMENT: &str = "READY db.internal:5432";

/// The prefix the gate accepts. Short, so a child that printed something
/// else first is not released by it.
const PREFIX: &str = "READY";

/// A `sh -c` spec that prints `ANNOUNCEMENT` after `delay` and then serves
/// for `serving` before exiting.
///
/// A real shell, a real child, and a real line: the readiness gate is closed
/// by the child's own stdout, so a journey that passed here could not have
/// passed on a timer. Every stage is a *short* real sleep, so the whole
/// journey costs the child's own lifetime rather than the supervisor's
/// deadline — the deadline above is a ceiling the test never expects to
/// reach, and a journey that took it would be measuring the ceiling instead
/// of the gate.
fn script(body: &str) -> ProcessSpec {
    let mut spec = ProcessSpec::new("sh");
    spec.arg("-c")
        .arg(body)
        .capture_stdout(CAPTURE)
        .deadline(CHILD_CEILING);
    spec
}

/// Announces after a short delay, serves briefly, exits zero.
fn announcing() -> ProcessSpec {
    script(&format!(
        "sleep 0.2; printf '%s\\n' '{ANNOUNCEMENT}'; sleep 0.2; exit 0"
    ))
}

/// Exits non-zero without ever announcing anything.
fn dying() -> ProcessSpec {
    script("sleep 0.2; exit 3")
}

/// Announces, serves briefly, then exits non-zero.
fn announcing_then_dying() -> ProcessSpec {
    script(&format!(
        "sleep 0.2; printf '%s\\n' '{ANNOUNCEMENT}'; sleep 0.2; exit 4"
    ))
}

/// The supervisor's ceiling for one journey's child.
///
/// Several times the child's own lifetime, so the run is decided by the
/// child's script and never by the deadline: a journey that hit this would
/// be a hang, not a readiness.
const CHILD_CEILING: Duration = Duration::from_secs(10);

/// How long a dependant that runs beside its child waits for the release.
///
/// The child sleeps 0.2 s before it announces, and that sleep starts only after
/// `sh` is spawned, which on a host running the whole suite beside thirteen
/// other jobs takes more than the 300 ms [`BUDGET`] leaves over (CI run
/// 39046637995, bot-full: "timed out after 500ms" with the child still
/// starting). A release ends the wait the moment the child prints, so a longer
/// budget costs a passing journey nothing; it stays under [`CHILD_CEILING`] so
/// a child that never announces ends the wait through the failure raised when
/// it exits, and never through this budget.
const RELEASE_BUDGET: Duration = Duration::from_secs(5);

/// Run `spec` on its own supervisor while a dependant waits on the
/// readiness, and report everything the two halves observed.
///
/// The two halves run concurrently on one runtime, which is the whole shape
/// of the row: the dependant is admitted and *waiting* before the child is
/// started, so a release it receives can only have come from the child's own
/// output. The gate is the child's output and nothing else — the observer
/// signals on the first line carrying the prefix, the readiness refuses
/// everything else, and the supervisor's own report says what the child did
/// afterwards.
async fn serve(readiness: &Named, dependant: &Scope, spec: &ProcessSpec) -> Result<Served, String> {
    let wait = {
        let gate = readiness.clone();
        async move {
            gate.wait(dependant, BUDGET)
                .await
                .map(|ready| ready.awaited().to_owned())
                .map_err(|error| error.to_string())
        }
    };
    let run = gate_on_child_output(spec, readiness).await?;
    let released = wait.await;
    Ok(Served {
        exit_code: run.exit_code(),
        stdout: run.stdout().bytes().to_vec(),
        dependant: released.as_ref().ok().cloned(),
        dependant_error: released.err(),
    })
}

/// Run `spec` on its own supervisor, closing `readiness` on the child's first
/// stdout line that starts with [`PREFIX`].
async fn gate_on_child_output(
    spec: &ProcessSpec,
    readiness: &Named,
) -> Result<lgwks_bot::rt::process::ProcessRun, String> {
    let gated = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&gated);
    let gate = readiness.clone();
    let needle = PREFIX.as_bytes().to_vec();
    let mut supervisor = Supervisor::new(2);
    let outcome = supervisor
        .run_process_observed(
            spec,
            Some(&move |line: &[u8]| {
                if flag.load(Ordering::SeqCst) || !line.starts_with(&needle) {
                    return;
                }
                let announced = String::from_utf8_lossy(line).into_owned();
                flag.store(true, Ordering::SeqCst);
                // A refusal here is reported by the readiness's own outcome
                // below rather than swallowed: a gate that could not close
                // must not look like one that did.
                let _refused = gate.ready(Generation::FIRST, announced);
            }),
        )
        .await;
    outcome.map_err(|error| format!("the child did not run: {error}"))
}

/// Everything one supervised child and its dependant observed.
#[derive(Debug)]
struct Served {
    /// The child's own exit code.
    exit_code: Option<i32>,
    /// The bytes the capture retained from the child's stdout.
    stdout: Vec<u8>,
    /// What the dependant was released with, or `None` if it was not.
    dependant: Option<String>,
    /// Why the dependant was not released, or `None` if it was.
    dependant_error: Option<String>,
}

/// Readiness comes from the child's own stdout line, and a dependant that was
/// already waiting is released by it.
///
/// The child sleeps before it announces, so the gate cannot have been open
/// when the dependant was admitted: a release that arrived anyway could only
/// have come from the line the child printed. The value the dependant
/// receives is that line, which no timer could fabricate.
#[test]
fn a_child_printing_its_address_releases_the_dependants() -> TestResult {
    let readiness: Named = Readiness::new("db")?;
    let dependant = crate::ready::scope_of("acme")?.enter("use-db")?;
    let served = lgwks_bot::block_on(serve(&readiness, &dependant, &announcing()))?;

    assert_eq!(served.exit_code, Some(0), "the child ran to its own end");
    assert!(
        String::from_utf8_lossy(&served.stdout).contains(ANNOUNCEMENT),
        "the announcement was retained by the capture the gate read from"
    );
    assert_eq!(
        served.dependant.as_deref(),
        Some(ANNOUNCEMENT),
        "the dependant was released with the line the child actually printed"
    );
    let outcome = readiness
        .outcome()
        .ok_or("the child's line must have closed the gate")?;
    assert_eq!(
        outcome.generation(),
        Generation::FIRST,
        "the release names the generation the gate was armed at"
    );
    assert!(
        outcome.is_ready(),
        "the gate is ready, not failed and not shut down"
    );
    Ok(())
}

/// A child that dies without printing is never released.
///
/// The child's own exit is a *report*, not a readiness: a service that exited
/// non-zero has not come up, and a dependant that waited on it must hear the
/// failure rather than be told a release that never happened. The dependant
/// here is admitted *before* the child runs, so its budget is what ends its
/// wait — which is the honest outcome for a service that never spoke, and is
/// asserted as a timeout rather than excused as a release.
#[test]
fn a_child_that_dies_without_printing_never_releases_the_dependants() -> TestResult {
    let readiness: Named = Readiness::new("db")?;
    let dependant = crate::ready::scope_of("acme")?.enter("use-db")?;
    let served = lgwks_bot::block_on(serve(&readiness, &dependant, &dying()))?;

    assert_eq!(
        served.exit_code,
        Some(3),
        "the child exited with its own non-zero code"
    );
    assert!(
        readiness.outcome().is_none(),
        "a child that never announced settled nothing"
    );
    assert!(
        served.dependant.is_none(),
        "and released no dependant, got {:?}",
        served.dependant
    );
    assert!(
        served
            .dependant_error
            .as_deref()
            .is_some_and(|error| error.contains("timed out")),
        "the dependant's own wait ended on its budget rather than a release, \
         got {:?}",
        served.dependant_error
    );
    assert_eq!(
        readiness.admitted(),
        1,
        "the dependant was admitted, which is what made it wait at all"
    );

    // The caller learns of the death by failing the readiness, which is what
    // a service body does when its supervisor's run comes back a failure.
    readiness.fail(Generation::FIRST, "the child exited 3")?;
    let later = crate::ready::scope_of("acme")?.enter("use-db")?;
    let error = refused(
        lgwks_bot::block_on(readiness.wait(&later, BUDGET)).map(|_ready| ()),
        "a dependant on a dead service must fail",
    )?;
    assert!(
        error.to_string().contains("the child exited 3"),
        "a dependant arriving after the failure hears the reason, got {error}"
    );
    Ok(())
}

/// A child that dies after announcing stops the dependants still running.
///
/// The real-exit form of the failure-after-ready journey, and the one the row
/// is really about. A dependant is released by the child's own announcement
/// line and is *still running* — parked on its own token — when the child
/// exits non-zero. The failure is raised by the body that supervised the
/// child, at the generation it was armed with, and the dependant's token is
/// cancelled: that cancellation is the observable, and it is what a
/// released-but-running dependant has to be given.
#[test]
fn a_child_that_dies_after_announcing_stops_the_dependants() -> TestResult {
    let readiness: Named = Readiness::new("db")?;
    let dependant = crate::ready::scope_of("acme")?.enter("use-db")?;
    let served = lgwks_bot::block_on(serve_and_depend(
        &readiness,
        &dependant,
        &announcing_then_dying(),
    ))?;

    assert_eq!(
        served.exit_code,
        Some(4),
        "the child announced and then exited with its own code"
    );
    assert_eq!(
        served.dependant.as_deref(),
        Some(ANNOUNCEMENT),
        "the dependant was released before the child died"
    );
    assert_eq!(
        served.released_at, None,
        "the readiness no longer reports a release: the failure replaced it"
    );
    let after = served
        .failure
        .ok_or("the child's exit must be raised as a failure at the released generation")?;
    assert_eq!(
        after.generation(),
        Generation::FIRST,
        "the recorded failure names the released generation"
    );
    assert_eq!(
        after.reason(),
        "the child exited 4",
        "and carries the child's own exit as its reason"
    );
    assert!(
        served.dependant_stopped,
        "the failure must reach the dependant that was still running: its \
         token was cancelled by the failure"
    );
    assert_eq!(
        readiness.dependants_live(),
        0,
        "the failure took every token it reached, so a second failure cancels \
         nobody twice"
    );
    Ok(())
}

/// What the announcement-then-exit child and its dependant observed.
#[derive(Debug)]
struct Died {
    /// The child's own exit code.
    exit_code: Option<i32>,
    /// What the dependant was released with, before the child died.
    dependant: Option<String>,
    /// The generation the gate was armed at.
    released_at: Option<Generation>,
    /// The failure raised after the child ended, if any.
    failure: Option<lgwks_bot::script::FailAfterReady>,
    /// Whether the dependant's token was cancelled by that failure.
    dependant_stopped: bool,
}

/// Run the announcement-then-exit child while a dependant waits on it.
///
/// The two halves are concurrent: the dependant is parked on its own token
/// the moment it is released, so it is *still running* when the child exits.
/// That is the state the row is about, and driving the child to completion
/// first would make it trivially finished instead.
///
/// The failure is raised after the supervisor's run returns — the child's
/// exit is a report, and a service body turns it into a readiness fact — and
/// the answer to "did the dependant hear it" is read from the dependant's own
/// token, which is the only channel a readiness owns for a dependant that has
/// already been released.
async fn serve_and_depend(
    readiness: &Named,
    dependant: &Scope,
    spec: &ProcessSpec,
) -> Result<Died, String> {
    let parked = Arc::new(AtomicBool::new(false));
    let waiter = {
        let gate = readiness.clone();
        let parked = Arc::clone(&parked);
        async move {
            let released = gate
                .wait(dependant, RELEASE_BUDGET)
                .await
                .map(|ready| ready.awaited().to_owned())
                .map_err(|error| error.to_string());
            // Parked on its own token, so it is still running when the child
            // exits below.
            parked.store(true, Ordering::SeqCst);
            dependant.token().cancelled().await;
            released
        }
    };
    // The three halves are concurrent, and the order matters: the dependant
    // is released and parks, the child then exits, and only then does the
    // service body turn that exit into a readiness fact. Driving them in
    // sequence would either park a dependant that no failure has yet reached
    // — a deadlock the test would time out on rather than explain — or make
    // the dependant trivially finished before the child died, which is not
    // the state the row is about.
    let supervise = async move {
        let run = gate_on_child_output(spec, readiness).await?;
        // A child reaped without an exit status has not reported one, and
        // `-1` is a real exit code a shell can produce.
        let code = match run.exit_code() {
            Some(code) => code.to_string(),
            None => String::from("without reporting one"),
        };
        readiness
            .fail(Generation::FIRST, &format!("the child exited {code}"))
            .map_err(|error| format!("the child's exit could not be raised: {error}"))?;
        Ok::<_, String>(run)
    };
    let (run, released) = lgwks_bot::join!(supervise, waiter);
    let run = run?;
    if let Err(error) = released.as_ref() {
        let refusal = Err(format!(
            "the dependant must have been released before the child died: {error}"
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "serve_and_depend: returning an error to the caller");
        return refusal;
    }
    assert!(
        parked.load(Ordering::SeqCst),
        "the dependant parked on its own token before the child's exit"
    );
    Ok(Died {
        exit_code: run.exit_code(),
        dependant: released.ok(),
        released_at: readiness.released_at(),
        failure: readiness.failed_at(),
        dependant_stopped: dependant.is_cancelled(),
    })
}
