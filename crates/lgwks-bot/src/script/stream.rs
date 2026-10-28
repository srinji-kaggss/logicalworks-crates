//! Bounded live streams with a declared overflow policy (#654).
//!
//! A live feed — prices, events, ticks — is a buffer with a bound, an overflow
//! policy and a cancellation. In `script!` it is
//! `whenever x in batches, buffering N, drop oldest:` (or `drop newest`,
//! `refuse`), which builds its own gate. [`StreamGate`] holds at most `cap` unhandled
//! events, and every event past the cap follows the declared policy instead of
//! growing memory until the process dies. [`pump`] reacts to each event of the
//! feed in turn, in its own scope with its own key, and stops when the feed
//! ends or the scope is cancelled.
//!
//! The bound is the estate's own: the gate keeps its events in a
//! [`WorkQueue`](crate::dist_lease::WorkQueue), which refuses past its
//! capacity rather than grows, and the policy beside it says what a refusal
//! becomes — shed oldest, shed newest, or handed back. One bound, audited once,
//! counted twice (shed and refused).

use std::future::Future;

use crate::dist_lease::{QueueError, WorkQueue};
use crate::journal::frame::SaturatingFrom;

use super::trail::Site;
use super::{FlowError, MAX_IN_FLIGHT, Scope};

/// What happens to an event past the buffer's cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Overflow {
    /// Keep the newest: drop the oldest buffered event.
    DropOldest,
    /// Keep what arrived first: drop the incoming event.
    DropNewest,
    /// Refuse the push: the producer waits or sheds load elsewhere.
    Refuse,
}

/// A bounded buffer of unhandled events, with its overflow policy.
///
/// The bound lives in the [`WorkQueue`](crate::dist_lease::WorkQueue): at most
/// `cap` events are ever retained, and every event past it is counted as
/// dropped or refused by policy, so a fast producer against a slow consumer is
/// a number in the report, not memory growth.
#[derive(Debug)]
pub struct StreamGate<T> {
    /// The retained events, oldest first, bounded by the queue's capacity.
    queue: WorkQueue<T>,
    /// What happens past the cap.
    policy: Overflow,
    /// Events dropped by policy.
    dropped: u64,
    /// Pushes refused by policy.
    refused: u64,
}

impl<T> StreamGate<T> {
    /// A gate retaining at most `cap` events, overflowing by `policy`.
    ///
    /// # Errors
    ///
    /// [`QueueError::ZeroCapacity`] when `cap` is zero: a gate that holds
    /// nothing is a refusal, not a gate.
    pub fn new(cap: usize, policy: Overflow) -> Result<Self, QueueError> {
        Ok(Self {
            queue: WorkQueue::new(cap)?,
            policy,
            dropped: 0,
            refused: 0,
        })
    }

    /// Push `item`, applying the overflow policy past the cap.
    ///
    /// `DropOldest` and `DropNewest` always keep the gate within its bound and
    /// count the shed event; `Refuse` returns the item to the producer and
    /// counts the refusal.
    pub fn push(&mut self, item: T) -> Result<(), T> {
        if !self.queue.is_full() {
            // Room was checked above, so a refusal here is unreachable; the
            // item travels back rather than panics on a path the bound ruled
            // out.
            return self.queue.try_push(item).map_err(|over| over.into_item());
        }
        match self.policy {
            Overflow::DropOldest => {
                // Shed the oldest: the gate is full, so this yields the event
                // the policy sacrifices for the incoming one.
                drop(self.queue.pop());
                self.dropped = self.dropped.saturating_add(1);
                // The pop freed exactly one slot, so this push fits; a refusal
                // is unreachable and hands the item back rather than panics.
                self.queue.try_push(item).map_err(|over| over.into_item())
            }
            Overflow::DropNewest => {
                self.dropped = self.dropped.saturating_add(1);
                Ok(())
            }
            Overflow::Refuse => {
                self.refused = self.refused.saturating_add(1);
                Err(item)
            }
        }
    }

    /// Take the oldest retained event, if any.
    pub fn pop(&mut self) -> Option<T> {
        self.queue.pop()
    }

    /// How many events are retained.
    #[must_use]
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Whether nothing is retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// The bound on retained events.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.queue.capacity()
    }

    /// Events shed by policy so far.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Pushes refused by policy so far.
    #[must_use]
    pub fn refused(&self) -> u64 {
        self.refused
    }
}

/// What one [`pump`] run did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PumpReport {
    /// Events handled.
    pub handled: u64,
    /// Events shed by the overflow policy.
    pub dropped: u64,
    /// Pushes refused by the overflow policy.
    pub refused: u64,
}

/// React to each event of a live feed, bounded, in order, cancellable.
///
/// `recv` is polled for the next batch; `None` ends the stream. Every received
/// event crosses `gate` (bursts past the cap follow its policy), and every
/// retained event is handled in turn, each in its own scope with its own key,
/// before the next batch is read. A stop ends the pump as
/// [`FlowError::Cancelled`] with nothing left running; a handler failure ends
/// it with that failure. Either way the pump's step is settled.
///
/// # Errors
///
/// The handler's failure, or [`FlowError::Cancelled`] on a stop.
pub async fn pump<T, F, Fut, G, GFut>(
    scope: &Scope,
    step: &str,
    gate: &mut StreamGate<T>,
    mut recv: F,
    mut handle: G,
) -> Result<PumpReport, FlowError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<Vec<T>>>,
    G: FnMut(Scope, T) -> GFut,
    GFut: Future<Output = Result<(), FlowError>>,
{
    let here = scope.enter(step)?;
    let finished = drain(
        &here,
        gate,
        async || recv().await,
        async |event_scope: Scope, event: T| handle(event_scope, event).await,
    )
    .await;
    here.settle(&finished);
    finished
}

#[doc(hidden)]
/// [`pump`] for a `whenever x in batches, buffering N, <overflow>:` block
/// `script!` wrote at `site`: the gate is built here from the written bound
/// and policy, so the block owns its buffer and nothing outlives it. Called by
/// `script!`.
///
/// # Errors
///
/// [`FlowError::InvalidBound`] for a buffer outside `1..=MAX_IN_FLIGHT`, then
/// as [`pump`].
pub async fn stream_at<T, F, G>(
    scope: &Scope,
    step: &str,
    site: &'static Site,
    cap: usize,
    overflow: Overflow,
    recv: F,
    handle: G,
) -> Result<PumpReport, FlowError>
where
    F: AsyncFnMut() -> Option<Vec<T>>,
    G: AsyncFnMut(Scope, T) -> Result<(), FlowError>,
{
    let mut gate = buffer(cap, overflow)?;
    let here = scope.enter_at(step, site)?;
    let finished = drain(&here, &mut gate, recv, handle).await;
    here.settle(&finished);
    finished
}

/// A gate for a written `buffering N`, refused outside `1..=MAX_IN_FLIGHT`:
/// the same ceiling `each` puts on what it holds at once.
fn buffer<T>(cap: usize, overflow: Overflow) -> Result<StreamGate<T>, FlowError> {
    let refused = FlowError::InvalidBound {
        what: "whenever: buffering N",
        value: u64::saturating_from(cap),
        max: u64::saturating_from(MAX_IN_FLIGHT),
    };
    if cap > MAX_IN_FLIGHT {
        let refusal = Err(refused);
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "buffer: returning an error to the caller");
        return refusal;
    }
    match StreamGate::new(cap, overflow) {
        Ok(gate) => Ok(gate),
        Err(zero) => {
            let refusal = Err(refused);
            lgwks_std::trace::debug!(gate = %zero, error = ?refusal.as_ref().err(), "buffer: a zero buffer is refused");
            refusal
        }
    }
}

/// The loop of [`pump`] and [`stream_at`], inside the step they entered.
async fn drain<T, F, G>(
    here: &Scope,
    gate: &mut StreamGate<T>,
    mut recv: F,
    mut handle: G,
) -> Result<PumpReport, FlowError>
where
    F: AsyncFnMut() -> Option<Vec<T>>,
    G: AsyncFnMut(Scope, T) -> Result<(), FlowError>,
{
    let mut handled: u64 = 0;
    loop {
        here.checkpoint()?;
        let Some(batch) = recv().await else {
            break;
        };
        for item in batch {
            let mut item = item;
            while let Err(returned) = gate.push(item) {
                // Backpressure: a refusal means the gate is full, so handle
                // the oldest retained event to make room, then push again. The
                // gate never holds past its cap, and nothing is shed.
                item = returned;
                let Some(older) = gate.pop() else {
                    let refusal = Err(FlowError::failed("a full buffer had no event to hand on"));
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "drain: returning an error to the caller");
                    return refusal;
                };
                handle_next(here, handled, older, &mut handle).await?;
                handled = handled.saturating_add(1);
            }
        }
        while let Some(event) = gate.pop() {
            handle_next(here, handled, event, &mut handle).await?;
            handled = handled.saturating_add(1);
        }
    }
    Ok(PumpReport {
        handled,
        dropped: gate.dropped(),
        refused: gate.refused(),
    })
}

/// Handle the `index`th event in its own scope, settling the step either way.
async fn handle_next<T, G>(
    here: &Scope,
    index: u64,
    event: T,
    handle: &mut G,
) -> Result<(), FlowError>
where
    G: AsyncFnMut(Scope, T) -> Result<(), FlowError>,
{
    let event_scope = here.item("event", usize::saturating_from(index))?;
    let outcome = handle(event_scope.clone(), event).await;
    event_scope.settle(&outcome);
    outcome.map_err(|error| error.located(&event_scope))
}

#[cfg(test)]
mod tests {
    use super::{Overflow, PumpReport, StreamGate, pump};
    use crate::dist_lease::QueueError;
    use crate::script::{FlowError, Scope, Tenant};

    /// A test's result: an error fails it with the error's text.
    type TestResult = Result<(), String>;

    /// A gate of `cap` with `policy`.
    fn gate<T>(cap: usize, policy: Overflow) -> Result<StreamGate<T>, String> {
        StreamGate::new(cap, policy).map_err(|error| error.to_string())
    }

    /// A zero cap is refused typed, not a gate that holds nothing.
    #[test]
    fn a_zero_cap_is_a_typed_refusal() -> TestResult {
        match StreamGate::<u32>::new(0, Overflow::Refuse) {
            Err(QueueError::ZeroCapacity) => Ok(()),
            Ok(_) => Err(String::from("a zero cap must not build")),
        }
    }

    /// `DropOldest` keeps the newest `cap` events and counts the shed ones.
    #[test]
    fn drop_oldest_keeps_the_newest_and_counts_the_shed() -> TestResult {
        let mut gate = gate(2, Overflow::DropOldest)?;
        for item in [1_u32, 2, 3, 4] {
            gate.push(item)
                .map_err(|_| String::from("drop-oldest never refuses"))?;
        }
        assert_eq!(gate.len(), 2, "the gate never holds past its cap");
        assert_eq!(gate.pop(), Some(3_u32), "the oldest events were shed");
        assert_eq!(gate.pop(), Some(4_u32), "the newest events are kept");
        assert_eq!(gate.dropped(), 2, "every shed event is counted");
        Ok(())
    }

    /// `DropNewest` keeps what arrived first; `Refuse` hands the event back.
    #[test]
    fn drop_newest_keeps_the_first_and_refuse_hands_back() -> TestResult {
        let mut keep = gate(1, Overflow::DropNewest)?;
        keep.push(1_u32)
            .map_err(|_| String::from("the first push fits"))?;
        keep.push(2_u32)
            .map_err(|_| String::from("drop-newest never refuses"))?;
        assert_eq!(keep.pop(), Some(1_u32), "what arrived first is kept");
        assert_eq!(keep.dropped(), 1, "the shed event is counted");

        let mut refuse = gate(1, Overflow::Refuse)?;
        refuse
            .push(1_u32)
            .map_err(|_| String::from("the first push fits"))?;
        let handed = match refuse.push(2_u32) {
            Err(item) => item,
            Ok(()) => return Err(String::from("refuse must hand the event back")),
        };
        assert_eq!(handed, 2_u32, "the refused event reaches the producer");
        assert_eq!(refuse.refused(), 1, "the refusal is counted");
        Ok(())
    }

    /// `pump` handles every event of a seeded feed in order, within the bound.
    #[test]
    fn pump_handles_every_event_within_the_bound() -> TestResult {
        use std::cell::RefCell;
        use std::collections::VecDeque;

        let scope = Scope::root(Tenant::new("acme").map_err(|error| error.to_string())?);
        let mut gate = gate(4, Overflow::DropOldest)?;
        let pending: RefCell<VecDeque<Option<Vec<u32>>>> = RefCell::new(
            [Some(vec![1, 2, 3]), Some(vec![4, 5]), None]
                .into_iter()
                .collect(),
        );
        let seen: RefCell<Vec<u32>> = RefCell::new(Vec::new());
        let report: PumpReport = crate::block_on(pump(
            &scope,
            "prices",
            &mut gate,
            || {
                let batch = pending.borrow_mut().pop_front().flatten();
                async move { batch }
            },
            |_, event| {
                if let Ok(mut held) = seen.try_borrow_mut() {
                    held.push(event);
                }
                async move { Ok::<(), FlowError>(()) }
            },
        ))
        .map_err(|error| error.to_string())?;
        let seen = seen
            .try_borrow()
            .map_err(|_| String::from("the seen order was poisoned"))?;
        assert_eq!(
            *seen,
            vec![1_u32, 2, 3, 4, 5],
            "every event is handled in order"
        );
        assert_eq!(
            report,
            PumpReport {
                handled: 5,
                dropped: 0,
                refused: 0,
            },
            "a feed inside the bound sheds nothing"
        );
        Ok(())
    }
}
