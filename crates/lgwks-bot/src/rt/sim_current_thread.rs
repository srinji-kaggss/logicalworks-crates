//! Deterministic simulation of [`Builder::current_thread`]: one seed draws a
//! schedule of tasks and their yields, runs it on a current-thread runtime and
//! hashes the order the steps ran in; another draws builder settings and checks
//! each build against the refusal model. Derived from deterministic simulation
//! testing as FoundationDB practised it: one thread, one seed, and the same seed
//! replays the same trace.

use std::cell::Cell;
use std::fmt;
use std::io;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use super::Builder;
use crate::rt::task::{JoinSet, yield_now};
use crate::sim_stream::{RECEIPT_START, Stream, fold};

/// Seeds swept.
const SEEDS: [u64; 8] = [0, 1, 2, 3, 5, 8, 13, 21];
/// The most tasks a schedule spawns.
const TASKS_MAX: usize = 8;
/// The most steps a task takes, yielding after each.
const STEPS_MAX: usize = 6;
/// Builder settings drawn per seed.
const SETTINGS: usize = 32;

/// One logged step: the task, its step, and whether it ran on the caller's thread.
type Step = (usize, usize, bool);

std::thread_local! {
    /// Set on the thread that calls `block_on` while a schedule runs, so a step
    /// can tell whether it ran there: a worker thread never sets it.
    static CALLER: Cell<bool> = const { Cell::new(false) };
}

/// Why a simulated schedule or setting failed.
#[derive(Debug)]
enum SimError {
    /// The runtime, a draw or a join refused.
    Refused(String),
    /// A step ran on a thread other than the one that called `block_on`.
    OffThread {
        /// The schedule's seed.
        seed: u64,
        /// The task whose step it was.
        task: usize,
    },
    /// A task's steps ran out of order, or the log missed a step.
    OutOfOrder {
        /// The schedule's seed.
        seed: u64,
        /// The task whose steps were out of order.
        task: usize,
    },
    /// The first task ran its second step before the second task ran at all.
    NoInterleave {
        /// The schedule's seed.
        seed: u64,
    },
    /// A build disagreed with the refusal model.
    Model {
        /// The settings' seed.
        seed: u64,
        /// Whether current-thread was asked for.
        current_thread: bool,
        /// The worker count asked for.
        workers: Option<NonZeroUsize>,
        /// Whether the build succeeded.
        built: bool,
    },
}

impl fmt::Display for SimError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Refused(ref why) => write!(formatter, "refused: {why}"),
            Self::OffThread { seed, task } => {
                write!(
                    formatter,
                    "seed {seed}: task {task} ran off the calling thread"
                )
            }
            Self::OutOfOrder { seed, task } => {
                write!(
                    formatter,
                    "seed {seed}: task {task} ran its steps out of order"
                )
            }
            Self::NoInterleave { seed } => {
                write!(
                    formatter,
                    "seed {seed}: the first task never yielded to the second"
                )
            }
            Self::Model {
                seed,
                current_thread,
                workers,
                built,
            } => write!(
                formatter,
                "seed {seed}: current_thread {current_thread} with workers {workers:?} built {built}"
            ),
        }
    }
}

impl std::error::Error for SimError {}

/// A refusal from the runtime, a draw or a join, kept as its own words.
fn refused(error: impl fmt::Display) -> SimError {
    SimError::Refused(error.to_string())
}

/// Append `step` to `log`; a poisoned log is still the log.
fn record(log: &Mutex<Vec<Step>>, step: Step) {
    match log.lock() {
        Ok(mut steps) => steps.push(step),
        Err(poisoned) => poisoned.into_inner().push(step),
    }
}

/// Run the schedule `seed` draws on a current-thread runtime; returns the
/// receipt of the order its steps ran in.
fn schedule(seed: u64) -> Result<u64, SimError> {
    let mut stream = Stream::new(seed);
    let tasks = stream.below(TASKS_MAX).map_err(refused)?.saturating_add(1);
    let mut plan = Vec::with_capacity(tasks);
    for _ in 0..tasks {
        plan.push(stream.below(STEPS_MAX).map_err(refused)?.saturating_add(1));
    }
    CALLER.with(|caller| caller.set(true));
    let log = Arc::new(Mutex::new(Vec::new()));
    let runtime = Builder::new().current_thread().build().map_err(refused)?;
    let spawned = plan.clone();
    let shared = Arc::clone(&log);
    let ran = runtime.block_on(async move {
        let mut owned = JoinSet::new();
        for (task, steps) in spawned.into_iter().enumerate() {
            let log = Arc::clone(&shared);
            owned.spawn(async move {
                for step in 0..steps {
                    record(&log, (task, step, CALLER.with(Cell::get)));
                    yield_now().await;
                }
            });
        }
        while let Some(joined) = owned.join_next().await {
            joined.map_err(refused)?;
        }
        Ok::<(), SimError>(())
    });
    CALLER.with(|caller| caller.set(false));
    ran?;
    let steps = match Arc::try_unwrap(log) {
        Ok(log) => match log.into_inner() {
            Ok(steps) => steps,
            Err(poisoned) => poisoned.into_inner(),
        },
        Err(_still_shared) => {
            let wrong = refused("a task outlived its join");
            lgwks_std::trace::debug!(error = %wrong, "schedule: a task outlived its join");
            return Err(wrong);
        }
    };
    check(seed, &plan, &steps)?;
    let mut receipt = RECEIPT_START;
    for (task, step, _) in steps {
        let task = u8::try_from(task).map_err(refused)?;
        let step = u8::try_from(step).map_err(refused)?;
        receipt = fold(receipt, &[task, step]);
    }
    Ok(receipt)
}

/// Every step ran on the caller's thread, each task's steps ran in order and
/// all of them ran, and the first task yielded to the second.
fn check(seed: u64, plan: &[usize], steps: &[Step]) -> Result<(), SimError> {
    let mut next = vec![0_usize; plan.len()];
    for &(task, step, here) in steps {
        if !here {
            let wrong = SimError::OffThread { seed, task };
            lgwks_std::trace::debug!(error = %wrong, "check: a step ran off the calling thread");
            return Err(wrong);
        }
        let expected = next
            .get_mut(task)
            .ok_or_else(|| refused("a step for no task"))?;
        if *expected != step {
            let wrong = SimError::OutOfOrder { seed, task };
            lgwks_std::trace::debug!(error = %wrong, "check: a task's steps ran out of order");
            return Err(wrong);
        }
        *expected = expected.saturating_add(1);
    }
    for (task, (ran, planned)) in next.iter().zip(plan).enumerate() {
        if ran != planned {
            let wrong = SimError::OutOfOrder { seed, task };
            lgwks_std::trace::debug!(error = %wrong, "check: a task did not run every step");
            return Err(wrong);
        }
    }
    let second_of_first = steps
        .iter()
        .position(|&(task, step, _)| (task, step) == (0, 1));
    let first_of_second = steps
        .iter()
        .position(|&(task, step, _)| (task, step) == (1, 0));
    if let (Some(late), Some(early)) = (second_of_first, first_of_second)
        && late < early
    {
        let wrong = SimError::NoInterleave { seed };
        lgwks_std::trace::debug!(error = %wrong, "check: the first task held the thread");
        return Err(wrong);
    }
    Ok(())
}

/// Build every setting `seed` draws and check it against the model: a worker
/// count beside current-thread is refused, and every other setting builds a
/// runtime that runs a future.
fn settings(seed: u64) -> Result<u64, SimError> {
    let mut stream = Stream::new(seed);
    let mut receipt = RECEIPT_START;
    for _ in 0..SETTINGS {
        let current_thread = stream.below(2).map_err(refused)? == 1;
        let workers = NonZeroUsize::new(stream.below(5).map_err(refused)?);
        let mut builder = Builder::new().worker_threads(workers);
        if current_thread {
            builder = builder.current_thread();
        }
        let refusal_expected = current_thread && workers.is_some();
        let built = match (builder.build(), refusal_expected) {
            (Err(error), true) if error.kind() == io::ErrorKind::InvalidInput => false,
            (Ok(runtime), false) if runtime.block_on(async { 7_u8 }) == 7 => true,
            (outcome, _) => {
                let wrong = SimError::Model {
                    seed,
                    current_thread,
                    workers,
                    built: outcome.is_ok(),
                };
                lgwks_std::trace::debug!(error = %wrong, "settings: a build disagreed with the model");
                return Err(wrong);
            }
        };
        let count = match workers {
            Some(count) => u8::try_from(count.get()).map_err(refused)?,
            None => 0,
        };
        receipt = fold(receipt, &[u8::from(current_thread), count, u8::from(built)]);
    }
    Ok(receipt)
}

#[test]
fn sim_seeded_schedules_interleave_on_the_calling_thread() -> Result<(), SimError> {
    for seed in SEEDS {
        let receipt = schedule(seed)?;
        assert_eq!(schedule(seed)?, receipt, "seed {seed} replays");
    }
    assert_ne!(schedule(1)?, schedule(2)?, "another seed, another schedule");
    Ok(())
}

#[test]
fn sim_drawn_builder_settings_are_refused_where_the_model_says() -> Result<(), SimError> {
    for seed in SEEDS {
        let receipt = settings(seed)?;
        assert_eq!(settings(seed)?, receipt, "seed {seed} replays");
    }
    assert_ne!(settings(1)?, settings(2)?, "another seed, other settings");
    Ok(())
}
