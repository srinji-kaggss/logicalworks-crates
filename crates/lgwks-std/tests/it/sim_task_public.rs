//! Seeded simulation of the blocking pool through its public API (#264).
//!
//! `src/sim_task_pool.rs` drives the pool's bookkeeping with numbered jobs and
//! every interleaving a seed can schedule. What it cannot see is the contract a
//! caller relies on through `spawn_blocking` and `try_spawn_blocking`: real
//! closures on real pool threads, results carried back through `JoinHandle`,
//! panics resumed on the right awaiter, and the thread ceiling holding under a
//! burst. One seed draws each burst here: how many jobs, how long each one
//! works, and which of them fail. Every result is checked against a reference
//! computed from the seed alone, and folded into a trace that the same seed
//! must replay.

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use lgwks_std::task::{block_on, join_all, spawn_blocking, try_spawn_blocking};

use crate::rng::Rng;
use crate::seeded_sweep::seeded_stream;
use crate::seeded_sweep::{SWEEP_SEEDS, fold_usize, initial_trace, next_index};

/// The pool's documented thread ceiling.
const THREAD_CEILING: usize = 512;

/// The most jobs one seeded burst submits.
const MAX_BURST: usize = 2_048;

/// The most yields one seeded job spends working.
const MAX_WORK: usize = 64;

/// The value job `index` of the burst seeded by `seed` must return: computed
/// from the seed and position alone, so a result that crossed to another job
/// or another burst cannot match.
///
/// The value is an index-width number end to end, because the job returns it and
/// the trace folds it: the seed becomes the base through the same word-fold the
/// stream uses, and the position is added to it, so two positions of one burst
/// can never share a value and no conversion between the 64-bit seed and the
/// index width can truncate one.
fn expected(seed: u64, index: usize) -> usize {
    let mut stream = seeded_stream(seed);
    next_index(&mut stream).wrapping_add(index)
}

/// A burst drawn from `seed`: each job's position and how long it works.
fn burst(seed: u64) -> Vec<(usize, usize)> {
    let mut rng = Rng::new(seed);
    let jobs = rng.below(MAX_BURST).saturating_add(1);
    (0..jobs)
        .map(|index| (index, rng.below(MAX_WORK)))
        .collect()
}

/// Spends `work` yields, so jobs overlap on the pool's threads.
fn work_for(work: usize) {
    for _ in 0..work {
        std::thread::yield_now();
    }
}

/// Runs the burst seeded by `seed` through `spawn_blocking` and returns the
/// results in the order `join_all` hands them back.
fn run_burst(seed: u64) -> Vec<usize> {
    let handles: Vec<_> = burst(seed)
        .into_iter()
        .map(|(index, work)| {
            spawn_blocking(move || {
                work_for(work);
                expected(seed, index)
            })
        })
        .collect();
    block_on(join_all(handles))
}

/// One seed's burst, checked against the reference, as its trace hash.
fn pool_trace(seed: u64) -> u64 {
    let mut trace = initial_trace();
    for (index, value) in run_burst(seed).into_iter().enumerate() {
        assert_eq!(
            value,
            expected(seed, index),
            "seed {seed:#x}: job {index} returned another job's value"
        );
        fold_usize(&mut trace, value);
    }
    trace
}

#[test]
/// Every job of a seeded burst runs exactly once: none is lost in the queue and
/// none is handed to two threads.
fn every_job_of_a_seeded_burst_runs_exactly_once() {
    for seed in SWEEP_SEEDS {
        let jobs = burst(seed);
        let runs: Arc<Vec<AtomicU32>> = Arc::new(jobs.iter().map(|_| AtomicU32::new(0)).collect());
        let handles: Vec<_> = jobs
            .into_iter()
            .map(|(index, work)| {
                let runs = Arc::clone(&runs);
                spawn_blocking(move || {
                    work_for(work);
                    if let Some(count) = runs.get(index) {
                        count.fetch_add(1, Ordering::SeqCst);
                    }
                })
            })
            .collect();
        block_on(join_all(handles));
        for (index, count) in runs.iter().enumerate() {
            assert_eq!(
                count.load(Ordering::SeqCst),
                1,
                "seed {seed:#x}: job {index} must run exactly once"
            );
        }
    }
}

#[test]
/// Results come back in submission order whatever order the jobs finish in,
/// and each is its own job's value.
fn results_return_in_submission_order() {
    for seed in SWEEP_SEEDS {
        pool_trace(seed);
    }
}

#[test]
/// However large the burst, no more than the ceiling's threads run jobs at
/// once, and the burst does run concurrently rather than one job at a time.
fn a_burst_never_exceeds_the_thread_ceiling() {
    for seed in SWEEP_SEEDS {
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let handles: Vec<_> = burst(seed)
            .into_iter()
            .map(|(_, work)| {
                let running = Arc::clone(&running);
                let peak = Arc::clone(&peak);
                spawn_blocking(move || {
                    let now = running.fetch_add(1, Ordering::SeqCst).saturating_add(1);
                    peak.fetch_max(now, Ordering::SeqCst);
                    work_for(work.saturating_add(MAX_WORK));
                    running.fetch_sub(1, Ordering::SeqCst);
                })
            })
            .collect();
        let jobs = handles.len();
        block_on(join_all(handles));
        let seen = peak.load(Ordering::SeqCst);
        assert!(
            seen <= THREAD_CEILING,
            "seed {seed:#x}: {seen} jobs ran at once, over the {THREAD_CEILING}-thread ceiling"
        );
        assert!(
            jobs < 2 || seen >= 2,
            "seed {seed:#x}: a burst of {jobs} jobs never ran two at once"
        );
    }
}

#[test]
/// A job that panics fails its own awaiter only: every sibling still returns
/// its own value, and the panic does not take a pool thread's later jobs down.
fn a_panicking_job_fails_only_its_own_awaiter() {
    for seed in SWEEP_SEEDS {
        let mut rng = Rng::new(seed);
        let jobs = burst(seed);
        let failing: Vec<bool> = jobs.iter().map(|_| rng.below(8) == 0).collect();
        // Every job is submitted before any is awaited, so the failing ones
        // run beside their siblings rather than one at a time.
        let mut handles = Vec::with_capacity(jobs.len());
        for (index, work) in jobs {
            // `failing` was drawn with one entry per job, so this position is
            // one it holds; the index is bounded by the draw that filled it.
            let Some(&fails) = failing.get(index) else {
                continue;
            };
            handles.push(spawn_blocking(move || {
                work_for(work);
                if fails {
                    resume_unwind(Box::new(index));
                }
                expected(seed, index)
            }));
        }
        for (index, handle) in handles.into_iter().enumerate() {
            let outcome = catch_unwind(AssertUnwindSafe(|| block_on(handle)));
            let Some(&fails) = failing.get(index) else {
                continue;
            };
            match outcome {
                Ok(value) => {
                    assert!(!fails, "seed {seed:#x}: job {index} was meant to fail");
                    assert_eq!(
                        value,
                        expected(seed, index),
                        "seed {seed:#x}: job {index} returned another job's value"
                    );
                }
                Err(payload) => {
                    assert!(
                        fails,
                        "seed {seed:#x}: job {index} failed although it should not"
                    );
                    assert_eq!(
                        payload.downcast_ref::<usize>(),
                        Some(&index),
                        "seed {seed:#x}: job {index}'s awaiter must receive its own panic"
                    );
                }
            }
        }
    }
}

#[test]
/// Below the queue bound the bounded entry point never refuses: a refusal is
/// reserved for a full queue, and a burst that fits must all run.
fn try_spawn_blocking_below_the_queue_bound_never_refuses() {
    for seed in SWEEP_SEEDS {
        let mut handles = Vec::new();
        for (index, work) in burst(seed) {
            let handle = try_spawn_blocking(move || {
                work_for(work);
                expected(seed, index)
            });
            assert!(
                handle.is_ok(),
                "seed {seed:#x}: job {index} was refused below the queue bound"
            );
            handles.extend(handle.ok());
        }
        for (index, value) in block_on(join_all(handles)).into_iter().enumerate() {
            assert_eq!(
                value,
                expected(seed, index),
                "seed {seed:#x}: job {index} returned another job's value"
            );
        }
    }
}

#[test]
/// The replay and divergence oracles: one seed, one trace, and two seeds,
/// two traces.
fn a_burst_replays_its_trace_and_distinct_seeds_diverge() {
    for seed in SWEEP_SEEDS {
        assert_eq!(
            pool_trace(seed),
            pool_trace(seed),
            "seed {seed:#x}: the same seed must replay the same trace"
        );
    }
    let [first, second, ..] = SWEEP_SEEDS;
    assert_ne!(
        pool_trace(first),
        pool_trace(second),
        "seeds {first:#x} and {second:#x} must drive different bursts"
    );
}
