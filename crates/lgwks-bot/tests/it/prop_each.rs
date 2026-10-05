//! Property tests over `script::each`, with shrinking (#273).
//!
//! The input is a list of bodies (how many polls each stays pending, and
//! whether it fails) and a fan-out limit. Whatever the mix, `each` must hand
//! back outputs in input order, never hold more bodies than the limit, and,
//! once a body fails, start no further item and report that failure at its
//! item's path. A failing mix shrinks to the smallest one that still fails.
#![cfg(feature = "script")]

use crate::prop;

use std::cell::Cell;
use std::num::NonZeroUsize;
use std::task::Poll;

use lgwks_bot::script::{FlowError, Scope, Tenant, each};
use prop::{Outcome, check};
use proptest::collection::vec;
use proptest::prelude::Strategy;
use proptest::test_runner::{TestCaseError, TestRunner};

/// The seed every run starts from. Changing it is a new corpus, not a retry.
const SEED: u64 = 0x2730_c0de_c5ee_d004;

/// A runner for this target's properties.
fn runner() -> TestRunner {
    prop::runner(SEED, 256, file!())
}

/// One body: how long it stays pending and whether it fails at the end.
#[derive(Debug, Clone, Copy)]
struct Body {
    /// Polls before it finishes, each one ending in a self-wake.
    turns: u8,
    /// Whether it ends in a permanent failure.
    fails: bool,
}

/// What the bodies report about themselves, borrowed by every body.
#[derive(Default)]
struct Probe {
    /// Bodies started and not yet finished.
    live: Cell<usize>,
    /// The most ever live at once.
    peak: Cell<usize>,
    /// Bodies started.
    started: Cell<usize>,
    /// The first failing item, and `started` at the moment it failed.
    first_failure: Cell<Option<(usize, usize)>>,
}

impl Probe {
    /// Record a start.
    fn start(&self) {
        self.live.set(self.live.get().saturating_add(1));
        self.peak.set(self.peak.get().max(self.live.get()));
        self.started.set(self.started.get().saturating_add(1));
    }

    /// Record an end, and the first failure with the start count it saw.
    fn end(&self, index: usize, fails: bool) {
        self.live.set(self.live.get().saturating_sub(1));
        if fails && self.first_failure.get().is_none() {
            self.first_failure.set(Some((index, self.started.get())));
        }
    }
}

/// Run one body to its end, reporting to `probe`.
async fn run_body(probe: &Probe, index: usize, body: Body) -> Result<usize, FlowError> {
    probe.start();
    let mut left = body.turns;
    std::future::poll_fn(|context| {
        let Some(fewer) = left.checked_sub(1) else {
            return Poll::Ready(());
        };
        left = fewer;
        context.waker().wake_by_ref();
        Poll::Pending
    })
    .await;
    probe.end(index, body.fails);
    if body.fails {
        lgwks_std::trace::debug!(index, "run_body: the item fails, as generated");
        return Err(FlowError::failed(format!("item {index} failed")));
    }
    Ok(index.saturating_mul(2))
}

/// A fan-out under test: run every body under `limit`, outputs in order.
type FanOut = fn(&Probe, NonZeroUsize, &[Body]) -> Result<Vec<usize>, FlowError>;

/// The shipped `each`, for tenant `prop`.
fn shipped(probe: &Probe, limit: NonZeroUsize, bodies: &[Body]) -> Result<Vec<usize>, FlowError> {
    let scope = Scope::root(Tenant::new("prop")?);
    lgwks_bot::block_on(each(
        &scope,
        "fan",
        Some(limit),
        bodies.iter().copied().enumerate(),
        |_item_scope, (index, body)| run_body(probe, index, body),
    ))
}

/// Order, bound, and fail-fast, over one mix.
fn each_property(
    fan_out: FanOut,
    limit: NonZeroUsize,
    bodies: &[Body],
) -> Result<(), TestCaseError> {
    let probe = Probe::default();
    let outcome = fan_out(&probe, limit, bodies);
    check(probe.peak.get() <= limit.get(), || {
        format!(
            "{} bodies were live under a limit of {limit}",
            probe.peak.get()
        )
    })?;
    match (outcome, probe.first_failure.get()) {
        (Ok(outputs), None) => {
            let expected: Vec<usize> = (0..bodies.len())
                .map(|index| index.saturating_mul(2))
                .collect();
            check(outputs == expected, || {
                format!("outputs {outputs:?}, expected {expected:?}")
            })
        }
        (Err(error), Some((index, started_then))) => {
            check(probe.started.get() == started_then, || {
                format!(
                    "{} items started, {started_then} had when item {index} failed",
                    probe.started.get()
                )
            })?;
            let text = error.to_string();
            check(text.contains(&format!("#{index}")), || {
                format!("item {index} failed first but the error reads {text:?}")
            })
        }
        (outcome, first) => Err(TestCaseError::fail(format!(
            "outcome {outcome:?} disagrees with the first failure {first:?}"
        ))),
    }
}

/// Mixes of up to twenty bodies under a limit of one to eight.
fn mixes() -> impl Strategy<Value = (NonZeroUsize, Vec<Body>)> {
    let body =
        (0u8..6, proptest::bool::weighted(0.1)).prop_map(|(turns, fails)| Body { turns, fails });
    let limit = (1usize..=8).prop_filter_map("a non-zero limit", NonZeroUsize::new);
    (limit, vec(body, 0..20))
}

#[test]
fn each_keeps_order_and_bound_and_stops_at_the_first_failure() -> Outcome {
    runner().run(&mixes(), |(limit, bodies)| {
        each_property(shipped, limit, &bodies)
    })?;
    Ok(())
}

#[test]
fn the_property_catches_a_fan_out_that_ignores_its_limit() -> Outcome {
    /// Starts every body at once: the unbounded join the limit exists to stop.
    fn unbounded(
        probe: &Probe,
        _limit: NonZeroUsize,
        bodies: &[Body],
    ) -> Result<Vec<usize>, FlowError> {
        let futures: Vec<_> = bodies
            .iter()
            .copied()
            .enumerate()
            .map(|(index, body)| run_body(probe, index, body))
            .collect();
        lgwks_std::task::block_on(lgwks_std::task::join_all(futures))
            .into_iter()
            .collect()
    }
    let minimal = prop::shrunk(SEED, &mixes(), |(limit, bodies)| {
        each_property(unbounded, limit, &bodies)
    })?;
    let shape: (usize, Vec<(u8, bool)>) = (
        minimal.0.get(),
        minimal
            .1
            .iter()
            .map(|body| (body.turns, body.fails))
            .collect(),
    );
    assert_eq!(
        shape,
        (1, vec![(1, false), (0, false)]),
        "two bodies under a limit of one, the first still pending"
    );
    Ok(())
}
