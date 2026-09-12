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
/// # Concurrency bound
///
/// The bound is enforced by a shared semaphore acquired inside each task, so a
/// task holds its permit for the whole of its future. All futures are placed on
/// the runtime up front; the semaphore is what serializes them. A caller that
/// also needs to bound *retained memory* must chunk its input.
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
    use std::sync::Arc;

    let items: Vec<F> = futures.into_iter().collect();
    let total = items.len();
    if total == 0 {
        return Vec::new();
    }

    let permits = Arc::new(Semaphore::new(limit.clamp(1, Semaphore::MAX_PERMITS)));
    let mut set = JoinSet::new();
    for (index, future) in items.into_iter().enumerate() {
        let permits = Arc::clone(&permits);
        set.spawn(async move {
            let permit = permits
                .acquire()
                .await
                .expect("join_all_bounded: the semaphore is never closed");
            let output = future.await;
            drop(permit);
            (index, output)
        });
    }

    let mut slots: Vec<Option<T>> = (0..total).map(|_| None).collect();
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
    }
    slots
        .into_iter()
        .map(|slot| slot.expect("join_all_bounded: every input produces exactly one output"))
        .collect()
}
