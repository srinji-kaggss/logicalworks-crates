//! Task spawning, joining, and bounded-concurrency fan-out.
//!
//! The handles and error types are tokio's; the value this module adds is
//! [`join_all_bounded`], which is the fan-out an agent SDK actually needs:
//! run many futures, never exceed a concurrency ceiling, and return results in
//! input order regardless of completion order.

use std::future::Future;

pub use lgwks_deps::tokio::task::{
    AbortHandle, JoinError, JoinHandle, JoinSet, spawn_blocking, yield_now,
};

/// Place a future on the current runtime without waiting for it.
///
/// # Panics
///
/// Panics when called outside a runtime context. Inside [`crate::Runtime::block_on`]
/// or a spawned task, the current runtime is always present.
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    lgwks_deps::tokio::spawn(future)
}

/// Run `futures` with at most `limit` in flight at once, returning their
/// outputs in input order.
///
/// Every input is polled to completion; `limit` bounds concurrency, not the
/// number of futures accepted. `limit` of zero is treated as one, and a `limit`
/// above [`Semaphore::MAX_PERMITS`][max] is clamped to it. An empty input
/// resolves immediately.
///
/// [max]: lgwks_deps::tokio::sync::Semaphore::MAX_PERMITS
///
/// # Ordering
///
/// Output `i` is the output of input `i`, whichever completes first. This is
/// the property a plain [`JoinSet`] does not give: `JoinSet::join_next` yields
/// completion order.
///
/// # Concurrency and memory bound
///
/// The bound is enforced by replenishment: at most `limit` tasks are spawned
/// and awaited at once, and each completion spawns the next pending input. The
/// retained [`JoinSet`] therefore never exceeds `limit` entries, and only the
/// output vector grows with input length — a caller fanning out over thousands
/// of inputs needs no manual chunking to keep task memory bounded.
///
/// # Cancellation and failure
///
/// Dropping the returned future drops the [`JoinSet`], which aborts every task
/// that has not finished. A panicking input is resumed on the *awaiting* task,
/// matching `join_all` — it is not converted into a [`JoinError`], and it does
/// not abort the process. The remaining tasks are then aborted as the set
/// drops. A completed input is never silently dropped from the result vector.
#[cfg(feature = "sync")]
pub async fn join_all_bounded<F, T>(limit: usize, futures: impl IntoIterator<Item = F>) -> Vec<T>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    use lgwks_deps::tokio::sync::Semaphore;

    let limit = limit.clamp(1, Semaphore::MAX_PERMITS);
    let mut inputs = futures.into_iter();
    let (lower, _) = inputs.size_hint();
    // Index slots are appended as inputs are spawned, so the result vector is
    // the only state that grows with input length; the JoinSet stays ≤ limit.
    let mut slots: Vec<Option<T>> = Vec::new();
    if lower > 0 {
        slots.reserve(lower);
    }
    let mut set = JoinSet::new();
    let mut next_index: usize = 0;
    let mut total: usize = 0;

    let spawn_next = |set: &mut JoinSet<(usize, T)>,
                      slots: &mut Vec<Option<T>>,
                      next_index: &mut usize,
                      total: &mut usize,
                      future: F| {
        let index = *next_index;
        *next_index += 1;
        *total += 1;
        slots.push(None);
        set.spawn(async move {
            let output = future.await;
            (index, output)
        });
    };

    // Prime the pipeline: at most `limit` tasks exist before the first
    // completion, so a 64-input fan-out over limit 4 holds 4 tasks, not 64.
    for _ in 0..limit {
        match inputs.next() {
            Some(future) => spawn_next(&mut set, &mut slots, &mut next_index, &mut total, future),
            None => break,
        }
    }
    if total == 0 {
        return Vec::new();
    }

    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((index, output)) => slots[index] = Some(output),
            Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            Err(_cancelled) => {
                // Not reachable here: nothing aborts this set while it is
                // awaited. A shrink would break the INV-RT-BOUNDED-FANOUT
                // guarantee, so fail loudly rather than fabricate a slot.
                panic!("join_all_bounded: a task was cancelled before it produced a value");
            }
        }
        match inputs.next() {
            Some(future) => spawn_next(&mut set, &mut slots, &mut next_index, &mut total, future),
            None => {
                if set.is_empty() {
                    break;
                }
            }
        }
    }
    slots
        .into_iter()
        .map(|slot| slot.expect("join_all_bounded: every input produces exactly one output"))
        .collect()
}
