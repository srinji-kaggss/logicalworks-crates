//! Runs into the world on the shipped binary (ADR 0006 R3).
//!
//! A host and a supervisor share one run feed over a file journal; a bot bound
//! to the feed fires once per settled run on its next tick and reads each run
//! as it ended, and a world bound to the feed reopened over the same file
//! fires nothing. Every number is asserted, so a run the world misses or fires
//! twice fails the smoke.

use std::cell::RefCell;
use std::fmt;
use std::path::Path;
use std::rc::Rc;

use lgwks_bot::journal::FileJournal;
use lgwks_bot::rt::supervise::Supervisor;
use lgwks_bot::script::{FlowError, Scope};
use lgwks_bot::spec::EffectScope;
use lgwks_bot::task::{Host, task};
use lgwks_bot::{Bot, GrantSet, RunCounts, RunFeed, RunOutcome, SettledRun};

/// Runs the host makes, on inputs `0..HOST_RUNS`.
const HOST_RUNS: u64 = 10;
/// The odd inputs among them, which the body fails: 1, 3, 5, 7 and 9.
const ODD_RUNS: u64 = 5;
/// Tasks the supervisor places.
const TASKS: u64 = 10;

/// Why the run-event evening failed.
#[derive(Debug)]
pub(super) enum RunEventsError {
    /// A step the evening needs was refused before anything was counted.
    Setup {
        /// The step, named for the smoke's output.
        step: &'static str,
        /// The refusal the library gave, rendered.
        refusal: String,
    },
    /// A count the world reported is not the count the evening made.
    Deviation(String),
}

impl fmt::Display for RunEventsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Setup { step, ref refusal } => write!(f, "{step} was refused: {refusal}"),
            Self::Deviation(ref what) => f.write_str(what),
        }
    }
}

impl std::error::Error for RunEventsError {}

/// The refusal of `step`, as the evening reports it.
fn setup<E: fmt::Display>(step: &'static str) -> impl FnOnce(E) -> RunEventsError {
    move |refusal| RunEventsError::Setup {
        step,
        refusal: refusal.to_string(),
    }
}

/// A run body that fails on odd inputs.
async fn odd_fails(_scope: Scope, input: u64) -> Result<u64, FlowError> {
    if input & 1 == 1 {
        Err(FlowError::failed(format_args!("{input} is odd")))
    } else {
        Ok(input)
    }
}

/// `Ok` when `held`, else the deviation, logged before it is returned.
fn check(held: bool, deviation: impl FnOnce() -> String) -> Result<(), RunEventsError> {
    if held {
        return Ok(());
    }
    let deviation = deviation();
    lgwks_std::trace::warn!(deviation = deviation.as_str(), "estate-smoke: run events");
    Err(RunEventsError::Deviation(deviation))
}

/// The feed over the journal file at `path`, opened as a fresh process would.
fn open(path: &Path) -> Result<RunFeed, RunEventsError> {
    let journal = FileJournal::open(path).map_err(setup("the run journal"))?;
    RunFeed::open(Box::new(journal)).map_err(setup("the run feed"))
}

/// A world bound to `feed`, ticked once; what it fired.
fn fire(feed: &RunFeed) -> Result<Vec<SettledRun>, RunEventsError> {
    let scope = EffectScope::ephemeral().map_err(setup("the effect scope"))?;
    let mut bot = Bot::builder("smoke-runs")
        .with_effects(scope)
        .build(&GrantSet::empty())
        .map_err(setup("the world"))?;
    let fired = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&fired);
    feed.bind(&mut bot, move |run| sink.borrow_mut().push(run));
    bot.tick().map_err(setup("the world's tick"))?;
    drop(bot);
    Ok(fired.take())
}

/// Prove runs entering the world through a journal under `dir`; the line to
/// print, or the first deviation.
pub(super) fn run(dir: &Path) -> Result<String, RunEventsError> {
    let path = dir.join("runs.jrnl");
    let feed = open(&path)?;
    let host = Host::builder("acme")
        .map_err(setup("the host name"))?
        .runs(feed.clone())
        .build()
        .map_err(setup("the fed host"))?;
    let probe = task("odd-fails", odd_fails).map_err(setup("the task"))?;
    for input in 0..HOST_RUNS {
        lgwks_bot::block_on(host.run(&probe, input));
    }
    lgwks_bot::block_on(async {
        let mut supervisor = Supervisor::new(4).with_runs(feed.clone());
        for _ in 0..TASKS {
            supervisor.spawn(|_token| async {}).await;
        }
        supervisor.wait_idle().await;
    });
    let RunCounts {
        admitted, settled, ..
    } = feed.counts();
    let total = HOST_RUNS.saturating_add(TASKS);
    check(admitted == total && settled == total, || {
        format!("{total} runs left {admitted} admitted and {settled} settled")
    })?;

    let fired = fire(&feed)?;
    let failed = fired
        .iter()
        .filter(|run| run.outcome() == RunOutcome::Failed)
        .count();
    check(u64::try_from(fired.len()) == Ok(total), || {
        format!("the world fired {} of {total} settled runs", fired.len())
    })?;
    check(u64::try_from(failed) == Ok(ODD_RUNS), || {
        format!("the world read {failed} failed runs, not {ODD_RUNS}")
    })?;
    drop((host, feed));

    let refired = fire(&open(&path)?)?;
    check(refired.is_empty(), || {
        format!("a reopened feed refired {} handled runs", refired.len())
    })?;
    Ok(format!(
        "run events: admitted={admitted} settled={settled} fired={} failed={failed} refired={}",
        fired.len(),
        refired.len()
    ))
}
