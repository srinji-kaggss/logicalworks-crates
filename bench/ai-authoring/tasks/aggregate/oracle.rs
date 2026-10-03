//! Oracle for the `aggregate` task. One test per contract clause.
//!
//! Deterministic: no counter is asserted on a wall-clock sleep. The only waits
//! are the short ones that let a fetched future begin, plus the 200 ms the drop
//! clause waits for a cancelled fetch to be counted out.

use std::time::Duration;

use ai_task_support::Fetcher;
use ai_trial::{SolveError, solve};
use lgwks_bot::rt::runtime::Runtime;
use lgwks_bot::rt::time;

/// A deadline long enough that no test's success path reaches it.
const LONG: Duration = Duration::from_secs(30);

/// The runtime every test drives its futures on.
fn runtime() -> Result<Runtime, std::io::Error> {
    Runtime::new()
}

/// The expected sum of `0..n` fetched values: `fetch(id) == id * 3`.
fn expected_sum(n: u32) -> u64 {
    (0..n).map(|id| u64::from(id).saturating_mul(3)).sum()
}

#[test]
fn empty_input_sums_to_zero() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = runtime()?;
    let result = runtime.block_on(solve(Vec::new(), Fetcher::new(1), LONG));
    match result {
        Ok(sum) => assert_eq!(sum, 0, "an empty input is a zero sum, not an error"),
        Err(error) => return Err(format!("expected Ok(0), got {error:?}").into()),
    }
    Ok(())
}

#[test]
fn at_most_four_fetches_are_in_flight() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = runtime()?;
    let fetch = Fetcher::new(7).uniform_delay(Duration::from_millis(15));
    let ids: Vec<u32> = (0..12).collect();
    let result = runtime.block_on(solve(ids, fetch.clone(), LONG));
    match result {
        Ok(sum) => assert_eq!(sum, expected_sum(12), "the sum of every fetched value"),
        Err(error) => return Err(format!("expected Ok, got {error:?}").into()),
    }
    let stats = fetch.stats();
    assert!(
        stats.max_live <= 4,
        "at most 4 fetches in flight, observed {}",
        stats.max_live
    );
    assert_eq!(stats.started, 12, "every id was fetched exactly once");
    Ok(())
}

#[test]
fn a_failure_is_reported_by_id() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = runtime()?;
    let fetch = Fetcher::new(11)
        .uniform_delay(Duration::from_millis(15))
        .failing([3]);
    let ids: Vec<u32> = (0..12).collect();
    let result = runtime.block_on(solve(ids, fetch, LONG));
    match result {
        Err(SolveError::Fetch { id }) => assert_eq!(id, 3, "the error names the failing id"),
        other => return Err(format!("expected Fetch{{id: 3}}, got {other:?}").into()),
    }
    Ok(())
}

#[test]
fn no_fetch_starts_after_the_failure_is_observed() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = runtime()?;
    // The failing id resolves first; every other fetch is slow, so any fetch
    // that starts after the failure had to be observed is visible in the count.
    let fetch = Fetcher::new(13)
        .uniform_delay(Duration::from_millis(60))
        .delay(3, Duration::from_millis(1))
        .failing([3]);
    let ids: Vec<u32> = (0..12).collect();
    let result = runtime.block_on(solve(ids, fetch.clone(), LONG));
    assert!(
        matches!(result, Err(SolveError::Fetch { .. })),
        "the run fails on the configured failure"
    );
    let stats = fetch.stats();
    assert_eq!(
        stats.started_after_first_failure, 0,
        "no fetch starts after the failure is observed"
    );
    Ok(())
}

#[test]
fn the_overall_deadline_is_honoured() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = runtime()?;
    let fetch = Fetcher::new(17).uniform_delay(Duration::from_secs(5));
    let ids: Vec<u32> = (0..12).collect();
    let result = runtime.block_on(solve(ids, fetch, Duration::from_millis(50)));
    match result {
        Err(SolveError::Deadline) => {}
        other => return Err(format!("expected Deadline, got {other:?}").into()),
    }
    Ok(())
}

#[test]
fn dropping_the_future_leaves_no_fetch_live() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = runtime()?;
    let fetch = Fetcher::new(19).uniform_delay(Duration::from_secs(5));
    let ids: Vec<u32> = (0..12).collect();
    let fetcher = fetch.clone();
    // Poll the solve future briefly so fetches are actually in flight, then let
    // the timeout drop it and give a cancelled fetch time to be counted out.
    let stats = runtime.block_on(async move {
        let _ = time::timeout(Duration::from_millis(50), solve(ids, fetcher, LONG)).await;
        time::sleep(Duration::from_millis(200)).await;
        fetch.stats()
    });
    assert_eq!(
        stats.live, 0,
        "no fetch is still live 200 ms after the future was dropped"
    );
    Ok(())
}
