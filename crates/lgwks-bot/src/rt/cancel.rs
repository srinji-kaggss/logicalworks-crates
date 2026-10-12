//! Cooperative cancellation.
//!
//! [`CancellationToken`] is written here rather than re-exported, because the
//! engine does not ship one: tokio's lives in `tokio-util`, a separate crate.
//! Admitting that crate would add a third-party edge to the storefront to carry
//! a single type, so the primitive is built on `tokio::sync::watch`, which the
//! `sync` feature already provides.
//!
//! This is not a convenience. Every background task must be tracked in a
//! `JoinSet` or supervisor and must listen to a `CancellationToken`, a standard
//! the SDK could not satisfy while the token was absent, because a caller had no
//! way to signal a spawned task to stop. The `JoinSet` half was already here;
//! this is the other half.
//!
//! # Shape
//!
//! A token is cheap to clone and every clone observes the same state. A **child
//! token** is cancelled when its parent is, but cancelling a child never touches
//! the parent, the direction a supervisor needs, where one subtask being
//! abandoned must not tear down its siblings.
//!
//! # Why the link points up
//!
//! A child holds a **strong** reference to its parent, and a parent holds
//! *nothing* pointing down. Two properties follow, and both are load-bearing:
//!
//! - **A token you hold can always be cancelled.** If the tree linked downward,
//!   dropping an intermediate token would orphan its descendants: a leaf held by
//!   a spawned task would silently become uncancellable, which is the one failure
//!   a cancellation primitive must not have. Pointing up means the chain a token
//!   needs is kept alive by the token itself.
//! - **Nothing leaks.** A long-lived root that creates transient children
//!   accumulates no references to them, so there is no child list to prune and no
//!   unbounded structure to bound. Dropping the last handle to a subtree frees
//!   that subtree.
//!
//! There is no cycle: the edge is one-directional, so the parent cannot be kept
//! alive by a child it keeps alive.
//!
//! Because the parent holds no child list, cancellation is **pulled, not
//! pushed**: [`is_cancelled`](CancellationToken::is_cancelled) walks up the
//! chain, and [`cancelled`](CancellationToken::cancelled) waits on every node's
//! signal from the token up to the root at once.
//!
//! # Depth is a heap cost, not a stack cost
//!
//! Ancestry depth is caller-shaped — `child_token` in a loop makes a chain as
//! deep as the loop runs — so nothing here may use stack proportional to it.
//! Three paths would, and each is written iteratively for that reason:
//!
//! - **[`is_cancelled`](CancellationToken::is_cancelled)** walks the parent links
//!   in a loop.
//! - **[`cancelled`](CancellationToken::cancelled)** builds one subscription per
//!   node in a loop and polls them from a flat list. The obvious recursive shape
//!   — each level racing its own signal against its parent's future — boxes the
//!   *type*, which bounds the size of the future's value, but not the *call
//!   stack* used to poll it: a 50,000-deep chain would use 50,000 nested polls.
//! - **Dropping the last handle to a chain** frees ancestors iteratively. A
//!   derived drop for `parent: Option<Arc<Inner>>` recurses once per link, which
//!   is the same stack exhaustion reached from a path that need not poll at all:
//!   `drop(leaf)` on a 50,000-deep chain.
//!
//! Depth is therefore bounded by memory, not stack, and no constructor imposes a
//! limit: a limit would be an arbitrary number that a caller can still exceed
//! through repeated `child_token` calls, whereas a flat representation is correct
//! at every depth.
//!
//! # Why `watch` and not `Notify`
//!
//! `Notify::notify_waiters` wakes only the waiters registered *at that instant*,
//! so a `cancelled()` future created a microsecond after `cancel()` would miss
//! the wake and hang forever. `watch` carries the state itself: a receiver
//! subscribed before or after the cancel reads the current value, and `changed()`
//! fires on any later write. There is no window to lose.
//!
//! # No runtime required
//!
//! Construction, [`cancel`](CancellationToken::cancel),
//! [`child_token`](CancellationToken::child_token), and
//! [`is_cancelled`](CancellationToken::is_cancelled) are synchronous and need no
//! reactor, so a token may be built anywhere: the same property
//! [`crate::rt::time::sleep`] was wrapped to provide. Only awaiting
//! [`cancelled`](CancellationToken::cancelled) requires a runtime, as any future
//! does.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};

use lgwks_deps::tokio::sync::watch;

/// Shared state behind one token.
///
/// Every clone of a [`CancellationToken`] holds an `Arc` to the same `Inner`, so
/// they share one cancellation state.
struct Inner {
    /// Set once, read by every `is_cancelled`. The authoritative flag; the watch
    /// channel mirrors it so waiters can be woken without a lock.
    cancelled: AtomicBool,
    /// Carries the same `bool` as a *value* rather than an event, which is what
    /// makes a late subscriber correct rather than merely lucky. Sending never
    /// fails, even with no receiver alive.
    ///
    /// **Built on the first subscriber rather than in the constructor.** The
    /// flag above is the authority and a `watch` channel exists only to *wake*
    /// waiters, so a node nobody ever waits on needs no channel. This is the whole
    /// per-task cost of `child_token`: measured on an Apple M5 Pro, building the
    /// channel cost 10 allocations against the 1 the node's own `Arc` costs, so
    /// `Supervisor::spawn` was paying eleven heap allocations per task for a
    /// channel with no reader. Building it lazily takes a child token from
    /// 11 allocations to 1 without changing what any of the three paths above
    /// observes — the subscriber re-reads the flag after installing the channel,
    /// which is what closes the window a lazy channel opens.
    signal: OnceLock<watch::Sender<bool>>,
    /// The token this one follows, if any. Strong, so the chain a token needs in
    /// order to be cancellable is kept alive by the token itself, and
    /// one-directional, so it cannot form a cycle. See the module docs.
    parent: Option<Arc<Inner>>,
}

impl Inner {
    /// The root of a fresh, uncancelled tree.
    fn root() -> Arc<Self> {
        Arc::new(Self {
            cancelled: AtomicBool::new(false),
            signal: OnceLock::new(),
            parent: None,
        })
    }

    /// A node following `parent`.
    fn child_of(parent: &Arc<Self>) -> Arc<Self> {
        Arc::new(Self {
            cancelled: AtomicBool::new(false),
            signal: OnceLock::new(),
            parent: Some(Arc::clone(parent)),
        })
    }

    /// This node's signal channel, built on first use, with the node's current
    /// cancellation state already published on it.
    ///
    /// The flag is re-read **after** the channel is installed rather than before,
    /// and that ordering is what makes a lazy channel correct. The three orders:
    ///
    /// - **Cancelled before the channel existed.** The store in `cancel` has
    ///   already happened, so this load reads `true` and publishes it here; the
    ///   cancel that set it found no channel to send on and did nothing else.
    /// - **Cancelled while the channel is being installed.** Either this load
    ///   reads `true` and publishes it, or `cancel`'s lookup finds the channel
    ///   this call just installed and sends on it.
    /// - **Cancelled after this call returns.** `cancel` finds the installed
    ///   channel and sends, and the receiver below is subscribed *before* it reads
    ///   the value, so the send either is the value it reads or wakes its
    ///   `changed` wait.
    ///
    /// A build that read the flag first would have a window in which a cancel
    /// between the read and the install published to nothing.
    fn subscribe(&self) -> watch::Receiver<bool> {
        let signal = self.signal.get_or_init(|| watch::channel(false).0);
        if self.cancelled.load(Ordering::SeqCst) {
            // Idempotent: `send_replace` on an already-`true` channel is a write
            // of the same value, and it is infallible with no receiver alive.
            signal.send_replace(true);
        }
        signal.subscribe()
    }

    /// Whether this node or any ancestor has been cancelled.
    ///
    /// Iterative: a chain is user-shaped, and a deep one would otherwise recurse
    /// once per link on a path that must not fail.
    fn is_cancelled(&self) -> bool {
        let mut node = self;
        loop {
            if node.cancelled.load(Ordering::SeqCst) {
                return true;
            }
            // `as_ref` rather than `&node.parent`: this crate forbids
            // `clippy::pattern_type_mismatch`, which refuses a `Some(_)` pattern
            // matched against an `&Option<_>`.
            match node.parent.as_ref() {
                Some(parent) => node = parent.as_ref(),
                None => return false,
            }
        }
    }

    /// Resolve when this node or any ancestor is cancelled.
    ///
    /// Iterative in construction, polling and destruction. The chain is walked
    /// once into one subscription per node, each owned by its own boxed future,
    /// and a single `poll_fn` polls that flat list. A recursive form would box
    /// the *type* while still using stack proportional to depth when polling it,
    /// and ancestry depth is caller-shaped: `child_token` in a loop.
    ///
    /// `Send` so that [`CancellationToken::cancelled_owned`] can move the result
    /// onto another thread: the state behind it is an `AtomicBool` and a `watch`
    /// channel, both of which are already thread-safe.
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        let mut waiters: Vec<Pin<Box<dyn Future<Output = ()> + Send + '_>>> = Vec::new();
        let mut node = Some(self);
        while let Some(current) = node {
            let mut receiver = current.subscribe();
            // `borrow_and_update` rather than `borrow`: it marks the current
            // value as seen, so a following `changed` waits for the *next*
            // write instead of returning at once on the value just read.
            //
            // A node that is already cancelled resolves the whole wait here. The
            // check happens while the chain is being walked, so the cost of an
            // already-cancelled chain is the walk, not a poll.
            if *receiver.borrow_and_update() {
                return Box::pin(std::future::ready(()));
            }
            // Each future owns its receiver, so the `changed` registration lives
            // in the future's state and survives across polls. Re-making the
            // future on every poll would drop the registration each time and
            // lose a wakeup that lands between polls.
            waiters.push(Box::pin(async move { wait_for_signal(receiver).await }));
            node = current.parent.as_deref();
        }
        Box::pin(std::future::poll_fn(move |context: &mut Context<'_>| {
            // Every waiter is polled every time: short-circuiting on the first
            // `Ready` would leave the rest unpolled, and for a `watch` receiver
            // that is how a waiter silently stops being registered.
            let mut ready = false;
            for waiter in &mut waiters {
                if waiter.as_mut().poll(context).is_ready() {
                    ready = true;
                }
            }
            if ready {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }))
    }

    /// Mark this node cancelled and wake everything waiting on it.
    ///
    /// Descendants are not visited: they observe this flag by walking up, and
    /// they are woken because their `cancelled()` future raced this node's
    /// signal. Cancelling is therefore local, and `Arc<Inner>` never has to
    /// reach sideways.
    fn cancel(&self) {
        // The store is the once-only gate for this node, and it comes **first**:
        // a node with no channel yet has nobody to wake, and the flag is what a
        // later subscriber reads (see `Inner::subscribe`), so publishing to an
        // absent channel loses nothing. `send_replace` is infallible, there is no
        // error to discard and no `let _ =`, and it writes the value even when
        // every receiver has already dropped.
        self.cancelled.store(true, Ordering::SeqCst);
        if let Some(signal) = self.signal.get() {
            signal.send_replace(true);
        }
    }
}

impl Drop for Inner {
    /// Free the ancestry iteratively.
    ///
    /// A derived drop for `parent: Option<Arc<Inner>>` recurses once per link
    /// when the field's owner is the last one, so freeing the last handle to the
    /// leaf of a chain built by a loop uses stack proportional to that loop's
    /// length — reached without polling, awaiting, or a runtime, by a plain
    /// `drop(leaf)`.
    ///
    /// Taking each link out before dropping the node that holds it flattens
    /// that: every node is dropped with its own link already detached, so no
    /// node's drop reaches another's.
    fn drop(&mut self) {
        let mut next = self.parent.take();
        while let Some(node) = next {
            match Arc::into_inner(node) {
                // Sole owner: detach the next link and let this node drop as a
                // leaf, immediately rather than on the way out of a deep stack.
                Some(mut inner) => next = inner.parent.take(),
                // Another handle still owns this node, so nothing below it can
                // be freed yet and the chain stays alive through that handle.
                //
                // `into_inner` rather than `try_unwrap` is load-bearing here.
                // A failed `try_unwrap` hands back the `Arc` in its `Err`, and
                // that returned handle drops inside this frame; if the other
                // owner releases between the failed unwrap and that drop, the
                // returned handle *is* the last one, and this destructor
                // re-enters on another thread's stack despite looking flat.
                // `into_inner` makes the consume-or-leave decision atomically
                // and on `None` holds no handle at all, so this loop can never
                // drop an `Arc` it did not fully own going in.
                None => return,
            }
        }
    }
}

impl fmt::Debug for Inner {
    /// Reports this node's state without following the parent chain.
    ///
    /// A derived `Debug` would recurse through `parent`, so formatting a leaf of
    /// a deep tree would walk and print the whole chain. Neither the walk nor the
    /// output is useful here; whether a parent exists is.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Inner")
            .field("cancelled", &self.cancelled.load(Ordering::SeqCst))
            .field("has_parent", &self.parent.is_some())
            .finish_non_exhaustive()
    }
}

/// A signal that a task should stop. Spawned work is expected to hold one of
/// these so that it can be told to stop rather than being left running.
///
/// Cloning is cheap and shares state: every clone is cancelled together. Use
/// [`child_token`](Self::child_token) when a subtask must be cancellable without
/// being granted the power to cancel its parent.
///
/// # Example
///
/// ```
/// use lgwks_bot::rt::sync::CancellationToken;
///
/// let token = CancellationToken::new();
/// let worker = token.clone();
///
/// // The worker observes the cancel; the supervisor decides when.
/// assert!(!worker.is_cancelled());
/// token.cancel();
/// assert!(worker.is_cancelled());
/// ```
#[derive(Clone, Debug)]
pub struct CancellationToken {
    /// Shared with every clone and child; the token's entire state.
    inner: Arc<Inner>,
}

impl CancellationToken {
    /// A token that is not yet cancelled.
    ///
    /// Needs no runtime, so it may be created before one exists.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Inner::root(),
        }
    }

    /// Cancel this token and every token derived from it.
    ///
    /// Idempotent and synchronous: a second call does nothing, and every clone
    /// and descendant observes the change immediately. Tasks waiting on
    /// [`cancelled`](Self::cancelled) are woken.
    ///
    /// Cancelling a **clone** cancels the whole tree it belongs to, because a
    /// clone is the same token. Cancelling a **child** affects only that child's
    /// subtree.
    pub fn cancel(&self) {
        self.inner.cancel();
    }

    /// Whether this token, or any token it was derived from, has been cancelled.
    ///
    /// Returns `true` as soon as any of them is cancelled, from any thread,
    /// through any handle.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    /// Resolve when this token is cancelled.
    ///
    /// Resolves immediately if it already is, so it is safe to await in a loop:
    /// "already cancelled" and "cancelled while waiting" are the same observable
    /// outcome, and there is no window between them that can hang.
    pub async fn cancelled(&self) {
        self.inner.cancelled().await;
    }

    /// A `'static` future that resolves when this token is cancelled.
    ///
    /// Takes a clone, so it can be moved into a body that must own everything
    /// it captures — the closure [`Supervisor::spawn`] takes, or a task placed
    /// on a [`JoinSet`](crate::rt::task::JoinSet) — without borrowing the
    /// original token. Resolves immediately if already cancelled.
    ///
    /// [`Supervisor::spawn`]: crate::rt::supervise::Supervisor::spawn
    //
    // No `#[must_use]`: the return type is `impl Future`, which is already
    // `#[must_use]`, so an attribute here would be redundant and would earn
    // `clippy::double_must_use`.
    pub fn cancelled_owned(&self) -> impl Future<Output = ()> + Send + 'static {
        let token = self.clone();
        async move { token.cancelled().await }
    }

    /// A token cancelled when this one is, which cannot cancel this one.
    ///
    /// Cancelling the child leaves the parent running; cancelling the parent
    /// cancels the child. A child created after cancellation starts cancelled.
    ///
    /// The child holds the parent, not the reverse, so the parent accumulates no
    /// state per child and a dropped subtree is freed immediately.
    #[must_use]
    pub fn child_token(&self) -> Self {
        Self {
            inner: Inner::child_of(&self.inner),
        }
    }

    /// A guard that cancels this token when dropped.
    ///
    /// The RAII form of cancellation: a scope owning the guard cancels its tasks
    /// on every exit path, including an early return or an unwind, so cleanup
    /// cannot be skipped by forgetting a call.
    #[must_use]
    pub fn drop_guard(self) -> DropGuard {
        DropGuard { token: self }
    }

    /// Run `future`, returning `None` if the token is cancelled first.
    ///
    /// The future is polled before cancellation is checked, so a future that is
    /// already complete yields its value even if the token was cancelled in the
    /// same instant. Dropping the returned future cancels nothing: the inner
    /// future is dropped exactly as dropping it directly would.
    pub async fn run_until_cancelled<F: Future>(&self, future: F) -> Option<F::Output> {
        // Boxed rather than `tokio::pin!` so this does not depend on the
        // `macros` feature; a cancellation wrapper is not a hot path, and one
        // allocation per call buys an unconditional API.
        let mut inner = Box::pin(future);
        let mut cancelled = Box::pin(self.cancelled());
        std::future::poll_fn(move |context: &mut Context<'_>| {
            if let Poll::Ready(output) = inner.as_mut().poll(context) {
                return Poll::Ready(Some(output));
            }
            if cancelled.as_mut().poll(context).is_ready() {
                return Poll::Ready(None);
            }
            Poll::Pending
        })
        .await
    }
}

impl Default for CancellationToken {
    /// Equivalent to [`CancellationToken::new`].
    fn default() -> Self {
        Self::new()
    }
}

/// Resolve when a subscribed signal reports `true`.
///
/// Split out of [`CancellationToken::cancelled`] so the root path and the child
/// path share one implementation of the wait, and so the child path can box it.
async fn wait_for_signal(mut receiver: watch::Receiver<bool>) {
    while receiver.changed().await.is_ok() {
        if *receiver.borrow_and_update() {
            return;
        }
    }
    // Exiting on `Err` is deliberate: the sender is gone, which cannot happen
    // while a clone of the token is alive, and the token is borrowed for this
    // future's lifetime, so one is. Returning rather than looping keeps the
    // future terminating even if that reasoning is invalidated by a later change
    // elsewhere. A cancellation wait that can hang is worse than one that can
    // return early.
}

/// Cancels its [`CancellationToken`] when dropped. Obtained from
/// [`CancellationToken::drop_guard`].
#[derive(Debug)]
pub struct DropGuard {
    /// The token this guard cancels on drop. Moved in, so the guard is the only
    /// owner and the cancel cannot already have been performed elsewhere.
    token: CancellationToken,
}

impl Drop for DropGuard {
    /// Cancel the held token. Never panics: `cancel` is infallible, and a panic
    /// in a destructor during an unwind would abort the process.
    fn drop(&mut self) {
        self.token.cancel();
    }
}

#[cfg(test)]
mod tests;
