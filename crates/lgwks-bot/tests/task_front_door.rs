//! The task front door, driven through its public interface.
//!
//! | Test | Row | What it pins |
//! |---|---|---|
//! | `one_declaration_one_call_returns_a_typed_value` | T01 | a task plus one call returns a typed value, with no observer, tick loop or handle to reap |
//! | `a_body_holding_non_send_state_and_a_borrowed_input_runs` | T02 | a body holding `Rc<RefCell<_>>` over a borrowed `&[u8]` compiles and runs |
//! | `dropping_a_suspended_run_releases_its_permit` | T03 | a dropped run returns its permit and leaves no borrowed body running, proven by an outside watchdog |
//! | `nested_runs_complete_at_a_ceiling_of_one` | T04 | four levels of nesting at an admission ceiling of one completes, `each` at most one included |
//! | `a_thousand_concurrent_runs_stay_under_the_ceiling` | — | 1,000 concurrent runs: never more than the ceiling in flight, all complete, no permit leaked |
//! | `two_tenants_never_share_a_step_key` | — | two hosts, one task name, one input: distinct `StepKey`s |
//! | `an_overflowing_trail_counts_what_it_dropped` | T36 | overflow loses only the declared detail, counts it, and leaves the terminal state intact |

//! # Why these bodies look the way they do
//!
//! `Task<F>` stores its body as its own type and `Host::run` calls it as
//! `F: Fn(Scope, I) -> Fut` for one concrete `Fut`. That is what keeps the
//! author's side free of `Box`, `Pin`, `Arc` and `'static`, and it has one
//! visible consequence a test must respect: **the returned future cannot borrow
//! from the body's environment**, because then its type would depend on the
//! borrow. A body therefore takes its input by value or borrow, copies whatever
//! it needs out of a borrow *synchronously*, and returns a future that owns its
//! data. Captured state is shared by `Rc` (non-`Send`, borrowed, local mode) or
//! moved in, never by reference.

#![cfg(feature = "script")]

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::future::Future;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::Poll;
use std::time::{Duration, Instant};

use lgwks_bot::script::{FlowError, Scope, Tenant, each, within};
use lgwks_bot::task::{Disposition, EffectKnowledge, Host, HostError, Report, Task, task};

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

/// A host for `tenant` with the machine's default ceilings.
fn host(tenant: &str) -> Result<Host, Box<dyn Error>> {
    Ok(Host::builder(tenant)?.build()?)
}

/// A host for `tenant` admitting at most `tasks` runs at once.
fn bounded_host(tenant: &str, tasks: usize) -> Result<Host, Box<dyn Error>> {
    let limit = NonZeroUsize::new(tasks).ok_or("the ceiling must be at least one")?;
    Ok(Host::builder(tenant)?.max_concurrent_tasks(limit).build()?)
}

/// Drive one future to completion on a fresh current-thread runtime.
fn drive<T>(future: impl Future<Output = T>) -> T {
    lgwks_bot::block_on(future)
}

/// Poll `future` once and report whether it is still pending.
///
/// Used to park a run inside its body without awaiting it to completion. One
/// helper rather than the same `poll_fn` closure written at four call sites,
/// because a body that must park is the shape of three of these rows.
fn pending_once<F: Future>(mut future: Pin<&mut F>) -> bool {
    drive(std::future::poll_fn(|context| {
        Poll::Ready(future.as_mut().poll(context).is_pending())
    }))
}

/// A task behind a reference count, so a body can call one task from another.
///
/// `Task<F>` is not `Clone` — the body is a value, not a factory — and a body
/// cannot borrow its own environment. An `Rc` is the one way a nested body
/// reaches the task it calls, and it is deliberately *not* `Send`: the whole
/// front door is the local mode.
type Shared<F> = Rc<Task<F>>;

// ── T01 ─────────────────────────────────────────────────────────────────────

/// One task declaration and one call return a typed value.
///
/// No observer is constructed, no tick is driven, nothing is spawned and no
/// handle is dropped: the whole surface between the declaration and the value
/// is `host.run(..).await`.
#[test]
fn one_declaration_one_call_returns_a_typed_value() -> TestResult {
    let host = host("acme")?;
    let review = task("review-pr", |_scope: Scope, files: Vec<u32>| async move {
        // The output type is the body's own, and it reaches the caller as
        // `Report<u32>`: no boxing, no `dyn`, no discarded value.
        Ok(files.iter().copied().sum::<u32>())
    })?;

    let report: Report<u32> = drive(host.run(&review, vec![1, 2, 3, 4]));
    assert!(
        report.elapsed() < Duration::from_secs(5),
        "an immediate body does not take seconds, took {:?}",
        report.elapsed()
    );
    assert_eq!(
        report.disposition(),
        Disposition::Succeeded,
        "a body that returns a value succeeds, with no error: {:?}",
        report.error()
    );
    assert_eq!(
        report.output().copied(),
        Some(10),
        "the typed value reaches the caller unchanged"
    );
    assert_eq!(
        report.into_result()?,
        10,
        "and the same value, consumed, from the same report"
    );
    Ok(())
}

/// A failing body reports `Failed` with its own located error and no output.
#[test]
fn a_failing_body_reports_failed_and_no_output() -> TestResult {
    let host = host("acme")?;
    let refuse = task("refuse", |_scope: Scope, ()| async {
        Err(FlowError::failed("the upstream said no"))
    })?;

    let report: Report<()> = drive(host.run(&refuse, ()));
    assert_eq!(
        report.disposition(),
        Disposition::Failed,
        "a permanent failure is Failed"
    );
    assert_eq!(report.output(), None, "a failed run has no output");
    let error = report.error().ok_or("a failed run names its failure")?;
    assert!(
        error.to_string().contains("refuse"),
        "the failure is located inside the task's own step: {error}"
    );
    assert!(
        report.into_result().is_err(),
        "the located failure surfaces by value"
    );
    Ok(())
}

/// A name that would corrupt a step path is refused at declaration, never at
/// run time.
#[test]
fn a_malformed_name_is_refused() -> TestResult {
    let build = |name: &str| task(name, |_scope: Scope, ()| async { Ok::<(), FlowError>(()) });
    for refused in ["", "has space", "has/slash", "has\nnewline"] {
        let error = build(refused)
            .err()
            .ok_or_else(|| format!("the name {refused:?} must be refused"))?;
        assert!(
            matches!(error, FlowError::InvalidName { .. }),
            "a bad name is a typed refusal, not a string: {error}"
        );
    }
    // `FlowError` has no `PartialEq`, so acceptance is read as the name that
    // came back and refusal by variant: the two facts worth distinguishing.
    let accepted = build("ok-name_1.v2:3")
        .map(|declared| declared.name().to_owned())
        .ok();
    assert_eq!(
        accepted,
        Some("ok-name_1.v2:3".to_owned()),
        "a name of allowed characters is accepted and kept verbatim"
    );
    Ok(())
}

/// A tenant that would corrupt every key and every error location is refused
/// when the host is built, not when it is first used.
#[test]
fn a_malformed_tenant_is_refused() -> TestResult {
    let error = Host::builder("has space")
        .err()
        .ok_or("a tenant with a space must be refused")?;
    assert!(
        matches!(error, FlowError::InvalidTenant { .. }),
        "a bad tenant is a typed refusal: {error}"
    );
    let tenant = Tenant::new("acme")?;
    assert_eq!(
        host("acme")?.tenant(),
        &tenant,
        "the host holds exactly the tenant it validated"
    );
    Ok(())
}

// ── T02 ─────────────────────────────────────────────────────────────────────

/// A body holding non-`Send` state over a borrowed input compiles and runs.
///
/// This test is itself the evidence. The input is a `&[u8]` borrowed from the
/// caller's stack, so nothing is copied into the run and nothing is owned by it;
/// the body holds an `Rc<RefCell<Vec<u32>>>`, so its future is not `Send`. No
/// `Box`, no `Pin`, no `Arc`, no `'static` coercion and no thread appears in the
/// author's code.
///
/// The one thing the body does is read the borrow into an owned `Vec` before
/// the `async` block, because `Host::run` names one concrete future type: a
/// future that borrowed the input would have a different type per call.
#[test]
fn a_body_holding_non_send_state_and_a_borrowed_input_runs() -> TestResult {
    let host = host("acme")?;
    let log: Rc<RefCell<Vec<u32>>> = Rc::new(RefCell::new(Vec::new()));
    let body_log = Rc::clone(&log);

    let tally = task("tally", move |_scope: Scope, bytes: &[u8]| {
        // Read the borrow now; the future below owns what it needs.
        let values: Vec<u32> = bytes.iter().map(|byte| u32::from(*byte)).collect();
        let log = Rc::clone(&body_log);
        async move {
            log.borrow_mut().extend(values.iter().copied());
            Ok(values.iter().copied().sum::<u32>())
        }
    })?;

    let input: Vec<u8> = vec![1, 2, 3];
    let report: Report<u32> = drive(host.run(&tally, input.as_slice()));
    assert_eq!(
        report.disposition(),
        Disposition::Succeeded,
        "a non-Send body over a borrowed input runs: {:?}",
        report.error()
    );
    assert_eq!(report.output().copied(), Some(6), "the bytes were summed");
    assert_eq!(
        *log.borrow(),
        vec![1, 2, 3],
        "the `Rc` the body captured is the caller's own"
    );
    Ok(())
}

// ── T03 ─────────────────────────────────────────────────────────────────────

/// Dropping a suspended run releases its permit and leaves nothing running.
///
/// The watchdog is a separate thread with its own wall clock, because the claim
/// under test is liveness and only an outside observer can say a run failed to
/// yield. It is joined before the test returns, and it fails loudly rather than
/// hanging: a permit that never comes back is a defect no assertion on this
/// thread could observe.
#[test]
fn dropping_a_suspended_run_releases_its_permit() -> TestResult {
    const WATCHDOG: Duration = Duration::from_secs(30);

    let host = bounded_host("acme", 1)?;
    let ended = Rc::new(Cell::new(0_usize));
    let body_ended = Rc::clone(&ended);

    // A body that never finishes, holding a guard whose drop is the evidence
    // that the abandoned body really went away.
    let never = task("suspend", move |_scope: Scope, ()| {
        let guard = BodyGuard {
            observed: Rc::clone(&body_ended),
        };
        async move {
            let _guard = guard;
            std::future::pending::<()>().await;
            Ok(())
        }
    })?;

    // The watchdog: an outside thread with a real deadline. `clippy.toml` bans
    // `std::thread::spawn`, the bare form with no name and no handle contract;
    // `Builder::spawn` names the thread and its handle is joined below, so no
    // suppression is needed or wanted — an unfulfilled `#[expect]` is itself a
    // defect.
    let watching = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&watching);
    let watchdog = std::thread::Builder::new()
        .name("task-permit-watchdog".to_owned())
        .spawn(move || {
            let deadline = Instant::now() + WATCHDOG;
            while Instant::now() < deadline {
                if !flag.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::park_timeout(Duration::from_millis(10));
            }
            // Reaching here means the permit never came back. A deadlock is a
            // failure, not a hang the suite has to time out.
            std::panic::resume_unwind(Box::new(
                "task: an abandoned run never released its admission permit",
            ));
        })?;

    // Poll the run once, so it is admitted, charged, and parked inside its body.
    let mut suspended = Box::pin(host.run(&never, ()));
    assert!(
        pending_once(suspended.as_mut()),
        "a body that parks forever must still be pending"
    );
    assert_eq!(
        host.admission().available_permits(),
        0,
        "the suspended run holds the host's only permit"
    );
    assert_eq!(
        host.admission().in_flight(),
        1,
        "the suspended run is counted as in flight"
    );

    // Abandon it. The permit must come back and the body must be dropped.
    drop(suspended);
    assert_eq!(
        host.admission().available_permits(),
        1,
        "an abandoned run releases its permit"
    );
    assert_eq!(
        host.admission().in_flight(),
        0,
        "an abandoned run stops counting as in flight"
    );
    assert_eq!(
        ended.get(),
        1,
        "the abandoned body was dropped, so nothing it borrowed outlived it"
    );
    assert_eq!(
        host.admission().refused(),
        0,
        "abandoning a run is not a refusal: it was admitted"
    );

    // Tell the watchdog the wait is over, and join it.
    watching.store(false, Ordering::Relaxed);
    watchdog
        .join()
        .map_err(|_| "the watchdog must not panic once the permit is back")?;

    // The freed permit is usable again, which is the point of releasing it.
    let echo = task("echo", |_scope: Scope, ()| async { Ok(()) })?;
    let report: Report<()> = drive(host.run(&echo, ()));
    assert_eq!(
        report.disposition(),
        Disposition::Succeeded,
        "the host still admits work after an abandoned run: {:?}",
        report.error()
    );
    Ok(())
}

/// The guard a parked body owns, so the body's drop is observable.
struct BodyGuard {
    /// Counts the drop.
    observed: Rc<Cell<usize>>,
}

impl Drop for BodyGuard {
    /// Record that the body ended without reaching its end.
    fn drop(&mut self) {
        self.observed.set(self.observed.get().saturating_add(1));
    }
}

// ── T04 ─────────────────────────────────────────────────────────────────────

/// Four levels of nesting complete at an admission ceiling of one.
///
/// A parent awaiting a child never waits for a permit while holding one: a
/// nested run is charged to the permit its own future already holds. A naive
/// permit-per-run deadlocks here on the first child, which is exactly why the
/// ceiling is one. The innermost task also runs an `each` bounded to one, so
/// the fan-out shares the same discipline.
///
/// Each level is a distinct closure type — that is what `Task<F>` is — and each
/// reaches the level below through an `Rc`, because a body cannot borrow its own
/// environment.
#[test]
fn nested_runs_complete_at_a_ceiling_of_one() -> TestResult {
    let host = bounded_host("acme", 1)?;
    let entered = Rc::new(Cell::new(0_usize));

    let leaf: Shared<_> = Rc::new(task("leaf", |scope: Scope, items: Vec<u32>| async move {
        each::<_, u32, _, _>(
            &scope,
            "item",
            NonZeroUsize::new(1),
            items,
            |step, item| async move {
                step.checkpoint()?;
                Ok(item * 2)
            },
        )
        .await
    })?);

    // The level below the leaf turns its vector into a scalar, so every wrapper
    // above works in one type and the chain's value is a single number.
    let inner: Shared<_> = Rc::new({
        let leaf = Rc::clone(&leaf);
        let host = host.clone();
        let log = Rc::clone(&entered);
        task("nest-3", move |_scope: Scope, value: u32| {
            let leaf = Rc::clone(&leaf);
            let host = host.clone();
            let log = Rc::clone(&log);
            async move {
                log.set(log.get().saturating_add(1));
                let report = host.run(&leaf, vec![value]).await;
                Ok(report.into_output().unwrap_or_default().iter().sum::<u32>())
            }
        })?
    });
    let middle: Shared<_> = Rc::new({
        let inner = Rc::clone(&inner);
        let host = host.clone();
        let log = Rc::clone(&entered);
        task("nest-2", move |_scope: Scope, value: u32| {
            let inner = Rc::clone(&inner);
            let host = host.clone();
            let log = Rc::clone(&log);
            async move {
                log.set(log.get().saturating_add(1));
                Ok(host
                    .run(&inner, value)
                    .await
                    .into_output()
                    .unwrap_or_default())
            }
        })?
    });
    let top: Shared<_> = Rc::new({
        let middle = Rc::clone(&middle);
        let host = host.clone();
        let log = Rc::clone(&entered);
        task("nest-1", move |_scope: Scope, value: u32| {
            let middle = Rc::clone(&middle);
            let host = host.clone();
            let log = Rc::clone(&log);
            async move {
                log.set(log.get().saturating_add(1));
                Ok(host
                    .run(&middle, value)
                    .await
                    .into_output()
                    .unwrap_or_default())
            }
        })?
    });

    let report: Report<u32> = drive(host.run(&top, 21_u32));
    assert_eq!(
        report.disposition(),
        Disposition::Succeeded,
        "four levels of nesting complete at a ceiling of one: {:?}",
        report.error()
    );
    assert_eq!(
        report.output().copied(),
        Some(42),
        "the innermost task's value came back up the whole chain"
    );
    assert_eq!(entered.get(), 3, "every wrapper above the leaf ran");
    assert_eq!(
        host.admission().available_permits(),
        1,
        "the single permit is back after the chain finished"
    );
    assert_eq!(
        host.admission().peak_in_flight(),
        1,
        "one permit was ever in flight at a time, never two"
    );
    assert_eq!(
        host.admission().refused(),
        0,
        "nesting at a ceiling of one refuses nothing: it charges the parent"
    );
    assert_eq!(
        host.admission().admitted(),
        4,
        "all four runs are counted: three wrappers and the leaf, which shared one permit"
    );
    Ok(())
}

/// The step paths a nested chain reports read as one tree, so a report explains
/// itself without a debugger.
#[test]
fn a_nested_report_locates_every_level() -> TestResult {
    let host = host("acme")?;
    let inner = task("inner", |scope: Scope, value: u32| async move {
        let deeper = scope.enter("deeper")?;
        deeper.checkpoint()?;
        Ok(value)
    })?;

    let report: Report<u32> = drive(host.run(&inner, 5_u32));
    assert_eq!(
        report.disposition(),
        Disposition::Succeeded,
        "the inner task runs: {:?}",
        report.error()
    );
    let steps: Vec<String> = report.steps().iter().map(|path| path.to_string()).collect();
    assert_eq!(
        steps,
        vec!["inner".to_owned(), "inner/deeper".to_owned()],
        "the trail reads as the path the run actually entered"
    );
    Ok(())
}

// ── Deadlines, stops and refusal ────────────────────────────────────────────

/// A body that overruns its host's deadline is `DeadlineExceeded`, located at
/// the task's own body step.
#[test]
fn a_deadline_is_reported_with_its_step_path() -> TestResult {
    let host = Host::builder("acme")?
        .default_deadline(Duration::from_millis(50))
        .build()?;
    let overrun = task("overrun", |scope: Scope, ()| async move {
        // A body that waits on something which never arrives, inside a step of
        // its own so the location is the one a reader would expect. The wait is
        // deliberately never reached: the host's deadline fires first.
        let inner = scope.enter("wait")?;
        let _waited = within(&inner, "forever", Duration::from_secs(3_600), async {
            std::future::pending::<Result<(), FlowError>>().await
        })
        .await;
        Ok(())
    })?;

    let report: Report<()> = drive(host.run(&overrun, ()));
    assert_eq!(
        report.disposition(),
        Disposition::DeadlineExceeded,
        "a body that outruns the host's deadline is DeadlineExceeded"
    );
    assert_eq!(report.output(), None, "an overrun has no output");
    let error = report.error().ok_or("an overrun names its failure")?;
    assert!(
        matches!(error, FlowError::TimedOut { .. }),
        "the failure is the timeout itself: {error}"
    );
    let path = report.ended_at().unwrap_or("no path");
    assert!(
        path.starts_with("overrun"),
        "the timeout is located under the task's own step: {path}"
    );
    assert_eq!(
        host.admission().available_permits(),
        host.limits().max_concurrent_tasks(),
        "an overrun releases its permit rather than holding it forever"
    );
    Ok(())
}

/// A run that cancels its own scope is `Cancelled` and located.
#[test]
fn a_stop_is_reported_as_cancelled_with_its_step_path() -> TestResult {
    let host = host("acme")?;
    let stop_self = task("stop-self", |scope: Scope, ()| async move {
        scope.cancel();
        // The stop reaches the body through the same token, so the very next
        // checkpoint observes it.
        scope.checkpoint()?;
        Ok(())
    })?;

    let report: Report<()> = drive(host.run(&stop_self, ()));
    assert_eq!(
        report.disposition(),
        Disposition::Cancelled,
        "a body that stops its own scope is Cancelled, not Failed"
    );
    assert!(
        report.error().is_some_and(FlowError::is_cancelled),
        "the report's error is the cancellation itself"
    );
    let path = report.ended_at().unwrap_or("no path");
    assert!(
        path.starts_with("stop-self"),
        "the stop is located under the task's own step: {path}"
    );
    Ok(())
}

/// A run waiting for a permit when the host is cancelled is `Refused`: it was
/// never admitted, so no body ran and no step was entered.
#[test]
fn a_refused_run_never_entered_its_body() -> TestResult {
    let host = bounded_host("acme", 1)?;
    let entered = Rc::new(Cell::new(0_usize));

    // Hold the only permit with a run that parks forever.
    let holder_task = task("block", |_scope: Scope, ()| async move {
        std::future::pending::<()>().await;
        Ok(())
    })?;
    let mut holder = Box::pin(host.run(&holder_task, ()));
    assert!(
        pending_once(holder.as_mut()),
        "the blocking run parks in its body"
    );
    assert_eq!(
        host.admission().available_permits(),
        0,
        "the blocking run holds the only permit"
    );

    // The next run has to wait. Cancel the host while it is waiting.
    let body_entered = Rc::clone(&entered);
    let waiter = task("waiter", move |_scope: Scope, ()| {
        let seen = Rc::clone(&body_entered);
        async move {
            seen.set(seen.get().saturating_add(1));
            Ok(())
        }
    })?;
    let mut waiting_run = Box::pin(host.run(&waiter, ()));
    let _ = pending_once(waiting_run.as_mut());
    host.cancel();

    let report: Report<()> = drive(waiting_run);
    assert_eq!(
        report.disposition(),
        Disposition::Refused,
        "a run stopped at the admission gate is Refused: {:?}",
        report.error()
    );
    assert!(
        !report.disposition().was_admitted(),
        "a refused run was not admitted"
    );
    assert!(
        report.steps().is_empty(),
        "a refused run entered no step, so its trail is empty"
    );
    assert_eq!(entered.get(), 0, "a refused run's body never ran");
    assert_eq!(host.admission().refused(), 1, "the refusal is counted");

    drop(holder);
    Ok(())
}

// ── The ceiling under load ──────────────────────────────────────────────────

/// A thousand concurrent runs never exceed the ceiling, all complete, and no
/// permit is left behind.
///
/// Each run is a separate future polled on the caller's task, so each has its
/// own task-local scope and each takes its own permit — the property the nested
/// case must *not* have. The ceiling is measured twice: once by the bodies
/// themselves, and once by the host's own high-water mark.
#[test]
fn a_thousand_concurrent_runs_stay_under_the_ceiling() -> TestResult {
    const RUNS: usize = 1_000;
    const CEILING: usize = 7;

    let host = bounded_host("acme", CEILING)?;
    let live = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    // The body takes its own handles per call, so the outer bindings stay
    // readable afterwards for the assertions below.
    let body_live = Arc::clone(&live);
    let body_peak = Arc::clone(&peak);

    let work = task("tick", move |_scope: Scope, value: usize| {
        let live = Arc::clone(&body_live);
        let peak = Arc::clone(&body_peak);
        async move {
            let now = live.fetch_add(1, Ordering::Relaxed).saturating_add(1);
            let _previous = peak.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |high| {
                (now > high).then_some(now)
            });
            // Yield, so the runs genuinely overlap rather than completing one
            // at a time inside the first poll.
            for _ in 0..4 {
                lgwks_bot::rt::task::yield_now().await;
            }
            let _previous = live.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                count.checked_sub(1)
            });
            Ok(value)
        }
    })?;

    let reports: Vec<Report<usize>> = drive(lgwks_std::task::join_all(
        (0..RUNS).map(|index| host.run(&work, index)),
    ));
    assert_eq!(reports.len(), RUNS, "every run produced a report");
    for (index, report) in reports.iter().enumerate() {
        assert_eq!(
            report.disposition(),
            Disposition::Succeeded,
            "run {index} completed: {:?}",
            report.error()
        );
        assert_eq!(
            report.output().copied(),
            Some(index),
            "run {index} kept its own output; association is by input order"
        );
    }

    let observed_peak = peak.load(Ordering::Relaxed);
    assert!(
        observed_peak <= CEILING,
        "the bodies observed {observed_peak} concurrent, over the ceiling {CEILING}"
    );
    assert!(
        observed_peak > 1,
        "the runs must have overlapped; the peak was {observed_peak}"
    );
    assert_eq!(
        host.admission().peak_in_flight(),
        observed_peak,
        "the host's own high-water mark agrees with what the bodies saw"
    );
    assert_eq!(
        host.admission().available_permits(),
        CEILING,
        "every permit came back"
    );
    assert_eq!(host.admission().in_flight(), 0, "nothing is left in flight");
    assert_eq!(
        live.load(Ordering::Relaxed),
        0,
        "every body finished, so nothing is left borrowed"
    );
    Ok(())
}

// ── Tenants ─────────────────────────────────────────────────────────────────

/// Two hosts running the same task name over the same input produce distinct
/// step keys.
#[test]
fn two_tenants_never_share_a_step_key() -> TestResult {
    let acme = host("acme")?;
    let other = host("acme-other")?;
    let observed: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));

    let capture = |log: Rc<RefCell<Vec<String>>>| -> Result<Shared<_>, Box<dyn Error>> {
        Ok(Rc::new(task(
            "same-name",
            move |scope: Scope, value: u32| {
                let log = Rc::clone(&log);
                async move {
                    log.borrow_mut().push(scope.key().to_hex());
                    Ok(value)
                }
            },
        )?))
    };
    let one = capture(Rc::clone(&observed))?;
    let two = capture(Rc::clone(&observed))?;

    drive(acme.run(&one, 7_u32));
    drive(other.run(&two, 7_u32));

    let keys = observed.borrow();
    assert_eq!(keys.len(), 2, "each run recorded its own key once");
    assert_ne!(
        keys[0], keys[1],
        "two tenants, one task name, one input: the keys must differ"
    );
    Ok(())
}

/// The same tenant and path key the same on every run, so a key is usable as an
/// idempotency key across a retry.
#[test]
fn a_step_key_is_stable_across_runs_for_one_tenant() -> TestResult {
    let host = host("acme")?;
    let observed: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let body_log = Rc::clone(&observed);
    let keyed: Shared<_> = Rc::new(task("keyed", move |scope: Scope, value: u32| {
        let log = Rc::clone(&body_log);
        async move {
            log.borrow_mut().push(scope.key().to_hex());
            Ok(value)
        }
    })?);

    drive(host.run(&keyed, 1_u32));
    drive(host.run(&keyed, 1_u32));

    let keys = observed.borrow();
    assert_eq!(keys.len(), 2, "each run recorded its key once");
    assert_eq!(
        keys[0], keys[1],
        "the same tenant and the same path key the same every run"
    );
    Ok(())
}

/// Two hosts hold separate budgets and separate stops: neither can starve the
/// other, and one host's cancellation does not reach another's work.
#[test]
fn two_hosts_hold_separate_budgets() -> TestResult {
    let left = bounded_host("left", 1)?;
    let right = bounded_host("right", 1)?;
    let echo = task("echo", |_scope: Scope, value: u32| async move { Ok(value) })?;

    let blocker = task("block", |_scope: Scope, ()| async move {
        std::future::pending::<()>().await;
        Ok(())
    })?;
    let mut holder = Box::pin(left.run(&blocker, ()));
    let _ = pending_once(holder.as_mut());

    assert_eq!(left.admission().available_permits(), 0, "left is busy");
    let report: Report<u32> = drive(right.run(&echo, 9_u32));
    assert_eq!(
        report.disposition(),
        Disposition::Succeeded,
        "a busy host must not starve an unrelated one: {:?}",
        report.error()
    );
    assert_eq!(report.output().copied(), Some(9));

    left.cancel();
    assert!(
        !right.is_cancelled(),
        "stopping one host leaves the other alone"
    );
    drop(holder);
    Ok(())
}

// ── The synchronous entry ───────────────────────────────────────────────────

/// `block_on` refuses inside a running runtime rather than parking the reactor,
/// and works outside one, owning a runtime once.
#[test]
fn a_synchronous_run_refuses_inside_a_running_runtime() -> TestResult {
    let host = host("acme")?;
    let echo = task("echo", |_scope: Scope, value: u32| async move { Ok(value) })?;

    let inside: Result<(), Box<dyn Error>> = drive(async {
        // Inside a runtime, so the synchronous entry must refuse rather than
        // park the thread whose reactor this very run needs.
        match host.block_on(&echo, 1_u32) {
            Err(HostError::InsideRuntime) => Ok(()),
            Err(other) => Err(format!("the refusal must name the runtime: {other}").into()),
            Ok(_) => Err("block_on must refuse inside a running runtime".into()),
        }
    });
    inside?;

    let first = host.block_on(&echo, 2_u32)?;
    assert_eq!(first.output().copied(), Some(2), "the sync path runs");
    let second = host.block_on(&echo, 3_u32)?;
    assert_eq!(
        second.output().copied(),
        Some(3),
        "a second synchronous run reuses the host's reactor rather than making another"
    );
    Ok(())
}

// ── The report's own claims ─────────────────────────────────────────────────

/// Every disposition claims no external effect: a local task journals nothing,
/// so no report can be read as proof a remote state changed.
#[test]
fn a_report_claims_no_external_effect() -> TestResult {
    let host = host("acme")?;
    let ok = task("ok", |_scope: Scope, ()| async { Ok(()) })?;
    let bad = task("bad", |_scope: Scope, ()| async {
        Err(FlowError::failed("no"))
    })?;

    let succeeded: Report<()> = drive(host.run(&ok, ()));
    let failed: Report<()> = drive(host.run(&bad, ()));
    for report in [&succeeded, &failed] {
        assert_eq!(
            report.effects(),
            EffectKnowledge::None,
            "a local task performs no journaled effect on any path"
        );
    }
    Ok(())
}

/// Trail overflow loses only the declared detail, counts it, and leaves the
/// terminal state intact.
#[test]
fn an_overflowing_trail_counts_what_it_dropped() -> TestResult {
    const CAPACITY: usize = 4;
    const ITEMS: u32 = 20;
    let host = Host::builder("acme")?
        .progress_capacity(CAPACITY)
        .default_deadline(Duration::from_secs(30))
        .build()?;

    let walk = task("walk", |scope: Scope, items: Vec<u32>| async move {
        each::<_, u32, _, _>(
            &scope,
            "page",
            NonZeroUsize::new(1),
            items,
            |step, item| async move {
                step.checkpoint()?;
                Ok(item)
            },
        )
        .await
    })?;

    let report: Report<Vec<u32>> = drive(host.run(&walk, (0..ITEMS).collect()));
    assert_eq!(
        report.disposition(),
        Disposition::Succeeded,
        "an overflowing trail does not fail the run"
    );
    assert_eq!(
        report.output().map(Vec::len),
        Some(usize::try_from(ITEMS)?),
        "the typed output survives the overflow intact"
    );
    assert_eq!(
        report.steps().len(),
        CAPACITY,
        "the trail is bounded by the host's declared capacity"
    );
    assert!(
        report.dropped_steps() > 0,
        "what the ring dropped is counted, not hidden"
    );
    assert_eq!(
        report.progress_capacity(),
        CAPACITY,
        "the report states what it could have retained"
    );
    let last = report
        .steps()
        .last()
        .map(|path| path.to_string())
        .ok_or("a truncated trail still ends where the run stopped")?;
    assert!(
        last.ends_with(&format!("page#{}", ITEMS - 1)),
        "the retained trail is the suffix, ending at the last item: {last}"
    );
    Ok(())
}

/// Every ceiling the host resolves from is finite and readable, and one outside
/// its declared bound is refused at build time.
#[test]
fn ceilings_are_finite_readable_and_bounded() -> TestResult {
    let host = host("acme")?;
    let limits = host.limits();
    assert!(
        (1..=lgwks_bot::task::MAX_ADMITTED_TASKS).contains(&limits.max_concurrent_tasks()),
        "the admission ceiling is at least one and bounded, got {}",
        limits.max_concurrent_tasks()
    );
    assert!(
        !limits.default_deadline().is_zero()
            && limits.default_deadline() <= lgwks_bot::task::MAX_TASK_DEADLINE,
        "the default deadline is finite and within the ceiling"
    );
    assert!(
        limits.progress_capacity() >= 1,
        "the trail retains at least one path"
    );

    let zero = Host::builder("acme")?.default_deadline(Duration::ZERO);
    assert!(
        matches!(zero.build(), Err(HostError::Bound { .. })),
        "a zero deadline would expire every run before its body starts"
    );
    let over = Host::builder("acme")?.default_deadline(lgwks_bot::task::MAX_TASK_DEADLINE * 2);
    assert!(
        matches!(over.build(), Err(HostError::Bound { .. })),
        "a deadline over the ceiling is refused"
    );
    let over = NonZeroUsize::new(lgwks_bot::task::MAX_ADMITTED_TASKS + 1)
        .ok_or("the ceiling plus one must be a valid nonzero bound")?;
    assert!(
        matches!(
            Host::builder("acme")?.max_concurrent_tasks(over).build(),
            Err(HostError::Bound { .. })
        ),
        "an admission ceiling over the bound is refused"
    );
    Ok(())
}

/// A task's body runs under the task's own named step, under the host's tenant.
#[test]
fn a_task_body_runs_under_its_own_named_step() -> TestResult {
    let host = host("acme")?;
    let paths: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let body_paths = Rc::clone(&paths);
    let located = task("located", move |scope: Scope, ()| {
        let log = Rc::clone(&body_paths);
        async move {
            log.borrow_mut().push(scope.path().to_owned());
            log.borrow_mut().push(scope.tenant().as_str().to_owned());
            // The key is read here rather than discarded: the body proves the scope
            // exposes one, and the value is bound so the must-use rule holds.
            let _key = scope.key();
            Ok(())
        }
    })?;

    let report: Report<()> = drive(host.run(&located, ()));
    assert_eq!(
        report.disposition(),
        Disposition::Succeeded,
        "the body ran: {:?}",
        report.error()
    );
    assert_eq!(
        *paths.borrow(),
        vec!["located".to_owned(), "acme".to_owned()],
        "the body's scope is the task's own step under the host's tenant"
    );
    assert_eq!(
        report.steps().first().map(|path| path.as_ref()),
        Some("located"),
        "the trail begins at the task's own step"
    );
    Ok(())
}

/// A task name is a label, not an identity: two declarations of one logical task
/// share its name, and each keeps its own body type.
///
/// `type_name_of_val` cannot tell two closures apart — both render as
/// `{{closure}}` — so the bodies differ in their *inputs* instead. That is a
/// stronger statement anyway: the type is carried, not erased.
#[test]
fn a_task_name_is_a_label_not_an_identity() -> TestResult {
    let numbers = task(
        "review-pr",
        |_scope: Scope, value: u32| async move { Ok(value) },
    )?;
    let lines = task("review-pr", |_scope: Scope, value: String| async move {
        Ok(value.len())
    })?;

    assert_eq!(
        numbers.name(),
        lines.name(),
        "two declarations of one logical task share its name"
    );
    // The same name, and two different bodies that the front door keeps apart.
    let host = host("acme")?;
    assert_eq!(
        drive(host.run(&numbers, 7_u32)).output().copied(),
        Some(7),
        "the first body answers its own input type"
    );
    assert_eq!(
        drive(host.run(&lines, "abc".to_owned())).output().copied(),
        Some(3),
        "the second body answers its own input type, under the same name"
    );
    Ok(())
}
