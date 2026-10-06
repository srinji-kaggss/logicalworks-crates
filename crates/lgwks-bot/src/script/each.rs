//! Bounded fan-out, driven on the awaiting task.

use std::collections::VecDeque;
use std::future::Future;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Wake, Waker};

use super::{FlowError, MAX_IN_FLIGHT, POLL_BUDGET, Scope};

// ── each ────────────────────────────────────────────────────────────────────

/// Run `body` once per item, at most `limit` at a time, and return the values
/// in input order. With no `limit`, the root scope's policy sizes the fan-out
/// to the machine (64 per available core).
///
/// - **Bounded.** At most `limit` bodies exist at once. Items are drawn from the
///   iterator only as slots free, so an iterator of a million items holds
///   `limit` bodies and one output vector, never a million futures.
/// - **Ordered.** Output `i` is item `i`'s value, whatever order they finish in.
/// - **Fail-fast and owned.** The first error stops the fan-out: the step's
///   token is cancelled, every body still running is dropped, no further item
///   is started, and that error is returned located at its item's path. No
///   sibling outlives the call.
/// - **Keyed.** Item `i` runs in the scope `<step>#i`, so its [`StepKey`](crate::script::StepKey) is
///   stable across runs over the same input order and distinct per tenant.
/// - **Woken precisely.** Each slot has its own waker; a wake re-polls that
///   body only, so one wake costs `O(1)` polls rather than a scan of every
///   body in flight.
/// - **Fair to its executor.** One poll polls at most a fixed number of
///   bodies, then yields with the task re-woken, so a fan-out never
///   monopolises its thread or spins on the engine's cooperative budget.
///
/// # Errors
///
/// The first body error, or [`FlowError::Cancelled`] when `scope` is stopped.
pub async fn each<I, T, F, Fut>(
    scope: &Scope,
    step: &str,
    limit: Option<NonZeroUsize>,
    items: I,
    body: F,
) -> Result<Vec<T>, FlowError>
where
    I: IntoIterator,
    F: Fn(Scope, I::Item) -> Fut,
    Fut: Future<Output = Result<T, FlowError>>,
{
    let here = scope.enter(step)?;
    // An absent limit is the scope's own policy — the machine's core count under
    // the declared ceiling — which is a decision about capacity rather than a
    // value this call failed to produce. A caller's own limit is bounded by the
    // same ceiling, so neither path can ask for more bodies than the crate runs.
    let declared = match limit {
        Some(caller_limit) => caller_limit,
        None => scope.policy().fan_out(),
    };
    let width = declared.get().min(MAX_IN_FLIGHT);
    let wakes = Arc::new(Wakes::new(width));
    let wakers: Vec<Waker> = (0..width)
        .map(|position| {
            Waker::from(Arc::new(SlotWaker {
                position,
                wakes: Arc::clone(&wakes),
            }))
        })
        .collect();
    let mut fan = Fan {
        slots: (0..width).map(|_| None).collect(),
        free: (0..width).rev().collect(),
        results: Vec::new(),
        exhausted: false,
    };
    let mut inputs = items.into_iter();
    let mut stopped = Box::pin(here.token().cancelled_owned());
    let body = &body;
    let here = &here;

    std::future::poll_fn(move |context: &mut Context<'_>| {
        wakes.set_parent(context.waker());
        let mut polled: usize = 0;
        loop {
            // Checked every round, not once per wake: a body may raise the
            // stop and finish inside one round, and a stop that arrived
            // before the last result must still end the flow as cancelled.
            if stopped.as_mut().poll(context).is_ready() {
                fan.clear();
                return Poll::Ready(Err(FlowError::Cancelled {
                    at: Arc::clone(here.shared_path()),
                }));
            }
            if let Err(error) = fan.admit(&mut inputs, here, body, &wakes) {
                fan.clear();
                return Poll::Ready(Err(error));
            }
            let ready = wakes.take_ready();
            if ready.is_empty() {
                break;
            }
            for position in ready {
                polled = polled.saturating_add(1);
                if let Err(error) = fan.poll_slot(position, &wakers) {
                    here.cancel();
                    fan.clear();
                    return Poll::Ready(Err(error));
                }
            }
            if polled >= POLL_BUDGET {
                context.waker().wake_by_ref();
                return Poll::Pending;
            }
        }
        if fan.exhausted && fan.free.len() == fan.slots.len() {
            Poll::Ready(Ok(fan.results.drain(..).flatten().collect()))
        } else {
            Poll::Pending
        }
    })
    .await
}

/// One occupied slot of a fan-out.
///
/// The body's future is stored as its own type, boxed only to pin it, never
/// erased to `dyn Future`: so a fan-out is `Send` exactly when its bodies are,
/// and a flow built on it can be spawned onto a multi-threaded runtime.
struct Slot<Fut> {
    /// The item's position in the input, which is its position in the output.
    index: usize,
    /// The item's scope path, for locating its error.
    at: Arc<str>,
    /// The running body.
    future: Pin<Box<Fut>>,
}

/// The state [`each`] drives: slots, the free list, and the ordered outputs.
struct Fan<T, Fut> {
    /// `limit` slots; `None` is free.
    slots: Vec<Option<Slot<Fut>>>,
    /// Free slot positions.
    free: Vec<usize>,
    /// One entry per item admitted, filled as each finishes.
    results: Vec<Option<T>>,
    /// Whether the input iterator has ended.
    exhausted: bool,
}

impl<T, Fut: Future<Output = Result<T, FlowError>>> Fan<T, Fut> {
    /// Start bodies for new items while a slot is free and input remains.
    fn admit<I, F>(
        &mut self,
        inputs: &mut I,
        here: &Scope,
        body: &F,
        wakes: &Wakes,
    ) -> Result<(), FlowError>
    where
        I: Iterator,
        F: Fn(Scope, I::Item) -> Fut,
    {
        while !self.exhausted {
            let Some(position) = self.free.pop() else {
                break;
            };
            let Some(item) = inputs.next() else {
                self.free.push(position);
                self.exhausted = true;
                break;
            };
            let index = self.results.len();
            self.results.push(None);
            let scope = here.numbered(index)?;
            let at = Arc::clone(scope.shared_path());
            let future = Box::pin(body(scope, item));
            if let Some(slot) = self.slots.get_mut(position) {
                *slot = Some(Slot { index, at, future });
            }
            wakes.mark(position);
        }
        Ok(())
    }

    /// Poll the body in `position` with its own waker, keeping its value or
    /// returning its located error.
    fn poll_slot(&mut self, position: usize, wakers: &[Waker]) -> Result<(), FlowError> {
        let (Some(entry), Some(waker)) = (self.slots.get_mut(position), wakers.get(position))
        else {
            return Ok(());
        };
        let Some(slot) = entry.as_mut() else {
            return Ok(());
        };
        let mut context = Context::from_waker(waker);
        let Poll::Ready(outcome) = slot.future.as_mut().poll(&mut context) else {
            return Ok(());
        };
        let index = slot.index;
        let at = Arc::clone(&slot.at);
        *entry = None;
        self.free.push(position);
        let value = outcome.map_err(|error| error.located_at(&at))?;
        if let Some(output) = self.results.get_mut(index) {
            *output = Some(value);
        }
        Ok(())
    }

    /// Drop every body still running and admit nothing more.
    fn clear(&mut self) {
        for slot in &mut self.slots {
            *slot = None;
        }
        self.exhausted = true;
    }
}

/// Which slots have been woken, and the waker of the task driving them.
struct Wakes {
    /// Guarded together so a wake cannot land between reading the queue and
    /// registering the parent.
    state: Mutex<WakeState>,
}

/// The fields behind [`Wakes`].
struct WakeState {
    /// Slot positions woken since the last drain, each at most once.
    ready: VecDeque<usize>,
    /// Whether each position is already in `ready`.
    queued: Vec<bool>,
    /// The task that awaits the fan-out.
    parent: Option<Waker>,
}

impl Wakes {
    /// Room for `width` slots, none woken.
    fn new(width: usize) -> Self {
        Self {
            state: Mutex::new(WakeState {
                ready: VecDeque::with_capacity(width),
                queued: vec![false; width],
                parent: None,
            }),
        }
    }

    /// Take the lock. A poisoned lock still guards a consistent value: every
    /// critical section here is a few plain writes that cannot be left half
    /// done, so a panic elsewhere is not a reason to deadlock the fan-out.
    fn lock(&self) -> MutexGuard<'_, WakeState> {
        crate::journal::owner::lock(&self.state)
    }

    /// Record the waker of the task driving the fan-out.
    fn set_parent(&self, waker: &Waker) {
        let mut state = self.lock();
        let stale = state
            .parent
            .as_ref()
            .is_none_or(|current| !current.will_wake(waker));
        if stale {
            state.parent = Some(waker.clone());
        }
    }

    /// Queue `position` for polling. Used when a body is first admitted, where
    /// the driving task is already running and needs no wake.
    fn mark(&self, position: usize) {
        enqueue(&mut self.lock(), position);
    }

    /// Queue `position` and hand back the waker of the task to wake.
    fn wake_slot(&self, position: usize) -> Option<Waker> {
        let mut state = self.lock();
        enqueue(&mut state, position);
        state.parent.clone()
    }

    /// Everything queued since the last call, with the flags cleared first so
    /// a wake during the coming polls queues its slot again.
    fn take_ready(&self) -> VecDeque<usize> {
        let mut state = self.lock();
        let ready = std::mem::take(&mut state.ready);
        for &position in &ready {
            if let Some(flag) = state.queued.get_mut(position) {
                *flag = false;
            }
        }
        ready
    }
}

/// Put `position` on the ready queue unless it is already there.
fn enqueue(state: &mut WakeState, position: usize) {
    if let Some(flag) = state.queued.get_mut(position)
        && !*flag
    {
        *flag = true;
        state.ready.push_back(position);
    }
}

/// The waker one slot's body sees: it queues the slot and wakes the parent.
struct SlotWaker {
    /// Which slot.
    position: usize,
    /// The fan-out's queue.
    wakes: Arc<Wakes>,
}

impl Wake for SlotWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if let Some(parent) = self.wakes.wake_slot(self.position) {
            parent.wake();
        }
    }
}
