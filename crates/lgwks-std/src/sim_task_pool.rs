//! Deterministic simulation of the blocking pool's accounting (#264).
//!
//! The pool's correctness is its bookkeeping: which jobs are queued, how many
//! threads are alive, how many are parked, and how many wakeups a submitter
//! has claimed for them. Real threads reach only the interleavings the OS
//! happens to schedule. Here one seed schedules every interleaving instead:
//! submits with and without a queue bound, a thread start that the OS
//! refuses, a notify that finds no waiter, a spurious wake, a keep-alive that
//! runs out at the same moment a job arrives. Each step drives the same
//! [`PoolState`] transitions [`super::Pool`] calls, with numbered jobs in
//! place of closures, and every invariant is checked after every step.
//!
//! - **No job lost, none run twice.** Every admitted job runs exactly once by
//!   the time the pool drains; a refused job never runs.
//! - **Bounds.** Live threads never exceed the ceiling; a bounded submit is
//!   refused only at its bound.
//! - **Accounting.** Parked threads always equal `idle + wakeups`, and the pool
//!   ends with nothing queued, idle, claimed, or alive.
//! - **Progress.** A queued job always has a thread that will reach it: one
//!   free or running, or a claimed wakeup on its way.
//!
//! A failure names its seed and step; the same seed replays the same trace.

#[path = "../tests/support/rng.rs"]
mod rng;
#[path = "../tests/support/seeded_sweep.rs"]
mod seeded_sweep;

use super::{Admitted, PoolState, Woken};
use rng::Rng;
use seeded_sweep::{
    SWEEP_SEEDS, assert_distinct_seeds_diverge, assert_same_seed_replays, fold, fold_usize,
    initial_trace, next_seed,
};

/// Seeds per sweep: each one a different pool shape and schedule.
const SEEDS: usize = 2_000;

/// Scheduled steps per seed before the pool is drained.
const STEPS: usize = 2_000;

/// Steps the drain may take before a pool that will not settle is a failure.
const DRAIN_STEPS: usize = 1_000_000;

/// One simulated thread, by where it is in [`super::Pool::run`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Thread {
    /// Holding the lock with no job: about to take one or park.
    Free,
    /// Running a job outside the lock.
    Running(usize),
    /// Parked on the condvar.
    Waiting,
    /// Back from the condvar, waiting for the lock.
    Waking {
        /// Whether its keep-alive ran out.
        timed_out: bool,
    },
}

/// What happened to one job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fate {
    /// Admitted: in the queue or run.
    Admitted,
    /// Refused at the queue bound.
    AtCapacity,
    /// Refused because no thread could start and none was alive.
    NoThread,
}

/// One seeded run of the pool.
struct Sim {
    /// The state under test.
    state: PoolState<usize>,
    /// Every thread the pool started that has not exited.
    threads: Vec<Thread>,
    /// The ceiling on live threads.
    ceiling: usize,
    /// The bound bounded submits are held to.
    limit: usize,
    /// Each job's fate, by job number.
    fates: Vec<Fate>,
    /// How many times each job ran, by job number.
    runs: Vec<u32>,
    /// The schedule.
    rng: Rng,
    /// Every event and its outcome, folded.
    trace: u64,
    /// The seed, for failure messages.
    seed: u64,
    /// The step, for failure messages.
    step: usize,
}

impl Sim {
    /// A pool shaped by `seed`: ceiling 1 to 4, bound 1 to 8.
    fn new(seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let ceiling = rng.below(4).saturating_add(1);
        let limit = rng.below(8).saturating_add(1);
        let mut trace = initial_trace();
        fold_usize(&mut trace, ceiling);
        fold_usize(&mut trace, limit);
        Self {
            state: PoolState::new(),
            threads: Vec::new(),
            ceiling,
            limit,
            fates: Vec::new(),
            runs: Vec::new(),
            rng,
            trace,
            seed,
            step: 0,
        }
    }

    /// Where a failure happened.
    fn at(&self) -> String {
        format!(
            "seed {:#018x} step {} (ceiling {}, limit {}, threads {:?}, queued {}, live {}, idle {}, wakeups {})",
            self.seed,
            self.step,
            self.ceiling,
            self.limit,
            self.threads,
            self.state.queue.len(),
            self.state.live,
            self.state.idle,
            self.state.wakeups,
        )
    }

    /// Submit one job, bounded three times in four, failing a thread start
    /// one time in eight.
    fn submit(&mut self) {
        let job = self.fates.len();
        self.fates.push(Fate::Admitted);
        self.runs.push(0);
        let bounded = self.rng.below(4) != 0;
        let limit = bounded.then_some(self.limit);
        let queued = self.state.queue.len();
        let admitted = self.state.admit(job, limit, self.ceiling);
        fold_usize(&mut self.trace, 1);
        match admitted {
            Err(back) => {
                assert!(
                    back == job && bounded && queued >= self.limit,
                    "refused job {back} with {queued} queued: {}",
                    self.at()
                );
                if let Some(fate) = self.fates.get_mut(job) {
                    *fate = Fate::AtCapacity;
                }
                fold_usize(&mut self.trace, 2);
            }
            Ok(Admitted::Woke) => {
                // `notify_one` wakes one waiter if there is one; with none (each
                // parked thread already back from the condvar) the notify is
                // lost, and the claimed wakeup must carry the job instead.
                let waiting: Vec<usize> = self
                    .threads
                    .iter()
                    .enumerate()
                    .filter(|&(_, thread)| *thread == Thread::Waiting)
                    .map(|(index, _)| index)
                    .collect();
                let woken = waiting
                    .get(self.rng.below(waiting.len()))
                    .and_then(|&index| self.threads.get_mut(index));
                if let Some(thread) = woken {
                    *thread = Thread::Waking { timed_out: false };
                }
                fold_usize(&mut self.trace, 3);
            }
            Ok(Admitted::Queued) => fold_usize(&mut self.trace, 4),
            Ok(Admitted::Start) => {
                if self.rng.below(8) == 0 {
                    let live = self.state.live;
                    match self.state.start_failed() {
                        None => assert!(live > 0, "a job was left to no thread: {}", self.at()),
                        Some(back) => {
                            assert!(
                                back == job && live == 0,
                                "a failed start returned job {back} with {live} live: {}",
                                self.at()
                            );
                            if let Some(fate) = self.fates.get_mut(job) {
                                *fate = Fate::NoThread;
                            }
                        }
                    }
                    fold_usize(&mut self.trace, 5);
                } else {
                    self.state.started();
                    self.threads.push(Thread::Free);
                    fold_usize(&mut self.trace, 6);
                }
            }
        }
    }

    /// Advance thread `index` by one step of [`super::Pool::run`]. In a drain,
    /// a parked thread's keep-alive runs out rather than waking spuriously.
    fn advance(&mut self, index: usize, draining: bool) {
        let Some(&thread) = self.threads.get(index) else {
            return;
        };
        let next = match thread {
            Thread::Free => match self.state.take() {
                Some(job) => {
                    if let Some(runs) = self.runs.get_mut(job) {
                        *runs = runs.saturating_add(1);
                    }
                    fold_usize(&mut self.trace, job);
                    Some(Thread::Running(job))
                }
                None => {
                    self.state.park();
                    Some(Thread::Waiting)
                }
            },
            Thread::Running(_) => Some(Thread::Free),
            Thread::Waiting => Some(Thread::Waking {
                timed_out: draining || self.rng.below(3) != 0,
            }),
            Thread::Waking { timed_out } => match self.state.woken(timed_out) {
                Woken::Resume => Some(Thread::Free),
                Woken::Wait => Some(Thread::Waiting),
                Woken::Exit => None,
            },
        };
        fold_usize(&mut self.trace, index.saturating_add(16));
        match next {
            Some(next) => {
                if let Some(slot) = self.threads.get_mut(index) {
                    *slot = next;
                }
            }
            None => {
                self.threads.swap_remove(index);
            }
        }
    }

    /// Every invariant that holds between any two steps.
    fn check(&self) {
        assert_eq!(
            self.state.live,
            self.threads.len(),
            "live threads counted: {}",
            self.at()
        );
        assert!(
            self.state.live <= self.ceiling,
            "over the ceiling: {}",
            self.at()
        );
        let parked = self
            .threads
            .iter()
            .filter(|thread| matches!(thread, Thread::Waiting | Thread::Waking { .. }))
            .count();
        assert_eq!(
            parked,
            self.state.idle.saturating_add(self.state.wakeups),
            "parked threads are idle or claimed: {}",
            self.at()
        );
        let working = self
            .threads
            .iter()
            .any(|thread| matches!(thread, Thread::Free | Thread::Running(_)));
        assert!(
            self.state.queue.is_empty() || working || self.state.wakeups > 0,
            "a queued job has no thread that will reach it: {}",
            self.at()
        );
        assert!(
            self.runs.iter().all(|&runs| runs <= 1),
            "a job ran twice: {}",
            self.at()
        );
    }

    /// The scheduled steps, then a drain with no new work.
    fn run(mut self) -> u64 {
        for step in 0..STEPS {
            self.step = step;
            let pick = self.rng.below(self.threads.len().saturating_add(2));
            match pick.checked_sub(2) {
                None => self.submit(),
                Some(index) => self.advance(index, false),
            }
            self.check();
        }
        let mut drained = 0usize;
        while !self.threads.is_empty() {
            assert!(
                drained < DRAIN_STEPS,
                "the pool never settles: {}",
                self.at()
            );
            drained = drained.saturating_add(1);
            self.step = STEPS.saturating_add(drained);
            let index = self.rng.below(self.threads.len());
            self.advance(index, true);
            self.check();
        }
        assert!(
            self.state.queue.is_empty() && self.state.idle == 0 && self.state.wakeups == 0,
            "a drained pool holds nothing: {}",
            self.at()
        );
        for (job, (&fate, &runs)) in self.fates.iter().zip(&self.runs).enumerate() {
            let expected = u32::from(fate == Fate::Admitted);
            assert_eq!(
                runs,
                expected,
                "job {job} ({fate:?}) ran {runs} times: {}",
                self.at()
            );
            fold(&mut self.trace, u64::from(runs));
        }
        self.trace
    }
}

/// One seed's whole run, as its trace hash.
fn sweep(seed: u64) -> u64 {
    Sim::new(seed).run()
}

#[test]
fn sim_every_admitted_job_runs_once_under_every_interleaving() {
    let mut state = SWEEP_SEEDS.first().copied().unwrap_or_default();
    for _ in 0..SEEDS {
        sweep(next_seed(&mut state));
    }
}

#[test]
fn sim_a_seed_replays_its_trace_and_distinct_seeds_diverge() {
    for seed in SWEEP_SEEDS {
        assert_same_seed_replays(sweep, seed);
    }
    let [first, second, ..] = SWEEP_SEEDS;
    assert_distinct_seeds_diverge(sweep, first, second);
}
