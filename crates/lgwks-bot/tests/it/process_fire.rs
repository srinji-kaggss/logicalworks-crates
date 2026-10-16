//! The fire test for #263: a thousand supervised process trees, cancelled
//! together at 100 and at 1,000 in flight, with zero descendants left in the
//! operating system's process table after each round.
//!
//! "Stop the process" has to mean the whole tree, and a build tool, a test
//! runner or an agent's shell command forks grandchildren (Keel's migration hit
//! this when `Child::kill` left a shell's grandchildren alive; systemd answers
//! it with a cgroup and `cgroup.kill`, the supervisor with its containment
//! mechanisms). This test puts the promise under the load where a signal aimed
//! at what a capture named, or a fork racing a kill, would show.
//!
//! Each tree is three generations deep: a root `sh` that starts a middle `sh`
//! that starts a `sleep` leaf, and the leaf's argument is a token no other
//! process on the host carries, so the table read afterwards tells this tree's
//! survivors from everything else. A tier supervises [`TREES`] trees in all, in
//! rounds of its in-flight level: each round starts that many trees under one
//! supervisor, waits until every leaf is up, cancels them together, and reads
//! the table until it holds no leaf of the round.
//!
//! The oracle is `/proc`, read by this file, and never the supervisor's own
//! receipt: the receipt is the thing under test. What is reported, one line per
//! tier, is what the issue names: the cancel-to-report latency of every tree at
//! p50, p95 and p99, the mechanisms the receipts named, how long the table took
//! to empty, and the test process's peak resident set.
//!
//! # The host's cap
//!
//! A thousand trees in flight is three thousand processes beside everything
//! else on the host, so the bound is the per-user process limit or the
//! container's pid limit, both of which each tier reports. A host that refuses
//! a start is not a failure of the tier: the round carries on with the trees it
//! got, later rounds ask for that level, and the line names the level reached.
//! A host that cannot reach the 100 tier's level fails, naming the cap.
//!
//! # Where it runs
//!
//! Linux only, and in the Linux container leg of CI: a thousand trees in flight
//! would take the per-user process table of a shared macOS runner away from
//! every other job on it, while the container has a pid namespace of its own.
//! The tests take the whole test slot (`.config/nextest.toml`) so no sibling
//! competes for the cap, and they print their numbers on success there too.

#![cfg(all(
    target_os = "linux",
    feature = "rt",
    feature = "time",
    feature = "sync",
    feature = "process"
))]

use std::collections::BTreeMap;
use std::io::Write;
use std::time::Duration;

use lgwks_bot::Runtime;
use lgwks_bot::rt::process::ProcessSpec;
use lgwks_bot::rt::supervise::{CleanupReceipt, Supervisor, TaskOutcome};
use lgwks_bot::rt::time::{Instant, sleep};

/// What a test reports when a precondition did not hold.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// How many trees a tier supervises in all.
const TREES: usize = 1_000;

/// The in-flight level every tier must reach, however low the host's cap: the
/// smaller tier's own.
const FLOOR: usize = 100;

/// How long a round's trees may take to come up, or the table to empty.
const BUDGET: Duration = Duration::from_secs(120);

/// How long the supervisor may take to report every stopped tree of a round.
const REPORT: Duration = Duration::from_secs(60);

/// How often the table is read while waiting for it to change.
const POLL: Duration = Duration::from_millis(25);

/// How often the supervisor's reports are drained while waiting for them, fine
/// enough that the latency reads the cleanup and not this loop.
const REPORT_POLL: Duration = Duration::from_millis(1);

/// The errno a read of `/proc/<pid>/...` returns when the process exited
/// between the directory listing and the read.
const ESRCH: i32 = 3;

/// A shell spec running `script`.
fn shell(script: &str) -> ProcessSpec {
    let mut spec = ProcessSpec::new("sh");
    spec.arg("-c").arg(script);
    spec
}

/// The script of one tree: a root shell, a middle shell and a `sleep` leaf.
///
/// The leaf's argument is `token`, which no other process of the host carries.
fn tree_script(token: &str) -> String {
    format!("sh -c 'sleep {token} & wait' & wait")
}

/// A `sleep` argument no other process of the host carries: sixty-four random
/// bits from the estate's entropy source, as a fixed-width fraction of a day.
///
/// A process id would not do, because the OS reuses it and a leaf left by an
/// earlier run could then read as this round's own.
fn token() -> Result<String, Box<dyn std::error::Error>> {
    let draw = u64::from_le_bytes(lgwks_std::random::bytes::<8>()?);
    Ok(format!("86400.{draw:020}"))
}

/// How many running `sleep` leaves carry `token`, read from `/proc`.
fn running(token: &str) -> std::io::Result<usize> {
    let program: &[u8] = b"sleep";
    let mut found: usize = 0;
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let name = entry.file_name();
        if !name.as_encoded_bytes().iter().all(u8::is_ascii_digit) {
            continue;
        }
        let bytes = match std::fs::read(entry.path().join("cmdline")) {
            Ok(bytes) => bytes,
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    || error.raw_os_error() == Some(ESRCH) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        let mut args = bytes.split(|byte| *byte == 0).filter(|arg| !arg.is_empty());
        if args.next() == Some(program)
            && args.next() == Some(token.as_bytes())
            && args.next().is_none()
        {
            found = found.saturating_add(1);
        }
    }
    Ok(found)
}

/// Wait until exactly `want` leaves of `token` are up, or name how far it got.
async fn leaves_are_up(token: &str, want: usize) -> TestResult {
    let started = Instant::now();
    let mut reached = 0;
    while started.elapsed() < BUDGET {
        reached = running(token)?;
        if reached == want {
            return Ok(());
        }
        sleep(POLL).await;
    }
    Err(format!(
        "{reached} of {want} leaves came up in {BUDGET:?}; the per-user process cap is {}, the pid limit is {}",
        process_cap(),
        pid_limit()
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
            break Err(format!("{left} leaves still run {BUDGET:?} after the stop").into());
        }
        sleep(POLL).await;
    }
}

/// The line of `path` that starts with `prefix`, trimmed, for a measurement
/// line; a file that cannot be read is named in the line itself, since the
/// reading is diagnostic.
fn proc_line(path: &str, prefix: &str) -> String {
    match std::fs::read_to_string(path) {
        Ok(text) => match text.lines().find(|line| line.starts_with(prefix)) {
            Some(line) => line.trim().to_owned(),
            None => format!("{prefix} not listed"),
        },
        Err(error) => format!("{path} unreadable ({error})"),
    }
}

/// The per-user process cap this process runs under, as `/proc` reports it.
fn process_cap() -> String {
    proc_line("/proc/self/limits", "Max processes")
}

/// This process's peak resident set, as `/proc` reports it.
fn peak_rss() -> String {
    proc_line("/proc/self/status", "VmHWM:")
}

/// The container's pid limit, as the cgroup reports it.
fn pid_limit() -> String {
    match std::fs::read_to_string("/sys/fs/cgroup/pids.max") {
        Ok(limit) => limit.trim().to_owned(),
        Err(error) => format!("unreadable ({error})"),
    }
}

/// The `pct`-th percentile of `sorted`, by nearest rank; zero for no samples.
fn percentile(sorted: &[Duration], pct: usize) -> Duration {
    let rank = sorted.len().saturating_mul(pct).div_ceil(100).max(1);
    match sorted.get(rank.saturating_sub(1)) {
        Some(found) => *found,
        None => Duration::ZERO,
    }
}

/// Write one measured line where `--no-capture` and a success-output override
/// show it.
fn report(line: &str) {
    let mut sink = std::io::stderr().lock();
    if let Err(error) = writeln!(sink, "process_fire: {line}") {
        lgwks_std::trace::debug!(%error, "process_fire: the measurement line was not written");
    }
}

/// Assert `outcome` is a cancellation whose receipt settled with no survivor,
/// and name the mechanism that stopped the tree.
fn check_stopped(outcome: &TaskOutcome) -> Result<String, Box<dyn std::error::Error>> {
    assert!(
        matches!(*outcome, TaskOutcome::Cancelled { .. }),
        "a stopped tree is reported cancelled, got {outcome:?}"
    );
    let cleanup = outcome
        .cleanup()
        .ok_or("a cancelled process carries a cleanup receipt")?;
    assert_eq!(
        cleanup,
        &CleanupReceipt::CleanupConfirmed,
        "the receipt of a tree the supervisor stopped settles confirmed"
    );
    let containment = outcome
        .containment()
        .ok_or("a cancelled process carries a containment report")?;
    assert!(
        containment.survivors().is_empty(),
        "the receipt names survivors of a tree it stopped: {containment:?}"
    );
    Ok(format!("{:?}", containment.mechanism()))
}

/// What one round measured.
struct Round {
    /// How many trees the host let the round start.
    started: usize,
    /// Cancel-to-report latency of every tree, in the order the reports came.
    latencies: Vec<Duration>,
    /// How many trees each containment mechanism stopped, by name.
    mechanisms: BTreeMap<String, usize>,
    /// How long the table took to empty after the last report.
    emptied: Duration,
}

/// Start up to `want` trees under one supervisor, cancel them together once
/// every leaf is up, and verify from the table that none of them survives.
async fn round(want: usize, token: &str) -> Result<Round, Box<dyn std::error::Error>> {
    let mut supervisor = Supervisor::new(want);
    let spec = shell(&tree_script(token));
    let mut started: usize = 0;
    for _ in 0..want {
        match supervisor.spawn_process(&spec).await {
            Ok(_task) => started = started.saturating_add(1),
            Err(error) => {
                report(&format!(
                    "the host refused tree {} of {want}: {error}",
                    started.saturating_add(1)
                ));
                break;
            }
        }
    }
    leaves_are_up(token, started).await?;
    let cancelled = Instant::now();
    supervisor.cancel();
    let mut latencies = Vec::with_capacity(started);
    let mut mechanisms: BTreeMap<String, usize> = BTreeMap::new();
    while latencies.len() < started {
        supervisor.reap();
        while let Some(outcome) = supervisor.next_report() {
            latencies.push(cancelled.elapsed());
            let count = mechanisms.entry(check_stopped(&outcome)?).or_default();
            *count = count.saturating_add(1);
        }
        if cancelled.elapsed() >= REPORT {
            return Err(format!(
                "{} of {started} stopped trees were reported in {REPORT:?}",
                latencies.len()
            )
            .into());
        }
        sleep(REPORT_POLL).await;
    }
    let emptied = table_is_empty(token).await?;
    assert_eq!(running(token)?, 0, "zero descendants survive the cancel");
    Ok(Round {
        started,
        latencies,
        mechanisms,
        emptied,
    })
}

/// One tier: [`TREES`] trees at `level` in flight, in rounds, each verified, and
/// one line of what the tier measured.
fn tier(level: usize) -> TestResult {
    let runtime = Runtime::new()?;
    runtime.block_on(async {
        let mut ask = level;
        let mut total: usize = 0;
        let mut rounds: usize = 0;
        let mut slowest_empty = Duration::ZERO;
        let mut latencies: Vec<Duration> = Vec::with_capacity(TREES);
        let mut mechanisms: BTreeMap<String, usize> = BTreeMap::new();
        while total < TREES {
            let want = ask.min(TREES.saturating_sub(total));
            let done = round(want, &token()?).await?;
            assert!(
                done.started > 0,
                "the host started no tree at all; the per-user process cap is {}, the pid limit is {}",
                process_cap(),
                pid_limit()
            );
            ask = ask.min(done.started.max(1));
            total = total.saturating_add(done.started);
            rounds = rounds.saturating_add(1);
            slowest_empty = slowest_empty.max(done.emptied);
            latencies.extend(done.latencies);
            for (name, count) in done.mechanisms {
                let sum = mechanisms.entry(name).or_default();
                *sum = sum.saturating_add(count);
            }
        }
        assert!(
            ask >= FLOOR.min(level),
            "the host reached {ask} trees in flight, below the {FLOOR} this test requires; the per-user process cap is {}, the pid limit is {}",
            process_cap(),
            pid_limit()
        );
        latencies.sort_unstable();
        report(&format!(
            "in flight {level}: reached {ask}; {total} trees in {rounds} rounds, all stopped, zero survivors; \
             cancel to report p50 {:?} p95 {:?} p99 {:?} max {:?}; table empty at most {slowest_empty:?} after the last report; \
             mechanisms {mechanisms:?}; peak rss: {}; {}; pid limit: {}",
            percentile(&latencies, 50),
            percentile(&latencies, 95),
            percentile(&latencies, 99),
            percentile(&latencies, 100),
            peak_rss(),
            process_cap(),
            pid_limit()
        ));
        Ok(())
    })
}

#[test]
fn a_thousand_trees_cancelled_at_in_flight_100_leave_no_survivor() -> TestResult {
    tier(100)
}

#[test]
fn a_thousand_trees_cancelled_at_in_flight_1000_leave_no_survivor() -> TestResult {
    tier(1_000)
}
