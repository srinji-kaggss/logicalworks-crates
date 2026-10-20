//! Scanning a journal file: reading frames, classifying a torn or lying tail, and verifying each frame's chain.

use super::{
    ChainBreak, Continuation, Corruption, CorruptionKind, EffectEvent, File, FramePiece,
    HEAD_BYTES, JournalEntry, JournalError, JournalLimitKind, JournalPosition, LENGTH_BYTES,
    MAX_FRAME_BYTES, Prefix, Read, SaturatingFrom, WireError, from_bytes,
};

/// Which read failure a frame scan hit, and where.
#[derive(Debug)]
pub(super) enum ScanStop {
    /// Clean end of file at the given offset.
    Complete(u64),
    /// The bytes from `offset` onward are an interrupted append: torn.
    Torn(u64),
    /// The file ended inside a frame whose declared length ran past the end, in
    /// its payload or in its head. Either an append the writer never finished, or
    /// a complete frame whose length prefix lies — the two are byte-identical
    /// here, and [`FileJournal::open`](crate::journal::FileJournal::open) resolves which with the frame's own head.
    AmbiguousTail {
        /// Where the ambiguous frame's length prefix begins.
        offset: u64,
    },
}

/// Read a fixed-size piece of a frame, classifying the ending.
///
/// The crate-wide grammar in [`super::super::frame`] makes the classification, so this
/// journal and the run store cannot disagree about which early end is repairable;
/// this wrapper only maps the device's refusal onto this module's error vocabulary.
pub(super) fn read_exact_classified(
    reader: &mut impl Read,
    buf: &mut [u8],
) -> Result<FramePiece, JournalError> {
    super::super::frame::read_piece(reader, buf).map_err(JournalError::Storage)
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
pub(super) fn resolve_ambiguous_tail(
    file: &mut File,
    offset: u64,
    position: JournalPosition,
    index: u64,
) -> Result<u64, JournalError> {
    let acknowledged = super::super::frame::cut_holds_acknowledged(
        file,
        offset,
        &position.head(),
        MAX_FRAME_BYTES,
        JournalError::Storage,
        |previous, payload| {
            is_framed_record(payload).then(|| super::super::chain_over_bytes(previous, payload))
        },
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

/// Whether `payload` decodes as a record this journal's writer frames: a sealed
/// checkpoint or an effect event.
///
/// A head is reproduced only by bytes a writer framed, and the writer frames only
/// these two, so a candidate that decodes as neither cannot authenticate and is not
/// hashed. The tail search tries every payload length under the bytes present and a
/// wrong length fails decoding at once, as it does in the run store; without this,
/// every candidate paid a blake3 over its length and a 64 KiB noise tail cost about
/// 1.4 s to open (`--example tail_cost`). A damaged payload is still found by the
/// search's second pass, which chains from the stored head before a later frame.
///
/// The search hands over a slice of the file at whatever offset the candidate
/// starts, and the archive is read only from an aligned copy, as the run store
/// reads its candidates (`frame::decodable`).
pub(super) fn is_framed_record(payload: &[u8]) -> bool {
    let aligned = super::super::frame::decodable(payload);
    let bytes = aligned.as_slice();
    match Continuation::from_payload(bytes) {
        Some(decoded) => decoded.is_ok(),
        None => from_bytes::<EffectEvent, WireError>(bytes).is_ok(),
    }
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
    match super::super::frame::read_prefix(reader, &mut prefix).map_err(JournalError::Storage)? {
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
    let payload_len = super::super::frame::declared_length(&prefix);
    if !super::super::frame::is_possible_length(payload_len, MAX_FRAME_BYTES) {
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
pub(super) fn scan(reader: &mut impl Read, max_events: usize) -> Result<Scanned, JournalError> {
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
        offset = offset.saturating_add(super::super::frame::framed_len(payload_len));
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
pub(super) fn verify_frame(
    payload: &[u8],
    head: &[u8; HEAD_BYTES],
    position: JournalPosition,
    index: u64,
) -> Result<JournalPosition, JournalError> {
    let recomputed = JournalPosition {
        sequence: position.sequence().saturating_add(1),
        head: super::super::chain_over_bytes(&position.head(), payload),
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
pub(super) struct Scanned {
    /// Every effect event committed before the stop, in commit order.
    pub(super) entries: Vec<JournalEntry>,
    /// The predecessor's seal, when this file is a successor.
    pub(super) carried: Option<Continuation>,
    /// This file's own seal, when it has sealed itself.
    pub(super) seal: Option<Continuation>,
    /// The position the committed events' chain starts from.
    ///
    /// The genesis for a base journal and the seal frame's own position for a
    /// successor, because the seal frame is not an event and so is not in
    /// `committed_entries`: folding a successor's events from the genesis, or from
    /// the predecessor's tail, disagrees at its very first entry.
    pub(super) chain_from: JournalPosition,
    /// The last position the scan verified, which is the journal's tail.
    ///
    /// Read from the loop rather than recomputed from the entries, because a
    /// journal that has sealed itself has a tail one past its last event and a
    /// seal frame is not an event.
    pub(super) tail: JournalPosition,
    /// Where the scan stopped.
    pub(super) stop: ScanStop,
}
