//! Deadlines, retries and the bounds they take.

use std::future::Future;
use std::num::{NonZeroU32, NonZeroUsize};
use std::sync::Arc;
use std::time::Duration;

use crate::rt::time;

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
/// [`FlowError::InvalidBound`](crate::script::FlowError::InvalidBound) outside `1..=MAX_IN_FLIGHT`.
pub fn at_most(limit: usize) -> Result<NonZeroUsize, FlowError> {
    NonZeroUsize::new(limit)
        .filter(|bound| bound.get() <= MAX_IN_FLIGHT)
        .ok_or(FlowError::InvalidBound {
            what: "each: at most N at once",
            value: u64::try_from(limit).unwrap_or(u64::MAX),
            max: u64::try_from(MAX_IN_FLIGHT).unwrap_or(u64::MAX),
        })
}

#[doc(hidden)]
/// Check a `retry up to N times` budget computed at run time.
///
/// # Errors
///
/// [`FlowError::InvalidBound`](crate::script::FlowError::InvalidBound) outside `1..=MAX_ATTEMPTS`.
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

/// Run `body`, failing with [`FlowError::TimedOut`](crate::script::FlowError::TimedOut) once `limit` passes and
/// with [`FlowError::Cancelled`](crate::script::FlowError::Cancelled) as soon as `scope` is stopped.
///
/// Either way the body is dropped, which is how a Rust future is stopped: an
/// expired step does not keep running in the background.
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
    match scope
        .token()
        .run_until_cancelled(time::timeout(limit, body))
        .await
    {
        Some(Ok(result)) => result,
        Some(Err(_elapsed)) => Err(FlowError::TimedOut {
            at: scope.join(step),
            after: limit,
        }),
        None => Err(FlowError::Cancelled {
            at: scope.join(step),
        }),
    }
}

// ── retry ───────────────────────────────────────────────────────────────────

/// Run `body` until it succeeds, fails permanently, or spends `attempts`.
///
/// Every attempt runs in the **same** step scope, so [`Scope::key`](crate::script::Scope::key) inside the
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
/// [`MAX_BACKOFF`](crate::script::MAX_BACKOFF), plus up to half again of deterministic jitter drawn from
/// the step key: siblings retrying the same failure spread out instead of
/// arriving together, and the same step always waits the same time, so a
/// replay is exact. The wait races cancellation.
///
/// # Errors
///
/// The first non-retryable error as-is; [`FlowError::Exhausted`](crate::script::FlowError::Exhausted) wrapping the
/// last error when `attempts` runs out; [`FlowError::Throttled`](crate::script::FlowError::Throttled) when
/// the run's retry budget is spent: 10 retries, plus one per five first
/// attempts, shared by every `retry` under the root scope;
/// [`FlowError::Cancelled`](crate::script::FlowError::Cancelled) on a stop.
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
    lgwks_std::trace::warn!(
        operation = "retry",
        "operation refused its request; the typed error carries the facts"
    );
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
            return Err(error);
        }
        if attempt >= attempts.get() {
            return Err(FlowError::Exhausted {
                at: Arc::clone(here.shared_path()),
                attempts: attempt,
                last: Box::new(error),
            });
        }
        if !here.policy().take_retry() {
            return Err(FlowError::Throttled {
                at: Arc::clone(here.shared_path()),
                attempts: attempt,
                last: Box::new(error),
            });
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
            return Err(FlowError::Cancelled {
                at: Arc::clone(here.shared_path()),
            });
        }
        attempt = attempt.saturating_add(1);
    }
}

/// The wait after failed attempt `attempt`: `base · 2^(attempt-1)`, capped at
/// [`MAX_BACKOFF`](crate::script::MAX_BACKOFF), plus `0..=½` of that again chosen by a byte of `key`.
fn backoff_delay(base: Duration, attempt: u32, key: &StepKey) -> Duration {
    if base.is_zero() {
        return Duration::ZERO;
    }
    let shift = attempt.saturating_sub(1).min(16);
    let factor = 1_u32.checked_shl(shift).unwrap_or(u32::MAX);
    let grown = base.saturating_mul(factor).min(MAX_BACKOFF);
    let byte = usize::try_from(attempt & 31)
        .ok()
        .and_then(|index| key.as_bytes().get(index).copied())
        .unwrap_or(0);
    let jitter = grown
        .saturating_mul(u32::from(byte))
        .checked_div(512)
        .unwrap_or(Duration::ZERO);
    grown.saturating_add(jitter)
}
