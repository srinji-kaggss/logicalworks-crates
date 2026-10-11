//! Driving futures on the calling thread: [`block_on`] for one, [`join_all`]
//! for many, with no reactor and no worker to schedule onto.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, Wake, Waker};

#[cfg(doc)]
use super::spawn_blocking;
use super::{lock, wait};

// ── block_on ───────────────────────────────────────────────────────────────

/// The waker [`block_on`] hands to the future it drives.
///
/// It is the whole of the runtime: waking sets the flag and signals the
/// condition the driving thread waits on, so there is no reactor and no worker
/// to schedule onto.
#[derive(Default)]
struct ThreadWaker {
    /// Set by `wake` and cleared by the driving thread once it has seen it,
    /// both under this lock. Checking it under the same lock the waker sets it
    /// under is what makes a wake that races the pending poll non-lossy: a wake
    /// landing between the poll and the wait leaves the flag set, so the
    /// driver sees it and never waits through the wakeup.
    notified: Mutex<bool>,
    /// Signalled by every wake; the driving thread is its only waiter.
    woken: Condvar,
}

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        *lock(&self.notified) = true;
        self.woken.notify_one();
    }
}

/// Synchronously polls a `Future` to completion on the current thread.
///
/// If the future is not immediately ready, the current thread waits until the
/// future's waker fires. A wake that races the transition into the wait is not
/// lost: the flag the waker sets is read under the lock it is set under, so a
/// wake delivered after the pending poll and before the wait skips the wait.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let signal = Arc::new(ThreadWaker::default());
    let waker = Waker::from(Arc::clone(&signal));
    let mut cx = Context::from_waker(&waker);

    loop {
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(val) => return val,
            Poll::Pending => {
                let mut notified = lock(&signal.notified);
                while !*notified {
                    notified = wait(&signal.woken, notified);
                }
                *notified = false;
            }
        }
    }
}

// ── join_all ───────────────────────────────────────────────────────────────

/// Drive `futures` concurrently to completion on the current thread and return
/// their outputs in input order.
///
/// Each child gets its own waker. A child's wake queues that child alone, and
/// the next poll of the group polls only the children that woke, so one wake
/// costs one child poll however many children are still pending. The first
/// poll polls every child once. It is still one thread: concurrency here is
/// interleaving, not parallelism, and it is not a work-stealing scheduler.
///
/// Completion order never affects the result: index `i` is the output of the
/// `i`-th input whichever finishes first. A future is dropped as soon as it
/// resolves, so the live-future count falls as work completes. An empty input
/// resolves immediately with an empty vector.
///
/// Cancellation: dropping the returned future drops every child that has not
/// resolved, which cancels a child only if that child is cancel-safe on drop.
/// It does not stop work already handed to [`spawn_blocking`]: a dropped
/// `JoinHandle` drops the handle, not the pool thread, which runs its closure
/// to completion. A caller that must stop in-flight blocking work has
/// to arrange that cooperatively inside the closure.
pub async fn join_all<F: Future>(futures: impl IntoIterator<Item = F>) -> Vec<F::Output> {
    join_all_boxed(futures.into_iter().map(Box::pin)).await
}

/// [`join_all`] over futures the caller has already boxed.
///
/// Same polling, ordering and cancellation semantics as [`join_all`]; the only
/// difference is the allocation model. `join_all` boxes each input once
/// (`n` boxes for `n` futures); this takes those boxes as given and adds none,
/// so a caller that already holds `Pin<Box<_>>` (for instance at a type-erasure
/// boundary) avoids a second box per future. The cost model
/// is `n` heap allocations either way, not a measured throughput claim.
pub async fn join_all_boxed<F: Future + ?Sized>(
    futures: impl IntoIterator<Item = Pin<Box<F>>>,
) -> Vec<F::Output> {
    let mut pending: Vec<Option<Pin<Box<F>>>> = futures.into_iter().map(Some).collect();
    let count = pending.len();
    let mut done: Vec<Option<F::Output>> = (0..count).map(|_| None).collect();
    let ready = Arc::new(ReadySet {
        inner: Mutex::new(ReadyInner {
            // Every child starts queued: the first poll polls each one once.
            order: (0..count).collect(),
            queued: vec![true; count],
            group: None,
        }),
    });
    let wakers: Vec<Waker> = (0..count)
        .map(|index| {
            Waker::from(Arc::new(ChildWaker {
                index,
                ready: Arc::clone(&ready),
            }))
        })
        .collect();
    let mut remaining = count;

    std::future::poll_fn(move |cx| {
        let woken = {
            let mut inner = lock(&ready.inner);
            let replace = inner
                .group
                .as_ref()
                .is_none_or(|existing| !existing.will_wake(cx.waker()));
            if replace {
                inner.group = Some(cx.waker().clone());
            }
            std::mem::take(&mut inner.order)
        };
        for index in woken {
            // Cleared before the poll, so a child that wakes itself while it
            // is being polled is queued again rather than lost.
            if let Some(flag) = lock(&ready.inner).queued.get_mut(index) {
                *flag = false;
            }
            let (Some(slot), Some(output), Some(waker)) = (
                pending.get_mut(index),
                done.get_mut(index),
                wakers.get(index),
            ) else {
                continue;
            };
            let Some(future) = slot.as_mut() else {
                continue;
            };
            if let Poll::Ready(value) = future.as_mut().poll(&mut Context::from_waker(waker)) {
                *output = Some(value);
                *slot = None;
                // Bounded by `count`: a slot is emptied once, here.
                remaining = remaining.saturating_sub(1);
            }
        }
        if remaining == 0 {
            // `remaining` falls by one exactly when a slot's output is filled,
            // so zero means every output slot was filled. `flatten` therefore
            // drops nothing; it is the panic-free spelling of the invariant,
            // and it keeps the result the same length as the input.
            Poll::Ready(done.drain(..).flatten().collect())
        } else {
            Poll::Pending
        }
    })
    .await
}

/// The children of one [`join_all_boxed`] that have woken since its last poll.
struct ReadySet {
    /// Guarded together, so a wake cannot land between the group reading the
    /// queue and registering its own waker.
    inner: Mutex<ReadyInner>,
}

/// The state behind [`ReadySet`]'s lock.
struct ReadyInner {
    /// Woken children, in wake order, each at most once.
    order: Vec<usize>,
    /// Whether each child is already in `order`, so a child that wakes twice
    /// before the next poll is polled once.
    queued: Vec<bool>,
    /// The waker of the task driving the group.
    group: Option<Waker>,
}

/// One child's waker: queue the child, then wake the group.
struct ChildWaker {
    /// The child's position in the input.
    index: usize,
    /// The group's queue.
    ready: Arc<ReadySet>,
}

impl Wake for ChildWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let group = {
            let mut inner = lock(&self.ready.inner);
            let newly = inner
                .queued
                .get_mut(self.index)
                .is_some_and(|flag| !std::mem::replace(flag, true));
            if newly {
                inner.order.push(self.index);
            }
            inner.group.clone()
        };
        // Outside the lock: the group's waker may run its task inline.
        if let Some(group) = group {
            group.wake();
        }
    }
}
