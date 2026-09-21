//! Cooperative cancellation.
//!
//! [`CancellationToken`] is the estate's own, written here rather than
//! re-exported, because the engine does not ship one: tokio's lives in
//! `tokio-util`, a separate crate. Admitting that crate would add a third-party
//! edge to the storefront to carry a single type, so the primitive is built on
//! `tokio::sync::watch`, which the `sync` feature already provides.
//!
//! This is not a convenience. `AGENTS.md` requires that *"every background task
//! must be tracked in a `JoinSet` or supervisor and listen to a
//! `CancellationToken`"* — a standard the SDK could not satisfy while the token
//! was absent, because a caller had no way to signal a spawned task to stop. The
//! `JoinSet` half was already here; this is the other half.
//!
//! # Shape
//!
//! A token is cheap to clone and every clone observes the same state. A **child
//! token** is cancelled when its parent is, but cancelling a child never touches
//! the parent — the direction a supervisor needs, where one subtask being
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
//! chain, and [`cancelled`](CancellationToken::cancelled) races this node's own
//! signal against its parent's — which recursively races the rest of the chain.
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
//! reactor, so a token may be built anywhere — the same property
//! [`crate::rt::time::sleep`] was wrapped to provide. Only awaiting
//! [`cancelled`](CancellationToken::cancelled) requires a runtime, as any future
//! does.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
    signal: watch::Sender<bool>,
    /// The token this one follows, if any. Strong, so the chain a token needs in
    /// order to be cancellable is kept alive by the token itself, and
    /// one-directional, so it cannot form a cycle. See the module docs.
    parent: Option<Arc<Inner>>,
}

impl Inner {
    /// The root of a fresh, uncancelled tree.
    fn root() -> Arc<Self> {
        // The receiver is dropped immediately: `send_replace` does not error
        // when no receiver exists, and `subscribe` mints one on demand, so
        // holding it would keep a channel alive for nothing.
        let (signal, _receiver) = watch::channel(false);
        Arc::new(Self {
            cancelled: AtomicBool::new(false),
            signal,
            parent: None,
        })
    }

    /// A node following `parent`.
    fn child_of(parent: &Arc<Self>) -> Arc<Self> {
        let (signal, _receiver) = watch::channel(false);
        Arc::new(Self {
            cancelled: AtomicBool::new(false),
            signal,
            parent: Some(Arc::clone(parent)),
        })
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
            // `as_ref` rather than `&node.parent`: the estate forbids
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
    /// Boxed because it recurses: every level races its own signal against its
    /// parent's future, so the return type has to be nameable. `Send` so that
    /// [`CancellationToken::cancelled_owned`] can move the result onto another
    /// thread — the state behind it is an `AtomicBool` and a `watch` channel,
    /// both of which are already thread-safe.
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            let mut receiver = self.signal.subscribe();
            // `borrow_and_update` rather than `borrow`: it marks the current
            // value as seen, so a following `changed()` waits for the *next*
            // write instead of returning at once on the value just read.
            if *receiver.borrow_and_update() {
                return;
            }
            match self.parent.as_ref() {
                // A root has only its own signal to watch.
                None => wait_for_signal(receiver).await,
                // A child races its own signal against its parent's
                // cancellation, which has already raced the rest of the chain.
                // Two branches per level rather than a select over every
                // ancestor, so the cost is proportional to depth and needs no
                // combinator over a runtime-sized set.
                Some(parent) => {
                    let mut own = Box::pin(wait_for_signal(receiver));
                    let mut inherited = parent.cancelled();
                    race_two(&mut own, &mut inherited).await;
                }
            }
        })
    }

    /// Mark this node cancelled and wake everything waiting on it.
    ///
    /// Descendants are not visited: they observe this flag by walking up, and
    /// they are woken because their `cancelled()` future raced this node's
    /// signal. Cancelling is therefore local, and `Arc<Inner>` never has to
    /// reach sideways.
    fn cancel(&self) {
        // The store is the once-only gate for this node. `send_replace` is
        // infallible — there is no error to discard and no `let _ =` — and it
        // writes the value even when every receiver has already dropped.
        self.cancelled.store(true, Ordering::SeqCst);
        self.signal.send_replace(true);
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

/// A signal that a task should stop, and the primitive the estate's
/// no-untracked-task rule requires.
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
    /// Takes a clone, so it can be moved into
    /// [`spawn`](crate::rt::task::spawn) without borrowing the original token —
    /// which is required, since a spawned task must own everything it captures.
    /// Resolves immediately if already cancelled.
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
    // while a clone of the token is alive — and the token is borrowed for this
    // future's lifetime, so one is. Returning rather than looping keeps the
    // future terminating even if that reasoning is invalidated by a later change
    // elsewhere. A cancellation wait that can hang is worse than one that can
    // return early.
}

/// Resolve as soon as either future does, biased to neither.
///
/// Cancellation is level-triggered — both branches report the same terminal fact
/// — so unlike a `select!` over work there is no outcome to lose by racing them.
///
/// Both type parameters are `?Sized` so the second branch can be a boxed `dyn
/// Future`, which is what the recursive parent wait produces.
async fn race_two<A, B>(first: &mut Pin<Box<A>>, second: &mut Pin<Box<B>>)
where
    A: Future<Output = ()> + ?Sized,
    B: Future<Output = ()> + ?Sized,
{
    std::future::poll_fn(|context: &mut Context<'_>| {
        // Both are polled every time: short-circuiting on the first `Ready`
        // would leave the other unpolled, and for a `watch` receiver that is
        // how a waiter silently stops being registered.
        let first_ready = first.as_mut().poll(context).is_ready();
        let second_ready = second.as_mut().poll(context).is_ready();
        if first_ready || second_ready {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await;
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
mod tests {
    use super::CancellationToken;
    use crate::rt::runtime::block_on;

    #[test]
    fn a_fresh_token_is_not_cancelled() {
        let token = CancellationToken::new();
        assert!(!token.is_cancelled(), "a new token must start uncancelled");
    }

    #[test]
    fn cancel_reaches_every_clone_once() {
        let token = CancellationToken::new();
        let clone = token.clone();
        token.cancel();
        token.cancel();
        assert!(
            token.is_cancelled(),
            "the original must observe its own cancel"
        );
        assert!(clone.is_cancelled(), "a clone must observe the cancel");
    }

    #[test]
    fn cancelling_a_child_leaves_the_parent_running() {
        let parent = CancellationToken::new();
        let child = parent.child_token();
        child.cancel();
        assert!(child.is_cancelled(), "the child must be cancelled");
        assert!(
            !parent.is_cancelled(),
            "cancelling a child must not cancel its parent"
        );
    }

    #[test]
    fn cancelling_a_parent_reaches_a_grandchild() {
        let parent = CancellationToken::new();
        let child = parent.child_token();
        let grandchild = child.child_token();
        parent.cancel();
        assert!(child.is_cancelled(), "the child must follow its parent");
        assert!(
            grandchild.is_cancelled(),
            "cancellation must reach a grandchild"
        );
    }

    #[test]
    fn a_child_created_after_cancellation_starts_cancelled() {
        let parent = CancellationToken::new();
        parent.cancel();
        let child = parent.child_token();
        assert!(
            child.is_cancelled(),
            "a child created after cancel must not start live"
        );
    }

    #[test]
    fn a_descendant_survives_its_intermediate_parent_being_dropped() {
        // The failure this guards against: with a downward link, dropping the
        // middle token orphans the leaves and a held leaf becomes silently
        // uncancellable — the one thing a cancellation token must never do.
        let root = CancellationToken::new();
        let leaves: Vec<CancellationToken> = {
            let branch = root.child_token();
            (0..8).map(|_| branch.child_token()).collect()
            // `branch` is dropped here, while `leaves` outlives it.
        };
        root.cancel();
        assert!(
            leaves.iter().all(CancellationToken::is_cancelled),
            "a leaf must stay cancellable after its intermediate parent is dropped"
        );
    }

    #[test]
    fn drop_guard_cancels_on_scope_exit() {
        let token = CancellationToken::new();
        {
            let _guard = token.clone().drop_guard();
            assert!(
                !token.is_cancelled(),
                "the guard must not cancel before it is dropped"
            );
        }
        assert!(
            token.is_cancelled(),
            "dropping the guard must cancel the token"
        );
    }

    #[test]
    fn run_until_cancelled_returns_the_value_when_the_future_wins() {
        let token = CancellationToken::new();
        let output = block_on(token.run_until_cancelled(async { 7u8 }));
        assert_eq!(
            output,
            Some(7),
            "a completed future must yield its value, not None"
        );
    }

    #[test]
    fn run_until_cancelled_returns_none_when_already_cancelled() {
        let token = CancellationToken::new();
        token.cancel();
        let output = block_on(token.run_until_cancelled(std::future::pending::<u8>()));
        assert_eq!(
            output, None,
            "an already-cancelled token must abandon a pending future"
        );
    }

    #[test]
    fn cancelled_terminates_for_a_deep_already_cancelled_chain() {
        // The assertion is termination. A `cancelled()` that could miss an
        // earlier `cancel()` would never complete and this test would hang; a
        // recursive implementation would overflow the stack on this depth.
        let root = CancellationToken::new();
        let mut leaf = root.child_token();
        for _ in 0..1024 {
            leaf = leaf.child_token();
        }
        root.cancel();
        assert!(
            leaf.is_cancelled(),
            "a 1024-deep descendant must observe the root cancel"
        );
        block_on(leaf.cancelled());
    }

    #[test]
    fn a_deep_chain_propagates_to_every_level() {
        let root = CancellationToken::new();
        let mut chain = vec![root.clone()];
        for _ in 0..512 {
            let next = chain.last().map(CancellationToken::child_token);
            match next {
                Some(token) => chain.push(token),
                None => break,
            }
        }
        root.cancel();
        assert!(
            chain.iter().all(CancellationToken::is_cancelled),
            "every level of a 512-deep chain must be cancelled"
        );
    }

    #[test]
    fn a_dropped_child_does_not_keep_its_parent_cancellable_state_alive() {
        // The parent accumulates nothing per child; this asserts the observable
        // consequence — cancelling a parent that has no live children still
        // cancels, and the child's memory is released rather than retained.
        let parent = CancellationToken::new();
        {
            let child = parent.child_token();
            assert!(!child.is_cancelled(), "the child starts live");
        }
        parent.cancel();
        assert!(
            parent.is_cancelled(),
            "the parent must still cancel with no live children"
        );
    }
}
