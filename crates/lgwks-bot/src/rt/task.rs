//! Task spawning, joining, and bounded-concurrency fan-out.
//!
//! The handles and error types are tokio's; the value this module adds is
//! [`join_all_bounded`], which is the fan-out an agent SDK actually needs:
//! run many futures, never exceed a concurrency ceiling, and return results in
//! input order regardless of completion order.
//!
//! # Send and non-`Send` tasks
//!
//! [`spawn`] requires `Send`, because it may place the task on any worker
//! thread. Every verb in this crate is deliberately **not** `Send` — a domain
//! may hold thread-local state, which is the whole reason
//! [`BoxFuture`](crate::BoxFuture) is unconstrained — so a bot's own futures
//! cannot go through it.
//!
//! [`LocalSet`] is the other half: it runs non-`Send` futures on the thread that
//! owns it, and [`spawn_local`] places a task there. A bot driven on one thread
//! uses these; a task that may migrate between workers uses [`spawn`]. Both are
//! needed, and only one of them was here before.

use std::future::Future;

pub use lgwks_deps::tokio::task::{
    AbortHandle, JoinError, JoinHandle, JoinSet, LocalSet, spawn_blocking, yield_now,
};

/// Place a non-`Send` future on the [`LocalSet`] running on this thread.
///
/// A re-export rather than a wrapper, unlike [`spawn`]: `clippy.toml` bans
/// `tokio::spawn` by path because an untracked task is dropped on the floor, but
/// the local variant carries no such history, and a call site here resolves to
/// this crate's path either way.
///
/// # Panics
///
/// Panics if there is no [`LocalSet`] running on this thread. Running one is the
/// point: a future that is not `Send` has nowhere else to go.
pub use lgwks_deps::tokio::task::spawn_local;

/// Place a future on the current runtime without waiting for it.
///
/// This is the estate's replacement for a bare `tokio::spawn` (`clippy.toml`),
/// and it is a wrapper rather than a re-export for exactly that reason.
/// `disallowed_methods` matches the *resolved* path, so re-exporting the
/// engine's `spawn` gets flagged at every consumer call site — the config would
/// name this function as the replacement and then refuse every call to it.
/// The wrapper resolves to this crate's own path at the call site, so the ban
/// keeps catching raw `tokio::spawn` while the sanctioned path stays usable. It
/// is also the single place the engine call is spelled, which is why the
/// reasoned `expect` below can be narrow and audited instead of being repeated
/// in every caller.
///
/// The bounds are the engine's own, so nothing new reaches this crate's public
/// surface. `#[track_caller]` so a call made outside a runtime reports the
/// caller's location rather than this line, matching the engine's own `spawn`.
///
/// # Panics
///
/// Panics when called outside a runtime context. Inside [`crate::Runtime::block_on`]
/// or a spawned task, the current runtime is always present.
#[track_caller]
#[expect(
    clippy::disallowed_methods,
    reason = "this function is the replacement clippy.toml names for `tokio::spawn`; the one call \
              that implements it is the only place the raw path may legally appear"
)]
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    lgwks_deps::tokio::task::spawn(future)
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
        // Bound: both counters advance only when a future is taken from the
        // input iterator, and each increment also pushes one entry onto
        // `slots`. Reaching `usize::MAX` would require that many live
        // allocations, which no address space holds, so the saturating ceiling
        // is unreachable and the counters stay exact.
        *next_index = (*next_index).saturating_add(1);
        *total = (*total).saturating_add(1);
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
                // guarantee, so fail on the awaiter rather than fabricate a
                // slot. `resume_unwind` rather than `panic!` — this is the
                // estate's form for a documented, unavoidable panic
                // (`lgwks_std::task::JoinHandle` uses it for the same reason):
                // it reports on the awaiting task and never aborts the process.
                std::panic::resume_unwind(Box::new(
                    "join_all_bounded: a task was cancelled before it produced a value",
                ))
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
        .map(|slot| match slot {
            Some(output) => output,
            // Same reasoning as the cancellation arm: every spawned input fills
            // exactly one slot, so a `None` here means the replenishment loop
            // lost a value. Fail on the awaiter instead of dropping the element
            // and returning a short vector.
            None => std::panic::resume_unwind(Box::new(
                "join_all_bounded: every input produces exactly one output",
            )),
        })
        .collect()
}
