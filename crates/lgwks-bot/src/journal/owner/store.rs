//! The storage owner thread itself: the bounded rings, the group commit and the
//! poison latch the parent module documents.
//!
//! Split from the poison-safe lock helpers beside it, which the whole crate
//! uses: this half exists only with `wire`, the encoding both file-backed
//! stores persist through, so a process-only build compiles none of it (#687).

use std::collections::VecDeque;
use std::fs::File;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::task::Waker;
use std::time::Duration;

use lgwks_std::task::{JoinHandle, block_on, spawn_blocking};

use super::{Gate, StorageGate, lock, wait, wait_timeout};

/// The most requests one store may have outstanding at the owner.
///
/// A journal asks for one, because `&mut self` already admits one. A run store's
/// append is `&self` and many runs reach it, so it asks for more; the number is a
/// bound, not a hint, and a caller that arrives at a full ring waits for room
/// rather than growing it.
pub(crate) const DEFAULT_QUEUE_DEPTH: usize = 64;

/// The most submissions that may wait for room in the request ring.
///
/// The second half of the backpressure: [`DEFAULT_QUEUE_DEPTH`] bounds the ring
/// the owner drains, and this bounds the ring of submitters waiting to get into
/// it. Both are bounded because both grow with callers — a run store's append is
/// `&self`, so any number of steps may reach it — and a wait that is itself
/// unbounded is a queue that never says it is full.
pub(crate) const MAX_WAITING_SUBMITTERS: usize = 64;

/// The most records one group commit stages and syncs together.
///
/// A ceiling on how much of a caller's record set is held under a single answer,
/// not a target: the owner drains whatever is queued when it wakes and stops at
/// whichever of the two batch ceilings it reaches first. A store whose per-record
/// frame is larger than [`MAX_BATCH_BYTES`] still stages one record, because a
/// record already written is a record whose answer is owed.
pub(crate) const MAX_BATCH_RECORDS: usize = 64;

/// The most bytes one group commit stages before it stops taking records.
///
/// A ceiling on the buffer one flush carries, checked against the staged total as
/// records go in. One record may take the batch past it — see the module's note
/// that a staged record is already written — so a batch's staged total is bounded
/// by this plus the largest frame any single store admits.
pub(crate) const MAX_BATCH_BYTES: usize = 256 * 1024;

/// One durable request's ordered step, as the storage owner runs it.
///
/// Generic in both because both are the caller's: a journal's request returns no
/// answer and has no state to fold, and a run store's returns which of its three
/// record outcomes happened and holds the index it settles into. Parameterising
/// the owner in both is what lets one owner serve both without either store
/// naming the other's vocabulary.
pub(crate) type Job<S, A> =
    Box<dyn FnOnce(&mut File, &mut S) -> Result<Stage<A, S>, std::io::Error> + Send>;

/// What one ordered step owes the batch after it returns.
///
/// The two-phase split that group commit is made of. `Settled` is a step that
/// moved no byte — a refusal, or an answer the index already held — and owes
/// nothing. `Unsynced` is a step whose bytes are on the file and whose answer is
/// owed one `sync_all` covering it and every other `Unsynced` member of its batch.
/// `Committed` is a step that performed its own durability inside the ordered step
/// and owes the batch nothing.
pub(crate) enum Stage<A, S> {
    /// The step wrote nothing, so its answer owes no flush. The answer is itself
    /// a `Result`, because a refusal is an answer and is answered the moment the
    /// step returns rather than at the end of a batch it does not belong to.
    Settled(Result<A, std::io::Error>),
    /// The step's bytes are on the file; the answer and the fold are owed the
    /// batch's one `sync_all`.
    Unsynced {
        /// The answer, published only after the covering sync returns `Ok`.
        answer: A,
        /// How many bytes this record added, charged against [`MAX_BATCH_BYTES`].
        bytes: usize,
        /// Fold the record into the caller's state, in submission order, once the
        /// covering sync has returned. Not run when that sync fails, which is what
        /// keeps the index from claiming bytes the device never took.
        settle: Box<dyn FnOnce(&mut S) + Send>,
    },
    /// The step wrote its bytes and flushed them itself, inside the ordered step,
    /// so its answer is ready and owes this batch no flush. The run ledger's
    /// charge is the one caller (`script`): its correctness rests on deciding,
    /// writing and folding under one lock before it returns — a later member of the
    /// same batch must decide against the fold this one produced — so its fold
    /// cannot be deferred to a batch settle the way a step record's can.
    #[cfg(feature = "script")]
    Committed(A),
}

impl<A, S> Stage<A, S> {
    /// How many bytes this step put on the file.
    ///
    /// A committed step's bytes are not carried by this batch — it already flushed
    /// them itself — so they are not charged against this batch's byte ceiling.
    fn staged_bytes(&self) -> usize {
        match *self {
            Self::Settled(_) => 0,
            #[cfg(feature = "script")]
            Self::Committed(_) => 0,
            Self::Unsynced { bytes, .. } => bytes,
        }
    }

    /// Whether this step's answer is owed the batch's one flush.
    fn owes_flush(&self) -> bool {
        matches!(self, Self::Unsynced { .. })
    }
}

/// One durable request and the slot its answer travels through.
///
/// The request travels with its own reply rather than beside it, because a
/// submission can be parked in the waiting ring before the owner has taken it and
/// the two must be promoted together or not at all.
struct Request<S, A> {
    /// The ordered step, taken by the owner when it stages this request.
    job: Mutex<Option<Job<S, A>>>,
    /// The answer, once the owner has written it.
    slot: Mutex<Option<Result<A, std::io::Error>>>,
    /// Woken when the answer is written.
    arrived: Condvar,
    /// The task to wake, when the caller is awaiting rather than blocking.
    waker: Mutex<Option<Waker>>,
    /// Whether the caller walked away before the answer arrived.
    abandoned: Mutex<bool>,
}

impl<S, A> Request<S, A> {
    /// An empty request slot, carrying no step yet.
    fn new() -> Self {
        Self {
            job: Mutex::new(None),
            slot: Mutex::new(None),
            arrived: Condvar::new(),
            waker: Mutex::new(None),
            abandoned: Mutex::new(false),
        }
    }

    /// Hand the ordered step to whoever stages this request.
    fn arm(&self, job: Job<S, A>) {
        *lock(&self.job) = Some(job);
    }

    /// Take the ordered step, or report that it is not there.
    ///
    /// The second arm is unreachable by construction: the owner only reaches a
    /// request through a ring it pushed with the step armed. `Result` rather than a
    /// panic because the owner must not be the thing that decides a caller's
    /// request was malformed.
    fn take_job(&self) -> Result<Job<S, A>, std::io::Error> {
        lock(&self.job)
            .take()
            .ok_or_else(|| std::io::Error::other("the storage owner was handed an unarmed request"))
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
        self.wake();
    }

    /// Wake the awaiting caller without an answer, which is how a request promoted
    /// out of the waiting ring learns it no longer has to wait for room.
    fn wake(&self) {
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
            held = wait(&self.arrived, held);
        }
    }
}

/// The two bounded rings, the poison the owner latches, and the flags a caller
/// changes while the owner is busy.
struct Slot<S, A> {
    /// Requests admitted and not yet staged, in arrival order.
    ///
    /// A bounded ring rather than a channel, and the bound is what makes it one.
    /// The estate's channel (`rt::sync::mpsc`) is the right choice *with a
    /// runtime*, and this queue deliberately has none: the owner thread outlives
    /// every future that ever waits on it, so it must not depend on a runtime
    /// being alive to be driven. `std::sync::mpsc` is the alternative and is
    /// disallowed workspace-wide (INV-RT-BOUNDED) precisely because it has no
    /// bounded form. A `VecDeque` with a checked push is the bound written out,
    /// and it costs one lock the owner already holds.
    pending: VecDeque<Arc<Request<S, A>>>,
    /// Requests that found [`pending`] full and are waiting for room.
    ///
    /// The second half of the backpressure. They are promoted into `pending` by
    /// the owner as it drains, in arrival order, so the order a submitter was
    /// admitted in survives the wait.
    waiting: VecDeque<Arc<Request<S, A>>>,
    /// Whether bytes may be on the disk that no acknowledgment names.
    poisoned: bool,
    /// Whether the handle is gone and the owner should finish and exit.
    closed: bool,
    /// Whether the owner has dropped the file.
    released: bool,
    /// Whether the next request should report a device refusal instead of writing.
    fail_next: bool,
    /// Whether the next batch's covering `sync_all` should report a refusal
    /// instead of syncing.
    ///
    /// Separate from [`fail_next`][Self::fail_next], which refuses one request's
    /// ordered step before it writes anything. This one fails the *whole batch's*
    /// flush, which is the failure that must answer every member of the batch at
    /// once — the one fact group commit can get wrong and a per-request switch
    /// cannot exercise.
    fail_next_flush: bool,
    /// How many `sync_all` calls this owner has made.
    ///
    /// The observable that says whether a batch formed: one per flush, so
    /// `flushes / acknowledged` is the fsyncs per record the mechanism actually
    /// paid. A caller reads it through its store, so the number is measured rather
    /// than inferred from a latency difference.
    flushes: u64,
    /// How many records the owner has staged since it started.
    ///
    /// The denominator for the ratio above, counted where the records are actually
    /// staged rather than by the callers that asked for them — a caller that gave
    /// up is still a staged record, and leaving it out would report a better
    /// batching factor than the store really achieved.
    staged_records: u64,
}

impl<S, A> Slot<S, A> {
    /// Whether a request may be admitted, or why it may not.
    fn admission(&self) -> Admission {
        if self.poisoned {
            Admission::Poisoned
        } else if self.pending.len() < DEFAULT_QUEUE_DEPTH {
            Admission::Admitted
        } else if self.waiting.len() < MAX_WAITING_SUBMITTERS {
            Admission::Waiting
        } else {
            Admission::Refused
        }
    }

    /// Whether anything is left for the owner to do.
    fn has_work(&self) -> bool {
        !self.pending.is_empty() || !self.waiting.is_empty()
    }
}

/// How one submission was admitted, or why it was not.
enum Admission {
    /// Straight into the ring the owner drains.
    Admitted,
    /// Parked in the waiting ring until the pending ring drains.
    Waiting,
    /// A previous request's outcome is unknown, so this handle cannot append.
    Poisoned,
    /// Both rings are full: the caller is told rather than made to wait without
    /// limit.
    Refused,
}

/// `Slot::default` by hand rather than derived.
///
/// The derived one would demand `S: Default` and `A: Default` for a struct that
/// stores neither: `S` and `A` appear only inside the rings' requests, which
/// start empty. Requiring the caller's state to be constructible just to open an
/// owner would be a bound neither store can satisfy.
impl<S, A> Default for Slot<S, A> {
    fn default() -> Self {
        Self {
            pending: VecDeque::new(),
            waiting: VecDeque::new(),
            poisoned: false,
            closed: false,
            released: false,
            fail_next: false,
            fail_next_flush: false,
            flushes: 0,
            staged_records: 0,
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
    /// The poison latch, the stall gate and the two bounded rings.
    slot: Arc<Mutex<Slot<S, A>>>,
    /// The same latch as its own type, so a gate outlives the store's generics.
    gate: Arc<Mutex<Gate>>,
    /// Woken on every state change, so a blocking drop and the owner's own wait
    /// both re-read rather than spin.
    signal: Arc<Condvar>,
    /// The owner thread. Closing the slot is its cancellation, and
    /// [`StorageOwner::drop`] joins it once it reports the file released, so no
    /// owner thread outlives the handle that started it.
    task: Option<JoinHandle<()>>,
}

/// Why an append did not reach the device, or reached it without an answer.
///
/// Three refusals and one device error, kept apart because the repair differs per
/// arm: a poison is fixed by reopening, a full pair of rings by retrying later, an
/// undelivered answer by reading the file back, and a device error by whatever the
/// device says.
#[derive(Debug)]
pub(crate) enum SubmitError {
    /// A previous request's outcome is unknown, so this handle cannot append
    /// again. Only a reopen, which replays the truth, clears it.
    Poisoned,
    /// Both bounded rings were full: more requests are outstanding than the two
    /// declared ceilings admit. Nothing was written, and the caller may retry.
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
        let (owned_slot, owned_gate, owned_signal) =
            (Arc::clone(&slot), Arc::clone(&gate), Arc::clone(&signal));
        let task = spawn_blocking(move || serve(file, state, owned_slot, owned_gate, owned_signal));
        Ok(Self {
            slot,
            gate,
            signal,
            task: Some(task),
        })
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

    /// How many `sync_all` calls this owner has made, and how many records it has
    /// staged since it started.
    ///
    /// Read through [`crate::task::RunStore::flush_counts`], which is the only
    /// caller: the counter exists to be measured from outside the crate, and a
    /// feature set with no run store has no caller to be measured by.
    #[cfg(feature = "script")]
    pub(crate) fn flush_counts(&self) -> (u64, u64) {
        let held = lock(&self.slot);
        (held.flushes, held.staged_records)
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

    /// Make the next batch's covering `sync_all` report a refusal instead of
    /// syncing.
    ///
    /// The flush failure is a different fault from [`fail_next_commit`][Self::fail_next_commit]:
    /// that one refuses a single request's ordered step before it writes, while
    /// this one fails the *whole batch's* one flush after every member's bytes are
    /// already on the file. That is the failure group commit must answer
    /// all-or-nothing, and no filesystem produces it on demand. Its only caller is
    /// the store's test module, so it is gated with the store it serves.
    #[cfg(all(test, feature = "script"))]
    pub(crate) fn fail_next_flush(&self) {
        lock(&self.slot).fail_next_flush = true;
    }

    /// Perform `job` on the owner thread and wait for it to finish.
    ///
    /// Blocking, for a caller on a thread with nothing to await. The awaited form is
    /// [`StorageOwner::submit_async`], which the durable path uses.
    ///
    /// # Errors
    ///
    /// Whatever `job` reports, plus both rings being full: an append refused for
    /// want of room anywhere is a typed refusal, never a silent wait.
    pub(crate) fn submit<F>(&self, job: F) -> Result<A, SubmitError>
    where
        F: FnOnce(&mut File, &mut S) -> Result<Stage<A, S>, std::io::Error> + Send + 'static,
    {
        let reply = Arc::new(Request::<S, A>::new());
        self.enqueue(Box::new(job), Arc::clone(&reply))?;
        match reply.wait_blocking() {
            Some(outcome) => outcome.map_err(SubmitError::Device),
            None => Err(SubmitError::OutcomeUnknown),
        }
    }

    /// The same door, for a caller that awaits rather than blocks.
    ///
    /// The difference is only in how the answer is delivered: a blocking caller
    /// waits on the request's slot, a task awaits it and is woken by the owner when
    /// the answer lands — and when the waiting ring is full enough to park it, by
    /// the owner when it is promoted back. The append itself is the same ordered
    /// step either way, so a caller choosing to await does not get a weaker write
    /// than one that chose to sit through it.
    pub(crate) fn submit_async<F>(&self, job: F) -> crate::BoxFuture<'_, Result<A, SubmitError>>
    where
        F: FnOnce(&mut File, &mut S) -> Result<Stage<A, S>, std::io::Error> + Send + 'static,
    {
        let awaiting = self.enqueue_awaiting(job);
        Box::pin(awaiting)
    }

    /// [`StorageOwner::submit_async`] without the type erasure, for a caller whose
    /// own future must be `Send`.
    ///
    /// The erased form boxes into [`crate::BoxFuture`], which is deliberately not
    /// `Send`: a durable step awaits it from inside a task body, and a body is
    /// `Send` exactly when its author made it so. A caller on the *host's* own
    /// path has no such choice — its future must be `Send` whatever the author's
    /// body is, because the host may drive it on a multi-threaded runtime. The
    /// concrete [`Awaiting`] is `Send` whenever `A` is, so returning it unboxed is
    /// what keeps the host's path `Send` without weakening the erasure a step
    /// body wants.
    ///
    /// The job is the same two-phase [`Stage`] step [`StorageOwner::submit`] runs;
    /// a caller that performs its own durability inside the ordered step answers
    /// [`Stage::Committed`] and owes the batch no flush.
    pub(crate) fn enqueue_awaiting<F>(&self, job: F) -> Awaiting<S, A>
    where
        F: FnOnce(&mut File, &mut S) -> Result<Stage<A, S>, std::io::Error> + Send + 'static,
    {
        let reply = Arc::new(Request::<S, A>::new());
        let outcome = self.enqueue(Box::new(job), Arc::clone(&reply));
        Awaiting {
            enqueued: Some(outcome),
            reply,
        }
    }

    /// Hand one request to the owner, or report that this handle may not append.
    ///
    /// The bound is backpressure rather than a refusal for the common case: a
    /// submitter that finds the draining ring full is parked in the waiting ring
    /// and is woken when the owner promotes it, so the memory the caller holds
    /// while it waits is one [`Request`] against a declared ceiling. Only a
    /// submitter that has outrun *both* rings is told so immediately, because
    /// queueing without limit is how a parked device turns into unbounded memory.
    fn enqueue(&self, job: Job<S, A>, reply: Arc<Request<S, A>>) -> Result<(), SubmitError> {
        let mut held = lock(&self.slot);
        match held.admission() {
            Admission::Poisoned => Err(SubmitError::Poisoned),
            Admission::Refused => Err(SubmitError::QueueFull),
            Admission::Admitted => {
                reply.arm(job);
                held.pending.push_back(Arc::clone(&reply));
                drop(held);
                self.signal.notify_all();
                Ok(())
            }
            Admission::Waiting => {
                reply.arm(job);
                held.waiting.push_back(Arc::clone(&reply));
                drop(held);
                self.signal.notify_all();
                Ok(())
            }
        }
    }
}

/// One request handed to the owner, waiting for its answer.
///
/// A named future rather than an inline `async` block because its `poll` has to
/// re-check the slot on every wake, and an `async` block's body would run once and
/// then park — which is exactly the shape that would need a second await to notice
/// the answer. Each poll registers its waker *before* it reads the slot, because a
/// wake is only ever fired once: a poll that read first and registered second
/// could miss the one wake the owner sends and park for ever.
pub(crate) struct Awaiting<S, A> {
    /// Whether the request reached the owner at all. An error here is the queue's,
    /// and there is nothing to wait for. Taken on the first poll only.
    enqueued: Option<Result<(), SubmitError>>,
    /// Where the owner will write, and where this poll reads.
    reply: Arc<Request<S, A>>,
}

impl<S, A> std::future::Future for Awaiting<S, A> {
    type Output = Result<A, SubmitError>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.get_mut();
        if let Some(Err(cause)) = this.enqueued.take() {
            return std::task::Poll::Ready(Err(cause));
        }
        // Register first, then look. The owner writes the slot and only then takes
        // the waker, so whichever side moves second sees the other: a publish that
        // takes the waker after this registration wakes this poll, and one that
        // took it before had already written the slot, which the look below reads.
        // Looking first and registering after is a lost wakeup — a publish landing
        // between the two finds no waker to fire and writes an answer this poll
        // has already decided is not there, so the task parks for ever.
        *lock(&this.reply.waker) = Some(cx.waker().clone());
        if let Some(answer) = this.reply.take() {
            return std::task::Poll::Ready(answer.map_err(SubmitError::Device));
        }
        std::task::Poll::Pending
    }
}

impl<S, A> Drop for Awaiting<S, A> {
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
            slot = wait(&self.signal, slot);
        }
        drop(slot);
        // Released is the owner's last act before it returns, so this join waits
        // for a return already under way and never for the device.
        if let Some(task) = self.task.take() {
            block_on(task);
        }
    }
}

/// The owner's loop: stage a batch, sync it once, answer everyone in it.
///
/// The file and the caller's state are dropped before `released` is set, so a
/// waiter that observes the release knows everything is already gone rather than
/// about to be. The loop ends when the handle has closed *and* both rings have
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
    while run_batch(&mut file, &mut state, &slot, &gate, &signal) {}
    drop(file);
    drop(state);
    let mut held = lock(&slot);
    held.released = true;
    signal.notify_all();
}

/// One staged member of a batch: the request it belongs to, and what it owes.
///
/// A named pair rather than a tuple because the two halves are read apart — the
/// batch counts one, and each member's answer takes both — and a tuple would make
/// a reader remember which element is which at every use.
struct Member<S, A> {
    /// Where this member's answer goes.
    request: Arc<Request<S, A>>,
    /// What its ordered step staged.
    staged: Stage<A, S>,
}

/// Stage one batch, sync it once, and answer it. `false` once there is nothing left.
///
/// Three phases, in this order and never another:
///
/// 1. **Stage.** Take requests from the front of the pending ring and run each
///    ordered step in arrival order. Every byte is on the file by the end of this
///    phase, and every step that owes a flush owes it *to this batch*.
/// 2. **Sync.** One `sync_all` if anything is owed, covering every record staged
///    above. There is no linger before it and none after it.
/// 3. **Answer.** Publish in arrival order: a settled answer as it stands, an
///    unsynced one only after the sync returned, and the folds in the same order.
///
/// The poison latch is written *before* any answer of a failed batch goes out: a
/// caller woken by a failure may append again at once and must find the handle
/// already refusing.
fn run_batch<S, A>(
    file: &mut File,
    state: &mut S,
    slot: &Mutex<Slot<S, A>>,
    gate: &Mutex<Gate>,
    signal: &Condvar,
) -> bool {
    promote(&mut lock(slot));
    let mut batch: Vec<Member<S, A>> = Vec::new();
    let mut staged_bytes = 0usize;
    loop {
        // The stall gate is checked before a request is taken, so a parked device
        // parks the append at the door rather than after the owner has claimed it.
        // Only a *first* record waits: a batch already holding staged bytes owes
        // them a flush, and waiting for company would be exactly the linger this
        // design refuses.
        if batch.is_empty() && !await_work(slot, gate, signal) {
            break;
        }
        if batch.len() >= MAX_BATCH_RECORDS || staged_bytes >= MAX_BATCH_BYTES {
            break;
        }
        // An empty ring means this batch is complete, not that it should be
        // retried: only this thread takes requests out of it, so an empty one
        // cannot fill again under us. An empty *batch* reaching here would mean
        // `await_work` reported work that is not there, which the owner cannot
        // recover from by waiting and must not report as a record.
        let Some(request) = take(slot) else {
            break;
        };
        let outcome = match request.take_job() {
            Err(cause) => Err(cause),
            Ok(job) => {
                let injected = {
                    let mut held = lock(slot);
                    std::mem::take(&mut held.fail_next)
                };
                if injected {
                    Err(std::io::Error::other("the injected device refusal"))
                } else {
                    job(file, state)
                }
            }
        };
        let staged = match outcome {
            Ok(staged) => staged,
            // A failure inside the ordered step may have moved a prefix of the
            // bytes, so the handle cannot say what is on the disk and refuses every
            // later append until a reopen replays the truth. The record still needs
            // an answer, and the answer is the same failure.
            Err(cause) => Stage::Settled(Err(cause)),
        };
        staged_bytes = staged_bytes.saturating_add(staged.staged_bytes());
        {
            let mut held = lock(slot);
            held.staged_records = held.staged_records.saturating_add(1);
        }
        batch.push(Member { request, staged });
    }

    if batch.is_empty() {
        return false;
    }

    // One flush for the batch. A member that owes nothing does not make the batch
    // sync; a member whose flush fails takes the same failure as every other, and
    // none of them is answered.
    let owes = batch.iter().any(|member| member.staged.owes_flush());
    let sync = if owes {
        let injected = {
            let mut held = lock(slot);
            std::mem::take(&mut held.fail_next_flush)
        };
        if injected {
            // A test refusal of the covering flush: the one failure no filesystem
            // produces on demand, and the failure that must answer the whole batch
            // at once. No `sync_all` runs, so no flush is counted.
            Err(std::io::Error::other("the injected flush refusal"))
        } else {
            let outcome = file.sync_all();
            let mut held = lock(slot);
            held.flushes = held.flushes.saturating_add(1);
            outcome
        }
    } else {
        Ok(())
    };

    let mut failed = false;
    // The flush's own failure kind, carried to each unsynced member, or `None`
    // for a flush that returned: no kind is made up for a flush that succeeded.
    let flush_failure = match sync.as_ref() {
        Ok(&()) => None,
        Err(cause) => Some(cause.kind()),
    };
    for Member { request, staged } in batch {
        // A failure anywhere in the batch poisons the handle, so the latch is
        // raised as each member is answered rather than once at the end: a caller
        // woken by the first failure may append again at once and must find the
        // handle already refusing.
        let answer = match (staged, flush_failure) {
            (Stage::Settled(answer), _) => answer,
            (Stage::Unsynced { answer, settle, .. }, None) => {
                // The fold runs only on a flush that returned, and in submission
                // order, so a store's index cannot name a record the device never
                // took.
                settle(state);
                Ok(answer)
            }
            (Stage::Unsynced { .. }, Some(kind)) => Err(std::io::Error::new(
                kind,
                "the batch's flush failed, so this record's outcome is unknown",
            )),
            // The step flushed its own bytes inside the ordered step, so its answer
            // is ready whether or not this batch syncs.
            #[cfg(feature = "script")]
            (Stage::Committed(answer), _) => Ok(answer),
        };
        if answer.is_err() {
            failed = true;
        }
        // A caller that went away while the bytes were moving leaves the handle
        // unable to say whether they landed, which is the same position a failed
        // write leaves it in: poisoned until a reopen replays the truth.
        if request.was_abandoned() {
            failed = true;
        }
        if failed {
            lock(slot).poisoned = true;
        }
        // The answer goes out whether or not anyone is listening: a caller that has
        // gone cannot be told, but the bytes have moved either way, and the poison
        // above is what makes the next caller read the file back rather than trust
        // a view that may be behind it.
        request.publish(answer);
    }
    true
}

/// Move waiting requests into the ring the owner drains, in arrival order.
///
/// Bounded by the pending ring's own ceiling: a promotion that would overfill it is
/// deferred to the next drain rather than raising the bound.
fn promote<S, A>(held: &mut MutexGuard<'_, Slot<S, A>>) {
    while held.pending.len() < DEFAULT_QUEUE_DEPTH {
        let Some(request) = held.waiting.pop_front() else {
            break;
        };
        request.wake();
        held.pending.push_back(request);
    }
}

/// Take one request from the front of the pending ring, or `None` if it is empty.
fn take<S, A>(slot: &Mutex<Slot<S, A>>) -> Option<Arc<Request<S, A>>> {
    lock(slot).pending.pop_front()
}

/// Wait until there is work, or report that there never will be again.
///
/// The wait is a poll rather than a blocking receive, and that is deliberate: a
/// blocking receive would hold the slot across the wait, which would make the stall
/// gate — the one thing an operator reaches for while a flush is parked —
/// invisible to the thread that has to answer it. A one-millisecond poll costs
/// nothing next to an `fsync` and keeps every state change observable.
fn await_work<S, A>(slot: &Mutex<Slot<S, A>>, gate: &Mutex<Gate>, signal: &Condvar) -> bool {
    let mut held = lock(slot);
    loop {
        // A parked device holds the request back, and the gate is what releases
        // it. The wait is timed rather than indefinite so a handle that closes
        // during a stall still ends the loop, and so a release arriving between
        // batches is not missed.
        while lock(gate).stalled && !held.closed {
            let (guard, _timed_out) = wait_timeout(signal, held, Duration::from_millis(1));
            held = guard;
        }
        promote(&mut held);
        if held.has_work() {
            return true;
        }
        if held.closed {
            // Closed and empty: drained. A request admitted just before the last
            // handle went is the one `take` above took, so nothing is stranded on
            // the way out.
            return false;
        }
        let (guard, _timed_out) = wait_timeout(signal, held, Duration::from_millis(1));
        held = guard;
    }
}

#[cfg(test)]
mod tests;
