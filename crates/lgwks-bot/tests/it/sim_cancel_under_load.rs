//! Simulation family: a thousand flows in flight, cancelled at a seeded point,
//! every one accounted for (#278 row 7).
//!
//! Every scenario drives the **real** [`Host`]: its admission semaphore, its
//! stop, its task-local scopes and its reports. A seed draws the admission
//! ceiling, each flow's step count, and the driver poll at which the host is
//! cancelled. The flows are polled together on one thread, so the cancel lands
//! at the same structural point on every replay, whatever the machine's load.
//!
//! After the cancel the property is conservation, flow by flow:
//!
//! - every flow produced a report, and its disposition is `Succeeded`,
//!   `Cancelled` or `Refused`, never a failure the cancel was turned into;
//! - a `Succeeded` flow ran every step and returned its own input;
//! - a `Refused` flow's body never started, and no body starts after the stop
//!   (each body reads the driver's stop flag on its first poll and counts a
//!   late start);
//! - every admitted flow started its body, and so did every `Cancelled` one: a
//!   run admitted and then refused entry at its first step is the defect below,
//!   and this oracle sees it whatever order the stop's wakes arrived in;
//! - a run still queued at the stop is `Refused` whatever order the stop's
//!   wakes arrive in: a permit a stopped run releases is not an admission for a
//!   queued one. Before that rule a queued run polled after a release was
//!   counted as admitted and reported `Cancelled`, decided by the wake order of
//!   the channel under the token, and the same seed did not replay;
//! - the host's own counters agree with the reports: admitted plus refused is
//!   every flow, and the refusals are the `Refused` reports;
//! - no body outlives the call (a drop guard counts the live ones), and every
//!   permit is back;
//! - a run offered to the stopped host afterwards is refused, not dropped.
//!
//! The flows perform no external effect, so `OutcomeUnknown` (an effect whose
//! settlement is uncertain) cannot arise here; the journal's side of that
//! question is `durable_crash_observation` and `sim_tenant_journal_kill`.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `cancel_band_00..07` | 1,000 flows per seed over 1,000 seeds, each band swept twice for one trace hash, and every disposition reached in every band |
//! | `the_cancel_bands_cover_every_seed_exactly_once` | the bands partition the seed space |

#![cfg(feature = "script")]

use crate::sim;

use crate::load::drive;

use std::error::Error;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use lgwks_bot::script::{FlowError, Scope};
use lgwks_bot::task::{Disposition, Host, task};

use sim::Band;

/// What a scenario reports when a precondition did not hold.
type TestResult = Result<(), Box<dyn Error>>;

/// The seed space this family sweeps, and how many bands it is cut into.
const SEEDS: u64 = 1_000;
const PARTS: u64 = 8;

/// How many flows are in flight in every scenario.
const FLOWS: usize = 1_000;

/// The most steps one flow takes; each step checks the stop, then yields.
const MAX_STEPS: u32 = 8;

/// The highest admission ceiling a scenario draws.
const MAX_CEILING: u32 = 128;

/// One flag per flow.
type Flags = Arc<Vec<AtomicBool>>;

/// Counts a body as live from its first poll until it is dropped, however it
/// ends: returned, cancelled at a checkpoint, or dropped mid-step.
struct Live(Arc<AtomicUsize>);

impl Live {
    fn enter(count: &Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::Relaxed);
        Self(Arc::clone(count))
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// A flag per flow, all clear.
fn flags() -> Flags {
    Arc::new((0..FLOWS).map(|_| AtomicBool::new(false)).collect())
}

/// Whether flow `index`'s flag is set.
fn flag(flags: &Flags, index: usize) -> bool {
    flags
        .get(index)
        .is_some_and(|flag| flag.load(Ordering::Relaxed))
}

/// Set flow `index`'s flag.
fn set(flags: &Flags, index: usize) {
    if let Some(flag) = flags.get(index) {
        flag.store(true, Ordering::Relaxed);
    }
}

/// The one band of seeds a declared test sweeps.
fn band(index: usize) -> Band {
    sim::bands(SEEDS, PARTS).swap_remove(index)
}

/// How many of each disposition one band reached, across both sweeps.
#[derive(Default)]
struct Reached {
    succeeded: usize,
    cancelled: usize,
    refused: usize,
}

/// One seed: a thousand flows, a cancel at a drawn poll, and the accounting.
fn cancel_case(sim: &mut sim::Sim, reached: &mut Reached) -> TestResult {
    let ceiling = usize::try_from(sim.rng().between(1, MAX_CEILING))?;
    let steps: Vec<u32> = (0..FLOWS)
        .map(|_| sim.rng().below(MAX_STEPS.saturating_add(1)))
        .collect();
    // A driver poll advances every runnable flow one step, so the run takes
    // about `FLOWS / ceiling` waves of up to `MAX_STEPS + 2` polls; drawing the
    // cancel across that span (and a little past it) reaches a cancel before
    // any admission, one in the middle of a wave, and none at all.
    let waves = FLOWS.div_ceil(ceiling);
    let span = waves.saturating_mul(usize::try_from(MAX_STEPS)?.saturating_add(2));
    let cancel_at = usize::try_from(
        sim.rng()
            .below(u32::try_from(span.saturating_add(span.div_ceil(8)))?),
    )?;

    let limit = NonZeroUsize::new(ceiling).ok_or("a ceiling is drawn from one")?;
    let host = Host::builder("acme")?.max_concurrent_tasks(limit).build()?;
    let live = Arc::new(AtomicUsize::new(0));
    let stopped = Arc::new(AtomicBool::new(false));
    let late = Arc::new(AtomicUsize::new(0));
    let started = flags();
    let completed = flags();
    let work = {
        let live = Arc::clone(&live);
        let stopped = Arc::clone(&stopped);
        let late = Arc::clone(&late);
        let started = Arc::clone(&started);
        let completed = Arc::clone(&completed);
        task(
            "cancellable",
            move |scope: Scope, (index, steps): (usize, u32)| {
                let live = Arc::clone(&live);
                let stopped = Arc::clone(&stopped);
                let late = Arc::clone(&late);
                let started = Arc::clone(&started);
                let completed = Arc::clone(&completed);
                async move {
                    let _live = Live::enter(&live);
                    if stopped.load(Ordering::Relaxed) {
                        late.fetch_add(1, Ordering::Relaxed);
                    }
                    set(&started, index);
                    for _ in 0..steps {
                        scope.checkpoint()?;
                        lgwks_bot::rt::task::yield_now().await;
                    }
                    scope.checkpoint()?;
                    set(&completed, index);
                    Ok::<usize, FlowError>(index)
                }
            },
        )?
    };

    let runs = lgwks_std::task::join_all(
        steps
            .iter()
            .enumerate()
            .map(|(index, &count)| host.run(&work, (index, count))),
    );
    let mut runs = std::pin::pin!(runs);
    let mut polls: usize = 0;
    let reports = drive(std::future::poll_fn(|context| {
        if polls == cancel_at {
            stopped.store(true, Ordering::Relaxed);
            host.cancel();
        }
        polls = polls.saturating_add(1);
        runs.as_mut().poll(context)
    }));

    assert_eq!(reports.len(), FLOWS, "every flow produced a report");
    let mut counts = Reached::default();
    for (index, report) in reports.iter().enumerate() {
        match report.disposition() {
            Disposition::Succeeded => {
                assert_eq!(
                    report.output().copied(),
                    Some(index),
                    "flow {index} kept its input"
                );
                assert!(
                    flag(&completed, index),
                    "a succeeded flow {index} ran every step"
                );
                counts.succeeded = counts.succeeded.saturating_add(1);
            }
            Disposition::Cancelled => {
                assert!(
                    flag(&started, index),
                    "a cancelled flow {index} was admitted, so its body started"
                );
                counts.cancelled = counts.cancelled.saturating_add(1);
            }
            Disposition::Refused => {
                assert!(
                    !flag(&started, index),
                    "a refused flow {index} never started"
                );
                counts.refused = counts.refused.saturating_add(1);
            }
            other => {
                let refusal: TestResult = Err(format!(
                    "flow {index} ended {other:?} under a cancel: {:?}",
                    report.error()
                )
                .into());
                lgwks_std::trace::debug!(
                    error = ?refusal.as_ref().err(),
                    "cancel_case: returning an error to the caller"
                );
                return refusal;
            }
        }
    }
    let admission = host.admission();
    assert_eq!(
        admission.admitted().saturating_add(admission.refused()),
        u64::try_from(FLOWS)?,
        "every flow was admitted or refused, once"
    );
    assert_eq!(
        admission.refused(),
        u64::try_from(counts.refused)?,
        "the host's refusals are the refused reports"
    );
    let bodies = (0..FLOWS).filter(|&index| flag(&started, index)).count();
    assert_eq!(
        admission.admitted(),
        u64::try_from(bodies)?,
        "every admitted flow started its body, and no other did"
    );
    assert_eq!(live.load(Ordering::Relaxed), 0, "no body outlives the call");
    assert_eq!(
        late.load(Ordering::Relaxed),
        0,
        "no body starts after the host's stop"
    );
    assert_eq!(
        admission.available_permits(),
        ceiling,
        "every permit came back"
    );
    assert_eq!(admission.in_flight(), 0, "nothing is left in flight");

    let cancelled = polls > cancel_at;
    if cancelled {
        let after = drive(host.run(&work, (0, 0)));
        assert_eq!(
            after.disposition(),
            Disposition::Refused,
            "a stopped host refuses new work rather than dropping it"
        );
    } else {
        assert_eq!(
            counts.succeeded, FLOWS,
            "with no cancel every flow succeeds"
        );
    }

    sim.trace.record_number("ceiling", ceiling);
    sim.trace.record_number("cancel-at", cancel_at);
    sim.trace.record_number("polls", polls);
    sim.trace.record_number("succeeded", counts.succeeded);
    sim.trace.record_number("cancelled", counts.cancelled);
    sim.trace.record_number("refused", counts.refused);
    sim.record(if cancelled {
        "cancelled"
    } else {
        "ran-to-completion"
    });
    reached.succeeded = reached.succeeded.saturating_add(counts.succeeded);
    reached.cancelled = reached.cancelled.saturating_add(counts.cancelled);
    reached.refused = reached.refused.saturating_add(counts.refused);
    Ok(())
}

/// One band, swept twice for the replay receipt, with every disposition reached.
fn cancel_band(index: usize) -> TestResult {
    let mut reached = Reached::default();
    sim::assert_replays(band(index), |sim| cancel_case(sim, &mut reached))?;
    assert!(
        reached.succeeded > 0 && reached.cancelled > 0 && reached.refused > 0,
        "band {index} reached every disposition: {} succeeded, {} cancelled, {} refused",
        reached.succeeded,
        reached.cancelled,
        reached.refused
    );
    Ok(())
}

macro_rules! cancel_family {
    ($($name:ident => $index:expr);+ $(;)?) => {
        $(
            /// A seeded band of a thousand flows cancelled at a drawn point.
            #[test]
            fn $name() -> TestResult {
                cancel_band($index)
            }
        )+
    };
}

cancel_family!(
    cancel_band_00 => 0; cancel_band_01 => 1; cancel_band_02 => 2; cancel_band_03 => 3;
    cancel_band_04 => 4; cancel_band_05 => 5; cancel_band_06 => 6; cancel_band_07 => 7;
);

/// The bands partition the seed space: no gap, no overlap.
#[test]
fn the_cancel_bands_cover_every_seed_exactly_once() -> TestResult {
    let mut covered = Vec::new();
    for band in sim::bands(SEEDS, PARTS) {
        covered.extend(band.seeds());
    }
    covered.sort_unstable();
    covered.dedup();
    assert_eq!(
        u64::try_from(covered.len())?,
        SEEDS,
        "the bands cover every seed"
    );
    Ok(())
}
