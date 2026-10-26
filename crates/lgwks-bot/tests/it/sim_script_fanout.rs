//! Simulation family: machine-sized and typed fan-outs.
//!
//! The fan-out words beyond the written bound: `each` with no bound stays
//! under the machine-sized ceiling, and `FanOut` returns the first failing
//! item's own error and position, keeps input order under its bound, meets
//! its deadline, and refuses an unusable bound before any body starts.
//! Every scenario runs flows the real `script!` macro expanded, on the real
//! `lgwks_bot::script` runtime. Nothing here reimplements a fan-out: what the
//! seed controls is the input, the bound, which item fails and how, and the
//! deadline. Time is consulted only by the deadline family, and there only to
//! assert the refusal, never a duration.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `fan_machine_sized` | `each` with no written bound keeps input order and never exceeds 64 bodies per core |
//! | `fan_out_typed_error` | `FanOut` returns the first failing item's own error and position, with nothing left running |
//! | `fan_out_bounded_ordered` | `FanOut` keeps input order and never exceeds its written bound |
//! | `fan_out_deadline` | a `FanOut` past its deadline is `TimedOut`, starts no more than its bound, and drops every body |
//! | `fan_out_refusals_and_drop` | a bound of zero or past the ceiling is refused before any body starts; dropping the future drops every body |
//!
//! Each declared test sweeps its band twice and requires identical trace
//! hashes, so a nondeterministic run fails even when every assertion passes.

#![cfg(feature = "script")]

use std::error::Error;
use std::future::Future;
use std::task::{Context, Waker};
use std::time::Duration;

use lgwks_bot::rt::sync::CancellationToken;
use lgwks_bot::script::{FanOut, FanOutError, FlowError, MAX_IN_FLIGHT};

use crate::band_family;
use crate::sim::{self, Band};
use crate::sim_script::{
    Item, Probe, Turns, doubled_sized, drive, items, seeded_limit, tenant_scope,
};

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

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
        let ceiling = std::thread::available_parallelism()?
            .get()
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

band_family::band_family! {
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
}
