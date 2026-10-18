//! The script tier's documented examples, run as tests (#526).
//!
//! Each used to be a doctest that named the crate by path: the bounded fan-out, its scoped form
//! and the durable `remember` step. They are tests here, so each documented claim runs under
//! nextest with the rest of the suite.

use std::error::Error;
use std::time::Duration;

use lgwks_bot::script::{FanOut, FanOutError, FlowError, Scope, Tenant, remember};

/// What every fallible test here returns.
type TestResult = Result<(), Box<dyn Error>>;

/// An item's own refusal, carried back as the typed error of the fan-out.
#[derive(Debug, PartialEq)]
struct Refused(u32);

#[test]
fn the_first_failing_item_stops_the_fan_out_with_its_index_and_its_own_error() -> TestResult {
    let outcome = lgwks_bot::block_on(
        FanOut::new(1_u32..=8)
            .at_most(4)
            .within(Duration::from_secs(5))
            .run(|id| async move {
                if id == 6 {
                    Err(Refused(id))
                } else {
                    Ok(id * 2)
                }
            }),
    );
    match outcome {
        Err(FanOutError::Item { index, error }) => {
            assert_eq!(
                (index, error),
                (5, Refused(6)),
                "the sixth item, with its own error"
            );
            Ok(())
        }
        other => Err(format!("expected the sixth item to fail, got {other:?}").into()),
    }
}

#[test]
fn a_scoped_fan_out_returns_every_value_in_input_order() -> TestResult {
    let scope = Scope::root(Tenant::new("acme")?);
    let doubled = match lgwks_bot::block_on(FanOut::new(1_u32..=8).at_most(4).run_in(
        &scope,
        "doubled",
        |id| async move { Ok::<u32, String>(id.saturating_mul(2)) },
    )) {
        Ok(values) => values,
        Err(error) => return Err(format!("the scoped fan-out failed: {error}").into()),
    };
    assert_eq!(
        doubled,
        vec![2, 4, 6, 8, 10, 12, 14, 16],
        "every item ran under the caller's scope"
    );
    Ok(())
}

#[test]
fn a_remembered_step_returns_its_value_with_no_store_installed() -> TestResult {
    let scope = Scope::root(Tenant::new("acme")?);
    let fetched = lgwks_bot::block_on(remember(&scope, "fetch", || async {
        Ok::<_, FlowError>(41u32)
    }))?;
    assert_eq!(fetched, 41);
    Ok(())
}
