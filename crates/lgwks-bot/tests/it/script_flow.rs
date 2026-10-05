//! `script!` end to end: real timers, real supervisors, the architecture map.
//!
//! The seeded families in `sim_script.rs` pin the fan-out, retry, tenancy and
//! stop properties without a clock. This file covers what needs one, or needs
//! a whole flow rather than a sweep: a deadline that really passes, branches
//! that really overlap, a flow owned by a `Supervisor` and stopped by its
//! shutdown, and the map a script emits.

#![cfg(feature = "script")]

use std::cell::RefCell;
use std::error::Error;
use std::time::Duration;

use lgwks_bot::rt::supervise::{Supervisor, TaskOutcome};
use lgwks_bot::rt::sync::CancellationToken;
use lgwks_bot::rt::time::sleep;
use lgwks_bot::script::{FlowError, Scope, StepKind, Tenant};

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

lgwks_bot::script! {
    /// Wait `pause`, but give up after 20 milliseconds.
    flow patient(pause: Duration) -> &'static str:
        within 20ms:
            sleep(pause).await
            "arrived"

    /// Two branches that interleave, recording the order they ran in.
    flow interleaved(log: &RefCell<Vec<&'static str>>) -> usize:
        together:
            let first = run branch(log, "a")
            let second = run branch(log, "b")
        give back first.saturating_add(second)

    /// Record a start, yield once, record an end.
    flow branch(log: &RefCell<Vec<&'static str>>, name: &'static str) -> usize:
        log.borrow_mut().push(name)
        lgwks_bot::rt::task::yield_now().await
        log.borrow_mut().push(name)
        give back 1

    /// The key a flow sees, so callers can compare them.
    flow key_of() -> String:
        scope.key().to_hex()

    /// Run `key_of` twice in one scope: the two calls must not share a key.
    flow twice() -> (String, String):
        let first = run key_of()
        let second = run key_of()
        give back (first, second)

    /// Sequential steps, each in its own scope.
    flow ladder(count: u32) -> Vec<String>:
        let mut paths = Vec::new()
        for rung in 0..count:
            step climb:
                paths.push(format!("{rung}:{}", scope.path()))
        give back paths

    /// Branch on a value, and fail from the branch that refuses it.
    flow classify(value: i64) -> &'static str:
        if value < 0:
            fail with "negative"
        else if value == 0:
            give back "zero"
        else:
            give back "positive"

    /// Sleep a millisecond per item on real timers, 256 at a time.
    flow timers(count: u32) -> usize:
        let done = each item in 0..count:
            sleep(Duration::from_millis(1)).await
            item
        give back done.len()

    /// Every block nested at once, fan-out sized by the machine: the shape a
    /// spawned tenant runs.
    flow spread(count: u32) -> usize:
        let done = each item in 0..count:
            retry up to 3 times, waiting 1ms:
                within 1s:
                    sleep(Duration::from_millis(1)).await
                    item
        give back done.len()

    /// Item 3 cancels its own scope; its siblings sleep past the stop.
    flow stop_from_body(count: u32) -> usize:
        let done = each item in 0..count:
            if item == 3 { scope.cancel(); }
            sleep(Duration::from_millis(50)).await
            item
        give back done.len()

    /// Wait until stopped: the shape of a long-running loop under a supervisor.
    flow until_stopped() -> ():
        within 1h:
            sleep(Duration::from_secs(3_600)).await
}

/// A root scope for tenant `acme`.
fn acme() -> Result<Scope, FlowError> {
    Ok(Scope::root(Tenant::new("acme")?))
}

#[test]
fn a_deadline_that_passes_fails_the_step_as_timed_out() -> TestResult {
    let scope = acme()?;
    let quick = lgwks_bot::block_on(patient(&scope, Duration::ZERO))?;
    assert_eq!(
        quick, "arrived",
        "a body inside its deadline returns its value"
    );

    let slow = lgwks_bot::block_on(patient(&scope, Duration::from_secs(5)));
    let Err(FlowError::TimedOut { ref at, after }) = slow else {
        return Err(format!("expected a timeout, got {slow:?}").into());
    };
    assert_eq!(
        &**at, "patient/within",
        "the timeout names the block that expired"
    );
    assert_eq!(
        after,
        Duration::from_millis(20),
        "and the deadline it declared"
    );
    assert!(
        slow.is_err_and(|error| error.is_retryable()),
        "a timeout is worth retrying"
    );
    Ok(())
}

#[test]
fn together_runs_its_branches_concurrently_and_binds_each_result() -> TestResult {
    let scope = acme()?;
    let log = RefCell::new(Vec::new());
    let total = lgwks_bot::block_on(interleaved(&scope, &log))?;
    assert_eq!(total, 2, "each branch's value is bound to its name");
    let order = log.take();
    assert_eq!(order.len(), 4, "both branches ran to their end");
    let first_end = order.get(2).copied();
    assert!(
        order
            .get(..2)
            .is_some_and(|starts| starts.contains(&"a") && starts.contains(&"b")),
        "both branches started before either finished: {order:?}, first end {first_end:?}"
    );
    Ok(())
}

#[test]
fn two_runs_of_one_flow_in_one_scope_get_distinct_stable_keys() -> TestResult {
    let scope = acme()?;
    let (first, second) = lgwks_bot::block_on(twice(&scope))?;
    assert_ne!(
        first, second,
        "a second call is a second step, not a duplicate of the first"
    );
    let (again_first, again_second) = lgwks_bot::block_on(twice(&scope))?;
    assert_eq!(
        (first, second),
        (again_first, again_second),
        "and a rerun reproduces both"
    );
    Ok(())
}

#[test]
fn every_iteration_of_a_for_runs_in_its_own_scope() -> TestResult {
    let scope = acme()?;
    let paths = lgwks_bot::block_on(ladder(&scope, 3))?;
    assert_eq!(
        paths,
        [
            "0:ladder/for:rung#0/climb",
            "1:ladder/for:rung#1/climb",
            "2:ladder/for:rung#2/climb",
        ],
        "the path names the flow, the iteration and the step"
    );
    Ok(())
}

#[test]
fn branches_choose_the_value_and_a_refusing_branch_fails_the_flow() -> TestResult {
    let scope = acme()?;
    assert_eq!(lgwks_bot::block_on(classify(&scope, 0))?, "zero");
    assert_eq!(lgwks_bot::block_on(classify(&scope, 7))?, "positive");
    let refused = lgwks_bot::block_on(classify(&scope, -1));
    let Err(error) = refused else {
        return Err("a negative value is refused".into());
    };
    assert_eq!(
        error.to_string(),
        "classify: failed: \"negative\"",
        "located and quoted"
    );
    Ok(())
}

/// Regression: a fan-out over real timers used to spin inside one poll once
/// the engine's cooperative budget ran out, and never finished. Ten thousand
/// one-millisecond bodies, 256 at a time, is about forty milliseconds of work;
/// the deadline here is two hundred times that, so only a hang fails it.
#[test]
fn a_fan_out_over_real_timers_finishes_and_yields_to_its_executor() -> TestResult {
    let scope = acme()?;
    let finished = lgwks_bot::block_on(lgwks_bot::rt::time::timeout(
        Duration::from_secs(8),
        timers(&scope, 10_000),
    ));
    let count = finished.map_err(|_elapsed| "the fan-out hung past its deadline")??;
    assert_eq!(count, 10_000, "every body ran");
    Ok(())
}

/// Regression: `retry` once took an `AsyncFnMut(&Scope, u32)` body, whose
/// higher-ranked future made any flow nesting it inside `each` fail to be
/// `Send`, so a tenant's flow could not be spawned. Spawning is the proof:
/// this file does not compile if a flow stops being `Send`.
#[test]
fn a_flow_nesting_every_block_is_send_and_runs_spawned_per_tenant() -> TestResult {
    let runtime = lgwks_bot::Runtime::new()?;
    let counts = runtime.block_on(async {
        let mut tenants = lgwks_bot::rt::task::JoinSet::new();
        for name in ["acme", "globex"] {
            tenants.spawn(async move {
                let scope = Scope::root(Tenant::new(name)?);
                spread(&scope, 2_000).await
            });
        }
        let mut counts = Vec::new();
        while let Some(joined) = tenants.join_next().await {
            counts.push(joined.map_err(|error| error.to_string())??);
        }
        Ok::<_, Box<dyn Error>>(counts)
    })?;
    assert_eq!(counts, [2_000, 2_000], "both tenants ran every item");
    Ok(())
}

/// An `each` body shares its step's stop, so cancelling from inside one ends
/// the whole fan-out as a `break` ends a loop: no sibling runs on, and the
/// stop is reported at the fan-out rather than lost.
#[test]
fn cancelling_inside_a_body_stops_the_whole_fan_out() -> TestResult {
    let scope = acme()?;
    let started = std::time::Instant::now();
    let stopped = lgwks_bot::block_on(stop_from_body(&scope, 100));
    let Err(ref error) = stopped else {
        return Err(format!("expected a stop, got {stopped:?}").into());
    };
    assert!(error.is_cancelled(), "reported as a stop: {error}");
    assert_eq!(error.at(), "stop_from_body/each:item", "at the fan-out");
    assert!(
        started.elapsed() < Duration::from_millis(40),
        "siblings were dropped, not waited for: {:?}",
        started.elapsed()
    );
    assert!(!scope.is_cancelled(), "the caller's scope is untouched");
    Ok(())
}

#[test]
fn a_supervisor_owns_a_flow_and_its_shutdown_stops_it() -> TestResult {
    let runtime = lgwks_bot::Runtime::new()?;
    let outcomes = runtime.block_on(async {
        let mut supervisor = Supervisor::new(4);
        supervisor
            .spawn(|token: CancellationToken| async move {
                let Ok(tenant) = Tenant::new("acme") else {
                    return;
                };
                let scope = Scope::with_token(tenant, token);
                let stopped = until_stopped(&scope).await;
                assert!(
                    stopped.is_err_and(|error| error.is_cancelled()),
                    "a supervisor's shutdown reaches the flow as a stop"
                );
            })
            .await;
        supervisor.shutdown().await
    });
    assert!(
        outcomes
            .outcomes()
            .iter()
            .all(|outcome| matches!(*outcome, TaskOutcome::Cancelled { .. })),
        "the supervised flow ended cancelled, not panicked: {:?}",
        outcomes.outcomes()
    );
    Ok(())
}

#[test]
fn the_script_emits_a_map_of_the_orchestration_it_declares() -> TestResult {
    let names: Vec<&str> = ARCHITECTURE
        .flows()
        .iter()
        .map(|flow| flow.name())
        .collect();
    assert_eq!(
        names,
        [
            "patient",
            "interleaved",
            "branch",
            "key_of",
            "twice",
            "ladder",
            "classify",
            "timers",
            "spread",
            "stop_from_body",
            "until_stopped",
        ],
        "every flow, in declaration order"
    );
    let interleaved = ARCHITECTURE
        .flow("interleaved")
        .ok_or("interleaved is mapped")?;
    assert_eq!(
        interleaved.runs(),
        ["branch", "branch"],
        "the map knows who calls whom"
    );
    let first = interleaved
        .steps()
        .first()
        .ok_or("interleaved has a block")?;
    assert_eq!(
        first.kind(),
        StepKind::Together,
        "the block kinds are recorded"
    );

    let rendered = ARCHITECTURE.to_string();
    assert!(
        rendered.contains("flow patient(pause: Duration) -> &'static str"),
        "the rendering carries each signature: {rendered}"
    );
    assert!(
        rendered.contains("within 20ms"),
        "and each block header: {rendered}"
    );
    let json = ARCHITECTURE.to_json()?;
    assert!(
        json.contains("\"kind\":\"Together\""),
        "the JSON form is machine-readable: {json}"
    );
    Ok(())
}
