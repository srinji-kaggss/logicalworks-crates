//! Simulation family: [`keyed::open_once`] over a seeded receiver.
//!
//! The receiver here is no service: an in-process store whose every fault is
//! drawn from the seed, so the family pins the layer itself rather than any
//! adapter. Each seed draws, per key, whether an earlier run was killed with
//! its create still in flight (and when it lands, inside the receiver's
//! bound); and for the receiver, whether it loses create answers, rejects
//! creates, fails reads at the start, or fails reads right after a create.
//! Two tenants then rerun their own keys in a seeded order on one clock the
//! scenario advances.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `one_key_holds_at_most_one_effect_under_seeded_faults` | in-flight creates from killed runs, lost answers, rejected creates and failing reads never leave two effects for one key, and every answered open names the key's first effect |
//! | `a_receiver_that_declares_no_in_flight_bound_is_duplicated_by_a_late_landing` | the fence is what holds the property: without the bound the same late landing doubles the key |
//! | `a_failing_read_back_read_does_not_end_the_read_back` | a read-back read that fails is read past, not reported |
//!
//! The band is swept twice through `sim::assert_replays`, so one seed is one
//! trace hash.

#![cfg(feature = "script")]

use std::error::Error;
use std::fmt;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use lgwks_bot::keyed::{self, CreateFailure, KeyedError, KeyedReceiver, Settlement};
use lgwks_bot::rt::clock::Clock;

use crate::sim::{self, Band, Sim};

/// A family's result: an error fails it with the seed's text.
type SimResult = Result<(), Box<dyn Error>>;

/// How many seeds the main family sweeps.
const SEEDS: u64 = 256;

/// The receiver's declared bound on a request in flight.
const BOUND: Duration = Duration::from_secs(5);

/// How far one driver step moves a scenario's clock.
///
/// A second always advances a wait, and the longest single wait the layer
/// makes, its in-flight bound, is spent in whole steps. Nothing here reads
/// the wall clock.
const DRIVE_STEP: Duration = Duration::from_secs(1);

/// Run `work` to its end, advancing `clock` whenever a wait is parked on it.
///
/// The driver advances only while a wait is actually parked
/// (`Clock::has_waiters`), so work busy elsewhere, on a child process say,
/// burns no logical time, and it stops on the work's own return. A wall clock
/// refuses the advance, and the driver then leaves the waits to be real.
pub(crate) fn drive<T>(clock: &Clock, work: impl Future<Output = T>) -> T {
    let finished = lgwks_bot::rt::sync::CancellationToken::new();
    let work_finished = finished.clone();
    let driver_clock = clock.clone();
    lgwks_bot::block_on(async {
        let run = async {
            let answer = work.await;
            // A driver parked on `wait_for_waiter` is woken by the stop, so
            // work that ends between two waits cannot leave the join open.
            work_finished.cancel();
            answer
        };
        let advance = async move {
            while !finished.is_cancelled() {
                if !driver_clock.has_waiters() {
                    // Nothing waits on the clock: park rather than spin, and
                    // wake when a wait registers.
                    lgwks_bot::select! {
                        () = driver_clock.wait_for_waiter() => {}
                        () = finished.cancelled() => {}
                    }
                } else if driver_clock.advance(DRIVE_STEP).is_ok() {
                    // Let the woken wait be polled before the next step.
                    lgwks_bot::rt::task::yield_now().await;
                } else {
                    break;
                }
            }
        };
        let (answer, ()) = lgwks_bot::join!(run, advance);
        answer
    })
}

/// Why the model receiver failed a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelError {
    /// The receiver rejected the create as a request; nothing landed.
    Rejected,
    /// The create landed and its answer was lost.
    Lost,
    /// A read failed.
    ReadFailed,
    /// The store's lock was poisoned by a panicking holder.
    Poisoned,
}

impl fmt::Display for ModelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match *self {
            Self::Rejected => "rejected",
            Self::Lost => "the answer was lost",
            Self::ReadFailed => "a read failed",
            Self::Poisoned => "the store's lock was poisoned",
        })
    }
}

impl Error for ModelError {}

/// What the seed decided the receiver does.
#[derive(Debug, Clone, Copy, Default)]
struct Faults {
    /// Creates land and lose their answer.
    lose_answers: bool,
    /// Creates are rejected.
    reject: bool,
    /// How many reads fail after each create.
    fail_after_create: u32,
}

/// The model's state: landed effects and creates still in flight.
#[derive(Debug, Default)]
struct Store {
    /// `(id, key)` of each landed effect, in landing order.
    landed: Vec<(u64, String)>,
    /// `(key, lands at)` of creates sent by killed runs and not landed yet.
    in_flight: Vec<(String, Duration)>,
    /// The last effect id given.
    last: u64,
    /// Reads still to fail.
    failing_reads: u32,
}

impl Store {
    /// Land every in-flight create whose time has come.
    fn settle(&mut self, now: Duration) {
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.in_flight)
            .into_iter()
            .partition(|&(_, at)| at <= now);
        self.in_flight = later;
        for (key, _) in due {
            self.land(key);
        }
    }

    /// Hold one effect for `key`.
    fn land(&mut self, key: String) -> u64 {
        self.last = self.last.saturating_add(1);
        self.landed.push((self.last, key));
        self.last
    }

    /// The first effect `key` holds.
    fn first(&self, key: &str) -> Option<u64> {
        self.landed
            .iter()
            .find(|landed| landed.1 == key)
            .map(|landed| landed.0)
    }

    /// How many effects `key` holds.
    fn held(&self, key: &str) -> usize {
        self.landed.iter().filter(|landed| landed.1 == key).count()
    }
}

/// The model receiver.
struct Model {
    /// The receiver's state.
    store: Mutex<Store>,
    /// The scenario's clock.
    clock: Clock,
    /// What it does.
    faults: Faults,
    /// The in-flight bound it declares.
    bound: Duration,
}

impl Model {
    /// A receiver on `clock` doing `faults`, declaring `bound`.
    fn new(clock: &Clock, faults: Faults, bound: Duration) -> Self {
        Self {
            store: Mutex::default(),
            clock: clock.clone(),
            faults,
            bound,
        }
    }

    /// The store, or the poisoning a panicking holder left.
    fn store(&self) -> Result<MutexGuard<'_, Store>, ModelError> {
        self.store.lock().map_err(|poisoned| {
            lgwks_std::trace::debug!(error = %poisoned, "sim_keyed: the store's lock was poisoned");
            ModelError::Poisoned
        })
    }

    /// One read, answered now.
    fn read(&self, key: &str) -> Result<Option<u64>, ModelError> {
        let mut store = self.store()?;
        store.settle(self.clock.now());
        if store.failing_reads > 0 {
            store.failing_reads = store.failing_reads.saturating_sub(1);
            lgwks_std::trace::debug!(key, "sim_keyed: the model fails a read");
            return Err(ModelError::ReadFailed);
        }
        Ok(store.first(key))
    }

    /// One create, answered now.
    fn write(&self, key: &str) -> Result<u64, ModelError> {
        if self.faults.reject {
            lgwks_std::trace::debug!(key, "sim_keyed: the model rejects a create");
            return Err(ModelError::Rejected);
        }
        let mut store = self.store()?;
        store.settle(self.clock.now());
        let id = store.land(key.to_owned());
        store.failing_reads = self.faults.fail_after_create;
        if self.faults.lose_answers {
            lgwks_std::trace::debug!(key, id, "sim_keyed: the model loses a create's answer");
            return Err(ModelError::Lost);
        }
        Ok(id)
    }
}

impl KeyedReceiver for Model {
    type Draft = String;
    type Held = u64;
    type Error = ModelError;

    fn find(&self, key: &String) -> impl Future<Output = Result<Option<u64>, ModelError>> + Send {
        let answer = self.read(key);
        async move { answer }
    }

    fn create(&self, key: &String) -> impl Future<Output = Result<u64, ModelError>> + Send {
        let answer = self.write(key);
        async move { answer }
    }

    fn create_failure(&self, failure: &ModelError) -> CreateFailure {
        match *failure {
            ModelError::Rejected => CreateFailure::NothingLanded,
            ModelError::Lost | ModelError::ReadFailed | ModelError::Poisoned => {
                CreateFailure::MayHaveLanded
            }
        }
    }

    fn in_flight_bound(&self) -> Duration {
        self.bound
    }
}

/// Run one open of `key` to its end on the model's clock.
fn open(model: &Model, key: &str) -> Result<(u64, Settlement), KeyedError<ModelError>> {
    let key = key.to_owned();
    drive(&model.clock, keyed::open_once(model, &key, &model.clock))
}

/// The model's word for one open's outcome.
fn outcome(
    answer: &Result<(u64, Settlement), KeyedError<ModelError>>,
) -> Result<&'static str, String> {
    Ok(match *answer {
        Ok((_, settlement)) => settlement.as_str(),
        Err(KeyedError::Refused(_)) => "refused",
        Err(KeyedError::Unread(_)) => "unread",
        Err(KeyedError::Indeterminate { .. }) => "indeterminate",
        Err(ref other) => {
            lgwks_std::trace::debug!(failure = ?other, "sim_keyed: an unmodelled failure");
            return Err(format!("an unmodelled failure {other:?}"));
        }
    })
}

/// One seed: two keys, each with its own drawn earlier kill, rerun in a
/// seeded order on one receiver.
fn seed(sim: &mut Sim) -> SimResult {
    let faults = Faults {
        lose_answers: sim.rng().below(3) == 0,
        reject: sim.rng().below(6) == 0,
        fail_after_create: sim.rng().below(3),
    };
    let clock = Clock::virtual_at(Duration::ZERO);
    let model = Model::new(&clock, faults, BOUND);
    model.store()?.failing_reads = sim.rng().below(2);
    sim.record(&format!("{faults:?}"));
    let keys = ["acme-key", "zenith-key"];
    // A killed earlier run's create, still in flight when the reruns begin,
    // landing at a drawn time inside the receiver's bound.
    for key in keys {
        if sim.rng().below(2) == 0 {
            let lands = Duration::from_millis(u64::from(sim.rng().below(5_000)));
            model.store()?.in_flight.push((key.to_owned(), lands));
            sim.record(&format!("{key}:in-flight:{}", lands.as_millis()));
        }
    }
    let mut left = [
        sim.rng().below(3).saturating_add(1),
        sim.rng().below(3).saturating_add(1),
    ];
    while left.iter().any(|&remaining| remaining > 0) {
        let mut turn = usize::try_from(sim.rng().below(2))?;
        if left.get(turn).copied() == Some(0) {
            turn ^= 1;
        }
        let key = *keys.get(turn).ok_or("two keys")?;
        let answer = open(&model, key);
        let said = outcome(&answer)?;
        let store = model.store()?;
        if let Ok((id, _)) = answer {
            assert_eq!(
                store.first(key),
                Some(id),
                "seed {}: {key} answered an effect that is not its first",
                sim.seed
            );
        }
        // The property, after every open: one key, at most one effect.
        for each in keys {
            let held = store.held(each);
            assert!(
                held <= 1,
                "seed {}: {each} holds {held} effects after {key} {said}",
                sim.seed
            );
        }
        drop(store);
        sim.record(&format!("{key}:{said}@{}", clock.now().as_millis()));
        if let Some(remaining) = left.get_mut(turn) {
            *remaining = remaining.saturating_sub(1);
        }
    }
    // Every create still in flight lands once the bound has passed; the
    // property holds of what the receiver is left with, not only of what the
    // opens saw.
    let mut store = model.store()?;
    store.settle(clock.now().saturating_add(BOUND));
    for key in keys {
        let held = store.held(key);
        assert!(
            held <= 1,
            "seed {}: {key} holds {held} effects in the end",
            sim.seed
        );
    }
    Ok(())
}

#[test]
fn one_key_holds_at_most_one_effect_under_seeded_faults() -> SimResult {
    sim::assert_replays(Band::new(0, SEEDS), seed)
}

/// The fence is load-bearing: the same receiver declaring no in-flight bound
/// creates beside a killed run's create that lands a second later.
#[test]
fn a_receiver_that_declares_no_in_flight_bound_is_duplicated_by_a_late_landing() -> SimResult {
    for (bound, held) in [(Duration::ZERO, 2), (BOUND, 1)] {
        let clock = Clock::virtual_at(Duration::ZERO);
        let model = Model::new(&clock, Faults::default(), bound);
        model
            .store()?
            .in_flight
            .push((String::from("key"), Duration::from_secs(1)));
        let answer = open(&model, "key").map_err(|failure| format!("{failure:?}"))?;
        let mut store = model.store()?;
        store.settle(clock.now().saturating_add(BOUND));
        assert_eq!(
            store.held("key"),
            held,
            "bound {bound:?} answered {answer:?}"
        );
    }
    Ok(())
}

/// A read-back read that fails is read past: the lost create's effect is
/// found by the next read and the open is reconciled, not indeterminate.
#[test]
fn a_failing_read_back_read_does_not_end_the_read_back() -> SimResult {
    let clock = Clock::virtual_at(Duration::ZERO);
    let faults = Faults {
        lose_answers: true,
        reject: false,
        fail_after_create: 2,
    };
    let model = Model::new(&clock, faults, BOUND);
    let (id, settlement) = open(&model, "key").map_err(|failure| format!("{failure:?}"))?;
    assert_eq!(settlement, Settlement::Reconciled);
    assert_eq!(model.store()?.first("key"), Some(id));
    assert_eq!(model.store()?.held("key"), 1);
    Ok(())
}
