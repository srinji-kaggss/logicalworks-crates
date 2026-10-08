//! Oracle for the `capture` task. One test per contract clause.
//!
//! Unix-only: every clause forks `sh` grandchildren and reads the process
//! table to prove they died. The solution itself is portable — only the
//! observation needs Unix.
//!
//! Bounded: the 70 MB flood is a fast pipe, every kill is followed by a
//! bounded `pgrep` poll, and the longest single wait is the 300 ms deadline
//! clause. No clause can hang the matrix: the harness's own oracle timeout is
//! the outer bound.

#![cfg(unix)]

use std::time::{Duration, Instant};

use ai_trial::{CaptureError, solve};
use lgwks_bot::rt::runtime::Runtime;

/// A deadline no success path reaches.
const LONG: Duration = Duration::from_secs(30);

/// The runtime every test drives its futures on.
fn runtime() -> Result<Runtime, std::io::Error> {
    Runtime::new()
}

/// Whether any process command line still matches `pattern`.
///
/// `pgrep -f` returning nothing (exit 1) is the observation, not an error: it
/// is how the table says the pattern is gone.
fn pattern_present(pattern: &str) -> Result<bool, Box<dyn std::error::Error>> {
    let output = std::process::Command::new("pgrep")
        .args(["-f", pattern])
        .output()?;
    Ok(output.status.success())
}

/// Wait until no command line matches `pattern`, or the bound runs out.
fn pattern_gone(pattern: &str, bound: Duration) -> Result<bool, Box<dyn std::error::Error>> {
    let start = Instant::now();
    loop {
        if !pattern_present(pattern)? {
            return Ok(true);
        }
        if start.elapsed() >= bound {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Zombie processes carrying `marker` in their command line.
///
/// Scoped to the marker rather than counted machine-wide: a shared machine
/// may already hold zombies that are not this test's to reap, and sibling
/// trials may fork while this one reads the table.
fn marker_zombies(marker: &str) -> Result<usize, Box<dyn std::error::Error>> {
    let output = std::process::Command::new("ps")
        .args(["-e", "-o", "stat=,command="])
        .output()?;
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text
        .lines()
        .filter(|line| line.contains(marker))
        .filter(|line| line.trim_start().starts_with('Z'))
        .count())
}

/// A process marker unique to this test process.
///
/// Sibling trials on a shared machine fork their own sleeps; a bare `sleep
/// 30` pattern would match theirs. `exec -a` renames the sleep's own argv[0]
/// to the marker, and the pid keeps parallel trials apart.
fn marker(tag: &str) -> String {
    format!("lgwkscap-{tag}-{}", std::process::id())
}

fn shell(script: &str) -> Vec<String> {
    vec!["sh".to_owned(), "-c".to_owned(), script.to_owned()]
}

#[test]
fn a_small_output_returns_exact_head_and_total() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = runtime()?;
    let result = runtime.block_on(solve(shell("printf hello-capture"), 4096, LONG));
    match result {
        Ok(outcome) => {
            assert_eq!(outcome.head(), b"hello-capture", "the exact head bytes");
            assert_eq!(outcome.total_bytes(), 13, "the exact total");
            assert!(!outcome.truncated(), "nothing was cut");
        }
        Err(error) => return Err(format!("expected Ok, got {error:?}").into()),
    }
    Ok(())
}

#[test]
fn a_flood_past_sixty_four_mebibytes_stays_within_its_ceiling(
) -> Result<(), Box<dyn std::error::Error>> {
    let runtime = runtime()?;
    const TOTAL: u64 = 70_000_000;
    const LIMIT: usize = 4096;
    let result = runtime.block_on(solve(shell("head -c 70000000 /dev/zero"), LIMIT, LONG));
    match result {
        Ok(outcome) => {
            assert_eq!(outcome.head().len(), LIMIT, "retained bytes never exceed the ceiling");
            assert!(
                outcome.head().iter().all(|byte| *byte == 0),
                "the retained head is the stream's head"
            );
            assert_eq!(outcome.total_bytes(), TOTAL, "the exact total however much was cut");
            assert!(outcome.truncated(), "seventy megabytes past a four-kibibyte ceiling is truncated");
        }
        Err(error) => return Err(format!("expected Ok, got {error:?}").into()),
    }
    Ok(())
}

#[test]
fn an_unstartable_program_is_a_typed_spawn_refusal() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = runtime()?;
    match runtime.block_on(solve(Vec::new(), 4096, LONG)) {
        Err(CaptureError::Spawn(_)) => {}
        other => return Err(format!("an empty argv starts nothing: {other:?}").into()),
    }
    match runtime.block_on(solve(vec!["definitely-not-a-program-xyz".to_owned()], 4096, LONG)) {
        Err(CaptureError::Spawn(_)) => {}
        other => return Err(format!("a missing program never ran: {other:?}").into()),
    }
    Ok(())
}

#[test]
fn a_deadline_stops_the_whole_group() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = runtime()?;
    let mark = marker("deadline30");
    let script = format!("exec -a {mark} sleep 30");
    match runtime.block_on(solve(shell(&script), 4096, Duration::from_millis(300))) {
        Err(CaptureError::Deadline) => {}
        other => return Err(format!("a 300 ms deadline on a 30 s sleep is Deadline, got {other:?}").into()),
    }
    assert!(
        pattern_gone(&mark, Duration::from_secs(5))?,
        "the deadline kill reaps the whole group"
    );
    Ok(())
}

#[test]
fn a_completed_run_leaves_no_orphan_behind() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = runtime()?;
    let mark = marker("grand45");
    // The background sleep outlives its parent and writes nowhere: the only
    // thing that can stop it is the supervisor's own cleanup at run end.
    let script = format!("sh -c \"exec -a {mark} sleep 45\" >/dev/null 2>&1 & echo done");
    let result = runtime.block_on(solve(shell(&script), 4096, LONG));
    match result {
        Ok(outcome) => assert_eq!(outcome.head(), b"done\n", "the parent's own output"),
        Err(error) => return Err(format!("expected Ok, got {error:?}").into()),
    }
    assert!(
        pattern_gone(&mark, Duration::from_secs(5))?,
        "the orphaned grandchild is stopped by the run's cleanup, not left running"
    );
    Ok(())
}

#[test]
fn cancelling_leaves_no_descendant() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = runtime()?;
    let mark = marker("cancel59");
    let script = format!("exec -a {mark} sleep 59 & wait");
    // Park the solve future on a 59-second sleep tree, then drop it: the drop
    // is the cancellation, and what the table says afterwards is the verdict.
    // The round must also be prompt: an implementation that blocks through the
    // sleep takes nearly a minute to report nothing.
    let start = Instant::now();
    runtime.block_on(async {
        let run = solve(shell(&script), 4096, LONG);
        let _ = lgwks_bot::rt::time::timeout(Duration::from_millis(200), run).await;
    });
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(10),
        "cancellation stops the tree promptly instead of blocking through the sleep: took {elapsed:?}"
    );
    assert!(
        pattern_gone(&mark, Duration::from_secs(5))?,
        "dropping the future stops children and grandchildren alike"
    );
    assert_eq!(
        marker_zombies(&mark)?,
        0,
        "no descendant remains, zombie or otherwise"
    );
    Ok(())
}
