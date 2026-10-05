//! A future that yields once and then resolves, without a reactor.
//!
//! This is the shape a runtime-independent future has: it makes progress by
//! waking its own waker, so any executor that re-polls on a wake completes it —
//! including the thread-parking one behind the synchronous adapter, and the only
//! executor a build without `rt` has. It needs no timer, socket or driver, which
//! is exactly the property that makes it a fair subject for an adapter that has
//! none.
//!
//! Included by path: `#[path = "support/yield_once.rs"] mod yield_once;` from a
//! target at the crate's `tests/` root, `#[path = "../support/yield_once.rs"]`
//! from a module of `tests/it/`.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

/// A future that returns `Pending` once, waking itself, and then `value`.
pub(crate) struct YieldOnce {
    /// Whether this future has returned `Pending` once already.
    yielded: bool,
    /// The value it resolves to.
    value: u32,
}

impl YieldOnce {
    /// A future that yields once and then resolves to `value`.
    pub(crate) fn new(value: u32) -> Self {
        Self {
            yielded: false,
            value,
        }
    }
}

impl Future for YieldOnce {
    type Output = u32;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<u32> {
        let this = self.get_mut();
        if this.yielded {
            return Poll::Ready(this.value);
        }
        this.yielded = true;
        context.waker().wake_by_ref();
        Poll::Pending
    }
}
