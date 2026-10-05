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
//!
//! - **Configure after use.** A running pool refuses a later ceiling by name,
//!   at any ceiling, and still drains.
//!
//! On top of those, the handle identity: a start cannot reap a thread that has
//! left the accounting but not yet returned, and the fixture holds that window
//! open on purpose, so the identity `handles == live + held` is checked
//! exactly rather than bounded.
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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

// The seeded generator and replay harness are declared once, by the parent
// module: a file loaded as a module twice is two copies of every type in it,
// which clippy refuses. Both simulation families use them. The gate is the
// other family's fixture too, so it is loaded from the one shared copy.
#[path = "../tests/support/gate.rs"]
mod gate;

use std::io;
use std::sync::OnceLock;

use super::{
    BLOCKING_KEEP_ALIVE, Handoff, Pool, PoolConfigError, PoolShutdown, SpawnError, block_on,
    configure_decision, join_all, lock, prepare, rng, seeded_sweep, serve_pool, spawn_blocking_on,
    start_os_thread,
};
use gate::{Gate, Release};
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

/// Cycles per seed: every cycle leaves the pool and comes back, which is what
/// a bursty process does for as long as it runs.
const CYCLES: usize = 6;

/// The keep-alive the burst/idle cycles run under: long enough for a burst's
/// work to finish, short enough that a cycle costs milliseconds.
const CYCLE_KEEP_ALIVE: Duration = Duration::from_millis(1);

/// How long one wait in this family may take before the simulation calls it a
/// thread that would not leave. Bounded, so a pool that never exits fails with
/// a message instead of hanging the sweep — and measured in elapsed time, not
/// in a count of millisecond polls, because the waits are for real threads and
/// a count of 500 was half a second of wall clock: on a host running the whole
/// gate at once, a thread that *did* leave took longer than that, and the
/// family reported a defect that was the host's load. The verdict must not
/// depend on how busy the machine is; the bound only separates a hang from a
/// slow scheduler, and the fast path still returns on the first poll that sees
/// the condition.
const HANG_BOUND: Duration = Duration::from_secs(30);

/// Poll `done` every millisecond until it holds or [`HANG_BOUND`] has elapsed,
/// answering whether it held.
fn poll_until(mut done: impl FnMut() -> bool) -> bool {
    let started = Instant::now();
    loop {
        if done() {
            return true;
        }
        if started.elapsed() >= HANG_BOUND {
            return done();
        }
        thread::sleep(Duration::from_millis(1));
    }
}

/// Wait until every thread of `pool` has left, or fail naming the seed.
///
/// The wait is a bounded poll rather than a sleep of a guessed length: what it
/// proves is that a thread left, and how long that took is the host's business.
fn wait_until_empty(pool: &Arc<Pool>, seed: u64) {
    if poll_until(|| lock(&pool.state).live == 0) {
        return;
    }
    let state = lock(&pool.state);
    assert!(
        state.live == 0,
        "seed {seed:#x}: {} threads were still alive after {HANG_BOUND:?} at a \
         {CYCLE_KEEP_ALIVE:?} keep-alive",
        state.live,
    );
}

/// The fact about the handle list that holds of any pool, at any step.
///
/// `handles.len() == live + (threads that have left the accounting and not yet
/// returned)` is an identity, and there is no constant on the second term: each
/// departure frees a slot immediately while the thread keeps running its last
/// instructions, so a scheduler that deschedules every departing thread in turn
/// admits the next one. What is checkable at any instant is that every live
/// thread is present and unreturned — a thread that has returned has already
/// decremented `live`.
///
/// Whether a *reap* took every returned thread is not checkable here: a kept
/// thread may return between the reap and this read, so `is_finished` read
/// afterwards describes a later instant than the reap did. [`cycles`] checks
/// the reap instead from a state it fixed first — every older thread returned
/// before the start that reaps — which is the only reading that cannot race.
fn assert_handles_accounted(pool: &Arc<Pool>, where_: &str, seed: u64) {
    let state = lock(&pool.state);
    let live = state.live;
    let held = state.handles.len();
    let unreturned = state
        .handles
        .iter()
        .filter(|handle| !handle.is_finished())
        .count();
    assert!(
        unreturned >= live,
        "seed {seed:#x} {where_}: {live} live threads but only {unreturned} unreturned handles \
         of {held}; a live thread cannot have returned, since it decrements `live` first"
    );
}

/// One seed's burst/idle cycles, as its trace hash.
///
/// Every cycle submits a burst, lets the threads finish, waits out the
/// keep-alive, and checks the handle bound after *every* step — after each
/// submit, not only once the pool has gone quiet. The next cycle's first submit
/// then has to find exactly its own handle: the exited threads were joined on
/// the way, so a handle per cycle cannot accumulate.
fn cycles(seed: u64) -> u64 {
    let mut rng = Rng::new(seed);
    let ceiling = rng.below(4).saturating_add(1);
    let cycles = rng.below(CYCLES).saturating_add(2);
    let mut trace = initial_trace();
    fold(&mut trace, seed);
    fold_usize(&mut trace, ceiling);
    fold_usize(&mut trace, cycles);

    let pool = Arc::new(Pool::tuned(ceiling, start_os_thread, CYCLE_KEEP_ALIVE));
    for cycle in 0..cycles {
        // A burst: every thread the ceiling allows, each returning its own
        // index, so a thread that never ran its job is a failure, not a
        // detail.
        let burst = ceiling.saturating_add(1);
        let mut handles = Vec::with_capacity(burst);
        for index in 0..burst {
            handles.push(spawn_blocking_on(&pool, move || index));
            assert_handles_accounted(&pool, &format!("cycle {cycle} submit {index}"), seed);
        }
        for (index, value) in block_on(join_all(handles)).into_iter().enumerate() {
            assert_eq!(
                value, index,
                "seed {seed:#x}: cycle {cycle} job {index} ran elsewhere"
            );
            fold_usize(&mut trace, value);
        }
        assert_handles_accounted(&pool, &format!("cycle {cycle} drained"), seed);
        wait_until_empty(&pool, seed);
        // With every thread gone and no start to reap them, what the list holds
        // is exactly the threads this cycle started — never more, however many
        // cycles came before.
        let held = lock(&pool.state).handles.len();
        assert!(
            held <= ceiling,
            "seed {seed:#x}: cycle {cycle} left {held} join handles for a ceiling of {ceiling}; \
             a thread that exits on its keep-alive must be reaped, not held"
        );
        fold(&mut trace, u64::from(held <= ceiling));
        assert_handles_accounted(&pool, &format!("cycle {cycle} idle"), seed);

        // The next cycle's first submit reaps what has returned. `live == 0`
        // says every thread left the accounting, not that each has returned —
        // one may still be in its last instructions, and the reap rightly keeps
        // it — so the reap is checked only once every older thread has returned.
        // Then the list after the start is exactly its own handle, whatever the
        // new thread does afterwards: a handle leaves only at the next reap.
        wait_until_all_returned(&pool, seed);
        let head = spawn_blocking_on(&pool, || 0u32);
        assert_eq!(block_on(head), 0);
        let after_reap = lock(&pool.state).handles.len();
        assert_eq!(
            after_reap, 1,
            "seed {seed:#x}: cycle {cycle} started a thread holding {after_reap} handles; \
             the ones that had already left were not joined"
        );
        fold(&mut trace, u64::try_from(after_reap).unwrap_or(u64::MAX));
        assert_handles_accounted(&pool, &format!("cycle {cycle} after the reap"), seed);
    }
    // The pool has nothing left to run: a shutdown still joins whatever the
    // last cycle's reap did not already take, and reports the threads it
    // joined, so the cycles end with no thread and no handle left behind.
    let report = pool.shutdown(GENEROUS);
    let state = lock(&pool.state);
    assert!(
        matches!(report, PoolShutdown::Drained { .. }) && state.handles.is_empty(),
        "seed {seed:#x}: the cycles ended holding handles the pool never joined: {report:?}"
    );
    trace
}

/// The hold a [`start_lingering`] thread waits on after leaving the pool.
///
/// One hold for the whole simulation family, re-armed per release so a replayed
/// seed is held by its own release rather than finding the previous run's.
static LINGER: OnceLock<Arc<Release>> = OnceLock::new();

/// A starter whose thread stays running after it has left the pool's
/// accounting.
///
/// This is the mid-exit window made deterministic. A real thread decrements
/// `live` under the lock, signals, and only then returns, so there is a span in
/// which the pool has counted it as gone while `is_finished` is still false.
/// This starter holds exactly that span open: the thread returns from `run()`
/// — so `live` is decremented — and then waits for its generation to pass before
/// the thread function ends.
fn start_lingering(pool: Arc<Pool>, first: Handoff) -> io::Result<super::ThreadHandle> {
    let hold = Arc::clone(LINGER.get_or_init(|| Arc::new(Release::new())));
    let held = hold.generation();
    thread::Builder::new()
        .name("lgwks-blocking".into())
        .spawn(move || {
            serve_pool(&pool, first);
            HELD_IN_WINDOW.fetch_add(1, Ordering::SeqCst);
            hold.wait_past(held);
            HELD_IN_WINDOW.fetch_sub(1, Ordering::SeqCst);
        })
}

/// How many threads [`start_lingering`] is holding between leaving the
/// accounting and returning: the second term of the handle identity, counted by
/// the threads themselves rather than inferred from a bound.
static HELD_IN_WINDOW: AtomicUsize = AtomicUsize::new(0);

/// Wait until exactly `count` threads are held in the window, or fail naming
/// the seed. The identity is checked against this count, so the check waits for
/// the threads rather than racing them.
fn wait_until_held(count: usize, seed: u64) {
    if poll_until(|| HELD_IN_WINDOW.load(Ordering::SeqCst) == count) {
        return;
    }
    let held = HELD_IN_WINDOW.load(Ordering::SeqCst);
    assert!(
        held == count,
        "seed {seed:#x}: {held} threads were held in the mid-exit window, not {count}, after \
         {HANG_BOUND:?}"
    );
}

/// The handle identity, against the count the fixture keeps: the list is the
/// live threads plus exactly the threads the fixture is holding.
fn assert_handles_equal_live_plus_held(pool: &Arc<Pool>, where_: &str, seed: u64) {
    let state = lock(&pool.state);
    let live = state.live;
    let held = state.handles.len();
    let in_window = HELD_IN_WINDOW.load(Ordering::SeqCst);
    assert_eq!(
        held,
        live.saturating_add(in_window),
        "seed {seed:#x} {where_}: {held} handles are not {live} live plus {in_window} threads \
         held between leaving the accounting and returning"
    );
}

/// Wait until every handle the pool still holds names a thread that has
/// returned, or fail naming the seed.
///
/// The difference between a departed thread and a departed-and-returned one is
/// what a reap acts on, so a case that reads the list after a release has to
/// wait for the return itself. `is_finished` never blocks, and the poll is
/// bounded so a thread that never returns fails with a message.
fn wait_until_all_returned(pool: &Arc<Pool>, seed: u64) {
    let returned = poll_until(|| {
        lock(&pool.state)
            .handles
            .iter()
            .all(super::ThreadHandle::is_finished)
    });
    if returned {
        return;
    }
    let held = lock(&pool.state).handles.len();
    assert!(
        held == 0,
        "seed {seed:#x}: {held} registered threads had not all returned after {HANG_BOUND:?}"
    );
}

/// One deterministic pass through the mid-exit window, as its trace hash.
///
/// The window is the second term of the handle identity: a thread that has left
/// the accounting still holds its handle, and a start keeps it while adding its
/// own. Here that is not a race but the fixture — a thread waits after `live` is
/// already decremented, and counts itself while it waits, so the identity
/// `handles == live + held` can be checked exactly rather than bounded.
fn mid_exit(seed: u64) -> u64 {
    let mut trace = initial_trace();
    fold(&mut trace, seed);
    let hold = Arc::clone(LINGER.get_or_init(|| Arc::new(Release::new())));
    // One thread, so the numbers below are exact: the window holds one
    // departed-and-unjoined handle, and a start adds exactly one more.
    let pool = Arc::new(Pool::tuned(1, start_lingering, CYCLE_KEEP_ALIVE));

    let first = spawn_blocking_on(&pool, || 1u32);
    assert_eq!(block_on(first), 1, "seed {seed:#x}: the first job must run");
    wait_until_empty(&pool, seed);
    wait_until_held(1, seed);
    {
        let state = lock(&pool.state);
        assert_eq!(
            (state.live, state.handles.len()),
            (0, 1),
            "seed {seed:#x}: the fixture must hold one thread mid-exit — gone from the \
             accounting, still unjoined"
        );
    }
    assert_handles_equal_live_plus_held(&pool, "in the mid-exit window", seed);
    fold(&mut trace, 60);

    // A start inside the window cannot reap the departing handle: that thread
    // has not returned. The list is then two handles for one ceiling, which is
    // the whole point of the bound being `live + ceiling + 1`.
    let second = spawn_blocking_on(&pool, || 2u32);
    {
        let state = lock(&pool.state);
        assert_eq!(
            (state.live, state.handles.len()),
            (1, 2),
            "seed {seed:#x}: a start inside the mid-exit window keeps the departing handle \
             and adds its own, so the list outgrows the ceiling"
        );
    }
    assert_eq!(
        block_on(second),
        2,
        "seed {seed:#x}: the second job must run"
    );
    wait_until_held(2, seed);
    assert_handles_equal_live_plus_held(&pool, "after the start in the window", seed);
    fold(&mut trace, 61);

    // Release both, wait for both to return, and start once more: now the two
    // departed handles are reapable, and the list is its own alone.
    hold.release();
    wait_until_empty(&pool, seed);
    wait_until_all_returned(&pool, seed);
    let third = spawn_blocking_on(&pool, || 3u32);
    assert_eq!(block_on(third), 3, "seed {seed:#x}: the third job must run");
    {
        let state = lock(&pool.state);
        assert_eq!(
            state.handles.len(),
            1,
            "seed {seed:#x}: a start after the threads returned reaps every departed handle"
        );
    }
    wait_until_held(0, seed);
    assert_handles_equal_live_plus_held(&pool, "after the reap", seed);
    fold(&mut trace, 62);

    // The last thread lingers too: release it, and the shutdown joins it rather
    // than reporting it, which is the whole of what a handle is for.
    wait_until_empty(&pool, seed);
    hold.release();
    wait_until_all_returned(&pool, seed);
    let report = pool.shutdown(GENEROUS);
    let state = lock(&pool.state);
    assert_eq!(
        report,
        PoolShutdown::Drained { threads: 1 },
        "seed {seed:#x}: the lingering thread returned, so the shutdown joined it"
    );
    assert!(
        state.handles.is_empty(),
        "seed {seed:#x}: the window case ended holding handles the pool never joined"
    );
    trace
}

#[test]
fn sim_a_start_inside_the_mid_exit_window_keeps_both_handles_and_the_next_reap_takes_one() {
    for seed in SWEEP_SEEDS {
        assert_same_seed_replays(mid_exit, seed);
    }
    let [first, second, ..] = SWEEP_SEEDS;
    assert_distinct_seeds_diverge(mid_exit, first, second);
}

#[test]
fn sim_every_scenario_drains_joins_and_never_loses_a_job() {
    let mut state = SWEEP_SEEDS.first().copied().unwrap_or_default();
    for _ in 0..SEEDS {
        journey(next_seed(&mut state));
    }
}

#[test]
fn sim_many_burst_and_idle_cycles_never_outgrow_the_ceiling_in_join_handles() {
    let mut state = SWEEP_SEEDS.first().copied().unwrap_or_default();
    for _ in 0..SEEDS {
        cycles(next_seed(&mut state));
    }
}

/// The replay oracle, over both journeys: one seed, one trace, each way.
#[test]
fn sim_a_seed_replays_its_pool_lifetime_trace() {
    for seed in SWEEP_SEEDS {
        assert_same_seed_replays(journey, seed);
        assert_same_seed_replays(cycles, seed);
        assert_same_seed_replays(mid_exit, seed);
    }
}

/// Two seeds, two traces, for both journeys.
#[test]
fn sim_distinct_seeds_diverge_in_their_pool_lifetime_trace() {
    let [first, second, ..] = SWEEP_SEEDS;
    assert_distinct_seeds_diverge(journey, first, second);
    assert_distinct_seeds_diverge(cycles, first, second);
}
