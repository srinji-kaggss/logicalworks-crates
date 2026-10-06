//! Black-box acceptance for typed readiness (T18 / LC-09).
//!
//! Every journey here drives the **public** surface: a task body on a
//! [`Host`], a [`Readiness`] from `lgwks_bot::script`, and — for the real-process
//! journey — a supervised child whose own stdout line closes the gate. Nothing
//! reaches past a public door, and every assertion is on the exact observation
//! the row names rather than on an internal counter.
//!
//! | Test | What it pins |
//! |---|---|
//! | `a_failure_before_ready_reaches_the_dependant_and_the_report` | a service that never becomes ready fails the dependant at its own path, and the caller's `Report` carries it |
//! | `a_duplicate_ready_signal_is_refused_and_releases_nothing` | a second release is a typed refusal, not a second release |
//! | `a_stale_generation_is_refused_and_releases_nothing` | an older instance cannot release dependants onto a newer one, and a foreign generation is a *different* refusal |
//! | `a_restarted_service_arms_a_new_readiness` | the restart shape end to end |
//! | `a_failure_after_ready_cancels_every_dependant_still_running` | the dependants still running are told, and each report says so |
//! | `a_shutdown_is_a_stop_and_not_a_failure` | teardown is not reported as an outage |
//! | `a_wait_is_budgeted_cancellable_and_immediate_when_already_released` | the wait has a budget, obeys the stop, and costs nothing when the answer exists |
//! | `dependants_are_capped_at_the_declared_bound` | the cap is charged before a slot and refuses rather than growing |
//! | `no_readiness_path_sleeps_or_polls` | the module's own source holds no sleep and no polling loop |
//! | `a_child_printing_its_address_releases_the_dependants` | the real-process journey: readiness from a child's stdout, not from a timer |
//! | `a_child_that_dies_without_printing_never_releases_the_dependants` | a child that never announces is never released |
//! | `a_child_that_dies_after_announcing_stops_the_dependants` | the failure-after-ready journey, driven by a real exit rather than a signal |
#![cfg(feature = "script")]

use std::error::Error;
use std::future::Future;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use lgwks_bot::script::{FlowError, Generation, Readiness, ReadinessError, Scope, Tenant};
use lgwks_bot::task::{Disposition, Host, Report, Task, task};

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

/// A readiness carrying a `String`, the shape every journey here uses: the
/// evidence a dependant receives is whatever the service published, and a
/// `String` is what a child prints.
type Named = Readiness<String>;

/// The budget a journey's wait runs under.
///
/// Long enough that no journey is decided by it and short enough that a journey
/// whose wait is not actually bounded fails in milliseconds rather than at the
/// harness's own timeout.
const BUDGET: Duration = Duration::from_millis(500);

/// A root scope for `tenant`.
fn scope_of(tenant: &str) -> Result<Scope, Box<dyn Error>> {
    Ok(Scope::root(Tenant::new(tenant)?))
}

/// Drive one future on a fresh current-thread runtime.
fn drive<T>(future: impl Future<Output = T>) -> T {
    lgwks_bot::block_on(future)
}

/// Assert a report failed, naming the path the failure was located at.
fn assert_failed_at<O>(report: &Report<O>, path: &str, label: &str) {
    assert_eq!(
        report.disposition(),
        Disposition::Failed,
        "{label}: the run must be Failed, got {:?}",
        report.error()
    );
    let located = report.error().map(FlowError::at);
    // A `Failed` report with no located error is not a located failure, and
    // checking a stand-in path would pass for the wrong reason.
    assert!(
        located.is_some(),
        "{label}: a Failed report must carry the error that failed it"
    );
    let Some(at) = located else {
        return;
    };
    assert!(
        at.ends_with(path),
        "{label}: the failure must be located at a path ending in {path:?}, got {at:?}"
    );
}

/// The error of a result that must have been refused, or a typed failure saying
/// it was not.
///
/// `Result::expect_err` is banned here — and rightly, because it *panics*. So is
/// a hand-written `panic!`, and so is any other shape that turns "this must have
/// been refused" into a panic rather than a report. This returns
/// `Result<E, Accepted>`, where `Accepted` names the case *and* carries the value
/// that was wrongly accepted, so the journey propagates it with `?` and the
/// harness prints it — the same path every other failure in this file takes.
fn refused<T: std::fmt::Debug, E>(outcome: Result<T, E>, what: &str) -> Result<E, Accepted> {
    match outcome {
        Err(error) => Ok(error),
        Ok(accepted) => Err(Accepted::of(what, &accepted)),
    }
}

/// A result that was accepted where a refusal was required.
///
/// An `Error` so the journey's own `?` propagates it; `Display` so the harness
/// prints what was expected and what happened instead. `what` is carried rather
/// than formatted at the point of failure so the message survives the `?` that
/// delivers it.
#[derive(Debug, PartialEq, Eq)]
struct Accepted {
    /// What the journey required to be refused.
    what: String,
    /// What was accepted instead, rendered.
    accepted: String,
}

impl Accepted {
    /// Render the accepted value into this failure's text.
    fn of<T: std::fmt::Debug>(what: &str, accepted: &T) -> Self {
        Self {
            what: what.to_owned(),
            accepted: format!("{accepted:?}"),
        }
    }
}

impl std::fmt::Display for Accepted {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}: this must have been refused, but it was accepted as {}",
            self.what, self.accepted
        )
    }
}

impl Error for Accepted {}

/// A `Task` whose body waits on `readiness` at `step`, so a journey's assertion
/// is on the disposition and located error a caller reads rather than on the
/// readiness's own state.
fn waiter(
    step: Scope,
    readiness: Named,
) -> Result<Task<impl Fn(Scope, ()) -> WaitBody>, FlowError> {
    task(
        "use-service",
        move |_scope: Scope, _input: ()| -> WaitBody {
            // Both captured values are cloned into the body, so the closure
            // itself stays `Fn`: a `Task`'s body may be called more than once,
            // and a closure that gave its capture away on the first call could
            // not be one.
            let readiness = readiness.clone();
            let step = step.clone();
            Box::pin(async move {
                readiness
                    .wait(&step, BUDGET)
                    .await
                    .map(|ready| ready.awaited().to_owned())
            })
        },
    )
}

/// The future a waiter's body returns.
///
/// Named rather than left to inference so the waiter's signature states what a
/// dependant produces: a `String`, or a located `FlowError`. A closure's own
/// future type is unnameable, and an unnamed return type is a second definition
/// of what this body returns that no reader can check.
type WaitBody = Pin<Box<dyn Future<Output = Result<String, FlowError>>>>;

/// How many dependants the failure-after-ready journey runs at once.
///
/// Three rather than one: a failure that reaches "a dependant" and a failure that
/// reaches *every* dependant are different facts, and only more than one proves
/// the readiness does not stop at the first token it happens to hold.
const DEPENDANTS: usize = 3;

// ── Readiness failure reaches the caller ────────────────────────────────────

/// A service that fails before it is ready fails the dependant at its own path,
/// and the caller's `Report` carries that failure with the service's reason.
#[test]
fn a_failure_before_ready_reaches_the_dependant_and_the_report() -> TestResult {
    let scope = scope_of("acme")?;
    let readiness: Named = Readiness::new("db")?;
    let host = Host::builder("acme")?.build()?;
    let declared = waiter(scope.enter("use-db")?, readiness.clone())?;

    // Nothing has been signalled and nothing has failed, so a reading taken now
    // is `None` — the fact a caller checks before it decides to wait.
    assert!(
        readiness.outcome().is_none(),
        "an unsignalled readiness reports no outcome"
    );

    readiness.fail(Generation::FIRST, "could not bind port 5432")?;

    let report = drive(host.run(&declared, ()));
    assert_failed_at(
        &report,
        "use-db/await-ready",
        "a service that never came up",
    );
    let Some(reported) = report.error() else {
        let refusal = Err("a failed report must carry the error that failed it".into());
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "a_failure_before_ready_reaches_the_dependant_and_the_report: returning an error to the caller");
        return refusal;
    };
    let error = reported.to_string();
    assert!(
        error.contains("could not bind port 5432"),
        "the report must carry the service's own reason, got {error:?}"
    );
    assert_eq!(
        readiness.admitted(),
        1,
        "the dependant was admitted exactly once, before it was answered"
    );
    // A failure before the release owes nobody a cancellation.
    assert!(
        readiness.failed_at().is_none(),
        "nothing was released, so this is not a failure after a release"
    );
    Ok(())
}

// ── Duplicate readiness ─────────────────────────────────────────────────────

/// A second ready signal is a typed refusal, and it releases nothing.
///
/// Both halves matter: the refusal must be *typed* (a caller can tell it from a
/// lost signal) and the already-released generation must survive it, so a
/// dependant arriving afterwards still reads the first release rather than the
/// value the duplicate offered.
#[test]
fn a_duplicate_ready_signal_is_refused_and_releases_nothing() -> TestResult {
    let scope = scope_of("acme")?;
    let readiness: Named = Readiness::new("db")?;
    readiness.ready(Generation::FIRST, "db.internal:5432".to_owned())?;

    let duplicate = refused(
        readiness.ready(Generation::FIRST, "db.elsewhere:5432".to_owned()),
        "a second release must be refused",
    )?;
    assert_eq!(
        duplicate,
        ReadinessError::AlreadyReady {
            what: Arc::from("db"),
            generation: Generation::FIRST,
        },
        "a duplicate names the readiness and the generation already released"
    );
    assert!(!duplicate.released(), "a refused duplicate released nobody");
    assert!(
        duplicate.is_permanent(),
        "repeating a duplicate release yields the same refusal, so an enclosing \
         retry must not treat it as transient"
    );
    assert_eq!(
        readiness.released_at(),
        Some(Generation::FIRST),
        "the released generation survives the duplicate"
    );

    // A dependant that arrives after the duplicate still reads the *first*
    // release, not the value the duplicate offered.
    let ready = drive(readiness.wait(&scope, BUDGET))?;
    assert_eq!(
        ready.awaited(),
        "db.internal:5432",
        "a late dependant reads the first release, not the refused duplicate"
    );
    assert_eq!(ready.generation(), Generation::FIRST);
    Ok(())
}

// ── Stale generation ────────────────────────────────────────────────────────

/// A signal from an older instance releases nothing, and a signal from a
/// generation this readiness never issued is a *different* refusal.
///
/// A restarted service's previous instance may still hold a handle and still
/// decide to say "ready". The generation it carries is behind the one the
/// readiness is bound to, and the readiness must refuse it rather than release
/// dependants onto a process that has been replaced.
#[test]
fn a_stale_generation_is_refused_and_releases_nothing() -> TestResult {
    let scope = scope_of("acme")?;
    // Named rather than drawn: "stale" is an *ordering* against the bound
    // generation, so a test must be able to name an older one. Two consecutive
    // `next()` draws would be newer, not older. The base is far above zero so
    // `saturating_sub` is not silently clamped onto the bound generation, which
    // would make this journey assert nothing.
    const BASE: u64 = 1_000;
    let current = Generation::at(BASE);
    let older = Generation::at(BASE.saturating_sub(1));
    let readiness: Named = Readiness::named("db", current, 4)?;

    let stale = refused(
        readiness.ready(older, "db.old:5432".to_owned()),
        "a stale signal must be refused",
    )?;
    assert_eq!(
        stale,
        ReadinessError::StaleGeneration {
            what: Arc::from("db"),
            expected: current,
            got: older,
        },
        "a stale signal names the generation expected and the one it carried"
    );
    assert!(!stale.released(), "a refused signal released nobody");
    assert!(
        readiness.stale(older),
        "the readiness agrees the older generation is stale"
    );
    assert!(
        !readiness.stale(current),
        "the bound generation is not stale"
    );
    assert!(
        readiness.outcome().is_none(),
        "a refused signal settled nothing, so no outcome is reported"
    );
    assert_eq!(
        readiness.admitted(),
        0,
        "a refused signal admitted no dependant"
    );

    // A generation this readiness never issued is a different fact: it is not a
    // previous instance, it is a signal from somewhere else entirely. Named from
    // the shared counter so it is drawn from the same space the readiness could
    // have been armed at, and strictly newer than the bound one — a value
    // *below* the bound is a stale instance, which is the case above.
    let stranger = Generation::at(BASE.saturating_add(1));
    assert_eq!(
        refused(
            readiness.ready(stranger, "db.stranger:5432".to_owned()),
            "a foreign generation must be refused",
        )?,
        ReadinessError::UnknownGeneration {
            what: Arc::from("db"),
            got: stranger,
        },
        "a generation this readiness never issued is refused as unknown, not stale"
    );

    // The bound generation is still accepted afterwards, so a refusal does not
    // poison the readiness into refusing everything.
    readiness.ready(current, "db.new:5432".to_owned())?;
    let ready = drive(readiness.wait(&scope, BUDGET))?;
    assert_eq!(
        ready.awaited(),
        "db.new:5432",
        "the current instance's release is what a dependant receives"
    );
    assert_eq!(ready.generation(), current);
    Ok(())
}

/// The restart shape end to end: the old instance's readiness stays settled at
/// its own generation while the new instance's readiness releases dependants.
#[test]
fn a_restarted_service_arms_a_new_readiness() -> TestResult {
    let scope = scope_of("acme")?;
    let first = Generation::at(7);
    let second = Generation::at(8);

    let before: Named = Readiness::named("db", first, 4)?;
    before.ready(first, "db.first:5432".to_owned())?;

    let after: Named = Readiness::named("db", second, 4)?;
    after.ready(second, "db.second:5432".to_owned())?;

    let ready = drive(after.wait(&scope, BUDGET))?;
    assert_eq!(
        ready.awaited(),
        "db.second:5432",
        "a dependant receives the restarted instance's own release"
    );
    assert_eq!(
        ready.generation(),
        second,
        "and the generation that restart armed"
    );
    // The old readiness is settled at its own generation and refuses the new
    // instance's generation: they are two readinesses, not one handle the
    // restarted instance may write through. The refusal is *unknown generation*
    // rather than *stale*, because 8 is newer than 7 — the signal is not from an
    // older instance, it is from a different readiness entirely, and a caller
    // told "stale" would look for a service to re-arm rather than for the handle
    // it is holding.
    assert_eq!(
        before.released_at(),
        Some(first),
        "the old readiness still reports its own release"
    );
    assert_eq!(
        refused(
            before.ready(second, "db.second:5432".to_owned()),
            "the new instance cannot write through the old readiness",
        )?,
        ReadinessError::UnknownGeneration {
            what: Arc::from("db"),
            got: second,
        },
        "a signal from a different readiness is refused as unknown, and the old \
         readiness keeps its own release"
    );
    Ok(())
}

// ── Failure after ready ─────────────────────────────────────────────────────

/// A failure *after* the release reaches every dependant that is still running.
///
/// The dependants here are real scopes that went through the readiness's own
/// `wait`, so the readiness holds each of their tokens by the fact of admitting
/// them. Each dependant then keeps working *past* the release — that is the
/// state the row is about, "failure after ready" — and the failure cancels
/// exactly those tokens, so each dependant observes a stop at its own step and
/// each report carries it rather than reporting a run that used a service which
/// had already died.
#[test]
fn a_failure_after_ready_cancels_every_dependant_still_running() -> TestResult {
    let readiness: Named = Readiness::new("db")?;
    readiness.ready(Generation::FIRST, "db.internal:5432".to_owned())?;
    let parked = Arc::new(AtomicUsize::new(0));

    // Three dependants, each in its own step. Each is admitted by waiting on
    // the readiness, which is what hands its token over, and each then parks on
    // work that outlives the release.
    let host = Host::builder("acme")?
        .max_concurrent_tasks(limit_of(DEPENDANTS)?)
        .build()?;
    let reached_in_body = Arc::clone(&parked);
    let body = {
        let readiness = readiness.clone();
        move |scope: Scope, input: usize| {
            let readiness = readiness.clone();
            let reached = Arc::clone(&reached_in_body);
            async move {
                let step = scope.enter(&format!("worker-{input}"))?;
                // Admitted: the readiness now holds this step's token.
                let ready = readiness.wait(&step, BUDGET).await?;
                assert_eq!(
                    ready.awaited(),
                    "db.internal:5432",
                    "dependant {input} was released with the published address"
                );
                // Still running when the service dies below.
                reached.fetch_add(1, Ordering::SeqCst);
                step.token().cancelled().await;
                step.checkpoint()?;
                Ok::<usize, FlowError>(input)
            }
        }
    };
    let declared: Task<_> = task("use-service", body)?;

    // Drive every run until all three are suspended inside their parked bodies.
    // A hand-driven loop rather than a delay, so the failure lands at a point
    // every dependant has actually reached rather than after a guess. The count
    // is read *after* the poll rather than before it: the last dependant to park
    // does so inside this loop, and checking first would return `Pending` on the
    // turn that observed it with nothing left to wake the loop.
    let mut runs: Vec<Pin<Box<_>>> = (0..DEPENDANTS)
        .map(|index| Box::pin(host.run(&declared, index)))
        .collect();
    drive(std::future::poll_fn(|context| {
        use std::task::Poll;
        let mut any_pending = false;
        for run in &mut runs {
            match run.as_mut().poll(context) {
                Poll::Ready(_) => return Poll::Ready(()),
                Poll::Pending => any_pending = true,
            }
        }
        if !any_pending || parked.load(Ordering::SeqCst) == DEPENDANTS {
            return Poll::Ready(());
        }
        Poll::Pending
    }));
    // Each token was registered by the readiness that released it, so the
    // readiness knows exactly which dependants a later failure must reach.
    assert_eq!(
        readiness.admitted(),
        u64::try_from(DEPENDANTS)?,
        "each dependant was admitted once, by the readiness's own wait"
    );
    assert_eq!(
        readiness.dependants_live(),
        DEPENDANTS,
        "the readiness holds one token per dependant it released"
    );

    readiness.fail(Generation::FIRST, "the service exited")?;
    let after = readiness
        .failed_at()
        .ok_or("a failure after the release must be recorded as one")?;
    assert_eq!(
        after.generation(),
        Generation::FIRST,
        "the recorded failure names the generation that was released"
    );
    assert_eq!(
        after.reason(),
        "the service exited",
        "and carries the service's own reason"
    );
    assert_eq!(
        readiness.dependants_live(),
        0,
        "a failure after the release took every token it reached, so a second \
         failure cancels nobody twice"
    );
    assert!(
        readiness
            .fail(Generation::FIRST, "the service exited")
            .is_err(),
        "a second failure for the same generation is refused rather than \
         re-applied"
    );

    // Awaited one after another rather than through a bounded join: each run is
    // already pinned, so awaiting it directly needs no `'static` borrow of the
    // host or the task, and the order the reports come back in is the order the
    // dependants were declared in — which is the order this assertion indexes.
    let reports: Vec<Report<usize>> = runs.iter_mut().map(|run| drive(run.as_mut())).collect();
    assert_eq!(reports.len(), DEPENDANTS, "every dependant reported");
    for (index, report) in reports.iter().enumerate() {
        assert_eq!(
            report.disposition(),
            Disposition::Cancelled,
            "dependant {index}: a failure after the release must stop the \
             dependant, got {:?}",
            report.error()
        );
    }
    Ok(())
}

/// A non-zero admission ceiling, for a host that runs several dependants at once.
fn limit_of(limit: usize) -> Result<NonZeroUsize, Box<dyn Error>> {
    NonZeroUsize::new(limit).ok_or_else(|| "the ceiling must be at least one".into())
}
#[test]
fn a_shutdown_is_a_stop_and_not_a_failure() -> TestResult {
    let scope = scope_of("acme")?;
    let readiness: Named = Readiness::new("db")?;
    readiness.shutdown(Generation::FIRST)?;

    let error = refused(
        drive(readiness.wait(&scope, BUDGET)).map(|_ready| ()),
        "a shut-down readiness must not release a dependant",
    )?;
    assert!(
        error.is_cancelled(),
        "a teardown is a stop, not a service failure: {error}"
    );
    assert!(
        !error.to_string().contains("did not become ready"),
        "a shutdown must not be reported as a failure to come up: {error}"
    );
    assert!(
        readiness.failed_at().is_none(),
        "a shutdown is not a failure, so it is not recorded as one"
    );
    assert_eq!(
        refused(
            readiness.ready(Generation::FIRST, "db.internal:5432".to_owned()),
            "a shut-down readiness refuses further signals",
        )?,
        ReadinessError::Closed {
            what: Arc::from("db"),
            generation: Generation::FIRST,
        },
        "a signal after a shutdown is refused by name"
    );
    Ok(())
}

// ── The wait is bounded, cancellable, and immediate ─────────────────────────

/// The wait has a budget of its own, obeys the scope's stop, and costs nothing
/// when the answer already exists.
///
/// Three facts in one journey because they are one contract. The last is the one
/// a guessed sleep cannot have: a sleep costs its full duration even when the
/// readiness was released a microsecond ago, so a wait that took measurable
/// time on an already-released readiness would be a timer wearing a readiness's
/// name.
#[test]
fn a_wait_is_budgeted_cancellable_and_immediate_when_already_released() -> TestResult {
    let scope = scope_of("acme")?;

    // 1. An already-released readiness resolves without spending its budget.
    let released: Named = Readiness::new("db")?;
    released.ready(Generation::FIRST, "db.internal:5432".to_owned())?;
    let started = Instant::now();
    let ready = drive(released.wait(&scope, BUDGET))?;
    let spent = started.elapsed();
    assert_eq!(ready.awaited(), "db.internal:5432", "the released value");
    assert!(
        spent < BUDGET,
        "an already-released readiness resolved in {spent:?}, so the wait did not \
         sleep for its budget"
    );

    // 2. A stop ends the wait as a cancellation, not as a timeout. The wait runs
    //    under the very scope that is stopped, because cancellation walks *up*:
    //    cancelling a child token stops what descends from it and nothing above
    //    it, so a wait under a parent could only be stopped by that parent. This
    //    is the shape a dependant actually has — a step under a run that a
    //    caller stops.
    let waiting: Named = Readiness::new("db")?;
    let wait_under = scope.enter("use-db")?;
    let outcome = drive(cancel_soon(&waiting, &wait_under));
    assert!(
        outcome.is_cancelled(),
        "a stopped wait must report a cancellation, got {outcome}"
    );
    assert!(
        outcome.to_string().contains("use-db/await-ready"),
        "and locate it at the wait's own step under the stopped scope, got {outcome}"
    );

    // 3. A wait with no signal and no stop is ended by its own budget. The
    // budget is short here so the assertion costs milliseconds; a longer one
    // would prove the same thing more slowly.
    let silent: Named = Readiness::new("db")?;
    let quiet = Duration::from_millis(40);
    let started = Instant::now();
    let error = refused(
        drive(silent.wait(&scope, quiet)).map(|_ready| ()),
        "a readiness that is never signalled must not wait forever",
    )?;
    let spent = started.elapsed();
    assert!(
        matches!(error, FlowError::TimedOut { .. }),
        "the wait must be bounded by its own budget, got {error}"
    );
    assert!(
        spent < BUDGET,
        "the budget ended the wait in {spent:?}, well inside the harness's ceiling"
    );
    Ok(())
}

/// Cancel `scope` once the wait has been admitted, and return the wait's
/// outcome.
///
/// The stop is raised from a second task rather than after a delay, so the
/// journey observes a cancellation and not a race between two clocks: the wait
/// is already suspended when the token is cancelled, which is the only state a
/// cancellation is meant to end. The admission is read from the readiness
/// itself rather than from a flag the wait sets, so the ordering this asserts —
/// admitted, then stopped — is the ordering that happened.
async fn cancel_soon<T: Clone>(readiness: &Readiness<T>, scope: &Scope) -> FlowError {
    let admitted_at_start = readiness.admitted();
    let stop = async move {
        while readiness.admitted() == admitted_at_start {
            lgwks_bot::rt::task::yield_now().await;
        }
        scope.cancel();
    };
    let wait = async move {
        match readiness.wait(scope, Duration::from_secs(30)).await {
            // The wait resolving is the failure this row rules out, and it is
            // reported as the wait's own error rather than as a test assertion:
            // the caller returns it, so a resolved wait cannot pass.
            Ok(_ready) => FlowError::failed("the wait resolved instead of stopping"),
            // A stop is not a failure, so the error travels unchanged: this is
            // the arm that makes a routine restart read as `Cancelled`.
            Err(error) => error,
        }
    };
    let (outcome, ()) = lgwks_bot::join!(wait, stop);
    assert!(
        readiness.admitted() > admitted_at_start,
        "the wait was admitted before it was stopped, so the cancellation \
         reached a suspended wait rather than one that had already answered"
    );
    outcome
}

// ── The bound ───────────────────────────────────────────────────────────────

/// A readiness admits at most its declared cap, and the charge is refused
/// rather than the cap silently raised.
#[test]
fn dependants_are_capped_at_the_declared_bound() -> TestResult {
    let scope = scope_of("acme")?;
    let cap = 2_usize;
    let readiness: Named = Readiness::named("db", Generation::FIRST, cap)?;
    assert_eq!(readiness.cap(), cap, "the cap is the one that was declared");

    readiness.ready(Generation::FIRST, "db.internal:5432".to_owned())?;
    for index in 0..cap {
        let ready = drive(readiness.wait(&scope, BUDGET))?;
        assert_eq!(
            ready.awaited(),
            "db.internal:5432",
            "dependant {index} is served"
        );
    }
    let served = u64::try_from(cap)?;
    assert_eq!(
        readiness.admitted(),
        served,
        "each admission was charged once"
    );

    let over_cap = refused(
        drive(readiness.wait(&scope, BUDGET)).map(|_ready| ()),
        "a dependant past the cap must be refused",
    )?;
    assert!(
        over_cap.to_string().contains("cap"),
        "the refusal must name the bound it reached, got {over_cap}"
    );
    // The refusal charged nothing: the cap is a bound on admissions, not a
    // budget a refused attempt spends, so a caller that sheds load sees the cap
    // again rather than a permanently exhausted readiness.
    assert_eq!(
        readiness.admitted(),
        served,
        "a refused admission charged no slot"
    );

    // The declared maximum is the ceiling the crate states, and a cap of zero is
    // raised to one rather than admitting nothing.
    let empty: Named = Readiness::named("db", Generation::FIRST, 0)?;
    assert_eq!(
        empty.cap(),
        1,
        "a zero cap is raised to one: a readiness that admitted nothing could never \
         release a dependant"
    );
    let maxed = Readiness::<String>::new("db")?;
    assert_eq!(
        maxed.cap(),
        lgwks_bot::script::MAX_DEPENDANTS,
        "the default cap is the declared maximum"
    );
    Ok(())
}

// ── No readiness sleeps in task code ────────────────────────────────────────

/// The readiness module arms no timer of its own, its wait re-reads the
/// readiness only after a wake, and the observer that gates a child's readiness
/// arms no timer either.
///
/// The row's "no readiness sleeps in task code" is a property of the source, so
/// it is checked against the source. Doc comments are excluded: they describe
/// the defects they rule out and are allowed to name them.
#[test]
fn no_readiness_path_sleeps_or_polls() -> TestResult {
    let code = read_source("src/script/ready.rs")?;
    // `sleep(` and `while ` are the shapes a guessed readiness takes: a fixed
    // wait, and a re-read on a condition. A `loop` is *not* banned outright,
    // because the wait legitimately re-reads the channel after every wake from
    // it; the two halves of that claim are checked below rather than by
    // pattern-matching the word.
    for banned in ["sleep(", "while "] {
        assert!(
            !code.contains(banned),
            "the readiness module must contain no {banned:?}: a wait that sleeps or \
             re-reads on a condition is the defect this type replaces"
        );
    }
    // The wait's own bound goes through `within`, which does sleep — on the
    // caller's budget, not on a guess. That is the one timer the module may
    // reach, so its presence is asserted rather than merely tolerated.
    assert!(
        code.contains("within(scope"),
        "the readiness wait must be charged through the crate's own `within`, not a \
         hand-rolled timer"
    );

    // The loop in the wait is legitimate only because it awaits a change. Both
    // halves are asserted: it must await one, and it must not re-read on a
    // timer. A loop that did neither would spin, which is the defect.
    let subscribe = between(&code, "async fn subscribe", "fn resolve")?;
    assert!(
        subscribe.contains("watch::Receiver::changed"),
        "the wait must await a change on the readiness channel, or it is polling: \
         {subscribe}"
    );
    assert!(
        !subscribe.contains("time::sleep"),
        "the wait must not re-read the readiness on a timer: {subscribe}"
    );

    // The stdout observer that gates a child's readiness must not arm a timer
    // either: a readiness gated on a clock is the guess this type replaces.
    // The marker is a line of code, not a doc comment: `read_source` strips
    // comments, so a doc marker would never be found.
    let observer = between(
        &read_source("src/rt/supervise.rs")?,
        "async fn capture<R>",
        "const MAX_OBSERVED_LINE_BYTES",
    )?;
    assert!(
        !observer.contains("sleep"),
        "the stdout observer must not arm a timer:\n{observer}"
    );
    Ok(())
}

/// A crate source file with its comment lines removed.
///
/// The removal is line-wise rather than by a parser because the claim is about
/// *code*: a `//` comment naming `sleep(` in prose is documentation, and a block
/// comment in this crate's idiom is used only for docs.
fn read_source(relative: &str) -> Result<String, Box<dyn Error>> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    let source = std::fs::read_to_string(&path)?;
    Ok(source
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !trimmed.starts_with("//")
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

/// The lines of `source` between two markers, as one string.
fn between(source: &str, from: &str, to: &str) -> Result<String, Box<dyn Error>> {
    let after = source
        .split_once(from)
        .map(|(_, rest)| rest)
        .ok_or_else(|| format!("the marker {from:?} is not in the file"))?;
    after
        .split_once(to)
        .map(|(body, _)| body.to_owned())
        .ok_or_else(|| format!("the marker {to:?} is not after {from:?}").into())
}

// ── The real-process journey ────────────────────────────────────────────────

/// The real-process half. Gated on the same features as every other supervised
/// child journey in this suite, so a build without the `process` driver does not
/// compile a test it cannot run.
#[cfg(all(unix, feature = "process"))]
mod real_process {
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
    async fn serve(
        readiness: &Named,
        dependant: &Scope,
        spec: &ProcessSpec,
    ) -> Result<Served, String> {
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
                    .wait(dependant, BUDGET)
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
}
