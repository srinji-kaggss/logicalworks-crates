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
            let mut receiver = current.signal.subscribe();
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
        // The store is the once-only gate for this node. `send_replace` is
        // infallible, there is no error to discard and no `let _ =`, and it
        // writes the value even when every receiver has already dropped.
        self.cancelled.store(true, Ordering::SeqCst);
        self.signal.send_replace(true);
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
            match Arc::try_unwrap(node) {
                // Sole owner: detach the next link and let this node drop as a
                // leaf, immediately rather than on the way out of a deep stack.
                Ok(mut inner) => next = inner.parent.take(),
                // Another handle still owns this node, so nothing below it can
                // be freed yet and the chain stays alive through that handle.
                // Dropping the `Err` here only decrements the count.
                Err(_still_shared) => return,
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
mod tests {
    use super::CancellationToken;
    use crate::rt::runtime::block_on;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Condvar, Mutex};
    use std::task::{Context, Waker};
    use std::time::Duration;

    /// Ancestry depths every deep journey runs at.
    ///
    /// Several rather than one, and none of them treated as the crash threshold:
    /// the failure depth depends on build mode and stack size, so a bound
    /// demonstrated at a single depth is a bound on one input. The deepest is the
    /// depth the issue's journey uses; the shallowest is already past the 1,024
    /// the previous test relied on.
    const DEPTHS: [usize; 3] = [1_024, 8_192, 50_000];

    /// Stack for a journey that must not use stack proportional to depth.
    ///
    /// Deliberately far below the 2 MiB a Rust test thread gets and the 8 MiB the
    /// main thread gets: stack use that scales with depth overflows here, where
    /// the budget is stated, rather than wherever the harness happens to run it.
    const SMALL_STACK: usize = 256 * 1024;

    /// Longest a watchdogged journey may run before the test calls it a hang.
    ///
    /// Generous on purpose. It is a hang detector, not a performance assertion:
    /// the journeys themselves take milliseconds, and a tight deadline would make
    /// this suite depend on the machine.
    const JOURNEY_LIMIT: Duration = Duration::from_secs(30);

    /// Build an uncancelled chain `depth` links below `root`, returning the root
    /// and the deepest token.
    ///
    /// The intermediate handles are dropped on the way, which is the property
    /// under test elsewhere: the leaf still holds every ancestor, so the chain
    /// survives them.
    fn chain_of(depth: usize) -> (CancellationToken, CancellationToken) {
        let root = CancellationToken::new();
        let mut leaf = root.clone();
        for _ in 0..depth {
            leaf = leaf.child_token();
        }
        (root, leaf)
    }

    /// The index of the middle link of a `depth`-link chain.
    ///
    /// `checked_div` rather than `/`: `clippy::integer_division` is forbidden
    /// workspace-wide. The fallback is unreachable for the depths these tests use,
    /// and if it were ever hit the `reached_middle` assertion in each caller fails
    /// rather than passing silently.
    fn middle_link(depth: usize) -> usize {
        depth.checked_div(2).unwrap_or(0)
    }

    /// Poll `future` exactly once and report whether it completed.
    ///
    /// No runtime, no waker and no waiting: the journeys that observe a pending
    /// wait, cancel, and observe it complete want the poll path alone, on a stack
    /// whose size the test chose. A no-op waker is correct here because nothing
    /// is waiting for a wake — the test polls again itself.
    fn polls_ready<F: Future + ?Sized>(future: &mut Pin<Box<F>>) -> bool {
        let mut context = Context::from_waker(Waker::noop());
        future.as_mut().poll(&mut context).is_ready()
    }

    /// A one-shot doorbell from a journey thread to the test thread.
    ///
    /// A condition variable rather than a channel: `std::sync::mpsc` is banned
    /// workspace-wide (unbounded, no async receiver) and the crate's own channel
    /// is async with no bounded blocking receive, so the wait is built here. The
    /// property this test needs is only that the wait is *bounded*: a journey that
    /// never returns has to fail the test, not hang the suite.
    #[derive(Debug)]
    struct Doorbell {
        /// Whether the journey has left the stage.
        rung: Mutex<bool>,
        /// Notified whenever `rung` is written, so a waiter wakes on the write.
        bell: Condvar,
    }

    impl Doorbell {
        /// A bell that has not been rung.
        fn new() -> Self {
            Self {
                rung: Mutex::new(false),
                bell: Condvar::new(),
            }
        }

        /// Ring it, waking every waiter.
        ///
        /// A poisoned lock is recovered rather than propagated: it is poisoned
        /// only if a previous holder panicked while holding it, which this never
        /// does, and a panic in a destructor during an unwind aborts the process.
        fn ring(&self) {
            let mut rung = self
                .rung
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            *rung = true;
            self.bell.notify_all();
        }

        /// Wait at most `timeout` for the ring, returning whether it rang.
        fn wait(&self, timeout: Duration) -> bool {
            let rung = self
                .rung
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let (rung, _timeout) = self
                .bell
                .wait_timeout_while(rung, timeout, |rung| !*rung)
                .unwrap_or_else(|poison| poison.into_inner());
            *rung
        }
    }

    /// Rings a [`Doorbell`] when dropped, on every exit path including an unwind.
    ///
    /// So a journey that panics is reported by its join rather than mistaken for
    /// one that hung.
    struct RingOnDrop {
        /// The bell to ring.
        bell: Arc<Doorbell>,
    }

    impl Drop for RingOnDrop {
        fn drop(&mut self) {
            self.bell.ring();
        }
    }

    /// Why a watchdogged journey did not report success.
    #[derive(Debug, PartialEq, Eq)]
    enum StackFailure {
        /// The journey returned. The test asserting on this is what turns a hang
        /// or a panic inside it into a failed assertion.
        Finished,
        /// The journey panicked. The string is its panic message, so an assertion
        /// that failed inside it is readable from the test that ran it.
        Panicked(String),
        /// The journey had not finished when its deadline passed. That is a hang.
        TimedOut,
        /// The OS refused the thread.
        Unspawnable(String),
    }

    /// The message out of a panic payload.
    fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
        match payload.downcast::<String>() {
            Ok(text) => *text,
            Err(payload) => match payload.downcast::<&'static str>() {
                Ok(text) => String::from(*text),
                Err(_other) => String::from("<panic payload was not a string>"),
            },
        }
    }

    /// Run `journey` on a thread with `stack_size` bytes of stack, waiting at most
    /// [`JOURNEY_LIMIT`] for it to finish.
    ///
    /// A thread of its own is the measurement: a small, stated stack is what makes
    /// "stack use does not scale with depth" checkable, and it keeps the failure
    /// where it can be observed instead of wherever the harness put the test. The
    /// deadline is the watchdog that turns a journey which never returns into a
    /// failed assertion.
    ///
    /// Stack exhaustion is the one outcome this cannot turn into a value: it
    /// aborts the process. That is why the depth-dependent paths are asserted on a
    /// small stack — the abort, not this function, is the report.
    fn on_a_stack(stack_size: usize, journey: impl FnOnce() + Send + 'static) -> StackFailure {
        let bell = Arc::new(Doorbell::new());
        let ring = Arc::clone(&bell);
        let started = std::thread::Builder::new()
            .name(String::from("cancellation-depth"))
            .stack_size(stack_size)
            .spawn(move || {
                let _ring = RingOnDrop { bell: ring };
                journey();
            });
        let handle = match started {
            Ok(handle) => handle,
            Err(error) => return StackFailure::Unspawnable(error.to_string()),
        };
        if !bell.wait(JOURNEY_LIMIT) {
            return StackFailure::TimedOut;
        }
        match handle.join() {
            Ok(()) => StackFailure::Finished,
            Err(payload) => StackFailure::Panicked(panic_message(payload)),
        }
    }

    #[test]
    fn a_fresh_wait_on_a_deep_chain_is_pending_then_resolves() {
        // The poll path, with no runtime: a fresh wait on a deep chain is pending,
        // and the *root's* cancel resolves it. Both are stack-bounded only if the
        // wait holds one subscription per ancestor in a flat list; the recursive
        // shape uses one nested poll per link.
        for depth in DEPTHS {
            let outcome = on_a_stack(SMALL_STACK, move || {
                let (root, leaf) = chain_of(depth);
                let mut wait = Box::pin(leaf.cancelled());
                assert!(
                    !polls_ready(&mut wait),
                    "a fresh wait on an uncancelled {depth}-deep chain must be pending"
                );
                root.cancel();
                assert!(
                    polls_ready(&mut wait),
                    "the wait must resolve once the root of a {depth}-deep chain is cancelled"
                );
                assert!(
                    leaf.is_cancelled(),
                    "the leaf of a {depth}-deep chain must observe the root cancel"
                );
            });
            assert_eq!(outcome, StackFailure::Finished, "depth {depth}");
        }
    }

    #[test]
    fn cancelling_an_intermediate_node_reaches_a_deep_leaf() {
        // Cancellation is observed from *any* ancestor, not only the root, and the
        // ancestor that cancels is the one whose signal has to wake the wait.
        for depth in DEPTHS {
            let outcome = on_a_stack(SMALL_STACK, move || {
                let root = CancellationToken::new();
                let mut leaf = root.clone();
                let mut middle = root.clone();
                let mut reached_middle = false;
                for index in 0..depth {
                    leaf = leaf.child_token();
                    if index == middle_link(depth) {
                        middle = leaf.clone();
                        reached_middle = true;
                    }
                }
                assert!(
                    reached_middle,
                    "a {depth}-deep chain contains a middle node"
                );
                middle.cancel();
                assert!(
                    leaf.is_cancelled(),
                    "a cancel at the middle of a {depth}-deep chain must reach the leaf"
                );
                assert!(
                    !root.is_cancelled(),
                    "cancelling a descendant must not cancel the root"
                );
                let mut wait = Box::pin(leaf.cancelled());
                assert!(
                    polls_ready(&mut wait),
                    "a wait on a chain already cancelled mid-way must be ready on its first poll"
                );
            });
            assert_eq!(outcome, StackFailure::Finished, "depth {depth}");
        }
    }

    #[test]
    fn dropping_a_pending_deep_wait_is_stack_bounded() {
        // The other depth-proportional stack path the issue names: a pending
        // cancellation future holds one boxed future per ancestor in the recursive
        // shape, so dropping it walks that nesting.
        for depth in DEPTHS {
            let outcome = on_a_stack(SMALL_STACK, move || {
                let (root, leaf) = chain_of(depth);
                let mut wait = Box::pin(leaf.cancelled());
                assert!(
                    !polls_ready(&mut wait),
                    "a fresh wait must be pending before it is dropped"
                );
                drop(wait);
                assert!(
                    !root.is_cancelled(),
                    "dropping a pending wait must not cancel the chain"
                );
            });
            assert_eq!(outcome, StackFailure::Finished, "depth {depth}");
        }
    }

    #[test]
    fn destroying_a_deep_chain_is_stack_bounded_and_leaks_nothing() {
        // `drop(leaf)` with no runtime, no wait and nothing cancelled: the last
        // owner of a chain frees every ancestor, and a derived drop for the parent
        // link does it on a stack proportional to depth.
        for depth in DEPTHS {
            let outcome = on_a_stack(SMALL_STACK, move || {
                let root = CancellationToken::new();
                let weak_root = Arc::downgrade(&root.inner);
                let mut leaf = root.clone();
                let mut weak_middle = Arc::downgrade(&root.inner);
                let mut reached_middle = false;
                for index in 0..depth {
                    leaf = leaf.child_token();
                    if index == middle_link(depth) {
                        weak_middle = Arc::downgrade(&leaf.inner);
                        reached_middle = true;
                    }
                }
                assert!(
                    reached_middle,
                    "a {depth}-deep chain contains a middle node"
                );
                let weak_leaf = Arc::downgrade(&leaf.inner);
                assert!(
                    weak_root.upgrade().is_some(),
                    "the root must be alive while a handle to it is held"
                );

                drop(leaf);
                assert!(
                    weak_leaf.upgrade().is_none(),
                    "the leaf must be freed when its last handle goes"
                );
                assert!(
                    weak_middle.upgrade().is_none(),
                    "freeing the leaf of a {depth}-deep chain must free its ancestry, not retain it"
                );
                assert!(
                    weak_root.upgrade().is_some(),
                    "the root is still held, so the chain must not have been freed from under it"
                );

                drop(root);
                assert!(
                    weak_root.upgrade().is_none(),
                    "the root must be freed once its last handle goes"
                );
            });
            assert_eq!(outcome, StackFailure::Finished, "depth {depth}");
        }
    }

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
        // uncancellable, the one thing a cancellation token must never do.
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
        // Cancelled *before* the wait exists, so the wait takes its
        // already-cancelled path at the deepest chain in `DEPTHS`.
        //
        // The assertion is termination, and it is made under a deadline: a
        // `cancelled()` that missed an earlier `cancel()` would never complete,
        // and the timeout turns that into a failed assertion rather than a hung
        // suite. (The previous form of this test ran at depth 1,024 and claimed a
        // recursive implementation would overflow there. It would not, and either
        // way one depth that happens to fit establishes nothing about bounded
        // stack use; the small-stack journeys above are what establish that.)
        for depth in DEPTHS {
            let outcome = on_a_stack(SMALL_STACK, move || {
                let (root, leaf) = chain_of(depth);
                root.cancel();
                assert!(
                    leaf.is_cancelled(),
                    "a {depth}-deep descendant must observe the root cancel"
                );
                let resolved = block_on(crate::rt::time::timeout(JOURNEY_LIMIT, leaf.cancelled()));
                assert!(
                    resolved.is_ok(),
                    "waiting on an already-cancelled {depth}-deep chain must resolve"
                );
            });
            assert_eq!(outcome, StackFailure::Finished, "depth {depth}");
        }
    }

    #[test]
    fn a_deep_chain_propagates_to_every_level() {
        // Exercises the iterative `is_cancelled` walk — which is the path this
        // test was always about — at every level of the chain.
        //
        // Moderate depth by design: checking *every* level at depth `d` walks
        // `d` links per level, so this is quadratic, and 50,000 levels would be
        // billions of pointer hops. Bounded stack *use* is asserted on the deep
        // chains above; this one asserts coverage.
        const EVERY_LEVEL: usize = 2_048;
        let root = CancellationToken::new();
        let mut chain = vec![root.clone()];
        for _ in 0..EVERY_LEVEL {
            let next = chain.last().map(CancellationToken::child_token);
            match next {
                Some(token) => chain.push(token),
                None => break,
            }
        }
        root.cancel();
        assert!(
            chain.iter().all(CancellationToken::is_cancelled),
            "every level of a {EVERY_LEVEL}-deep chain must be cancelled"
        );
    }

    #[test]
    fn dropping_a_deep_intermediate_handle_keeps_the_leaf_cancellable() {
        // Dropping a handle in the middle of a chain must free nothing the leaf
        // still needs: the parent link is held by the child, so the ancestry stays
        // alive and the leaf stays cancellable.
        for depth in DEPTHS {
            let outcome = on_a_stack(SMALL_STACK, move || {
                let root = CancellationToken::new();
                let mut leaf = root.clone();
                let mut weak_middle = Arc::downgrade(&root.inner);
                let mut reached_middle = false;
                for index in 0..depth {
                    let next = leaf.child_token();
                    // The old handle is dropped here, while the new one holds it
                    // as its parent.
                    leaf = next;
                    if index == middle_link(depth) {
                        weak_middle = Arc::downgrade(&leaf.inner);
                        reached_middle = true;
                    }
                }
                assert!(
                    reached_middle,
                    "a {depth}-deep chain contains a middle node"
                );
                assert!(
                    weak_middle.upgrade().is_some(),
                    "the middle node must be held by its descendant after its own handle goes"
                );
                root.cancel();
                assert!(
                    leaf.is_cancelled(),
                    "the leaf of a {depth}-deep chain must stay cancellable"
                );
            });
            assert_eq!(outcome, StackFailure::Finished, "depth {depth}");
        }
    }

    #[test]
    fn a_dropped_child_does_not_keep_its_parent_cancellable_state_alive() {
        // The parent accumulates nothing per child; this asserts the observable
        // consequence: cancelling a parent that has no live children still
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
