//! Deadlines, stops, refusal, the ceiling under load, tenant isolation and the synchronous entry of the task front door.

use super::*;

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
    let path = report
        .ended_at()
        .ok_or("the report names the step it ended at")?;
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
    let path = report
        .ended_at()
        .ok_or("the report names the step it ended at")?;
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
    // The shared instrument clones the handles per call, so the outer bindings
    // stay readable afterwards for the assertions below. Four yields, so the
    // runs genuinely overlap rather than completing inside the first poll.
    let work = load::ticking_task!(live, peak, 4)?;

    let reports: Vec<Report<usize>> = drive(lgwks_std::task::join_all(
        (0..RUNS).map(|index| host.run(&work, index)),
    ));
    assert_eq!(reports.len(), RUNS, "every run produced a report");
    load::assert_runs_succeeded(&reports, "the thousand runs");

    let observed_peak = peak.load(Ordering::Relaxed);
    assert!(
        observed_peak > 1,
        "the runs must have overlapped; the peak was {observed_peak}"
    );
    load::assert_admission_conserved(&host, observed_peak, CEILING, "the thousand runs");
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
