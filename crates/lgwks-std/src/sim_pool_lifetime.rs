//! Seeded simulation of the pool's lifetime over real threads (#264).
//!
//! `sim_task_pool` drives the pool's accounting with numbered jobs and every
//! interleaving a seed can schedule. What no amount of scheduling can show is
//! the part a real operating system decides: whether a real thread reaches
//! its park before a shutdown begins, whether the broadcast that closes
//! admission wakes it, whether the join the shutdown reports as `Drained`
//! really returns, and whether a job is still waiting for a thread at a
//! deadline that expires while every one of them is busy.
//!
//! So one seed draws the whole journey here, over real OS threads on a pool
//! of its own: a ceiling of one to four threads, zero to seven jobs, and one
//! of four scenarios.
//!
//! - **Burst drain.** `jobs` gated jobs at once: the burst fills the ceiling
//!   and queues the rest, then the gate opens and a shutdown lets every queued
//!   and running job finish and joins every thread.
//! - **Deadline then drain.** The same burst, but the first shutdown gives
//!   itself a deadline the gated jobs cannot meet, so it reports the threads
//!   still running and the jobs still waiting; a later shutdown waits for and
//!   joins what the deadline left, and no job is cancelled by either.
//! - **Parked then shutdown.** One instant job, so its thread parks; the
//!   shutdown must then come back inside the pool's keep-alive rather than
//!   waiting the thread out — the wake, measured in wall time.
//! - **Configure after use.** A running pool refuses a later ceiling by name,
//!   at any ceiling, and still drains.
//!
//! In every scenario admission is closed afterwards: both entry points refuse
//! the job, the never-refusing one with the typed refusal as its awaiter's
//! payload, and the job never runs.
//!
//! A failure names its seed; the same seed replays the same trace. The trace
//! folds the seed and every fact the draw fixes — the accounting, each
//! shutdown report, each job's own value — and nothing that depends on how
//! fast a thread happened to run.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

// The seeded generator and replay harness are declared once, by the parent
// module: a file loaded as a module twice is two copies of every type in it,
// which clippy refuses. Both simulation families use them.
use super::{
    BLOCKING_KEEP_ALIVE, Pool, PoolConfigError, PoolShutdown, SpawnError, block_on,
    configure_decision, join_all, lock, prepare, rng, seeded_sweep, spawn_blocking_on,
    start_os_thread,
};
use rng::Rng;
use seeded_sweep::{
    SWEEP_SEEDS, assert_distinct_seeds_diverge, assert_same_seed_replays, fold, fold_usize,
    initial_trace, next_seed,
};

/// Seeds per sweep, each a different ceiling, burst and scenario.
const SEEDS: usize = 200;

/// The deadline a scenario that must expire gives itself, on jobs that are all
/// held at a shut gate: no thread can finish, so the deadline decides.
const EXPIRED: Duration = Duration::from_millis(20);

/// The deadline a scenario that must finish gives itself.
const GENEROUS: Duration = Duration::from_secs(30);

/// How long a woken thread has to come back, against a keep-alive of
/// [`BLOCKING_KEEP_ALIVE`]: a shutdown that returns inside this left the
/// thread nothing to do but answer the broadcast.
const WAKE_BOUND: Duration = Duration::from_secs(5);

/// What one seed's journey exercises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scenario {
    /// A burst at the ceiling, then a shutdown that lets it all finish.
    BurstDrain,
    /// The same burst against a deadline that expires, then one that does not.
    DeadlineThenDrain,
    /// One instant job, so the thread parks, then a shutdown that must wake it.
    ParkedThenShutdown,
    /// A running pool refusing a later ceiling, then draining.
    ConfigureAfterUse,
}

/// Every scenario, in a fixed order so the draw picks a named one.
const SCENARIOS: [Scenario; 4] = [
    Scenario::BurstDrain,
    Scenario::DeadlineThenDrain,
    Scenario::ParkedThenShutdown,
    Scenario::ConfigureAfterUse,
];

impl Scenario {
    /// A stable number per scenario, folded into the trace. The tags are the
    /// scenario's identity in the trace; the arm folds below are separate
    /// numbers for the outcomes, so a trace says which journey ran as well as
    /// what it found.
    const fn tag(self) -> u64 {
        match self {
            Self::BurstDrain => 40,
            Self::DeadlineThenDrain => 41,
            Self::ParkedThenShutdown => 42,
            Self::ConfigureAfterUse => 43,
        }
    }
}

/// The arm a shutdown reported: its deadline expired, or it drained.
const ARM_DRAINED: u64 = 50;
const ARM_EXPIRED: u64 = 51;
/// A parked thread came back inside the keep-alive.
const ARM_WOKEN: u64 = 52;
/// A running pool refused a later ceiling.
const ARM_IN_USE: u64 = 53;
/// A drained pool refused a job, typed, and the job never ran.
const ARM_CLOSED: u64 = 54;

/// A gate every job waits on, so no thread can finish a job and take the next
/// while the burst is being measured.
struct Gate {
    open: Mutex<bool>,
    opened: Condvar,
}

impl Gate {
    /// A gate nobody has opened.
    const fn new() -> Self {
        Self {
            open: Mutex::new(false),
            opened: Condvar::new(),
        }
    }

    /// Wait until the gate opens.
    fn pass(&self) {
        let mut open = lock(&self.open);
        while !*open {
            open = self
                .opened
                .wait(open)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// Open the gate, releasing every waiter.
    fn release(&self) {
        *lock(&self.open) = true;
        self.opened.notify_all();
    }
}

/// One seed's whole journey, as its trace hash.
fn journey(seed: u64) -> u64 {
    let mut rng = Rng::new(seed);
    let ceiling = rng.below(4).saturating_add(1);
    let jobs = rng.below(8);
    let scenario = SCENARIOS
        .get(rng.below(SCENARIOS.len()))
        .copied()
        .unwrap_or(Scenario::BurstDrain);
    // The threads a burst of this shape can start: the gate holds every job,
    // so each thread that starts stays started and the rest queue.
    let live = jobs.min(ceiling);
    let mut trace = initial_trace();
    fold(&mut trace, seed);
    fold_usize(&mut trace, ceiling);
    fold_usize(&mut trace, jobs);
    fold(&mut trace, scenario.tag());

    let pool = Arc::new(Pool::new(ceiling, start_os_thread));

    match scenario {
        Scenario::BurstDrain | Scenario::DeadlineThenDrain => {
            let gate = Arc::new(Gate::new());
            let mut handles = Vec::with_capacity(jobs);
            for index in 0..jobs {
                let gate = Arc::clone(&gate);
                handles.push(spawn_blocking_on(&pool, move || {
                    gate.pass();
                    index
                }));
            }
            {
                let state = lock(&pool.state);
                assert_eq!(
                    state.live, live,
                    "seed {seed:#x}: {} threads alive, not the {live} a burst of {jobs} starts at a ceiling of {ceiling}",
                    state.live
                );
                assert_eq!(
                    state.queue.len(),
                    jobs.saturating_sub(live),
                    "seed {seed:#x}: the rest of the burst waits its turn"
                );
                assert_eq!(
                    state.handles.len(),
                    live,
                    "seed {seed:#x}: every started thread is registered to be joined"
                );
            }
            fold_usize(&mut trace, live);
            fold_usize(&mut trace, jobs.saturating_sub(live));

            if scenario == Scenario::DeadlineThenDrain && jobs > 0 {
                assert_eq!(
                    pool.shutdown(EXPIRED),
                    PoolShutdown::DeadlineExceeded {
                        joined: 0,
                        running: live,
                        queued: jobs.saturating_sub(live)
                    },
                    "seed {seed:#x}: the deadline names the threads still working and the jobs still waiting"
                );
                fold(&mut trace, ARM_EXPIRED);
                fold_usize(&mut trace, live);
                fold_usize(&mut trace, jobs.saturating_sub(live));
            }

            gate.release();
            assert_eq!(
                pool.shutdown(GENEROUS),
                PoolShutdown::Drained { threads: live },
                "seed {seed:#x}: shutdown let every queued and running job finish and joined every thread"
            );
            fold(&mut trace, ARM_DRAINED);
            for (index, value) in block_on(join_all(handles)).into_iter().enumerate() {
                assert_eq!(
                    value, index,
                    "seed {seed:#x}: job {index} returned another job's value"
                );
                fold_usize(&mut trace, value);
            }
        }
        Scenario::ParkedThenShutdown => {
            assert_eq!(block_on(spawn_blocking_on(&pool, || 5u32)), 5);
            // The pause is load-bearing: it lets the thread reach its park, so
            // the broadcast is what releases it rather than a running job
            // finishing. The module's `thread::sleep` exemption is exactly
            // this line — releasing a parked thread is what is under test.
            thread::sleep(Duration::from_millis(30));
            let began = Instant::now();
            let report = pool.shutdown(WAKE_BOUND.saturating_add(BLOCKING_KEEP_ALIVE));
            let waited = began.elapsed();
            assert_eq!(
                report,
                PoolShutdown::Drained { threads: 1 },
                "seed {seed:#x}: a parked thread was woken and joined"
            );
            assert!(
                waited < WAKE_BOUND,
                "seed {seed:#x}: a parked thread was waited out ({waited:?}) rather than woken, \
                 against a {BLOCKING_KEEP_ALIVE:?} keep-alive"
            );
            fold(&mut trace, ARM_WOKEN);
        }
        Scenario::ConfigureAfterUse => {
            assert_eq!(block_on(spawn_blocking_on(&pool, || 6u32)), 6);
            assert_eq!(
                configure_decision(&pool, ceiling.saturating_add(1)),
                Err(PoolConfigError::InUse {
                    requested: ceiling.saturating_add(1),
                    running: ceiling
                }),
                "seed {seed:#x}: a running pool refuses another ceiling by name"
            );
            // Even its own ceiling: this pool was built by first use, and the
            // answer says so rather than pretending a configure took effect.
            assert_eq!(
                configure_decision(&pool, ceiling),
                Err(PoolConfigError::InUse {
                    requested: ceiling,
                    running: ceiling
                }),
                "seed {seed:#x}: the same ceiling is still refused as unconfigured"
            );
            fold(&mut trace, ARM_IN_USE);
            assert_eq!(
                pool.shutdown(GENEROUS),
                PoolShutdown::Drained { threads: 1 },
                "seed {seed:#x}: a pool that refused a configure still drains"
            );
            fold(&mut trace, ARM_DRAINED);
        }
    }

    // Admission is closed in every scenario, and the job refused never runs.
    let ran = Arc::new(AtomicBool::new(false));
    let witness = Arc::clone(&ran);
    let (refused, work) = prepare(move || witness.store(true, Ordering::SeqCst));
    assert!(
        matches!(pool.submit(work, Some(8)), Err(SpawnError::Shutdown)),
        "seed {seed:#x}: a drained pool refuses admission with the typed reason"
    );
    drop(refused);
    let handle = spawn_blocking_on(&pool, || 9u32);
    let payload = catch_unwind(AssertUnwindSafe(|| block_on(handle)))
        .err()
        .map(|payload| payload.downcast::<SpawnError>());
    assert!(
        matches!(payload, Some(Ok(ref error)) if matches!(**error, SpawnError::Shutdown)),
        "seed {seed:#x}: the never-refusing entry point fails its awaiter with the refusal"
    );
    assert!(
        !ran.load(Ordering::SeqCst),
        "seed {seed:#x}: a refused job never ran"
    );
    fold(&mut trace, ARM_CLOSED);
    trace
}

#[test]
fn sim_every_scenario_drains_joins_and_never_loses_a_job() {
    let mut state = SWEEP_SEEDS.first().copied().unwrap_or_default();
    for _ in 0..SEEDS {
        journey(next_seed(&mut state));
    }
}

#[test]
fn sim_the_same_seed_replays_the_same_pool_lifetime_trace() {
    for seed in SWEEP_SEEDS {
        assert_same_seed_replays(journey, seed);
    }
}

#[test]
fn sim_distinct_seeds_diverge_in_their_pool_lifetime_trace() {
    let [first, second, ..] = SWEEP_SEEDS;
    assert_distinct_seeds_diverge(journey, first, second);
}
