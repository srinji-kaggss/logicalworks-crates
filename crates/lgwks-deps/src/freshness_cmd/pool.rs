//! A fixed set of workers drawing items from one cursor, for work that blocks.
//!
//! The in-flight bound is the number of workers, and nothing else: a worker
//! takes the next item only when it has finished its last, so no more than
//! [`workers_for`] items are ever in progress. That makes the bound a property
//! of construction rather than of a counter someone must remember to decrement,
//! and it is the only thing a caller can raise or lower. The workers run on
//! `lgwks_std::task`'s blocking pool, which is bounded and owned by the process,
//! and are awaited together, so nothing outlives the call that started it.
//!
//! A job that panics costs its own item and no other: the panic is caught at
//! the item, the item's slot reports it, and the worker takes the next one.

use std::fmt;
use std::iter::Enumerate;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};
use std::vec::IntoIter;

use lgwks_std::task::{block_on, join_all, try_spawn_blocking};

/// The shared cursor over the items, each with its position.
type Cursor<I> = Mutex<Enumerate<IntoIter<I>>>;

/// What one worker brings back: its items' positions and outcomes.
type Finished<T> = Vec<(usize, Result<T, PoolError>)>;

/// Why an item has no result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PoolError {
    /// The job panicked on this item.
    Panicked,
    /// No worker could be started, so the item was never attempted.
    Refused {
        /// The blocking pool's reason.
        reason: String,
    },
    /// A worker ended without reporting this item. It cannot happen while the
    /// workers are well behaved; it is a result so the totals still add up if
    /// one is not.
    Lost,
}

impl fmt::Display for PoolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Panicked => formatter.write_str("the lookup panicked"),
            Self::Refused { ref reason } => write!(formatter, "no worker could start: {reason}"),
            Self::Lost => formatter.write_str("the lookup was lost"),
        }
    }
}

impl std::error::Error for PoolError {}

/// What a run produced.
#[derive(Debug)]
pub(super) struct Done<T> {
    /// One outcome per item, in the order the items were given.
    pub(super) results: Vec<Result<T, PoolError>>,
    /// How many workers ran, which is the most items that were ever in progress
    /// at once.
    pub(super) workers: usize,
}

/// How many workers serve `items` items under a bound of `limit`.
///
/// Never more than the bound, and never more than there is work for: a worker
/// with no item would only hold a thread.
pub(super) fn workers_for(limit: usize, items: usize) -> usize {
    limit.min(items)
}

/// Runs `job` on every item with at most `limit` in progress at once.
///
/// Results come back in input order whatever order the items finished in.
pub(super) fn run<I, T, F>(items: Vec<I>, limit: usize, job: F) -> Done<T>
where
    I: Send + 'static,
    T: Send + 'static,
    F: Fn(I) -> T + Send + Sync + 'static,
{
    let total = items.len();
    let workers = workers_for(limit, total);
    let cursor: Arc<Cursor<I>> = Arc::new(Mutex::new(items.into_iter().enumerate()));
    let job = Arc::new(job);
    let mut handles = Vec::with_capacity(workers);
    let mut refusal = None;
    for _ in 0..workers {
        let worker_cursor = Arc::clone(&cursor);
        let worker_job = Arc::clone(&job);
        match try_spawn_blocking(move || work(&worker_cursor, worker_job.as_ref())) {
            Ok(handle) => handles.push(handle),
            Err(error) => {
                refusal = Some(error.to_string());
                break;
            }
        }
    }
    let started = handles.len();
    let finished: Vec<Finished<T>> = block_on(join_all(handles));

    let mut slots: Vec<Option<Result<T, PoolError>>> = (0..total).map(|_| None).collect();
    for (position, outcome) in finished.into_iter().flatten() {
        if let Some(slot) = slots.get_mut(position) {
            *slot = Some(outcome);
        }
    }
    // With no worker started, every item is still on the cursor: say so for
    // each, rather than leaving a hole.
    let reason = match refusal {
        Some(reason) => reason,
        None => "the pool started no worker".to_owned(),
    };
    let results = slots
        .into_iter()
        .map(|slot| match slot {
            Some(outcome) => outcome,
            None if started == 0 => Err(PoolError::Refused {
                reason: reason.clone(),
            }),
            None => Err(PoolError::Lost),
        })
        .collect();
    Done {
        results,
        workers: started,
    }
}

/// One worker: takes the next item, runs the job on it, and repeats until the
/// cursor is empty.
fn work<I, T, F>(cursor: &Cursor<I>, job: &F) -> Finished<T>
where
    F: Fn(I) -> T,
{
    let mut finished = Vec::new();
    while let Some((position, item)) = next(cursor) {
        let outcome = match catch_unwind(AssertUnwindSafe(|| job(item))) {
            Ok(value) => Ok(value),
            Err(_payload) => Err(PoolError::Panicked),
        };
        finished.push((position, outcome));
    }
    finished
}

/// The next item, taking the lock for that and nothing else.
///
/// The guard is gone before the job runs, so no lock is held across the
/// blocking call. A poisoned lock still guards a consistent cursor, because
/// the only thing done under it is moving one item out.
fn next<I>(cursor: &Cursor<I>) -> Option<(usize, I)> {
    let mut guard = match cursor.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.next()
}

#[cfg(test)]
mod sim_pool;
