//! Simulation family: `graph` and `saga` on the real run path.
//!
//! `graph` and `node`, `saga` and `compensate`, each in flows the real
//! `script!` macro expanded, driven on the real `lgwks_bot::script` runtime.
//! Nothing here reimplements a word: what the seed controls is the input —
//! each lane's work, the failing lane and the bound; the failing step and the
//! failing undo. Time is not consulted: a lane is slow by a count of turns,
//! never a clock. `whenever` and `justify` are `sim_script_feeds`.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `graph_needs_respected` | a node starts only after every node it needs has ended; a node whose need failed never starts |
//! | `graph_bounded` | at most the written bound of nodes run at once, and each node runs once |
//! | `graph_node_scope` | each node runs in its own scope, `<flow>/<graph>/<node>`, with its own key, reproduced on a rerun |
//! | `saga_reverse_once` | when a step fails, the finished steps' undos run in reverse order, once each; a success runs none |
//! | `saga_undo_own_key` | an undo runs in `<saga>/undo:<step>`, never under the key of the step it reverses |
//! | `saga_undo_failure_returned` | an undo that fails is what the caller gets, and every other undo still ran |
//! | `saga_bounded` | at most `MAX_COMPENSATIONS` undos register; one past it fails the saga |
//!
//! Each declared test sweeps its band twice and requires identical trace
//! hashes, so a nondeterministic run fails even when every assertion passes.

#![cfg(feature = "script")]

use std::error::Error;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use lgwks_bot::rt::sync::CancellationToken;
use lgwks_bot::script::{FlowError, MAX_COMPENSATIONS, Scope, compensated};

use crate::band_family;
use crate::sim::{self, Band};
use crate::sim_script::{Turns, drive, locked, tenant_scope};

/// A test's result: an error fails it with the error's text.
pub(crate) type TestResult = Result<(), Box<dyn Error>>;

/// Keep `event` in `log`; a poisoned log keeps what it had.
pub(crate) fn note<T>(log: &Mutex<Vec<T>>, event: T) {
    if let Ok(mut held) = log.lock() {
        held.push(event);
    }
}

/// A run that ended in a way the family does not expect, logged first.
pub(crate) fn unexpected<T>(what: String) -> Result<T, Box<dyn Error>> {
    let refusal: Result<T, Box<dyn Error>> = Err(what.into());
    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "sim: an unexpected ending");
    refusal
}

/// A seeded failure of `what`, logged as every refusal is.
fn seeded_failure<T>(what: String) -> Result<T, FlowError> {
    let refusal = Err(FlowError::failed(what));
    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "sim: failing as the seed drew");
    refusal
}

/// The lanes of the `pipeline` flow, in declaration order.
const LANE_NAMES: [&str; 6] = ["fetch", "lint", "merge", "docs", "publish", "audit"];
/// Each lane's needs, as the `pipeline` flow declares them.
const LANE_NEEDS: [&[&str]; 6] = [&[], &[], &["fetch", "lint"], &["fetch"], &["merge"], &[]];

/// What the nodes of one `graph` run saw. Shared by nodes running at once, so
/// every counter is an atomic or behind a lock.
#[derive(Default)]
struct Lanes {
    /// Each lane's work, in turns.
    works: Vec<u32>,
    /// The lane that fails, when the seed drew one.
    failing: Option<usize>,
    /// Lanes in flight now.
    live: AtomicUsize,
    /// The most lanes in flight at once.
    peak: AtomicUsize,
    /// `start <lane>` and `end <lane>`, in the order they happened.
    events: Mutex<Vec<String>>,
    /// Each started lane's scope path and key.
    scopes: Mutex<Vec<(String, String)>>,
}

impl Lanes {
    /// Run lane `index` in `scope`: count it in, wait its turns, count it out.
    async fn lane(&self, scope: &Scope, index: usize) -> Result<u64, FlowError> {
        let name = LANE_NAMES
            .get(index)
            .copied()
            .ok_or_else(|| FlowError::failed("no such lane"))?;
        let turns = self
            .works
            .get(index)
            .copied()
            .ok_or_else(|| FlowError::failed("a lane has no work"))?;
        let now = self.live.fetch_add(1, Ordering::Relaxed).saturating_add(1);
        self.peak.fetch_max(now, Ordering::Relaxed);
        note(&self.events, format!("start {name}"));
        note(
            &self.scopes,
            (scope.path().to_owned(), scope.key().to_hex()),
        );
        Turns(turns).await;
        self.live.fetch_sub(1, Ordering::Relaxed);
        note(&self.events, format!("end {name}"));
        if self.failing == Some(index) {
            return seeded_failure(format!("lane {name} failed"));
        }
        Ok(u64::from(turns))
    }
}

/// The steps of the `book` saga, in order.
const TRIP_STEPS: [&str; 3] = ["reserve", "charge", "issue"];

/// What one `book` run did: the steps it ran and the undos it ran, with the
/// scope each ran in.
#[derive(Default)]
struct Trip {
    /// The step that fails, when the seed drew one.
    fail_step: Option<usize>,
    /// The undo that fails, when the seed drew one.
    fail_undo: Option<usize>,
    /// Each step run, with its path and key.
    ran: Mutex<Vec<(&'static str, String, String)>>,
    /// Each undo run, with its path and key.
    undone: Mutex<Vec<(&'static str, String, String)>>,
}

impl Trip {
    /// Run step `index`, failing when the seed drew it.
    async fn act(&self, scope: &Scope, index: usize) -> Result<u64, FlowError> {
        let name = TRIP_STEPS
            .get(index)
            .copied()
            .ok_or_else(|| FlowError::failed("no such step"))?;
        note(
            &self.ran,
            (name, scope.path().to_owned(), scope.key().to_hex()),
        );
        if self.fail_step == Some(index) {
            return seeded_failure(format!("step {name} failed"));
        }
        u64::try_from(index).map_err(FlowError::failed)
    }

    /// Undo step `index`, failing when the seed drew it.
    async fn undo(&self, scope: &Scope, index: usize) -> Result<(), FlowError> {
        let name = TRIP_STEPS
            .get(index)
            .copied()
            .ok_or_else(|| FlowError::failed("no such step"))?;
        note(
            &self.undone,
            (name, scope.path().to_owned(), scope.key().to_hex()),
        );
        if self.fail_undo == Some(index) {
            return seeded_failure(format!("undo {name} failed"));
        }
        Ok(())
    }
}

lgwks_bot::script! {
    flow pipeline(lanes: &Lanes, limit: usize) -> usize:
        let report = graph, at most (limit) at once:
            node fetch:
                lanes.lane(scope, 0).await?
            node lint:
                lanes.lane(scope, 1).await?
            node merge needs fetch, lint:
                lanes.lane(scope, 2).await?
            node docs needs fetch:
                lanes.lane(scope, 3).await?
            node publish needs merge:
                lanes.lane(scope, 4).await?
            node audit:
                lanes.lane(scope, 5).await?
        give back report.succeeded().len()

    flow book(trip: &Trip) -> u64:
        let ticket = saga booking:
            step reserve:
                trip.act(scope, 0).await?
            compensate reserve:
                trip.undo(scope, 0).await?
            step charge:
                trip.act(scope, 1).await?
            compensate charge:
                trip.undo(scope, 1).await?
            step issue:
                trip.act(scope, 2).await?
        give back ticket

}

/// A seeded lane graph: each lane's work, the failing lane, the bound.
fn seeded_lanes(sim: &mut sim::Sim) -> Result<(Lanes, usize), Box<dyn Error>> {
    let works = (0..LANE_NAMES.len())
        .map(|_| sim.rng().between(0, 4))
        .collect();
    let failing = usize::try_from(sim.rng().below(8))?;
    let limit = usize::try_from(sim.rng().between(1, 4))?;
    let lanes = Lanes {
        works,
        failing: (failing < LANE_NAMES.len()).then_some(failing),
        ..Lanes::default()
    };
    Ok((lanes, limit))
}

/// Every lane downstream of `failed`, by the declared needs.
fn downstream_of(failed: &str) -> Vec<&'static str> {
    let mut closed: Vec<&'static str> = Vec::new();
    let mut open = vec![failed];
    while let Some(next) = open.pop() {
        for (name, needs) in LANE_NAMES.iter().zip(LANE_NEEDS) {
            if needs.contains(&next) && !closed.contains(name) {
                closed.push(name);
                open.push(name);
            }
        }
    }
    closed
}

/// A node starts only after every node it needs has ended, and a node whose
/// need failed never starts.
fn graph_needs_respected(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let (lanes, limit) = seeded_lanes(sim)?;
        let scope = tenant_scope("acme", CancellationToken::new())?;
        let succeeded = drive(pipeline(&scope, &lanes, limit))?;
        let events = locked(&lanes.events, "the lane log was poisoned")?;
        let at = |event: &str| events.iter().position(|seen| seen == event);
        let blocked = match lanes.failing.and_then(|index| LANE_NAMES.get(index)) {
            Some(failed) => downstream_of(failed),
            None => Vec::new(),
        };
        for (name, needs) in LANE_NAMES.iter().zip(LANE_NEEDS) {
            let started = at(&format!("start {name}"));
            if blocked.contains(name) {
                assert_eq!(started, None, "{name} started though a need of it failed");
                continue;
            }
            let started = started.ok_or_else(|| format!("{name} never started"))?;
            for need in needs {
                let ended =
                    at(&format!("end {need}")).ok_or_else(|| format!("{need} never ended"))?;
                assert!(
                    ended < started,
                    "{name} started before its need {need} ended: {events:?}"
                );
            }
        }
        let failed = usize::from(lanes.failing.is_some());
        assert_eq!(
            succeeded,
            LANE_NAMES
                .len()
                .saturating_sub(blocked.len())
                .saturating_sub(failed),
            "the report counts every lane that ran to a value"
        );
        sim.record(&format!(
            "works={:?} failing={:?} limit={limit} events={events:?}",
            lanes.works, lanes.failing
        ));
        Ok(())
    })
}

/// At most the written bound of nodes run at once, and each node runs once.
fn graph_bounded(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let (lanes, limit) = seeded_lanes(sim)?;
        let scope = tenant_scope("acme", CancellationToken::new())?;
        drive(pipeline(&scope, &lanes, limit))?;
        let peak = lanes.peak.load(Ordering::Relaxed);
        assert!(
            peak <= limit,
            "{peak} lanes ran at once under a bound of {limit}"
        );
        let events = locked(&lanes.events, "the lane log was poisoned")?;
        for name in LANE_NAMES {
            let starts = events
                .iter()
                .filter(|seen| **seen == format!("start {name}"))
                .count();
            assert!(starts <= 1, "{name} started {starts} times");
        }
        sim.record(&format!(
            "limit={limit} peak={peak} works={:?}",
            lanes.works
        ));
        Ok(())
    })
}

/// Each node runs in its own scope, `<flow>/<graph>/<node>`, with its own
/// key, and a rerun reproduces every key.
fn graph_node_scope(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let (seeded, limit) = seeded_lanes(sim)?;
        let works = seeded.works;
        let run = || -> Result<Vec<(String, String)>, Box<dyn Error>> {
            let lanes = Lanes {
                works: works.clone(),
                ..Lanes::default()
            };
            let scope = tenant_scope("acme", CancellationToken::new())?;
            drive(pipeline(&scope, &lanes, limit))?;
            let mut scopes = locked(&lanes.scopes, "the scope log was poisoned")?;
            scopes.sort_unstable();
            Ok(scopes)
        };
        let scopes = run()?;
        assert_eq!(
            scopes,
            run()?,
            "a rerun reproduces every node's scope and key"
        );
        let mut keys: Vec<&String> = scopes.iter().map(|entry| &entry.1).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(
            keys.len(),
            LANE_NAMES.len(),
            "every node has a key of its own"
        );
        for name in LANE_NAMES {
            let path = format!("pipeline/graph/{name}");
            assert!(
                scopes.iter().any(|entry| entry.0 == path),
                "{name} ran in {path}: {scopes:?}"
            );
        }
        sim.record(&format!("limit={limit} scopes={scopes:?}"));
        Ok(())
    })
}

/// A seeded trip: the failing step and the failing undo, each or neither.
fn seeded_trip(sim: &mut sim::Sim) -> Result<Trip, Box<dyn Error>> {
    let fail_step = usize::try_from(sim.rng().below(4))?;
    let fail_undo = usize::try_from(sim.rng().below(4))?;
    Ok(Trip {
        fail_step: (fail_step < TRIP_STEPS.len()).then_some(fail_step),
        fail_undo: (fail_undo < TRIP_STEPS.len()).then_some(fail_undo),
        ..Trip::default()
    })
}

/// The undos a failure at `fail_step` runs, newest first: the compensated
/// steps that finished before it. `issue` has no `compensate` line, so only
/// `reserve` and `charge` ever undo.
fn expected_undos(fail_step: Option<usize>) -> Vec<&'static str> {
    let finished = match fail_step {
        Some(at) => at.min(2),
        None => 0,
    };
    let mut undos: Vec<&'static str> = TRIP_STEPS.iter().take(finished).copied().collect();
    undos.reverse();
    undos
}

/// When a step fails, the finished steps' undos run in reverse order, once
/// each, and a saga that succeeds runs none.
fn saga_reverse_once(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let trip = Trip {
            fail_undo: None,
            ..seeded_trip(sim)?
        };
        let scope = tenant_scope("acme", CancellationToken::new())?;
        let outcome = drive(book(&scope, &trip));
        let undone: Vec<&str> = locked(&trip.undone, "the undo log was poisoned")?
            .iter()
            .map(|entry| entry.0)
            .collect();
        assert_eq!(
            undone,
            expected_undos(trip.fail_step),
            "undos run newest first, once each"
        );
        match (trip.fail_step, outcome) {
            (None, Ok(ticket)) => assert_eq!(ticket, 2, "the saga's value is its last step's"),
            (Some(at), Err(error)) => {
                let name = TRIP_STEPS.get(at).copied().ok_or("no such step")?;
                assert!(
                    error.to_string().contains(name),
                    "the caller gets the step's failure: {error}"
                );
            }
            (failing, other) => return unexpected(format!("failing={failing:?} ended {other:?}")),
        }
        sim.record(&format!("failing={:?} undone={undone:?}", trip.fail_step));
        Ok(())
    })
}

/// An undo runs in `<saga>/undo:<step>`, never under the key of the step it
/// reverses.
fn saga_undo_own_key(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let trip = Trip {
            fail_step: Some(2),
            fail_undo: None,
            ..seeded_trip(sim)?
        };
        let scope = tenant_scope(
            &format!("tenant-{}", sim.rng().below(1_000)),
            CancellationToken::new(),
        )?;
        let outcome = drive(book(&scope, &trip));
        assert!(outcome.is_err(), "`issue` fails, so the saga does");
        let ran = locked(&trip.ran, "the step log was poisoned")?;
        let undone = locked(&trip.undone, "the undo log was poisoned")?;
        for entry in &undone {
            let name = entry.0;
            assert_eq!(
                entry.1,
                format!("book/booking/undo:{name}"),
                "the undo's scope is its own"
            );
            let step = ran
                .iter()
                .find(|ran_step| ran_step.0 == name)
                .ok_or("an undo with no step")?;
            assert_eq!(
                step.1,
                format!("book/booking/{name}"),
                "the step ran in its own scope"
            );
            assert_ne!(step.2, entry.2, "an undo never shares its step's key");
        }
        sim.record(&format!("undone={undone:?}"));
        Ok(())
    })
}

/// An undo that fails is what the caller gets, and every other undo still ran.
fn saga_undo_failure_returned(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let trip = Trip {
            fail_step: Some(2),
            ..seeded_trip(sim)?
        };
        let scope = tenant_scope("acme", CancellationToken::new())?;
        let error = match drive(book(&scope, &trip)) {
            Err(error) => error,
            Ok(ticket) => {
                return unexpected(format!("`issue` fails, yet the saga gave back {ticket}"));
            }
        };
        let undone: Vec<&str> = locked(&trip.undone, "the undo log was poisoned")?
            .iter()
            .map(|entry| entry.0)
            .collect();
        assert_eq!(
            undone,
            vec!["charge", "reserve"],
            "every undo ran, even after one failed"
        );
        let expected = match trip.fail_undo.and_then(|at| TRIP_STEPS.get(at)) {
            Some(&name) if name != "issue" => format!("undo {name} failed"),
            _ => String::from("step issue failed"),
        };
        assert!(
            error.to_string().contains(&expected),
            "the caller gets `{expected}`: {error}"
        );
        sim.record(&format!("fail_undo={:?} error={error}", trip.fail_undo));
        Ok(())
    })
}

/// At most `MAX_COMPENSATIONS` undos register; one past it fails the saga.
fn saga_bounded(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let extra = usize::try_from(sim.rng().below(3))?;
        let count = MAX_COMPENSATIONS.saturating_sub(1).saturating_add(extra);
        let scope = tenant_scope("acme", CancellationToken::new())?;
        let outcome = drive(compensated(
            &scope,
            "flood",
            |_scope, compensations| async move {
                for _ in 0..count {
                    compensations.add("step", |_undo: Scope| async { Ok(()) })?;
                }
                Ok::<usize, FlowError>(compensations.len())
            },
        ));
        match outcome {
            Ok(registered) => {
                assert!(
                    count <= MAX_COMPENSATIONS,
                    "{count} registrations passed the bound"
                );
                assert_eq!(
                    registered, count,
                    "every registration within the bound held"
                );
            }
            Err(FlowError::InvalidBound { .. }) => {
                assert!(
                    count > MAX_COMPENSATIONS,
                    "a stack within its bound was refused"
                );
            }
            Err(other) => return unexpected(format!("the flood ended with {other}")),
        }
        sim.record(&format!("count={count}"));
        Ok(())
    })
}

band_family::band_family! {
    graph_needs_respected_band_00 => graph_needs_respected, 0;
    graph_needs_respected_band_01 => graph_needs_respected, 1;
    graph_needs_respected_band_02 => graph_needs_respected, 2;
    graph_needs_respected_band_03 => graph_needs_respected, 3;
    graph_bounded_band_00 => graph_bounded, 0;
    graph_bounded_band_01 => graph_bounded, 1;
    graph_bounded_band_02 => graph_bounded, 2;
    graph_bounded_band_03 => graph_bounded, 3;
    graph_node_scope_band_00 => graph_node_scope, 0;
    graph_node_scope_band_01 => graph_node_scope, 1;
    graph_node_scope_band_02 => graph_node_scope, 2;
    graph_node_scope_band_03 => graph_node_scope, 3;
    saga_reverse_once_band_00 => saga_reverse_once, 0;
    saga_reverse_once_band_01 => saga_reverse_once, 1;
    saga_reverse_once_band_02 => saga_reverse_once, 2;
    saga_reverse_once_band_03 => saga_reverse_once, 3;
    saga_undo_own_key_band_00 => saga_undo_own_key, 0;
    saga_undo_own_key_band_01 => saga_undo_own_key, 1;
    saga_undo_own_key_band_02 => saga_undo_own_key, 2;
    saga_undo_own_key_band_03 => saga_undo_own_key, 3;
    saga_undo_failure_returned_band_00 => saga_undo_failure_returned, 0;
    saga_undo_failure_returned_band_01 => saga_undo_failure_returned, 1;
    saga_undo_failure_returned_band_02 => saga_undo_failure_returned, 2;
    saga_undo_failure_returned_band_03 => saga_undo_failure_returned, 3;
    saga_bounded_band_00 => saga_bounded, 0;
    saga_bounded_band_01 => saga_bounded, 1;

}
