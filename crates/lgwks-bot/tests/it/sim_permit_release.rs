//! Simulation family: a run dropped while suspended returns its permit
//! (acceptance row T03).
//!
//! Row T03 of `docs/orchestration-acceptance.spec.md`: "Drop local composition
//! while suspended; no borrowed child escapes. An independently timed child
//! process detects a non-yielding callback without claiming in-process
//! preemption." This family is the in-process half, under a seeded load: the
//! host's permit is the observable, because a permit that does not come back is
//! a ceiling that shrinks every time a caller walks away.
//!
//! A counting semaphore maintains a set of permits and a waiter is only queued
//! behind them; walking away from a queued waiter must take nothing from a
//! holder, and walking away from a holder must give exactly one permit back.
//!
//! | Property | What a seed checks |
//! |---|---|
//! | a suspended run holds a permit | after one poll, `in_flight` is the number of started runs up to the ceiling and `available_permits` is the rest |
//! | a queued run holds none | runs past the ceiling leave both counters as they were, and dropping them changes neither |
//! | a drop gives one back | dropping holders in a seeded order moves both counters by exactly one per drop |
//! | the ceiling is whole again | a second wave of `ceiling` runs is admitted at once and the peak never passes the ceiling |
//!
//! # What is real and what is seeded
//!
//! The host, its admission counters and the runs are the shipped types. The seed
//! draws the ceiling, how many runs queue behind it and the order the holders
//! are dropped in. No timer is read: a parked body never returns, a run is
//! polled once and dropped, so the outcome is decided by structure alone.

#![cfg(feature = "script")]

use crate::band_family;
use crate::load;
use crate::sim;

use std::error::Error;
use std::future::Future;
use std::num::NonZeroUsize;
use std::pin::Pin;

use lgwks_bot::script::{FlowError, Scope};
use lgwks_bot::task::{Disposition, Host, Report, Task, task};

use sim::{Band, Rng};

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

/// The largest admission ceiling a seed draws.
const MOST_CEILING: u32 = 4;

/// The most runs a seed queues behind a full ceiling.
const MOST_WAITING: u32 = 3;

/// What a parked body would return, were it ever to finish.
type ParkFuture = lgwks_bot::BoxFuture<'static, Result<u32, FlowError>>;

/// A task whose body never returns.
type ParkingTask = Task<fn(Scope, u32) -> ParkFuture>;

/// A run suspended mid-flight, borrowing its host and its task.
///
/// Boxed because a seeded number of them are alive at once, and a box
/// (`Box::pin`) rather than a stack pin because they are stored in a vector.
type Parked<'run> = Pin<Box<dyn Future<Output = Report<u32>> + 'run>>;

/// The body of [`parking_task`]: suspends forever.
fn park_forever(_scope: Scope, _value: u32) -> ParkFuture {
    Box::pin(std::future::pending::<Result<u32, FlowError>>())
}

/// A task that parks inside its body, so its run holds its permit until dropped.
///
/// A named `fn` rather than a closure so every run in a vector has one type.
fn parking_task() -> Result<ParkingTask, Box<dyn Error>> {
    let body: fn(Scope, u32) -> ParkFuture = park_forever;
    Ok(task("parking", body)?)
}

/// Start `count` runs of `parking` on `host` and poll each once.
///
/// Every run is suspended when this returns: inside its body holding a permit,
/// or queued behind a full ceiling holding none. A run that finished on its
/// first poll would make the row pass for a run that released its permit on its
/// own, so that is refused here.
fn start<'run>(
    host: &'run Host,
    parking: &'run ParkingTask,
    count: usize,
) -> Result<Vec<Option<Parked<'run>>>, Box<dyn Error>> {
    let mut started: Vec<Option<Parked<'run>>> = Vec::with_capacity(count);
    for index in 0..count {
        let mut run = Box::pin(host.run(parking, u32::try_from(index)?));
        assert!(
            load::pending_once(run.as_mut()),
            "run {index} is suspended when it is walked away from, never finished"
        );
        let erased: Parked<'run> = run;
        started.push(Some(erased));
    }
    Ok(started)
}

/// A permutation of `0..count`, Fisher-Yates over the seed's own stream.
fn shuffled(rng: &mut Rng, count: usize) -> Result<Vec<usize>, Box<dyn Error>> {
    let mut order: Vec<usize> = (0..count).collect();
    for position in (1..count).rev() {
        let width = u32::try_from(position.saturating_add(1))?;
        let pick = usize::try_from(rng.below(width))?;
        order.swap(position, pick);
    }
    Ok(order)
}

/// Dropping suspended runs returns exactly their permits, whatever the ceiling,
/// the queue behind it and the order they are dropped in.
fn dropped_runs_return_their_permits(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let seed = sim.seed;
        let ceiling = usize::try_from(sim.rng().between(1, MOST_CEILING))?;
        let waiting = usize::try_from(sim.rng().between(0, MOST_WAITING))?;
        let host = Host::builder("t03-sim")?
            .max_concurrent_tasks(NonZeroUsize::new(ceiling).ok_or("a ceiling of at least one")?)
            .build()?;
        let parking = parking_task()?;
        let quick = task("quick", |_: Scope, value: u32| async move { Ok(value) })?;

        let mut holders = start(&host, &parking, ceiling)?;
        assert_eq!(
            host.admission().in_flight(),
            ceiling,
            "seed {seed}: {ceiling} suspended runs hold {ceiling} permits"
        );
        assert_eq!(
            host.admission().available_permits(),
            0,
            "seed {seed}: a full ceiling has no permit left"
        );

        let queued = start(&host, &parking, waiting)?;
        assert_eq!(
            host.admission().in_flight(),
            ceiling,
            "seed {seed}: {waiting} queued runs hold no permit"
        );
        drop(queued);
        assert_eq!(
            host.admission().in_flight(),
            ceiling,
            "seed {seed}: walking away from a queued run takes no permit from a holder"
        );
        assert_eq!(
            host.admission().available_permits(),
            0,
            "seed {seed}: and gives none back, since it never held one"
        );

        let order = shuffled(sim.rng(), ceiling)?;
        for (released, index) in order.iter().copied().enumerate() {
            drop(holders.get_mut(index).and_then(Option::take));
            let holding = ceiling.saturating_sub(released.saturating_add(1));
            assert_eq!(
                host.admission().in_flight(),
                holding,
                "seed {seed}: drop {} of {ceiling} (holder {index}) returns one permit",
                released.saturating_add(1)
            );
            assert_eq!(
                host.admission().available_permits(),
                ceiling.saturating_sub(holding),
                "seed {seed}: and the free count rises by the one that came back"
            );
        }
        load::assert_budget_whole(&host, ceiling, "after every holder was dropped");

        let second = start(&host, &parking, ceiling)?;
        assert_eq!(
            host.admission().in_flight(),
            ceiling,
            "seed {seed}: a second wave of {ceiling} runs is admitted at once"
        );
        drop(second);
        load::assert_budget_whole(&host, ceiling, "after the second wave was dropped");
        assert_eq!(
            host.admission().peak_in_flight(),
            ceiling,
            "seed {seed}: the high-water mark reached the ceiling and never passed it"
        );

        let report: Report<u32> = lgwks_bot::block_on(host.run(&quick, 7));
        assert_eq!(
            report.disposition(),
            Disposition::Succeeded,
            "seed {seed}: a run that finishes is admitted after all that: {report:?}"
        );
        assert_eq!(
            report.output().copied(),
            Some(7),
            "seed {seed}: and it returns the value its body computed"
        );

        sim.record(&format!(
            "ceiling={ceiling} waiting={waiting} order={order:?} peak={}",
            host.admission().peak_in_flight()
        ));
        Ok(())
    })
}

band_family::band_family! {
    dropped_runs_return_their_permits_band_00_t03 => dropped_runs_return_their_permits, 0;
    dropped_runs_return_their_permits_band_01_t03 => dropped_runs_return_their_permits, 1;
    dropped_runs_return_their_permits_band_02_t03 => dropped_runs_return_their_permits, 2;
    dropped_runs_return_their_permits_band_03_t03 => dropped_runs_return_their_permits, 3;
    dropped_runs_return_their_permits_band_04_t03 => dropped_runs_return_their_permits, 4;
    dropped_runs_return_their_permits_band_05_t03 => dropped_runs_return_their_permits, 5;
    dropped_runs_return_their_permits_band_06_t03 => dropped_runs_return_their_permits, 6;
    dropped_runs_return_their_permits_band_07_t03 => dropped_runs_return_their_permits, 7;
    dropped_runs_return_their_permits_band_08_t03 => dropped_runs_return_their_permits, 8;
    dropped_runs_return_their_permits_band_09_t03 => dropped_runs_return_their_permits, 9;
    dropped_runs_return_their_permits_band_10_t03 => dropped_runs_return_their_permits, 10;
    dropped_runs_return_their_permits_band_11_t03 => dropped_runs_return_their_permits, 11;
    dropped_runs_return_their_permits_band_12_t03 => dropped_runs_return_their_permits, 12;
    dropped_runs_return_their_permits_band_13_t03 => dropped_runs_return_their_permits, 13;
    dropped_runs_return_their_permits_band_14_t03 => dropped_runs_return_their_permits, 14;
    dropped_runs_return_their_permits_band_15_t03 => dropped_runs_return_their_permits, 15;
}
