//! Deadlines, retries and the bounds they take.

use crate::journal::frame::SaturatingFrom;
use std::future::Future;
use std::num::{NonZeroU32, NonZeroUsize};
use std::sync::Arc;
use std::time::Duration;

use crate::rt::clock::{Clock, TimeSource};
use crate::rt::time;
use crate::rt::time::Deadline;

use super::{FlowError, MAX_ATTEMPTS, MAX_BACKOFF, MAX_IN_FLIGHT, Scope, StepKey};

// ── Bounds ──────────────────────────────────────────────────────────────────

#[doc(hidden)]
/// Check an `at most N at once` bound computed at run time.
///
/// `script!` checks a literal bound at compile time; this is the same rule for
/// a bound that is an expression.
///
/// # Errors
///
/// [`FlowError::InvalidBound`] outside `1..=MAX_IN_FLIGHT`.
pub fn at_most(limit: usize) -> Result<NonZeroUsize, FlowError> {
    NonZeroUsize::new(limit)
        .filter(|bound| bound.get() <= MAX_IN_FLIGHT)
        .ok_or(FlowError::InvalidBound {
            what: "each: at most N at once",
            value: u64::saturating_from(limit),
            max: u64::saturating_from(MAX_IN_FLIGHT),
        })
}

#[doc(hidden)]
/// Check a `retry up to N times` budget computed at run time.
///
/// # Errors
///
/// [`FlowError::InvalidBound`] outside `1..=MAX_ATTEMPTS`.
pub fn attempts(count: u32) -> Result<NonZeroU32, FlowError> {
    NonZeroU32::new(count)
        .filter(|budget| budget.get() <= MAX_ATTEMPTS)
        .ok_or(FlowError::InvalidBound {
            what: "retry: up to N times",
            value: u64::from(count),
            max: u64::from(MAX_ATTEMPTS),
        })
}

// ── within ──────────────────────────────────────────────────────────────────

/// Records why a step was refused, outside the caller's frame.
///
/// The event macro expands to a callsite, a value set and a formatter; inline
/// in an `async fn` those become part of its state, and `within` is paid once
/// per nesting level, so a deep nest of steps would otherwise overflow the
/// stack the nest test budgets for. Out of line, the frame holds one call.
#[inline(never)]
fn note_refusal(site: &str, error: &FlowError) {
    lgwks_std::trace::debug!(site, ?error, "a step was refused before it ran");
}

/// The refusal a step returns once `note_refusal` has recorded it.
///
/// Always inlined so the `Err` is built in the caller's return slot, as a
/// direct `return Err(..)` builds it: a call that returned the `Result` by
/// value would add a body-sized temporary to every nested `within`.
#[inline(always)]
fn refuse_step<T>(site: &str, error: FlowError) -> Result<T, FlowError> {
    note_refusal(site, &error);
    Err(error)
}

/// Run `body`, failing with [`FlowError::TimedOut`] once `limit` passes and
/// with [`FlowError::Cancelled`] as soon as `scope` is stopped.
///
/// Either way the body is dropped, which is how a Rust future is stopped: an
/// expired step does not keep running in the background.
///
/// # The two bounds, and which clock reads which
///
/// `limit` is checked against **the scope's declared [`Clock`]**, not against a
/// local wall timer, so a scope built on a caller-advanceable clock reports the
/// same [`FlowError::TimedOut`] when the clock is advanced past `limit`
/// that a real overrun reports — with no real wait. A scope built by
/// [`Scope::root`] carries a wall clock, so the
/// default behaviour is exactly what it always was.
///
/// The body is raced against the scope's stop **and** against the wall watchdog
/// on `limit`. The watchdog is what bounds a body that has stopped making
/// progress rather than one that is waiting: a virtual clock cannot observe a
/// blocking callback that will never return, and a deadline that only a virtual
/// clock could satisfy would hang forever on it. The watchdog is real elapsed
/// time from this call, so it is unaffected by whatever a test does to the
/// logical clock — which is the contract the two are kept separate to express.
///
/// # Errors
///
/// The body's own error, `TimedOut`, or `Cancelled`.
pub async fn within<T, Fut>(
    scope: &Scope,
    step: &str,
    limit: Duration,
    body: Fut,
) -> Result<T, FlowError>
where
    Fut: Future<Output = Result<T, FlowError>>,
{
    within_on(scope.clock(), scope, step, limit, body).await
}

/// [`within`] with the logical bound measured on an explicitly named `clock`.
///
/// The same two bounds and the same two errors; the difference is only whose
/// clock reads the logical one. [`within`] reads the scope's, so this is for the
/// caller whose step is bounded by a *shorter* clock than the flow's — an
/// admission budget that is tighter than the run's, say — and for tests that
/// want to say which clock governs rather than imply it.
pub async fn within_on<T, Fut>(
    clock: &Clock,
    scope: &Scope,
    step: &str,
    limit: Duration,
    body: Fut,
) -> Result<T, FlowError>
where
    Fut: Future<Output = Result<T, FlowError>>,
{
    let deadline = Deadline::after(clock, limit);
    // The location every refusal from this step carries, computed once: both
    // arms below refuse here, and joining it per refusal would allocate the same
    // string twice on the path that fails.
    let at = scope.join(step);
    let timed_out = || FlowError::TimedOut {
        at: Arc::clone(&at),
        after: limit,
    };
    // The logical bound is not left to the engine's timer, which a
    // caller-advanceable clock cannot move. It is settled before the first poll
    // and re-checked on every wake, which is what lets an advance past `limit`
    // refuse the step with no real wait and produce the same error a real
    // overrun produces.
    if deadline.is_exhausted() {
        return refuse_step("within_on", timed_out());
    }
    // The two refusal arms and the body are boxed. A `select!` builds one future
    // per branch and holds them all for the body's whole life, so an unboxed
    // engine timer makes *every* `within` frame as large as the engine's timer
    // state. That is paid once per nesting level, and on a deeply nested tree the
    // frames alone exhaust the caller's stack. Boxing moves the arms to the heap
    // and keeps one poll frame per level, which is what the deep-nest regression
    // measures.
    //
    // A wall clock's bound is the engine timer itself, so `settle_logical_bound`
    // awaits its body directly and arms no re-read timer: only a
    // caller-advanceable clock, whose movement no timer can see, pays for the
    // logical poll.
    let logical = Box::pin(settle_logical_bound(
        &deadline,
        clock.source() == TimeSource::Wall,
        Arc::clone(&at),
        limit,
        body,
    ));
    let watchdog = Box::pin(time::sleep(limit));
    let stopped = Box::pin(scope.token().cancelled());
    lgwks_deps::tokio::select! {
        biased;
        // Read the logical bound before the engine's timer, so an advance that
        // has already spent the budget wins even if the timer arm is also ready.
        finished = logical => finished,
        // The engine's timer is the real-time watchdog for either clock: a
        // virtual clock nobody advances still cannot hold a stuck body past
        // `limit` of real time.
        () = watchdog => Err(timed_out()),
        // A stop, from the scope or anything it descends from.
        () = stopped => Err(FlowError::Cancelled { at: Arc::clone(&at) }),
    }
}

/// Settle once the deadline's logical clock has reached its budget, or once the
/// body's own outcome arrives.
///
/// Not a timer: the engine's timer cannot be moved by a caller-advanceable
/// clock, so waiting for one is how a three-day logical outage would cost three
/// real days. This future completes the moment the logical bound is met — which
/// is immediate when the bound is already spent — and otherwise waits for
/// `body`.
///
/// The poll interval bounds only how *late* an advance is noticed, never
/// whether it is: the loop re-reads the clock, so the refusal happens whenever
/// the advance lands. The body is polled every interval too, so a body that
/// completes during a poll gap is not delayed behind the next tick.
async fn settle_logical_bound<T, Fut>(
    deadline: &Deadline<'_>,
    wall: bool,
    at: Arc<str>,
    limit: Duration,
    body: Fut,
) -> Result<T, FlowError>
where
    Fut: Future<Output = Result<T, FlowError>>,
{
    /// How often a caller-advanceable clock is re-read while a body runs.
    ///
    /// The bound on how promptly an advance is noticed, nothing else. Small
    /// enough that a test's expiry assertion does not sleep noticeably; large
    /// enough that a saturated run does not spend its time polling.
    const LOGICAL_POLL: Duration = Duration::from_millis(1);
    // A wall clock is governed by the caller's engine timer, which already
    // races this future: re-reading it here would only arm a second timer per
    // step for the same answer.
    if wall {
        return body.await;
    }
    let mut body = std::pin::pin!(body);
    // Every refusal this future raises reports `at`, the step path its caller
    // computed from the same `Scope`. Passing it down is what keeps one error
    // variant from being constructed two different ways, which is how a
    // timeout's location and a cancellation's location drift apart.
    let refused = || FlowError::TimedOut {
        at: Arc::clone(&at),
        after: limit,
    };
    loop {
        if deadline.is_exhausted() {
            return refuse_step("settle_logical_bound", refused());
        }
        lgwks_deps::tokio::select! {
            biased;
            () = time::sleep(LOGICAL_POLL) => {}
            finished = &mut body => {
                // The bound is re-read *after* the body returns, which is what
                // makes a logical overrun mean the same thing as a real one. On
                // a wall clock the timer arm wins that race, because a body that
                // outran its budget is already past it when it finishes; here a
                // body may return an instant after the clock moved, and without
                // this check that body would be reported as having finished in
                // time. The check is the same fact read one instant later, so
                // the two timelines cannot disagree about an overrun.
                if deadline.is_exhausted() {
                    return Err(refused());
                }
                return finished;
            }
        }
    }
}

// ── retry ───────────────────────────────────────────────────────────────────

/// Run `body` until it succeeds, fails permanently, or spends `attempts`.
///
/// Every attempt runs in the **same** step scope, so [`Scope::key`] inside the
/// body is identical on each attempt: an effect keyed by it is one effect
/// however many times it is sent. The attempt number (from 1) is passed
/// alongside for the rare body that must know it.
///
/// The body takes its scope by value (a reference-count bump) and returns a
/// future of one nameable type, so a flow holding a `retry` is `Send` exactly
/// when its body is, nested in an [`each`](crate::script::each) or not. An
/// `async` closure that borrows its surroundings is such a body.
///
/// Between attempts the wait doubles from `backoff`, capped at
/// [`MAX_BACKOFF`], plus up to half again of deterministic jitter drawn from
/// the step key: siblings retrying the same failure spread out instead of
/// arriving together, and the same step always waits the same time, so a
/// replay is exact. The wait races cancellation.
///
/// # Errors
///
/// The first non-retryable error as-is; [`FlowError::Exhausted`] wrapping the
/// last error when `attempts` runs out; [`FlowError::Throttled`] when
/// the run's retry budget is spent: 10 retries, plus one per five first
/// attempts, shared by every `retry` under the root scope;
/// [`FlowError::Cancelled`] on a stop.
pub async fn retry<T, F, Fut>(
    scope: &Scope,
    step: &str,
    attempts: NonZeroU32,
    backoff: Duration,
    mut body: F,
) -> Result<T, FlowError>
where
    F: FnMut(Scope, u32) -> Fut,
    Fut: Future<Output = Result<T, FlowError>>,
{
    let here = scope.enter_sharing(step)?;
    here.policy().first_attempt();
    let mut attempt: u32 = 1;
    loop {
        here.checkpoint()?;
        let error = match body(here.clone(), attempt).await {
            Ok(value) => return Ok(value),
            Err(error) => error.located(&here),
        };
        if !error.is_retryable() {
            let refusal = Err(error);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "retry: returning an error to the caller");
            return refusal;
        }
        if attempt >= attempts.get() {
            let refusal = Err(FlowError::Exhausted {
                at: Arc::clone(here.shared_path()),
                attempts: attempt,
                last: Box::new(error),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "retry: returning an error to the caller");
            return refusal;
        }
        if !here.policy().take_retry() {
            let refusal = Err(FlowError::Throttled {
                at: Arc::clone(here.shared_path()),
                attempts: attempt,
                last: Box::new(error),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "retry: returning an error to the caller");
            return refusal;
        }
        // The key is hashed here, only once a retry is due: most steps never
        // retry, and those should not pay for a digest they do not use.
        let delay = backoff_delay(backoff, attempt, &here.key());
        if !delay.is_zero()
            && here
                .token()
                .run_until_cancelled(time::sleep(delay))
                .await
                .is_none()
        {
            let refusal = Err(FlowError::Cancelled {
                at: Arc::clone(here.shared_path()),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "retry: returning an error to the caller");
            return refusal;
        }
        attempt = attempt.saturating_add(1);
    }
}

/// The wait after failed attempt `attempt`: `base · 2^(attempt-1)`, capped at
/// [`MAX_BACKOFF`], plus `0..=½` of that again chosen by a byte of `key`.
fn backoff_delay(base: Duration, attempt: u32, key: &StepKey) -> Duration {
    if base.is_zero() {
        return Duration::ZERO;
    }
    // The doubling is applied the declared number of times rather than as a shift:
    // `1 << n` for a runtime `n` is either an overflow or an `Option`, and both
    // would want a value invented here. The trip count is [`MAX_BACKOFF`]'s own
    // cap, so the loop is bounded and every intermediate saturates.
    let shift = attempt.saturating_sub(1).min(16);
    let mut doubled = base;
    for _ in 0..shift {
        doubled = doubled.saturating_mul(2);
    }
    let grown = doubled.min(MAX_BACKOFF);
    // The jitter byte comes from the step key, so two siblings that failed
    // together spread out rather than retrying in lockstep, and the same step
    // always waits the same time. A key with no byte at this index contributes no
    // jitter: the key is a step path, and the index is inside its low five bits.
    let jitter_byte = key
        .as_bytes()
        .get(usize::saturating_from(attempt) & 31)
        .copied()
        .map_or(0, u32::from);
    // The halving is done in nanoseconds, which is a width wide enough that the
    // product cannot overflow and a divide by a constant is total — where
    // `Duration` has no total divide and its checked one returns an `Option` whose
    // absent case nobody here has measured. The conversion back saturates in the
    // host's own width, which no jitter this formula can produce reaches.
    let scaled = grown.as_nanos().saturating_mul(u128::from(jitter_byte));
    let jitter = Duration::from_nanos(u64::saturating_from(scaled.div_euclid(512)));
    grown.saturating_add(jitter)
}
