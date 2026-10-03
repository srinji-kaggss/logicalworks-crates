//! The one thread that owns a file-backed store's device.
//!
//! Two stores in this crate write a disk: the effect journal and the run store.
//! Both made the same mistake before this module existed — `write_all` and
//! `sync_all` reached the device from the thread that awaited the append, so the
//! device's latency became the process's latency. A heartbeat, a timer and every
//! unrelated task in the same runtime stalled for the length of an `fsync`. That
//! is the defect #122 removed from [`FileJournal`](super::FileJournal) and that
//! the run store inherited when it was written later. This module is the
//! extraction: one owner thread, one bounded request queue, one poison latch, two
//! instantiations.
//!
//! # The owner runs the caller's whole critical section, not just the write
//!
//! What runs on this thread is a caller-supplied closure over the file *and* the
//! state the caller shares. That is what makes it correct rather than merely
//! off-thread: a store that staged its frames on the calling thread and sent only
//! the bytes would have to read its "what length does the file have" answer from
//! shared state, and a second append admitted in between would make that answer
//! stale before the owner's fence used it. Running the closure here makes the
//! fence, the write, the sync and the fold of the answer into shared state one
//! ordered step no other append can overtake — which is the property both stores
//! actually rest on.
//!
//! It is also what makes an abandoned await safe. A caller that walks away from a
//! record it asked for cannot un-write the bytes or un-fold the index, and needs
//! no repair: the step's record either landed and will be replayed, or never
//! started, and both are states a resume already knows how to read.
//!
//! # The bound
//!
//! One bounded queue, sized by the caller at [`StorageOwner::spawn`]. A journal
//! serialises its appends through `&mut self`, so one request at a time is already
//! all it can have; a run store's append takes `&self` and many runs reach it at
//! once, so it asks for more and still gets a typed refusal rather than an
//! unbounded wait once the queue is full.
//!
//! # What the owner preserves
//!
//! Ordering is the owner's: it is one thread taking one request at a time, so the
//! order requests were admitted in is the order they reach the disk. A request that
//! fails inside the ordered step may have moved a prefix of the bytes, so the owner
//! latches the poison and the handle refuses every later append until a reopen
//! replays the truth.

use std::collections::VecDeque;
use std::fs::File;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::task::Waker;
use std::time::Duration;

/// The most requests one store may have outstanding at the owner.
///
/// A journal asks for one, because `&mut self` already admits one. A run store's
/// append is `&self` and many runs reach it, so it asks for more; the number is a
/// bound, not a hint, and a caller that arrives at a full queue is told so rather
/// than made to wait without limit.
pub(crate) const DEFAULT_QUEUE_DEPTH: usize = 64;

/// Take the lock, treating a poisoned one as recoverable.
///
/// Nothing in either store's append path can panic while the lock is held — every
/// arm is a `Result` that propagates — so a poison is a bug in an unrelated thread,
/// and refusing every later append because of it would turn one failure into a
/// bricked store.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One durable request, as the storage owner receives it, and the answer it gives
/// back to the caller that asked.
///
/// Generic in both because both are the caller's: a journal's request returns no
/// answer and has no state, and a run store's returns which of its three record
/// outcomes happened and holds the index it folds into. Parameterising the owner in
/// both is what lets one owner serve both without either store naming the other's
/// vocabulary.
pub(crate) type Job<S, A> = Box<dyn FnOnce(&mut File, &mut S) -> Result<A, std::io::Error> + Send>;

/// One job and the slot its answer is written into.
///
/// The owner loop owns the write rather than the closure: the closure's whole job is
/// the ordered step, and making it also responsible for reporting means an early
/// return inside it would leave a caller waiting on an answer nothing will ever
/// write. Here the publish is one statement the loop cannot skip.
struct Envelope<S, A> {
    /// The ordered step.
    job: Job<S, A>,
    /// Where the answer goes, or nowhere if the caller has gone.
    reply: Arc<Answer<A>>,
}

/// The slot one answer travels through, awaitable and blockable alike.
///
/// A channel would not serve both doors: the blocking caller has no reactor to park
/// a waker with, and the awaiting caller must not park its thread. One slot with a
/// condvar serves the first, and a waker the owner fires serves the second, so one
/// answer type answers both without either caller knowing which it is.
struct Answer<A> {
    /// The answer, once the owner has written it.
    slot: Mutex<Option<Result<A, std::io::Error>>>,
    /// Woken when it is written.
    arrived: Condvar,
    /// The task to wake, when the caller is awaiting rather than blocking.
    waker: Mutex<Option<Waker>>,
    /// Whether the caller walked away before the answer arrived.
    abandoned: Mutex<bool>,
}

impl<A> Answer<A> {
    /// An empty answer slot.
    fn new() -> Self {
        Self {
            slot: Mutex::new(None),
            arrived: Condvar::new(),
            waker: Mutex::new(None),
            abandoned: Mutex::new(false),
        }
    }

    /// Record that the caller went away before its answer arrived.
    ///
    /// Idempotent, and deliberately not a cancellation: the owner still performs the
    /// request, because bytes already moving are not undoable, and only the handle's
    /// claim about them is affected.
    fn abandon(&self) {
        *lock(&self.abandoned) = true;
    }

    /// Whether the caller of this request went away.
    fn was_abandoned(&self) -> bool {
        *lock(&self.abandoned)
    }

    /// Publish the answer and wake whoever is waiting for it.
    ///
    /// # Errors
    ///
    /// Nothing fails here: a slot already holding an answer means two owners wrote
    /// to one slot, which the type does not permit, so the first answer is kept and
    /// the second is dropped rather than replacing it.
    fn publish(&self, outcome: Result<A, std::io::Error>) {
        *lock(&self.slot) = Some(outcome);
        if let Some(waker) = lock(&self.waker).take() {
            waker.wake();
        }
        self.arrived.notify_all();
    }

    /// Take the answer if it is already there, without waiting.
    fn take(&self) -> Option<Result<A, std::io::Error>> {
        lock(&self.slot).take()
    }

    /// Take the answer, blocking the calling thread until it arrives.
    fn wait_blocking(&self) -> Option<Result<A, std::io::Error>> {
        let mut held = lock(&self.slot);
        loop {
            if let Some(answer) = held.take() {
                return Some(answer);
            }
            held = self
                .arrived
                .wait(held)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
}

/// The slot both sides read, the bounded request ring, and the poison the owner
/// latches.
struct Slot<S, A> {
    /// Requests admitted and not yet taken by the owner, in arrival order.
    ///
    /// A bounded ring rather than a channel, and the bound is what makes it one.
    /// The estate's channel (`rt::sync::mpsc`) is the right choice *with a
    /// runtime*, and this queue deliberately has none: the owner thread outlives
    /// every future that ever waits on it, so it must not depend on a runtime
    /// being alive to be driven. `std::sync::mpsc` is the alternative and is
    /// disallowed workspace-wide (INV-RT-BOUNDED) precisely because it has no
    /// bounded form. A `VecDeque` with a checked push is the bound written out,
    /// and it costs one lock the owner already holds.
    pending: VecDeque<Envelope<S, A>>,
    /// Whether bytes may be on the disk that no acknowledgment names.
    poisoned: bool,
    /// Whether the handle is gone and the owner should finish and exit.
    closed: bool,
    /// Whether the owner has dropped the file.
    released: bool,
    /// Whether the next request should report a device refusal instead of writing.
    fail_next: bool,
}

/// `Slot::default` by hand rather than derived.
///
/// The derived one would demand `S: Default` and `A: Default` for a struct that
/// stores neither: `S` and `A` appear only inside `pending`'s envelopes, which
/// start empty. Requiring the caller's state to be constructible just to open an
/// owner would be a bound neither store can satisfy.
impl<S, A> Default for Slot<S, A> {
    fn default() -> Self {
        Self {
            pending: VecDeque::new(),
            poisoned: false,
            closed: false,
            released: false,
            fail_next: false,
        }
    }
}

/// The one thread that owns a store's file.
///
/// # ASSUMPTION: the owner is the only writer
///
/// Every write on both stores goes through this queue, and the only other handle
/// anyone holds is a read-only view that can be asked a length and nothing else.
/// This is a durability boundary, not a style preference: it is why the length
/// check and the write it guards can be one ordered step.
/// `grep -n "ASSUMPTION: the owner is the only writer"` finds every place that
/// claim is relied on.
///
/// # ASSUMPTION: an append is at-least-once, never exactly-once
///
/// The owner performs what it was asked and reports what happened. It cannot know
/// whether a caller that stopped listening will retry, and it does not pretend to:
/// a failed request latches the poison, and the next reader reconciles by reading
/// the file back rather than by assuming. Exactly-once is a property of a caller's
/// idempotency key, never of this thread.
///
/// # Why the file is not on the handle
///
/// `write_all` and `sync_all` are the storage device's cost, and the durable path
/// reaches them from the thread that awaits the append. On a current-thread
/// executor there is no other thread to run anything, so the device's latency became
/// the process's latency: a heartbeat, a timer and every unrelated bot in the same
/// runtime stalled for the length of an `fsync`. Moving the file here makes the
/// device's cost the owner's, and the caller's cost a wait it may await rather than
/// sit through.
pub(crate) struct StorageOwner<S, A>
where
    S: Send + 'static,
    A: Send + 'static,
{
    /// The poison latch, the stall gate and the bounded request ring.
    slot: Arc<Mutex<Slot<S, A>>>,
    /// The same latch as its own type, so a gate outlives the store's generics.
    gate: Arc<Mutex<Gate>>,
    /// Woken on every state change, so a blocking drop and the owner's own wait
    /// both re-read rather than spin.
    signal: Arc<Condvar>,
}

/// A handle onto one store's storage device, cloneable and independent of the store.
///
/// It exists because a caller awaiting an append cannot reach the store to release
/// it: the future holds the handle's borrow for exactly as long as the append is
/// outstanding, which is the window in which the device most needs to be unstuck. An
/// operator un-sticking a real device is in the same position — their handle is busy
/// on the append that is waiting.
#[derive(Clone)]
pub struct StorageGate {
    /// The stall latch, held apart from the request ring.
    ///
    /// Its own state rather than a parameterised view of the owner's `Slot`: a
    /// gate is reachable from either store and from an operator's separate
    /// handle, so a type parameterised by one store's state and another's answer
    /// would make it nameable only from the store that made it. The latch holds
    /// only the flag an operator changes and the poison a reader observes, which
    /// is the whole of what a gate is.
    latch: Arc<Mutex<Gate>>,
    /// Woken so the owner re-reads the gate.
    signal: Arc<Condvar>,
}

/// The flags a [`StorageGate`] can reach, shared with the owner.
///
/// Split out of [`Slot`] so the gate stays one type for both stores: the stall
/// latch and the poison are the only state a releaser or an observer needs, and
/// neither is specific to a store's request or answer type.
#[derive(Default)]
struct Gate {
    /// Whether a flush should wait for an explicit release before syncing.
    ///
    /// The stall latch alone, not the poison: a releaser changes the stall and
    /// nothing else, so the poison stays on the request side where the owner
    /// writes it and the store's append path reads it. Carrying it here too
    /// would be a second latch with no writer, which is the shape of a bug.
    stalled: bool,
}

impl StorageGate {
    /// Let a flush that is waiting for a release proceed, and every flush after it.
    /// The stall is not re-armed.
    pub fn release(&self) {
        lock(&self.latch).stalled = false;
        self.signal.notify_all();
    }
}

/// Why an append did not reach the device, or reached it without an answer.
///
/// Three refusals and one device error, kept apart because the repair differs per
/// arm: a poison is fixed by reopening, a full queue by waiting, an undelivered
/// answer by reading the file back, and a device error by whatever the device says.
#[derive(Debug)]
pub(crate) enum SubmitError {
    /// A previous request's outcome is unknown, so this handle cannot append
    /// again. Only a reopen, which replays the truth, clears it.
    Poisoned,
    /// The bounded queue was full: more requests are outstanding than the ceiling
    /// admits. Nothing was written, and the caller may retry.
    QueueFull,
    /// The owner finished the request but the answer could not be delivered, so a
    /// prefix or all of the bytes may be on the disk.
    OutcomeUnknown,
    /// Whatever the device or the store's own check reported.
    Device(std::io::Error),
}

impl SubmitError {
    /// Whether the bytes may be on the disk under no acknowledgment.
    ///
    /// The one question a caller has to answer before deciding a retry is safe:
    /// `true` means the answer is genuinely unknown and only reading the file back
    /// settles it, `false` means nothing was written and the append may simply be
    /// tried again.
    pub(crate) const fn is_outcome_unknown(&self) -> bool {
        matches!(self, Self::OutcomeUnknown | Self::Device(_))
    }

    /// This refusal as a device error, which is what both stores report it as.
    pub(crate) fn into_io(self) -> std::io::Error {
        match self {
            Self::Poisoned => std::io::Error::other(
                "a previous append's outcome is unknown; reopen the store to replay the truth",
            ),
            Self::QueueFull => std::io::Error::other(
                "the store's write queue is full; this append was refused, not queued",
            ),
            Self::OutcomeUnknown => std::io::Error::other(
                "the storage owner answered nobody; a prefix of the bytes may be on the disk",
            ),
            Self::Device(cause) => cause,
        }
    }
}

impl From<std::io::Error> for SubmitError {
    fn from(cause: std::io::Error) -> Self {
        Self::Device(cause)
    }
}

impl<S, A> StorageOwner<S, A>
where
    S: Send + 'static,
    A: Send + 'static,
{
    /// Hand `file` and `state` to a new owner thread and return the handle side.
    ///
    /// A dedicated thread, not a pooled blocking job: this one outlives every job it
    /// runs and holds the file for the handle's whole life, so it is a thread with a
    /// lifetime rather than work handed to a pool. The handle is owned by
    /// [`StorageOwner::drop`], which closes the slot and waits for the owner to
    /// report it has let the file go.
    ///
    /// # Errors
    ///
    /// [`std::io::Error`] when the OS refuses to start the thread, which is the one
    /// way a store cannot be made durable at all.
    pub(crate) fn spawn(file: File, state: S, stalled: bool) -> std::io::Result<Self> {
        let slot = Arc::new(Mutex::new(Slot::<S, A>::default()));
        let gate = Arc::new(Mutex::new(Gate { stalled }));
        let signal = Arc::new(Condvar::new());
        let owner = Self {
            slot: Arc::clone(&slot),
            gate: Arc::clone(&gate),
            signal: Arc::clone(&signal),
        };
        let _task = lgwks_std::task::spawn_blocking(move || serve(file, state, slot, gate, signal));
        Ok(owner)
    }

    /// A gate that can release this owner's stall, independently of the store.
    pub(crate) fn gate(&self) -> StorageGate {
        StorageGate {
            latch: Arc::clone(&self.gate),
            signal: Arc::clone(&self.signal),
        }
    }

    /// Whether a later append may be admitted at all.
    pub(crate) fn poisoned(&self) -> bool {
        lock(&self.slot).poisoned
    }

    /// Make the next request report a device refusal instead of writing.
    ///
    /// Private, and deliberately so: the failure this models is a write that fails
    /// after the fence has passed, which no filesystem will produce on demand, and
    /// the poison latch behind it has to be tested somehow.
    #[cfg(test)]
    pub(crate) fn fail_next_commit(&self) {
        lock(&self.slot).fail_next = true;
    }

    /// Perform `job` on the owner thread and wait for it to finish.
    ///
    /// Blocking, for a caller on a thread with nothing to await. The awaited form is
    /// [`StorageOwner::submit_async`], which the durable path uses.
    ///
    /// # Errors
    ///
    /// Whatever `job` reports, plus the queue being full: an append refused for want
    /// of room is a typed refusal, never a silent wait.
    pub(crate) fn submit<F>(&self, job: F) -> Result<A, SubmitError>
    where
        F: FnOnce(&mut File, &mut S) -> Result<A, std::io::Error> + Send + 'static,
    {
        let reply = Arc::new(Answer::new());
        self.enqueue(Box::new(job), Arc::clone(&reply))?;
        match reply.wait_blocking() {
            Some(outcome) => outcome.map_err(SubmitError::Device),
            None => Err(SubmitError::OutcomeUnknown),
        }
    }

    /// The same door, for a caller that awaits rather than blocks.
    ///
    /// The difference is only in how the answer is delivered: a blocking caller
    /// waits on the channel, a task awaits it and is woken by the owner when the
    /// answer lands. The append itself is the same ordered step either way, so a
    /// caller choosing to await does not get a weaker write than one that chose to
    /// sit through it.
    pub(crate) fn submit_async<F>(&self, job: F) -> crate::BoxFuture<'_, Result<A, SubmitError>>
    where
        F: FnOnce(&mut File, &mut S) -> Result<A, std::io::Error> + Send + 'static,
    {
        let reply = Arc::new(Answer::new());
        let outcome = self.enqueue(Box::new(job), Arc::clone(&reply));
        Box::pin(Awaiting {
            enqueued: Some(outcome),
            reply,
        })
    }

    /// Hand one request to the owner, or report that this handle may not append.
    ///
    /// The bound is a refusal, not a wait: a caller that has outrun the device by
    /// more than [`DEFAULT_QUEUE_DEPTH`] requests is told so immediately, because
    /// queueing without limit is how a parked device turns into unbounded memory.
    fn enqueue(&self, job: Job<S, A>, reply: Arc<Answer<A>>) -> Result<(), SubmitError> {
        let mut held = lock(&self.slot);
        if held.poisoned {
            return Err(SubmitError::Poisoned);
        }
        if held.pending.len() >= DEFAULT_QUEUE_DEPTH {
            return Err(SubmitError::QueueFull);
        }
        held.pending.push_back(Envelope { job, reply });
        drop(held);
        self.signal.notify_all();
        Ok(())
    }
}

/// One request handed to the owner, waiting for its answer.
///
/// A named future rather than an inline `async` block because its `poll` has to
/// re-check the slot on every wake, and an `async` block's body would run once and
/// then park — which is exactly the shape that would need a second await to notice
/// the answer. Here each poll reads the slot, so a missed wake costs a re-poll
/// rather than a hang.
struct Awaiting<A> {
    /// Whether the request reached the owner at all. An error here is the queue's,
    /// and there is nothing to wait for. Taken on the first poll only.
    enqueued: Option<Result<(), SubmitError>>,
    /// Where the owner will write, and where this poll reads.
    reply: Arc<Answer<A>>,
}

impl<A> std::future::Future for Awaiting<A> {
    type Output = Result<A, SubmitError>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.get_mut();
        if let Some(Err(cause)) = this.enqueued.take() {
            return std::task::Poll::Ready(Err(cause));
        }
        // The answer is published under the slot's lock and the waker registered
        // under the waker's, and the owner publishes before it notifies, so neither
        // can be missed: either the answer is already here, or the waker it fired
        // belongs to this poll.
        if let Some(answer) = this.reply.take() {
            return std::task::Poll::Ready(answer.map_err(SubmitError::Device));
        }
        *lock(&this.reply.waker) = Some(cx.waker().clone());
        std::task::Poll::Pending
    }
}

impl<A> Drop for Awaiting<A> {
    /// A caller that walks away before its answer arrives marks the request
    /// abandoned.
    ///
    /// Not a cancellation and not a repair: the bytes may be on the disk under no
    /// acknowledgment, which is exactly the position a failed write leaves the
    /// handle in, so the owner poisons the handle the same way (INV-BOT-15). The
    /// owner performs what it was asked regardless — an append already in flight is
    /// not undone — and only a reopen, which replays the truth, clears it.
    ///
    /// Dropping after the answer arrived is not an abandonment, so a request that
    /// completed normally leaves the handle usable.
    fn drop(&mut self) {
        if self.reply.take().is_none() {
            self.reply.abandon();
        }
    }
}

impl<S, A> Drop for StorageOwner<S, A>
where
    S: Send + 'static,
    A: Send + 'static,
{
    fn drop(&mut self) {
        let mut slot = lock(&self.slot);
        slot.closed = true;
        self.signal.notify_all();
        // Wait for the owner to report the file released, so the descriptor — and
        // any advisory lock held on it — is gone before a caller reopens the same
        // path. The wait is bounded by the device: it ends when the owner's own
        // in-flight append ends, which is the same append whose bytes are on the
        // disk under the acknowledgment the caller may never see.
        while !slot.released {
            slot = self
                .signal
                .wait(slot)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
}

/// The owner's loop: take a request, perform it, answer, repeat.
///
/// The file and the caller's state are dropped before `released` is set, so a
/// waiter that observes the release knows everything is already gone rather than
/// about to be. The loop ends when the handle has closed *and* the queue has
/// drained, so a request submitted just before the last handle went is still
/// performed: the bytes it would have written are the ones an acknowledgment may
/// already name.
fn serve<S, A>(
    mut file: File,
    mut state: S,
    slot: Arc<Mutex<Slot<S, A>>>,
    gate: Arc<Mutex<Gate>>,
    signal: Arc<Condvar>,
) {
    while let Some(envelope) = next(&slot, &gate, &signal) {
        // The whole critical section runs here: the fence, the write, the sync and
        // the fold into shared state, in one ordered step no other request can
        // overtake.
        let Envelope { job, reply } = envelope;
        let injected = {
            let mut held = lock(&slot);
            std::mem::take(&mut held.fail_next)
        };
        let outcome = if injected {
            Err(std::io::Error::other("the injected device refusal"))
        } else {
            job(&mut file, &mut state)
        };
        // A caller that went away while the bytes were moving leaves the handle
        // unable to say whether they landed, which is the same position a failed
        // write leaves it in: poisoned until a reopen replays the truth.
        let failed = outcome.is_err() || reply.was_abandoned();
        // The answer goes out whether or not anyone is listening: a caller that has
        // gone cannot be told, but the bytes have moved either way, and the poison
        // below is what makes the next caller read the file back rather than trust a
        // view that may be behind it.
        reply.publish(outcome);
        if failed {
            // A failure inside the ordered step may have moved a prefix of the
            // bytes, so the handle cannot say what is on the disk and refuses every
            // later append until a reopen replays the truth.
            lock(&slot).poisoned = true;
        }
    }
    drop(file);
    drop(state);
    let mut held = lock(&slot);
    held.released = true;
    signal.notify_all();
}

/// Take the next job, or `None` once the handle has closed and the queue is drained.
///
/// The wait is a poll rather than a blocking receive, and that is deliberate: a
/// blocking receive would hold the slot across the wait, which would make the stall
/// gate — the one thing an operator reaches for while a flush is parked — invisible
/// to the thread that has to answer it. A one-millisecond poll costs nothing next to
/// an `fsync` and keeps every state change observable.
fn next<S, A>(
    slot: &Mutex<Slot<S, A>>,
    gate: &Mutex<Gate>,
    signal: &Condvar,
) -> Option<Envelope<S, A>> {
    let mut held = lock(slot);
    loop {
        // A parked device holds the request back, and the gate is what releases
        // it. This is checked *before* a request is taken, so a stalled device
        // parks the append at the door rather than after the owner has claimed
        // it — which is what an `fsync` that has not answered looks like, and
        // what makes the caller's wait observable to the rest of the runtime.
        //
        // The wait is timed rather than indefinite so a handle that closes during
        // a stall still ends the loop, and so a release arriving between requests
        // is not missed.
        while lock(gate).stalled && !held.closed {
            let (guard, _timed_out) = signal
                .wait_timeout(held, Duration::from_millis(1))
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            held = guard;
        }
        if let Some(envelope) = held.pending.pop_front() {
            return Some(envelope);
        }
        if held.closed {
            // Closed and empty: drained. A request admitted just before the last
            // handle went is the one `pop_front` above took, so nothing is
            // stranded on the way out.
            return None;
        }
        let (guard, _timed_out) = signal
            .wait_timeout(held, Duration::from_millis(1))
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        held = guard;
    }
}
