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
//! `tests/it/journal_writer_fence.rs`, which re-executes this test binary as a
//! second process rather than simulating one.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use super::continuation::{
    Continuation, ContinuationPolicy, ContinuationWatermark, SealPause, SettledAttempt,
    UnresolvedAttempt, successor_path,
};
use super::frame::{HEAD_BYTES, LENGTH_BYTES, Piece, Prefix, SaturatingFrom, read_exact_or_eof};
use super::owner::{StorageGate, StorageOwner};
use super::{
    AttemptStatus, ChainBreak, DurabilityPromise, DurableAck, EffectEvent, EffectEvidence,
    EffectJournal, EventKind, JournalEntry, JournalError, JournalLimitKind, JournalPosition,
    MAX_JOURNAL_BYTES, MAX_JOURNAL_EVENTS, Recovered, check_append_order, next_allowed_of,
    recover_continued,
};
use lgwks_std::wire::{WireError, from_bytes};

/// The largest frame this journal will read or write.
///
/// Events are a key, a verdict and a digest; a real frame is a few hundred
/// bytes. A length field beyond this bound does not name a frame this
/// journal writes, so a complete prefix carrying one is refused as rot; a
/// partial prefix is a torn tail.
///
/// A sealed checkpoint is framed under this same bound, which is why the carried
/// state is charged against its own declared counts first: a carry this grammar
/// cannot frame is refused before a byte moves, not discovered at the writer.
pub(super) const MAX_FRAME_BYTES: usize = 64 * 1024;

/// The most successor files one `open_active` walk will follow.
///
/// A declared bound rather than a loop until a name repeats, because the chain is
/// unbounded in principle — a run that continues for a year has a year of files —
/// and a walk with no bound is a walk that can park on a directory tree somebody
/// else is generating. A generation beyond this is reported, not guessed past.
pub const MAX_GENERATION_WALK: u64 = 4096;

/// One whole piece of a frame's bytes: the payload or the head.
///
/// The crate-wide grammar names the pieces it reads; this is the same shape under
/// the name this module's error vocabulary uses, so `Interrupted` here and
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
    /// The file holds a second sealed checkpoint.
    ///
    /// One journal continues once. A second seal frame in one file is bytes no
    /// writer of this journal produces, and it is refused rather than read as a
    /// later continuation: the successor is a distinct file, so a seal that
    /// appears here twice means the file's own chain has been given two
    /// incompatible answers about where its authoritative journal is.
    Sealed,
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
            Self::Sealed => "the file holds a second sealed checkpoint",
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

/// A whole frame read from a journal file, before its payload is decoded.
///
/// The payload is handed back rather than decoded here because this file holds
/// **two** record types — an effect event and a sealed checkpoint — and the
/// question "which is this?" is answered by the payload's magic prefix, which the
/// caller sees. A reader that decoded here would have to guess and fall back,
/// and a guess that falls back is how a sealed checkpoint gets read as an event.
struct Frame {
    /// The archived record, exactly as the frame stored it.
    payload: Vec<u8>,
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
) -> Result<Result<Frame, Halt>, JournalError> {
    let mut prefix = [0u8; LENGTH_BYTES];
    match super::frame::read_prefix(reader, &mut prefix).map_err(JournalError::Storage)? {
        Prefix::Eof => return Ok(Err(Halt::Complete)),
        Prefix::Torn => return Ok(Err(Halt::Torn)),
        Prefix::Full => {
            let requested = u64::saturating_from(held).saturating_add(1);
            if requested > u64::saturating_from(max_events) {
                let refusal = Err(JournalError::CapacityExceeded {
                    resource: JournalLimitKind::Events,
                    limit: u64::saturating_from(max_events),
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
    Ok(Ok(Frame {
        payload,
        head,
        payload_len,
    }))
}

/// Read frames from `reader`, stopping at the first torn frame.
///
/// Returns the decoded entries, the sealed checkpoint this file opened from (only
/// a successor has one), or the corruption that refuses the file. The scan is
/// streaming and every iteration consumes at least one byte, so the loop is
/// bounded by the file's own length.
///
/// The first frame is where a continuation lives, and it is read by the same loop
/// rather than by a second pass: its payload carries the magic prefix, and the
/// chain it continues from is the `predecessor` position inside it. Verifying that
/// frame against the position it *names* rather than against the genesis is what
/// lets a successor's own chain start at its predecessor's tail and still be a
/// chain — the same thing an etcd snapshot does when its index names the entry it
/// was taken at.
fn scan(reader: &mut impl Read, max_events: usize) -> Result<Scanned, JournalError> {
    let mut entries = Vec::new();
    let mut carried: Option<Continuation> = None;
    let mut seal: Option<Continuation> = None;
    let mut position = JournalPosition::genesis();
    let mut chain_from = JournalPosition::genesis();
    let mut offset = 0u64;
    let mut index = 0u64;
    loop {
        let Frame {
            payload,
            head,
            payload_len,
        } = match next_frame(reader, entries.len(), max_events, index)? {
            Ok(frame) => frame,
            Err(halt) => {
                return Ok(Scanned {
                    entries,
                    carried,
                    seal,
                    chain_from,
                    tail: position,
                    stop: halt.at(offset),
                });
            }
        };

        if let Some(decoded) = Continuation::from_payload(&payload) {
            // A checkpoint is legal in exactly two places: as this file's first
            // frame, which is the predecessor's seal this file continues from, and
            // as this file's last, which is this file sealing itself. Two in one
            // file are bytes no writer of this journal produces; one in the middle
            // is caught by the event-after-seal arm below, because a frame that
            // follows a seal cannot be part of the chain that seal closed.
            if seal.is_some() {
                let refusal = Err(JournalError::Corrupt(Box::new(Corruption::new(
                    index,
                    CorruptionKind::Sealed,
                ))));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "scan: the file holds a second sealed checkpoint");
                return refusal;
            }
            let checkpoint = match decoded {
                Ok(checkpoint) => checkpoint,
                Err(cause) => {
                    lgwks_std::trace::debug!(index, "scan: the sealed checkpoint did not decode");
                    return Err(cause);
                }
            };
            position = verify_frame(&payload, &head, checkpoint.predecessor(), index)?;
            // Only the carried seal moves where this file's events chain from. This
            // file's *own* closing seal follows its events, so taking its position
            // would start the chain after the last event it is meant to verify.
            if index == 0 {
                carried = Some(checkpoint);
                chain_from = position;
            } else {
                seal = Some(checkpoint);
            }
        } else {
            if seal.is_some() {
                let refusal = Err(JournalError::Corrupt(Box::new(Corruption::new(
                    index,
                    CorruptionKind::Sealed,
                ))));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "scan: an event follows this journal's own seal");
                return refusal;
            }
            let event: EffectEvent = match from_bytes::<EffectEvent, WireError>(&payload) {
                Ok(event) => event,
                Err(error) => {
                    lgwks_std::trace::debug!(?error, index, "scan: the payload did not decode");
                    let refusal = Err(JournalError::Corrupt(Box::new(Corruption::new(
                        index,
                        CorruptionKind::Undecodable,
                    ))));
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "scan: returning an error to the caller");
                    return refusal;
                }
            };
            position = verify_frame(&payload, &head, position, index)?;
            entries.push(JournalEntry::new(position, event));
        }
        offset = offset.saturating_add(super::frame::framed_len(payload_len));
        index = index.saturating_add(1);
    }
}

/// The position one whole frame records, or the refusal that its stored head is
/// not.
///
/// The chain function is the shared one every frame of this chain uses, whether
/// its payload is an event or a sealed checkpoint: a frame that cannot
/// re-derive its own head was not framed by this journal's writer, and the bytes
/// behind it may have been acknowledged.
fn verify_frame(
    payload: &[u8],
    head: &[u8; HEAD_BYTES],
    position: JournalPosition,
    index: u64,
) -> Result<JournalPosition, JournalError> {
    let recomputed = JournalPosition {
        sequence: position.sequence().saturating_add(1),
        head: super::chain_over_bytes(&position.head(), payload),
    };
    let recorded = JournalPosition {
        sequence: recomputed.sequence(),
        head: lgwks_std::hash::Digest::from_bytes(*head),
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
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "verify_frame: returning an error to the caller");
        return refusal;
    }
    Ok(recorded)
}

/// What a scan found: the events, the two checkpoints a file can hold, and where
/// the scan stopped.
struct Scanned {
    /// Every effect event committed before the stop, in commit order.
    entries: Vec<JournalEntry>,
    /// The predecessor's seal, when this file is a successor.
    carried: Option<Continuation>,
    /// This file's own seal, when it has sealed itself.
    seal: Option<Continuation>,
    /// The position the committed events' chain starts from.
    ///
    /// The genesis for a base journal and the seal frame's own position for a
    /// successor, because the seal frame is not an event and so is not in
    /// `committed_entries`: folding a successor's events from the genesis, or from
    /// the predecessor's tail, disagrees at its very first entry.
    chain_from: JournalPosition,
    /// The last position the scan verified, which is the journal's tail.
    ///
    /// Read from the loop rather than recomputed from the entries, because a
    /// journal that has sealed itself has a tail one past its last event and a
    /// seal frame is not an event.
    tail: JournalPosition,
    /// Where the scan stopped.
    stop: ScanStop,
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
    storage: StorageOwner<(), SealOutcome>,
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
    /// The last committed position, including this file's own seal when it has one.
    position: JournalPosition,
    /// Whether open repaired a torn tail to get here.
    torn_tail_repaired: bool,
    /// Whether this handle continues at the watermark.
    ///
    /// Declared by the constructor rather than inferred, because "this journal
    /// will not continue" is a policy and a policy nobody chose is not one: the
    /// default constructor keeps every existing behaviour, and a host running
    /// unattended asks for the lifecycle explicitly.
    continuing: bool,
    /// Where this handle continues.
    continuation: ContinuationPolicy,
    /// The predecessor's seal, when this file is a successor.
    ///
    /// The carried state a recovery fold starts from, and the reason a successor's
    /// [`FileJournal::recover`] answers exactly what its predecessor's did. Kept
    /// whole rather than folded into the indexes beside it, because a fold is a
    /// report and the record behind it is the evidence.
    carried: Option<Continuation>,
    /// This file's own seal, when it has sealed itself.
    seal: Option<Continuation>,
    /// The position the committed events' chain starts from.
    ///
    /// The genesis for a base journal and the seal frame's own position for a
    /// successor, because the seal frame is not an event and so is not in
    /// `committed_entries`: folding a successor's events from the genesis, or from
    /// the predecessor's tail, disagrees at its very first entry.
    chain_from: JournalPosition,
    /// The predecessor's chain head, which is the position this file's first frame
    /// chains from. Carried beside the checkpoint because the seal frame is not
    /// an event and so is not in [`Self::committed`].
    base: JournalPosition,
    /// One folded record per action the sealed history walked, the answer to "was
    /// this attempt already walked?" for an attempt the checkpoint does not carry.
    folded: Vec<SettledAttempt>,
    /// Where this handle's own seal sent the authoritative journal.
    ///
    /// `Some` means this handle sealed the file and must not append again. The
    /// predecessor is read-only from here, and the refusal is the seal rather
    /// than the device's poison: the bytes are exactly what was asked for.
    sealed_by: Option<PathBuf>,
    /// The boundary an armed continuation stops at, if one is armed.
    pause: Option<SealPause>,
}

impl core::fmt::Debug for FileJournal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("FileJournal")
            .field("path", &self.path)
            .field("committed_events", &self.committed.len())
            .field("disk_len", &self.disk_len)
            .field("torn_tail_repaired", &self.torn_tail_repaired)
            .field("continuing", &self.continuing)
            .field("generation", &self.generation())
            .field("sealed_by", &self.sealed_by)
            .finish()
    }
}

/// What one ordered step on the journal's storage owner produced.
///
/// The owner's answer type rather than `()` because a seal has to say more than an
/// append does, and a type that could only say `()` would make the seal report
/// through a side channel the owner does not have. It is the same thread and the
/// same ordered step either way (INV-BOT-50), so this is a richer answer and not a
/// second mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SealOutcome {
    /// An ordinary append moved its bytes and owes the batch's one flush.
    Appended,
    /// A seal wrote the predecessor's frame and the successor's first frame.
    Sealed {
        /// How many bytes the predecessor's seal frame added.
        bytes: usize,
    },
    /// The armed fault injector stopped the seal at this boundary.
    Paused(SealPause),
}

/// Which constructor a handle came in through.
///
/// Three shapes rather than three booleans, because the two properties are not
/// independent: a fault injector and a continuing journal are both things a
/// caller *declares*, and a struct of two flags would let a caller open a stalled
/// journal that also claims to continue without either being what it meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenKind {
    /// An ordinary journal, opened with [`FileJournal::open`].
    Plain,
    /// An ordinary journal whose device answers only when released.
    Stalled,
    /// A journal that continues at the policy's trigger points.
    Continuing(ContinuationPolicy),
}

impl OpenKind {
    /// Whether the device holds its flush until released.
    const fn stalled(self) -> bool {
        matches!(self, Self::Stalled)
    }

    /// Whether this handle continues at the watermark.
    const fn continues(self) -> bool {
        matches!(self, Self::Continuing(_))
    }

    /// The trigger points a continuing handle was opened with.
    const fn policy(self) -> ContinuationPolicy {
        match self {
            Self::Continuing(policy) => policy,
            Self::Plain | Self::Stalled => ContinuationPolicy::declared(),
        }
    }
}

/// Record one carried key's rung in an append fence's ladder index.
///
/// The carried rung and the rung an event recorded are the same fact about the
/// same key, so they land in the same index: a successor's ladder check is
/// literally the predecessor's, with the checkpoint as where the index was
/// seeded.
fn ladder_hold(
    ladder: &mut HashMap<crate::effect::EffectKey, EventKind>,
    key: crate::effect::EffectKey,
    rung: EventKind,
) {
    ladder.insert(key, rung);
}

/// Whether `successor` is a complete, authenticated successor of `seal`'s
/// predecessor.
///
/// The one fact that decides who is authoritative, asked of the bytes rather than
/// of anything a running process remembers: a successor exists, its first frame
/// is whole, and that frame chains from exactly the predecessor's tail the seal
/// names. Anything short of all three — absent, torn, or naming some other
/// position — is a continuation that never committed, and the predecessor stays
/// live.
///
/// Read through its own descriptor with the shared grammar's classification, so a
/// torn first frame reads as *not complete* rather than as a decode failure: this
/// question is "did the continuation commit?", and a half-written file is the
/// answer "no", not an error.
fn successor_is_complete(successor: &Path, seal: &Continuation) -> bool {
    let Ok(mut file) = OpenOptions::new().read(true).open(successor) else {
        return false;
    };
    let mut prefix = [0u8; LENGTH_BYTES];
    let Ok(Prefix::Full) = super::frame::read_prefix(&mut file, &mut prefix) else {
        return false;
    };
    let declared = super::frame::declared_length(&prefix);
    if !super::frame::is_possible_length(declared, MAX_FRAME_BYTES) {
        return false;
    }
    let mut payload = vec![0u8; declared];
    let mut head = [0u8; HEAD_BYTES];
    if super::frame::read_piece(&mut file, &mut payload).is_err()
        || super::frame::read_piece(&mut file, &mut head).is_err()
    {
        return false;
    }
    match Continuation::from_payload(&payload) {
        Some(Ok(carried)) => {
            carried.predecessor() == seal.predecessor()
                && super::chain_over_bytes(&seal.predecessor().head(), &payload)
                    == lgwks_std::hash::Digest::from_bytes(head)
        }
        Some(Err(_)) | None => false,
    }
}

/// Make the successor's name itself durable, where the platform has such a call.
///
/// A directory entry that has not reached the disk is a successor that a power
/// loss would not find, and the whole authority rule asks whether that file is
/// there. `#[cfg(unix)]` rather than best-effort: on a platform with no directory
/// sync the call cannot be made at all, and a swallowed error would be a bound
/// nothing observes. What the journal promises is process-crash survival, which
/// does not depend on this call — the name is in the running system's page cache
/// across any process death — so its absence costs power-loss durability the
/// journal never claimed.
#[cfg(unix)]
fn sync_directory(path: &Path, created_here: bool) -> std::io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    File::open(parent)?.sync_all()?;
    // Only a directory this seal *created* still has an unflushed name of its own.
    // Every later generation writes into a directory that already exists, and a
    // directory fsync is the most expensive call this seal makes, so paying for the
    // grandparent on every continuation would cost a long run far more than the
    // durability it buys.
    if created_here && let Some(grandparent) = parent.parent() {
        File::open(grandparent)?.sync_all()?;
    }
    Ok(())
}

/// [`sync_directory`] where the platform has no directory sync to make.
#[cfg(not(unix))]
fn sync_directory(_path: &Path, _created_here: bool) -> std::io::Result<()> {
    Ok(())
}

/// Write the seal frame into the successor, creating it or completing it.
///
/// The two cases are one operation because one is the other's recovery: a
/// successor that exists here is a continuation that did not commit — this
/// journal's authority rule already decided that, or the open would have refused
/// — so its bytes are an interrupted write and taking them back is the same repair
/// the open performs on a torn tail. Its frame is rewritten byte for byte, so the
/// result is the same file either way.
fn write_successor(successor: &Path, frame: &[u8]) -> std::io::Result<bool> {
    // The generation directory is a name this seal creates, so it is made here
    // rather than by an operator: a host that asked for a lifecycle asked for the
    // directories that lifecycle needs. Its own durability is `sync_parent`'s.
    //
    // Whether this call created it is read *before* creating it and reported,
    // because only a name that has just been created still needs its own parent
    // entry flushed, and a directory fsync is the most expensive call this seal
    // makes. Asked after the create, the answer is always "it exists", and a new
    // generation directory's entry would never reach the disk.
    let created = successor.parent().is_some_and(|parent| !parent.exists());
    if let Some(parent) = successor.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let written = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(successor)
    {
        Ok(mut file) => file.write_all(frame),
        Err(ref error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let file = OpenOptions::new().read(true).write(true).open(successor)?;
            file.set_len(0)?;
            let mut handle = file;
            handle.write_all(frame)
        }
        Err(error) => {
            let refusal = Err(error);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "write_successor: the successor could not be created");
            return refusal;
        }
    };
    written.map(|()| created)
}

/// The seal, as the one ordered step the journal's storage owner runs.
///
/// Every byte this journal ever writes goes through this owner thread
/// (INV-BOT-50), and a continuation is not an exception to that: it is the case
/// where the fence, the write and the flush guard **two** files, so running it
/// anywhere else would make the length check and the write it guards overtakable
/// by the very appends they exist to order against.
///
/// The four pauses sit between the four durable steps, in the order the steps
/// happen, so a kill at any of them leaves a different file to reopen and a
/// harness can sweep the whole set from [`SealPause::all`].
fn seal_on_owner(
    file: &mut File,
    expected_len: u64,
    frame: &[u8],
    successor: &Path,
    pause: Option<SealPause>,
) -> std::io::Result<super::owner::Stage<SealOutcome, ()>> {
    fence_and_write(file, expected_len, frame)?;
    let Some(paused) = paused_at(pause, SealPause::AfterSealWrite) else {
        let created_directory = write_successor(successor, frame)?;
        let Some(paused) = paused_at(pause, SealPause::AfterSuccessorWrite) else {
            OpenOptions::new().read(true).open(successor)?.sync_all()?;
            let Some(paused) = paused_at(pause, SealPause::AfterSuccessorSync) else {
                sync_directory(successor, created_directory)?;
                let Some(paused) = paused_at(pause, SealPause::AfterDirectorySync) else {
                    return Ok(super::owner::Stage::Unsynced {
                        answer: SealOutcome::Sealed { bytes: frame.len() },
                        bytes: frame.len(),
                        settle: Box::new(|_: &mut ()| {}),
                    });
                };
                return Ok(super::owner::Stage::Settled(Ok(paused)));
            };
            return Ok(super::owner::Stage::Settled(Ok(paused)));
        };
        return Ok(super::owner::Stage::Settled(Ok(paused)));
    };
    Ok(super::owner::Stage::Settled(Ok(paused)))
}

/// The pause answer for `boundary`, when this seal is armed to stop there.
fn paused_at(pause: Option<SealPause>, boundary: SealPause) -> Option<SealOutcome> {
    match pause {
        Some(armed) if armed == boundary => Some(SealOutcome::Paused(boundary)),
        _ => None,
    }
}

/// A read-only window onto the journal's file, for the handle's own fence.///
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
///
/// A sealed checkpoint gets the open scan's dispositions too, because it is not an
/// event. A successor's first frame is the seal its predecessor wrote: it is
/// verified against the predecessor's tail it names and moves where this file's
/// events chain from, and it is not yielded — the carried state is
/// [`FileJournal::checkpoint`], and [`FileJournal::recover`] is the fold that
/// includes it. This file's own closing seal ends the stream, and a frame after it
/// is refused. So the stream is exactly [`FileJournal::events`] on every file of a
/// chain, which is what lets a successor be streamed at all.
pub struct Replay {
    /// The streaming frame reader, on its own descriptor.
    reader: BufReader<File>,
    /// The position the next event must chain from.
    position: JournalPosition,
    /// How many events the stream has already yielded, for the event ceiling.
    yielded: u64,
    /// How many frames the stream has consumed, which is where a refusal is
    /// located: the open scan counts frames, and a successor's first frame is
    /// a checkpoint rather than an event.
    frames: u64,
    /// Whether the stream has passed this file's own closing seal, after which
    /// no frame is legal.
    sealed: bool,
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
            frames: 0,
            sealed: false,
            offset: 0,
            done: false,
        })
    }

    /// Read exactly one more committed event, or end the stream.
    ///
    /// `None` means the acknowledged prefix is exhausted at a clean end, a torn
    /// tail or this file's own seal, which is never an error: the tail was never
    /// acknowledged and the seal is not an event, so the stream of committed
    /// events is complete without either. Bounded: every turn consumes a frame,
    /// and at most one checkpoint frame is consumed without yielding, because a
    /// second one is refused.
    fn read_one(&mut self) -> Option<Result<EffectEvent, JournalError>> {
        loop {
            match self.read_frame()? {
                Ok(Some(event)) => return Some(Ok(event)),
                Ok(None) => {}
                Err(error) => return Some(Err(error)),
            }
        }
    }

    /// Consume one whole frame: `Some(Ok(Some(event)))` for an event,
    /// `Some(Ok(None))` for a checkpoint frame, `None` at the end.
    fn read_frame(&mut self) -> Option<Result<Option<EffectEvent>, JournalError>> {
        let frame = self.frames;
        let mut prefix = [0u8; LENGTH_BYTES];
        match read_exact_or_eof(&mut self.reader, &mut prefix) {
            Err(error) => return Some(Err(error)),
            Ok(None) => return None,
            Ok(Some(read_len)) if read_len < LENGTH_BYTES => return None,
            Ok(Some(_)) => {}
        }
        let limit = u64::saturating_from(MAX_JOURNAL_EVENTS);
        if self.yielded >= limit {
            let refusal = Err(JournalError::CapacityExceeded {
                resource: JournalLimitKind::Events,
                limit,
                requested: self.yielded.saturating_add(1),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "Replay::read_frame: the stream reached the event ceiling");
            return Some(refusal);
        }
        let payload_len = usize::saturating_from(u32::from_be_bytes(prefix));
        if payload_len == 0 || payload_len > MAX_FRAME_BYTES {
            let refusal = Err(JournalError::Corrupt(Box::new(Corruption::new(
                frame,
                CorruptionKind::Framed,
            ))));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "Replay::read_frame: a length no writer produces");
            return Some(refusal);
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
        let admitted = self.admit(&payload, &head, frame);
        self.frames = self.frames.saturating_add(1);
        self.offset = self
            .offset
            .saturating_add(super::frame::framed_len(payload_len));
        Some(admitted)
    }

    /// Decide one whole frame the way the open scan decides it.
    fn admit(
        &mut self,
        payload: &[u8],
        head: &[u8; HEAD_BYTES],
        frame: u64,
    ) -> Result<Option<EffectEvent>, JournalError> {
        if self.sealed {
            let refusal = Err(JournalError::Corrupt(Box::new(Corruption::new(
                frame,
                CorruptionKind::Sealed,
            ))));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "Replay::admit: a frame follows this journal's own seal");
            return refusal;
        }
        if let Some(decoded) = Continuation::from_payload(payload) {
            let checkpoint = decoded?;
            let sealed_at = verify_frame(payload, head, checkpoint.predecessor(), frame)?;
            // Either seal is the last whole frame, which is what a cut after it
            // resolves against. The carried one is where this file's events chain
            // from; this file's own one closes it, and nothing may follow.
            self.position = sealed_at;
            self.sealed = frame != 0;
            return Ok(None);
        }
        let Ok(event) = from_bytes::<EffectEvent, WireError>(payload) else {
            let refusal = Err(JournalError::Corrupt(Box::new(Corruption::new(
                frame,
                CorruptionKind::Undecodable,
            ))));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "Replay::admit: the payload did not decode");
            return refusal;
        };
        self.position = verify_frame(payload, head, self.position, frame)?;
        self.yielded = self.yielded.saturating_add(1);
        Ok(Some(event))
    }

    /// What a frame that ran past the end of the file means to this stream.
    ///
    /// The same answer `open` gives it, from the same resolver: bytes that are a
    /// prefix of one cut-short append end the stream, and a tail that holds an
    /// acknowledged frame under a lying length is the refusal `open` would have
    /// made. A stream that ended quietly there would hand a fold fewer events than
    /// were acknowledged, which is the loss the refusal exists to prevent.
    fn end_of_acknowledged_prefix(&mut self) -> Option<Result<Option<EffectEvent>, JournalError>> {
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
    /// This handle does **not** continue past its ceilings; use
    /// [`FileJournal::open_continuing`] for a run that is meant to be
    /// unattended. That is a policy rather than a default, so every existing
    /// behaviour of this constructor is unchanged.
    ///
    /// # Errors
    ///
    /// [`JournalError::Locked`] when another writer holds the file;
    /// [`JournalError::Storage`] when the file cannot be opened, read or
    /// repaired; [`JournalError::Corrupt`] when committed bytes are refused;
    /// [`JournalError::Superseded`] when this file has been continued and its
    /// successor is the authoritative journal; and
    /// [`JournalError::CapacityExceeded`] when the complete history exceeds
    /// [`MAX_JOURNAL_BYTES`] or [`MAX_JOURNAL_EVENTS`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        Self::open_impl(path.as_ref(), OpenKind::Plain)
    }

    /// Open a journal that continues at the declared watermark.
    ///
    /// The lifecycle an unattended run needs, and the same journal in every other
    /// respect: the same frame grammar, the same chain, the same storage-owner
    /// thread, the same refusals. What it adds is that
    /// [`EffectJournal::continue_as_new`] seals this journal at
    /// [`CONTINUATION_WATERMARK_NUMERATOR`](super::continuation::CONTINUATION_WATERMARK_NUMERATOR)
    /// of either ceiling and hands back the
    /// successor, so a bot that runs for weeks keeps acting instead of stopping
    /// safely at a ceiling.
    ///
    /// The watermark leaves the remaining fifth of each ceiling as settlement
    /// headroom, so an attempt already handed to the outside world can always
    /// record its outcome — which is #143's "near-full settlement-capacity
    /// reservation", the half that had no API behind it.
    ///
    /// A journal whose sealed successor is complete refuses itself with
    /// [`JournalError::Superseded`] rather than reporting an empty run, and
    /// [`FileJournal::open_active`] follows the chain for a caller that only has
    /// the original path.
    ///
    /// # Errors
    ///
    /// Whatever [`FileJournal::open`] reports.
    pub fn open_continuing(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        Self::open_impl(
            path.as_ref(),
            OpenKind::Continuing(ContinuationPolicy::declared()),
        )
    }

    /// Open a journal that continues at `policy`'s two trigger points.
    ///
    /// The same lifecycle as [`FileJournal::open_continuing`] with the trigger
    /// stated rather than derived, for a host whose retention budget is not this
    /// crate's default and for a simulation that has to reach a hundred
    /// continuations inside a test's wall clock. A policy at or past a ceiling is
    /// refused by [`ContinuationPolicy::new`] rather than clamped, so this
    /// constructor cannot open a journal whose trigger is unreachable.
    ///
    /// # Errors
    ///
    /// Whatever [`FileJournal::open`] reports.
    pub fn open_continuing_with(
        path: impl AsRef<Path>,
        policy: ContinuationPolicy,
    ) -> Result<Self, JournalError> {
        Self::open_impl(path.as_ref(), OpenKind::Continuing(policy))
    }

    /// Open the journal at `path`, following its continuations to the live one.
    ///
    /// What a restarting controller wants: it holds the path its host recorded, and
    /// the file at that path may be a hundred sealed predecessors old. The walk
    /// follows one [`JournalError::Superseded`] at a time and stops at the first
    /// journal that is not sealed, and it is bounded by
    /// `MAX_GENERATION_WALK` rather than by a loop that could follow a
    /// directory tree somebody else is generating.
    ///
    /// # Errors
    ///
    /// Whatever the last journal reports, and [`JournalError::Superseded`] naming
    /// the next successor when the walk ran out of generations.
    pub fn open_active(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        let mut here = path.as_ref().to_path_buf();
        for _ in 0..MAX_GENERATION_WALK {
            match Self::open_impl(&here, OpenKind::Continuing(ContinuationPolicy::declared())) {
                Ok(journal) => return Ok(journal),
                Err(JournalError::Superseded { path: next }) => here = PathBuf::from(next),
                Err(other) => {
                    let refusal = Err(other);
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "FileJournal::open_active: a generation on the walk refused to open");
                    return refusal;
                }
            }
        }
        let next = successor_path(&here).display().to_string();
        let refusal = Err(JournalError::Superseded { path: next });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "open_active: the generation walk ran out before reaching a live journal");
        refusal
    }

    /// Arm the next continuation to stop at `boundary`.
    ///
    /// A fault injector, and the reason it is public rather than hidden behind
    /// `cfg(test)` is the one [`FileJournal::open_with_stalled_storage`] gives: a
    /// continuation has four places where bytes become durable in order, and "which
    /// journal is authoritative after the disk dies at the third one" is a question
    /// about a real process. A continuation armed to stop refuses with
    /// [`JournalError::ContinuationPaused`] naming the boundary it reached,
    /// *before* the next byte moves, so a harness that kills the process on that
    /// refusal kills it exactly at the boundary.
    ///
    /// [`SealPause::all`] sweeps the whole set, so a boundary added later is swept
    /// the day it exists rather than the day somebody remembers.
    pub fn arm_continuation_pause(&mut self, boundary: SealPause) {
        self.pause = Some(boundary);
    }

    /// Which generation this journal is. One for a journal that never continued.
    ///
    /// Counted from this file's own name rather than read out of its checkpoint,
    /// so it answers the same for a predecessor that carries no checkpoint at all
    /// — and so two places never hold a generation that can disagree.
    #[must_use]
    pub fn generation(&self) -> u64 {
        super::continuation::generation_of(&self.path)
    }

    /// The predecessor's sealed checkpoint this journal's chain starts from, when
    /// this journal is a successor.
    #[must_use]
    pub fn checkpoint(&self) -> Option<&Continuation> {
        self.carried.as_ref()
    }

    /// This journal's own seal, when it has sealed itself.
    #[must_use]
    pub fn own_seal(&self) -> Option<&Continuation> {
        self.seal.as_ref()
    }

    /// The predecessor's chain head, which is the position this journal's first
    /// frame chains from.
    #[must_use]
    pub const fn base(&self) -> JournalPosition {
        self.base
    }

    /// The position this journal's committed events chain from.
    ///
    /// What [`verify_chain_from`](crate::journal::verify_chain_from) takes for this
    /// journal: the genesis for a base journal, and the seal frame's own position for
    /// a successor. Both are facts about the file rather than about a caller's view of
    /// it, which is what makes a successor's chain checkable at all.
    #[must_use]
    pub const fn chain_from(&self) -> JournalPosition {
        self.chain_from
    }

    /// Where this handle's own seal sent the authoritative journal.
    #[must_use]
    pub fn sealed_by(&self) -> Option<&Path> {
        self.sealed_by.as_deref()
    }

    /// The successor of this journal's path.
    ///
    /// The same name the checkpoint carries and the walk follows, so a caller
    /// predicting where a continuation will go and a reader following one to its
    /// end cannot disagree.
    #[must_use]
    pub fn successor_path(&self) -> PathBuf {
        successor_path(&self.path)
    }

    /// Open, with the device's answering behaviour chosen by the caller.
    ///
    /// One implementation for every constructor: a stalled device is a property of
    /// the storage owner and a continuing journal is a declared policy, and neither
    /// is a second way to open a journal.
    ///
    /// # Errors
    ///
    /// Whatever the file, the lock, the scan or the authority rule reports.
    fn open_impl(path: &Path, kind: OpenKind) -> Result<Self, JournalError> {
        let path = path.to_path_buf();
        super::continuation::refuse_ambiguous_base(&path)?;
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
        let scanned = scan(&mut reader, MAX_JOURNAL_EVENTS)?;
        drop(reader);
        let Scanned {
            entries,
            carried,
            seal,
            chain_from,
            tail,
            stop,
        } = scanned;

        // The authority rule, asked before anything is repaired: a journal whose
        // successor is complete is not this run's journal, and repairing its tail
        // would be writing to a file this process does not own.
        if let Some(checkpoint) = seal.as_ref() {
            // The successor's name is a property of *this* file, read from the
            // path the caller already opened rather than from the checkpoint: that
            // is what lets the seal frame be byte-identical in both files.
            let next = successor_path(&path);
            if successor_is_complete(&next, checkpoint) {
                let refusal = Err(JournalError::Superseded {
                    path: next.display().to_string(),
                });
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "open_impl: this journal is sealed and its successor is authoritative");
                return refusal;
            }
        }

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
                let index = u64::saturating_from(entries.len());
                // The cut is resolved against the last whole frame, whatever it
                // was: the carried seal of a successor with no event of its own,
                // this file's own seal, or its last event. Reading it off the
                // events alone named the genesis for the first two, and a later
                // whole frame chained from a seal would not authenticate there, so
                // an acknowledged frame under a lying length read as a torn append.
                let offset = resolve_ambiguous_tail(&mut file, offset, tail, index)?;
                file.set_len(offset).map_err(JournalError::Storage)?;
                file.sync_all().map_err(JournalError::Storage)?;
                (offset, true)
            }
        };

        let mut ladder: HashMap<crate::effect::EffectKey, EventKind> = entries
            .iter()
            .map(|entry| (entry.event().key(), entry.event().kind()))
            .collect();
        let mut outcomes = HashMap::new();
        for entry in &entries {
            if let EffectEvent::OutcomeObserved { key, evidence } = *entry.event() {
                outcomes.insert(key, (entry.position(), evidence));
            }
        }
        // The carried state seeds the append fence's own indexes: the same ladder
        // and the same folded folds, so a successor's append check is literally the
        // predecessor's with the checkpoint as where the index was primed.
        let mut folded = Vec::new();
        let mut base = JournalPosition::genesis();
        if let Some(checkpoint) = carried.as_ref() {
            base = checkpoint.predecessor();
            folded = checkpoint.settled().to_vec();
            for held in checkpoint.unresolved() {
                if let Some(rung) = held.rung() {
                    ladder_hold(&mut ladder, held.key(), rung);
                }
            }
            for held in checkpoint.settled() {
                if let Some(rung) = held.rung() {
                    ladder_hold(&mut ladder, held.key(), rung);
                }
            }
        }

        let storage =
            StorageOwner::spawn(file, (), kind.stalled()).map_err(JournalError::Storage)?;
        let view = FileView::read_only(&path)?;
        Ok(Self {
            path,
            storage,
            view,
            committed: entries,
            position: tail,
            ladder,
            outcomes,
            disk_len: acked_len,
            torn_tail_repaired,
            continuing: kind.continues(),
            continuation: kind.policy(),
            carried,
            seal,
            base,
            chain_from,
            folded,
            sealed_by: None,
            pause: None,
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
        let requested = u64::saturating_from(self.committed.len()).saturating_add(additional);
        let limit = u64::saturating_from(MAX_JOURNAL_EVENTS);
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
    ///
    /// A successor folds its carried checkpoint first, so the answer is the same
    /// one its predecessor would have given: an attempt that was
    /// `OutcomeUnknown` before a continuation is `OutcomeUnknown` after it, with
    /// the same verification digest on the settled ones.
    #[must_use]
    pub fn recover(&self) -> Recovered {
        recover_continued(self.carried.as_ref(), self.events())
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
        self.refuse_sealed()?;
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

    /// Refuse every write on a handle that sealed its own journal.
    ///
    /// Asked **first**, ahead of the ladder and the length fence, because a sealed
    /// journal has no ladder answer left to give: its successor is the journal,
    /// and a caller that got `OutOfOrder` or `AttemptAlreadyWalked` instead would
    /// be told about a fact about *this* file when the truth is that this file is
    /// not the one to write to any more. The other refusals still name what they
    /// found, and they are the right answer on a handle that has not sealed.
    fn refuse_sealed(&self) -> Result<(), JournalError> {
        let Some(next) = self.sealed_by.as_ref() else {
            return Ok(());
        };
        let refusal = Err(JournalError::Superseded {
            path: next.display().to_string(),
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "refuse_sealed: this handle sealed its journal and is read-only");
        refusal
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
        let requested = self.disk_len.saturating_add(u64::saturating_from(staged));
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

    /// Whether this handle continues at its declared trigger points.
    ///
    /// The `continuing` flag and the policy are one fact read twice, so they are
    /// kept in step at construction and this is the only place that compares them.
    /// A handle opened by [`FileJournal::open`] carries the declared policy and
    /// `false`, which is why it answers `false` rather than consulting the policy
    /// it never asked for.
    fn continuation_continues(&self) -> bool {
        self.continuing
    }

    /// Where a continuation of this handle would go.
    fn successor(&self) -> PathBuf {
        successor_path(&self.path)
    }

    /// The fold every continuation carries, read from this handle's own history.
    ///
    /// Two lists, and the split between them is the whole design: the **unresolved**
    /// attempts in full identity, because dropping one would be the one thing this
    /// mechanism must never do, and one folded record per **action** for everything
    /// already settled, because `AttemptId` is monotonic per action so that one
    /// record answers "was this walked?" for every attempt behind it.
    fn carried(&self) -> (Vec<SettledAttempt>, Vec<UnresolvedAttempt>) {
        let recovered = self.recover();
        let mut settled: Vec<SettledAttempt> = Vec::new();
        let mut unresolved: Vec<UnresolvedAttempt> = Vec::new();
        // One record per *action*, kept in first-seen order, which is what bounds
        // the carry by the shape of a bot rather than by how long it has run. An
        // action already folded is replaced in place, so the vector's order stays
        // the admission order a recovery fold reads in and the record is the
        // latest attempt of that action rather than the first one seen.
        let mut folded_at: HashMap<crate::effect::ActionId, usize> = HashMap::new();
        for attempt in recovered.attempts() {
            match attempt.status() {
                AttemptStatus::Prepared => {
                    unresolved.push(UnresolvedAttempt::new(
                        attempt.key(),
                        EventKind::IntentAdmitted,
                    ));
                }
                AttemptStatus::OutcomeUnknown => {
                    unresolved.push(UnresolvedAttempt::new(
                        attempt.key(),
                        EventKind::DispatchPrepared,
                    ));
                }
                status => {
                    let rung = if recovered
                        .history(attempt.key())
                        .last()
                        .is_some_and(|change| {
                            change.to() == AttemptStatus::Verified
                                || change.to() == AttemptStatus::VerificationFailed
                        }) {
                        EventKind::Verified
                    } else {
                        EventKind::OutcomeObserved
                    };
                    let record =
                        SettledAttempt::new(attempt.key(), rung, status, attempt.verification());
                    match folded_at.get(&record.action()) {
                        Some(at) => {
                            if let Some(slot) = settled.get_mut(*at) {
                                *slot = record;
                            }
                        }
                        None => {
                            let _ = folded_at.insert(record.action(), settled.len());
                            settled.push(record);
                        }
                    }
                }
            }
        }
        (settled, unresolved)
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
                .map(|_| ())
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
            .map(|_| ())
            .map_err(write_refusal)
    }

    /// Fold a committed frame into this handle's view of the journal.
    ///
    /// The acknowledgment is minted from the position the frame was chained at,
    /// so the two cannot disagree.
    fn accept(&mut self, event: &EffectEvent, position: JournalPosition, frame_len: usize) {
        self.committed.push(JournalEntry::new(position, *event));
        self.position = position;
        self.ladder.insert(event.key(), event.kind());
        if let EffectEvent::OutcomeObserved { key, evidence } = *event {
            self.outcomes.insert(key, (position, evidence));
        }
        self.disk_len = self
            .disk_len
            .saturating_add(u64::saturating_from(frame_len));
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
        self.refuse_sealed()?;
        let actual = self.tail();
        check_append_order(
            expected_tail,
            actual,
            event,
            self.ladder.get(&event.key()).copied(),
        )?;
        self.refuse_walked(event)?;
        self.fence()?;
        self.bound_events(1)?;
        let (position, frame) = self.frame(event, actual)?;
        self.bound_bytes(frame.len())?;
        Ok((position, frame))
    }

    /// The seal, handing back the successor as the concrete adapter.
    ///
    /// [`EffectJournal::continue_as_new`] is this wrapped in the trait object a
    /// controller holds, and this is the form a caller with a typed handle wants:
    /// the successor's [`FileJournal::checkpoint`], [`FileJournal::base`] and
    /// [`FileJournal::compare_and_append_all`] are all this adapter's own surface,
    /// and a caller that had to reach them through the trait could not read the
    /// thing it just sealed.
    ///
    /// # Errors
    ///
    /// Every [`JournalError`] [`EffectJournal::continue_as_new`] reports.
    pub fn continue_as_file(&mut self) -> Result<Option<Self>, JournalError> {
        if !self.continuation_continues() {
            return Ok(None);
        }

        if let Some(next) = self.sealed_by.clone() {
            let refusal = Err(JournalError::Superseded {
                path: next.display().to_string(),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "continue_as_new: this handle already sealed its journal");
            return refusal;
        }
        let successor = self.successor();
        let (settled, unresolved) = self.carried();
        let checkpoint = Continuation::new(
            self.generation().saturating_add(1),
            self.tail(),
            settled,
            unresolved,
        )?;
        let payload = checkpoint.to_payload()?;
        let (_position, framed) = self.frame_payload(payload, self.tail())?;
        let expected = self.disk_len;
        let pause = self.pause.take();
        let sealed_at = successor.clone();
        let answer = self
            .storage
            .submit(move |file, _state| seal_on_owner(file, expected, &framed, &sealed_at, pause))
            .map_err(write_refusal)?;
        match answer {
            SealOutcome::Sealed { bytes } => {
                self.disk_len = self.disk_len.saturating_add(u64::saturating_from(bytes));
                self.sealed_by = Some(successor.clone());
                Ok(Some(Self::open_impl(
                    &successor,
                    OpenKind::Continuing(self.continuation),
                )?))
            }
            SealOutcome::Paused(boundary) => {
                self.sealed_by = Some(successor);
                let refusal = Err(JournalError::ContinuationPaused { boundary });
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "continue_as_new: an armed fault injector stopped the seal");
                refusal
            }
            // The seal job and the append job share one answer type and one thread,
            // and neither can answer with the other's arm. This arm is therefore
            // unreachable by construction, and is typed rather than a panic.
            SealOutcome::Appended => {
                let refusal = Err(JournalError::Storage(std::io::Error::other(
                    "the seal reported an append's answer; the journal's own steps disagree",
                )));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "continue_as_new: returning an error to the caller");
                refusal
            }
        }
    }

    /// Refuse an append naming an attempt the sealed history already walked.
    ///
    /// The counterpart of carrying one folded record per action rather than every
    /// attempt: this is where "at or below the latest attempt of that action"
    /// becomes a refusal instead of a promise nobody keeps. The comparison is on
    /// `AttemptId`, which is monotonic per `ActionId` and never reused, so it is
    /// the same question the ladder asks for a key the journal *does* hold and
    /// the same answer: this attempt was already recorded.
    fn refuse_walked(&self, event: &EffectEvent) -> Result<(), JournalError> {
        let key = event.key();
        if self.ladder.contains_key(&key) {
            return Ok(());
        }
        let walked = self
            .folded
            .iter()
            .find(|entry| entry.already_walked(key))
            .map(|entry| entry.attempt());
        match walked {
            Some(latest) => {
                let refusal = Err(JournalError::AttemptAlreadyWalked {
                    key: Box::new(key),
                    latest,
                });
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "refuse_walked: the sealed history already walked this attempt");
                refusal
            }
            None => Ok(()),
        }
    }

    /// Frame an already-archived payload at the end of this chain.
    ///
    /// The shared framing step, taken with this journal's own head function, so a
    /// sealed checkpoint is chained exactly as an effect event is. That is what
    /// lets the successor's first frame be the predecessor's last one byte for
    /// byte, and it is the whole reason no second chain function exists here.
    ///
    /// # Errors
    ///
    /// [`JournalError::Exhausted`] when the position cannot advance,
    /// [`JournalError::Storage`] when the payload is past the frame bound, and
    /// whatever [`Self::fence`] reports, because a seal is a write like any other.
    fn frame_payload(
        &self,
        payload: Vec<u8>,
        from: JournalPosition,
    ) -> Result<(JournalPosition, Vec<u8>), JournalError> {
        self.fence()?;
        let sequence = from
            .sequence()
            .checked_add(1)
            .ok_or(JournalError::Exhausted)?;
        let (framed, head) = super::frame::frame_record::<Vec<u8>, _, _, _, _>(
            &payload,
            &from.head,
            MAX_FRAME_BYTES,
            |archived: &Vec<u8>| Ok(archived.clone()),
            |_payload, previous, archived| super::chain_over_bytes(previous, archived),
            |_len| {
                JournalError::Storage(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "the sealed checkpoint exceeds this journal's frame bound",
                ))
            },
        )?;
        Ok((JournalPosition { sequence, head }, framed))
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
        Self::open_impl(path.as_ref(), OpenKind::Stalled)
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
        // empty case: a stale or sealed handle answers for itself, not with an
        // empty success.
        self.fence()?;
        if events.is_empty() {
            return Ok(Vec::new());
        }
        self.bound_events(u64::saturating_from(events.len()))?;

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

/// The length fence and the write it guards, as one step.
///
/// Both doors that write a frame open with this: the append and the seal. They are
/// the same check over the same number for the same reason — an append that wrote
/// past a file that had moved would fork the chain — and the seal inherits the
/// append's fence rather than being trusted to re-check it.
fn fence_and_write(file: &mut File, expected_len: u64, bytes: &[u8]) -> std::io::Result<()> {
    let on_disk = file.metadata()?.len();
    if on_disk != expected_len {
        let refusal = Err(stale_file());
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "fence_and_write: returning an error to the caller");
        return refusal;
    }
    file.write_all(bytes)
}

/// The device error for a file whose acknowledged length no longer matches.
///
/// One definition because the append and the seal ask the same question at the
/// same layer — "is this still the file I hold?" — and two spellings of the
/// answer would make a caller grep for one of them.
fn stale_file() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "the journal file moved under this controller; reopen before appending",
    )
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
) -> std::io::Result<super::owner::Stage<SealOutcome, ()>> {
    fence_and_write(file, expected_len, frames)?;
    // A journal folds no state of its own, so the settle closure is empty: the
    // bytes and the acknowledgment are the whole of the step. The one thing that
    // does move is `self.disk_len`, and that is the caller's, mutated after its
    // answer lands rather than here — so two appends can never have folded a length
    // the file had not reached.
    Ok(super::owner::Stage::Unsynced {
        answer: SealOutcome::Appended,
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
        self.position
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
        // `committed` holds this file's own events, and a successor's first one is
        // not sequence one: its sequences continue from the seal frame it was opened
        // from. Indexed from the genesis instead, every position a continued
        // journal acknowledged read back as absent, and a controller settling an
        // attempt across a continuation reported its outcome as unrecorded.
        let Some(index) = position
            .sequence()
            .checked_sub(self.chain_from.sequence())
            .and_then(|offset| offset.checked_sub(1))
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

    /// How much of each ceiling this journal has used, and whether it continues.
    ///
    /// Read from the handle's own committed count and acknowledged byte length,
    /// which are the two numbers the append fence already keeps exact. There is no
    /// third source and no rounding: the watermark falls where the constants say,
    /// and a caller reading this can compare it against the same ceiling every
    /// other refusal names.
    fn continuation_watermark(&self) -> Result<ContinuationWatermark, JournalError> {
        let events = u64::saturating_from(self.committed.len());
        let limits = (u64::saturating_from(MAX_JOURNAL_EVENTS), MAX_JOURNAL_BYTES);
        let watermark = ContinuationWatermark::measured(
            events,
            limits.0,
            self.disk_len,
            limits.1,
            self.continuation,
        );
        Ok(match self.continuation_continues() {
            true => watermark,
            false => ContinuationWatermark::inert(events, limits.0, self.disk_len, limits.1),
        })
    }

    /// Seal this journal and hand back the successor opened from the checkpoint.
    ///
    /// One ordered step on the storage owner, because a continuation writes two
    /// files and the fence that guards the predecessor's length has to run on the
    /// thread that holds it. The step is: the seal frame into this journal, the
    /// same bytes into the successor, the successor's flush, its directory entry's
    /// flush, and then the batch's one flush over the predecessor's seal — which
    /// is the order the crash table in [`super::continuation`] reasons about, in
    /// that order.
    ///
    /// `None` for a journal opened without a lifecycle, which is what leaves every
    /// existing behaviour of this adapter unchanged.
    ///
    /// # Errors
    ///
    /// [`JournalError::Superseded`] when this handle already sealed its journal,
    /// [`JournalError::CapacityExceeded`] when the carried state is past a declared
    /// bound or the frame is past the shared ceiling,
    /// [`JournalError::ContinuationPaused`] when an armed fault injector stopped the
    /// seal, and [`JournalError::OutcomeUnknown`] when the device's answer was lost
    /// after bytes may have moved.
    fn continue_as_new(&mut self) -> Result<Option<Box<dyn EffectJournal>>, JournalError> {
        let opened = self.continue_as_file()?;
        Ok(opened.map(|journal| {
            let boxed: Box<dyn EffectJournal> = Box::new(journal);
            boxed
        }))
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
    use crate::journal::chain;
    use crate::journal::frame::probe::{declared_at, frame_starts, with_prefix};
    use crate::journal::{AttemptStatus, EventKind};

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

    /// A scratch path unique to one test run.
    ///
    /// Shared with `journal::owner`'s tests, so the journal's two test suites name
    /// their scratch files one way (INV-DEP-6). The construction itself is
    /// `frame::probe`'s, because the two halves of a unique name are not a detail a
    /// second copy may spell differently.
    pub(in crate::journal) fn scratch(name: &str) -> Result<PathBuf, std::io::Error> {
        crate::journal::frame::probe::scratch_path("journal-file", name)
    }

    /// The one key builder: every field but the attempt is this module's fixture
    /// constant, and a test that wants a second attempt differs only in that
    /// field. Building it in three copies meant three places for a field to differ.
    fn key_for_attempt(
        attempt: &str,
    ) -> Result<crate::effect::EffectKey, Box<dyn std::error::Error>> {
        let run = RunId::from_hex(RUN)?;
        let action = ActionId::from_hex(ACTION)?;
        let attempt = AttemptId::from_decimal(attempt)?;
        let flow = FlowRevision::from_tagged("blake3_256", FLOW_HEX)?;
        let digest = ActionDigest::from_tagged("blake3_256", DIGEST_HEX)?;
        let environment = EnvironmentId::from_hex(ENV)?;
        let epoch = EnvironmentEpoch::from_decimal("1")?;
        Ok(crate::effect::EffectIdentity::new(run, environment, flow)
            .key(action, attempt, digest, epoch))
    }

    /// The first attempt of the run every test in this module writes.
    fn key() -> Result<crate::effect::EffectKey, Box<dyn std::error::Error>> {
        key_for_attempt("1")
    }

    /// A scratch path and the guard that removes it when the test ends.
    ///
    /// Both halves in one value because both are needed before anything is
    /// written, and a guard returned beside its path cannot be dropped by a test
    /// that only meant to keep the path.
    fn subject(name: &str) -> Result<(PathBuf, TempGuard), Box<dyn std::error::Error>> {
        let path = scratch(name)?;
        let guard = TempGuard(path.clone());
        Ok((path, guard))
    }

    /// The successor writer reports a generation directory it made, and only one.
    ///
    /// The answer decides whether the directory's own name is flushed into its
    /// parent, so an answer of "already there" for a directory this call created
    /// is a new generation whose name a power loss can take back. Read after the
    /// create, it was always "already there".
    #[test]
    fn writing_a_successor_reports_the_directory_it_created() -> TestResult {
        let (base, _guard) = subject("successor-dir")?;
        std::fs::create_dir_all(&base)?;
        let first = base.join("run.jrnl.cont").join("000001");
        assert!(
            write_successor(&first, b"frame")?,
            "the first generation's directory is created by this write"
        );
        let second = base.join("run.jrnl.cont").join("000002");
        assert!(
            !write_successor(&second, b"frame")?,
            "a later generation writes into a directory that already exists"
        );
        assert_eq!(std::fs::read(&second)?, b"frame");
        Ok(())
    }

    #[test]
    fn a_batched_ladder_is_four_acknowledgments_from_one_sync() -> TestResult {
        let (path, _guard) = subject("batch")?;
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
        key_for_attempt(&n.to_string())
    }

    /// A second key, so a batch can fail on its own rung.
    fn key2() -> Result<crate::effect::EffectKey, Box<dyn std::error::Error>> {
        key_for_attempt("7")
    }

    #[test]
    fn a_batch_climbs_a_key_the_disk_already_knows() -> TestResult {
        let (path, _guard) = subject("batch-committed")?;
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
        let (path, _guard) = subject("replay")?;
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
        let (path, _guard) = subject("confirm")?;
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
        let (path, _guard) = subject("outcome-index")?;
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
        let (path, _guard) = subject("length-rot")?;
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
        let len_bytes = u32::saturating_from(LENGTH_BYTES);
        let head_bytes = u32::saturating_from(HEAD_BYTES);
        let second = usize::saturating_from(
            first_len
                .saturating_add(len_bytes)
                .saturating_add(head_bytes),
        )
        .min(bytes.len());
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
        let (path, _guard) = subject("undecodable")?;
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
        let payload_len = usize::saturating_from(raw_len);
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
        let (path, _guard) = subject("ladder")?;
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
            // A `serve` this host cannot count is at least every byte the buffer
            // holds, so the ceiling below is what bounds the read either way.
            let cap = usize::saturating_from(remaining).min(buf.len());
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
            match scan(&mut faulty, MAX_JOURNAL_EVENTS) {
                Err(JournalError::Storage(_)) => {}
                Err(other) => {
                    return Err(format!(
                        "a fault {serve} bytes in was classified {other}, not storage"
                    )
                    .into());
                }
                Ok(scanned) => {
                    let stop = scanned.stop;
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

        match scan(&mut reader, 1) {
            Err(JournalError::CapacityExceeded {
                resource: JournalLimitKind::Events,
                limit,
                requested,
            }) => {
                assert_eq!(limit, 1);
                assert_eq!(requested, 2);
            }
            Err(other) => return Err(format!("expected event-limit refusal, got {other}").into()),
            Ok(scanned) => {
                let entries = scanned.entries;
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
        let (path, _guard) = subject("byte-limit")?;
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
        let (path, _guard) = subject("event-limit")?;
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
        let (path, _guard) = subject("inflated-length")?;
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
        let (path, _guard) = subject("torn-tail")?;
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
        let (path, _guard) = subject("lengthened-final")?;
        let (bytes, third) = three_frame_file(&path)?;
        let declared = declared_at(&bytes, third);
        for extra in 1u32..=1024 {
            let lied = with_prefix(&bytes, third, declared + extra);
            std::fs::write(&path, &lied)?;
            require_refused_untouched(&path, &lied, 2, &format!("final frame L+{extra}"))?;
        }
        Ok(())
    }

    /// The same lie on a successor whose one event follows the seal it carries.
    ///
    /// The cut is resolved against the last whole frame, and in a successor with
    /// no event before the lie that frame is the carried seal. Resolved against
    /// the events alone it was the genesis, which the acknowledged event does not
    /// chain from, so the lie read as a torn append and the event was trimmed.
    #[test]
    fn a_lengthened_event_behind_a_carried_seal_is_refused_not_trimmed() -> TestResult {
        let (dir, _guard) = subject("lengthened-successor")?;
        std::fs::create_dir_all(&dir)?;
        let successor = {
            let mut journal = FileJournal::open_continuing_with(
                dir.join("run.jrnl"),
                crate::journal::ContinuationPolicy::declared(),
            )?;
            let first = EffectEvent::IntentAdmitted {
                key: attempt_key(1)?,
            };
            journal.compare_and_append(journal.tail(), &first)?;
            let mut next = journal
                .continue_as_file()?
                .ok_or("a continuing journal hands back its successor")?;
            let second = EffectEvent::IntentAdmitted {
                key: attempt_key(2)?,
            };
            next.compare_and_append(next.tail(), &second)?;
            next.path().to_path_buf()
        };
        let bytes = std::fs::read(&successor)?;
        let event = frame_starts(&bytes, 0)?[1];
        let declared = declared_at(&bytes, event);
        for extra in 1u32..=96 {
            let lied = with_prefix(&bytes, event, declared + extra);
            std::fs::write(&successor, &lied)?;
            require_refused_untouched(&successor, &lied, 0, &format!("successor event L+{extra}"))?;
        }
        Ok(())
    }

    /// The second shape from the same finding: an acknowledged frame with frames
    /// behind it whose length is inflated past the end of the file. The true frame
    /// is still the first thing behind the prefix, so it authenticates and the two
    /// frames after it are not read as one more candidate and dropped.
    #[test]
    fn an_inflated_non_final_length_is_refused_and_every_byte_survives() -> TestResult {
        let (path, _guard) = subject("inflated-middle")?;
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
        let (path, _guard) = subject("cut-every-byte")?;
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
        let (path, _guard) = subject("damaged-middle")?;
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
        let (path, _guard) = subject("two-faults")?;
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
        let (path, _guard) = subject("replay-lengthened")?;
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
        let (path, _guard) = subject("batch-poison")?;
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

        let (path, _guard) = subject("stalled-device")?;
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

        let (path, _guard) = subject("dropped-waiter")?;
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
