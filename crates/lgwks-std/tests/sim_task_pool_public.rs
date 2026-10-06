//! The blocking pool's public journey, in a process that owns its pool (#264).
//!
//! `sim_pool_lifetime` drives pools of its own; nothing outside this crate can
//! reach those. What only this binary can do is exercise the two public
//! functions against the real process-wide pool: `configure_blocking_pool`
//! against the pool's own creation (which is the whole of its arbitration), and
//! `shutdown_blocking_pool` against a pool that already has work in it.
//!
//! It owns its process for the same reason `tests/t03_non_yielding.rs` does and
//! for a sharper reason: **a shutdown closes admission for the life of the
//! process.** Any second test in this process would find a pool that admits
//! nothing, so this file is a separate binary beside `tests/it` and the crate's
//! manifest names it. Its cargo test name is `sim_task_pool_public`, which the
//! simulation-evidence lane counts.
//!
//! One journey, one seed, in order — a test that configured the pool and a
//! second test that did not would make the order the answer, which is worse
//! than a slow test:
//!
//! 1. `configure_blocking_pool(ceiling)` fixes the ceiling before first use.
//! 2. A seeded burst runs through `try_spawn_blocking`, every job returning its
//!    own value, and the same seed replays the same burst.
//! 3. Configuring again is refused, by name: the same ceiling is the one in
//!    force, any other is `AlreadyConfigured`.
//! 4. Shutdown lets the queued and running jobs finish and joins every thread.
//! 5. Admission is closed: both entry points refuse, the never-refusing one
//!    with the typed refusal as its awaiter's payload, and the job never runs.
//!    A second shutdown has nothing left to join.
//!
//! A failure names its seed.

// Fixtures more than one module uses, declared here once: a file loaded as a
// module twice is two copies of every type in it, and clippy refuses it. This
// binary is its own crate root, so it declares what it uses under the names the
// other families already gave them.
#[path = "support/gate.rs"]
mod gate;
#[path = "support/rng.rs"]
mod rng;
#[path = "support/seeded_sweep.rs"]
mod seeded_sweep;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use lgwks_std::task::{
    PoolConfigError, PoolShutdown, SpawnError, block_on, configure_blocking_pool, join_all,
    shutdown_blocking_pool, spawn_blocking, try_spawn_blocking,
};

use crate::gate::Gate;
use crate::rng::Rng;
use crate::seeded_sweep::{SWEEP_SEEDS, fold_usize, initial_trace, next_index};

/// The smallest ceiling a seed may draw: above one, so a burst of the drawn
/// size can be both under and over the ceiling.
const MIN_CEILING: usize = 2;

/// The largest ceiling a seed may draw. Small on purpose: a journey at 512
/// threads measures the host's thread budget, not the pool.
const MAX_CEILING: usize = 17;

/// Most jobs the replayable burst submits.
const MAX_BURST: usize = 64;

/// Most jobs the shutdown journey leaves in flight at the deadline.
const MAX_GATED: usize = 8;

/// The deadline a shutdown of a burst of gated jobs is given once the gate
/// opens: the jobs are then ordinary work, and nothing here waits on a
/// machine's speed.
const GENEROUS: Duration = Duration::from_secs(30);

/// A deadline shorter than jobs held at a shut gate can take, so the report
/// under test is the one a caller gets when its patience runs out.
const TOO_SHORT: Duration = Duration::from_millis(50);

/// The value job `index` of a burst seeded by `seed` must return: computed
/// from the seed and position alone, so a result that crossed to another job
/// or another burst cannot match.
fn expected(seed: u64, index: usize) -> usize {
    // The value is an index-width number end to end: the seed becomes the base
    // through the shared word fold and the position is added to it, so two
    // positions of one burst can never share a value and no conversion between
    // the 64-bit seed and the index width can truncate one.
    let mut stream = seed;
    next_index(&mut stream).wrapping_add(index)
}

/// The burst a seed draws: how many jobs, and how many yields each spends.
fn burst(seed: u64, jobs: usize) -> Vec<(usize, usize)> {
    let mut rng = Rng::new(seed);
    (0..jobs).map(|index| (index, rng.below(32))).collect()
}

/// Spends `work` yields, so the burst's jobs overlap on the pool's threads.
fn work_for(work: usize) {
    for _ in 0..work {
        std::thread::yield_now();
    }
}

/// Holds the burst's first job until a second job is running beside it, or
/// until [`GENEROUS`] has passed.
///
/// Without it, "two jobs ran at once" is a claim about the host's scheduler: on
/// a fast machine the first job can finish its yields before the second is
/// submitted, and a correct pool reports a peak of one. Holding the first job
/// makes overlap a property of the pool. A pool with a ceiling of two or more
/// starts the second job on another thread and releases this wait; a pool that
/// ran jobs one at a time never does, and the peak assertion then names it.
fn wait_for_company(running: &AtomicUsize) {
    let started = Instant::now();
    while running.load(Ordering::SeqCst) < 2 && started.elapsed() < GENEROUS {
        std::thread::yield_now();
    }
}

/// One seeded burst through the public bounded entry point, as its trace.
///
/// The peak is asserted but not folded: how many threads the burst had free at
/// once is the host's scheduling, not the seed's fact, and folding it would
/// make a replay of a correct run fail. What the seed fixes — and what the
/// trace therefore holds — is every job's own value, in submission order.
fn burst_trace(seed: u64, jobs: usize, ceiling: usize) -> u64 {
    let running = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::with_capacity(jobs);
    for (index, work) in burst(seed, jobs) {
        let running = Arc::clone(&running);
        let peak = Arc::clone(&peak);
        let holds_for_company = index == 0 && jobs >= 2;
        let handle = try_spawn_blocking(move || {
            let now = running.fetch_add(1, Ordering::SeqCst).saturating_add(1);
            peak.fetch_max(now, Ordering::SeqCst);
            if holds_for_company {
                wait_for_company(&running);
            }
            work_for(work);
            running.fetch_sub(1, Ordering::SeqCst);
            expected(seed, index)
        });
        assert!(
            handle.is_ok(),
            "seed {seed:#x}: job {index} was refused by a pool of {ceiling} threads \
             holding at most {MAX_BURST} jobs"
        );
        handles.extend(handle.ok());
    }
    let mut trace = initial_trace();
    for (index, value) in block_on(join_all(handles)).into_iter().enumerate() {
        assert_eq!(
            value,
            expected(seed, index),
            "seed {seed:#x}: job {index} returned another job's value"
        );
        fold_usize(&mut trace, value);
    }
    let seen = peak.load(Ordering::SeqCst);
    assert!(
        seen <= ceiling,
        "seed {seed:#x}: {seen} jobs ran at once, over the configured ceiling of {ceiling}"
    );
    assert!(
        jobs < 2 || seen >= 2,
        "seed {seed:#x}: a burst of {jobs} jobs never ran two at once"
    );
    trace
}

#[test]
fn the_public_pool_journey_configures_drains_joins_and_then_refuses() {
    let [seed, ..] = SWEEP_SEEDS;
    let mut rng = Rng::new(seed);
    let ceiling = rng
        .below(MAX_CEILING.saturating_sub(MIN_CEILING).saturating_add(1))
        .saturating_add(MIN_CEILING);
    let jobs = rng.below(MAX_BURST).saturating_add(1);
    let gated = rng.below(MAX_GATED.saturating_add(1));

    // 1. The ceiling, fixed before the pool has run anything.
    assert_eq!(
        configure_blocking_pool(ceiling),
        Ok(()),
        "seed {seed:#x}: the ceiling a startup configure asked for must be the one in force"
    );
    assert_eq!(
        configure_blocking_pool(0),
        Err(PoolConfigError::InvalidCeiling {
            requested: 0,
            minimum: 1
        }),
        "seed {seed:#x}: a ceiling below one can run nothing and must be refused"
    );

    // 2. A burst through the bounded entry point, and its replay.
    let first = burst_trace(seed, jobs, ceiling);
    assert_eq!(
        first,
        burst_trace(seed, jobs, ceiling),
        "seed {seed:#x}: the same seed must replay the same burst through the same pool"
    );

    // 3. A later configure is refused, and refused by name.
    assert_eq!(
        configure_blocking_pool(ceiling),
        Ok(()),
        "seed {seed:#x}: asking again for the ceiling in force describes the pool as it is"
    );
    let other = ceiling.saturating_add(1);
    assert_eq!(
        configure_blocking_pool(other),
        Err(PoolConfigError::AlreadyConfigured {
            requested: other,
            configured: ceiling
        }),
        "seed {seed:#x}: a configure that cannot take effect must never be silently ignored"
    );

    // 4. Shutdown, with a burst of gated jobs still in flight. The first
    // deadline is shorter than the jobs can take, because the gate is shut, so
    // it must report what is left rather than cancel it.
    let gate = Arc::new(Gate::new());
    let mut inflight = Vec::with_capacity(gated);
    for index in 0..gated {
        let gate = Arc::clone(&gate);
        inflight.push(spawn_blocking(move || {
            gate.pass();
            index
        }));
    }
    let report = shutdown_blocking_pool(TOO_SHORT);
    if gated == 0 {
        assert_eq!(
            report,
            PoolShutdown::Drained { threads: 0 },
            "seed {seed:#x}: an idle pool has nothing to wait for"
        );
    } else {
        assert!(
            matches!(report, PoolShutdown::DeadlineExceeded { running, .. } if running > 0),
            "seed {seed:#x}: {gated} jobs held at a shut gate cannot finish inside \
             {TOO_SHORT:?}, so the deadline must report the threads still running, got {report:?}"
        );
    }
    // The gate opens: a later shutdown waits for and joins what the deadline
    // left, and no job is cancelled by either.
    gate.release();
    let report = shutdown_blocking_pool(GENEROUS);
    assert!(
        matches!(report, PoolShutdown::Drained { .. }),
        "seed {seed:#x}: with the gate open every job finishes and every thread is joined, \
         got {report:?}"
    );
    if let PoolShutdown::Drained { threads } = report {
        assert!(
            threads <= ceiling,
            "seed {seed:#x}: {threads} threads were joined under a ceiling of {ceiling}"
        );
        assert_eq!(
            threads > 0,
            gated > 0,
            "seed {seed:#x}: {gated} jobs in flight left {threads} threads to join"
        );
    }
    assert_eq!(
        block_on(join_all(inflight)),
        (0..gated).collect::<Vec<_>>(),
        "seed {seed:#x}: shutdown let the queued and running jobs finish"
    );

    // 5. Admission is closed, and a second shutdown has nothing left to join.
    let ran = Arc::new(AtomicBool::new(false));
    let witness = Arc::clone(&ran);
    assert!(
        matches!(
            try_spawn_blocking(move || witness.load(Ordering::SeqCst)),
            Err(SpawnError::Shutdown)
        ),
        "seed {seed:#x}: a drained pool refuses the bounded entry point, typed"
    );
    // A second Arc for the never-refusing arm: that job is refused, so it never
    // runs, and `ran` is what proves it.
    let refused = Arc::clone(&ran);
    let handle = spawn_blocking(move || refused.load(Ordering::SeqCst));
    let outcome = catch_unwind(AssertUnwindSafe(|| block_on(handle)));
    let payload = outcome
        .err()
        .map(|payload| payload.downcast::<SpawnError>());
    assert!(
        matches!(payload, Some(Ok(ref error)) if matches!(**error, SpawnError::Shutdown)),
        "seed {seed:#x}: the never-refusing entry point fails its awaiter with the refusal, \
         got {payload:?}"
    );
    assert!(
        !ran.load(Ordering::SeqCst),
        "seed {seed:#x}: a job refused after shutdown never ran"
    );
    assert_eq!(
        shutdown_blocking_pool(GENEROUS),
        PoolShutdown::Drained { threads: 0 },
        "seed {seed:#x}: a pool whose threads were all joined has nothing left to join"
    );
}
