//! Runs into the world (ADR 0006 R3): a host and a supervisor write `admitted`
//! and `settled` through the journal, the world's bound system fires once per
//! settled run, and a restart replays without refiring.
//!
//! The falsifier is the ADR's own row: 100 runs leave 100 `admitted` and 100
//! `settled` events, a system bound to `settled` fires 100 times, and kill and
//! restart replays without refiring. The kill row is a real `SIGKILL` of a
//! child process that wrote the runs through the shipped `FileJournal`, the
//! failure a crash-only design exists for (Candea and Fox, "Crash-Only
//! Software", HotOS 2003).
//!
//! | Test | What it pins |
//! |---|---|
//! | `a_hundred_host_runs_fire_the_bound_system_a_hundred_times_and_a_restart_none` | the ADR row, on a host |
//! | `a_hundred_supervised_tasks_enter_the_world_the_same_way` | the ADR row, on a supervisor |
//! | `a_run_the_world_never_handled_fires_once_after_a_restart` | a settlement with no handled mark fires on reopen, then never again |
//! | `a_feed_that_cannot_record_refuses_the_run_rather_than_hiding_it` | a full journal refuses the run at admission |
//! | `a_killed_writer_leaves_every_settled_run_to_fire_exactly_once` | `SIGKILL` after 50 settlements: reopen fires 50, a second reopen fires 0 |

use std::error::Error;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use lgwks_bot::journal::{FileJournal, MemoryJournal};
use lgwks_bot::rt::supervise::Supervisor;
use lgwks_bot::task::{Disposition, Host, task};
use lgwks_bot::{RunCounts, RunFeed, RunOutcome, SettledRun};

use crate::run_world::{fallible, fire};
use crate::scratch::Scratch;

type TestResult = Result<(), Box<dyn Error>>;

/// The environment variable that turns the kill probe's body on in the child.
const PROBE_JOURNAL: &str = "LGWKS_RUN_EVENTS_PROBE_JOURNAL";

/// A feed over the file journal at `path`, opened as a fresh process would.
fn feed_at(path: &Path) -> Result<RunFeed, Box<dyn Error>> {
    Ok(RunFeed::open(Box::new(FileJournal::open(path)?))?)
}

/// What a feed has recorded: admitted, settled, handled and due.
fn counts(feed: &RunFeed) -> (u64, u64, u64, usize) {
    let RunCounts {
        admitted,
        settled,
        handled,
        due,
        ..
    } = feed.counts();
    (admitted, settled, handled, due)
}

#[test]
fn a_hundred_host_runs_fire_the_bound_system_a_hundred_times_and_a_restart_none() -> TestResult {
    let scratch = Scratch::new("run-events")?;
    let path = scratch.path().join("runs.jrnl");
    let feed = feed_at(&path)?;
    let host = Host::builder("acme")?.runs(feed.clone()).build()?;
    let probe = task("probe", |_scope, input: u64| async move { Ok(input) })?;
    for input in 0..100_u64 {
        let report = lgwks_bot::block_on(host.run(&probe, input));
        assert_eq!(report.disposition(), Disposition::Succeeded, "run {input}");
    }
    assert_eq!(
        counts(&feed),
        (100, 100, 0, 100),
        "100 admitted, 100 settled"
    );

    let fired = fire(&feed)?;
    assert_eq!(
        fired.len(),
        100,
        "the bound system fired once per settled run"
    );
    assert!(
        fired
            .iter()
            .all(|run| run.outcome() == RunOutcome::Succeeded),
        "every run is read as it ended"
    );
    let mut runs: Vec<_> = fired.iter().map(SettledRun::run).collect();
    runs.sort_unstable();
    runs.dedup();
    assert_eq!(runs.len(), 100, "100 distinct runs, none fired twice");
    assert_eq!(counts(&feed), (100, 100, 100, 0), "every run is handled");

    drop((host, feed));
    let restarted = feed_at(&path)?;
    assert_eq!(
        counts(&restarted),
        (100, 100, 100, 0),
        "a restart reads every fact back from the journal"
    );
    assert!(fire(&restarted)?.is_empty(), "and refires nothing");

    let host = Host::builder("acme")?.runs(restarted.clone()).build()?;
    let report = lgwks_bot::block_on(host.run(&probe, 7));
    assert_eq!(
        report.disposition(),
        Disposition::Succeeded,
        "a run after the restart"
    );
    let next = fire(&restarted)?;
    assert_eq!(next.len(), 1, "fires once");
    assert!(
        next.iter().all(|run| !runs.contains(&run.run())),
        "under a run id the first process never issued"
    );
    Ok(())
}

#[test]
fn a_hundred_supervised_tasks_enter_the_world_the_same_way() -> TestResult {
    let scratch = Scratch::new("run-events")?;
    let path = scratch.path().join("runs.jrnl");
    let feed = feed_at(&path)?;
    let work = Arc::new(AtomicU32::new(0));
    lgwks_bot::block_on(async {
        let mut supervisor = Supervisor::new(8).with_runs(feed.clone());
        for _ in 0..100 {
            let work = Arc::clone(&work);
            supervisor
                .spawn(move |_token| async move {
                    work.fetch_add(1, Ordering::SeqCst);
                })
                .await;
        }
        supervisor.wait_idle().await;
    });
    assert_eq!(work.load(Ordering::SeqCst), 100, "every body ran");
    assert_eq!(
        counts(&feed),
        (100, 100, 0, 100),
        "100 admitted, 100 settled"
    );
    assert_eq!(fire(&feed)?.len(), 100, "the bound system fired 100 times");
    drop(feed);
    assert!(
        fire(&feed_at(&path)?)?.is_empty(),
        "a restart refires nothing"
    );
    Ok(())
}

#[test]
fn a_run_the_world_never_handled_fires_once_after_a_restart() -> TestResult {
    let scratch = Scratch::new("run-events")?;
    let path = scratch.path().join("runs.jrnl");
    let feed = feed_at(&path)?;
    let host = Host::builder("acme")?.runs(feed.clone()).build()?;
    let probe = task("probe", fallible)?;
    for fail in [true, false] {
        lgwks_bot::block_on(host.run(&probe, fail));
    }
    drop((host, feed));

    let restarted = feed_at(&path)?;
    assert_eq!(
        counts(&restarted),
        (2, 2, 0, 2),
        "two admitted, two settled, none handled"
    );
    let outcomes: Vec<_> = fire(&restarted)?.iter().map(SettledRun::outcome).collect();
    assert_eq!(
        outcomes,
        [RunOutcome::Failed, RunOutcome::Succeeded],
        "the unhandled settlements fire in the order they settled"
    );
    drop(restarted);
    assert!(fire(&feed_at(&path)?)?.is_empty(), "and never again");
    Ok(())
}

#[test]
fn a_feed_that_cannot_record_refuses_the_run_rather_than_hiding_it() -> TestResult {
    // The in-memory adapter enforces the same ceiling as the file one, and
    // filling it costs no fsync per rung.
    let feed = RunFeed::open(Box::new(MemoryJournal::new()))?;
    let host = Host::builder("acme")?.runs(feed.clone()).build()?;
    let probe = task("probe", |_scope, input: u64| async move { Ok(input) })?;
    let ceiling = u64::try_from(lgwks_bot::journal::MAX_JOURNAL_EVENTS)?;
    let mut refused = None;
    // Bounded by the ceiling itself: every admitted run writes at least one rung.
    for input in 0..=ceiling {
        let report = lgwks_bot::block_on(host.run(&probe, input));
        if report.disposition() != Disposition::Succeeded {
            refused = Some(report);
            break;
        }
    }
    let report = refused.ok_or("the feed never refused a run")?;
    assert_eq!(
        report.disposition(),
        Disposition::Refused,
        "a run the world cannot record is refused: {:?}",
        report.error()
    );
    let (admitted, settled, _, _) = counts(&feed);
    assert_eq!(admitted, settled, "and nothing was admitted for it");
    // A settled, unhandled run holds four rungs; admission reserves the five a
    // run needs, so the refusal comes within one run of the ceiling.
    assert!(
        admitted.saturating_mul(4) > ceiling.saturating_sub(5),
        "the feed filled to within one run of the ceiling, not short of it: {admitted} runs"
    );
    Ok(())
}

/// The child half of the kill row: write 50 runs through a host, mark that
/// they are acknowledged, and park until the parent's `SIGKILL`.
#[test]
fn run_events_kill_probe() -> TestResult {
    let Some(journal) = std::env::var_os(PROBE_JOURNAL) else {
        return Ok(());
    };
    let path = std::path::PathBuf::from(journal);
    let feed = feed_at(&path)?;
    let host = Host::builder("acme")?.runs(feed).build()?;
    let probe = task("probe", |_scope, input: u64| async move { Ok(input) })?;
    for input in 0..50_u64 {
        let report = lgwks_bot::block_on(host.run(&probe, input));
        assert_eq!(report.disposition(), Disposition::Succeeded, "run {input}");
    }
    std::fs::write(path.with_extension("marker"), b"50")?;
    // Parked for the kill, and bounded: a parent that never kills does not
    // leave this child running past ten minutes.
    for _ in 0..600 {
        crate::journal_fixtures::pause(1_000);
    }
    Err("the parent never killed the probe".into())
}

#[cfg(unix)]
#[test]
fn a_killed_writer_leaves_every_settled_run_to_fire_exactly_once() -> TestResult {
    let scratch = Scratch::new("run-events")?;
    let path = scratch.path().join("runs.jrnl");
    let mut command =
        crate::probe_command(&crate::probe_test(module_path!(), "run_events_kill_probe"))?;
    command.env(PROBE_JOURNAL, &path);
    command.stdout(std::process::Stdio::null());
    command.stderr(std::process::Stdio::null());
    let mut guard = crate::journal_fixtures::ProbeGuard(Some(command.spawn()?));
    guard.kill_after_marker(&path.with_extension("marker"), "run_events_kill_probe")?;

    let restarted = feed_at(&path)?;
    assert_eq!(
        counts(&restarted),
        (50, 50, 0, 50),
        "every acknowledged run survived the kill"
    );
    assert_eq!(
        fire(&restarted)?.len(),
        50,
        "each fires once after the kill"
    );
    drop(restarted);
    assert!(
        fire(&feed_at(&path)?)?.is_empty(),
        "and a second restart fires none"
    );
    Ok(())
}
