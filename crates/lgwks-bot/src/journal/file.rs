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
//! # A length that lies is refused, whichever way it lies
//!
//! A complete length prefix over a frame the file cannot hold is byte-identical in
//! two files: an append a writer never finished, and an acknowledged frame whose
//! prefix was changed afterwards (`L` to `L + k`, for a final frame or one with
//! frames behind it). The prefix cannot say which. The stored head can, because a
//! head is a hash of the previous head and the payload and is reproduced only by
//! bytes a writer really framed. So an early end after a complete prefix, in the
//! payload **or in the head**, is resolved by `resolve_ambiguous_tail` on the
//! bytes themselves, through the shared grammar's `holds_acknowledged_frame`:
//!
//! - if some payload length under the bytes present reproduces the head stored
//!   behind it, a frame was on the disk and the prefix lied: the file is **refused
//!   with [`JournalError::Corrupt`] and left untouched**;
//! - if a later complete frame authenticates against the head just before it, the
//!   same is true even when the cut frame is itself damaged, because failing to
//!   authenticate one candidate does not prove that no acknowledged frame follows;
//! - only when neither holds are the bytes a prefix of one cut-short append, and
//!   they are trimmed. This is the disposition etcd's WAL gives a torn final
//!   record, and the one SQLite's and PostgreSQL's recovery give a frame that
//!   fails its chain: nothing that authenticates is rewritten.
//!
//! What this does **not** do, stated so the claim is no larger than the test:
//! a final frame whose prefix was changed *and* whose payload or head was also
//! damaged authenticates as nothing, has nothing behind it to authenticate, and
//! is indistinguishable from a torn append. That takes two independent faults in
//! one frame and is trimmed. A journal also cannot tell a hand that truncates the
//! file mid-frame from a crash. Owner of the one-fault case:
//! [#262](https://github.com/srinji-kaggss/logicalworks-crates/issues/262).
//!
//! # Bounds
//!
//! One frame may not exceed [`MAX_FRAME_BYTES`]; a journal whose events were
//! always a key plus a verdict cannot approach it, so a complete over-long
//! length field is refused as rot, and only a partial one is a torn write. The
//! scan is streaming:
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
//! that no longer matches the file is refused.
//!
//! On top of that, [`FileJournal::open`] takes the file's **exclusive advisory
//! lock** through `File::try_lock` *before* it reads, scans or repairs a byte,
//! and refuses a second opener with [`JournalError::Locked`] rather than
//! scanning a file somebody else is writing. The lock lives for the lifetime of
//! the returned journal, and a writer that dies releases it, so the next
//! `open` succeeds.
//!
//! What that lock is, stated precisely, because the difference matters:
//!
//! - It is an **advisory** lock. It binds writers that come through
//!   [`FileJournal::open`]. A writer that never asks for the lock is outside
//!   its reach entirely, and a hostile editor that truncates or rewrites the
//!   file behind the owner's back is not detected by it.
//! - It is **lifetime-scoped and local**. It is an operating-system file lock
//!   on one host. It is not a distributed lease, and two controllers on two
//!   hosts pointed at one network file are not serialized by it.
//! - It depends on the **filesystem** implementing advisory locks. A
//!   filesystem that does not is not refused; see [`JournalError::Locked`],
//!   which states this.
//!
//! Both limits are stated on the error rather than hidden. The stored heads
//! additionally make an interleaved write detectable on the next open rather
//! than silently accepted, and that detection is permanent:
//! [`JournalError::Corrupt`] is never trimmed and no tool in this module
//! rewrites refused bytes, so a bricked file stays bricked until an operator
//! takes it in hand.
//!
//! The cross-process half of this — that the fence actually holds between two
//! live processes and is reacquired when the holder dies — is exercised by
//! `tests/journal_writer_fence.rs`, which re-executes this test binary as a
//! second process rather than simulating one.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use super::frame::{HEAD_BYTES, LENGTH_BYTES, Piece, Prefix, read_exact_or_eof};
use super::owner::{StorageGate, StorageOwner};
use super::{
    ChainBreak, DurabilityPromise, DurableAck, EffectEvent, EffectEvidence, EffectJournal,
    EventKind, JournalEntry, JournalError, JournalLimitKind, JournalPosition, MAX_JOURNAL_BYTES,
    MAX_JOURNAL_EVENTS, Recovered, chain, check_append_order, next_allowed_of, recover,
};
use lgwks_std::wire::{WireError, from_bytes};

/// The largest frame this journal will read or write.
///
/// Events are a key, a verdict and a digest; a real frame is a few hundred
/// bytes. A length field beyond this bound does not name a frame this
/// journal writes, so a complete prefix carrying one is refused as rot; a
/// partial prefix is a torn tail.
const MAX_FRAME_BYTES: usize = 64 * 1024;

/// One whole piece of a frame's bytes: the payload or the head.
///
/// The crate-wide grammar names the pieces it reads; this is the same shape under
/// the name this module's own error vocabulary uses, so `Interrupted` here and
/// there cannot be two different classifications of the same bytes.
type FramePiece = Piece;

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
    /// The file ended inside a frame whose declared length ran past the end, in
    /// its payload or in its head. Either an append the writer never finished, or
    /// a complete frame whose length prefix lies — the two are byte-identical
    /// here, and [`FileJournal::open`] resolves which with the frame's own head.
    AmbiguousTail {
        /// Where the ambiguous frame's length prefix begins.
        offset: u64,
    },
}

/// Read a fixed-size piece of a frame, classifying the ending.
///
/// The crate-wide grammar in [`super::frame`] makes the classification, so this
/// journal and the run store cannot disagree about which early end is repairable;
/// this wrapper only maps the device's refusal onto this module's error vocabulary.
fn read_exact_classified(
    reader: &mut impl Read,
    buf: &mut [u8],
) -> Result<FramePiece, JournalError> {
    super::frame::read_piece(reader, buf).map_err(JournalError::Storage)
}

/// Decide what an end of file inside a declared frame means.
///
/// The scan read a legal length prefix, then hit the end of the file before the
/// declared payload or the head behind it was whole. Two different files are
/// byte-identical here: an append the writer never finished, and an acknowledged
/// frame whose length prefix lies. The stored head decides, through the shared
/// grammar: bytes that reproduce a head under any payload length, or a later frame
/// that authenticates, were acknowledged and the file is refused untouched;
/// anything else is the prefix of one cut-short append and the caller trims it.
///
/// The `index` and `position` are the refused frame's would-be index in the file
/// and the chain position of the last committed entry before it. The answer is the
/// offset to trim back to, which is where the cut frame begins.
///
/// # Errors
///
/// [`JournalError::Corrupt`] when the tail holds an acknowledged frame under a
/// lying length; [`JournalError::Storage`] when the disk refuses, or when the file
/// holds more behind the prefix than any cut-short frame could have left (it moved
/// while it was being opened).
fn resolve_ambiguous_tail(
    file: &mut File,
    offset: u64,
    position: JournalPosition,
    index: u64,
) -> Result<u64, JournalError> {
    let acknowledged = super::frame::cut_holds_acknowledged(
        file,
        offset,
        &position.head(),
        MAX_FRAME_BYTES,
        JournalError::Storage,
        |previous, payload| Some(super::chain_over_bytes(previous, payload)),
    )?;
    if acknowledged {
        let refusal = Err(JournalError::Corrupt(Box::new(Corruption::new(
            index,
            CorruptionKind::Framed,
        ))));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "resolve_ambiguous_tail: the tail holds an acknowledged frame under a lying length");
        return refusal;
    }
    Ok(offset)
}

/// A whole, decodable frame read from a journal file.
struct Whole {
    /// The event the payload decoded to.
    event: EffectEvent,
    /// The chain head the frame recorded for itself.
    head: [u8; HEAD_BYTES],
    /// The payload length the frame declared.
    payload_len: usize,
}

/// Why a frame read halted the scan, before the offset it halted at is known.
#[derive(Clone, Copy)]
enum Halt {
    /// A clean end of file.
    Complete,
    /// An interrupted length prefix.
    Torn,
    /// A payload or head that ran past the end of the file under a complete
    /// length prefix.
    Ambiguous,
}

impl Halt {
    /// The stop this halt is once the offset of the frame being read is known.
    const fn at(self, offset: u64) -> ScanStop {
        match self {
            Self::Complete => ScanStop::Complete(offset),
            Self::Torn => ScanStop::Torn(offset),
            Self::Ambiguous => ScanStop::AmbiguousTail { offset },
        }
    }
}

/// Read the frame at ordinal `index`, with `held` entries already read.
///
/// A complete prefix naming a frame this journal never writes is not a torn
/// append: a write leaves only a prefix of its bytes, so the length a writer
/// did complete is the length it intended, and that is always a frame this
/// journal writes. It is refused, never trimmed, because the bytes after it may
/// be acknowledged. A declared payload that runs past the end of the file is
/// left to `resolve_ambiguous_tail`, which decides on the frame's own stored
/// head and never by trusting the prefix. The same holds when the payload is
/// whole and the head behind it is short: that is a length that is one to
/// thirty-two bytes too long over a complete frame as readily as it is an
/// append cut inside its head.
fn next_frame(
    reader: &mut impl Read,
    held: usize,
    max_events: usize,
    index: u64,
) -> Result<Result<Whole, Halt>, JournalError> {
    let mut prefix = [0u8; LENGTH_BYTES];
    match super::frame::read_prefix(reader, &mut prefix).map_err(JournalError::Storage)? {
        Prefix::Eof => return Ok(Err(Halt::Complete)),
        Prefix::Torn => return Ok(Err(Halt::Torn)),
        Prefix::Full => {
            let requested = u64::try_from(held).unwrap_or(u64::MAX).saturating_add(1);
            if requested > u64::try_from(max_events).unwrap_or(u64::MAX) {
                let refusal = Err(JournalError::CapacityExceeded {
                    resource: JournalLimitKind::Events,
                    limit: u64::try_from(max_events).unwrap_or(u64::MAX),
                    requested,
                });
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "next_frame: the file holds more events than the ceiling");
                return refusal;
            }
        }
    }
    let payload_len = super::frame::declared_length(&prefix);
    if !super::frame::is_possible_length(payload_len, MAX_FRAME_BYTES) {
        let refusal = Err(JournalError::Corrupt(Box::new(Corruption::new(
            index,
            CorruptionKind::Framed,
        ))));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), payload_len, "next_frame: the declared length is one this journal never writes");
        return refusal;
    }
    let mut payload = vec![0u8; payload_len];
    if let FramePiece::Interrupted = read_exact_classified(reader, &mut payload)? {
        return Ok(Err(Halt::Ambiguous));
    }
    let mut head = [0u8; HEAD_BYTES];
    if let FramePiece::Interrupted = read_exact_classified(reader, &mut head)? {
        return Ok(Err(Halt::Ambiguous));
    }
    let event = from_bytes::<EffectEvent, WireError>(&payload).map_err(|error| {
        lgwks_std::trace::debug!(?error, index, "next_frame: the payload did not decode");
        JournalError::Corrupt(Box::new(Corruption::new(
            index,
            CorruptionKind::Undecodable,
        )))
    })?;
    Ok(Ok(Whole {
        event,
        head,
        payload_len,
    }))
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
        let Whole {
            event,
            head,
            payload_len,
        } = match next_frame(reader, entries.len(), max_events, index)? {
            Ok(whole) => whole,
            Err(halt) => return Ok((entries, halt.at(offset))),
        };
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
            let refusal = Err(JournalError::Corrupt(Box::new(Corruption::new(
                index,
                CorruptionKind::Chain(ChainBreak::Disagreement {
                    at: recorded.sequence(),
                    recorded,
                    recomputed,
                }),
            ))));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "scan: returning an error to the caller");
            return refusal;
        }
        entries.push(JournalEntry::new(recorded, event));
        position = recorded;
        offset = offset.saturating_add(super::frame::framed_len(payload_len));
        index = index.saturating_add(1);
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
    /// The thread that holds the file, and the one door an append reaches the
    /// disk through. It also carries the poison, so the handle never has to
    /// guess whether a write it did not see succeed.
    storage: StorageOwner<(), ()>,
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

/// A bounded, streaming replay of a journal file.
///
/// [`FileJournal::open`] must materialize the complete history, because the
/// append fence, the ladder index and the outcome index are all built from it.
/// A caller that only needs to *fold* the record — a recovery pass, an audit, a
/// migration — does not need that, and this is the door for it: it reads one
/// frame at a time from its own read-only descriptor and retains at most one
/// decoded event, so its memory is the largest single frame rather than the
/// whole log.
///
/// It applies the same frame validation the open scan does — an impossible or
/// lying length prefix and a head that does not follow are refusals, an early
/// end is a torn tail that ends the stream — so a streamed replay cannot accept
/// bytes the handle would refuse. It is bounded by [`MAX_JOURNAL_EVENTS`]; a
/// file longer than that ends the stream with
/// [`JournalError::CapacityExceeded`] rather than continuing past the ceiling.
pub struct Replay {
    /// The streaming frame reader, on its own descriptor.
    reader: BufReader<File>,
    /// The position the next event must chain from.
    position: JournalPosition,
    /// How many events the stream has already yielded, for the event ceiling.
    yielded: u64,
    /// Where the next frame's length prefix begins.
    offset: u64,
    /// Whether the stream has reached its end (clean, torn, or refused).
    done: bool,
}

impl Replay {
    /// Open a fresh read-only descriptor at `path` and stream its frames.
    ///
    /// # Errors
    ///
    /// [`JournalError::Storage`] when the file cannot be opened or seeks.
    fn open(path: &Path) -> Result<Self, JournalError> {
        let mut file = OpenOptions::new()
            .read(true)
            .open(path)
            .map_err(JournalError::Storage)?;
        file.seek_read_zero()?;
        Ok(Self {
            reader: BufReader::new(file),
            position: JournalPosition::genesis(),
            yielded: 0,
            offset: 0,
            done: false,
        })
    }

    /// Read exactly one more committed event, or end the stream.
    ///
    /// `None` means the acknowledged prefix is exhausted at a clean end or a
    /// torn tail, which is never an error: the tail was never acknowledged, so
    /// the stream of committed events is complete without it.
    fn read_one(&mut self) -> Option<Result<EffectEvent, JournalError>> {
        let index = self.yielded;
        let mut prefix = [0u8; LENGTH_BYTES];
        match read_exact_or_eof(&mut self.reader, &mut prefix) {
            Err(error) => return Some(Err(error)),
            Ok(None) => return None,
            Ok(Some(read_len)) if read_len < LENGTH_BYTES => return None,
            Ok(Some(_)) => {}
        }
        let limit = u64::try_from(MAX_JOURNAL_EVENTS).unwrap_or(u64::MAX);
        if index >= limit {
            return Some(Err(JournalError::CapacityExceeded {
                resource: JournalLimitKind::Events,
                limit,
                requested: index.saturating_add(1),
            }));
        }
        let payload_len = usize::try_from(u32::from_be_bytes(prefix)).unwrap_or(usize::MAX);
        if payload_len == 0 || payload_len > MAX_FRAME_BYTES {
            return Some(Err(JournalError::Corrupt(Box::new(Corruption::new(
                index,
                CorruptionKind::Framed,
            )))));
        }
        let mut payload = vec![0u8; payload_len];
        match read_exact_classified(&mut self.reader, &mut payload) {
            Ok(FramePiece::Interrupted) => return self.end_of_acknowledged_prefix(),
            Err(error) => return Some(Err(error)),
            Ok(FramePiece::Filled) => {}
        }
        let mut head = [0u8; HEAD_BYTES];
        match read_exact_classified(&mut self.reader, &mut head) {
            Ok(FramePiece::Interrupted) => return self.end_of_acknowledged_prefix(),
            Err(error) => return Some(Err(error)),
            Ok(FramePiece::Filled) => {}
        }
        let event: EffectEvent = match from_bytes::<EffectEvent, WireError>(&payload) {
            Ok(event) => event,
            Err(_) => {
                return Some(Err(JournalError::Corrupt(Box::new(Corruption::new(
                    index,
                    CorruptionKind::Undecodable,
                )))));
            }
        };
        let head_digest = match chain(self.position, &event) {
            Ok(digest) => digest,
            Err(error) => return Some(Err(error)),
        };
        let recorded = JournalPosition {
            sequence: self.position.sequence().saturating_add(1),
            head: lgwks_std::hash::Digest::from_bytes(head),
        };
        let recomputed = JournalPosition {
            sequence: recorded.sequence(),
            head: head_digest,
        };
        if recorded != recomputed {
            return Some(Err(JournalError::Corrupt(Box::new(Corruption::new(
                index,
                CorruptionKind::Chain(ChainBreak::Disagreement {
                    at: recorded.sequence(),
                    recorded,
                    recomputed,
                }),
            )))));
        }
        self.position = recorded;
        self.yielded = self.yielded.saturating_add(1);
        self.offset = self
            .offset
            .saturating_add(super::frame::framed_len(payload_len));
        Some(Ok(event))
    }

    /// What a frame that ran past the end of the file means to this stream.
    ///
    /// The same answer `open` gives it, from the same resolver: bytes that are a
    /// prefix of one cut-short append end the stream, and a tail that holds an
    /// acknowledged frame under a lying length is the refusal `open` would have
    /// made. A stream that ended quietly there would hand a fold fewer events than
    /// were acknowledged, which is the loss the refusal exists to prevent.
    fn end_of_acknowledged_prefix(&mut self) -> Option<Result<EffectEvent, JournalError>> {
        match resolve_ambiguous_tail(
            self.reader.get_mut(),
            self.offset,
            self.position,
            self.yielded,
        ) {
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        }
    }
}

impl Iterator for Replay {
    type Item = Result<EffectEvent, JournalError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let item = self.read_one();
        // A refusal ends the stream as well: reading on from where it stopped would
        // yield frames that no longer chain from anything this stream returned.
        if item.as_ref().is_none_or(Result::is_err) {
            self.done = true;
        }
        item
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
            let refusal = Err(JournalError::CapacityExceeded {
                resource: JournalLimitKind::Bytes,
                limit: MAX_JOURNAL_BYTES,
                requested: file_len,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "open_impl: returning an error to the caller");
            return refusal;
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
            ScanStop::AmbiguousTail { offset } => {
                let index = u64::try_from(entries.len()).unwrap_or(u64::MAX);
                let offset = resolve_ambiguous_tail(
                    &mut file,
                    offset,
                    entries
                        .last()
                        .map_or_else(JournalPosition::genesis, JournalEntry::position),
                    index,
                )?;
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

        let storage = StorageOwner::spawn(file, (), stalled).map_err(JournalError::Storage)?;
        let view = FileView::read_only(&path)?;
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
            let refusal = Err(JournalError::CapacityExceeded {
                resource: JournalLimitKind::Events,
                limit,
                requested,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "bound_events: returning an error to the caller");
            return refusal;
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

    /// Stream the committed events from the file, frame by frame.
    ///
    /// This is the bounded replay path: unlike [`Self::events`], which borrows
    /// the history `open` already materialized, a `Replay` reads from its own
    /// descriptor and retains one event at a time, so a caller folding a large
    /// log pays for the largest frame rather than the whole history. What it
    /// yields is exactly the acknowledged prefix: a torn tail ends the stream
    /// and an impossible or lying frame refuses it, the same dispositions the
    /// open scan gives them.
    ///
    /// # Errors
    ///
    /// [`JournalError::Storage`] when the read-only descriptor cannot be opened.
    pub fn replay(&self) -> Result<Replay, JournalError> {
        Replay::open(&self.path)
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
            let refusal = Err(JournalError::Storage(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "a previous append failed while writing; the file may hold \
             unacknowledged bytes, reopen to replay",
            )));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "fence: returning an error to the caller");
            return refusal;
        }
        if self.view.len()? != self.disk_len {
            let refusal = Err(JournalError::Storage(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the journal file moved under this controller; reopen before appending",
            )));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "fence: returning an error to the caller");
            return refusal;
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
        // Archive, bound, chain and lay out — the shared step, which the run store
        // takes too. Only the event's own archiving and its position-chaining head
        // are this journal's, and both arrive as the closures they are.
        let (framed, head) = super::frame::frame_record(
            event,
            &from.head,
            MAX_FRAME_BYTES,
            |event| {
                event
                    .to_bytes()
                    .map(|bytes| bytes.as_ref().to_vec())
                    .map_err(JournalError::Encoding)
            },
            |_event, previous, archived| super::chain_over_bytes(previous, archived),
            |_len| {
                JournalError::Storage(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "the event exceeds this journal's frame bound",
                ))
            },
        )?;
        Ok((JournalPosition { sequence, head }, framed))
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
            let refusal = Err(JournalError::CapacityExceeded {
                resource: JournalLimitKind::Bytes,
                limit: MAX_JOURNAL_BYTES,
                requested,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "bound_bytes: returning an error to the caller");
            return refusal;
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
        self.write_and_sync_blocking(frames)
    }

    /// [`Self::write_and_sync`] for the door a task awaits rather than sits through.
    ///
    /// # Errors
    ///
    /// As [`Self::write_and_sync`].
    fn write_and_sync_async<'a>(
        &'a mut self,
        frames: Vec<u8>,
    ) -> crate::BoxFuture<'a, Result<(), JournalError>> {
        let expected = self.disk_len;
        Box::pin(async move {
            self.storage
                .submit_async(move |file, _state| commit(file, expected, &frames))
                .await
                .map_err(write_refusal)
        })
    }

    /// The synchronous door: perform the append on the owner thread and block for
    /// its answer.
    ///
    /// # Errors
    ///
    /// [`JournalError::OutcomeUnknown`] from the write or the sync, and
    /// [`JournalError::Storage`] when the owner refuses a request outright.
    fn write_and_sync_blocking(&self, frames: &[u8]) -> Result<(), JournalError> {
        let staged = frames.to_vec();
        let expected = self.disk_len;
        self.storage
            .submit(move |file, _state| commit(file, expected, &staged))
            .map_err(write_refusal)
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
        check_append_order(
            expected_tail,
            actual,
            event,
            self.ladder.get(&event.key()).copied(),
        )?;
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
    /// This is a fault-injection and liveness instrument, not a production
    /// door: while the gate is held closed — the state a journal opened with
    /// [`Self::open_with_stalled_storage`] starts in — **every append on this
    /// journal parks on the device by design**. Nothing is acknowledged until
    /// [`StorageGate::release`] runs, which is exactly what a stalled device
    /// does, and it is what lets a caller ask "what does this bot do while its
    /// disk has stopped answering" against a real process rather than a mock.
    ///
    /// Returned alongside the journal rather than only as
    /// [`Self::release_storage`] because the caller that most needs to un-stick
    /// the device is the one awaiting an append on it, and that caller holds
    /// the journal's borrow for the whole wait. It is the instrument a
    /// slow-store liveness test releases from an independent thread; an
    /// ordinary journal is opened with [`Self::open`] and is never gated.
    #[must_use]
    pub fn storage_gate(&self) -> StorageGate {
        self.storage.gate()
    }

    /// Let the outstanding flush proceed, and every flush after it.
    ///
    /// After this, the handle is an ordinary file journal: the stall is over
    /// and is not re-armed. A caller blocked inside
    /// [`Self::compare_and_append`] cannot reach this method — the borrow the
    /// append holds is the same one — and needs the gate that
    /// `open_with_stalled_storage` hands back instead.
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
                let refusal = Err(JournalError::OutOfOrder {
                    key: Box::new(key),
                    expected,
                    attempted,
                });
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "compare_and_append_all: returning an error to the caller");
                return refusal;
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

/// The ordered step that is the append: check the fence, write.
///
/// It runs on the storage owner's thread, which is what makes the length check and
/// the write it guards one step no other append can overtake. The `sync_all` is
/// *not* here: the frames are returned owed to the batch's one flush, so a journal
/// that appends alongside other durable writes syncs once for all of them and is
/// answered only after that flush has returned.
///
/// # Errors
///
/// Whatever the device reports. A length that is not the one the caller expected
/// means the file moved, and is refused before any byte is written.
fn commit(
    file: &mut File,
    expected_len: u64,
    frames: &[u8],
) -> std::io::Result<super::owner::Stage<(), ()>> {
    let on_disk = file.metadata()?.len();
    if on_disk != expected_len {
        let refusal = Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the journal file moved under this controller; reopen before appending",
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "commit: returning an error to the caller");
        return refusal;
    }
    file.write_all(frames)?;
    // A journal folds no state of its own, so the settle closure is empty: the
    // bytes and the acknowledgment are the whole of the step. The one thing that
    // does move is `self.disk_len`, and that is the caller's, mutated after its
    // answer lands rather than here — so two appends can never have folded a length
    // the file had not reached.
    Ok(super::owner::Stage::Unsynced {
        answer: (),
        bytes: frames.len(),
        settle: Box::new(|_: &mut ()| {}),
    })
}

/// Map one owner refusal onto this module's error vocabulary.
///
/// The distinction that matters is whether the bytes may be on the disk under no
/// acknowledgment: a failed write or an undelivered answer is
/// [`JournalError::OutcomeUnknown`], because only a reopen settles it, while a
/// poisoned handle or a queue that was merely full wrote nothing and is a plain
/// [`JournalError::Storage`] the caller may reason about and retry.
fn write_refusal(cause: super::owner::SubmitError) -> JournalError {
    if cause.is_outcome_unknown() {
        JournalError::OutcomeUnknown {
            cause: cause.into_io(),
        }
    } else {
        JournalError::Storage(cause.into_io())
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

    /// Reserve room for the whole three-rung handoff before any of it is
    /// written, so an attempt can never be left admitted and unprepared to
    /// settle. The ceiling is the same [`MAX_JOURNAL_EVENTS`] the appends
    /// enforce, checked here against the count this serialized path reads.
    fn reserve_handoff_capacity(&self, rungs: u64) -> Result<(), JournalError> {
        self.bound_events(rungs)
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
    /// The decision — the fence, the ladder and the bounds — is the
    /// synchronous form's; only the wait differs. The
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
            self.write_and_sync_async(frame.clone()).await?;
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
            let refusal = Err(JournalError::ReceiptUnavailable { required });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "confirm_outcome: returning an error to the caller");
            return refusal;
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
pub(super) mod tests {
    use super::*;
    use crate::effect::{
        ActionDigest, ActionId, AttemptId, EnvironmentEpoch, EnvironmentId, FlowRevision, RunId,
    };
    use crate::journal::frame::probe::{declared_at, frame_starts, with_prefix};
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
    /// The process id, the nanos and a counter, each covering what the others
    /// cannot. The counter is per process and the clock resolves to
    /// microseconds on macOS, so two test processes started together can derive
    /// the same name. A process id is unique among *live* processes, which are
    /// the ones that can collide; the nanos separate a pid the OS reuses later,
    /// and the counter separates calls within one process.
    static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A scratch path unique to one test run.
    ///
    /// Shared with `journal::owner`'s tests, so the journal's two test suites name
    /// their scratch files one way (INV-DEP-6).
    pub(in crate::journal) fn scratch(name: &str) -> PathBuf {
        let unique = SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or_default();
        let process = std::process::id();
        std::env::temp_dir().join(format!(
            "lgwks-journal-file-{name}-{process}-{nanos}-{unique}"
        ))
    }

    fn key() -> Result<crate::effect::EffectKey, Box<dyn std::error::Error>> {
        let run = RunId::from_hex(RUN)?;
        let action = ActionId::from_hex(ACTION)?;
        let attempt = AttemptId::from_decimal("1")?;
        let flow = FlowRevision::from_tagged("blake3_256", FLOW_HEX)?;
        let digest = ActionDigest::from_tagged("blake3_256", DIGEST_HEX)?;
        let environment = EnvironmentId::from_hex(ENV)?;
        let epoch = EnvironmentEpoch::from_decimal("1")?;
        Ok(crate::effect::EffectKey::new(
            run,
            action,
            attempt,
            flow,
            digest,
            environment,
            epoch,
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
        let run = RunId::from_hex(RUN)?;
        let action = ActionId::from_hex(ACTION)?;
        let attempt = AttemptId::from_decimal(&n.to_string())?;
        let flow = FlowRevision::from_tagged("blake3_256", FLOW_HEX)?;
        let digest = ActionDigest::from_tagged("blake3_256", DIGEST_HEX)?;
        let environment = EnvironmentId::from_hex(ENV)?;
        let epoch = EnvironmentEpoch::from_decimal("1")?;
        Ok(crate::effect::EffectKey::new(
            run,
            action,
            attempt,
            flow,
            digest,
            environment,
            epoch,
        ))
    }

    /// A second key, so a batch can fail on its own rung.
    fn key2() -> Result<crate::effect::EffectKey, Box<dyn std::error::Error>> {
        let run = RunId::from_hex(RUN)?;
        let action = ActionId::from_hex(ACTION)?;
        let attempt = AttemptId::from_decimal("7")?;
        let flow = FlowRevision::from_tagged("blake3_256", FLOW_HEX)?;
        let digest = ActionDigest::from_tagged("blake3_256", DIGEST_HEX)?;
        let environment = EnvironmentId::from_hex(ENV)?;
        let epoch = EnvironmentEpoch::from_decimal("1")?;
        Ok(crate::effect::EffectKey::new(
            run,
            action,
            attempt,
            flow,
            digest,
            environment,
            epoch,
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
    ///
    /// Through the shared grammar rather than by hand, because a hand-rolled frame
    /// in the test would be a second definition of the thing the tests are supposed
    /// to be checking — a fixture that drifted from the writer would prove nothing.
    fn frame_bytes(
        position: JournalPosition,
        event: &EffectEvent,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let payload = event.to_bytes()?;
        let head = chain(position, event)?;
        let length = u32::try_from(payload.len())?;
        Ok(crate::journal::frame::encode(length, &payload, &head))
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
                let refusal = Err(std::io::Error::other("injected storage fault"));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "read: returning an error to the caller");
                return refusal;
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
        let first_len = crate::journal::frame::framed_len(usize::try_from(first)?);
        let next = first_len;
        let mut second_prefix = [0u8; LENGTH_BYTES];
        for (slot, offset) in second_prefix.iter_mut().zip(0u64..) {
            let index = next.saturating_add(offset);
            *slot = *bytes
                .get(usize::try_from(index)?)
                .ok_or("fixture file is shorter than its own first frame")?;
        }
        let second = u32::from_be_bytes(second_prefix);
        let second_len = crate::journal::frame::framed_len(usize::try_from(second)?);
        Ok((
            bytes,
            usize::try_from(first_len.saturating_add(second_len))?,
        ))
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

    /// Open `path` and require a refusal as `Corrupt` at frame `at`, leaving the
    /// file byte-identical to `before`.
    fn require_refused_untouched(path: &Path, before: &[u8], at: u64, why: &str) -> TestResult {
        let outcome: TestResult = match FileJournal::open(path) {
            Err(JournalError::Corrupt(corruption)) => {
                assert_eq!(corruption.at(), at, "{why}: the lying frame is named");
                assert!(
                    matches!(corruption.kind(), CorruptionKind::Framed),
                    "{why}: the refusal names the framing, got {:?}",
                    corruption.kind()
                );
                Ok(())
            }
            Err(other) => Err(format!("{why}: expected a corruption refusal, got {other}").into()),
            Ok(opened) => Err(format!(
                "{why}: reopened as a journal of {} events, so an acknowledged frame was lost",
                opened.events().count()
            )
            .into()),
        };
        outcome?;
        assert_eq!(
            lgwks_std::hash::blake3(&std::fs::read(path)?),
            lgwks_std::hash::blake3(before),
            "{why}: refused bytes are never touched"
        );
        Ok(())
    }

    /// The acknowledged final frame whose length grows by `k` is the defect #262
    /// reported. `k` in `1..=32` leaves the payload whole and the head short, so the
    /// payload read succeeds and the head read is what ends early; `k` above `32`
    /// ends inside the payload. Both are one lie told two ways, and both refuse.
    #[test]
    fn a_lengthened_acknowledged_final_frame_is_refused_not_trimmed() -> TestResult {
        let path = scratch("lengthened-final");
        let _guard = TempGuard(path.clone());
        let (bytes, third) = three_frame_file(&path)?;
        let declared = declared_at(&bytes, third);
        for extra in 1u32..=1024 {
            let lied = with_prefix(&bytes, third, declared + extra);
            std::fs::write(&path, &lied)?;
            require_refused_untouched(&path, &lied, 2, &format!("final frame L+{extra}"))?;
        }
        Ok(())
    }

    /// The second shape from the same finding: an acknowledged frame with frames
    /// behind it whose length is inflated past the end of the file. The true frame
    /// is still the first thing behind the prefix, so it authenticates and the two
    /// frames after it are not read as one more candidate and dropped.
    #[test]
    fn an_inflated_non_final_length_is_refused_and_every_byte_survives() -> TestResult {
        let path = scratch("inflated-middle");
        let _guard = TempGuard(path.clone());
        let (bytes, _) = three_frame_file(&path)?;
        let middle = frame_starts(&bytes, 0)?[1];
        let remaining = u32::try_from(bytes.len() - middle)?;
        for declared in [
            remaining,
            remaining + 1,
            remaining + 31,
            remaining + 500,
            u32::try_from(MAX_FRAME_BYTES)?,
        ] {
            let lied = with_prefix(&bytes, middle, declared);
            std::fs::write(&path, &lied)?;
            require_refused_untouched(
                &path,
                &lied,
                1,
                &format!("middle frame declared {declared}"),
            )?;
        }
        Ok(())
    }

    /// The control: a genuinely cut append is still repaired. Every byte offset
    /// inside the final frame is a place a killed writer could have stopped, and each
    /// must reopen with the two acknowledged frames, report the repair, and leave the
    /// file at exactly the acknowledged prefix. This is what keeps the refusal above
    /// from being bought by refusing everything.
    #[test]
    fn an_append_cut_at_every_byte_of_the_final_frame_is_repaired() -> TestResult {
        let path = scratch("cut-every-byte");
        let _guard = TempGuard(path.clone());
        let (bytes, third) = three_frame_file(&path)?;
        for cut in third + 1..bytes.len() {
            std::fs::write(&path, &bytes[..cut])?;
            let reopened = FileJournal::open(&path).map_err(|error| {
                format!(
                    "a final frame cut at byte {cut} of {} was refused: {error}",
                    bytes.len()
                )
            })?;
            assert!(
                reopened.torn_tail_repaired(),
                "cut at {cut} was not reported as repaired"
            );
            assert_eq!(
                reopened.committed()?.len(),
                2,
                "cut at {cut} lost an acknowledged frame"
            );
            drop(reopened);
            assert_eq!(
                std::fs::metadata(&path)?.len(),
                u64::try_from(third)?,
                "cut at {cut} was not trimmed to the acknowledged prefix"
            );
        }
        Ok(())
    }

    /// A middle frame whose length is inflated *and* whose own payload is damaged
    /// authenticates as nothing itself, but the acknowledged frame behind it still
    /// does, so the open refuses rather than trimming both.
    #[test]
    fn a_damaged_cut_frame_with_an_acknowledged_frame_behind_it_is_refused() -> TestResult {
        let path = scratch("damaged-middle");
        let _guard = TempGuard(path.clone());
        let (bytes, _) = three_frame_file(&path)?;
        let starts = frame_starts(&bytes, 0)?;
        let middle = starts[1];
        let remaining = u32::try_from(bytes.len() - middle)?;
        let mut lied = with_prefix(&bytes, middle, remaining + 7);
        lied[middle + LENGTH_BYTES + 3] ^= 0x55;
        std::fs::write(&path, &lied)?;
        require_refused_untouched(&path, &lied, 1, "damaged middle frame")
    }

    /// The stated limit, pinned so the claim is no larger than the test: a final
    /// frame whose length was changed and whose head was also damaged authenticates as
    /// nothing and has nothing behind it, so it is indistinguishable from an append
    /// cut inside its head, and it is trimmed.
    #[test]
    fn a_final_frame_with_a_lying_length_and_a_damaged_head_is_the_stated_limit() -> TestResult {
        let path = scratch("two-faults");
        let _guard = TempGuard(path.clone());
        let (bytes, third) = three_frame_file(&path)?;
        let declared = declared_at(&bytes, third);
        let mut lied = with_prefix(&bytes, third, declared + 5);
        let last = lied.len() - 1;
        lied[last] ^= 0xff;
        std::fs::write(&path, &lied)?;
        let reopened = FileJournal::open(&path)?;
        assert!(reopened.torn_tail_repaired());
        assert_eq!(reopened.committed()?.len(), 2);
        Ok(())
    }

    /// The streaming replay answers a lying length as `open` does. It reads from
    /// its own descriptor, so a file changed after `open` is the case it can meet.
    #[test]
    fn a_streaming_replay_refuses_a_lengthened_final_frame_and_then_ends() -> TestResult {
        let path = scratch("replay-lengthened");
        let _guard = TempGuard(path.clone());
        let (bytes, third) = three_frame_file(&path)?;
        let journal = FileJournal::open(&path)?;
        let declared = declared_at(&bytes, third);
        for extra in [1u32, 17, 32, 33, 400] {
            std::fs::write(&path, with_prefix(&bytes, third, declared + extra))?;
            let mut replay = journal.replay()?;
            assert!(
                matches!(replay.next(), Some(Ok(_))),
                "L+{extra}: first event"
            );
            assert!(
                matches!(replay.next(), Some(Ok(_))),
                "L+{extra}: second event"
            );
            assert!(
                matches!(replay.next(), Some(Err(JournalError::Corrupt(_)))),
                "L+{extra}: the lengthened frame must be a refusal, not the end of the stream"
            );
            assert!(
                replay.next().is_none(),
                "L+{extra}: a refusal ends the stream"
            );
        }
        std::fs::write(&path, &bytes[..third + 10])?;
        let events = journal.replay()?.collect::<Result<Vec<_>, _>>()?;
        assert_eq!(
            events.len(),
            2,
            "a cut append still ends the stream quietly"
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
