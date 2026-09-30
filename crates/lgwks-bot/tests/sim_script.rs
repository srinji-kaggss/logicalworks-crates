//! Simulation family: `script!` flows, driven by a seeded sweep.
//!
//! Every scenario runs flows the real `script!` macro expanded, on the real
//! `lgwks_bot::script` runtime. Nothing here reimplements a fan-out, a retry or
//! a scope. What the seed controls is the input: how many items, how many at
//! once, how long each body stays pending, which item fails and how, when a
//! stop arrives, and which tenants run. Time is not consulted: a body stays
//! pending for a seeded number of polls ([`Turns`]), so poll order, and with it
//! the whole trace, is a function of the seed.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `fan_bounded` | output is in input order; in-flight never exceeds the bound and reaches it |
//! | `fan_fail_fast` | the first failure stops the fan-out, is located at its item, and leaves no body running |
//! | `retry_one_key` | every attempt sees one key; transient failures retry to the budget, permanent ones never |
//! | `tenants_isolated` | two tenants never share a key; the same tenant rerunning gets the same keys |
//! | `stop_reaches_every_body` | a stop mid-fan-out ends the flow as cancelled with nothing left running |
//!
//! Each declared test sweeps its band twice and requires identical trace
//! hashes, so a nondeterministic run fails even when every assertion passes.

#![cfg(feature = "script")]

mod sim;

// The band-declaration macro, defined once for the whole layer.
#[path = "sim/bands.rs"]
mod band_family;

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use lgwks_bot::rt::sync::CancellationToken;
use lgwks_bot::script::{FlowError, Scope, Tenant};

use sim::Band;

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

/// A future that stays pending for a fixed number of polls, waking itself
/// each time, so "slow" is a count the seed chose rather than a clock reading.
struct Turns(u32);

impl Future for Turns {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        if self.0 == 0 {
            return Poll::Ready(());
        }
        self.0 = self.0.saturating_sub(1);
        context.waker().wake_by_ref();
        Poll::Pending
    }
}

/// What the flows report about the bodies they ran.
///
/// Borrowed by the flows, not shared through `Arc`: a script body borrows its
/// surroundings the way a Python loop body would, and these tests are the
/// proof that it can.
#[derive(Default)]
struct Probe {
    /// Bodies started and not yet finished or dropped.
    live: Cell<usize>,
    /// The most bodies ever live at once.
    peak: Cell<usize>,
    /// Bodies started.
    started: Cell<usize>,
    /// Bodies that ran to the end.
    finished: Cell<usize>,
    /// Bodies dropped before their end.
    abandoned: Cell<usize>,
    /// Every step key a body observed, in observation order.
    keys: RefCell<Vec<String>>,
}

impl Probe {
    /// Count a body in; the guard counts it out.
    fn enter(&self) -> Live<'_> {
        self.started.set(self.started.get().saturating_add(1));
        let live = self.live.get().saturating_add(1);
        self.live.set(live);
        if live > self.peak.get() {
            self.peak.set(live);
        }
        Live {
            probe: self,
            finished: false,
        }
    }

    /// Record a key a body saw.
    fn saw(&self, scope: &Scope) {
        self.keys.borrow_mut().push(scope.key().to_hex());
    }

    /// No body is still running, and every body that started either reached
    /// its end or was dropped: nothing outlived the flow.
    fn assert_settled(&self) {
        assert_eq!(self.live.get(), 0, "no body outlives the flow");
        assert_eq!(
            self.finished.get().saturating_add(self.abandoned.get()),
            self.started.get(),
            "every started body is accounted for"
        );
    }
}

/// A body's presence: dropping it unfinished counts the body as abandoned.
struct Live<'probe> {
    /// Where to report.
    probe: &'probe Probe,
    /// Whether the body reached its end.
    finished: bool,
}

impl Live<'_> {
    /// The body reached its end.
    fn finish(mut self) {
        self.finished = true;
    }
}

impl Drop for Live<'_> {
    fn drop(&mut self) {
        let probe = self.probe;
        probe.live.set(probe.live.get().saturating_sub(1));
        let counter = if self.finished {
            &probe.finished
        } else {
            &probe.abandoned
        };
        counter.set(counter.get().saturating_add(1));
    }
}

/// One item of a seeded fan-out: its value, how many polls it takes, and
/// whether it fails or stops the run.
#[derive(Clone, Copy, Debug, Default)]
struct Item {
    /// The value the body doubles.
    value: u64,
    /// Polls before the body finishes.
    turns: u32,
    /// Whether the body fails instead of finishing.
    fails: bool,
    /// Whether the body stops the whole run instead of finishing.
    stops: bool,
}

lgwks_bot::script! {
    /// Double every value, `limit` at a time, reporting to `probe`.
    flow doubled(probe: &Probe, stop: &CancellationToken, items: Vec<Item>, limit: usize) -> Vec<u64>:
        let doubled = each item in items, at most (limit) at once:
            let live = probe.enter()
            probe.saw(scope)
            Turns(item.turns).await
            if item.stops:
                stop.cancel()
                Turns(1).await
            if item.fails:
                fail with format!("item {} refused", item.value)
            live.finish()
            item.value.wrapping_mul(2)
        give back doubled

    /// Fail transiently `failures` times (or permanently once), within a
    /// budget of `budget` attempts, recording the key each attempt sees.
    flow flaky(probe: &Probe, failures: u32, permanent: bool, budget: u32) -> u32:
        let calls = Cell::new(0_u32)
        let answer = retry up to (budget) times, waiting (std::time::Duration::ZERO):
            calls.set(calls.get().saturating_add(1))
            probe.saw(scope)
            if permanent:
                fail with "permanent"
            if calls.get() <= failures:
                fail transiently with "not yet"
            calls.get()
        give back answer
}

/// Drive a flow to its end on the crate's runtime.
fn drive<T>(flow: impl Future<Output = Result<T, FlowError>>) -> Result<T, FlowError> {
    lgwks_bot::block_on(flow)
}

/// A seeded list of items, `0..=max` long.
fn items(sim: &mut sim::Sim, max: u32) -> Vec<Item> {
    let count = sim.rng().between(0, max);
    (0..count)
        .map(|index| Item {
            value: u64::from(index),
            turns: sim.rng().between(0, 6),
            ..Item::default()
        })
        .collect()
}

/// A seeded, non-empty list with one seeded item marked by `mark`; returns
/// the list and the marked position.
fn items_with_one(
    sim: &mut sim::Sim,
    max: u32,
    mark: fn(&mut Item),
) -> Result<(Vec<Item>, usize), Box<dyn Error>> {
    let mut list = items(sim, max);
    if list.is_empty() {
        list.push(Item::default());
    }
    let last = u32::try_from(list.len().saturating_sub(1))?;
    let position = usize::try_from(sim.rng().between(0, last))?;
    if let Some(item) = list.get_mut(position) {
        mark(item);
    }
    Ok((list, position))
}

/// A seeded fan-out bound, `1..=max`.
fn seeded_limit(sim: &mut sim::Sim, max: u32) -> Result<usize, Box<dyn Error>> {
    Ok(usize::try_from(sim.rng().between(1, max))?)
}

/// Run `doubled` over `list` for tenant `acme`, reporting to `probe`.
fn run_doubled(probe: &Probe, list: Vec<Item>, limit: usize) -> Result<Vec<u64>, FlowError> {
    let stop = CancellationToken::new();
    let scope = tenant_scope("acme", stop.clone())?;
    drive(doubled(&scope, probe, &stop, list, limit))
}

/// A root scope for a tenant name.
fn tenant_scope(name: &str, token: CancellationToken) -> Result<Scope, FlowError> {
    Ok(Scope::with_token(Tenant::new(name)?, token))
}

// ── Families ──────────────────────────────────────────────────────────────

/// Output is in input order; in-flight never exceeds the bound, and reaches
/// it when every body takes at least one poll.
fn fan_bounded(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let mut list = items(sim, 200);
        let limit = seeded_limit(sim, 32)?;
        let all_slow = sim.rng().chance(500);
        if all_slow {
            for item in &mut list {
                item.turns = item.turns.max(1);
            }
        }
        let probe = Probe::default();
        let expected: Vec<u64> = list.iter().map(|item| item.value.wrapping_mul(2)).collect();
        let count = list.len();
        let output = run_doubled(&probe, list, limit)?;

        assert_eq!(
            output, expected,
            "each returns item i's value at position i"
        );
        assert!(
            probe.peak.get() <= limit,
            "in-flight {} exceeded the bound {limit}",
            probe.peak.get()
        );
        if all_slow {
            assert_eq!(
                probe.peak.get(),
                limit.min(count),
                "with every body pending at least once, the fan-out fills its bound"
            );
        }
        probe.assert_settled();
        assert_eq!(probe.finished.get(), count, "every body ran to its end");
        sim.record(&format!(
            "n={count} limit={limit} peak={} out={output:?}",
            probe.peak.get()
        ));
        Ok(())
    })
}

/// The first failure stops the fan-out, is located at its item, and leaves no
/// body running.
fn fan_fail_fast(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let (list, failing) = items_with_one(sim, 120, |item| item.fails = true)?;
        let limit = seeded_limit(sim, 16)?;
        let probe = Probe::default();
        let Err(error) = run_doubled(&probe, list, limit) else {
            return Err("a failing item must fail the flow".into());
        };
        assert!(
            matches!(error, FlowError::Failed { .. }),
            "the item's own failure is returned, not a cancellation: {error}"
        );
        assert!(
            error.at().ends_with(&format!("#{failing}")),
            "the failure is located at its item: {}",
            error.at()
        );
        assert!(!error.is_retryable(), "`fail with` is permanent");
        probe.assert_settled();
        sim.record(&format!(
            "fail={failing} limit={limit} started={} abandoned={} at={}",
            probe.started.get(),
            probe.abandoned.get(),
            error.at()
        ));
        Ok(())
    })
}

/// Every attempt sees one key; transient failures retry to the budget,
/// permanent ones never.
fn retry_one_key(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let budget = sim.rng().between(1, 8);
        let failures = sim.rng().between(0, 10);
        let permanent = sim.rng().chance(200);
        let probe = Probe::default();
        let scope = tenant_scope("acme", CancellationToken::new())?;
        let outcome = drive(flaky(&scope, &probe, failures, permanent, budget));
        let keys = probe.keys.borrow();
        let attempts = u32::try_from(keys.len())?;

        assert!(
            keys.windows(2).all(|pair| pair.first() == pair.last()),
            "every attempt of one retry sees the same key"
        );
        match outcome {
            Ok(calls) => {
                assert!(
                    !permanent && failures < budget,
                    "success only when the budget covers the failures"
                );
                assert_eq!(
                    calls,
                    failures.saturating_add(1),
                    "the successful attempt is the one after the failures"
                );
                assert_eq!(attempts, calls, "one key observation per attempt");
            }
            Err(FlowError::Exhausted {
                attempts: made,
                ref last,
                ..
            }) => {
                assert!(
                    !permanent && failures >= budget,
                    "exhaustion only when failures outlast the budget"
                );
                assert_eq!(made, budget, "the whole budget was spent");
                assert!(
                    last.is_retryable(),
                    "only retryable failures exhaust a budget"
                );
            }
            Err(ref other) if permanent => {
                assert!(
                    matches!(*other, FlowError::Failed { .. }),
                    "a permanent failure surfaces as itself: {other}"
                );
                assert_eq!(attempts, 1, "a permanent failure is never repeated");
            }
            Err(other) => return Err(format!("unexpected outcome: {other}").into()),
        }
        sim.record(&format!(
            "budget={budget} failures={failures} permanent={permanent} attempts={attempts}"
        ));
        Ok(())
    })
}

/// Two tenants never share a key; the same tenant rerunning gets the same
/// keys, which is what makes a key an idempotency key across a restart.
fn tenants_isolated(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let list = items(sim, 64);
        let limit = seeded_limit(sim, 8)?;
        let first = format!("tenant-{}", sim.rng().below(1_000));
        let second = format!("{first}-other");
        let run = |name: &str| -> Result<Vec<String>, Box<dyn Error>> {
            let probe = Probe::default();
            let stop = CancellationToken::new();
            let scope = tenant_scope(name, stop.clone())?;
            drive(doubled(&scope, &probe, &stop, list.clone(), limit))?;
            let mut keys = probe.keys.take();
            keys.sort_unstable();
            Ok(keys)
        };
        let ours = run(&first)?;
        let theirs = run(&second)?;
        let again = run(&first)?;

        assert_eq!(ours.len(), list.len(), "one key per item");
        let mut distinct = ours.clone();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            ours.len(),
            "items of one run never share a key"
        );
        assert!(
            ours.iter().all(|key| theirs.binary_search(key).is_err()),
            "no key is shared across tenants"
        );
        assert_eq!(
            ours, again,
            "a rerun for the same tenant reproduces every key"
        );
        sim.record(&format!(
            "tenant={first} n={} first={:?}",
            ours.len(),
            ours.first()
        ));
        Ok(())
    })
}

/// A stop mid-fan-out ends the flow as cancelled with nothing left running.
fn stop_reaches_every_body(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let (list, stopping) = items_with_one(sim, 120, |item| item.stops = true)?;
        let limit = seeded_limit(sim, 16)?;
        let probe = Probe::default();
        let Err(error) = run_doubled(&probe, list, limit) else {
            return Err("a stopped flow must not report success".into());
        };
        assert!(
            error.is_cancelled(),
            "a stop is reported as a stop: {error}"
        );
        probe.assert_settled();
        sim.record(&format!(
            "stop={stopping} limit={limit} started={} at={}",
            probe.started.get(),
            error.at()
        ));
        Ok(())
    })
}

band_family::band_family! {
    fan_bounded_band_00 => fan_bounded, 0;
    fan_bounded_band_01 => fan_bounded, 1;
    fan_bounded_band_02 => fan_bounded, 2;
    fan_bounded_band_03 => fan_bounded, 3;
    fan_bounded_band_04 => fan_bounded, 4;
    fan_bounded_band_05 => fan_bounded, 5;
    fan_bounded_band_06 => fan_bounded, 6;
    fan_bounded_band_07 => fan_bounded, 7;
    fan_bounded_band_08 => fan_bounded, 8;
    fan_bounded_band_09 => fan_bounded, 9;
    fan_bounded_band_10 => fan_bounded, 10;
    fan_bounded_band_11 => fan_bounded, 11;
    fan_bounded_band_12 => fan_bounded, 12;
    fan_bounded_band_13 => fan_bounded, 13;
    fan_bounded_band_14 => fan_bounded, 14;
    fan_bounded_band_15 => fan_bounded, 15;
    fan_fail_fast_band_00 => fan_fail_fast, 0;
    fan_fail_fast_band_01 => fan_fail_fast, 1;
    fan_fail_fast_band_02 => fan_fail_fast, 2;
    fan_fail_fast_band_03 => fan_fail_fast, 3;
    fan_fail_fast_band_04 => fan_fail_fast, 4;
    fan_fail_fast_band_05 => fan_fail_fast, 5;
    fan_fail_fast_band_06 => fan_fail_fast, 6;
    fan_fail_fast_band_07 => fan_fail_fast, 7;
    fan_fail_fast_band_08 => fan_fail_fast, 8;
    fan_fail_fast_band_09 => fan_fail_fast, 9;
    fan_fail_fast_band_10 => fan_fail_fast, 10;
    fan_fail_fast_band_11 => fan_fail_fast, 11;
    fan_fail_fast_band_12 => fan_fail_fast, 12;
    fan_fail_fast_band_13 => fan_fail_fast, 13;
    fan_fail_fast_band_14 => fan_fail_fast, 14;
    fan_fail_fast_band_15 => fan_fail_fast, 15;
    retry_one_key_band_00 => retry_one_key, 0;
    retry_one_key_band_01 => retry_one_key, 1;
    retry_one_key_band_02 => retry_one_key, 2;
    retry_one_key_band_03 => retry_one_key, 3;
    retry_one_key_band_04 => retry_one_key, 4;
    retry_one_key_band_05 => retry_one_key, 5;
    retry_one_key_band_06 => retry_one_key, 6;
    retry_one_key_band_07 => retry_one_key, 7;
    retry_one_key_band_08 => retry_one_key, 8;
    retry_one_key_band_09 => retry_one_key, 9;
    retry_one_key_band_10 => retry_one_key, 10;
    retry_one_key_band_11 => retry_one_key, 11;
    retry_one_key_band_12 => retry_one_key, 12;
    retry_one_key_band_13 => retry_one_key, 13;
    retry_one_key_band_14 => retry_one_key, 14;
    retry_one_key_band_15 => retry_one_key, 15;
    tenants_isolated_band_00 => tenants_isolated, 0;
    tenants_isolated_band_01 => tenants_isolated, 1;
    tenants_isolated_band_02 => tenants_isolated, 2;
    tenants_isolated_band_03 => tenants_isolated, 3;
    tenants_isolated_band_04 => tenants_isolated, 4;
    tenants_isolated_band_05 => tenants_isolated, 5;
    tenants_isolated_band_06 => tenants_isolated, 6;
    tenants_isolated_band_07 => tenants_isolated, 7;
    tenants_isolated_band_08 => tenants_isolated, 8;
    tenants_isolated_band_09 => tenants_isolated, 9;
    tenants_isolated_band_10 => tenants_isolated, 10;
    tenants_isolated_band_11 => tenants_isolated, 11;
    tenants_isolated_band_12 => tenants_isolated, 12;
    tenants_isolated_band_13 => tenants_isolated, 13;
    tenants_isolated_band_14 => tenants_isolated, 14;
    tenants_isolated_band_15 => tenants_isolated, 15;
    stop_reaches_every_body_band_00 => stop_reaches_every_body, 0;
    stop_reaches_every_body_band_01 => stop_reaches_every_body, 1;
    stop_reaches_every_body_band_02 => stop_reaches_every_body, 2;
    stop_reaches_every_body_band_03 => stop_reaches_every_body, 3;
    stop_reaches_every_body_band_04 => stop_reaches_every_body, 4;
    stop_reaches_every_body_band_05 => stop_reaches_every_body, 5;
    stop_reaches_every_body_band_06 => stop_reaches_every_body, 6;
    stop_reaches_every_body_band_07 => stop_reaches_every_body, 7;
    stop_reaches_every_body_band_08 => stop_reaches_every_body, 8;
    stop_reaches_every_body_band_09 => stop_reaches_every_body, 9;
    stop_reaches_every_body_band_10 => stop_reaches_every_body, 10;
    stop_reaches_every_body_band_11 => stop_reaches_every_body, 11;
    stop_reaches_every_body_band_12 => stop_reaches_every_body, 12;
    stop_reaches_every_body_band_13 => stop_reaches_every_body, 13;
    stop_reaches_every_body_band_14 => stop_reaches_every_body, 14;
    stop_reaches_every_body_band_15 => stop_reaches_every_body, 15;
}
