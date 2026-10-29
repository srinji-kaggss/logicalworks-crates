//! Matched-workload measurement for ADR 0006 R3: what putting a run into the world costs.
//!
//! The same task, the same input, the same count, four ways:
//!
//! 1. `before` — a host with no feed: the run as it cost before R3.
//! 2. `memory` — the same host writing `admitted` and `settled` to an in-memory feed.
//! 3. `file` — the same, through the durable `FileJournal`, one synced append per rung.
//! 4. `event-to-effect` — from a settled run to the bound system having fired it and
//!    marked it handled: one `tick` of a world that has exactly one run due, over each
//!    journal.
//!
//! Decision 8 of the ADR asks for these numbers before and after, so the cost of the
//! step is stated rather than assumed. Peak RSS is read from the process by the caller:
//!
//! ```text
//! cargo build --release -p lgwks_bot --example run_events_cost
//! /usr/bin/time -l target/release/examples/run_events_cost
//! ```
//!
//! Written through `std::io::Write`, because `clippy::print_stdout` is forbidden
//! workspace-wide.

use std::cell::Cell;
use std::io::Write;
use std::rc::Rc;
use std::time::Instant;

use lgwks_bot::journal::{EffectJournal, FileJournal, MemoryJournal};
use lgwks_bot::task::{Host, task};
use lgwks_bot::{Bot, GrantSet, RunFeed};

#[path = "../tests/support/effects.rs"]
mod effects;
#[path = "support/measure.rs"]
mod measure;
#[path = "../tests/support/scratch.rs"]
mod scratch;

/// How many runs each measurement makes.
const RUNS: u32 = 1_000;

/// Report one measurement.
fn report(label: &str, samples: &mut [u128]) {
    let mut out = std::io::stdout().lock();
    let _written = writeln!(out, "{}", measure::Summary::of(samples).line(label));
}

/// A measurement that cannot be trusted: logged, then handed to the caller.
fn refused<T>(reason: String) -> Result<T, Box<dyn std::error::Error>> {
    lgwks_std::trace::warn!(reason = reason.as_str(), "run_events_cost: refused");
    Err(reason.into())
}

/// Run [`RUNS`] probes on `host`, timing each.
fn host_runs(host: &Host) -> Result<Vec<u128>, Box<dyn std::error::Error>> {
    let probe = task("probe", |_scope, input: u32| async move { Ok(input) })?;
    let mut samples = Vec::with_capacity(usize::try_from(RUNS)?);
    for input in 0..RUNS {
        let started = Instant::now();
        let ran = lgwks_bot::block_on(host.run(&probe, input));
        samples.push(started.elapsed().as_micros());
        if !ran.disposition().is_success() {
            return refused(format!("run {input} did not succeed: {:?}", ran.error()));
        }
    }
    Ok(samples)
}

/// One host run then one world tick, [`RUNS`] times: the tick is timed, and it must
/// fire exactly the one run that settled before it.
fn event_to_effect(feed: &RunFeed) -> Result<Vec<u128>, Box<dyn std::error::Error>> {
    let host = Host::builder("bench")?.runs(feed.clone()).build()?;
    let probe = task("probe", |_scope, input: u32| async move { Ok(input) })?;
    let mut bot = Bot::builder("bench")
        .with_effects(effects::memory_scope()?)
        .build(&GrantSet::empty())?;
    let fired = Rc::new(Cell::new(0_u32));
    let sink = Rc::clone(&fired);
    feed.bind(&mut bot, move |_run| sink.set(sink.get().saturating_add(1)));
    let mut samples = Vec::with_capacity(usize::try_from(RUNS)?);
    for input in 0..RUNS {
        let ran = lgwks_bot::block_on(host.run(&probe, input));
        if !ran.disposition().is_success() {
            return refused(format!("run {input} did not succeed: {:?}", ran.error()));
        }
        let started = Instant::now();
        bot.tick()?;
        samples.push(started.elapsed().as_micros());
    }
    if fired.get() != RUNS {
        return refused(format!("the world fired {} of {RUNS} runs", fired.get()));
    }
    Ok(samples)
}

/// A host writing to a fresh feed over `journal`.
fn fed_host(
    journal: Box<dyn EffectJournal + Send>,
) -> Result<(Host, RunFeed), Box<dyn std::error::Error>> {
    let feed = RunFeed::open(journal)?;
    let host = Host::builder("bench")?.runs(feed.clone()).build()?;
    Ok((host, feed))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let scratch = scratch::Scratch::new("run-events-cost")?;
    let mut out = std::io::stdout().lock();
    let _written = writeln!(
        out,
        "matched workload: one host run of a task returning its u32 input; {RUNS} runs each"
    );
    drop(out);

    let bare = Host::builder("bench")?.build()?;
    report("before (no feed)     ", &mut host_runs(&bare)?);

    let (memory_host, _memory_feed) = fed_host(Box::new(MemoryJournal::new()))?;
    report("after  (memory feed) ", &mut host_runs(&memory_host)?);

    let file = FileJournal::open(scratch.path().join("runs.jrnl"))?;
    let (file_host, _file_feed) = fed_host(Box::new(file))?;
    report("after  (file feed)   ", &mut host_runs(&file_host)?);

    let memory = RunFeed::open(Box::new(MemoryJournal::new()))?;
    report("event-to-effect (memory)", &mut event_to_effect(&memory)?);

    let durable = RunFeed::open(Box::new(FileJournal::open(
        scratch.path().join("world.jrnl"),
    )?))?;
    report("event-to-effect (file)  ", &mut event_to_effect(&durable)?);
    Ok(())
}
