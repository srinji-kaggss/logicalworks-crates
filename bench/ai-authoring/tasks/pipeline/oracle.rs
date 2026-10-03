//! Oracle for the `pipeline` task. One test per contract clause.
//!
//! Deterministic: no counter is asserted on a wall-clock sleep. The only waits
//! are the short ones that let a stage begin, plus the 200 ms the drop clause
//! waits for a cancelled stage to be counted out.

use std::time::Duration;

use ai_task_support::{Stage, StageName};
use ai_trial::{PipelineError, solve};
use lgwks_bot::rt::runtime::Runtime;
use lgwks_bot::rt::time;

/// A deadline long enough that no test's success path reaches it.
const LONG: Duration = Duration::from_secs(30);

/// The value `Stage` publishes when every stage succeeds: `(11 + 22) * 2`.
const PUBLISHED: u64 = 66;

/// The runtime every test drives its futures on.
fn runtime() -> Result<Runtime, std::io::Error> {
    Runtime::new()
}

#[test]
fn success_publishes_the_combined_artifact() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = runtime()?;
    let stage = Stage::new(1);
    let result = runtime.block_on(solve(stage.clone(), LONG));
    match result {
        Ok(published) => assert_eq!(published.value(), PUBLISHED, "the published value"),
        Err(error) => return Err(format!("expected Ok, got {error:?}").into()),
    }
    let stats = stage.stats();
    assert_eq!(stats.runs(StageName::FetchA), 1, "fetch_a ran once");
    assert_eq!(stats.runs(StageName::FetchB), 1, "fetch_b ran once");
    assert_eq!(stats.runs(StageName::Combine), 1, "combine ran once");
    assert_eq!(stats.runs(StageName::Publish), 1, "publish ran once");
    Ok(())
}

#[test]
fn a_failed_fetch_a_stops_combine_and_publish() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = runtime()?;
    let stage = Stage::new(3).failing([StageName::FetchA]);
    let result = runtime.block_on(solve(stage.clone(), LONG));
    match result {
        Err(PipelineError::Stage { name }) => {
            assert_eq!(name, StageName::FetchA, "the error names fetch_a");
        }
        other => return Err(format!("expected Stage{{FetchA}}, got {other:?}").into()),
    }
    let stats = stage.stats();
    assert_eq!(
        stats.runs(StageName::Combine),
        0,
        "combine never runs when fetch_a fails"
    );
    assert_eq!(
        stats.runs(StageName::Publish),
        0,
        "publish never runs when fetch_a fails"
    );
    Ok(())
}

#[test]
fn a_failed_fetch_b_stops_combine_and_publish() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = runtime()?;
    let stage = Stage::new(5).failing([StageName::FetchB]);
    let result = runtime.block_on(solve(stage.clone(), LONG));
    match result {
        Err(PipelineError::Stage { name }) => {
            assert_eq!(name, StageName::FetchB, "the error names fetch_b");
        }
        other => return Err(format!("expected Stage{{FetchB}}, got {other:?}").into()),
    }
    let stats = stage.stats();
    assert_eq!(
        stats.runs(StageName::Combine),
        0,
        "combine never runs when fetch_b fails"
    );
    assert_eq!(
        stats.runs(StageName::Publish),
        0,
        "publish never runs when fetch_b fails"
    );
    Ok(())
}

#[test]
fn a_slow_stage_past_the_deadline_publishes_nothing() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = runtime()?;
    let stage = Stage::new(7).slow(StageName::FetchA, Duration::from_secs(5));
    let result = runtime.block_on(solve(stage.clone(), Duration::from_millis(50)));
    match result {
        Err(PipelineError::Deadline) => {}
        other => return Err(format!("expected Deadline, got {other:?}").into()),
    }
    let stats = stage.stats();
    assert_eq!(
        stats.runs(StageName::Publish),
        0,
        "publish never runs after the deadline"
    );
    Ok(())
}

#[test]
fn dropping_the_future_leaves_no_stage_live() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = runtime()?;
    let stage = Stage::new(9).slow(StageName::FetchA, Duration::from_secs(5));
    let stage_for_run = stage.clone();
    // Poll the solve future briefly so a stage is actually running, then let the
    // timeout drop it and give a cancelled stage time to be counted out.
    let stats = runtime.block_on(async move {
        let _ = time::timeout(Duration::from_millis(50), solve(stage_for_run, LONG)).await;
        time::sleep(Duration::from_millis(200)).await;
        stage.stats()
    });
    assert_eq!(
        stats.live, 0,
        "no stage is still live 200 ms after the future was dropped"
    );
    Ok(())
}
