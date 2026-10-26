//! The one thread that owns a file-backed store's device.
//!
//! Two stores in this crate write a disk: the effect journal and the run store.
//! Both made the same mistake before this module existed — `write_all` and
//! `sync_all` reached the device from the thread that awaited the append, so the
//! device's latency became the process's latency. A heartbeat, a timer and every
//! unrelated task in the same runtime stalled for the length of an `fsync`. That
//! is the defect #122 removed from [`FileJournal`] and that
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
//! fence and the write it guards un-overtakable — which is the property both
//! stores actually rest on.
//!
//! It is also what makes an abandoned await safe. A caller that walks away from a
//! record it asked for cannot un-write the bytes or un-fold the index, and needs
//! no repair: the step's record either landed and will be replayed, or never
//! started, and both are states a resume already knows how to read.
//!
//! # Group commit: one `sync_all` for a batch, not for a record
//!
//! A store that acknowledged every record behind its own `sync_all` runs at the
//! device's flush rate. Under concurrency that is the whole ceiling: N steps that
//! finish together pay N flushes where the device would have carried them in one.
//! So an ordered step here is two phases. The **stage** phase is everything that
//! must be ordered against the other members — the in-memory checks, the length
//! fence, the framing, and the `write_all` — and it runs per record, in submission
//! order. The **settle** phase is one `sync_all` for the whole batch, followed by
//! the folds, also in submission order. An answer is published only after the
//! `sync_all` that covers its bytes has returned, so nothing is acknowledged
//! early; and a failure acknowledges *nobody* in the batch, so the poison is
//! latched once for all of them rather than per record.
//!
//! Three properties of that split are load-bearing and are stated here because
//! the code cannot show them:
//!
//! * The **layout order is the submission order.** The stage phase is per record
//!   and strictly ordered, so the frames land in the order the ring admitted them
//!   regardless of how they were grouped into flushes.
//! * The **fold happens after the sync, never before.** The settle closure a
//!   stage hands back is *not* run when the covering `sync_all` fails, which is
//!   what keeps the index from claiming a record whose bytes never reached the
//!   device. That is the same position an un-synced single append leaves today.
//! * **A record's bytes are already in the file when it is staged.** So the byte
//!   ceiling below bounds a batch's staged total to one record over the ceiling,
//!   and that one record is bounded by the store's own per-record ceiling. There
//!   is no path here that stages a record it will not answer.
//!
//! # Latency at low load
//!
//! There is no linger. The owner drains whatever the ring holds *at the moment it
//! wakes*, stages it, and syncs once — so a lone append stages one record and pays
//! exactly one `sync_all`, with no timer to wait out. Grouping happens because
//! work was already queued, never because the owner decided to wait for more.
//!
//! # The bound
//!
//! Three declared ceilings, all `usize`, all enforced rather than asserted:
//! [`store::MAX_BATCH_RECORDS`] and [`store::MAX_BATCH_BYTES`] cap one flush's
//! work, and [`store::DEFAULT_QUEUE_DEPTH`] plus
//! [`store::MAX_WAITING_SUBMITTERS`] cap the two rings the
//! requests pass through. A submitter that arrives at a full ring **waits for
//! room** rather than growing it: the request is parked in the waiting ring and
//! the owner promotes it as the pending ring drains. Only a submitter that
//! outruns *both* rings is refused, with the typed [`SubmitError::QueueFull`].
//!
//! A journal serialises its appends through `&mut self`, so one request at a time
//! is already all it can have; a run store's append takes `&self` and many runs
//! reach it at once, so both rings matter there.

#[cfg(feature = "wire")]
use std::sync::Arc;
#[cfg(any(feature = "wire", feature = "ecs", all(test, feature = "rt")))]
use std::sync::Condvar;
use std::sync::{Mutex, MutexGuard};
#[cfg(feature = "script")]
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
#[cfg(any(feature = "wire", feature = "ecs", all(test, feature = "rt")))]
use std::time::Duration;

/// The owner thread, its rings and its ceilings. Only the two file-backed
/// stores run one, and both persist through `wire`, so it exists with `wire`.
#[cfg(feature = "wire")]
mod store;

#[cfg(feature = "wire")]
pub(crate) use store::{Stage, StorageOwner, SubmitError};

/// Take the lock, treating a poisoned one as recoverable.
///
/// Nothing in either store's append path can panic while the lock is held — every
/// arm is a `Result` that propagates — so a poison is a bug in an unrelated thread,
/// and refusing every later append because of it would turn one failure into a
/// bricked store.
///
/// The crate's one lock site, not this module's: a mutex in `script`, in `task` or
/// in a domain's own fixture guards the same kind of plain data under the same
/// "no arm panics" argument, and a second copy of the recovery is a second place
/// for that argument to go stale.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Read `lock`, recovering the guard when a previous holder panicked.
///
/// The read half of [`lock`], for the same reason: a shared index whose values
/// have no `Drop` that can fail cannot be left half-written by a panic, and a
/// reader should not be turned away by a panic in an unrelated writer.
///
/// `script`-gated because the crate's `RwLock` user is the proposal artifact
/// shelf, which sits on that feature; a build without it has no shared index.
#[cfg(feature = "script")]
pub(crate) fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    match lock.read() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Write `lock`, recovering the guard when a previous holder panicked.
///
/// The exclusive half of [`lock`], for the same reason.
#[cfg(feature = "script")]
pub(crate) fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    match lock.write() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Wait on `condvar`, recovering the guard when a previous holder panicked.
///
/// The same recovery [`lock`] makes, for the waits that follow it. A poisoned wait
/// still hands back the guard: the panic belonged to a *previous* holder, and every
/// caller here re-reads the state it waits on inside its own loop, so a value left
/// by a panic is read and judged rather than assumed.
#[cfg(feature = "wire")]
pub(crate) fn wait<'a, T>(condvar: &Condvar, held: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
    match condvar.wait(held) {
        Ok(woke) => woke,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Wait on `condvar` for at most `bound`, recovering the guard from a panic.
///
/// The timed form of [`wait`], saying the same thing for the same reason. The
/// timeout verdict is the caller's to read and this hands back the one the call
/// itself produced, so a poisoned wait does not also invent a verdict it never
/// observed.
#[cfg(any(feature = "wire", feature = "ecs"))]
pub(crate) fn wait_timeout<'a, T>(
    condvar: &Condvar,
    held: MutexGuard<'a, T>,
    bound: Duration,
) -> (MutexGuard<'a, T>, std::sync::WaitTimeoutResult) {
    match condvar.wait_timeout(held, bound) {
        Ok((woke, verdict)) => (woke, verdict),
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Wait on `condvar` until `satisfied` holds or `bound` elapses, recovering the
/// guard from a panic.
///
/// The predicate form of [`wait_timeout`], and the recovery is the same for the
/// same reason: the guard comes back either way, and the caller's own predicate
/// is what decides when it stops waiting.
///
/// Test-only, because that is the only shape that needs it: the shipped owner
/// waits with [`wait`] and [`wait_timeout`], neither of which takes a closure.
/// A fixture's doorbell does open on a bounded predicate, and this keeps its
/// recovery the same one rather than a second copy beside the other three.
#[cfg(all(test, any(feature = "wire", feature = "rt")))]
pub(crate) fn wait_timeout_while<'a, T, F>(
    condvar: &Condvar,
    held: MutexGuard<'a, T>,
    bound: Duration,
    satisfied: F,
) -> (MutexGuard<'a, T>, std::sync::WaitTimeoutResult)
where
    F: FnMut(&mut T) -> bool,
{
    match condvar.wait_timeout_while(held, bound, satisfied) {
        Ok((woke, verdict)) => (woke, verdict),
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(feature = "wire")]
/// A handle onto one store's storage device, cloneable and independent of the store.
///
/// It exists because a caller awaiting an append cannot reach the store to release
/// it: the future holds the handle's borrow for exactly as long as the append is
/// outstanding, which is the window in which the device most needs to be unstuck. An
/// operator un-sticking a real device is in the same position — their handle is busy
/// on the append that is waiting.
#[derive(Clone)]
pub struct StorageGate {
    /// The stall latch, held apart from the request rings.
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

#[cfg(feature = "wire")]
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

#[cfg(feature = "wire")]
impl StorageGate {
    /// Let a flush that is waiting for a release proceed, and every flush after it.
    /// The stall is not re-armed.
    pub fn release(&self) {
        lock(&self.latch).stalled = false;
        self.signal.notify_all();
    }
}
