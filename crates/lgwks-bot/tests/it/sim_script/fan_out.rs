//! The FanOut families: a body's typed error comes back unchanged, output stays bounded and ordered, a deadline ends the fan, and refusals and a drop release every body.

use super::*;

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

sixteen_bands!(
    fan_out_typed_error,
    fan_out_bounded_ordered,
    fan_out_deadline,
    fan_out_refusals_and_drop,
);
