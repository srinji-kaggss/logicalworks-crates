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
//! | `retry_budget_holds` | a correlated failure across a fan-out spends at most `10 + first attempts / 5` retries, then refuses as `Throttled` |
//! | `fan_machine_sized` | `each` with no written bound keeps input order and never exceeds 64 bodies per core |
//! | `fan_out_typed_error` | `FanOut` returns the first failing item's own error and position, with nothing left running |
//! | `fan_out_bounded_ordered` | `FanOut` keeps input order and never exceeds its written bound |
//! | `fan_out_deadline` | a `FanOut` past its deadline is `TimedOut`, starts no more than its bound, and drops every body |
//! | `fan_out_refusals_and_drop` | a bound of zero or past the ceiling is refused before any body starts; dropping the future drops every body |
//! | `together_owned` | one poll of the flow starts every `together` branch, and dropping the flow drops every branch |
//! | `together_fail_fast` | the first failing branch ends `together` with its own failure, and no sibling runs on to its end |
//! | `step_own_key` | a step's key is distinct from its flow's and its sibling's, and a rerun reproduces it |
//! | `step_failure_located` | a failure inside a step is located at that step, and no later step runs |
//! | `for_scope_per_item` | `for` gives each item its own scope and key, named by its position |
//! | `for_in_order` | `for` runs its items in input order, one at a time |
//! | `let_fails_before_binding` | a failing `let x = <block>:` ends the flow with the block's failure before any later line runs |
//! | `run_in_callers_tenant` | a `run` callee sees the caller's tenant, and two tenants never share its key |
//! | `fail_is_classified` | `fail with` is permanent and `fail transiently with` retryable, each located and carrying its reason |
//!
//! The lexicon in `lgwks_macros` cites these families as the proof of its
//! words' guarantees, and a test there fails if a cited family is renamed or
//! removed.
//!
//! Each declared test sweeps its band twice and requires identical trace
//! hashes, so a nondeterministic run fails even when every assertion passes.

#![cfg(feature = "script")]

use crate::sim;

// The band-declaration macro, defined once for the whole layer.
use crate::band_family;

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use lgwks_bot::rt::sync::CancellationToken;
use lgwks_bot::script::{FanOut, FanOutError, FlowError, MAX_IN_FLIGHT, Scope, Tenant};

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

    /// Every item fails transiently on every attempt: a correlated outage,
    /// the case a retry storm is made of.
    flow stormy(probe: &Probe, items: Vec<Item>, limit: usize, budget: u32) -> Vec<u64>:
        let answers = each item in items, at most (limit) at once:
            retry up to (budget) times, waiting (std::time::Duration::ZERO):
                let live = probe.enter()
                probe.saw(scope)
                Turns(item.turns).await
                live.finish()
                fail transiently with "the upstream is down"
        give back answers

    /// `doubled` with no written bound: the runtime sizes the fan-out.
    flow doubled_sized(probe: &Probe, items: Vec<Item>) -> Vec<u64>:
        let doubled = each item in items:
            let live = probe.enter()
            Turns(item.turns).await
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

    /// One branch of `trio`: wait its turns, then fail or double its value.
    flow part(probe: &Probe, item: Item) -> u64:
        let live = probe.enter()
        Turns(item.turns).await
        if item.fails:
            fail with format!("part {} refused", item.value)
        live.finish()
        item.value.wrapping_mul(2)

    /// Three branches at once, each result bound to its name.
    flow trio(probe: &Probe, first: Item, second: Item, third: Item) -> u64:
        together:
            let left = run part(probe, first)
            let middle = run part(probe, second)
            let right = run part(probe, third)
        give back left.wrapping_add(middle).wrapping_add(right)

    /// Two named steps, recording the key the flow and each step sees;
    /// `failing` names the step that refuses (0: neither, 1: `fetch`, 2:
    /// `store`).
    flow stepped(probe: &Probe, turns: u32, failing: u32) -> u32:
        probe.saw(scope)
        step fetch:
            probe.saw(scope)
            if failing == 1:
                fail with "fetch refused"
            Turns(turns).await
        step store:
            probe.saw(scope)
            if failing == 2:
                fail with "store refused"
            Turns(0).await
        give back turns

    /// Every item in turn, recording the key and the path each one ran at.
    flow in_turn(probe: &Probe, items: Vec<Item>) -> Vec<String>:
        let mut paths = Vec::new()
        for item in items:
            let live = probe.enter()
            probe.saw(scope)
            Turns(item.turns).await
            paths.push(scope.path().to_owned())
            live.finish()
        give back paths

    /// Bind a step's value, then record that the line after the `let` ran.
    flow bound(probe: &Probe, item: Item) -> u64:
        let doubled = step compute:
            Turns(item.turns).await
            if item.fails:
                fail with "compute refused"
            item.value.wrapping_mul(2)
        let after = probe.enter()
        after.finish()
        give back doubled

    /// The tenant a called flow sees, recording its key.
    flow tenant_seen(probe: &Probe) -> String:
        probe.saw(scope)
        scope.tenant().as_str().to_owned()

    /// Call `tenant_seen` and give back what it saw.
    flow caller(probe: &Probe) -> String:
        let seen = run tenant_seen(probe)
        give back seen

    /// Fail with `reason`, transiently or for good.
    flow verdict(transient: bool, reason: u32):
        if transient:
            fail transiently with format!("reason {reason}")
        fail with format!("reason {reason}")
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

/// The failure a run was required to produce, or why it produced none.
///
/// One reading of "this run must fail" for the fan families, so a run that
/// unexpectedly succeeded is reported in the same words wherever it is asserted.
fn failure_of<T>(
    outcome: Result<T, FlowError>,
    because: &str,
) -> Result<FlowError, Box<dyn std::error::Error>> {
    match outcome {
        Err(error) => Ok(error),
        Ok(_) => Err(because.into()),
    }
}

/// The refusal for an outcome no branch of an assertion accounts for.
fn unexpected(outcome: &dyn std::fmt::Display) -> Result<(), Box<dyn std::error::Error>> {
    Err(format!("unexpected outcome: {outcome}").into())
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
        let error = failure_of(
            run_doubled(&probe, list, limit),
            "a failing item must fail the flow",
        )?;
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
            Err(other) => unexpected(&other)?,
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
        let error = failure_of(
            run_doubled(&probe, list, limit),
            "a stopped flow must not report success",
        )?;
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

/// A correlated failure across a fan-out spends at most the run's retry
/// budget: `10 + first attempts / 5` retries, then `Throttled`.
fn retry_budget_holds(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let list = items(sim, 400);
        let limit = seeded_limit(sim, 64)?;
        let budget = sim.rng().between(2, 12);
        let probe = Probe::default();
        let scope = tenant_scope("acme", CancellationToken::new())?;
        let outcome = drive(stormy(&scope, &probe, list.clone(), limit, budget));
        let keys = probe.keys.borrow();
        let attempts = u64::try_from(keys.len())?;
        let mut firsts = keys.clone();
        firsts.sort_unstable();
        firsts.dedup();
        let first = u64::try_from(firsts.len())?;
        let retries = attempts.saturating_sub(first);
        if list.is_empty() {
            assert!(outcome.is_ok(), "no items, no failure");
            sim.record(&format!(
                "n=0 limit={limit} budget={budget} attempts={attempts} end=ok"
            ));
            return Ok(());
        }
        let error = failure_of(outcome, "an upstream that always fails must fail the flow")?;
        assert!(
            matches!(
                error,
                FlowError::Throttled { .. } | FlowError::Exhausted { .. }
            ),
            "a spent retry ends as Throttled or Exhausted: {error}"
        );
        let allowance = first
            .checked_div(5)
            .ok_or("the budget grows by one per five first attempts")?;
        assert!(
            retries <= 10_u64.saturating_add(allowance),
            "retries {retries} exceeded the budget for {first} first attempts"
        );
        assert!(!error.is_retryable(), "a spent budget is not retried again");
        probe.assert_settled();
        sim.record(&format!(
            "n={} limit={limit} budget={budget} first={first} retries={retries} end={}",
            list.len(),
            error.at()
        ));
        Ok(())
    })
}

/// With no written bound, `each` keeps input order and stays under the
/// machine-sized ceiling of 64 bodies per core.
fn fan_machine_sized(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let list = items(sim, 2_000);
        let probe = Probe::default();
        let expected: Vec<u64> = list.iter().map(|item| item.value.wrapping_mul(2)).collect();
        let count = list.len();
        let scope = tenant_scope("acme", CancellationToken::new())?;
        let output = drive(doubled_sized(&scope, &probe, list))?;
        // A host that cannot report its parallelism is held to one core's
        // ceiling: the tightest bound, so the check can only get stricter.
        let cores = match std::thread::available_parallelism() {
            Ok(cores) => cores.get(),
            Err(_unknown) => 1,
        };
        let ceiling = cores.saturating_mul(64);
        assert_eq!(
            output, expected,
            "input order survives the machine-sized fan-out"
        );
        assert!(
            probe.peak.get() <= ceiling,
            "in-flight {} exceeded the machine ceiling {ceiling}",
            probe.peak.get()
        );
        probe.assert_settled();
        assert_eq!(probe.finished.get(), count, "every body ran to its end");
        sim.record(&format!("n={count} out={output:?}"));
        Ok(())
    })
}

/// Sixteen seeded sweeps of each named family, one test module per family.
///
/// A module per family rather than one flat list of names: the band numbers
/// are written once here, so no family can be declared with fewer bands than
/// its siblings, and a test's path still names its family and band
/// (`fan_bounded::band_07`).
macro_rules! sixteen_bands {
    ($($family:ident),+ $(,)?) => {
        $(
            mod $family {
                use super::*;
                band_family::band_family! {
                    band_00 => super::$family, 0;
                    band_01 => super::$family, 1;
                    band_02 => super::$family, 2;
                    band_03 => super::$family, 3;
                    band_04 => super::$family, 4;
                    band_05 => super::$family, 5;
                    band_06 => super::$family, 6;
                    band_07 => super::$family, 7;
                    band_08 => super::$family, 8;
                    band_09 => super::$family, 9;
                    band_10 => super::$family, 10;
                    band_11 => super::$family, 11;
                    band_12 => super::$family, 12;
                    band_13 => super::$family, 13;
                    band_14 => super::$family, 14;
                    band_15 => super::$family, 15;
                }
            }
        )+
    };
}

sixteen_bands!(
    fan_bounded,
    fan_fail_fast,
    retry_one_key,
    tenants_isolated,
    stop_reaches_every_body,
    retry_budget_holds,
    fan_machine_sized,
);

mod fan_out;

mod flows;
