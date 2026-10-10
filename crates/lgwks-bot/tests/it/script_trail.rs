//! A run reads back as its script (SL-6, #386).
//!
//! Every step a `script!` line enters carries that line into the run's trail
//! and settles there with its outcome, so `Report::script_trail` renders a
//! finished run as the script's own lines in the order they ran. These tests
//! drive real flows through a real `Host` and assert the rendering line by
//! line; the line numbers are this file's own.

#![cfg(feature = "script")]

use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use lgwks_bot::rt::time::sleep;
use lgwks_bot::script::{Outcome, Scope, StepKind, Tenant};
use lgwks_bot::task::{Disposition, Host, task};

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

lgwks_bot::script! {
    /// Fetch through two busy answers, then wait on a call that never answers.
    flow fetch_then_wait(busy: &AtomicU32) -> u32:
        let fetched = retry up to 3 times, waiting 1ms:
            if busy.fetch_add(1, Ordering::SeqCst) < 2:
                fail transiently with "busy"
            7
        step wait:
            within 20ms:
                sleep(Duration::from_secs(5)).await
        give back fetched

    /// Two sequential rungs, each with a fan-out of two.
    flow rungs() -> usize:
        let mut total = 0_usize
        for rung in 0..2:
            let done = each item in 0..2:
                usize::saturating_add(item, rung)
            total = total.saturating_add(done.len())
        give back total

    /// Walk `count` sequential rungs and nothing else.
    flow ladder(count: u32) -> u32:
        for rung in 0..count:
            step climb:
                rung
        give back count

    /// The key of the one step inside.
    flow keyed() -> String:
        step inner:
            scope.key().to_hex()
}

/// A host whose trail keeps `capacity` entries and whose runs have time to
/// spare, so only the flow decides how a run ends.
fn host(capacity: usize) -> Result<Host, Box<dyn Error>> {
    Ok(Host::builder("acme")?
        .progress_capacity(capacity)
        .default_deadline(Duration::from_secs(30))
        .build()?)
}

/// The rendered trail, one string per line.
fn lines(rendered: &str) -> Vec<&str> {
    rendered.lines().collect()
}

#[test]
fn a_run_through_a_retry_and_a_timeout_reads_back_line_by_line() -> TestResult {
    let host = host(64)?;
    let fetching = task(
        "fetching",
        |scope: Scope, busy: Arc<AtomicU32>| async move { fetch_then_wait(&scope, &busy).await },
    )?;
    let report = lgwks_bot::block_on(host.run(&fetching, Arc::new(AtomicU32::new(0))));
    assert_eq!(report.disposition(), Disposition::DeadlineExceeded);

    let rendered = report.script_trail().to_string();
    assert_eq!(
        lines(&rendered),
        [
            "   - | fetching  -> timed out after 20ms",
            "  25 |   flow fetch_then_wait(busy: &AtomicU32) -> u32  -> timed out after 20ms",
            "  26 |     retry up to 3 times, waiting 1ms  -> retried 2, then ok",
            "  30 |     step wait  -> timed out after 20ms",
            "  31 |       within 20ms  -> timed out after 20ms",
        ],
        "each entered step, its line, its nesting and its outcome:\n{rendered}"
    );

    let retry = report
        .trail()
        .get(2)
        .ok_or("the retry is the third entry")?;
    assert_eq!(retry.path(), "fetching/fetch_then_wait/retry");
    assert_eq!(retry.outcome(), Some(&Outcome::Retried { attempts: 3 }));
    let site = retry.site().ok_or("a script line entered the retry")?;
    assert_eq!(
        (site.kind(), site.line(), site.text()),
        (StepKind::Retry, 26, "retry up to 3 times, waiting 1ms"),
        "the site is the word, the line and the text"
    );
    let paths: Vec<&str> = report.trail().iter().map(|entry| entry.path()).collect();
    let steps: Vec<&str> = report.steps().iter().map(|step| &**step).collect();
    assert_eq!(paths, steps, "the trail and the path list are one ring");
    Ok(())
}

#[test]
fn each_body_and_for_iteration_names_its_header_line_and_item() -> TestResult {
    let host = host(64)?;
    let climbing = task(
        "climbing",
        |scope: Scope, ()| async move { rungs(&scope).await },
    )?;
    let report = lgwks_bot::block_on(host.run(&climbing, ()));
    assert_eq!(report.result().ok().copied(), Some(4));
    let rendered = report.script_trail().to_string();
    assert_eq!(
        lines(&rendered),
        [
            "   - | climbing  -> ok",
            "  36 |   flow rungs() -> usize  -> ok",
            "  38 |     for rung in 0..2 #0  -> ok",
            "  39 |       each item in 0..2  -> ok",
            "  39 |         each item in 0..2 #0  -> ok",
            "  39 |         each item in 0..2 #1  -> ok",
            "  38 |     for rung in 0..2 #1  -> ok",
            "  39 |       each item in 0..2  -> ok",
            "  39 |         each item in 0..2 #0  -> ok",
            "  39 |         each item in 0..2 #1  -> ok",
        ],
        "every body is its header's line with its item number:\n{rendered}"
    );
    Ok(())
}

/// INV-BOT-20: the trail is the bounded ring it was before it carried lines.
#[test]
fn the_trail_stays_bounded_and_says_how_much_it_dropped() -> TestResult {
    let host = host(4)?;
    let climbing = task("climbing", |scope: Scope, count: u32| async move {
        ladder(&scope, count).await
    })?;
    let report = lgwks_bot::block_on(host.run(&climbing, 10));
    assert_eq!(report.result().ok().copied(), Some(10));
    // Entered: the task step, the flow, then a rung and a climb for each of
    // ten iterations, 22 in all; the ring keeps the last four.
    assert_eq!(report.trail().len(), 4, "the ring keeps its capacity");
    assert_eq!(report.steps().len(), 4);
    assert_eq!(report.dropped_steps(), 18);
    let rendered = report.script_trail().to_string();
    assert_eq!(
        lines(&rendered),
        [
            "     | (18 earlier steps not kept)",
            "  46 | for rung in 0..count #8  -> ok",
            "  47 |   step climb  -> ok",
            "  46 | for rung in 0..count #9  -> ok",
            "  47 |   step climb  -> ok",
        ],
        "a truncated trail is a suffix, and it says so first:\n{rendered}"
    );
    Ok(())
}

/// The site rides beside the key, never in it: a step `script!` wrote keys
/// exactly as the same path entered by hand, so moving a line replays.
#[test]
fn a_script_step_keys_as_its_path_entered_by_hand() -> TestResult {
    let tenant = Tenant::new("acme")?;
    let written = lgwks_bot::block_on(keyed(&Scope::root(tenant.clone())))?;
    let by_hand = Scope::root(tenant).enter("keyed")?.enter("inner")?.key();
    assert_eq!(written, by_hand.to_hex());
    Ok(())
}
