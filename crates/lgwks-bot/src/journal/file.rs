//! The file-backed effect journal.
//!
//! [`FileJournal`] is the trait's second shipped adapter: the one whose
//! `ProcessCrash` promise is a claim a process kill can check, because the
//! bytes are on a disk before the acknowledgment is minted. Every append is
//! written and `sync_all`-ed before [`DurableAck`] is returned, so the
//! promise the acknowledgment carries is a statement about bytes that are
//! already through the file system, not about a `Vec` in the process that is
//! about to die.
//!
//! # Frame format
//!
//! One committed event is one frame: a `u32` big-endian payload length, the
//! event's archived bytes ([`EffectEvent::to_bytes`]), and the 32-byte chain
//! head the append committed to. The head is stored *with* the frame rather
//! than recomputed on faith, which is what separates the two failure shapes
//! a file can be found in after a crash:
//!
//! - A **torn tail** — a partial length prefix, or a short body or head at
//!   the end of the file — is an append that was interrupted before it could
//!   be acknowledged. A write leaves only a prefix of its bytes, so a torn
//!   tail is always a prefix cut short; it was never anyone's answer, so
//!   opening truncates back to the last whole frame and says so via
//!   [`FileJournal::torn_tail_repaired`]. This is the same disposition etcd's
//!   WAL gives a torn final record.
//! - A **lying frame** — a length prefix no writer of this journal can have
//!   completed, a payload that does not decode, or a stored head that does
//!   not follow from the events before it — is committed bytes that no longer
//!   mean what the chain says. It is refused with [`JournalError::Corrupt`],
//!   never trimmed, because the frame may have been acknowledged, and an
//!   acknowledgment the journal quietly rewrites is not a record.
//!
//! # Bounds
//!
//! One frame may not exceed [`MAX_FRAME_BYTES`]; a journal whose events were
//! always a key plus a verdict cannot approach it, so an over-long length
//! field is read as a torn write rather than as data. The scan is streaming:
//! the file's bytes are never loaded whole, and the loop is bounded by the
//! file's own length because every iteration consumes at least one byte. The
//! shipped adapter also retains the complete decoded history, so it refuses
//! files above [`MAX_JOURNAL_BYTES`] or [`MAX_JOURNAL_EVENTS`] rather than
//! truncate, compact, or partially replay them.
//!
//! # Concurrency
//!
//! The [`EffectJournal::compare_and_append`] fence is a fence over positions,
//! and this adapter adds a byte-length staleness check: an append from a view
//! that no longer matches the file is refused. What no std-only adapter can
//! provide is mutual exclusion between two live writers, because the platform's
//! advisory locks are outside `std`; concurrent controllers on one file remain
//! a caller obligation. The stored heads make any interleaving they produce
//! detectable on the next open rather than silently accepted, and detection
//! here is permanent: [`JournalError::Corrupt`] is never trimmed and no tool
//! in this module rewrites refused bytes, so a bricked file stays bricked
//! until an operator takes it in hand.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::task::Waker;

use super::{
    ChainBreak, DurabilityPromise, DurableAck, EffectEvent, EffectEvidence, EffectJournal,
    EventKind, JournalEntry, JournalError, JournalLimitKind, JournalPosition, MAX_JOURNAL_BYTES,
    MAX_JOURNAL_EVENTS, Recovered, chain, next_allowed_of, recover,
};
use lgwks_std::wire::{WireError, from_bytes};

/// The largest frame this journal will read or write.
///
/// Events are a key, a verdict and a digest; a real frame is a few hundred
/// bytes. A length field beyond this bound does not name a frame this
/// journal writes, so a complete prefix carrying one is refused as rot; a
/// partial prefix is a torn tail.
const MAX_FRAME_BYTES: usize = 64 * 1024;

/// The byte width of a stored chain head.
const HEAD_BYTES: usize = 32;

/// The byte width of a frame's length prefix.
const LENGTH_BYTES: usize = 4;

/// Why the journal refused its own committed bytes.
///
/// Distinct from a torn tail, and the distinction is the point: a torn tail
/// was never acknowledged and is repaired, while either arm here may have
/// been, and so is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CorruptionKind {
    /// The frame's bytes do not decode to an event.
    Undecodable,
    /// The frame's length prefix cannot be true of any frame this journal
    /// writes. A killed writer leaves only a prefix of its bytes, so a
    /// complete prefix that names an impossible frame is rot or a hand,
    /// never an interrupted append. The same holds for a length that is
    /// *possible* but lies: the frame's own stored head authenticates the
    /// bytes actually on the disk, and a tail that verifies as a complete
    /// frame under its true length was never interrupted.
    Framed,
    /// The stored head does not follow from the events before it.
    Chain(ChainBreak),
}

impl CorruptionKind {
    /// The human spelling, for the error chain.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Undecodable => "the frame's bytes do not decode to an event",
            Self::Framed => {
                "the frame's length prefix cannot be true of any frame this journal writes"
            }
            Self::Chain(_) => "the stored head does not follow from the events before it",
        }
    }
}

impl core::fmt::Display for CorruptionKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One refused frame, named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Corruption {
    /// The frame's index, counted from zero in file order.
    at: u64,
    /// Why it was refused.
    kind: CorruptionKind,
}

impl Corruption {
    /// Name a refused frame.
    #[must_use]
    pub const fn new(at: u64, kind: CorruptionKind) -> Self {
        Self { at, kind }
    }

    /// The frame's index, counted from zero in file order.
    #[must_use]
    pub const fn at(&self) -> u64 {
        self.at
    }

    /// Why the frame was refused.
    #[must_use]
    pub const fn kind(&self) -> &CorruptionKind {
        &self.kind
    }
}

impl core::fmt::Display for Corruption {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "frame {} corrupt: {}", self.at, self.kind)
    }
}

impl std::error::Error for Corruption {}

/// Which read failure a frame scan hit, and where.
#[derive(Debug)]
enum ScanStop {
    /// Clean end of file at the given offset.
    Complete(u64),
    /// The bytes from `offset` onward are an interrupted append: torn.
    Torn(u64),
    /// The file ended inside a frame whose declared length ran past the end.
    /// Either an append the writer never finished, or a complete frame whose
    /// length prefix lies — the two are byte-identical here, and
    /// [`FileJournal::open`] resolves which with the frame's own head.
    AmbiguousTail {
        /// Where the ambiguous frame's length prefix begins.
        offset: u64,
        /// The payload length the prefix declares.
        declared_len: usize,
    },
}

/// How one frame piece's read ended.
enum FramePiece {
    /// The buffer filled.
    Filled,
    /// The source ended before the buffer did: `read_exact`'s own evidence
    /// of an early end, which is the only thing a tail may be repaired on.
    Interrupted,
}

/// Read a fixed-size piece of a frame, classifying the ending.
///
/// Only a true early end — [`std::io::ErrorKind::UnexpectedEof`] — reads as
/// [`FramePiece::Interrupted`]. Every other error is the store refusing, and
/// travels unchanged: a fault is not evidence that the bytes after it were
/// never written, and treating it as one would authorize repair over bytes
/// that may be acknowledged.
fn read_exact_classified(
    reader: &mut impl Read,
    buf: &mut [u8],
) -> Result<FramePiece, JournalError> {
    match reader.read_exact(buf) {
        Ok(()) => Ok(FramePiece::Filled),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
            Ok(FramePiece::Interrupted)
        }
        Err(error) => Err(JournalError::Storage(error)),
    }
}

/// Decide what an end of file inside a declared frame means.
///
/// The scan read a legal length prefix, then hit the end of the file before
/// that many payload bytes arrived. Two different files are byte-identical
/// here: an append the writer never finished, and a complete frame whose
/// length prefix lies. The frame's own head decides. The bytes actually on
/// the disk — the tail minus a head — are tried as a frame: if they decode,
/// chain from the committed history, and reproduce the stored head exactly,
/// then a complete frame was already on the disk and its prefix lied, which
/// is rot or a hand, refused with the file untouched. Anything else was
/// never a complete frame, and answers as the torn tail it is.
///
/// The `index` and `position` are the refused frame's would-be index in the
/// file and the chain position of the last committed entry before it.
///
/// # Errors
///
/// [`JournalError::Corrupt`] when the tail is a complete frame with a lying
/// length; [`JournalError::Storage`] when the disk refuses.
fn resolve_ambiguous_tail(
    file: &mut File,
    offset: u64,
    declared_len: usize,
    position: JournalPosition,
    index: u64,
) -> Result<ScanStop, JournalError> {
    let length_u64 = u64::from(u32::try_from(LENGTH_BYTES).unwrap_or(u32::MAX));
    let head_u64 = u64::from(u32::try_from(HEAD_BYTES).unwrap_or(u32::MAX));
    let file_len = file.metadata().map_err(JournalError::Storage)?.len();
    let after_prefix = file_len.saturating_sub(offset).saturating_sub(length_u64);
    // A complete frame must leave room for its head, and a tail that already
    // satisfied the declared length could not have ended its read early.
    let candidate = after_prefix.saturating_sub(head_u64);
    let declared = u64::try_from(declared_len).unwrap_or(u64::MAX);
    if candidate == 0 || candidate >= declared {
        return Ok(ScanStop::Torn(offset));
    }

    let candidate_len = usize::try_from(candidate).unwrap_or(usize::MAX);
    let tail_len = candidate_len.checked_add(HEAD_BYTES).ok_or_else(|| {
        JournalError::Storage(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the ambiguous tail exceeds the addressable length",
        ))
    })?;
    let start = offset.saturating_add(length_u64);
    {
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(start))
            .map_err(JournalError::Storage)?;
    }
    let mut tail = vec![0u8; tail_len];
    file.read_exact(&mut tail).map_err(JournalError::Storage)?;
    let candidate_payload = &tail[..candidate_len];
    let candidate_head = &tail[candidate_len..];

    let event: EffectEvent = match from_bytes::<EffectEvent, WireError>(candidate_payload) {
        Ok(event) => event,
        Err(_) => return Ok(ScanStop::Torn(offset)),
    };
    let head_digest = chain(position, &event)?;
    if head_digest.as_bytes() == candidate_head {
        // A complete, chain-valid frame sits at the tail: the declared
        // length lied about a frame this journal had acknowledged. The
        // prefix is not evidence of an interrupted append, so the file is
        // refused with every byte preserved.
        return Err(JournalError::Corrupt(Box::new(Corruption::new(
            index,
            CorruptionKind::Framed,
        ))));
    }
    Ok(ScanStop::Torn(offset))
}

/// Read frames from `reader`, stopping at the first torn frame.
///
/// Returns the decoded entries, or the corruption that refuses the file.
/// The scan is streaming and every iteration consumes at least one byte, so
/// the loop is bounded by the file's own length.
fn scan(
    reader: &mut impl Read,
    previous: JournalPosition,
    max_events: usize,
) -> Result<(Vec<JournalEntry>, ScanStop), JournalError> {
    let mut entries = Vec::new();
    let mut position = previous;
    let mut offset = 0u64;
    let mut index = 0u64;
    loop {
        let mut prefix = [0u8; LENGTH_BYTES];
        match read_exact_or_eof(reader, &mut prefix)? {
            None => return Ok((entries, ScanStop::Complete(offset))),
            Some(read_len) if read_len < LENGTH_BYTES => {
                return Ok((entries, ScanStop::Torn(offset)));
            }
            Some(_) => {
                let requested = u64::try_from(entries.len())
                    .unwrap_or(u64::MAX)
                    .saturating_add(1);
                if requested > u64::try_from(max_events).unwrap_or(u64::MAX) {
                    return Err(JournalError::CapacityExceeded {
                        resource: JournalLimitKind::Events,
                        limit: u64::try_from(max_events).unwrap_or(u64::MAX),
                        requested,
                    });
                }
            }
        }
        let payload_len = usize::try_from(u32::from_be_bytes(prefix)).unwrap_or(usize::MAX);
        if payload_len == 0 || payload_len > MAX_FRAME_BYTES {
            // A complete prefix that names an impossible frame is not a torn
            // append: a write leaves only a prefix of its bytes, so the
            // length a writer did complete is the length it intended, and
            // that is always a frame this journal writes. Refuse, never
            // trim: the bytes after this point may be acknowledged.
            return Err(JournalError::Corrupt(Box::new(Corruption::new(
                index,
                CorruptionKind::Framed,
            ))));
        }
        let mut payload = vec![0u8; payload_len];
        if let FramePiece::Interrupted = read_exact_classified(reader, &mut payload)? {
            // The declared payload ran past the end of the file. Whether that
            // is an interrupted append or a lying length is decided in
            // `resolve_ambiguous_tail`, on the frame's own stored head — not
            // here, and never by trusting the prefix.
            return Ok((
                entries,
                ScanStop::AmbiguousTail {
                    offset,
                    declared_len: payload_len,
                },
            ));
        }
        let mut head = [0u8; HEAD_BYTES];
        if let FramePiece::Interrupted = read_exact_classified(reader, &mut head)? {
            return Ok((entries, ScanStop::Torn(offset)));
        }

        let event: EffectEvent = from_bytes::<EffectEvent, WireError>(&payload).map_err(|_| {
            JournalError::Corrupt(Box::new(Corruption::new(
                index,
                CorruptionKind::Undecodable,
            )))
        })?;
        let head_digest = chain(position, &event)?;
        let recomputed = JournalPosition {
            sequence: position.sequence().saturating_add(1),
            head: head_digest,
        };
        let recorded = JournalPosition {
            sequence: recomputed.sequence(),
            head: lgwks_std::hash::Digest::from_bytes(head),
        };
        if recorded != recomputed {
            return Err(JournalError::Corrupt(Box::new(Corruption::new(
                index,
                CorruptionKind::Chain(ChainBreak::Disagreement {
                    at: recorded.sequence(),
                    recorded,
                    recomputed,
                }),
            ))));
        }
        entries.push(JournalEntry::new(recorded, event));
        position = recorded;
        let frame_len = LENGTH_BYTES
            .saturating_add(payload_len)
            .saturating_add(HEAD_BYTES);
        let frame_len = u64::try_from(frame_len).unwrap_or(u64::MAX);
        offset = offset.saturating_add(frame_len);
        index = index.saturating_add(1);
    }
}

/// Read `buf.len()` bytes, or fewer at end of file, reporting how many.
///
/// `None` means nothing at all was read: a clean end of file.
fn read_exact_or_eof(
    reader: &mut impl Read,
    buf: &mut [u8],
) -> Result<Option<usize>, JournalError> {
    let mut filled = 0usize;
    while filled < buf.len() {
        let read = reader
            .read(&mut buf[filled..])
            .map_err(JournalError::Storage)?;
        if read == 0 {
            break;
        }
        filled = filled.saturating_add(read);
    }
    if filled == 0 {
        Ok(None)
    } else {
        Ok(Some(filled))
    }
}

/// Take the lock, treating a poisoned one as recoverable.
///
/// Nothing in this module can panic while the lock is held — every arm is a
/// `Result` that propagates — so a poison would be a bug in an unrelated
/// thread, and refusing every later append because of it would turn one
/// failure into a bricked journal.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One durable append, as the storage owner receives it.
struct Append {
    /// The acknowledged prefix length this handle believes the file has.
    expected_len: u64,
    /// The frames to write, in chain order.
    frames: Vec<u8>,
}

/// The state the handle and its storage owner share.
///
/// A single slot, not a queue. Every append API takes `&mut self`, so the type
/// system already admits one outstanding request per journal: a second is not
/// representable. That is the whole bound, and it is the tightest one
/// available — an owner that could be handed two requests would need a queue,
/// and a queue would need a length to bound it.
#[derive(Default)]
struct Slot {
    /// The append waiting to be performed, if one is.
    request: Option<Append>,
    /// The outcome of the append the owner is performing or has performed.
    outcome: Option<std::io::Result<()>>,
    /// Whether the caller that submitted is still there to receive it.
    waiting: bool,
    /// The task to wake when it is, for a caller awaiting rather than blocking.
    waker: Option<Waker>,
    /// Whether bytes may be on the disk that no acknowledgment names.
    poisoned: bool,
    /// Whether the handle is gone and the owner should finish and exit.
    closed: bool,
    /// Whether the owner has dropped the file and released the writer lock.
    released: bool,
    /// Whether a flush should wait for an explicit release before syncing.
    stalled: bool,
    /// Whether the next commit should report a device refusal instead of
    /// writing. Private, and never reachable from outside the crate: a write
    /// that fails after the fence has passed is the one failure the poison
    /// exists for, and it cannot be produced from the filesystem on demand.
    fail_next: bool,
}

/// The one thread that owns a journal's file.
///
/// # ASSUMPTION: the owner is the only writer
///
/// Every append on this journal goes through the owner's slot, and the only
/// other handle anyone holds is a read-only view that can be asked a length
/// and nothing else. This is a durability boundary, not a style preference: it
/// is why the length check and the write it guards can be one ordered step.
/// `grep -n "ASSUMPTION: the owner is the only writer"` finds every place that
/// claim is relied on.
///
/// # ASSUMPTION: the advisory lock is held by the file, not the thread
///
/// The lock is taken in `open` and the `File` then moves to the owner thread,
/// so the lock must follow the descriptor rather than the thread that took it.
/// This holds on the platforms the estate builds for; a platform where an
/// advisory lock were owned per-thread would need the lock taken on the owner
/// instead, which is a one-line move of `try_lock` into `serve`.
///
/// # ASSUMPTION: an append is at-least-once, never exactly-once
///
/// The owner performs what it was asked and reports what happened. It cannot
/// know whether a caller that stopped listening will retry, and it does not
/// pretend to: a caller that leaves latches the poison, and the next reader
/// reconciles by reading the file back rather than by assuming. Exactly-once
/// is a property of a caller's idempotency key, never of this thread.
///
/// # Why the file is not on the handle
///
/// `write_all` and `sync_all` are the storage device's cost, and the durable
/// path reaches them from the thread that awaits the dispatch. On a
/// current-thread executor there is no other thread to run anything, so the
/// device's latency became the process's latency: a heartbeat, a timer and
/// every unrelated bot in the same runtime stalled for the length of an
/// `fsync`. Moving the file here makes the device's cost the owner's, and the
/// caller's cost a wait it may await rather than sit through.
///
/// # What the owner preserves
///
/// Ordering is the owner's: it is one thread taking one request at a time, so
/// the order appends were admitted in is the order they reach the disk. The
/// staleness fence moved here with the write, so the length check and the
/// write it guards are one ordered step and no append can be admitted between
/// them. And an append whose caller has gone is not quietly forgotten: the
/// owner notices the waiter is gone, latches the poison, and the handle refuses
/// every later append rather than continuing from a view that may be behind
/// the file.
struct StorageOwner {
    /// The slot both sides read, and the poison the owner latches.
    shared: Arc<Mutex<Slot>>,
    /// Woken on every state change, so a blocking caller and the owner's own
    /// wait both re-read rather than spin.
    signal: Arc<Condvar>,
}

/// A handle onto one journal's storage device, cloneable and independent of
/// the journal.
///
/// It exists because a caller awaiting an append cannot reach the journal to
/// release it: the future holds the handle's borrow for exactly as long as the
/// append is outstanding, which is the window in which the device most needs
/// to be unstuck. An operator un-sticking a real device is in the same
/// position — their handle is busy on the append that is waiting.
#[derive(Clone)]
pub struct StorageGate {
    /// The owner's slot, shared with the thread that performs the appends.
    shared: Arc<Mutex<Slot>>,
    /// Woken so the owner re-reads the gate.
    signal: Arc<Condvar>,
}

impl StorageGate {
    /// Let a flush that is waiting for a release proceed, and every flush
    /// after it. The stall is not re-armed.
    pub fn release(&self) {
        let mut slot = lock(&self.shared);
        slot.stalled = false;
        self.signal.notify_all();
    }
}

impl StorageOwner {
    /// Hand `file` to a new owner thread and return the handle side, plus a
    /// read-only view of the same file for the handle's own fence.
    ///
    /// Two handles, one writer. The owner holds the only handle that can write
    /// and the lock; the view can only be asked how long the file is, which is
    /// what the fence needs before it decides anything. The fence cannot go to
    /// the owner for that answer, because a fence that had to wait for the
    /// device would charge the caller the device's cost before it had decided
    /// to append at all.
    fn spawn(file: File, path: &Path, stalled: bool) -> Result<(Self, FileView), JournalError> {
        let shared = Arc::new(Mutex::new(Slot {
            stalled,
            ..Slot::default()
        }));
        let signal = Arc::new(Condvar::new());
        let owner = Self {
            shared: Arc::clone(&shared),
            signal: Arc::clone(&signal),
        };
        // A dedicated thread, not a pooled blocking job: this one outlives
        // every job it runs and holds the file for the handle's whole life, so
        // it is a thread with a lifetime rather than work handed to a pool.
        // The handle is owned by `StorageOwner::drop`, which closes the slot
        // and waits for the owner to report it has let the file go.
        let view = FileView::read_only(path)?;
        let _task = lgwks_std::task::spawn_blocking(move || serve(file, shared, signal));
        Ok((owner, view))
    }

    /// Whether an append may be admitted at all.
    fn poisoned(&self) -> bool {
        lock(&self.shared).poisoned
    }

    /// Perform `frames` and wait for the owner to finish.
    fn submit(&self, expected_len: u64, frames: Vec<u8>) -> Result<(), std::io::Error> {
        let mut slot = lock(&self.shared);
        slot.request = Some(Append {
            expected_len,
            frames,
        });
        slot.outcome = None;
        slot.waiting = true;
        self.signal.notify_all();
        loop {
            if let Some(outcome) = slot.outcome.take() {
                slot.waiting = false;
                return outcome;
            }
            slot = self
                .signal
                .wait(slot)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    /// Make the next commit report a device refusal instead of writing.
    ///
    /// Private, and deliberately so: the failure this models is a write that
    /// fails after the fence has passed, which no filesystem will produce on
    /// demand, and the poison latch behind it has to be tested somehow.
    #[cfg(test)]
    fn fail_next_commit(&self) {
        lock(&self.shared).fail_next = true;
    }

    /// The same door, for a caller that awaits rather than blocks.
    ///
    /// The difference is only in how the answer is delivered: a blocking caller
    /// waits on the signal, a task registers a waker and is woken by the owner
    /// when it answers. The append itself is the same ordered step either way,
    /// so a caller choosing to await does not get a weaker write than one that
    /// chose to sit through it.
    fn submit_async<'a>(&'a self, expected_len: u64, frames: Vec<u8>) -> Awaiting<'a> {
        Awaiting {
            owner: self,
            pending: Some(Append {
                expected_len,
                frames,
            }),
            submitted: false,
        }
    }
}

/// A submitted append waiting for the storage owner's answer.
///
/// Dropping this is not a cancellation of the append, and the type says so:
/// [`Drop`] tells the owner its waiter is gone, which is the same position a
/// failed write leaves the handle in, and the owner latches the poison on it.
/// A caller that walks away from a durable write it started must not then keep
/// appending from a view that may be behind the file.
struct Awaiting<'a> {
    /// The owner that was asked, and will answer.
    owner: &'a StorageOwner,
    /// The append, until it has been handed over.
    pending: Option<Append>,
    /// Whether it has been handed over.
    submitted: bool,
}

impl Drop for Awaiting<'_> {
    fn drop(&mut self) {
        if !self.submitted {
            return;
        }
        let mut slot = lock(&self.owner.shared);
        slot.waiting = false;
    }
}

impl std::future::Future for Awaiting<'_> {
    type Output = Result<(), std::io::Error>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.get_mut();
        let mut slot = lock(&this.owner.shared);
        if !this.submitted {
            let request = this.pending.take().unwrap_or_else(|| Append {
                expected_len: 0,
                frames: Vec::new(),
            });
            this.submitted = true;
            slot.request = Some(request);
            slot.outcome = None;
            slot.waiting = true;
            this.owner.signal.notify_all();
        }
        if let Some(outcome) = slot.outcome.take() {
            slot.waiting = false;
            return std::task::Poll::Ready(outcome);
        }
        slot.waker = Some(cx.waker().clone());
        std::task::Poll::Pending
    }
}

impl Drop for StorageOwner {
    fn drop(&mut self) {
        let mut slot = lock(&self.shared);
        slot.closed = true;
        self.signal.notify_all();
        // Wait for the owner to report the file released, so the advisory lock
        // this journal held is gone before a caller reopens the same path. The
        // wait is bounded by the device: it ends when the owner's own in-flight
        // append ends, which is the same append whose bytes are on the disk
        // under the acknowledgment the caller may never see.
        while !slot.released {
            slot = self
                .signal
                .wait(slot)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
}

/// The owner's loop: take a request, perform it, publish the outcome, repeat.
///
/// The file is dropped before `released` is set, so a waiter that observes the
/// release knows the advisory lock is already gone rather than about to be.
fn serve(mut file: File, shared: Arc<Mutex<Slot>>, signal: Arc<Condvar>) {
    loop {
        let request = {
            let mut slot = lock(&shared);
            loop {
                if slot.closed {
                    break None;
                }
                if let Some(request) = slot.request.take() {
                    break Some(request);
                }
                slot = signal
                    .wait(slot)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        };
        let Some(request) = request else { break };

        // A stalled flush models a device that has not answered yet. It is the
        // only reason this thread ever waits on anything but its own request.
        {
            let mut slot = lock(&shared);
            while slot.stalled && !slot.closed {
                slot = signal
                    .wait(slot)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        }

        let outcome = {
            let mut slot = lock(&shared);
            if std::mem::take(&mut slot.fail_next) {
                Err(std::io::Error::other("the injected device refusal"))
            } else {
                drop(slot);
                commit(&mut file, request.expected_len, &request.frames)
            }
        };

        let mut slot = lock(&shared);
        // A caller that went away while the bytes were moving leaves the handle
        // unable to say whether they landed. That is the same position a failed
        // write leaves it in, so it is refused the same way.
        if outcome.is_err() || !slot.waiting {
            slot.poisoned = true;
        }
        slot.outcome = Some(outcome);
        if let Some(waker) = slot.waker.take() {
            waker.wake();
        }
        signal.notify_all();
    }
    drop(file);
    let mut slot = lock(&shared);
    slot.released = true;
    signal.notify_all();
}

/// The ordered step that is the append: check the fence, write, sync.
///
/// # Errors
///
/// Whatever the device reports. A length that is not the one the caller
/// expected means the file moved, and is refused before any byte is written.
fn commit(file: &mut File, expected_len: u64, frames: &[u8]) -> std::io::Result<()> {
    let on_disk = file.metadata()?.len();
    if on_disk != expected_len {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the journal file moved under this controller; reopen before appending",
        ));
    }
    file.write_all(frames)?;
    file.sync_all()
}

/// An effect journal whose acknowledged appends are on the disk.
///
/// Opened with [`FileJournal::open`], which creates the file when it does not
/// exist and replays it when it does. The [`DurabilityPromise::ProcessCrash`]
/// this journal reports is earned per append: the frame is written and the
/// file is synced before the acknowledgment exists. What it does not claim is
/// power-loss durability — `sync_all` asks the operating system to reach the
/// device, and what the device does with that is the device's contract, not
/// this module's.
pub struct FileJournal {
    /// Where the journal lives.
    path: PathBuf,
    /// The thread that holds the file, and the one door an append reaches the
    /// disk through. It also carries the poison, so the handle never has to
    /// guess whether a write it did not see succeed.
    storage: StorageOwner,
    /// A read-only window onto the same file, so the fence can check the
    /// acknowledged length without a round trip to the storage owner.
    view: FileView,
    /// The replayed history, which is also the append fence's view.
    committed: Vec<JournalEntry>,
    /// The last kind recorded per key, the append fence's index, so an
    /// append's ladder check does not walk the replayed history.
    ladder: HashMap<crate::effect::EffectKey, EventKind>,
    /// Latest outcome record and position per attempt, built during replay.
    outcomes: HashMap<crate::effect::EffectKey, (JournalPosition, EffectEvidence)>,
    /// Byte length of the acknowledged prefix on disk.
    disk_len: u64,
    /// Whether open repaired a torn tail to get here.
    torn_tail_repaired: bool,
}

impl core::fmt::Debug for FileJournal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("FileJournal")
            .field("path", &self.path)
            .field("committed_events", &self.committed.len())
            .field("disk_len", &self.disk_len)
            .field("torn_tail_repaired", &self.torn_tail_repaired)
            .finish()
    }
}

/// A read-only window onto the journal's file, for the handle's own fence.
///
/// It exists so the fence can ask how long the file is without asking the
/// storage owner, and asking the owner would mean waiting for the device
/// before deciding whether to append at all. It cannot write: it is opened
/// without write permission, so the owner stays the only writer even though
/// two descriptors exist.
struct FileView {
    /// The read-only descriptor.
    file: File,
}

impl FileView {
    /// Open a read-only descriptor onto `path`.
    fn read_only(path: &Path) -> Result<Self, JournalError> {
        Ok(Self {
            file: OpenOptions::new()
                .read(true)
                .open(path)
                .map_err(JournalError::Storage)?,
        })
    }

    /// The file's current byte length.
    fn len(&self) -> Result<u64, JournalError> {
        self.file
            .metadata()
            .map_err(JournalError::Storage)
            .map(|meta| meta.len())
    }
}

impl FileJournal {
    /// Open the journal at `path`, creating the file when it does not exist
    /// and replaying it when it does.
    ///
    /// The exclusive advisory file lock is taken before a single byte is
    /// read: the replay and the torn-tail repair belong to the lock's owner
    /// alone, and a second opener is refused with
    /// [`JournalError::Locked`] rather than allowed to scan or truncate a
    /// file someone else is appending to. The lock is held for the handle's
    /// whole life and released by the kernel when the handle closes or the
    /// process dies.
    ///
    /// A torn tail — an append a killed writer never finished — is truncated
    /// back to the last whole frame, because it was never acknowledged. A
    /// lying frame — undecodable bytes, a length no write of this journal
    /// can produce, or a head that does not follow — is refused, because it
    /// may have been. The distinction is reported: after a repair,
    /// [`FileJournal::torn_tail_repaired`] is `true`.
    ///
    /// # Errors
    ///
    /// [`JournalError::Locked`] when another writer holds the file;
    /// [`JournalError::Storage`] when the file cannot be opened, read or
    /// repaired; [`JournalError::Corrupt`] when committed bytes are refused;
    /// [`JournalError::CapacityExceeded`] when the complete history exceeds
    /// [`MAX_JOURNAL_BYTES`] or [`MAX_JOURNAL_EVENTS`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        Self::open_impl(path.as_ref(), false)
    }

    /// Open, with the device's answering behaviour chosen by the caller.
    ///
    /// One implementation for both constructors: a stalled device is a
    /// property of the storage owner, not a second way to open a journal.
    ///
    /// # Errors
    ///
    /// Whatever the file, the lock or the scan reports.
    fn open_impl(path: &Path, stalled: bool) -> Result<Self, JournalError> {
        let path = path.to_path_buf();
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&path)
            .map_err(JournalError::Storage)?;
        // Fail closed: a filesystem that cannot supply the lock refuses the
        // open rather than getting an unfenced journal. The lock is advisory
        // and binds writers through this constructor; see
        // [`JournalError::Locked`] for the exact reach of that promise.
        file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => JournalError::Locked {
                path: path.display().to_string(),
            },
            std::fs::TryLockError::Error(io) => JournalError::Storage(io),
        })?;

        let file_len = file.metadata().map_err(JournalError::Storage)?.len();
        if file_len > MAX_JOURNAL_BYTES {
            return Err(JournalError::CapacityExceeded {
                resource: JournalLimitKind::Bytes,
                limit: MAX_JOURNAL_BYTES,
                requested: file_len,
            });
        }

        file.seek_read_zero()?;
        let mut reader = BufReader::new(&mut file);
        let (entries, stop) = scan(&mut reader, JournalPosition::genesis(), MAX_JOURNAL_EVENTS)?;
        drop(reader);

        let (acked_len, torn_tail_repaired) = match stop {
            ScanStop::Complete(len) => (len, false),
            ScanStop::Torn(offset) => {
                // The interrupted append was never acknowledged; taking it
                // back restores the file to the prefix every acknowledgment
                // still names.
                file.set_len(offset).map_err(JournalError::Storage)?;
                file.sync_all().map_err(JournalError::Storage)?;
                (offset, true)
            }
            ScanStop::AmbiguousTail {
                offset,
                declared_len,
            } => {
                let index = u64::try_from(entries.len()).unwrap_or(u64::MAX);
                let resolved = resolve_ambiguous_tail(
                    &mut file,
                    offset,
                    declared_len,
                    entries
                        .last()
                        .map_or_else(JournalPosition::genesis, JournalEntry::position),
                    index,
                )?;
                let ScanStop::Torn(offset) = resolved else {
                    // Unreachable by construction: the resolver answers
                    // either a torn tail or refuses with the corruption
                    // itself.
                    return Err(JournalError::Storage(std::io::Error::other(
                        "ambiguous tail resolved to an impossible state",
                    )));
                };
                file.set_len(offset).map_err(JournalError::Storage)?;
                file.sync_all().map_err(JournalError::Storage)?;
                (offset, true)
            }
        };

        let ladder = entries
            .iter()
            .map(|entry| (entry.event().key(), entry.event().kind()))
            .collect();
        let mut outcomes = HashMap::new();
        for entry in &entries {
            if let EffectEvent::OutcomeObserved { key, evidence } = *entry.event() {
                outcomes.insert(key, (entry.position(), evidence));
            }
        }

        let (storage, view) = StorageOwner::spawn(file, &path, stalled)?;
        Ok(Self {
            path,
            storage,
            view,
            committed: entries,
            ladder,
            outcomes,
            disk_len: acked_len,
            torn_tail_repaired,
        })
    }

    /// Refuse an append that would take the file past [`MAX_JOURNAL_EVENTS`].
    ///
    /// `additional` is how many events this append is about to add.
    ///
    /// # Errors
    ///
    /// [`JournalError::CapacityExceeded`] when the total would be over the
    /// bound. The history is not compacted to make room, so nothing
    /// acknowledged is evicted to admit more work.
    fn bound_events(&self, additional: u64) -> Result<(), JournalError> {
        let requested = u64::try_from(self.committed.len())
            .unwrap_or(u64::MAX)
            .saturating_add(additional);
        let limit = u64::try_from(MAX_JOURNAL_EVENTS).unwrap_or(u64::MAX);
        if requested > limit {
            return Err(JournalError::CapacityExceeded {
                resource: JournalLimitKind::Events,
                limit,
                requested,
            });
        }
        Ok(())
    }

    /// Where the journal lives.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether open repaired a torn tail to produce this journal.
    ///
    /// `true` means a killed writer's last append was taken back. The events
    /// this journal holds are exactly the ones that were acknowledged.
    #[must_use]
    pub const fn torn_tail_repaired(&self) -> bool {
        self.torn_tail_repaired
    }

    /// The committed events, in append order.
    pub fn events(&self) -> impl Iterator<Item = &EffectEvent> {
        self.committed.iter().map(JournalEntry::event)
    }

    /// What the journal has learned about each attempt.
    #[must_use]
    pub fn recover(&self) -> Recovered {
        recover(self.events())
    }

    /// The one door both append paths take before they are allowed to write.
    ///
    /// A handle whose own write failed is stale by definition: bytes may be on
    /// the disk that no acknowledgment names, and only a reopen replays the
    /// truth. A file that is no longer exactly the acknowledged prefix means
    /// something else moved it, and an append from here would branch the chain
    /// rather than extend it.
    ///
    /// The length half here is a precondition, not the guarantee: it is a
    /// `stat` against a file another writer may still be moving. The
    /// authoritative check is inside the storage owner's ordered step, where it
    /// cannot be overtaken by another append. This one exists so a stale
    /// handle is refused before it spends anything, and so an append with no
    /// frames to write is still answered for itself.
    ///
    /// # Errors
    ///
    /// [`JournalError::Storage`] when this handle is stale and must be reopened.
    fn fence(&self) -> Result<(), JournalError> {
        if self.storage.poisoned() {
            return Err(JournalError::Storage(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "a previous append failed while writing; the file may hold \
                 unacknowledged bytes, reopen to replay",
            )));
        }
        if self.view.len()? != self.disk_len {
            return Err(JournalError::Storage(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the journal file moved under this controller; reopen before appending",
            )));
        }
        Ok(())
    }

    /// Frame one event: its length prefix, its archived bytes, and the chain
    /// head it commits to.
    ///
    /// The head is stored with the frame rather than recomputed on faith at
    /// read time. A frame over [`MAX_FRAME_BYTES`] is refused here rather than
    /// written, because the bytes behind it may be acknowledged.
    ///
    /// # Errors
    ///
    /// [`JournalError::Exhausted`] when the position cannot advance,
    /// [`JournalError::Encoding`] when the event cannot be archived, and
    /// [`JournalError::Storage`] when the frame is over the bound.
    fn frame(
        &self,
        event: &EffectEvent,
        from: JournalPosition,
    ) -> Result<(JournalPosition, Vec<u8>), JournalError> {
        let sequence = from
            .sequence()
            .checked_add(1)
            .ok_or(JournalError::Exhausted)?;
        let head = chain(from, event)?;
        let position = JournalPosition { sequence, head };
        let payload = event.to_bytes().map_err(JournalError::Encoding)?;
        let payload_len = u32::try_from(payload.len())
            .ok()
            .filter(|len| usize::try_from(*len).is_ok_and(|len| len <= MAX_FRAME_BYTES));
        let Some(payload_len) = payload_len else {
            return Err(JournalError::Storage(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the event exceeds this journal's frame bound",
            )));
        };
        let mut framed = Vec::with_capacity(
            LENGTH_BYTES
                .saturating_add(payload.len())
                .saturating_add(HEAD_BYTES),
        );
        framed.extend_from_slice(&payload_len.to_be_bytes());
        framed.extend_from_slice(&payload);
        framed.extend_from_slice(head.as_bytes());
        Ok((position, framed))
    }

    /// Refuse an append that would take the file past [`MAX_JOURNAL_BYTES`].
    ///
    /// The file is refused rather than compacted, so no acknowledged evidence
    /// is evicted to make room.
    ///
    /// # Errors
    ///
    /// [`JournalError::CapacityExceeded`] when the staged write is over the
    /// bound.
    fn bound_bytes(&self, staged: usize) -> Result<(), JournalError> {
        let requested = self
            .disk_len
            .saturating_add(u64::try_from(staged).unwrap_or(u64::MAX));
        if requested > MAX_JOURNAL_BYTES {
            return Err(JournalError::CapacityExceeded {
                resource: JournalLimitKind::Bytes,
                limit: MAX_JOURNAL_BYTES,
                requested,
            });
        }
        Ok(())
    }

    /// Hand the frames to the storage owner, which is where the write and the
    /// sync happen, and wait for it to answer.
    ///
    /// A failure of either half is [`JournalError::OutcomeUnknown`], because a
    /// prefix of the bytes, or all of them, may be on the disk with only the
    /// reply lost — never a bare [`JournalError::Storage`], which would claim
    /// the journal is unchanged when the write may have landed. The owner
    /// latches the poison, so this handle refuses every later append until a
    /// reopen replays the truth.
    ///
    /// # Errors
    ///
    /// [`JournalError::OutcomeUnknown`] from either the write or the sync.
    fn write_and_sync(&mut self, frames: &[u8]) -> Result<(), JournalError> {
        self.storage
            .submit(self.disk_len, frames.to_vec())
            .map_err(|cause| JournalError::OutcomeUnknown { cause })
    }

    /// Fold a committed frame into this handle's view of the journal.
    ///
    /// The acknowledgment is minted from the position the frame was chained at,
    /// so the two cannot disagree.
    fn accept(&mut self, event: &EffectEvent, position: JournalPosition, frame_len: usize) {
        self.committed.push(JournalEntry::new(position, *event));
        self.ladder.insert(event.key(), event.kind());
        if let EffectEvent::OutcomeObserved { key, evidence } = *event {
            self.outcomes.insert(key, (position, evidence));
        }
        self.disk_len = self
            .disk_len
            .saturating_add(u64::try_from(frame_len).unwrap_or(u64::MAX));
    }

    /// Everything one append checks and frames before the disk is touched.
    ///
    /// The synchronous and asynchronous forms differ only in how they wait for
    /// the storage owner, so everything that decides *whether* to append lives
    /// here and is reached by both: the tail fence, the ladder, the event
    /// bound, the frame, and the byte bound. A check that exists once cannot
    /// drift between the two doors.
    ///
    /// # Errors
    ///
    /// [`JournalError::TailMismatch`] when the caller's view of the tail is
    /// not this journal's, [`JournalError::OutOfOrder`] when the rung cannot
    /// follow what is committed for the key, [`JournalError::Storage`] when
    /// this handle is stale or the frame is over the bound, and
    /// [`JournalError::CapacityExceeded`] when the append is over an event or
    /// byte ceiling. Nothing is written by any of them.
    fn prepare_append(
        &self,
        expected_tail: JournalPosition,
        event: &EffectEvent,
    ) -> Result<(JournalPosition, Vec<u8>), JournalError> {
        let actual = self.tail();
        if expected_tail != actual {
            return Err(JournalError::TailMismatch {
                expected: expected_tail,
                actual,
            });
        }
        let attempted = event.kind();
        let expected = next_allowed_of(self.ladder.get(&event.key()).copied());
        if expected != Some(attempted) {
            return Err(JournalError::OutOfOrder {
                key: Box::new(event.key()),
                expected,
                attempted,
            });
        }
        self.fence()?;
        self.bound_events(1)?;
        let (position, frame) = self.frame(event, actual)?;
        self.bound_bytes(frame.len())?;
        Ok((position, frame))
    }

    /// Open a journal whose storage does not answer a flush until
    /// [`Self::release_storage`] is called.
    ///
    /// A fault injector, and the reason it is public rather than hidden behind
    /// `cfg(test)`: "what does this bot do while its disk has stopped
    /// answering" is a question an operator has to be able to ask on a real
    /// process, and a probe that only exists inside the crate's own test
    /// binary cannot answer it. The bytes written are real and the frames are
    /// the ones the journal always writes — only the device's answer is held
    /// back, which is exactly what a stalled device does.
    ///
    /// Everything else about the handle is unchanged, including what it
    /// promises: nothing is acknowledged until the release, so a stall delays
    /// acknowledgments and never mints one early.
    ///
    /// # Errors
    ///
    /// Whatever [`FileJournal::open`] reports.
    pub fn open_with_stalled_storage(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        Self::open_impl(path.as_ref(), true)
    }

    /// A handle that can release the stall, independently of this journal.
    ///
    /// Returned alongside the journal rather than only as
    /// [`Self::release_storage`] because the caller that most needs to un-stick
    /// the device is the one awaiting an append on it, and that caller holds
    /// the journal's borrow for the whole wait.
    fn storage_gate(&self) -> StorageGate {
        StorageGate {
            shared: Arc::clone(&self.storage.shared),
            signal: Arc::clone(&self.storage.signal),
        }
    }

    /// Let the outstanding flush proceed, and every flush after it.
    ///
    /// After this, the handle is an ordinary file journal: the stall is over
    /// and is not re-armed. A caller blocked inside
    /// [`Self::compare_and_append`] cannot reach this method — the borrow the
    /// append holds is the same one — and needs
    /// [`Self::storage_gate`] instead.
    pub fn release_storage(&self) {
        self.storage_gate().release();
    }
    /// every durable log converges on: one flush pays for many records.
    ///
    /// Every rung is checked first — fence, ladder, frame bound — against
    /// the view the batch itself builds, and a batch in which any rung would
    /// be refused writes nothing and returns that refusal: validation is
    /// all-or-nothing. When the checks pass, the frames go out in one
    /// `write_all` and one `sync_all`, and only then are the acknowledgments
    /// minted, so each one carries the same earned promise
    /// [`DurabilityPromise::ProcessCrash`] a single append's does. Nothing is
    /// weakened: a kill between the sync and the return costs the whole
    /// batch, exactly as a kill between the sync and the return of any one
    /// append would cost that append. A write that fails mid-way may leave
    /// an unacknowledged prefix of the batch on the disk; like a single
    /// append's failure it is [`JournalError::OutcomeUnknown`] — a prefix or
    /// all of the batch may have landed — and the handle refuses further
    /// appends until a reopen replays the truth, then repairs the prefix as
    /// a torn tail, the same disposition a killed writer's bytes get.
    ///
    /// A controller that records several rungs of one attempt in a single
    /// turn pays one flush instead of one per rung: measured on this
    /// machine's file system, a four-rung attempt through four appends costs
    /// four flushes at about 3.3 ms each, and through this batch one.
    ///
    /// # Errors
    ///
    /// Every [`JournalError`] a single append can produce. A rung's refusal
    /// is returned and no bytes are written; a failure of the write itself
    /// is [`JournalError::OutcomeUnknown`] — the frames may be on the disk
    /// with the reply lost — and the handle is poisoned until a reopen.
    pub fn compare_and_append_all(
        &mut self,
        events: &[EffectEvent],
    ) -> Result<Vec<DurableAck>, JournalError> {
        // The fence runs once for the batch, ahead of everything including the
        // empty case: a stale handle answers for itself, not with an empty
        // success.
        self.fence()?;
        if events.is_empty() {
            return Ok(Vec::new());
        }
        self.bound_events(u64::try_from(events.len()).unwrap_or(u64::MAX))?;

        // Validate and frame every rung against the evolving view before any
        // byte moves: the staged kinds for keys this batch is itself climbing
        // take precedence, because they are the newest fact about the key,
        // and the committed ladder serves keys the batch has not touched
        // yet. Nothing here mutates the journal.
        let mut position = self.tail();
        let mut staged: HashMap<crate::effect::EffectKey, EventKind> = HashMap::new();
        let mut frames = Vec::new();
        let mut pending: Vec<(JournalPosition, usize)> = Vec::with_capacity(events.len());
        for event in events {
            let key = event.key();
            let attempted = event.kind();
            let expected =
                next_allowed_of(staged.get(&key).or_else(|| self.ladder.get(&key)).copied());
            if expected != Some(attempted) {
                return Err(JournalError::OutOfOrder {
                    key: Box::new(key),
                    expected,
                    attempted,
                });
            }
            staged.insert(key, attempted);
            let (next, framed) = self.frame(event, position)?;
            position = next;
            self.bound_bytes(frames.len().saturating_add(framed.len()))?;
            pending.push((position, framed.len()));
            frames.extend_from_slice(&framed);
        }

        // One write, one flush, through the same door the single append
        // uses, so a failure carries the single append's contract: the
        // outcome is unknown and the handle is poisoned.
        self.write_and_sync(&frames)?;

        let mut acks = Vec::with_capacity(pending.len());
        for (event, entry) in events.iter().zip(&pending) {
            let (position, frame_len) = *entry;
            self.accept(event, position, frame_len);
            acks.push(DurableAck::new(position, self.durability()));
        }
        Ok(acks)
    }
}

impl EffectJournal for FileJournal {
    fn durability(&self) -> DurabilityPromise {
        DurabilityPromise::ProcessCrash
    }

    fn tail(&self) -> JournalPosition {
        match self.committed.last() {
            Some(entry) => entry.position(),
            None => JournalPosition::genesis(),
        }
    }

    fn committed(&self) -> Result<Vec<EffectEvent>, JournalError> {
        Ok(self.events().copied().collect())
    }

    fn committed_entries(&self) -> Result<Vec<JournalEntry>, JournalError> {
        Ok(self.committed.clone())
    }

    fn committed_entry(
        &self,
        position: JournalPosition,
    ) -> Result<Option<JournalEntry>, JournalError> {
        let Some(index) = position
            .sequence()
            .checked_sub(1)
            .and_then(|n| usize::try_from(n).ok())
        else {
            return Ok(None);
        };
        Ok(self
            .committed
            .get(index)
            .copied()
            .filter(|entry| entry.position() == position))
    }

    fn outcome_at(
        &self,
        key: crate::effect::EffectKey,
    ) -> Result<Option<(JournalPosition, EffectEvidence)>, JournalError> {
        Ok(self.outcomes.get(&key).copied())
    }

    /// Append one event at `expected_tail`, or refuse.
    ///
    /// The order of the checks is the order the promises are made: the fence
    /// first, then the ladder, then the disk. Nothing is written until every
    /// check that can refuse in memory has passed, and the acknowledgment is
    /// minted only after `sync_all` returns, which is the whole reason this
    /// adapter may promise `ProcessCrash`.
    ///
    /// # Errors
    ///
    /// Every [`JournalError`] the fence, the ladder or the disk can produce.
    fn compare_and_append(
        &mut self,
        expected_tail: JournalPosition,
        event: &EffectEvent,
    ) -> Result<DurableAck, JournalError> {
        // The staleness fence, the ladder, the bounds and the framing are the
        // two doors' shared decision, so they are made once. The file's byte
        // length is checked against this write on the thread that holds it.
        let (position, frame) = self.prepare_append(expected_tail, event)?;

        // Through the shared door before the acknowledgment: after this
        // returns, the bytes are through the file system, and a kill of this
        // process cannot take the fact back out of the file.
        self.write_and_sync(&frame)?;

        self.accept(event, position, frame.len());
        Ok(DurableAck::new(position, self.durability()))
    }

    /// The same append, waited for rather than sat through.
    ///
    /// The decision is [`Self::prepare_append`]'s, so the fence, the ladder
    /// and the bounds are the synchronous form's; only the wait differs. The
    /// caller may go away while the bytes are moving, and the storage owner
    /// latches its poison when it does, so a handle that lost its waiter
    /// refuses the next append rather than continue from a view that may be
    /// behind the file.
    fn compare_and_append_async<'a>(
        &'a mut self,
        expected_tail: JournalPosition,
        event: &'a EffectEvent,
    ) -> crate::BoxFuture<'a, Result<DurableAck, JournalError>> {
        Box::pin(async move {
            let (position, frame) = self.prepare_append(expected_tail, event)?;
            self.storage
                .submit_async(self.disk_len, frame.clone())
                .await
                .map_err(|cause| JournalError::OutcomeUnknown { cause })?;
            self.accept(event, position, frame.len());
            Ok(DurableAck::new(position, self.durability()))
        })
    }

    /// Attest that an outcome this journal committed is durable.
    ///
    /// The receipt names a position; this journal answers by finding that
    /// position among its own committed entries and checking it is exactly
    /// the named outcome. A receipt this journal cannot find names a
    /// different append than the one being settled, which is
    /// [`JournalError::ReceiptMismatch`]; a grade above what this journal can
    /// promise is [`JournalError::ReceiptUnavailable`].
    ///
    /// # Errors
    ///
    /// [`JournalError::ReceiptUnavailable`] when `required` exceeds this
    /// journal's promise; [`JournalError::ReceiptMismatch`] when the position
    /// does not name this outcome as committed.
    fn confirm_outcome(
        &mut self,
        key: crate::effect::EffectKey,
        evidence: EffectEvidence,
        position: JournalPosition,
        required: DurabilityPromise,
    ) -> Result<DurableAck, JournalError> {
        if !self.durability().meets(required) {
            return Err(JournalError::ReceiptUnavailable { required });
        }
        let settled = self
            .committed
            .iter()
            .find(|entry| entry.position() == position)
            .map(JournalEntry::event)
            .copied();
        match settled {
            Some(EffectEvent::OutcomeObserved {
                key: observed_key,
                evidence: observed_evidence,
            }) if observed_key == key && observed_evidence == evidence => {
                Ok(DurableAck::new(position, self.durability()))
            }
            _ => Err(JournalError::ReceiptMismatch {
                expected: position,
                actual: self.tail(),
            }),
        }
    }
}

/// Cursor reset for the replay, spelled on the type so the call reads as
/// intent rather than as a magic constant.
trait ReplayCursor {
    /// Move the read cursor back to the first byte of the file.
    ///
    /// # Errors
    ///
    /// [`JournalError::Storage`] when the seek is refused.
    fn seek_read_zero(&mut self) -> Result<(), JournalError>;
}

impl ReplayCursor for File {
    fn seek_read_zero(&mut self) -> Result<(), JournalError> {
        use std::io::Seek;
        use std::io::SeekFrom;
        self.seek(SeekFrom::Start(0))
            .map(|_: u64| ())
            .map_err(JournalError::Storage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::{
        ActionDigest, ActionId, AttemptId, EnvironmentEpoch, EnvironmentId, FlowRevision, RunId,
    };
    use crate::journal::{AttemptStatus, EventKind};
    use std::sync::atomic::{AtomicU64, Ordering};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Removes a test's scratch path when the test ends, however it ends.
    ///
    /// The guard is best effort: a scratch file the system refuses to remove
    /// is litter, not a failed observation, so the error is dropped rather
    /// than allowed to mask the test's own verdict.
    struct TempGuard(std::path::PathBuf);

    impl Drop for TempGuard {
        fn drop(&mut self) {
            if self.0.is_dir() {
                drop(std::fs::remove_dir_all(&self.0));
            } else {
                drop(std::fs::remove_file(&self.0));
            }
        }
    }

    const RUN: &str = "0102030405060708090a0b0c0d0e0f10";
    const ACTION: &str = "1112131415161718191a1b1c1d1e1f20";
    const ENV: &str = "2122232425262728292a2b2c2d2e2f30";
    const FLOW_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    const DIGEST_HEX: &str = "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeffe0e1e2e3e4e5e6e7e8e9eaebecedeeef";

    /// A counter that gives concurrent test runs distinct scratch names.
    ///
    /// Nanos plus a counter, not a process id: the OS reuses both pids and
    /// threads, and a reused id must never make two runs share a journal.
    static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A scratch path unique to one test run.
    fn scratch(name: &str) -> PathBuf {
        let unique = SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or_default();
        std::env::temp_dir().join(format!("lgwks-journal-file-{name}-{nanos}-{unique}"))
    }

    fn key() -> Result<crate::effect::EffectKey, Box<dyn std::error::Error>> {
        Ok(crate::effect::EffectKey::new(
            RunId::from_hex(RUN)?,
            ActionId::from_hex(ACTION)?,
            AttemptId::from_decimal("1")?,
            FlowRevision::from_tagged("blake3_256", FLOW_HEX)?,
            ActionDigest::from_tagged("blake3_256", DIGEST_HEX)?,
            EnvironmentId::from_hex(ENV)?,
            EnvironmentEpoch::from_decimal("1")?,
        ))
    }

    #[test]
    fn a_batched_ladder_is_four_acknowledgments_from_one_sync() -> TestResult {
        let path = scratch("batch");
        let _guard = TempGuard(path.clone());
        let key = key()?;
        let verdict = crate::journal::Verification::new(
            crate::effect::Id128::from_hex(&"42".repeat(16))?,
            1,
            lgwks_std::hash::blake3(b"postcondition observed"),
            crate::journal::VerificationResult::Satisfied,
        );
        let mut journal = FileJournal::open(&path)?;
        let ladder = [
            EffectEvent::IntentAdmitted { key },
            EffectEvent::DispatchPrepared { key },
            EffectEvent::OutcomeObserved {
                key,
                evidence: EffectEvidence::Applied,
            },
            EffectEvent::Verified {
                key,
                verification: verdict,
            },
        ];
        let acks = journal.compare_and_append_all(&ladder)?;
        assert_eq!(acks.len(), 4, "one acknowledgment per rung");
        assert!(
            acks.iter()
                .all(|ack| ack.promise() == DurabilityPromise::ProcessCrash),
            "every batched acknowledgment carries the same earned promise"
        );

        // The file carries exactly the batch, and a fresh controller reads it.
        drop(journal);
        let reopened = FileJournal::open(&path)?;
        assert_eq!(reopened.committed()?.len(), 4);
        assert_eq!(
            reopened.recover().status(key),
            Some(AttemptStatus::Verified),
            "the batch folded to the ladder's end state"
        );
        drop(reopened);

        // All-or-nothing: a batch whose second rung violates the ladder is
        // refused whole, and nothing of it reaches the file. The fence is
        // exclusive, so the stale handle is dropped before the fresh one
        // opens.
        let mut journal = FileJournal::open(&path)?;
        let before = journal.committed()?.len();
        let refused = journal.compare_and_append_all(&[
            EffectEvent::IntentAdmitted { key: key2()? },
            EffectEvent::IntentAdmitted { key: key2()? },
        ]);
        match refused {
            Err(JournalError::OutOfOrder { .. }) => {}
            Err(other) => {
                return Err(format!("expected an out-of-order refusal, got {other}").into());
            }
            Ok(acks) => {
                let _ = acks;
                return Err("a double admission in one batch must be refused whole".into());
            }
        }
        assert_eq!(
            journal.committed()?.len(),
            before,
            "a refused batch commits nothing"
        );
        drop(journal);
        let mut reopened = FileJournal::open(&path)?;
        assert_eq!(
            reopened.committed()?.len(),
            before,
            "and the file carries none of it"
        );

        // An empty batch is still behind the fence: a handle whose file
        // moved under it answers with the staleness refusal, not with an
        // empty success. `reopened` was opened before the foreign write, so
        // its view is the stale one.
        {
            use std::io::Write as _;
            let mut other = std::fs::OpenOptions::new().append(true).open(&path)?;
            other.write_all(&[0x00, 0x00, 0x00])?;
            other.sync_all()?;
        }
        let empty = reopened.compare_and_append_all(&[]);
        assert!(
            matches!(empty, Err(JournalError::Storage(_))),
            "a stale handle must not answer an empty batch with success"
        );
        Ok(())
    }

    /// Make the next append of `journal` fail the way a device fails: after
    /// the fence has passed, so the refusal is genuinely about the write.
    ///
    /// Growing the file from outside is the other way in, and these tests use
    /// it too — but that is a *fence* refusal, because the handle is stale,
    /// which is a different contract with a different answer. A write that
    /// fails after the fence is what the poison latch exists for, and no
    /// filesystem produces one on demand.
    fn fail_next_write_of(journal: &FileJournal) {
        journal.storage.fail_next_commit();
    }

    /// A key for attempt `n`, so a frame count can be built without
    /// tripping the ladder's one-climb-per-key rule.
    fn attempt_key(n: u64) -> Result<crate::effect::EffectKey, Box<dyn std::error::Error>> {
        Ok(crate::effect::EffectKey::new(
            RunId::from_hex(RUN)?,
            ActionId::from_hex(ACTION)?,
            AttemptId::from_decimal(&n.to_string())?,
            FlowRevision::from_tagged("blake3_256", FLOW_HEX)?,
            ActionDigest::from_tagged("blake3_256", DIGEST_HEX)?,
            EnvironmentId::from_hex(ENV)?,
            EnvironmentEpoch::from_decimal("1")?,
        ))
    }

    /// A second key, so a batch can fail on its own rung.
    fn key2() -> Result<crate::effect::EffectKey, Box<dyn std::error::Error>> {
        Ok(crate::effect::EffectKey::new(
            RunId::from_hex(RUN)?,
            ActionId::from_hex(ACTION)?,
            AttemptId::from_decimal("7")?,
            FlowRevision::from_tagged("blake3_256", FLOW_HEX)?,
            ActionDigest::from_tagged("blake3_256", DIGEST_HEX)?,
            EnvironmentId::from_hex(ENV)?,
            EnvironmentEpoch::from_decimal("1")?,
        ))
    }

    #[test]
    fn a_batch_climbs_a_key_the_disk_already_knows() -> TestResult {
        let path = scratch("batch-committed");
        let _guard = TempGuard(path.clone());
        let key = key()?;
        let mut journal = FileJournal::open(&path)?;
        // One rung committed by ordinary appends.
        journal.compare_and_append(journal.tail(), &EffectEvent::IntentAdmitted { key })?;

        // A batch that continues that same key's climb: the staged kinds must
        // follow the committed ladder, not be shadowed by it.
        let acks = journal.compare_and_append_all(&[
            EffectEvent::DispatchPrepared { key },
            EffectEvent::OutcomeObserved {
                key,
                evidence: EffectEvidence::Applied,
            },
        ])?;
        assert_eq!(acks.len(), 2, "both rungs acknowledged");
        assert_eq!(
            journal.recover().status(key),
            Some(AttemptStatus::Applied),
            "the batch continued the committed ladder"
        );
        Ok(())
    }

    #[test]
    fn a_replayed_journal_is_the_journal_that_was_written() -> TestResult {
        let path = scratch("replay");
        let _guard = TempGuard(path.clone());
        let key = key()?;
        {
            let mut journal = FileJournal::open(&path)?;
            journal.compare_and_append(journal.tail(), &EffectEvent::IntentAdmitted { key })?;
            journal.compare_and_append(journal.tail(), &EffectEvent::DispatchPrepared { key })?;
        }
        let reopened = FileJournal::open(&path)?;
        assert_eq!(reopened.committed()?.len(), 2);
        assert!(!reopened.torn_tail_repaired());
        assert_eq!(
            reopened.recover().status(key),
            Some(AttemptStatus::OutcomeUnknown)
        );
        assert_eq!(
            reopened.durability(),
            DurabilityPromise::ProcessCrash,
            "the file-backed promise is the one a kill can check"
        );
        Ok(())
    }

    #[test]
    fn confirm_outcome_attests_only_its_own_committed_outcome() -> TestResult {
        let path = scratch("confirm");
        let _guard = TempGuard(path.clone());
        let key = key()?;
        let mut journal = FileJournal::open(&path)?;
        journal.compare_and_append(journal.tail(), &EffectEvent::IntentAdmitted { key })?;
        journal.compare_and_append(journal.tail(), &EffectEvent::DispatchPrepared { key })?;
        let ack = journal.compare_and_append(
            journal.tail(),
            &EffectEvent::OutcomeObserved {
                key,
                evidence: EffectEvidence::Applied,
            },
        )?;

        // The honest receipt: the same position, the same fact, a grade this
        // journal can promise.
        journal.confirm_outcome(
            key,
            EffectEvidence::Applied,
            ack.position(),
            DurabilityPromise::ProcessCrash,
        )?;

        // The lying receipt: the right position, the wrong fact.
        let wrong_evidence = journal.confirm_outcome(
            key,
            EffectEvidence::NotApplied,
            ack.position(),
            DurabilityPromise::ProcessCrash,
        );
        assert!(
            matches!(wrong_evidence, Err(JournalError::ReceiptMismatch { .. })),
            "a receipt for an outcome this journal did not commit must be refused"
        );

        // The grade this journal cannot promise.
        let power_loss = journal.confirm_outcome(
            key,
            EffectEvidence::Applied,
            ack.position(),
            DurabilityPromise::PowerLoss,
        );
        assert!(
            matches!(
                power_loss,
                Err(JournalError::ReceiptUnavailable {
                    required: DurabilityPromise::PowerLoss
                })
            ),
            "a file-backed journal must not claim power-loss durability"
        );
        Ok(())
    }

    #[test]
    fn a_reopened_journal_answers_the_latest_outcome_by_key() -> TestResult {
        let path = scratch("outcome-index");
        let _guard = TempGuard(path.clone());
        let key = key()?;
        let mut journal = FileJournal::open(&path)?;
        journal.compare_and_append(journal.tail(), &EffectEvent::IntentAdmitted { key })?;
        journal.compare_and_append(journal.tail(), &EffectEvent::DispatchPrepared { key })?;
        let outcome = EffectEvent::OutcomeObserved {
            key,
            evidence: EffectEvidence::Applied,
        };
        let ack = journal.compare_and_append(journal.tail(), &outcome)?;
        assert_eq!(
            journal.outcome_at(key)?,
            Some((ack.position(), EffectEvidence::Applied)),
            "the live index answers the committed outcome without scanning history"
        );
        drop(journal);
        let reopened = FileJournal::open(&path)?;
        assert_eq!(
            reopened.outcome_at(key)?,
            Some((ack.position(), EffectEvidence::Applied)),
            "the replay rebuilds the outcome index, so a restart keeps it"
        );
        assert_eq!(
            reopened
                .committed_entry(ack.position())?
                .map(|entry| *entry.event()),
            Some(outcome),
            "the acknowledged position still holds the exact outcome"
        );
        Ok(())
    }

    #[test]
    fn a_rotted_length_prefix_is_refused_and_nothing_is_trimmed() -> TestResult {
        let path = scratch("length-rot");
        let _guard = TempGuard(path.clone());
        {
            let mut journal = FileJournal::open(&path)?;
            for attempt in 1u64..=3 {
                let attempt_key_n = attempt_key(attempt)?;
                journal.compare_and_append(
                    journal.tail(),
                    &EffectEvent::IntentAdmitted { key: attempt_key_n },
                )?;
            }
        }
        let before = std::fs::metadata(&path)?.len();
        assert!(before > 3, "three frames are on the disk");

        // Rot in the second frame's length prefix: a complete prefix that no
        // torn write of this journal can produce, because a write only ever
        // leaves a prefix of the bytes it intended. A plausible writer cannot
        // have written this; only rot or a hand can have.
        let mut bytes = std::fs::read(&path)?;
        let first_len = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let len_bytes = u32::try_from(LENGTH_BYTES).unwrap_or(u32::MAX);
        let head_bytes = u32::try_from(HEAD_BYTES).unwrap_or(u32::MAX);
        let second = usize::try_from(
            first_len
                .saturating_add(len_bytes)
                .saturating_add(head_bytes),
        )
        .unwrap_or(bytes.len());
        if second < bytes.len() {
            bytes[second] ^= 0x40;
        }
        std::fs::write(&path, &bytes)?;

        match FileJournal::open(&path) {
            Err(JournalError::Corrupt(corruption)) => {
                assert_eq!(corruption.at(), 1, "the rotted frame is named");
                assert!(
                    matches!(corruption.kind(), CorruptionKind::Framed),
                    "the refusal names the framing, not the contents"
                );
            }
            Err(other) => return Err(format!("expected a corruption refusal, got {other}").into()),
            Ok(_) => return Err("a rotted length prefix must not reopen as a journal".into()),
        }
        assert_eq!(
            std::fs::metadata(&path)?.len(),
            before,
            "refused bytes are never trimmed"
        );
        Ok(())
    }

    #[test]
    fn an_undecodable_committed_frame_is_refused_not_trimmed() -> TestResult {
        let path = scratch("undecodable");
        let _guard = TempGuard(path.clone());
        let key = key()?;
        {
            let mut journal = FileJournal::open(&path)?;
            journal.compare_and_append(journal.tail(), &EffectEvent::IntentAdmitted { key })?;
        }
        // Overwrite the frame's payload, keeping the length and the stored
        // head: well-framed bytes that no longer decode to the event whose
        // head they carry.
        let mut bytes = std::fs::read(&path)?;
        let raw_len = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let payload_len = usize::try_from(raw_len).unwrap_or(usize::MAX);
        let mid = LENGTH_BYTES.saturating_add(payload_len >> 1);
        bytes[mid] = bytes[mid].wrapping_add(0x55);
        std::fs::write(&path, &bytes)?;

        match FileJournal::open(&path) {
            Err(JournalError::Corrupt(corruption)) => {
                assert_eq!(corruption.at(), 0);
                assert!(matches!(
                    corruption.kind(),
                    CorruptionKind::Undecodable | CorruptionKind::Chain(_)
                ));
            }
            Err(other) => return Err(format!("expected a corruption refusal, got {other}").into()),
            Ok(_) => return Err("undecodable committed bytes must not reopen".into()),
        }
        Ok(())
    }

    #[test]
    fn the_ladder_refuses_a_second_prepared_dispatch_from_a_replayed_view() -> TestResult {
        let path = scratch("ladder");
        let _guard = TempGuard(path.clone());
        let key = key()?;
        let mut journal = FileJournal::open(&path)?;
        journal.compare_and_append(journal.tail(), &EffectEvent::IntentAdmitted { key })?;
        journal.compare_and_append(journal.tail(), &EffectEvent::DispatchPrepared { key })?;
        let refused =
            journal.compare_and_append(journal.tail(), &EffectEvent::DispatchPrepared { key });
        match refused {
            Err(JournalError::OutOfOrder {
                expected: Some(EventKind::OutcomeObserved),
                ..
            }) => {}
            Err(other) => {
                return Err(format!("expected an out-of-order refusal, got {other}").into());
            }
            Ok(_) => return Err("a second dispatch of one attempt must be unrepresentable".into()),
        }
        Ok(())
    }

    /// The raw bytes of one frame, built the way the writer builds them, so
    /// the scan-level fault tests can serve exact prefixes without a file.
    fn frame_bytes(
        position: JournalPosition,
        event: &EffectEvent,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let payload = event.to_bytes()?;
        let head = chain(position, event)?;
        let mut frame = Vec::new();
        let len = u32::try_from(payload.len())?;
        frame.extend_from_slice(&len.to_be_bytes());
        frame.extend_from_slice(&payload);
        frame.extend_from_slice(head.as_bytes());
        Ok(frame)
    }

    /// A reader that serves bytes up to an absolute offset and fails every
    /// read past it, the way a storage fault mid-frame presents to the scan.
    struct FaultyAfter {
        inner: std::io::Cursor<Vec<u8>>,
        serve: u64,
    }

    impl Read for FaultyAfter {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.inner.position() >= self.serve {
                return Err(std::io::Error::other("injected storage fault"));
            }
            let remaining = self.serve.saturating_sub(self.inner.position());
            let cap = usize::try_from(remaining).unwrap_or(buf.len());
            let cap = cap.min(buf.len());
            self.inner.read(&mut buf[..cap])
        }
    }

    #[test]
    fn a_storage_fault_mid_frame_is_storage_not_a_torn_tail() -> TestResult {
        let event = EffectEvent::IntentAdmitted { key: key()? };
        let frame = frame_bytes(JournalPosition::genesis(), &event)?;
        // One fault inside the length prefix, one inside the payload, one
        // inside the head: whichever field the fault reaches, the scan must
        // answer with the storage error itself. Every frame read shares one
        // classification, so no fault can be mistaken for the evidence of an
        // interrupted append and repaired destructively.
        let mid_head = frame.len().saturating_sub(HEAD_BYTES).saturating_add(1);
        for serve in [1usize, LENGTH_BYTES.saturating_add(1), mid_head] {
            let serve = u64::try_from(serve)?;
            let mut faulty = FaultyAfter {
                inner: std::io::Cursor::new(frame.clone()),
                serve,
            };
            match scan(&mut faulty, JournalPosition::genesis(), MAX_JOURNAL_EVENTS) {
                Err(JournalError::Storage(_)) => {}
                Err(other) => {
                    return Err(format!(
                        "a fault {serve} bytes in was classified {other}, not storage"
                    )
                    .into());
                }
                Ok((_, stop)) => {
                    return Err(format!(
                        "a fault {serve} bytes in stopped the scan as {stop:?}, not storage"
                    )
                    .into());
                }
            }
        }
        Ok(())
    }

    #[test]
    fn scanning_refuses_a_complete_event_beyond_the_limit() -> TestResult {
        let first = EffectEvent::IntentAdmitted {
            key: attempt_key(1)?,
        };
        let second = EffectEvent::IntentAdmitted {
            key: attempt_key(2)?,
        };
        let genesis = JournalPosition::genesis();
        let first_position = JournalPosition {
            sequence: 1,
            head: chain(genesis, &first)?,
        };
        let mut bytes = frame_bytes(genesis, &first)?;
        bytes.extend_from_slice(&frame_bytes(first_position, &second)?);
        let mut reader = std::io::Cursor::new(bytes);

        match scan(&mut reader, genesis, 1) {
            Err(JournalError::CapacityExceeded {
                resource: JournalLimitKind::Events,
                limit,
                requested,
            }) => {
                assert_eq!(limit, 1);
                assert_eq!(requested, 2);
            }
            Err(other) => return Err(format!("expected event-limit refusal, got {other}").into()),
            Ok((entries, _)) => {
                return Err(format!(
                    "two frames under a one-event limit must refuse, retained {}",
                    entries.len()
                )
                .into());
            }
        }
        Ok(())
    }

    #[test]
    fn open_refuses_an_over_limit_file_without_truncating_it() -> TestResult {
        let path = scratch("byte-limit");
        let _guard = TempGuard(path.clone());
        let file = File::create(&path)?;
        let requested = MAX_JOURNAL_BYTES.saturating_add(1);
        file.set_len(requested)?;
        drop(file);

        match FileJournal::open(&path) {
            Err(JournalError::CapacityExceeded {
                resource: JournalLimitKind::Bytes,
                limit,
                requested: actual,
            }) => {
                assert_eq!(limit, MAX_JOURNAL_BYTES);
                assert_eq!(actual, requested);
            }
            Err(other) => return Err(format!("expected byte-limit refusal, got {other}").into()),
            Ok(_) => return Err("an over-limit journal must not be opened".into()),
        }
        assert_eq!(
            std::fs::metadata(&path)?.len(),
            requested,
            "capacity refusal preserves the existing file byte-for-byte in length"
        );
        Ok(())
    }

    #[test]
    fn batch_admission_refuses_history_over_the_event_limit_without_writing() -> TestResult {
        let path = scratch("event-limit");
        let _guard = TempGuard(path.clone());
        let requested = MAX_JOURNAL_EVENTS.saturating_add(1);
        let mut events = Vec::new();
        for attempt in 1..=requested {
            events.push(EffectEvent::IntentAdmitted {
                key: attempt_key(u64::try_from(attempt)?)?,
            });
        }
        let mut journal = FileJournal::open(&path)?;

        match journal.compare_and_append_all(&events) {
            Err(JournalError::CapacityExceeded {
                resource: JournalLimitKind::Events,
                limit,
                requested: actual,
            }) => {
                assert_eq!(limit, u64::try_from(MAX_JOURNAL_EVENTS)?);
                assert_eq!(actual, u64::try_from(requested)?);
            }
            Err(other) => return Err(format!("expected event-limit refusal, got {other}").into()),
            Ok(acks) => {
                return Err(format!(
                    "over-limit batch must refuse before write, returned {} acknowledgments",
                    acks.len()
                )
                .into());
            }
        }
        assert!(
            journal.committed()?.is_empty(),
            "refused batch leaves the complete prior history unchanged"
        );
        assert_eq!(
            std::fs::metadata(&path)?.len(),
            0,
            "refused batch writes no partial prefix"
        );
        Ok(())
    }

    /// Writes three acknowledged frames and returns the file's bytes plus the
    /// byte offset where the third frame begins.
    fn three_frame_file(path: &Path) -> Result<(Vec<u8>, usize), Box<dyn std::error::Error>> {
        {
            let mut journal = FileJournal::open(path)?;
            for attempt in 1u64..=3 {
                journal.compare_and_append(
                    journal.tail(),
                    &EffectEvent::IntentAdmitted {
                        key: attempt_key(attempt)?,
                    },
                )?;
            }
        }
        let bytes = std::fs::read(path)?;
        let first = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let first_len = LENGTH_BYTES
            .saturating_add(usize::try_from(first)?)
            .saturating_add(HEAD_BYTES);
        let next = u64::try_from(first_len)?;
        let mut second_prefix = [0u8; LENGTH_BYTES];
        for (slot, offset) in second_prefix.iter_mut().zip(0u64..) {
            let index = next.saturating_add(offset);
            *slot = *bytes
                .get(usize::try_from(index)?)
                .ok_or("fixture file is shorter than its own first frame")?;
        }
        let second = u32::from_be_bytes(second_prefix);
        let second_len = LENGTH_BYTES
            .saturating_add(usize::try_from(second)?)
            .saturating_add(HEAD_BYTES);
        Ok((bytes, first_len.saturating_add(second_len)))
    }

    #[test]
    fn an_inflated_length_over_a_complete_final_frame_is_refused_not_trimmed() -> TestResult {
        let path = scratch("inflated-length");
        let _guard = TempGuard(path.clone());
        let (mut bytes, third_start) = three_frame_file(&path)?;
        let before = bytes.len();

        // Every length a scan can meet at the last frame's prefix: the
        // maximum the frame bound allows, one over it, and zero. None of them
        // is a torn append — a killed writer leaves a prefix of the bytes it
        // intended, and its completed prefix is always the true length — so
        // each must be refused with the file left byte-for-byte alone. The
        // inflation case is the destructive one: the declared payload runs
        // past the end of the file, and the tail looks torn to a reader that
        // trusts the prefix.
        let inflated = u32::try_from(MAX_FRAME_BYTES)?;
        let over_limit = inflated.saturating_add(1);
        for (name, replacement) in [
            ("the maximum legal length", inflated),
            ("one byte over the limit", over_limit),
            ("a zero length", 0u32),
        ] {
            for (slot, offset) in bytes[third_start..]
                .iter_mut()
                .take(LENGTH_BYTES)
                .zip(0usize..)
            {
                *slot = replacement.to_be_bytes()[offset];
            }
            std::fs::write(&path, &bytes)?;

            match FileJournal::open(&path) {
                Err(JournalError::Corrupt(corruption)) => {
                    assert_eq!(corruption.at(), 2, "{name}: the lying frame is named");
                    assert!(
                        matches!(corruption.kind(), CorruptionKind::Framed),
                        "{name}: the refusal names the framing, got {:?}",
                        corruption.kind()
                    );
                }
                Err(other) => {
                    return Err(
                        format!("{name}: expected a corruption refusal, got {other}").into(),
                    );
                }
                Ok(_) => {
                    return Err(
                        format!("{name}: a lying length must not reopen as a journal").into(),
                    );
                }
            }
            assert_eq!(
                std::fs::metadata(&path)?.len(),
                u64::try_from(before)?,
                "{name}: refused bytes are never trimmed"
            );
        }
        Ok(())
    }

    #[test]
    fn a_short_final_frame_is_still_repaired_as_a_torn_tail() -> TestResult {
        let path = scratch("torn-tail");
        let _guard = TempGuard(path.clone());
        let (bytes, third_start) = three_frame_file(&path)?;
        let complete_len = u64::try_from(third_start)?;

        // A real interrupted append: the file ends mid-frame. This tail was
        // never acknowledged, so the reopen repairs it — and the repair must
        // survive the corruption distinction, not be swallowed by it.
        let torn = &bytes[..third_start.saturating_add(10)];
        std::fs::write(&path, torn)?;

        let reopened = FileJournal::open(&path)?;
        assert!(
            reopened.torn_tail_repaired(),
            "an interrupted append is repaired, not refused"
        );
        assert_eq!(
            reopened.committed()?.len(),
            2,
            "the two kept frames survive"
        );
        assert_eq!(
            std::fs::metadata(&path)?.len(),
            complete_len,
            "the repair restores the prefix every acknowledgment names"
        );
        Ok(())
    }

    #[test]
    fn a_failed_batch_write_latches_the_poison_like_a_single_append() -> TestResult {
        let path = scratch("batch-poison");
        let _guard = TempGuard(path.clone());
        // Every call gets its own attempt key: the ladder refuses a second
        // rung one attempt, and that refusal must never mask the poison
        // fence these assertions are about.
        let event = |attempt: u64| -> Result<EffectEvent, Box<dyn std::error::Error>> {
            Ok(EffectEvent::IntentAdmitted {
                key: attempt_key(attempt)?,
            })
        };

        // The single append's policy, pinned for parity: a write that fails
        // reports an unknown outcome and latches the poison, and the handle
        // refuses everything after it.
        let mut journal = FileJournal::open(&path)?;
        journal.compare_and_append(journal.tail(), &event(200)?)?;
        fail_next_write_of(&journal);
        match journal.compare_and_append(journal.tail(), &event(201)?) {
            Err(JournalError::OutcomeUnknown { .. }) => {}
            Err(other) => return Err(format!("expected an unknown outcome, got {other}").into()),
            Ok(_) => return Err("a failed write must not acknowledge".into()),
        }
        match journal.compare_and_append(journal.tail(), &event(201)?) {
            Err(error) => assert!(
                error.to_string().contains("previous append failed"),
                "the poisoned handle refused with its own fence, not {error}"
            ),
            Ok(_) => return Err("a poisoned handle must refuse further appends".into()),
        }
        drop(journal);

        // The batch must answer with the same two facts: the failure is an
        // unknown outcome — a prefix or all of the batch may be on the disk —
        // and the handle is poisoned afterwards, not silently reusable.
        let mut journal = FileJournal::open(&path)?;
        journal.compare_and_append(journal.tail(), &event(300)?)?;
        fail_next_write_of(&journal);
        match journal.compare_and_append_all(&[event(301)?, event(302)?]) {
            Err(JournalError::OutcomeUnknown { .. }) => {}
            Err(other) => {
                return Err(format!(
                    "a failed batch write answered {other}, not the single append's unknown outcome"
                )
                .into());
            }
            Ok(acks) => {
                let _ = acks;
                return Err("a failed batch write must not acknowledge".into());
            }
        }
        match journal.compare_and_append_all(&[event(303)?]) {
            Err(error) => assert!(
                error.to_string().contains("previous append failed"),
                "the poisoned handle refused with its own fence, not {error}"
            ),
            Ok(_) => return Err("a poisoned handle must refuse further batches".into()),
        }
        Ok(())
    }

    /// A storage device that has stopped answering must not stop the task that
    /// is waiting on it.
    ///
    /// This is finding R07's acceptance case, and the instrument is chosen so
    /// there is no timing race anywhere in it. The journal is opened on a
    /// stalled device, so the flush provably cannot complete until
    /// [`FileJournal::release_storage`] is called — the append is not "slow",
    /// it is *parked*, and no amount of waiting would finish it. The test then
    /// drives the same current-thread runtime the finding is about, counts how
    /// many times an unrelated task ran while the append was outstanding, and
    /// only then releases the storage.
    ///
    /// On a tree whose durable write runs on the awaiting thread, the append is
    /// not merely slow: it never returns, so the release is never reached. That
    /// is the negative control, and it is why the assertion is about the count
    /// and not about a duration.
    #[test]
    #[cfg(feature = "rt")]
    fn a_stalled_device_does_not_stop_the_task_waiting_on_it() -> TestResult {
        use std::future::poll_fn;
        use std::task::Poll;
        use std::time::{Duration, Instant};

        let path = scratch("stalled-device");
        let _guard = TempGuard(path.clone());
        let mut journal = FileJournal::open_with_stalled_storage(&path)?;
        // Taken before the append starts: once the future holds the journal's
        // borrow, the journal itself is unreachable for the whole wait.
        let gate = journal.storage_gate();
        let event = EffectEvent::IntentAdmitted {
            key: attempt_key(900)?,
        };
        let tail = journal.tail();
        let runtime = crate::rt::runtime::Runtime::new()?;

        let mut append = Box::pin(journal.compare_and_append_async(tail, &event));
        let mut last = Instant::now();
        let mut longest = Duration::ZERO;
        let mut ticks = 0u64;

        let ack = runtime.block_on(poll_fn(|cx| {
            // The unrelated task, sampled on every wakeup. Its gap is the
            // runtime's unavailability, which is the thing under test.
            let now = Instant::now();
            longest = longest.max(now.duration_since(last));
            last = now;
            ticks = ticks.saturating_add(1);

            match append.as_mut().poll(cx) {
                Poll::Ready(answer) => Poll::Ready(answer),
                Poll::Pending => {
                    // Nothing else is queued on this runtime, so the wakeup
                    // that stands in for "an unrelated task got a turn" has to
                    // come from here. Without it the task parks on the stalled
                    // device and never reaches the release below — which is
                    // the point: the device is genuinely not answering.
                    if ticks >= 64 {
                        gate.release();
                    }
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            }
        }))?;
        // The future holds the handle's borrow, and it is the future's drop
        // that tells the storage owner nobody is waiting any more.
        drop(append);

        assert!(
            ticks >= 64,
            "the runtime stopped after {ticks} wakeups before the parked flush \
             was released: the durable write is running on the awaiting thread"
        );
        assert_eq!(
            ack.promise(),
            DurabilityPromise::ProcessCrash,
            "the acknowledgment still has to be earned, and a released device earns it"
        );
        assert!(
            longest < Duration::from_millis(500),
            "the executor was unavailable for {longest:?} while a flush that \
             could not finish was outstanding"
        );
        assert_eq!(
            EffectJournal::committed(&journal)?.len(),
            1,
            "the frame was folded in"
        );
        drop(journal);
        let reopened = FileJournal::open(&path)?;
        assert_eq!(
            EffectJournal::committed(&reopened)?.len(),
            1,
            "the released frame must be on the disk, not only in the handle"
        );
        Ok(())
    }

    /// A caller that walks away from a durable append it started leaves the
    /// handle unable to say whether the bytes landed, and the handle must say
    /// so rather than carry on.
    ///
    /// The finding's second half: dropping a waiter is not proof that the
    /// append stopped. This drives that case and then checks the two things
    /// that make it safe — the handle refuses to append again, and a reopen
    /// reconciles by reading the file back, with no second frame for the same
    /// attempt.
    #[test]
    #[cfg(feature = "rt")]
    fn a_dropped_waiter_poisons_the_handle_and_a_reopen_does_not_duplicate() -> TestResult {
        use std::future::poll_fn;
        use std::task::Poll;

        let path = scratch("dropped-waiter");
        let _guard = TempGuard(path.clone());
        let mut journal = FileJournal::open_with_stalled_storage(&path)?;
        let gate = journal.storage_gate();
        let event = EffectEvent::IntentAdmitted {
            key: attempt_key(901)?,
        };
        let tail = journal.tail();
        let runtime = crate::rt::runtime::Runtime::new()?;

        // Poll the append exactly once, so the storage owner has been handed
        // the request, then drop it: the caller is gone while the bytes are
        // still moving. This is the cancellation the finding names, and the
        // device is parked so the ordering is the one under test rather than a
        // race with a fast disk — the owner cannot finish before the drop.
        let mut append = Box::pin(journal.compare_and_append_async(tail, &event));
        let started = runtime.block_on(poll_fn(|cx| {
            let polled = append.as_mut().poll(cx);
            if polled.is_pending() {
                cx.waker().wake_by_ref();
            }
            Poll::Ready(polled)
        }));
        assert!(
            started.is_pending(),
            "the append completed before it could be abandoned"
        );
        drop(append);
        gate.release();

        // The owner finishes the write it was given and finds no waiter. The
        // handle must be poisoned once it has. Polled against a deadline
        // rather than a spin count: what is being waited for is a `sync_all`,
        // which on a real device is milliseconds, so a yield budget would be
        // racing the disk rather than the owner.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !journal.storage.poisoned() {
            if std::time::Instant::now() >= deadline {
                return Err("the storage owner never latched the poison".into());
            }
            std::thread::yield_now();
        }
        match journal.compare_and_append(journal.tail(), &event) {
            Err(error) => assert!(
                error.to_string().contains("previous append failed"),
                "an abandoned append must refuse with its own fence, not {error}"
            ),
            Ok(_) => return Err("a handle that lost its waiter must refuse".into()),
        }
        drop(journal);

        // The reopen is the reconciliation: it reads what actually landed, and
        // the ladder then refuses the same rung again, so a retry cannot write
        // a second frame for one attempt.
        let mut reopened = FileJournal::open(&path)?;
        let landed = EffectJournal::committed(&reopened)?.len();
        assert!(
            landed <= 1,
            "an abandoned append wrote {landed} frames for one attempt"
        );
        let same = EffectEvent::IntentAdmitted {
            key: attempt_key(901)?,
        };
        assert!(
            reopened.compare_and_append(reopened.tail(), &same).is_err(),
            "the replayed ladder must refuse a rung the file already records"
        );
        Ok(())
    }
}
