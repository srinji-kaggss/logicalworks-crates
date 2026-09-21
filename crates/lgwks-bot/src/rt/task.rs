//! Task sets, joining, and bounded-concurrency fan-out.
//!
//! This module deliberately exports **no function that hands back a handle to a
//! running task**. There is no `spawn`. A `JoinHandle` is droppable, and a
//! dropped handle is a task whose outcome nobody ever sees: the task keeps
//! running, the caller never learns whether it succeeded, and the only record
//! that it existed is gone. That is the failure the crate's background-work
//! rule exists to prevent, and a convention ("remember to join it") is not a
//! guarantee. The hole is closed by removing the constructor, not by
//! documenting it.
//!
//! Concurrent work is started two ways, and both own what they start:
//!
//! - [`Supervisor`](crate::rt::supervise::Supervisor) — a bounded set of tasks
//!   that reports every terminal outcome ([`TaskOutcome`]) and stops its tasks
//!   when it is dropped or cancelled. This is the module a bot's background work
//!   belongs in, and the only place a subprocess is started
//!   ([`Supervisor::spawn_process`]).
//! - [`JoinSet`] — the tracked envelope itself. Its constructor takes the tasks,
//!   its drop aborts them, and `join_next` yields [`JoinError`]-carrying results
//!   to the caller, so nothing is started that nothing owns.
//!
//! [`Supervisor`]: crate::rt::supervise::Supervisor
//! [`Supervisor::spawn_process`]: crate::rt::supervise::Supervisor::spawn_process
//! [`TaskOutcome`]: crate::rt::supervise::TaskOutcome
//!
//! What remains here besides the set is the fan-out an agent SDK actually
//! needs: [`join_all_bounded`] runs many futures, never exceeds a concurrency
//! ceiling, and returns results in input order regardless of completion order.
//!
//! # Starting work, at a glance
//!
//! - **One future, and you need its value.** Run it: `join_all_bounded(1,
//!   [work()]).await` returns a one-element vector, or await the future
//!   directly. There is no handle in either form, so there is nothing to drop.
//! - **One task now, read later.** Build a [`JoinSet`], [`JoinSet::spawn`] into
//!   it, and `join_next` when you want the result. Dropping the set aborts
//!   whatever has not finished.
//! - **Background work that must be cancellable and accounted for.** Use
//!   [`Supervisor`](crate::rt::supervise::Supervisor): it awaits a permit
//!   before it starts anything, reaps finished tasks, and reports how each one
//!   ended.
//!
//! # Non-`Send` futures
//!
//! Every verb in this crate is deliberately **not** `Send`, so a bot's own
//! futures cannot be placed on another worker thread — that is why
//! [`BoxFuture`](crate::BoxFuture) is unconstrained. A non-`Send` future is
//! driven by awaiting it, or by
//! [`lgwks_std::task::join_all`], which polls many
//! futures on the calling thread and returns their outputs in input order.
//!
//! There is no public API here that *spawns* a non-`Send` future. The
//! single-threaded spawn was the same un-owned handle as the multi-threaded one
//! — a caller could start a local task and forget it just as easily, with the
//! added trap that the handle's type could not even be named at the call site —
//! so it is gone for the same reason: nothing may be started that nothing owns.

pub use lgwks_deps::tokio::task::{AbortHandle, JoinError, JoinSet, yield_now};

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
/// output vector grows with input length, so a caller fanning out over thousands
/// of inputs needs no manual chunking to keep task memory bounded.
///
/// # Cancellation and failure
///
/// Dropping the returned future drops the [`JoinSet`], which aborts every task
/// that has not finished. A panicking input is resumed on the *awaiting* task,
/// matching `join_all`: it is not converted into a [`JoinError`], and it does
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
                // slot. `resume_unwind` rather than `panic!`: this is the
                // crate's form for a documented, unavoidable panic
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
