//! Async timers.
//!
//! This module wraps the engine's timer constructors rather than re-exporting
//! them. tokio's `sleep`/`timeout` capture the runtime timer **at
//! construction**, so building one before entering the runtime panics with
//! "there is no reactor running" — the single most common tokio footgun. The
//! wrappers here defer construction to first poll, so `sleep(d)` may be built
//! anywhere and awaited inside a runtime.
//!
//! `interval` is the exception: it is a value, not a future, so it must still be
//! constructed inside a runtime context. Call it inside an `async` block.

use std::future::Future;

pub use lgwks_deps::tokio::time::error::Elapsed;
pub use lgwks_deps::tokio::time::{Instant, MissedTickBehavior, interval, interval_at};
pub use std::time::Duration;

/// Wait for `duration` to elapse.
///
/// The deadline is captured on first poll, not at construction, so this may be
/// created before the runtime is entered. It panics if polled outside a runtime
/// with the `time` driver, exactly as any timer must.
pub async fn sleep(duration: Duration) {
    lgwks_deps::tokio::time::sleep(duration).await;
}

/// Wait until `deadline` passes. The deadline is captured on first poll.
pub async fn sleep_until(deadline: Instant) {
    lgwks_deps::tokio::time::sleep_until(deadline).await;
}

/// Run `future`, returning [`Err(Elapsed)`](Elapsed) if `duration` passes first.
///
/// Construction is safe outside the runtime; the timer starts on first poll.
pub async fn timeout<F: Future>(duration: Duration, future: F) -> Result<F::Output, Elapsed> {
    lgwks_deps::tokio::time::timeout(duration, future).await
}

/// Run `future`, returning [`Err(Elapsed)`](Elapsed) if `deadline` passes first.
pub async fn timeout_at<F: Future>(deadline: Instant, future: F) -> Result<F::Output, Elapsed> {
    lgwks_deps::tokio::time::timeout_at(deadline, future).await
}
