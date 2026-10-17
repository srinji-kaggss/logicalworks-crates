//! The budgeted loop, and the panic rendering every task report shares.

use super::{Budget, MAX_PANIC_MESSAGE_CHARS, Outcome, YIELD_INTERVAL};
use crate::rt::cancel::CancellationToken;
use crate::rt::clock::{Clock, TimeSource};
use crate::rt::task::yield_now;
use std::any::Any;
use std::time::Duration;
/// Render a panic payload, keeping the head of it.
///
/// A payload is whatever the panicking code passed, so it is arbitrary text
/// rather than a message with a shape. The two `downcast`s cover what
/// `panic!` and `assert_eq!` actually produce; anything else is described
/// rather than dropped, because "this task panicked with something unrecognized"
/// is still better evidence than an empty string.
pub(super) fn panic_message(payload: Box<dyn Any + Send + 'static>) -> String {
    if let Some(text) = payload.downcast_ref::<&'static str>() {
        return truncate_panic_message(text);
    }
    if let Some(text) = payload.downcast_ref::<String>() {
        return truncate_panic_message(text);
    }
    String::from("<panic payload was neither &str nor String>")
}

/// Cut `text` to [`MAX_PANIC_MESSAGE_CHARS`] characters, marking the cut.
///
/// The count is characters rather than bytes so the result is always valid
/// UTF-8, and the marker is a single character so a truncated message is never
/// mistaken for a complete one.
pub(super) fn truncate_panic_message(text: &str) -> String {
    let mut kept: String = text.chars().take(MAX_PANIC_MESSAGE_CHARS).collect();
    if kept.len() < text.len() {
        kept.push('…');
    }
    kept
}

/// Run `body` repeatedly under `budget`, stopping when the budget runs out or
/// `token` is cancelled.
///
/// Every iteration is raced against `token` rather than merely checked before
/// it, so cancellation interrupts a body that is itself awaiting: the body's
/// future is dropped and the loop returns. That is the difference between a
/// loop that stops and a loop that stops *eventually*.
///
/// # Cancellation
///
/// A cancel is observed even when the budget is still unspent, and is reported
/// as [`Outcome::Cancelled`](crate::rt::supervise::Outcome::Cancelled) rather than as exhaustion, so a caller can tell a
/// completed run from an interrupted one.
///
/// # Bounded poll
///
/// No single poll of this loop runs an unbounded number of iterations. After a
/// private interval of iterations the loop yields to the executor, so a
/// cancellable token is observably cancelled even when the body never suspends:
/// the task holding the token runs, the token is cancelled, and the loop exits
/// at its next iteration boundary. Without that, an immediately-ready body
/// holds the executor for the whole run and a cancellation ordered by another
/// task is not delivered until the budget is spent — on a current-thread
/// runtime, never.
///
/// This is the loop's own contract and it is the only part of the contract this
/// crate can enforce: a *body* that never returns from `poll` is noncooperative
/// user code, and no amount of yielding here can preempt it. See
/// [`Supervisor::shutdown`](crate::rt::supervise::Supervisor::shutdown).
pub async fn repeat<F, Fut>(token: &CancellationToken, budget: Budget, body: F) -> Outcome
where
    F: FnMut(u64) -> Fut,
    Fut: Future<Output = ()>,
{
    repeat_on(&Clock::wall(), token, budget, body).await
}

/// `repeat` with the budget measured on a declared [`Clock`].
///
/// The same loop, and the same [`Outcome`](crate::rt::supervise::Outcome), with one difference that matters:
/// `Budget::For` is compared against the clock's own timeline rather than
/// against a local monotonic instant. A wall clock is the default through
/// `repeat`, so a caller that never asks for a clock observes the behaviour it
/// always did — real elapsed time — and a caller that supplies a
/// caller-advanceable one gets a budget that is exact and costs no real wait to
/// exhaust.
///
/// The iteration bound is unaffected: a count of iterations is a fact about the
/// body, not about time, and no clock governs it.
pub async fn repeat_on<F, Fut>(
    clock: &Clock,
    token: &CancellationToken,
    budget: Budget,
    mut body: F,
) -> Outcome
where
    F: FnMut(u64) -> Fut,
    Fut: Future<Output = ()>,
{
    // The iteration bound and the time bound are read once, outside the loop, and
    // neither uses an arithmetic operator: a saturating add cannot wrap, and a
    // budget of `Duration::MAX` is a deadline that never arrives rather than one
    // that arrives immediately.
    let iteration_limit = match budget {
        Budget::Iterations(limit) => Some(limit.get()),
        Budget::For(_) | Budget::Ongoing => None,
    };
    // The instant the budget is measured from, on the governing clock.
    let started_at = clock.now();
    let deadline = match budget {
        Budget::For(limit) => Some(started_at.saturating_add(limit)),
        Budget::Iterations(_) | Budget::Ongoing => None,
    };

    let mut iterations: u64 = 0;
    // Counts down to the next cooperative yield. A countdown rather than a
    // modulo of `iterations`: `clippy::modulo_arithmetic` and
    // `clippy::integer_division` are both forbidden workspace-wide, and a
    // countdown states the bound without either.
    let mut until_yield: u64 = YIELD_INTERVAL;
    loop {
        if token.is_cancelled() {
            return Outcome::Cancelled { iterations };
        }
        // Both bounds are "already spent" predicates rather than nested
        // conditionals, so the two ways a budget can run out read as one
        // decision and the loop has a single exit for each reason.
        let spent = iteration_limit.is_some_and(|limit| iterations >= limit)
            || deadline.is_some_and(|deadline| clock.now() >= deadline);
        if spent {
            return Outcome::Exhausted { iterations };
        }
        // The body is raced against the stop *and*, on a caller-advanceable
        // clock, against the bound.
        //
        // Without the second arm a suspended body would be checked against its
        // budget only after it returned — that is, never — and a body that waits
        // indefinitely would wait for a cancel the budget was supposed to have
        // delivered. A budget consulted only between iterations bounds nothing
        // that is suspended, which is most of what this loop exists to bound.
        //
        // The bound is polled on a short real interval rather than by a timer,
        // because a caller-advanceable clock's bound is not a real time at all:
        // no engine timer can be armed for it. The interval bounds how *late*
        // an advance is noticed, never whether it is — the loop re-reads the
        // clock, so an advance that lands at any point is observed.
        //
        // A wall clock takes the other arm, governed by its own timer and
        // unchanged from what this loop always did. Racing both for a wall clock
        // would double every wakeup for no difference in the reported outcome.
        if let Some(deadline) = deadline {
            if clock.source() != TimeSource::Wall {
                match wait_for_bound(clock, token, deadline, body(iterations)).await {
                    Bounded::Spent => return Outcome::Exhausted { iterations },
                    Bounded::Stopped => return Outcome::Cancelled { iterations },
                    Bounded::Finished => iterations = iterations.saturating_add(1),
                }
            } else {
                // The timer arm. Anchored on real elapsed time from now, which
                // is the wall clock's own reading and therefore the same value
                // the deadline above carries.
                // `timeout` rather than a `select!` against a sleep: the macro
                // is not part of every feature set this crate builds under, and
                // the two settle alike — a body that finishes at the instant
                // the budget runs out is counted, and the check at the top of
                // the loop then reports the budget spent.
                let remaining = deadline.saturating_sub(clock.now());
                match crate::rt::time::timeout(
                    remaining,
                    token.run_until_cancelled(body(iterations)),
                )
                .await
                {
                    Err(_elapsed) => return Outcome::Exhausted { iterations },
                    Ok(None) => return Outcome::Cancelled { iterations },
                    Ok(Some(())) => iterations = iterations.saturating_add(1),
                }
                continue;
            }
        } else {
            match token.run_until_cancelled(body(iterations)).await {
                Some(()) => iterations = iterations.saturating_add(1),
                None => return Outcome::Cancelled { iterations },
            }
        }
        // Hand the executor back periodically so a ready body cannot hold it
        // for the whole run. `yield_now` resolves on its second poll, so this
        // costs one extra poll per interval and never blocks: outside a runtime
        // the waker is woken immediately, inside one the task is placed behind
        // the tasks already ready to run, which is how the cancellation this
        // loop checks for at the top gets a chance to be performed.
        until_yield = until_yield.saturating_sub(1);
        if until_yield == 0 {
            until_yield = YIELD_INTERVAL;
            yield_now().await;
        }
    }
}

/// How one bounded wait on a caller-advanceable clock settled.
pub(super) enum Bounded {
    /// The body returned.
    Finished,
    /// The logical clock reached `deadline`.
    Spent,
    /// The token was cancelled first.
    Stopped,
}

/// Run one body iteration under a logical deadline no engine timer can reach.
///
/// The polling interval is real time, and that is the point of the split between
/// the two clocks in this module: the *bound* is logical and exact, and the
/// *observation* of it is real and slightly late. A caller-advanceable clock
/// cannot be armed on any timer, so something has to wake up to notice — and
/// what wakes up reads the clock, so the bound itself is never approximated.
///
/// A one-millisecond poll is the observation granularity and not a budget: a
/// body that runs for an hour under a virtual clock is polled 3.6 million times,
/// which is the honest cost of asking a real executor to watch a counter nobody
/// real time is moving. A wall clock never reaches this function.
pub(super) async fn wait_for_bound<Fut>(
    clock: &Clock,
    token: &CancellationToken,
    deadline: Duration,
    body: Fut,
) -> Bounded
where
    Fut: Future<Output = ()>,
{
    /// How often the logical clock is re-read while a body is suspended.
    const LOGICAL_POLL: Duration = Duration::from_millis(1);
    let mut body = std::pin::pin!(body);
    loop {
        if clock.now() >= deadline {
            return Bounded::Spent;
        }
        if crate::rt::time::timeout(LOGICAL_POLL, &mut body)
            .await
            .is_ok()
        {
            return Bounded::Finished;
        }
        // The stop is checked on every pass rather than only when the bound is
        // met, so a cancelled loop under a virtual clock stops in a
        // millisecond rather than when its budget happens to be reached — which
        // under a clock nobody is advancing is never.
        if token.is_cancelled() {
            return Bounded::Stopped;
        }
    }
}
