//! The fire test for #263: a thousand-process tree under the supervisor, stopped
//! three ways, with zero descendants left in the operating system's process
//! table afterwards.
//!
//! The row asks that "stop the process" mean the whole tree at a scale where a
//! signal aimed at what a capture named, or a fork racing a kill, would show.
//! Each test starts one shell that forks a tree three levels deep: ten children,
//! each with ten children, each with ten `sleep` leaves. That is a thousand
//! leaves, a hundred and ten shells between them and the root, so 1,111
//! processes. The leaves all sleep a long, test-unique time, so the table read at
//! the end can tell this tree's survivors from every other process on the host.
//!
//! - **Cancel.** `Supervisor::cancel` while all thousand leaves are up: the task
//!   reports [`TaskOutcome::Cancelled`] with a receipt that names no survivor.
//! - **Shutdown.** `Supervisor::shutdown` over the same tree: the drained report
//!   carries the same cancelled outcome and receipt.
//! - **Drop.** The supervisor dropped with no cancel and no shutdown: the guard in
//!   the task's own frame stops the group, and the table empties.
//!
//! The oracle is the process table (`ps -A -o args=`), read after each stop, and
//! never the supervisor's own receipt: a receipt is the thing under test.
//!
//! # The host's cap
//!
//! The tree needs 1,111 processes beside whatever else the user runs, so the cap
//! that bounds it is the per-user process limit (`ulimit -u`), which each test
//! reports beside its numbers. A host whose cap is below the tree cannot reach a
//! thousand leaves, and the test then fails naming the count it reached and the
//! cap, rather than passing at a smaller tree. The tests take the whole test
//! slot (`.config/nextest.toml`), so no sibling competes for the cap.

#![cfg(all(
    unix,
    feature = "rt",
    feature = "time",
    feature = "sync",
    feature = "process"
))]

use std::io::Write;
use std::time::Duration;

use lgwks_bot::Runtime;
use lgwks_bot::rt::process::ProcessSpec;
use lgwks_bot::rt::supervise::{Supervisor, TaskOutcome};
use lgwks_bot::rt::time::{Instant, sleep};

/// What a test reports when a precondition did not hold.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// How many leaves the tree has: ten shells, each with ten shells, each with ten
/// `sleep` processes.
const LEAVES: usize = 1_000;

/// How long the tree may take to come up, or the table to empty.
const BUDGET: Duration = Duration::from_secs(60);

/// How long the supervisor may take to report the stop of a tree.
const REPORT: Duration = Duration::from_secs(30);

/// How often the table is read while waiting for it to change.
const POLL: Duration = Duration::from_millis(25);

/// The shell script that forks the tree and then waits on it.
///
/// Ten at each of three levels, in functions so each level is a subshell of its
/// parent and the leaves are `sleep` processes whose argument is `token`.
fn tree_script(token: &str) -> String {
    format!(
        "leaf() {{ for i in 1 2 3 4 5 6 7 8 9 10; do sleep {token} & done; wait; }}\n\
         mid() {{ for i in 1 2 3 4 5 6 7 8 9 10; do leaf & done; wait; }}\n\
         for i in 1 2 3 4 5 6 7 8 9 10; do mid & done; wait\n"
    )
}

/// A `sleep` argument no other process on the host has: long, and unique to this
/// test process and variant.
fn token(variant: u32) -> String {
    let nanos = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(elapsed) => elapsed.subsec_nanos(),
        Err(_) => 0,
    };
    format!("86400.{variant}{nanos}")
}

/// A shell spec running `script`.
fn shell(script: &str) -> ProcessSpec {
    let mut spec = ProcessSpec::new("sh");
    spec.arg("-c").arg(script);
    spec
}

/// How many running `sleep` processes carry `token`, from the process table.
fn running(token: &str) -> std::io::Result<usize> {
    let output = std::process::Command::new("ps")
        .args(["-A", "-o", "args="])
        .output()?;
    let wanted = format!("sleep {token}");
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.trim() == wanted)
        .count())
}

/// What `script` prints, for a measurement line; a shell that cannot run is
/// reported in the line itself, since the reading is diagnostic.
fn shell_reading(script: &str) -> String {
    match std::process::Command::new("sh")
        .args(["-c", script])
        .output()
    {
        Ok(output) => String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        Err(error) => format!("unreadable ({error})"),
    }
}

/// The per-user process cap, as the shell reports it.
fn process_cap() -> String {
    shell_reading("ulimit -u")
}

/// This process's resident set in KiB, sampled from the process table.
fn resident_kib() -> String {
    // The shell's parent is this test process.
    shell_reading("ps -o rss= -p $PPID")
}

/// Write one measured line where `--no-capture` shows it.
fn report(line: &str) {
    let mut sink = std::io::stderr().lock();
    if let Err(error) = writeln!(sink, "process_fire: {line}") {
        lgwks_std::trace::debug!(%error, "process_fire: the measurement line was not written");
    }
}

/// Wait until exactly [`LEAVES`] leaves are up, or name how far the tree got.
async fn tree_is_up(token: &str) -> TestResult {
    let started = Instant::now();
    let mut reached = 0;
    while started.elapsed() < BUDGET {
        reached = running(token)?;
        if reached == LEAVES {
            report(&format!(
                "{LEAVES} leaves up in {:?}; ulimit -u = {}; test process rss = {} KiB",
                started.elapsed(),
                process_cap(),
                resident_kib()
            ));
            return Ok(());
        }
        sleep(POLL).await;
    }
    Err(format!(
        "the tree reached {reached} of {LEAVES} leaves in {BUDGET:?}; the per-user process cap is {}",
        process_cap()
    )
    .into())
}

/// Wait for the table to hold no leaf of `token`, and return how long it took.
async fn table_is_empty(token: &str) -> Result<Duration, Box<dyn std::error::Error>> {
    let started = Instant::now();
    loop {
        let left = running(token)?;
        if left == 0 {
            return Ok(started.elapsed());
        }
        if started.elapsed() >= BUDGET {
            break Err(
                format!("{left} of {LEAVES} leaves still run {BUDGET:?} after the stop").into(),
            );
        }
        sleep(POLL).await;
    }
}

/// The task's terminal outcome, reaped from `supervisor`.
async fn outcome(supervisor: &mut Supervisor) -> Result<TaskOutcome, Box<dyn std::error::Error>> {
    let started = Instant::now();
    loop {
        supervisor.reap();
        if let Some(outcome) = supervisor.next_report() {
            return Ok(outcome);
        }
        if started.elapsed() >= REPORT {
            break Err("the supervisor never reported the stopped tree".into());
        }
        sleep(Duration::from_millis(5)).await;
    }
}

/// Assert `outcome` is a cancellation whose receipt names no survivor, and
/// report what the receipt measured.
fn assert_cancelled_clean(outcome: &TaskOutcome, latency: Duration) -> TestResult {
    assert!(
        matches!(*outcome, TaskOutcome::Cancelled { .. }),
        "a stopped tree is reported cancelled, got {outcome:?}"
    );
    let cleanup = outcome
        .cleanup()
        .ok_or("a cancelled process carries a cleanup receipt")?;
    let containment = outcome
        .containment()
        .ok_or("a cancelled process carries a containment report")?;
    assert!(
        cleanup.survivors().is_empty() && containment.survivors().is_empty(),
        "the receipt names survivors of a tree it stopped: {cleanup:?} {containment:?}"
    );
    report(&format!(
        "stop to report {latency:?}; receipt {cleanup:?}; mechanism {:?}; captured {}; signalled {}; complete {}",
        containment.mechanism(),
        containment.captured(),
        containment.signalled(),
        containment.is_complete()
    ));
    Ok(())
}

/// A supervisor holding one running thousand-leaf tree named by `token`.
async fn supervising_a_tree(token: &str) -> Result<Supervisor, Box<dyn std::error::Error>> {
    let mut supervisor = Supervisor::default();
    supervisor
        .spawn_process(&shell(&tree_script(token)))
        .await?;
    tree_is_up(token).await?;
    Ok(supervisor)
}

#[test]
fn a_cancelled_thousand_process_tree_leaves_no_survivor() -> TestResult {
    let token = token(1);
    let runtime = Runtime::new()?;
    runtime.block_on(async {
        let mut supervisor = supervising_a_tree(&token).await?;
        let started = Instant::now();
        supervisor.cancel();
        let outcome = outcome(&mut supervisor).await?;
        assert_cancelled_clean(&outcome, started.elapsed())?;
        let drained = table_is_empty(&token).await?;
        report(&format!("cancel: table empty {drained:?} after the report"));
        assert_eq!(running(&token)?, 0, "zero descendants survive a cancel");
        Ok(())
    })
}

#[test]
fn a_shut_down_thousand_process_tree_leaves_no_survivor() -> TestResult {
    let token = token(2);
    let runtime = Runtime::new()?;
    runtime.block_on(async {
        let supervisor = supervising_a_tree(&token).await?;
        let started = Instant::now();
        let drained = supervisor.shutdown().await;
        let latency = started.elapsed();
        let outcomes = drained.outcomes();
        assert_eq!(
            outcomes.len(),
            1,
            "one process task drains exactly one outcome"
        );
        let Some(outcome) = outcomes.first() else {
            return Err("the drained report holds the tree's outcome".into());
        };
        assert_cancelled_clean(outcome, latency)?;
        let emptied = table_is_empty(&token).await?;
        report(&format!(
            "shutdown: table empty {emptied:?} after the report"
        ));
        assert_eq!(running(&token)?, 0, "zero descendants survive a shutdown");
        Ok(())
    })
}

#[test]
fn a_dropped_supervisor_stops_a_thousand_process_tree() -> TestResult {
    let token = token(3);
    let runtime = Runtime::new()?;
    runtime.block_on(async {
        let supervisor = supervising_a_tree(&token).await?;
        let started = Instant::now();
        drop(supervisor);
        let emptied = table_is_empty(&token).await?;
        report(&format!(
            "drop: table empty {emptied:?} after the drop ({:?} from the drop call)",
            started.elapsed()
        ));
        assert_eq!(running(&token)?, 0, "zero descendants survive a drop");
        Ok(())
    })
}
