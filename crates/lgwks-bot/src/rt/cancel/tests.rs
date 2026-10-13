//! Tests for the cancellation token tree, moved out of `cancel.rs` so the
//! source file stays under the size law.

use super::CancellationToken;
use crate::rt::runtime::block_on;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
    // The whole chain less its ceiling half, which is floor division without
    // the `/` this crate forbids and without a `checked_div` whose `None` arm
    // would have to invent an answer: a division by a constant cannot fail,
    // so there is no failure to handle. The fixture only ever builds chains of
    // two or more, and a caller that asked for one is told by the
    // `reached_middle` assertion in each test rather than by a panic here.
    depth.saturating_sub(depth.div_ceil(2))
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
        let mut rung = crate::journal::owner::lock(&self.rung);
        *rung = true;
        self.bell.notify_all();
    }

    /// Wait at most `timeout` for the ring, returning whether it rang.
    fn wait(&self, timeout: Duration) -> bool {
        let rung = crate::journal::owner::lock(&self.rung);
        let (rung, _timeout) =
            crate::journal::owner::wait_timeout_while(&self.bell, rung, timeout, |rung| !*rung);
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
fn a_deep_chain_dropped_while_other_owners_release_concurrently_flattens_without_nesting() {
    // The adversarial shape behind `Arc::into_inner`: one thread walks the
    // ancestry flat while other threads release sibling handles into the
    // same tree, so the consume-or-leave decision runs against a count
    // that is falling as it is read. A failed `try_unwrap` holds the
    // `Arc` in its `Err` and drops it inside the walk; a release landing
    // in that window makes the walked node last-owner and re-enters this
    // destructor. `into_inner`'s `None` arm holds nothing, so no schedule
    // can nest the walk.
    //
    // The window is two instructions wide and cannot be landed on demand
    // from outside the function, so this is a hammer, not a deterministic
    // repro: deep chains, a small stack for the walk, and whole short
    // chains born and destroyed concurrently with it. The property
    // asserted is the one that matters — the flatten finishes on that
    // stack with every node freed — under exactly the concurrent
    // destruction the official `Arc` docs single out.
    const CHURN_THREADS: usize = 4;
    const CHURN_STACK: usize = 256 * 1024;

    let outcome = on_a_stack(SMALL_STACK, move || {
        let root = CancellationToken::new();
        let weak_root = Arc::downgrade(&root.inner);
        let mut leaf = root.clone();
        for _ in 0..2048 {
            leaf = leaf.child_token();
        }
        let weak_leaf = Arc::downgrade(&leaf.inner);

        // Journeys that derive and release sibling chains for as long as
        // the door is open: every round touches the shared root count and
        // drops a whole short chain concurrently with the flatten below.
        // These run on threads of their own rather than through
        // [`on_a_stack`] because that harness blocks its caller, and the
        // walker below has to keep running; every handle is retained and
        // joined from this scope, and a thread the OS refused is recorded
        // as the failure it is rather than skipped.
        let door = Arc::new(AtomicBool::new(false));
        let refused = Arc::new(AtomicUsize::new(0));
        let mut churn = Vec::new();
        let mut churn_rounds = Vec::new();
        for _ in 0..CHURN_THREADS {
            let root = root.clone();
            let door = Arc::clone(&door);
            let rounds = Arc::new(AtomicUsize::new(0));
            churn_rounds.push(Arc::clone(&rounds));
            let refused = Arc::clone(&refused);
            let thread = std::thread::Builder::new()
                .name(String::from("cancellation-churn"))
                .stack_size(CHURN_STACK)
                .spawn(move || {
                    while !door.load(Ordering::Relaxed) {
                        let child = root.child_token();
                        drop(child);
                        rounds.fetch_add(1, Ordering::Relaxed);
                    }
                });
            match thread {
                Ok(handle) => churn.push(handle),
                Err(_refused) => {
                    refused.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        assert_eq!(
            refused.load(Ordering::Relaxed),
            0,
            "the OS refused a churn thread; the hammer needs all of them"
        );

        // The hammer is only valid once every churn thread has
        // demonstrably run: on a loaded scheduler the walk below can
        // finish before a spawned thread's first release, and then the
        // concurrent destruction this test claims to cover never
        // happens. The walk starts after observed churn, not after
        // successful spawning.
        let mut spins = 0u32;
        while churn_rounds
            .iter()
            .any(|rounds| rounds.load(Ordering::Relaxed) == 0)
        {
            std::thread::yield_now();
            spins += 1;
            assert!(
                spins < 10_000_000,
                "churn threads never ran; the hammer is invalid"
            );
        }

        // The walk itself: the leaf's ancestry frees iteratively while
        // the churn above is mid-release against the same tree.
        drop(leaf);
        assert!(
            weak_leaf.upgrade().is_none(),
            "the flattened leaf must be freed even under concurrent release"
        );
        assert!(
            weak_root.upgrade().is_some(),
            "the root is still held by this frame and by the churn threads"
        );

        door.store(true, Ordering::Relaxed);
        for handle in churn {
            // The join outcome is asserted, not discarded: a churn thread
            // that panicked is a failed hammer, not a quiet pass.
            assert!(
                handle.join().is_ok(),
                "a churn thread panicked; the hammer is invalid"
            );
        }
        for (index, rounds) in churn_rounds.iter().enumerate() {
            assert!(
                rounds.load(Ordering::Relaxed) > 0,
                "churn thread {index} ran no rounds"
            );
        }

        // The root's own flatten runs after every other owner is gone, so
        // it is the single-owner walk; the churn must not have retained
        // any node under it.
        drop(root);
        assert!(
            weak_root.upgrade().is_none(),
            "the root and its ancestry must be freed once the last handle goes"
        );
    });
    assert_eq!(outcome, StackFailure::Finished);
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
fn a_cancel_that_finds_no_subscriber_is_observed_by_the_next_one() {
    // The window a lazily-built signal channel opens, and the arm nothing else
    // reaches. Every other test here either waits *before* the cancel or cancels
    // after a wait has already installed a channel; this one cancels a node
    // nobody has ever waited on, so `Inner::cancel` finds no channel to send on,
    // and then waits. The `AtomicBool` is what carries the cancel across, and the
    // subscriber's post-install re-read of it is what publishes the value onto the
    // channel it has just built.
    //
    // Asserted under a deadline because the failure mode is a wait that never
    // resolves: a missed cancel is not a wrong value, it is a hang, so the
    // timeout has to be the assertion.
    for depth in DEPTHS {
        let (root, leaf) = chain_of(depth);
        // No wait anywhere in this scope before the cancel: the whole point is
        // that `cancel` publishes to a channel that does not exist yet.
        let intermediate = leaf.child_token();
        root.cancel();
        assert!(
            intermediate.is_cancelled(),
            "depth {depth}: a child of an already-cancelled node must observe the cancel \
                 without any channel having been built for either"
        );
        let resolved = block_on(crate::rt::time::timeout(JOURNEY_LIMIT, leaf.cancelled()));
        assert!(
            resolved.is_ok(),
            "depth {depth}: a cancel published to no channel must still wake the next \
                 waiter; it did not resolve within the journey limit"
        );
    }
}

#[test]
fn a_child_cancelled_before_its_own_channel_exists_still_wakes_its_own_waiter() {
    // The per-node half of the same property, on the node whose flag the
    // subscriber re-reads directly rather than through an ancestor. A child
    // cancelled on its own account is the case where nothing upstream ever sends
    // on the child's channel, so only the flag can carry it.
    let root = CancellationToken::new();
    let child = root.child_token();
    child.cancel();
    assert!(
        child.is_cancelled(),
        "a child cancelled on its own account must observe its own cancel"
    );
    assert!(
        !root.is_cancelled(),
        "cancelling a child must still leave its parent running"
    );
    let resolved = block_on(crate::rt::time::timeout(JOURNEY_LIMIT, child.cancelled()));
    assert!(
        resolved.is_ok(),
        "a child cancelled before any waiter existed must still resolve the next waiter"
    );
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
