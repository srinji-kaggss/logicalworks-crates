//! The frame grammar both file-backed append logs in this crate share.
//!
//! Two stores write a disk: [`FileJournal`](super::FileJournal) and
//! [`RunStore`](crate::task::RunStore). Both put a record on it the same way —
//! a `u32` big-endian payload length, the archived record, then the 32-byte head
//! the append commits to — and both read it back the same way, classifying the
//! three ways a frame's bytes can end. That grammar is here, once, so the torn
//! tail discipline and the refusal of a non-following frame cannot drift between
//! the two stores: a second definition of "what a torn tail is" is a store that
//! trims bytes another store would have refused.
//!
//! # What is *not* here
//!
//! What a record *means* and what its head is over. Those are the caller's: the
//! journal chains effect positions and [`RunStore`](crate::task::RunStore) chains
//! step records, and neither is derivable from the other. So this module is pure
//! grammar — how many bytes a frame occupies, how a prefix classifies, when a
//! length is possible — and each store keeps its own loop, its own chain
//! verification and its own error vocabulary around it.
//!
//! # The torn-tail rule, once
//!
//! A write leaves only a *prefix* of its bytes, so bytes from some offset onward
//! that do not form a whole frame were never anyone's answer and may be trimmed.
//! A *complete* length prefix naming a frame no writer of this crate can produce
//! is the opposite: the writer finished computing its length, so those bytes were
//! acknowledged and are refused. The two are byte-identical to a reader that only
//! looks at the prefix, which is why the prefix alone never decides anything.

use std::io::Read;

use lgwks_std::hash::Digest;

/// The byte width of a frame's length prefix.
///
/// A `u32`, because that is what both stores write and what a reader that has
/// to be stream-bounded can decode without a full record in hand.
pub(crate) const LENGTH_BYTES: usize = 4;

/// The byte width of a frame's stored head.
pub(crate) const HEAD_BYTES: usize = 32;

/// How a fixed-size piece of a frame's bytes ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Piece {
    /// The buffer filled: the piece is whole.
    Filled,
    /// The source ended before the buffer did, which is the only evidence a tail
    /// may be repaired on.
    Interrupted,
}

/// Read a fixed-size piece of a frame, classifying the ending.
///
/// Only a true early end reads as [`Piece::Interrupted`]. Every other error is
/// the device refusing and travels unchanged as the caller's own refusal: a fault
/// is not evidence that the bytes after it were never written, and treating it as
/// one would authorize repair over bytes that may be acknowledged.
pub(crate) fn read_piece(reader: &mut impl Read, buf: &mut [u8]) -> Result<Piece, std::io::Error> {
    match reader.read_exact(buf) {
        Ok(()) => Ok(Piece::Filled),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(Piece::Interrupted),
        Err(error) => Err(error),
    }
}

/// How a length prefix read ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Prefix {
    /// Nothing at all: a clean end of records.
    Eof,
    /// Some of the prefix arrived: an interrupted append.
    Torn,
    /// The whole prefix arrived, and `buf` holds it.
    Full,
}

/// Read the length prefix, classifying the three ways it can end.
///
/// This is a counted read rather than the [`read_piece`] above, because the two
/// torn cases are byte-identical unless the reader knows how many bytes arrived:
/// a clean end is zero bytes and an interrupted append is one to three. Reading
/// into a buffer of exactly [`LENGTH_BYTES`] and counting is what tells them apart
/// without trusting the bytes' values — a torn write whose bytes happened to be
/// zero is still an interrupted append, and must be trimmed as one.
pub(crate) fn read_prefix(
    reader: &mut impl Read,
    buf: &mut [u8; LENGTH_BYTES],
) -> Result<Prefix, std::io::Error> {
    let mut filled = 0usize;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(read) => filled = filled.saturating_add(read),
            Err(ref error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(match filled {
        0 => Prefix::Eof,
        n if n < buf.len() => Prefix::Torn,
        _ => Prefix::Full,
    })
}

/// The payload length a whole prefix declares.
pub(crate) fn declared_length(prefix: &[u8; LENGTH_BYTES]) -> usize {
    usize::try_from(u32::from_be_bytes(*prefix)).unwrap_or(usize::MAX)
}

/// Whether a declared length can be true of any frame either store writes.
///
/// A zero-length frame is never written, and a length past a store's own ceiling
/// names a frame that store does not produce. A complete prefix carrying one is
/// rot or a hand, not an interrupted append, and is refused rather than trimmed.
pub(crate) fn is_possible_length(payload_len: usize, max_frame_bytes: usize) -> bool {
    payload_len != 0 && payload_len <= max_frame_bytes
}

/// A payload's length as the prefix carries it, or `None` when this store would
/// never write a frame of that size.
///
/// The one conversion both a reader and a writer need, and both halves of the
/// question: representable in a `u32`, and within the store's own ceiling. A store
/// that asked `is_possible_length` and then did the conversion itself would have
/// two answers to one question, and the second could disagree with the first.
pub(crate) fn writable_length(payload_len: usize, max_frame_bytes: usize) -> Option<u32> {
    u32::try_from(payload_len)
        .ok()
        .filter(|_| is_possible_length(payload_len, max_frame_bytes))
}

/// The bytes one frame occupies: prefix, payload, head.
pub(crate) fn framed_len(payload_len: usize) -> u64 {
    u64::try_from(
        LENGTH_BYTES
            .saturating_add(payload_len)
            .saturating_add(HEAD_BYTES),
    )
    .unwrap_or(u64::MAX)
}

/// One framed record: its length prefix, its archived bytes, and the head the
/// append committed to.
///
/// The head is *stored* rather than recomputed on faith at read time. That is what
/// separates a torn tail — a frame cut short, so it has no head at all — from a
/// frame that does not follow from the records before it, which has one and is
/// refused.
///
/// The length is a parameter rather than something derived here, because every
/// caller has just proved the payload is one it will write: each store checks its
/// own ceiling first, and that check is what makes the conversion safe. Taking the
/// proof as an argument rather than repeating it is what keeps one definition of
/// what a frame is from becoming two.
pub(crate) fn encode(payload_len: u32, payload: &[u8], head: &Digest) -> Vec<u8> {
    let claimed = usize::try_from(payload_len).unwrap_or(usize::MAX);
    debug_assert_eq!(
        claimed,
        payload.len(),
        "a caller framed a payload under a length that is not its own"
    );
    let mut frame = Vec::with_capacity(usize::try_from(framed_len(claimed)).unwrap_or(usize::MAX));
    frame.extend_from_slice(&payload_len.to_be_bytes());
    frame.extend_from_slice(payload);
    frame.extend_from_slice(head.as_bytes());
    frame
}

/// One record's whole framing step: archive it, refuse it if this store's ceiling
/// says no, chain its head over `previous`, and lay out the frame.
///
/// What both stores were doing by hand, and what the review of #87 step 5 named as
/// the duplication: the journal archived an `EffectEvent` and the run store
/// archived a `Stored`, and each then did the same four things to it. Only two of
/// those are the store's — what a record *is*, and what its head chains — so those
/// arrive as closures and the rest lives here, once.
///
/// `archive` reports a codec failure in whatever type the caller's codec uses,
/// because the two stores name theirs differently and neither should have to rename
/// the other's error. `refuse` reports a ceiling the same way, and is called only
/// for a payload past `max_frame_bytes`, so one cause has one error.
///
/// # Errors
///
/// Whatever `archive` reports, or whatever `refuse` reports for a payload past
/// `max_frame_bytes`.
pub(crate) fn frame_record<R, A, H, C, E>(
    record: &R,
    previous: &Digest,
    max_frame_bytes: usize,
    archive: A,
    head: H,
    refuse: C,
) -> Result<(Vec<u8>, Digest), E>
where
    A: FnOnce(&R) -> Result<Vec<u8>, E>,
    H: FnOnce(&R, &Digest, &[u8]) -> Digest,
    C: FnOnce(usize) -> E,
{
    let payload = archive(record)?;
    let Some(payload_len) = writable_length(payload.len(), max_frame_bytes) else {
        return Err(refuse(payload.len()));
    };
    // The head is chained from the record and the archived bytes together, and the
    // bytes are handed to the closure rather than re-encoded inside it: a store that
    // chains over its own fields needs the record, one that chains over the payload
    // needs the bytes, and neither should have to archive a second time to get them.
    let head = head(record, previous, &payload);
    Ok((encode(payload_len, &payload, &head), head))
}
/// The grammar's own tests: the round trip, the three prefix endings, the piece
/// ending, and the two questions about a length.
#[cfg(test)]
mod tests {
    use super::{
        Digest, HEAD_BYTES, LENGTH_BYTES, Piece, Prefix, Read, declared_length, encode, framed_len,
        is_possible_length, read_piece, read_prefix, writable_length,
    };
    use std::io::Cursor;

    /// The classification, with the device's refusal dropped.
    ///
    /// Every read here is over an in-memory cursor that cannot fail, so the error
    /// arm is unreachable by construction; unwrapping it says so rather than
    /// comparing a type that has no `PartialEq`.
    fn prefix_of(reader: &mut impl Read, buf: &mut [u8; LENGTH_BYTES]) -> Prefix {
        read_prefix(reader, buf).unwrap_or(Prefix::Eof)
    }

    /// One piece's classification, with the device's refusal dropped.
    fn piece_of(reader: &mut impl Read, buf: &mut [u8]) -> Piece {
        read_piece(reader, buf).unwrap_or(Piece::Filled)
    }

    /// A head, for the encode tests. Any 32 bytes; nothing here hashes.
    fn head() -> Digest {
        Digest::from_bytes([7u8; 32])
    }

    /// The bytes a payload frames into.
    fn framed(payload: &[u8]) -> Vec<u8> {
        let out = encode(
            writable_length(payload.len(), usize::MAX).unwrap_or(0),
            payload,
            &head(),
        );
        assert_eq!(
            usize::try_from(framed_len(payload.len())).unwrap_or(usize::MAX),
            out.len(),
            "a frame's bytes are exactly the length the grammar says a frame is"
        );
        out
    }

    /// A frame round-trips: the prefix names the payload, and the head is last.
    #[test]
    fn a_frame_round_trips_through_the_shared_grammar() {
        let payload: Vec<u8> = (0..200u16)
            .map(u8::try_from)
            .collect::<Result<Vec<u8>, _>>()
            .unwrap_or_default();
        let bytes = framed(&payload);
        assert_eq!(&bytes[..LENGTH_BYTES], &200u32.to_be_bytes());
        assert_eq!(
            &bytes[LENGTH_BYTES..LENGTH_BYTES + payload.len()],
            &payload[..]
        );
        assert_eq!(&bytes[bytes.len() - HEAD_BYTES..], head().as_bytes());
    }

    /// A clean end of file is `Eof`, and a whole prefix is `Full`.
    #[test]
    fn a_prefix_classifies_eof_full_and_torn() {
        let payload: Vec<u8> = vec![1u8, 2, 3];
        let bytes = framed(&payload);
        let mut clean = Cursor::new(Vec::new());
        let mut buffer = [0u8; LENGTH_BYTES];
        assert_eq!(prefix_of(&mut clean, &mut buffer), Prefix::Eof);

        let mut whole = Cursor::new(bytes.clone());
        assert_eq!(prefix_of(&mut whole, &mut buffer), Prefix::Full);
        assert_eq!(declared_length(&buffer), 3);

        // Any prefix cut is an interrupted append, including one whose bytes are
        // zero: the count is what decides, never the values.
        for cut in 1..LENGTH_BYTES {
            let zeroed: Vec<u8> = bytes[..cut].iter().map(|_| 0u8).collect();
            let mut torn = Cursor::new(zeroed);
            let mut scratch = [0u8; LENGTH_BYTES];
            assert_eq!(prefix_of(&mut torn, &mut scratch), Prefix::Torn);
        }
    }

    /// A frame cut inside a piece reads as `Interrupted`, whatever the piece is.
    #[test]
    fn a_frame_cut_inside_a_piece_is_interrupted() {
        let payload: Vec<u8> = vec![9u8; 16];
        let bytes = framed(&payload);
        // Each cut leaves fewer bytes than a whole-frame read would ask for, which is
        // what "the source ended before the buffer did" means: the read wanted more
        // than arrived, rather than wanting a fixed eight bytes that happened to fit.
        for cut in [LENGTH_BYTES + 1, LENGTH_BYTES + 8, bytes.len() - HEAD_BYTES] {
            let mut source = Cursor::new(bytes[..cut].to_vec());
            let wanted = bytes.len().saturating_sub(cut).saturating_add(1);
            let mut piece = vec![0u8; wanted];
            assert_eq!(piece_of(&mut source, &mut piece), Piece::Interrupted);
        }
    }

    /// Only a non-zero length within the ceiling names a frame either store writes.
    #[test]
    fn only_a_possible_length_is_possible() {
        assert!(is_possible_length(1, 64 * 1024));
        assert!(is_possible_length(64 * 1024, 64 * 1024));
        assert!(!is_possible_length(0, 64 * 1024));
        assert!(!is_possible_length(64 * 1024 + 1, 64 * 1024));
    }

    /// The framed length is prefix plus payload plus head, and never overflows.
    #[test]
    fn a_frames_length_accounts_for_every_piece() {
        assert_eq!(framed_len(0), 36);
        assert_eq!(framed_len(100), 136);
        assert_eq!(framed_len(usize::MAX), u64::MAX);
    }

    /// A frame's bytes are exactly prefix, payload and head — the property both
    /// stores' readers rely on when they compute a frame's length from its parts.
    #[test]
    fn a_frames_bytes_are_prefix_payload_and_head() {
        for size in [1usize, 64, 4096] {
            let payload = vec![7u8; size];
            let bytes = encode(
                writable_length(payload.len(), usize::MAX).unwrap_or(0),
                &payload,
                &head(),
            );
            assert_eq!(bytes.len(), LENGTH_BYTES + size + HEAD_BYTES);
            assert_eq!(&bytes[LENGTH_BYTES..LENGTH_BYTES + size], &payload[..]);
            assert_eq!(
                &bytes[bytes.len() - HEAD_BYTES..],
                head().as_bytes(),
                "the stored head is the frame's last {HEAD_BYTES} bytes"
            );
        }
    }
}
