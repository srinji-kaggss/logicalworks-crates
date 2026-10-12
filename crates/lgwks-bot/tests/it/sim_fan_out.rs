//! Seeded simulation of [`FanOut`], the one-call bounded fan-out with typed
//! errors, through its public interface.
//!
//! One seed draws each scenario — how many items, their values, the bound (or
//! the runtime's), which items fail — and every body yields once before it
//! answers, so bodies are genuinely in flight together rather than finishing
//! inside the poll that started them. Each family asserts what a caller sees:
//!
//! - every value comes back, in input order, and every item ran exactly once;
//! - a failing item comes back as the caller's own error with its position,
//!   one of the items that did fail;
//! - one at a time, the first failure is the lowest failing position and no
//!   later item starts;
//! - bodies in flight never exceed the bound;
//! - a bound of zero or past `MAX_IN_FLIGHT` is refused before any body runs,
//!   and `MAX_IN_FLIGHT` itself is admitted;
//! - an empty input runs nothing; `run_in` under a caller's scope agrees with
//!   `run`; the error reads as the item and its cause, and its `source` is the
//!   caller's error;
//! - the same seed replays the same outcomes and distinct seeds diverge.
//!
//! Sources: structured concurrency (Trio nurseries, JEP 453 `ShutdownOnFailure`:
//! the first failure cancels the rest and is the one reported) and the
//! acceptance rule that a common task body needs no shared cell to return a
//! typed error.

#![cfg(feature = "script")]

use std::future::Future;
use std::num::TryFromIntError;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};

use lgwks_bot::script::{FanOut, FanOutError, MAX_IN_FLIGHT, Scope, Tenant};

use crate::sim::{Rng, Trace};

/// The crate's test idiom: `Result` and `?`.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The seeds every family replays.
const SEEDS: [u64; 4] = [
    0xFA40_0000_0000_0001,
    0xFA40_0000_0000_0002,
    0xFA40_FFFF_0000_0003,
    0x0D1E_FA40_0000_0004,
];

/// Scenarios each family draws per seed.
const SCENARIOS: usize = 32;

/// The caller's own error: which item refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Refused(u32);

impl std::fmt::Display for Refused {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "refused {}", self.0)
    }
}

impl std::error::Error for Refused {}

/// A future that returns `Pending` once, waking itself, and then resolves.
struct Yield(bool);

impl Future for Yield {
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();
        if this.0 {
            return Poll::Ready(());
        }
        this.0 = true;
        context.waker().wake_by_ref();
        Poll::Pending
    }
}

/// One drawn fan-out.
struct Scenario {
    /// The input, in order.
    items: Vec<u32>,
    /// The written bound, or `None` for the runtime's.
    limit: Option<usize>,
    /// Which positions fail.
    failing: Vec<bool>,
}

impl Scenario {
    /// A scenario drawn from `rng`; `fail_one_in` of zero fails nothing, and
    /// otherwise each position fails with those odds and at least one does.
    fn draw(rng: &mut Rng, fail_one_in: u32) -> Result<Self, TryFromIntError> {
        let len = usize::try_from(rng.below(160))?;
        let items: Vec<u32> = (0..len).map(|_| rng.below(1_000_000)).collect();
        let limit = match rng.below(3) {
            0 => None,
            _ => Some(usize::try_from(rng.between(1, 48))?),
        };
        let mut failing: Vec<bool> = (0..len)
            .map(|_| fail_one_in > 0 && rng.below(fail_one_in) == 0)
            .collect();
        if fail_one_in > 0 && !failing.contains(&true) {
            let forced = usize::try_from(rng.below(u32::try_from(len)?))?;
            if let Some(slot) = failing.get_mut(forced) {
                *slot = true;
            }
        }
        Ok(Self {
            items,
            limit,
            failing,
        })
    }

    /// The same items and failures under another bound.
    fn bounded(&self, limit: Option<usize>) -> Self {
        Self {
            items: self.items.clone(),
            limit,
            failing: self.failing.clone(),
        }
    }

    /// Whether position `index` fails.
    fn fails(&self, index: usize) -> bool {
        self.failing.get(index).copied() == Some(true)
    }

    /// The fan-out this scenario declares, over `(index, item)` pairs.
    fn fan_out(&self) -> FanOut<std::iter::Enumerate<std::vec::IntoIter<u32>>> {
        let fan_out = FanOut::new(self.items.clone().into_iter().enumerate());
        match self.limit {
            Some(limit) => fan_out.at_most(limit),
            None => fan_out,
        }
    }
}

/// What the bodies of one run observed.
#[derive(Default)]
struct Watch {
    /// Bodies started.
    started: AtomicUsize,
    /// Bodies in flight now.
    live: AtomicUsize,
    /// The most bodies ever in flight at once.
    peak: AtomicUsize,
}

impl Watch {
    /// One body's life: counted in, yielded once, counted out.
    async fn body(&self, scenario: &Scenario, index: usize, item: u32) -> Result<u64, Refused> {
        self.started.fetch_add(1, Ordering::Relaxed);
        let now = self.live.fetch_add(1, Ordering::Relaxed).saturating_add(1);
        self.peak.fetch_max(now, Ordering::Relaxed);
        Yield(false).await;
        self.live.fetch_sub(1, Ordering::Relaxed);
        if scenario.fails(index) {
            Err(Refused(item))
        } else {
            Ok(u64::from(item).saturating_mul(2))
        }
    }
}

/// Run `scenario` and return its outcome with what the bodies observed.
fn run(scenario: &Scenario) -> (Result<Vec<u64>, FanOutError<Refused>>, Watch) {
    let watch = Watch::default();
    let outcome = lgwks_bot::block_on(
        scenario
            .fan_out()
            .run(|(index, item)| watch.body(scenario, index, item)),
    );
    (outcome, watch)
}

/// Every drawn scenario under each seed.
fn sweep(fail_one_in: u32, mut check: impl FnMut(u64, &Scenario) -> TestResult) -> TestResult {
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        for _ in 0..SCENARIOS {
            let scenario = Scenario::draw(&mut rng, fail_one_in)?;
            check(seed, &scenario)?;
        }
    }
    Ok(())
}

#[test]
/// Every value comes back in input order, and every item ran exactly once.
fn sim_values_come_back_in_input_order() -> TestResult {
    sweep(0, |seed, scenario| {
        let (outcome, watch) = run(scenario);
        let values = outcome.map_err(|error| format!("seed {seed:#x}: {error}"))?;
        let expected: Vec<u64> = scenario
            .items
            .iter()
            .map(|&item| u64::from(item).saturating_mul(2))
            .collect();
        assert_eq!(values, expected, "seed {seed:#x}: values in input order");
        assert_eq!(
            watch.started.load(Ordering::Relaxed),
            scenario.items.len(),
            "seed {seed:#x}: every item ran exactly once"
        );
        Ok(())
    })
}

#[test]
/// A failing item comes back as the caller's own error, at a position that
/// did fail.
fn sim_a_failure_comes_back_as_the_callers_own_error() -> TestResult {
    sweep(6, |seed, scenario| {
        let (outcome, _) = run(scenario);
        match outcome {
            Ok(values) if scenario.items.is_empty() => {
                assert!(values.is_empty(), "seed {seed:#x}: nothing in, nothing out");
                Ok(())
            }
            Err(FanOutError::Item { index, error }) => {
                assert!(scenario.fails(index), "seed {seed:#x}: #{index} did not fail");
                assert_eq!(
                    Some(&error.0),
                    scenario.items.get(index),
                    "seed {seed:#x}: the error is the item's own"
                );
                Ok(())
            }
            other => Err(format!("seed {seed:#x}: expected an item failure, got {other:?}").into()),
        }
    })
}

#[test]
/// One at a time, the first failure is the lowest failing position and no
/// later item is started.
fn sim_one_at_a_time_stops_at_the_lowest_failure() -> TestResult {
    sweep(6, |seed, drawn| {
        let scenario = drawn.bounded(Some(1));
        let Some(lowest) = scenario.failing.iter().position(|&fails| fails) else {
            return Ok(());
        };
        let (outcome, watch) = run(&scenario);
        match outcome {
            Err(FanOutError::Item { index, .. }) => {
                assert_eq!(index, lowest, "seed {seed:#x}: the lowest failure wins");
                assert_eq!(
                    watch.started.load(Ordering::Relaxed),
                    lowest.saturating_add(1),
                    "seed {seed:#x}: nothing after the failure started"
                );
                Ok(())
            }
            other => {
                Err(format!("seed {seed:#x}: expected #{lowest} to fail, got {other:?}").into())
            }
        }
    })
}

#[test]
/// Bodies in flight never exceed the written bound.
fn sim_in_flight_never_exceeds_the_bound() -> TestResult {
    sweep(0, |seed, scenario| {
        let (outcome, watch) = run(scenario);
        outcome.map_err(|error| format!("seed {seed:#x}: {error}"))?;
        let peak = watch.peak.load(Ordering::Relaxed);
        if let Some(limit) = scenario.limit {
            assert!(peak <= limit, "seed {seed:#x}: {peak} in flight past {limit}");
        }
        assert_eq!(
            peak > 0,
            !scenario.items.is_empty(),
            "seed {seed:#x}: a body ran exactly when there was an item"
        );
        assert_eq!(
            watch.live.load(Ordering::Relaxed),
            0,
            "seed {seed:#x}: none left in flight"
        );
        Ok(())
    })
}

#[test]
/// A bound of zero or past `MAX_IN_FLIGHT` is refused before any body runs.
fn sim_an_out_of_range_bound_is_refused_before_any_body_runs() -> TestResult {
    sweep(0, |seed, drawn| {
        let past = MAX_IN_FLIGHT
            .saturating_add(1)
            .saturating_add(drawn.items.len());
        for limit in [0, past] {
            let scenario = drawn.bounded(Some(limit));
            let (outcome, watch) = run(&scenario);
            assert!(
                matches!(outcome, Err(FanOutError::Flow(_))),
                "seed {seed:#x}: a bound of {limit} is refused, got {outcome:?}"
            );
            assert_eq!(
                watch.started.load(Ordering::Relaxed),
                0,
                "seed {seed:#x}: nothing ran"
            );
        }
        Ok(())
    })
}

#[test]
/// `MAX_IN_FLIGHT` itself is a bound, and every value still comes back.
fn sim_the_ceiling_bound_is_admitted() -> TestResult {
    sweep(0, |seed, drawn| {
        let scenario = drawn.bounded(Some(MAX_IN_FLIGHT));
        let (outcome, _) = run(&scenario);
        let values = outcome.map_err(|error| format!("seed {seed:#x}: {error}"))?;
        assert_eq!(
            values.len(),
            scenario.items.len(),
            "seed {seed:#x}: every value back"
        );
        Ok(())
    })
}

#[test]
/// An empty input runs nothing and returns nothing, under any bound in range.
fn sim_an_empty_input_runs_nothing() -> TestResult {
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        for _ in 0..SCENARIOS {
            let scenario = Scenario {
                items: Vec::new(),
                limit: Some(usize::try_from(rng.between(1, 4096))?),
                failing: Vec::new(),
            };
            let (outcome, watch) = run(&scenario);
            let values = outcome.map_err(|error| format!("seed {seed:#x}: {error}"))?;
            assert!(values.is_empty(), "seed {seed:#x}: nothing out");
            assert_eq!(
                watch.started.load(Ordering::Relaxed),
                0,
                "seed {seed:#x}: nothing ran"
            );
        }
    }
    Ok(())
}

#[test]
/// `run_in` under a caller's scope returns what `run` returns.
fn sim_run_in_agrees_with_run() -> TestResult {
    let mut tenant_number = 0_u32;
    sweep(0, |seed, scenario| {
        tenant_number = tenant_number.saturating_add(1);
        let scope = Scope::root(Tenant::new(&format!("tenant-{tenant_number}"))?);
        let watch = Watch::default();
        let scoped = lgwks_bot::block_on(scenario.fan_out().run_in(
            &scope,
            "fan",
            |(index, item)| watch.body(scenario, index, item),
        ))
        .map_err(|error| format!("seed {seed:#x}: {error}"))?;
        let (plain, _) = run(scenario);
        let plain = plain.map_err(|error| format!("seed {seed:#x}: {error}"))?;
        assert_eq!(scoped, plain, "seed {seed:#x}: scoped and standalone agree");
        Ok(())
    })
}

#[test]
/// The error reads as the item and its cause, and its source is the caller's
/// error.
fn sim_an_item_error_reads_as_the_item_and_its_cause() -> TestResult {
    sweep(4, |seed, scenario| {
        let (outcome, _) = run(scenario);
        let Err(failure) = outcome else {
            return Ok(());
        };
        let FanOutError::Item { index, error } = failure else {
            return Err(format!("seed {seed:#x}: expected an item failure, got {failure:?}").into());
        };
        let rebuilt: FanOutError<Refused> = FanOutError::Item { index, error };
        assert_eq!(
            rebuilt.to_string(),
            format!("item #{index} failed: refused {}", error.0),
            "seed {seed:#x}: the message names the item and its cause"
        );
        let source = std::error::Error::source(&rebuilt)
            .and_then(|source| source.downcast_ref::<Refused>());
        assert_eq!(
            source,
            Some(&error),
            "seed {seed:#x}: the source is the caller's error"
        );
        Ok(())
    })
}

/// The replay receipt of every scenario's outcome under `seed`.
fn outcome_trace(seed: u64) -> Result<u64, TryFromIntError> {
    let mut rng = Rng::new(seed);
    let mut trace = Trace::new();
    for _ in 0..SCENARIOS {
        let scenario = Scenario::draw(&mut rng, 5)?;
        let (outcome, watch) = run(&scenario);
        match outcome {
            Ok(values) => trace.record_number("ok", values.len()),
            Err(FanOutError::Item { index, error }) => {
                trace.record_number("item", index);
                trace.record_number("refused", error.0);
            }
            Err(other) => trace.record(&format!("other {other}")),
        }
        if scenario.limit == Some(1) {
            trace.record_number("started", watch.started.load(Ordering::Relaxed));
        }
    }
    Ok(trace.hash())
}

#[test]
/// The replay oracle: one seed, one outcome trace.
fn sim_the_same_seed_replays_the_same_outcomes() -> TestResult {
    for seed in SEEDS {
        assert_eq!(
            outcome_trace(seed)?,
            outcome_trace(seed)?,
            "seed {seed:#x} replays"
        );
    }
    Ok(())
}

#[test]
/// The divergence oracle: a sweep that ignored its seed would pass the replay.
fn sim_distinct_seeds_draw_different_outcomes() -> TestResult {
    assert_ne!(
        outcome_trace(SEEDS[0])?,
        outcome_trace(SEEDS[1])?,
        "two seeds drew alike"
    );
    Ok(())
}
