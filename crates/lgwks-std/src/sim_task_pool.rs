//! Deterministic simulation of the blocking pool's accounting (#264).
//!
//! The pool's correctness is its bookkeeping: which jobs are queued, how many
//! threads are alive, how many are parked, and how many wakeups a submitter
//! has claimed for them. Real threads reach only the interleavings the OS
//! happens to schedule. Here one seed schedules every interleaving instead:
//! submits with and without a queue bound, a thread start that the OS
//! refuses, a notify that finds no waiter, a spurious wake, a keep-alive that
//! runs out at the same moment a job arrives, a configure attempt that must
//! leave the running pool's ceiling where it stands, and a shutdown that
//! closes admission at a seeded step and must still see every admitted job
//! run exactly once. Each step drives the same [`PoolState`] transitions
//! [`super::Pool`] calls, with numbered jobs in place of closures, and every
//! invariant is checked after every step.
//!
//! - **No job lost, none run twice.** Every admitted job runs exactly once by
//!   the time the pool drains; a refused job never runs — at the queue bound,
//!   for want of a thread, or because admission had closed.
//! - **Bounds.** Live threads never exceed the ceiling, and the ceiling never
//!   moves once the pool runs; a bounded submit is refused only at its bound
//!   or after shutdown begins.
//! - **Accounting.** Parked threads always equal `idle + wakeups`, and the
//!   pool ends with nothing queued, idle, claimed, or alive.
//! - **Hand-off.** A job that starts a thread is that thread's first job,
//!   never queued; a start that fails queues it beside a live thread or hands
//!   it back.
//! - **Progress.** A queued job always has a thread that will reach it: one
//!   free or running, or a claimed wakeup on its way — through a shutdown
//!   too, because a thread only leaves once the queue it would reach is
//!   empty.
//!
//! A failure names its seed and step; the same seed replays the same trace.

// The seeded generator and replay harness are declared once, by the parent
// module, and shared with the pool-lifetime family: a file loaded as a module
// twice is two copies of every type in it, which clippy refuses.
use super::{Admitted, PoolState, Refused, Woken};
use super::{rng, seeded_sweep};
use rng::Rng;
use seeded_sweep::{
    SWEEP_SEEDS, assert_distinct_seeds_diverge, assert_same_seed_replays, fold, fold_usize,
    initial_trace, next_seed,
};

/// Seeds per sweep: each one a different pool shape and schedule.
const SEEDS: usize = 2_000;

/// Scheduled steps per seed before the pool is drained.
const STEPS: usize = 2_000;

/// Steps the settle may take before a pool that will not settle is a failure.
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
    /// Refused because the pool no longer admits work.
    Draining,
}

/// The arms a configure attempt can meet, folded by number. These mirror
/// [`super::configure_decision`], which the unit tests drive directly; what
/// the simulation adds is the invariant the arms stand for — a running pool's
/// ceiling never moves.
const CONFIGURE_INVALID: u64 = 20;
const CONFIGURE_IN_FORCE: u64 = 21;
const CONFIGURE_ALREADY: u64 = 22;
const CONFIGURE_IN_USE: u64 = 23;

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
    /// Whether this pool was built by a configure call, as
    /// [`super::configure_blocking_pool`] builds it, or by first use.
    configured_at_birth: bool,
    /// The scheduled step at which shutdown closes admission.
    drain_at: usize,
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
    /// A pool shaped by `seed`: ceiling 1 to 4, bound 1 to 8, a birth by
    /// configure or by first use, and the step shutdown arrives at.
    fn new(seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let ceiling = rng.below(4).saturating_add(1);
        let limit = rng.below(8).saturating_add(1);
        let configured_at_birth = rng.below(4) == 0;
        let drain_at = rng.below(STEPS);
        let mut trace = initial_trace();
        fold_usize(&mut trace, ceiling);
        fold_usize(&mut trace, limit);
        fold(&mut trace, u64::from(configured_at_birth));
        fold_usize(&mut trace, drain_at);
        Self {
            state: PoolState::new(),
            threads: Vec::new(),
            ceiling,
            limit,
            configured_at_birth,
            drain_at,
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
            "seed {:#018x} step {} (ceiling {}, limit {}, draining {}, threads {:?}, queued {}, live {}, idle {}, wakeups {})",
            self.seed,
            self.step,
            self.ceiling,
            self.limit,
            self.state.draining,
            self.threads,
            self.state.queue.len(),
            self.state.live,
            self.state.idle,
            self.state.wakeups,
        )
    }

    /// Submit one job, bounded three times in four, failing a thread start
    /// one time in eight. After shutdown begins, every submit is refused.
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
            Err(Refused::AtCapacity(back)) => {
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
            Err(Refused::Draining(back)) => {
                assert!(
                    back == job && self.state.draining,
                    "job {back} was refused as draining while the pool still admits: {}",
                    self.at()
                );
                if let Some(fate) = self.fates.get_mut(job) {
                    *fate = Fate::Draining;
                }
                fold_usize(&mut self.trace, 8);
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
            Ok(Admitted::Start(first)) => {
                assert!(
                    first == job,
                    "admission started job {first}, not {job}: {}",
                    self.at()
                );
                if self.rng.below(8) == 0 {
                    let live = self.state.live;
                    match self.state.start_failed(first) {
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
                    // The new thread runs its first job before it ever
                    // takes the lock.
                    self.state.started();
                    if let Some(runs) = self.runs.get_mut(first) {
                        *runs = runs.saturating_add(1);
                    }
                    self.threads.push(Thread::Running(first));
                    fold_usize(&mut self.trace, 6);
                }
            }
        }
    }

    /// Attempt to fix the ceiling mid-run, as [`super::configure_blocking_pool`]
    /// would: the pool exists, so every arm refuses, the ceiling stands, and
    /// no work the pool holds is moved. The arms mirror
    /// [`super::configure_decision`], which the unit tests drive directly;
    /// what this step adds is that no arm disturbs the pool's accounting.
    fn configure(&mut self) {
        let proposed = self.rng.below(9);
        let before = (
            self.state.queue.len(),
            self.state.live,
            self.state.idle,
            self.state.wakeups,
        );
        let arm = if proposed < 1 {
            CONFIGURE_INVALID
        } else if self.configured_at_birth {
            if proposed == self.ceiling {
                CONFIGURE_IN_FORCE
            } else {
                CONFIGURE_ALREADY
            }
        } else {
            CONFIGURE_IN_USE
        };
        assert_eq!(
            before,
            (
                self.state.queue.len(),
                self.state.live,
                self.state.idle,
                self.state.wakeups
            ),
            "a configure attempt moved work it must not touch: {}",
            self.at()
        );
        fold(&mut self.trace, arm);
    }

    /// Advance thread `index` by one step of [`super::Pool::run`]. In a
    /// settle, a parked thread's keep-alive runs out rather than waking
    /// spuriously.
    fn advance(&mut self, index: usize, settling: bool) {
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
                    if self.state.park_or_retire() {
                        None
                    } else {
                        Some(Thread::Waiting)
                    }
                }
            },
            Thread::Running(_) => Some(Thread::Free),
            Thread::Waiting => Some(Thread::Waking {
                timed_out: settling || self.rng.below(3) != 0,
            }),
            Thread::Waking { timed_out } => match self.state.woken(timed_out) {
                Woken::Resume => Some(Thread::Free),
                // A wake that carried no claimed wakeup: the drain rules
                // decide whether this thread works or leaves.
                Woken::Wait => match self.state.drain_wake() {
                    Woken::Resume => Some(Thread::Free),
                    Woken::Exit => None,
                    Woken::Wait => Some(Thread::Waiting),
                },
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

    /// The scheduled steps — with shutdown closing admission at its seeded
    /// step — then a settle with no new work, then the accounting.
    fn run(mut self) -> u64 {
        for step in 0..STEPS {
            self.step = step;
            if step == self.drain_at {
                // The broadcast a shutdown makes under its lock: every parked
                // thread comes back to re-check, none is left to sleep
                // through the close.
                self.state.begin_drain();
                for thread in &mut self.threads {
                    if *thread == Thread::Waiting {
                        *thread = Thread::Waking { timed_out: false };
                    }
                }
                fold_usize(&mut self.trace, 9);
            }
            let pick = self.rng.below(self.threads.len().saturating_add(3));
            match pick {
                0 => self.submit(),
                1 => self.configure(),
                index => self.advance(index.saturating_sub(2), false),
            }
            self.check();
        }
        let mut settled = 0usize;
        while !self.threads.is_empty() {
            assert!(
                settled < DRAIN_STEPS,
                "the pool never settles: {}",
                self.at()
            );
            settled = settled.saturating_add(1);
            self.step = STEPS.saturating_add(settled);
            let index = self.rng.below(self.threads.len());
            self.advance(index, true);
            self.check();
        }
        assert!(
            self.state.draining,
            "shutdown began at step {} but the pool never closed admission: {}",
            self.drain_at,
            self.at()
        );
        assert!(
            self.state.queue.is_empty()
                && self.state.idle == 0
                && self.state.wakeups == 0
                && self.state.live == 0,
            "a settled pool holds nothing: {}",
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
            fold(
                &mut self.trace,
                match fate {
                    Fate::Admitted => 30,
                    Fate::AtCapacity => 31,
                    Fate::NoThread => 32,
                    Fate::Draining => 33,
                },
            );
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
    // `SWEEP_SEEDS` is a non-empty const array, so its first seed is a value
    // the pattern binds rather than one an `Option` could withhold.
    let [first_seed, ..] = SWEEP_SEEDS;
    let mut state = first_seed;
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
