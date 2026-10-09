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
            let a = run part(probe, first)
            let b = run part(probe, second)
            let c = run part(probe, third)
        give back a.wrapping_add(b).wrapping_add(c)

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
        let ceiling = std::thread::available_parallelism()
            .map_or(1, std::num::NonZeroUsize::get)
            .saturating_mul(64);
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

/// The error a `FanOut` body returns in these families: a type of the test's
/// own, so the family can tell it came back unchanged.
#[derive(Debug, PartialEq)]
struct Refused(u64);

impl std::fmt::Display for Refused {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "item {} refused", self.0)
    }
}

impl Error for Refused {}

/// Run `FanOut` over `list` for `limit` at once, reporting to `probe`. A body
/// that `fails` returns `Refused(its value)`; the rest double their value.
fn run_fan_out(
    probe: &Probe,
    list: Vec<Item>,
    limit: usize,
) -> Result<Vec<u64>, FanOutError<Refused>> {
    lgwks_bot::block_on(FanOut::new(list).at_most(limit).run(|item| async move {
        let live = probe.enter();
        Turns(item.turns).await;
        if item.fails {
            let refusal: Result<u64, Refused> = Err(Refused(item.value));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "fan_out body: returning an error to the caller");
            return refusal;
        }
        live.finish();
        Ok(item.value.wrapping_mul(2))
    }))
}

/// `FanOut` returns the first failing item's own error and its position, and
/// leaves nothing running.
fn fan_out_typed_error(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let mut list = items(sim, 120);
        if list.is_empty() {
            list.push(Item::default());
        }
        let last = u32::try_from(list.len().saturating_sub(1))?;
        let marked = sim.rng().between(1, 3);
        let mut failing = Vec::new();
        for _ in 0..marked {
            let position = usize::try_from(sim.rng().between(0, last))?;
            if let Some(item) = list.get_mut(position) {
                item.fails = true;
            }
            failing.push(position);
        }
        let limit = seeded_limit(sim, 16)?;
        let values: Vec<u64> = list.iter().map(|item| item.value).collect();
        let probe = Probe::default();
        let Err(FanOutError::Item { index, error }) = run_fan_out(&probe, list, limit) else {
            let refusal: TestResult =
                Err("a failing item must come back as FanOutError::Item".into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "sweep: returning an error to the caller");
            return refusal;
        };

        assert!(
            failing.contains(&index),
            "the failure is one of the marked items: {index} not in {failing:?}"
        );
        assert_eq!(
            Some(error),
            values.get(index).copied().map(Refused),
            "the item's own error comes back unchanged, at its own position"
        );
        assert!(
            probe.peak.get() <= limit,
            "in-flight {} exceeded the bound {limit}",
            probe.peak.get()
        );
        probe.assert_settled();
        sim.record(&format!(
            "n={} limit={limit} index={index} started={}",
            values.len(),
            probe.started.get()
        ));
        Ok(())
    })
}

/// `FanOut` keeps input order and never exceeds its written bound.
fn fan_out_bounded_ordered(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let list = items(sim, 200);
        let limit = seeded_limit(sim, 32)?;
        let expected: Vec<u64> = list.iter().map(|item| item.value.wrapping_mul(2)).collect();
        let count = list.len();
        let probe = Probe::default();
        let output = run_fan_out(&probe, list, limit)?;

        assert_eq!(
            output, expected,
            "FanOut returns item i's value at position i"
        );
        assert!(
            probe.peak.get() <= limit,
            "in-flight {} exceeded the bound {limit}",
            probe.peak.get()
        );
        probe.assert_settled();
        assert_eq!(probe.finished.get(), count, "every body ran to its end");
        sim.record(&format!(
            "n={count} limit={limit} peak={} out={output:?}",
            probe.peak.get()
        ));
        Ok(())
    })
}

/// A `FanOut` body that starts, reports to `probe` and never finishes: the
/// shape of an upstream that has stopped answering.
async fn stalled(probe: &Probe, value: u32) -> Result<u64, Refused> {
    let live = probe.enter();
    std::future::pending::<()>().await;
    live.finish();
    Ok(u64::from(value))
}

/// A `FanOut` whose bodies never finish is `TimedOut` at its deadline, started
/// no more than its bound's worth of bodies, and dropped every one of them.
fn fan_out_deadline(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let count = sim.rng().between(1, 60);
        let limit = seeded_limit(sim, 8)?;
        let deadline = Duration::from_millis(20);
        let probe = Probe::default();
        let outcome = lgwks_bot::block_on(
            FanOut::new(0..count)
                .at_most(limit)
                .within(deadline)
                .run(|value| stalled(&probe, value)),
        );

        match outcome {
            Err(FanOutError::TimedOut { after }) => {
                assert_eq!(after, deadline, "the declared deadline is reported");
            }
            other => {
                let refusal: TestResult = Err(format!("expected TimedOut, got {other:?}").into());
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "sweep: returning an error to the caller");
                return refusal;
            }
        }
        probe.assert_settled();
        assert_eq!(
            probe.started.get(),
            limit.min(usize::try_from(count)?),
            "no body past the bound ever started"
        );
        sim.record(&format!(
            "n={count} limit={limit} started={}",
            probe.started.get()
        ));
        Ok(())
    })
}

/// A bound of zero, or past the ceiling, is refused before any body starts;
/// and dropping the future mid-run drops every body it owns.
fn fan_out_refusals_and_drop(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let probe = Probe::default();
        let body = |value: u32| stalled(&probe, value);
        for refused in [0, MAX_IN_FLIGHT.saturating_add(1)] {
            let outcome = lgwks_bot::block_on(FanOut::new(0..3_u32).at_most(refused).run(body));
            assert!(
                matches!(
                    outcome,
                    Err(FanOutError::Flow(FlowError::InvalidBound { .. }))
                ),
                "a bound of {refused} is refused as InvalidBound: {outcome:?}"
            );
            assert_eq!(probe.started.get(), 0, "a refused fan-out starts no body");
        }

        let count = sim.rng().between(2, 40);
        let limit = seeded_limit(sim, 8)?;
        let mut running = Box::pin(FanOut::new(0..count).at_most(limit).run(body));
        let mut context = Context::from_waker(Waker::noop());
        assert!(
            running.as_mut().poll(&mut context).is_pending(),
            "bodies that never finish leave the fan-out pending"
        );
        assert!(probe.live.get() > 0, "the fan-out owns running bodies");
        drop(running);
        probe.assert_settled();
        assert_eq!(
            probe.abandoned.get(),
            probe.started.get(),
            "every body the dropped future owned was dropped with it"
        );
        sim.record(&format!(
            "n={count} limit={limit} started={}",
            probe.started.get()
        ));
        Ok(())
    })
}

/// One of `trio`'s branches: item `index`, waiting a seeded number of turns.
fn branch_item(sim: &mut sim::Sim, index: u32, least_turns: u32) -> Item {
    Item {
        value: u64::from(index),
        turns: sim.rng().between(least_turns, 6),
        ..Item::default()
    }
}

/// `together`'s branches are futures of this task, not spawned tasks: one
/// poll of the flow starts every branch, and dropping the flow drops every
/// branch with it.
fn together_owned(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let first = branch_item(sim, 0, 1);
        let second = branch_item(sim, 1, 1);
        let third = branch_item(sim, 2, 1);
        // A branch of `t` turns finishes on its `t + 1`th poll, so no branch
        // can finish within the fewest turns any of them takes.
        let fewest = first.turns.min(second.turns).min(third.turns);
        let polls = sim.rng().between(1, fewest);
        let probe = Probe::default();
        let scope = tenant_scope("acme", CancellationToken::new())?;
        let mut running = Box::pin(trio(&scope, &probe, first, second, third));
        let mut context = Context::from_waker(Waker::noop());
        for _ in 0..polls {
            assert!(
                running.as_mut().poll(&mut context).is_pending(),
                "no branch finishes within its turns"
            );
        }
        assert_eq!(
            probe.started.get(),
            3,
            "polling the flow started every branch: they are its own futures"
        );
        assert_eq!(probe.live.get(), 3, "and all three are live inside it");
        drop(running);
        probe.assert_settled();
        assert_eq!(
            probe.abandoned.get(),
            3,
            "dropping the flow dropped every branch it owned"
        );
        sim.record(&format!(
            "turns={},{},{} polls={polls}",
            first.turns, second.turns, third.turns
        ));
        Ok(())
    })
}

/// The first branch of `together` to fail ends it: the failure is that
/// branch's own, and no sibling runs on to its end.
fn together_fail_fast(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let failing = sim.rng().between(0, 2);
        // The failing branch fails on its first poll; every sibling needs at
        // least two, so a sibling reaching its end is one `together` let run.
        let mut parts = [
            branch_item(sim, 0, 1),
            branch_item(sim, 1, 1),
            branch_item(sim, 2, 1),
        ];
        for (index, part) in parts.iter_mut().enumerate() {
            if u32::try_from(index)? == failing {
                part.turns = 0;
                part.fails = true;
            }
        }
        let [first, second, third] = parts;
        let probe = Probe::default();
        let scope = tenant_scope("acme", CancellationToken::new())?;
        let error = failure_of(
            drive(trio(&scope, &probe, first, second, third)),
            "a failing branch must fail the flow",
        )?;
        assert!(
            matches!(error, FlowError::Failed { .. }),
            "the branch's own failure comes back, not a cancellation: {error}"
        );
        assert!(
            error
                .to_string()
                .contains(&format!("part {failing} refused")),
            "the failure is the failing branch's: {error}"
        );
        assert_eq!(
            probe.finished.get(),
            0,
            "no sibling ran on to its end after the first failure"
        );
        probe.assert_settled();
        sim.record(&format!(
            "failing={failing} started={} at={}",
            probe.started.get(),
            error.at()
        ));
        Ok(())
    })
}

/// A step's key is its own: distinct from its flow's and its sibling's, and
/// the same on a rerun, so a step is retried or resumed by its key alone.
fn step_own_key(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let tenant = format!("tenant-{}", sim.rng().below(1_000));
        let turns = sim.rng().between(0, 6);
        let keys_of = || -> Result<Vec<String>, Box<dyn Error>> {
            let probe = Probe::default();
            let scope = tenant_scope(&tenant, CancellationToken::new())?;
            drive(stepped(&scope, &probe, turns, 0))?;
            Ok(probe.keys.take())
        };
        let keys = keys_of()?;
        let again = keys_of()?;
        assert_eq!(
            keys.len(),
            3,
            "the flow, `fetch` and `store` each saw a key"
        );
        let mut distinct = keys.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(distinct.len(), 3, "each step's key is its own: {keys:?}");
        assert_eq!(keys, again, "a rerun reproduces every step's key");
        sim.record(&format!("tenant={tenant} keys={keys:?}"));
        Ok(())
    })
}

/// A failure inside a step is located at that step, and no later step runs.
fn step_failure_located(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let failing = sim.rng().between(0, 2);
        let turns = sim.rng().between(0, 6);
        let probe = Probe::default();
        let scope = tenant_scope("acme", CancellationToken::new())?;
        let outcome = drive(stepped(&scope, &probe, turns, failing));
        let entered = probe.keys.borrow().len();
        match (failing, &outcome) {
            (0, &Ok(value)) => {
                assert_eq!(
                    value, turns,
                    "with no refusal the flow gives back its value"
                );
                assert_eq!(entered, 3, "and both steps ran");
            }
            (1, Err(error)) => {
                assert_eq!(error.at(), "stepped/fetch", "located at `fetch`: {error}");
                assert_eq!(entered, 2, "`store` never ran after `fetch` failed");
            }
            (2, Err(error)) => {
                assert_eq!(error.at(), "stepped/store", "located at `store`: {error}");
                assert_eq!(entered, 3, "`fetch` ran before `store` failed");
            }
            _ => unexpected(&format!("failing={failing} outcome={outcome:?}"))?,
        }
        sim.record(&format!(
            "failing={failing} turns={turns} entered={entered}"
        ));
        Ok(())
    })
}

/// The path `in_turn` gives item `index`.
fn item_path(index: usize) -> String {
    format!("in_turn/for:item#{index}")
}

/// `for` gives every item its own scope: one key per item, none shared, each
/// scope named by its item's position, and a rerun reproduces the keys.
fn for_scope_per_item(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let list = items(sim, 64);
        let walk = || -> Result<(Vec<String>, Vec<String>), Box<dyn Error>> {
            let probe = Probe::default();
            let scope = tenant_scope("acme", CancellationToken::new())?;
            let paths = drive(in_turn(&scope, &probe, list.clone()))?;
            Ok((probe.keys.take(), paths))
        };
        let (keys, mut paths) = walk()?;
        let (again, _) = walk()?;
        assert_eq!(keys.len(), list.len(), "one key per item");
        let mut distinct = keys.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(distinct.len(), keys.len(), "no two items share a key");
        paths.sort_unstable();
        let mut named: Vec<String> = (0..list.len()).map(item_path).collect();
        named.sort_unstable();
        assert_eq!(paths, named, "every item's scope is named by its position");
        assert_eq!(keys, again, "a rerun reproduces every item's key");
        sim.record(&format!("n={} first={:?}", list.len(), keys.first()));
        Ok(())
    })
}

/// `for` runs its items in input order, one at a time.
fn for_in_order(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let list = items(sim, 64);
        let count = list.len();
        let probe = Probe::default();
        let scope = tenant_scope("acme", CancellationToken::new())?;
        let paths = drive(in_turn(&scope, &probe, list))?;
        let ordered: Vec<String> = (0..count).map(item_path).collect();
        assert_eq!(paths, ordered, "the items ran in input order");
        assert!(
            probe.peak.get() <= 1,
            "one item at a time, but {} ran at once",
            probe.peak.get()
        );
        assert_eq!(probe.finished.get(), count, "every item ran to its end");
        probe.assert_settled();
        sim.record(&format!("n={count} peak={}", probe.peak.get()));
        Ok(())
    })
}

/// `let x = <block>:` binds only a block that succeeded: when the block fails,
/// the flow ends with that failure and no line after the `let` runs.
fn let_fails_before_binding(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let fails = sim.rng().chance(500);
        let item = Item {
            value: u64::from(sim.rng().below(1_000)),
            turns: sim.rng().between(0, 6),
            fails,
            ..Item::default()
        };
        let probe = Probe::default();
        let scope = tenant_scope("acme", CancellationToken::new())?;
        let outcome = drive(bound(&scope, &probe, item));
        match outcome {
            Ok(value) if !fails => {
                assert_eq!(
                    value,
                    item.value.wrapping_mul(2),
                    "`x` is the block's value"
                );
                assert_eq!(probe.finished.get(), 1, "the line after the `let` ran");
            }
            Err(ref error) if fails => {
                assert!(
                    matches!(*error, FlowError::Failed { .. }),
                    "the block's own failure ends the flow: {error}"
                );
                assert_eq!(error.at(), "bound/compute", "located at the block");
                assert_eq!(probe.started.get(), 0, "no line after the failed `let` ran");
            }
            ref other => unexpected(&format!("fails={fails} outcome={other:?}"))?,
        }
        sim.record(&format!("fails={fails} turns={}", item.turns));
        Ok(())
    })
}

/// `run` calls a flow inside the caller's scope: the callee sees the caller's
/// tenant, and two tenants calling the same flow never share its key.
fn run_in_callers_tenant(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let first = format!("tenant-{}", sim.rng().below(1_000));
        let second = format!("{first}-other");
        let call = |name: &str| -> Result<(String, Vec<String>), Box<dyn Error>> {
            let probe = Probe::default();
            let scope = tenant_scope(name, CancellationToken::new())?;
            let seen = drive(caller(&scope, &probe))?;
            Ok((seen, probe.keys.take()))
        };
        let (ours, our_keys) = call(&first)?;
        let (theirs, their_keys) = call(&second)?;
        assert_eq!(ours, first, "the callee ran in the first caller's tenant");
        assert_eq!(theirs, second, "and in the second caller's");
        assert_eq!(our_keys.len(), 1, "the callee saw one key");
        assert_ne!(
            our_keys, their_keys,
            "two tenants never share the callee's key"
        );
        sim.record(&format!("tenant={first} key={our_keys:?}"));
        Ok(())
    })
}

/// `fail with` is permanent and `fail transiently with` is retryable, each
/// located at its flow and carrying its reason, so a caller classifies the
/// failure without reading the callee.
fn fail_is_classified(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let transient = sim.rng().chance(500);
        let reason = sim.rng().below(1_000);
        let scope = tenant_scope("acme", CancellationToken::new())?;
        let error = failure_of(
            drive(verdict(&scope, transient, reason)),
            "`fail` must fail the flow",
        )?;
        assert_eq!(
            error.is_retryable(),
            transient,
            "retryable exactly when written `transiently`: {error}"
        );
        assert!(
            matches!(
                (transient, &error),
                (true, FlowError::Transient { .. }) | (false, FlowError::Failed { .. })
            ),
            "the variant names the class: {error:?}"
        );
        assert_eq!(error.at(), "verdict", "located at the flow that failed");
        assert!(
            error.to_string().contains(&format!("reason {reason}")),
            "the reason survives: {error}"
        );
        sim.record(&format!("transient={transient} reason={reason}"));
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
    retry_budget_holds_band_00 => retry_budget_holds, 0;
    retry_budget_holds_band_01 => retry_budget_holds, 1;
    retry_budget_holds_band_02 => retry_budget_holds, 2;
    retry_budget_holds_band_03 => retry_budget_holds, 3;
    retry_budget_holds_band_04 => retry_budget_holds, 4;
    retry_budget_holds_band_05 => retry_budget_holds, 5;
    retry_budget_holds_band_06 => retry_budget_holds, 6;
    retry_budget_holds_band_07 => retry_budget_holds, 7;
    retry_budget_holds_band_08 => retry_budget_holds, 8;
    retry_budget_holds_band_09 => retry_budget_holds, 9;
    retry_budget_holds_band_10 => retry_budget_holds, 10;
    retry_budget_holds_band_11 => retry_budget_holds, 11;
    retry_budget_holds_band_12 => retry_budget_holds, 12;
    retry_budget_holds_band_13 => retry_budget_holds, 13;
    retry_budget_holds_band_14 => retry_budget_holds, 14;
    retry_budget_holds_band_15 => retry_budget_holds, 15;
    fan_machine_sized_band_00 => fan_machine_sized, 0;
    fan_machine_sized_band_01 => fan_machine_sized, 1;
    fan_machine_sized_band_02 => fan_machine_sized, 2;
    fan_machine_sized_band_03 => fan_machine_sized, 3;
    fan_machine_sized_band_04 => fan_machine_sized, 4;
    fan_machine_sized_band_05 => fan_machine_sized, 5;
    fan_machine_sized_band_06 => fan_machine_sized, 6;
    fan_machine_sized_band_07 => fan_machine_sized, 7;
    fan_machine_sized_band_08 => fan_machine_sized, 8;
    fan_machine_sized_band_09 => fan_machine_sized, 9;
    fan_machine_sized_band_10 => fan_machine_sized, 10;
    fan_machine_sized_band_11 => fan_machine_sized, 11;
    fan_machine_sized_band_12 => fan_machine_sized, 12;
    fan_machine_sized_band_13 => fan_machine_sized, 13;
    fan_machine_sized_band_14 => fan_machine_sized, 14;
    fan_machine_sized_band_15 => fan_machine_sized, 15;
    fan_out_typed_error_band_00 => fan_out_typed_error, 0;
    fan_out_typed_error_band_01 => fan_out_typed_error, 1;
    fan_out_typed_error_band_02 => fan_out_typed_error, 2;
    fan_out_typed_error_band_03 => fan_out_typed_error, 3;
    fan_out_typed_error_band_04 => fan_out_typed_error, 4;
    fan_out_typed_error_band_05 => fan_out_typed_error, 5;
    fan_out_typed_error_band_06 => fan_out_typed_error, 6;
    fan_out_typed_error_band_07 => fan_out_typed_error, 7;
    fan_out_typed_error_band_08 => fan_out_typed_error, 8;
    fan_out_typed_error_band_09 => fan_out_typed_error, 9;
    fan_out_typed_error_band_10 => fan_out_typed_error, 10;
    fan_out_typed_error_band_11 => fan_out_typed_error, 11;
    fan_out_typed_error_band_12 => fan_out_typed_error, 12;
    fan_out_typed_error_band_13 => fan_out_typed_error, 13;
    fan_out_typed_error_band_14 => fan_out_typed_error, 14;
    fan_out_typed_error_band_15 => fan_out_typed_error, 15;
    fan_out_bounded_ordered_band_00 => fan_out_bounded_ordered, 0;
    fan_out_bounded_ordered_band_01 => fan_out_bounded_ordered, 1;
    fan_out_bounded_ordered_band_02 => fan_out_bounded_ordered, 2;
    fan_out_bounded_ordered_band_03 => fan_out_bounded_ordered, 3;
    fan_out_bounded_ordered_band_04 => fan_out_bounded_ordered, 4;
    fan_out_bounded_ordered_band_05 => fan_out_bounded_ordered, 5;
    fan_out_bounded_ordered_band_06 => fan_out_bounded_ordered, 6;
    fan_out_bounded_ordered_band_07 => fan_out_bounded_ordered, 7;
    fan_out_bounded_ordered_band_08 => fan_out_bounded_ordered, 8;
    fan_out_bounded_ordered_band_09 => fan_out_bounded_ordered, 9;
    fan_out_bounded_ordered_band_10 => fan_out_bounded_ordered, 10;
    fan_out_bounded_ordered_band_11 => fan_out_bounded_ordered, 11;
    fan_out_bounded_ordered_band_12 => fan_out_bounded_ordered, 12;
    fan_out_bounded_ordered_band_13 => fan_out_bounded_ordered, 13;
    fan_out_bounded_ordered_band_14 => fan_out_bounded_ordered, 14;
    fan_out_bounded_ordered_band_15 => fan_out_bounded_ordered, 15;
    fan_out_deadline_band_00 => fan_out_deadline, 0;
    fan_out_deadline_band_01 => fan_out_deadline, 1;
    fan_out_deadline_band_02 => fan_out_deadline, 2;
    fan_out_deadline_band_03 => fan_out_deadline, 3;
    fan_out_deadline_band_04 => fan_out_deadline, 4;
    fan_out_deadline_band_05 => fan_out_deadline, 5;
    fan_out_deadline_band_06 => fan_out_deadline, 6;
    fan_out_deadline_band_07 => fan_out_deadline, 7;
    fan_out_deadline_band_08 => fan_out_deadline, 8;
    fan_out_deadline_band_09 => fan_out_deadline, 9;
    fan_out_deadline_band_10 => fan_out_deadline, 10;
    fan_out_deadline_band_11 => fan_out_deadline, 11;
    fan_out_deadline_band_12 => fan_out_deadline, 12;
    fan_out_deadline_band_13 => fan_out_deadline, 13;
    fan_out_deadline_band_14 => fan_out_deadline, 14;
    fan_out_deadline_band_15 => fan_out_deadline, 15;
    fan_out_refusals_and_drop_band_00 => fan_out_refusals_and_drop, 0;
    fan_out_refusals_and_drop_band_01 => fan_out_refusals_and_drop, 1;
    fan_out_refusals_and_drop_band_02 => fan_out_refusals_and_drop, 2;
    fan_out_refusals_and_drop_band_03 => fan_out_refusals_and_drop, 3;
    fan_out_refusals_and_drop_band_04 => fan_out_refusals_and_drop, 4;
    fan_out_refusals_and_drop_band_05 => fan_out_refusals_and_drop, 5;
    fan_out_refusals_and_drop_band_06 => fan_out_refusals_and_drop, 6;
    fan_out_refusals_and_drop_band_07 => fan_out_refusals_and_drop, 7;
    fan_out_refusals_and_drop_band_08 => fan_out_refusals_and_drop, 8;
    fan_out_refusals_and_drop_band_09 => fan_out_refusals_and_drop, 9;
    fan_out_refusals_and_drop_band_10 => fan_out_refusals_and_drop, 10;
    fan_out_refusals_and_drop_band_11 => fan_out_refusals_and_drop, 11;
    fan_out_refusals_and_drop_band_12 => fan_out_refusals_and_drop, 12;
    fan_out_refusals_and_drop_band_13 => fan_out_refusals_and_drop, 13;
    fan_out_refusals_and_drop_band_14 => fan_out_refusals_and_drop, 14;
    fan_out_refusals_and_drop_band_15 => fan_out_refusals_and_drop, 15;
    together_owned_band_00 => together_owned, 0;
    together_owned_band_01 => together_owned, 1;
    together_owned_band_02 => together_owned, 2;
    together_owned_band_03 => together_owned, 3;
    together_owned_band_04 => together_owned, 4;
    together_owned_band_05 => together_owned, 5;
    together_owned_band_06 => together_owned, 6;
    together_owned_band_07 => together_owned, 7;
    together_owned_band_08 => together_owned, 8;
    together_owned_band_09 => together_owned, 9;
    together_owned_band_10 => together_owned, 10;
    together_owned_band_11 => together_owned, 11;
    together_owned_band_12 => together_owned, 12;
    together_owned_band_13 => together_owned, 13;
    together_owned_band_14 => together_owned, 14;
    together_owned_band_15 => together_owned, 15;
    together_fail_fast_band_00 => together_fail_fast, 0;
    together_fail_fast_band_01 => together_fail_fast, 1;
    together_fail_fast_band_02 => together_fail_fast, 2;
    together_fail_fast_band_03 => together_fail_fast, 3;
    together_fail_fast_band_04 => together_fail_fast, 4;
    together_fail_fast_band_05 => together_fail_fast, 5;
    together_fail_fast_band_06 => together_fail_fast, 6;
    together_fail_fast_band_07 => together_fail_fast, 7;
    together_fail_fast_band_08 => together_fail_fast, 8;
    together_fail_fast_band_09 => together_fail_fast, 9;
    together_fail_fast_band_10 => together_fail_fast, 10;
    together_fail_fast_band_11 => together_fail_fast, 11;
    together_fail_fast_band_12 => together_fail_fast, 12;
    together_fail_fast_band_13 => together_fail_fast, 13;
    together_fail_fast_band_14 => together_fail_fast, 14;
    together_fail_fast_band_15 => together_fail_fast, 15;
    step_own_key_band_00 => step_own_key, 0;
    step_own_key_band_01 => step_own_key, 1;
    step_own_key_band_02 => step_own_key, 2;
    step_own_key_band_03 => step_own_key, 3;
    step_own_key_band_04 => step_own_key, 4;
    step_own_key_band_05 => step_own_key, 5;
    step_own_key_band_06 => step_own_key, 6;
    step_own_key_band_07 => step_own_key, 7;
    step_own_key_band_08 => step_own_key, 8;
    step_own_key_band_09 => step_own_key, 9;
    step_own_key_band_10 => step_own_key, 10;
    step_own_key_band_11 => step_own_key, 11;
    step_own_key_band_12 => step_own_key, 12;
    step_own_key_band_13 => step_own_key, 13;
    step_own_key_band_14 => step_own_key, 14;
    step_own_key_band_15 => step_own_key, 15;
    step_failure_located_band_00 => step_failure_located, 0;
    step_failure_located_band_01 => step_failure_located, 1;
    step_failure_located_band_02 => step_failure_located, 2;
    step_failure_located_band_03 => step_failure_located, 3;
    step_failure_located_band_04 => step_failure_located, 4;
    step_failure_located_band_05 => step_failure_located, 5;
    step_failure_located_band_06 => step_failure_located, 6;
    step_failure_located_band_07 => step_failure_located, 7;
    step_failure_located_band_08 => step_failure_located, 8;
    step_failure_located_band_09 => step_failure_located, 9;
    step_failure_located_band_10 => step_failure_located, 10;
    step_failure_located_band_11 => step_failure_located, 11;
    step_failure_located_band_12 => step_failure_located, 12;
    step_failure_located_band_13 => step_failure_located, 13;
    step_failure_located_band_14 => step_failure_located, 14;
    step_failure_located_band_15 => step_failure_located, 15;
    for_scope_per_item_band_00 => for_scope_per_item, 0;
    for_scope_per_item_band_01 => for_scope_per_item, 1;
    for_scope_per_item_band_02 => for_scope_per_item, 2;
    for_scope_per_item_band_03 => for_scope_per_item, 3;
    for_scope_per_item_band_04 => for_scope_per_item, 4;
    for_scope_per_item_band_05 => for_scope_per_item, 5;
    for_scope_per_item_band_06 => for_scope_per_item, 6;
    for_scope_per_item_band_07 => for_scope_per_item, 7;
    for_scope_per_item_band_08 => for_scope_per_item, 8;
    for_scope_per_item_band_09 => for_scope_per_item, 9;
    for_scope_per_item_band_10 => for_scope_per_item, 10;
    for_scope_per_item_band_11 => for_scope_per_item, 11;
    for_scope_per_item_band_12 => for_scope_per_item, 12;
    for_scope_per_item_band_13 => for_scope_per_item, 13;
    for_scope_per_item_band_14 => for_scope_per_item, 14;
    for_scope_per_item_band_15 => for_scope_per_item, 15;
    for_in_order_band_00 => for_in_order, 0;
    for_in_order_band_01 => for_in_order, 1;
    for_in_order_band_02 => for_in_order, 2;
    for_in_order_band_03 => for_in_order, 3;
    for_in_order_band_04 => for_in_order, 4;
    for_in_order_band_05 => for_in_order, 5;
    for_in_order_band_06 => for_in_order, 6;
    for_in_order_band_07 => for_in_order, 7;
    for_in_order_band_08 => for_in_order, 8;
    for_in_order_band_09 => for_in_order, 9;
    for_in_order_band_10 => for_in_order, 10;
    for_in_order_band_11 => for_in_order, 11;
    for_in_order_band_12 => for_in_order, 12;
    for_in_order_band_13 => for_in_order, 13;
    for_in_order_band_14 => for_in_order, 14;
    for_in_order_band_15 => for_in_order, 15;
    let_fails_before_binding_band_00 => let_fails_before_binding, 0;
    let_fails_before_binding_band_01 => let_fails_before_binding, 1;
    let_fails_before_binding_band_02 => let_fails_before_binding, 2;
    let_fails_before_binding_band_03 => let_fails_before_binding, 3;
    let_fails_before_binding_band_04 => let_fails_before_binding, 4;
    let_fails_before_binding_band_05 => let_fails_before_binding, 5;
    let_fails_before_binding_band_06 => let_fails_before_binding, 6;
    let_fails_before_binding_band_07 => let_fails_before_binding, 7;
    let_fails_before_binding_band_08 => let_fails_before_binding, 8;
    let_fails_before_binding_band_09 => let_fails_before_binding, 9;
    let_fails_before_binding_band_10 => let_fails_before_binding, 10;
    let_fails_before_binding_band_11 => let_fails_before_binding, 11;
    let_fails_before_binding_band_12 => let_fails_before_binding, 12;
    let_fails_before_binding_band_13 => let_fails_before_binding, 13;
    let_fails_before_binding_band_14 => let_fails_before_binding, 14;
    let_fails_before_binding_band_15 => let_fails_before_binding, 15;
    run_in_callers_tenant_band_00 => run_in_callers_tenant, 0;
    run_in_callers_tenant_band_01 => run_in_callers_tenant, 1;
    run_in_callers_tenant_band_02 => run_in_callers_tenant, 2;
    run_in_callers_tenant_band_03 => run_in_callers_tenant, 3;
    run_in_callers_tenant_band_04 => run_in_callers_tenant, 4;
    run_in_callers_tenant_band_05 => run_in_callers_tenant, 5;
    run_in_callers_tenant_band_06 => run_in_callers_tenant, 6;
    run_in_callers_tenant_band_07 => run_in_callers_tenant, 7;
    run_in_callers_tenant_band_08 => run_in_callers_tenant, 8;
    run_in_callers_tenant_band_09 => run_in_callers_tenant, 9;
    run_in_callers_tenant_band_10 => run_in_callers_tenant, 10;
    run_in_callers_tenant_band_11 => run_in_callers_tenant, 11;
    run_in_callers_tenant_band_12 => run_in_callers_tenant, 12;
    run_in_callers_tenant_band_13 => run_in_callers_tenant, 13;
    run_in_callers_tenant_band_14 => run_in_callers_tenant, 14;
    run_in_callers_tenant_band_15 => run_in_callers_tenant, 15;
    fail_is_classified_band_00 => fail_is_classified, 0;
    fail_is_classified_band_01 => fail_is_classified, 1;
    fail_is_classified_band_02 => fail_is_classified, 2;
    fail_is_classified_band_03 => fail_is_classified, 3;
    fail_is_classified_band_04 => fail_is_classified, 4;
    fail_is_classified_band_05 => fail_is_classified, 5;
    fail_is_classified_band_06 => fail_is_classified, 6;
    fail_is_classified_band_07 => fail_is_classified, 7;
    fail_is_classified_band_08 => fail_is_classified, 8;
    fail_is_classified_band_09 => fail_is_classified, 9;
    fail_is_classified_band_10 => fail_is_classified, 10;
    fail_is_classified_band_11 => fail_is_classified, 11;
    fail_is_classified_band_12 => fail_is_classified, 12;
    fail_is_classified_band_13 => fail_is_classified, 13;
    fail_is_classified_band_14 => fail_is_classified, 14;
    fail_is_classified_band_15 => fail_is_classified, 15;
}
