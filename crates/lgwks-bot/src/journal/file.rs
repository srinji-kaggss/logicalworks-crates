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
//! the file is never loaded whole, and the loop is bounded by the file's own
//! length because every iteration consumes at least one byte.
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

use super::{
    ChainBreak, DurabilityPromise, DurableAck, EffectEvent, EffectEvidence, EffectJournal,
    EventKind, JournalEntry, JournalError, JournalPosition, Recovered, chain, next_allowed_of,
    recover,
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
    /// never an interrupted append.
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
enum ScanStop {
    /// Clean end of file at the given offset.
    Complete(u64),
    /// The bytes from `offset` onward are an interrupted append: torn.
    Torn(u64),
}

/// Read frames from `reader`, stopping at the first torn frame.
///
/// Returns the decoded entries, or the corruption that refuses the file.
/// The scan is streaming and every iteration consumes at least one byte, so
/// the loop is bounded by the file's own length.
fn scan(
    reader: &mut impl Read,
    previous: JournalPosition,
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
            Some(_) => {}
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
        if reader.read_exact(&mut payload).is_err() {
            return Ok((entries, ScanStop::Torn(offset)));
        }
        let mut head = [0u8; HEAD_BYTES];
        if reader.read_exact(&mut head).is_err() {
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
    /// The append handle. Append mode writes at the end of the file whatever
    /// a concurrent reader believes, so no cursor has to survive the replay.
    file: File,
    /// The replayed history, which is also the append fence's view.
    committed: Vec<JournalEntry>,
    /// The last kind recorded per key, the append fence's index, so an
    /// append's ladder check does not walk the replayed history.
    ladder: HashMap<crate::effect::EffectKey, EventKind>,
    /// Byte length of the acknowledged prefix on disk.
    disk_len: u64,
    /// Whether a write of this handle failed after bytes may have reached
    /// the disk. The handle's view can no longer be trusted against the
    /// file's, so appends are refused until a reopen replays the truth.
    write_failed: bool,
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

impl FileJournal {
    /// Open the journal at `path`, creating the file when it does not exist
    /// and replaying it when it does.
    ///
    /// A torn tail — an append a killed writer never finished — is truncated
    /// back to the last whole frame, because it was never acknowledged. A
    /// lying frame — undecodable bytes, or a head that does not follow — is
    /// refused, because it may have been. The distinction is reported: after
    /// a repair, [`FileJournal::torn_tail_repaired`] is `true`.
    ///
    /// # Errors
    ///
    /// [`JournalError::Storage`] when the file cannot be opened, read or
    /// repaired; [`JournalError::Corrupt`] when committed bytes are refused.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        let path = path.as_ref().to_path_buf();
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&path)
            .map_err(JournalError::Storage)?;

        file.seek_read_zero()?;
        let mut reader = BufReader::new(&mut file);
        let (entries, stop) = scan(&mut reader, JournalPosition::genesis())?;
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
        };

        let ladder = entries
            .iter()
            .map(|entry| (entry.event().key(), entry.event().kind()))
            .collect();

        Ok(Self {
            path,
            file,
            committed: entries,
            ladder,
            disk_len: acked_len,
            write_failed: false,
            torn_tail_repaired,
        })
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

    /// Append a whole batch of events for one `sync_all`, the group commit
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
    /// an unacknowledged prefix of the batch on the disk; the handle refuses
    /// further appends, and the next open repairs the prefix as a torn tail
    /// — the same disposition a killed writer's bytes get.
    ///
    /// A controller that records several rungs of one attempt in a single
    /// turn pays one flush instead of one per rung: measured on this
    /// machine's file system, a four-rung attempt through four appends costs
    /// four flushes at about 3.3 ms each, and through this batch one.
    ///
    /// # Errors
    ///
    /// Every [`JournalError`] a single append can produce. A rung's refusal
    /// is returned and no bytes are written; a [`JournalError::Storage`]
    /// failure of the write itself may have written a prefix, which no
    /// acknowledgment names and the next open repairs.
    pub fn compare_and_append_all(
        &mut self,
        events: &[EffectEvent],
    ) -> Result<Vec<DurableAck>, JournalError> {
        // The staleness fence runs once for the batch, ahead of everything
        // including the empty case: a stale handle answers for itself, not
        // with an empty success. Between the checks and the single write
        // this controller holds the file's only moving part.
        if self.write_failed {
            return Err(JournalError::Storage(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "a previous append failed while writing; the file may hold \
                 unacknowledged bytes, reopen to replay",
            )));
        }
        let on_disk = self.file.metadata().map_err(JournalError::Storage)?.len();
        if on_disk != self.disk_len {
            return Err(JournalError::Storage(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the journal file moved under this controller; reopen before appending",
            )));
        }
        if events.is_empty() {
            return Ok(Vec::new());
        }

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
            let sequence = position
                .sequence()
                .checked_add(1)
                .ok_or(JournalError::Exhausted)?;
            let head = chain(position, event)?;
            position = JournalPosition { sequence, head };
            let payload = event.to_bytes().map_err(JournalError::Encoding)?;
            if payload.len() > MAX_FRAME_BYTES {
                return Err(JournalError::Storage(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "the event exceeds this journal's frame bound",
                )));
            }
            let payload_len = u32::try_from(payload.len()).map_err(|_| {
                JournalError::Storage(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "the event exceeds this journal's frame bound",
                ))
            })?;
            frames.extend_from_slice(&payload_len.to_be_bytes());
            frames.extend_from_slice(&payload);
            frames.extend_from_slice(head.as_bytes());
            let frame_len = LENGTH_BYTES
                .saturating_add(payload.len())
                .saturating_add(HEAD_BYTES);
            pending.push((position, frame_len));
        }

        // One write, one flush, then the in-memory commit, then the acks.
        self.file
            .write_all(&frames)
            .map_err(JournalError::Storage)?;
        self.file.sync_all().map_err(JournalError::Storage)?;

        let mut acks = Vec::with_capacity(pending.len());
        for (event, entry) in events.iter().zip(&pending) {
            let (position, frame_len) = *entry;
            self.committed.push(JournalEntry::new(position, *event));
            self.ladder.insert(event.key(), event.kind());
            self.disk_len = self
                .disk_len
                .saturating_add(u64::try_from(frame_len).unwrap_or(u64::MAX));
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
        let actual = self.tail();
        if expected_tail != actual {
            return Err(JournalError::TailMismatch {
                expected: expected_tail,
                actual,
            });
        }
        let key = event.key();
        let attempted = event.kind();
        let expected = next_allowed_of(self.ladder.get(&key).copied());
        if expected != Some(attempted) {
            return Err(JournalError::OutOfOrder {
                key: Box::new(key),
                expected,
                attempted,
            });
        }
        // The staleness fence: the file on the disk must still be exactly the
        // prefix this controller holds, or the controller's view is stale and
        // its append would branch the chain. A write this handle already
        // failed mid-way counts as stale by definition: bytes may be on the
        // disk that no acknowledgment names, and only a reopen replays the
        // truth.
        if self.write_failed {
            return Err(JournalError::Storage(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "a previous append failed while writing; the file may hold \
                 unacknowledged bytes, reopen to replay",
            )));
        }
        let on_disk = self.file.metadata().map_err(JournalError::Storage)?.len();
        if on_disk != self.disk_len {
            return Err(JournalError::Storage(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the journal file moved under this controller; reopen before appending",
            )));
        }

        let sequence = actual
            .sequence()
            .checked_add(1)
            .ok_or(JournalError::Exhausted)?;
        let head = chain(actual, event)?;
        let position = JournalPosition { sequence, head };
        let payload = event.to_bytes().map_err(JournalError::Encoding)?;
        if payload.len() > MAX_FRAME_BYTES {
            return Err(JournalError::Storage(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the event exceeds this journal's frame bound",
            )));
        }
        let payload_len = u32::try_from(payload.len()).map_err(|_| {
            JournalError::Storage(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the event exceeds this journal's frame bound",
            ))
        })?;

        let mut frame = Vec::with_capacity(
            LENGTH_BYTES
                .saturating_add(payload.len())
                .saturating_add(HEAD_BYTES),
        );
        frame.extend_from_slice(&payload_len.to_be_bytes());
        frame.extend_from_slice(&payload);
        frame.extend_from_slice(head.as_bytes());

        // The write and the sync happen before the acknowledgment: after this
        // returns, the bytes are through the file system, and a kill of this
        // process cannot take the fact back out of the file. A failure here
        // may have left a prefix of the frame on the disk, so the handle
        // poisons itself rather than keep appending on a view it can no
        // longer vouch for.
        if let Err(error) = self.file.write_all(&frame) {
            self.write_failed = true;
            return Err(JournalError::Storage(error));
        }
        if let Err(error) = self.file.sync_all() {
            self.write_failed = true;
            return Err(JournalError::Storage(error));
        }

        self.committed.push(JournalEntry::new(position, *event));
        self.ladder.insert(key, attempted);
        self.disk_len = self
            .disk_len
            .saturating_add(u64::try_from(frame.len()).unwrap_or(u64::MAX));
        Ok(DurableAck::new(position, self.durability()))
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

        // All-or-nothing: a batch whose second rung violates the ladder is
        // refused whole, and nothing of it reaches the file.
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
}
