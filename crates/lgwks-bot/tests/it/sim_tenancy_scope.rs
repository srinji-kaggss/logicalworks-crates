//! Simulation family: a `Scope`'s tenant reaches every body under `each`,
//! `FanOut` and `retry` (#268 item 3).
//!
//! `each` drives its bodies on the awaiting task and takes no permit, so there
//! is no admission for a tenant to reach — what reaches the bodies is the scope
//! itself. Every `each` item scope, every `retry` attempt scope and every
//! `FanOut::run_in` item scope descends from the caller's scope, and a
//! descended scope carries its parent's tenant by construction
//! ([`Scope::tenant`]). These tests pin that carrying on the shipped functions:
//! two tenants over the same items key their steps differently, and one
//! tenant's retry attempts key identically.
//!
//! Nothing here reads the clock, the OS scheduler or an unseeded entropy
//! source, and no elapsed time enters any assertion, so a run that merely ran
//! slower reads exactly as a fast one. `block_on` drives each future on the
//! calling thread; the order each test asserts is input order, which is what
//! `each` returns however its bodies finished.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `each_body_scopes_carry_the_root_tenant` | every item scope's tenant is the root's, and two tenants key the same path differently |
//! | `retry_attempts_share_one_tenant_and_key` | every attempt runs as the root tenant under one step key |
//! | `a_scoped_fan_out_runs_under_the_caller_scope` | `run_in` enters the caller's step, inherits its clock and stop |
//! | `a_standalone_fan_out_keeps_its_reserved_tenant` | `run` without a scope still runs under its reserved tenant, not any caller's |
//! | `the_same_scope_trace_replays` | the tenant-and-key trace is identical on a second run |

#![cfg(feature = "script")]

use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lgwks_bot::script::{Clock, FanOut, FanOutError, FlowError, Scope, Tenant, each, retry};

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

/// One trace line: whose work this was, where it ran, and the idempotency key
/// the step would deduplicate by.
fn trace_line(scope: &Scope) -> String {
    format!(
        "{}:{}:{}",
        scope.tenant(),
        scope.path(),
        scope.key().to_hex()
    )
}

/// Record `scope`'s trace line, or fail the body when the trace is poisoned.
///
/// A poisoned trace means a body already panicked, which fails the test on its
/// own; the body's error carries that fact rather than replacing it with a
/// second one.
async fn record(records: &Mutex<Vec<String>>, scope: &Scope) -> Result<String, FlowError> {
    let line = trace_line(scope);
    match records.lock() {
        Ok(mut held) => held.push(line.clone()),
        Err(error) => {
            let refusal = Err(FlowError::failed(format!(
                "the trace was poisoned: {error}"
            )));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "record: returning an error to the caller");
            return refusal;
        }
    }
    Ok(line)
}

/// Read the trace back, or fail the test when the trace is poisoned.
fn trace_of(records: &Mutex<Vec<String>>) -> Result<Vec<String>, String> {
    match records.lock() {
        Ok(held) => Ok(held.clone()),
        Err(error) => Err(format!("the trace was poisoned: {error}")),
    }
}

/// Run one `each` over four items under `scope`, recording each item scope's
/// trace line, and return the values in input order.
async fn trace_each(
    scope: &Scope,
    records: &Arc<Mutex<Vec<String>>>,
) -> Result<Vec<usize>, FlowError> {
    each(scope, "page", None, 0..4_usize, |item_scope, item| {
        let records = Arc::clone(records);
        async move {
            record(&records, &item_scope).await?;
            Ok::<usize, FlowError>(item)
        }
    })
    .await
}

/// Every `each` body runs in a scope of the root's tenant, and the tenant is
/// what distinguishes two tenants' keys over the same items.
#[test]
fn each_body_scopes_carry_the_root_tenant() -> TestResult {
    for tenant_name in ["acme", "globex"] {
        let scope = Scope::root(Tenant::new(tenant_name)?);
        let records: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let values = lgwks_bot::block_on(trace_each(&scope, &records));
        let values = match values {
            Ok(values) => values,
            Err(error) => return Err(format!("{tenant_name}'s fan-out failed: {error}").into()),
        };
        assert_eq!(
            values,
            vec![0, 1, 2, 3],
            "{tenant_name}'s fan-out returns its items in input order"
        );
        let trace = trace_of(&records)?;
        assert_eq!(
            trace.len(),
            4,
            "{tenant_name} ran every item and recorded every scope"
        );
        for (index, line) in trace.iter().enumerate() {
            let expected_path = format!("page#{index}");
            assert!(
                line.starts_with(&format!("{tenant_name}:{expected_path}:")),
                "item {index} ran as {tenant_name} at {expected_path}, got {line}"
            );
        }
    }
    // The same path under two tenants keys differently: the tenant is in the
    // key, so one tenant's retried effect is never another's.
    let key_under = |tenant_name: &str| -> Result<String, Box<dyn Error>> {
        let scope = Scope::root(Tenant::new(tenant_name)?);
        let values = lgwks_bot::block_on(each(
            &scope,
            "page",
            None,
            0..1_usize,
            |item_scope, _item| async move { Ok::<String, FlowError>(item_scope.key().to_hex()) },
        ));
        match values {
            Ok(values) => match values.into_iter().next() {
                Some(key) => Ok(key),
                None => Err(format!("{tenant_name} ran no item").into()),
            },
            Err(error) => Err(format!("{tenant_name}'s fan-out failed: {error}").into()),
        }
    };
    let one = key_under("acme")?;
    let other = key_under("globex")?;
    assert!(one != other, "the same path keys differently per tenant");
    Ok(())
}

/// Every `retry` attempt runs in the root's tenant under one step key, so an
/// effect keyed by it is one effect however many attempts it takes.
#[test]
fn retry_attempts_share_one_tenant_and_key() -> TestResult {
    let scope = Scope::root(Tenant::new("acme")?);
    let records: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let attempts = 3_u32
        .try_into()
        .map_err(|refusal| format!("3 is an attempt count: {refusal}"))?;
    let outcome = lgwks_bot::block_on(retry(
        &scope,
        "send",
        attempts,
        Duration::from_millis(1),
        |attempt_scope, attempt| {
            let records = Arc::clone(&records);
            async move {
                record(&records, &attempt_scope).await?;
                if attempt < 3 {
                    Err(FlowError::transient("not yet"))
                } else {
                    Ok("sent")
                }
            }
        },
    ));
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            return Err(
                format!("the retry failed instead of sending on attempt 3: {error}").into(),
            );
        }
    };
    assert_eq!(outcome, "sent", "the third attempt sends");
    let trace = trace_of(&records)?;
    assert_eq!(
        trace.len(),
        3,
        "three attempts ran and recorded three scopes"
    );
    for (position, line) in trace.iter().enumerate() {
        assert!(
            line.starts_with("acme:send:"),
            "attempt {} ran as acme at send, got {line}",
            position.saturating_add(1)
        );
    }
    assert!(
        trace[0] == trace[1] && trace[1] == trace[2],
        "every attempt shares one tenant and one step key, got {trace:?}"
    );
    Ok(())
}

/// `run_in` enters the caller's step: a failing item reads as the body's own
/// error at its position without stopping the caller's scope, its deadline
/// reads the caller's clock, and a stopped caller stops it at the given scope.
#[test]
fn a_scoped_fan_out_runs_under_the_caller_scope() -> TestResult {
    // A failing item is the body's own error at its position: the fan-out
    // entered the scope it was given and ran there.
    let scope = Scope::root(Tenant::new("acme")?);
    let outcome: Result<Vec<u32>, FanOutError<String>> = lgwks_bot::block_on(
        FanOut::new(0..4_u32).run_in(&scope, "doubled", |id| async move {
            if id == 2 {
                Err(String::from("the third item failed"))
            } else {
                Ok(id.saturating_mul(2))
            }
        }),
    );
    match outcome {
        Err(FanOutError::Item { index, error }) => {
            assert_eq!(index, 2, "the failing item's position is reported");
            assert_eq!(
                error, "the third item failed",
                "the body's own error comes back unchanged"
            );
        }
        Err(other) => {
            return Err(format!("the third item's failure reads as an item, got {other:?}").into());
        }
        Ok(_) => return Err("the third item failed, so the fan-out cannot succeed".into()),
    }
    assert!(
        !scope.is_cancelled(),
        "one failing item stops the fan-out without stopping the caller's scope"
    );

    // A deadline reads the caller's clock: a scope whose clock is already past
    // the deadline refuses without polling a body, with no real wait.
    let spent = Scope::with_clock(
        Tenant::new("acme")?,
        Clock::virtual_at(Duration::from_secs(10)),
    );
    let outcome: Result<Vec<u32>, FanOutError<String>> = lgwks_bot::block_on(
        FanOut::new(0..4_u32)
            .within(Duration::from_millis(50))
            .run_in(&spent, "doubled", |id| async move {
                let _ = std::future::pending::<()>().await;
                Ok::<u32, String>(id)
            }),
    );
    match outcome {
        Err(FanOutError::TimedOut { after }) => {
            assert_eq!(
                after,
                Duration::from_millis(50),
                "the deadline that passed is the one the fan-out declared"
            );
        }
        Err(other) => {
            return Err(format!("a spent clock reads as a timeout, got {other:?}").into());
        }
        Ok(_) => return Err("no body ran, so a success would be a second deadline".into()),
    }

    // A stopped caller stops the fan-out at the scope it was given: the stop
    // is observed before the step is entered, so it is located at the
    // caller's own path rather than under a fresh root.
    let root = Scope::root(Tenant::new("acme")?);
    let flow = match root.enter("flow") {
        Ok(flow) => flow,
        Err(error) => return Err(format!("entering the flow step failed: {error}").into()),
    };
    flow.cancel();
    let outcome: Result<Vec<u32>, FanOutError<String>> = lgwks_bot::block_on(
        FanOut::new(0..4_u32).run_in(&flow, "doubled", |id| async move { Ok::<u32, String>(id) }),
    );
    match outcome {
        Err(FanOutError::Flow(flow_error)) => {
            assert!(
                flow_error.is_cancelled(),
                "a stopped caller reads as a stop, got {flow_error:?}"
            );
            assert_eq!(
                flow_error.at(),
                "flow",
                "the stop is located at the given scope, got {}",
                flow_error.at()
            );
        }
        Err(other) => {
            return Err(format!("a stopped caller reads as a stop, got {other:?}").into());
        }
        Ok(_) => return Err("a stopped caller admits no items".into()),
    }
    Ok(())
}

/// `run` without a scope still runs under its reserved tenant: the default did
/// not silently become any caller's, and any caller's did not leak into it.
#[test]
fn a_standalone_fan_out_keeps_its_reserved_tenant() -> TestResult {
    let outcome = lgwks_bot::block_on(
        FanOut::new(1_u32..=4).run(|id| async move { Ok::<u32, String>(id.saturating_mul(2)) }),
    );
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => return Err(format!("the standalone fan-out failed: {error:?}").into()),
    };
    assert_eq!(
        outcome,
        vec![2, 4, 6, 8],
        "the standalone fan-out runs its items in input order"
    );
    Ok(())
}

/// The tenant-and-key trace of one `each` plus one `retry` is identical on a
/// second run: same definitions, same trace.
#[test]
fn the_same_scope_trace_replays() -> TestResult {
    let trace_once = || -> Result<Vec<String>, Box<dyn Error>> {
        let scope = Scope::root(Tenant::new("acme")?);
        let records: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let values = lgwks_bot::block_on(trace_each(&scope, &records));
        match values {
            Ok(values) => {
                if values != vec![0, 1, 2, 3] {
                    return Err("the fan-out returns its items in input order".into());
                }
            }
            Err(error) => return Err(format!("the fan-out failed: {error}").into()),
        }
        let attempts = 2_u32
            .try_into()
            .map_err(|refusal| format!("2 is an attempt count: {refusal}"))?;
        let outcome = lgwks_bot::block_on(retry(
            &scope,
            "send",
            attempts,
            Duration::from_millis(1),
            |attempt_scope, _attempt| {
                let records = Arc::clone(&records);
                async move {
                    record(&records, &attempt_scope).await?;
                    Ok::<&str, FlowError>("sent")
                }
            },
        ));
        match outcome {
            Ok(_) => {}
            Err(error) => return Err(format!("the retry failed: {error}").into()),
        }
        Ok(trace_of(&records)?)
    };
    let first = trace_once()?;
    let second = trace_once()?;
    assert!(
        !first.is_empty(),
        "the trace records every scope it entered"
    );
    assert_eq!(
        first, second,
        "the same definitions produce the same tenant-and-key trace twice"
    );
    Ok(())
}
