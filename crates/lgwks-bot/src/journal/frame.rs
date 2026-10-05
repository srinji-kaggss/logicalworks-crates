//! The frame grammar both file-backed append logs in this crate share.
//!
//! Two stores write a disk: [`FileJournal`] and
//! [`RunStore`]. Both put a record on it the same way —
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
//! journal chains effect positions and [`RunStore`] chains
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

use std::io::{Read, Seek, SeekFrom};

use lgwks_std::hash::Digest;

use super::JournalError;

/// Read as much of `buf` as the reader will give, and report how much that was.
///
/// `None` for a reader that has nothing left at all, `Some(n)` for one that
/// stopped short — a short read is the *torn tail* signal rather than an error,
/// because a write that was cut off leaves exactly this shape. Reading in a loop
/// is what makes it a torn-tail detector rather than a single-`read` guess: a
/// reader whose first `read` returns a few bytes is not at end of file, and
/// treating it as such would silently truncate a record that was merely
/// fragmented across reads.
pub(crate) fn read_exact_or_eof(
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
            Err(error) => {
                let refusal = Err(error);
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "read_prefix: returning an error to the caller");
                return refusal;
            }
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
        {
            lgwks_std::trace::debug!(
                payload_len = payload.len(),
                max_frame_bytes,
                "frame_record: the archived record exceeds the frame ceiling"
            );
            return Err(refuse(payload.len()));
        };
    };
    // The head is chained from the record and the archived bytes together, and the
    // bytes are handed to the closure rather than re-encoded inside it: a store that
    // chains over its own fields needs the record, one that chains over the payload
    // needs the bytes, and neither should have to archive a second time to get them.
    let head = head(record, previous, &payload);
    Ok((encode(payload_len, &payload, &head), head))
}

/// Whether the bytes behind a complete length prefix hold a frame this store
/// acknowledged, so that the prefix lied and the bytes must not be trimmed.
///
/// A complete prefix whose frame the file cannot hold is byte-identical in two
/// files: an append that was cut short, and an acknowledged frame whose length
/// field was changed afterwards. The prefix cannot tell them apart and the head can,
/// because a head is a hash of the previous head and the payload, so it is
/// reproduced only by bytes a writer really framed. `suffix` is everything from the
/// first byte after that prefix to the end of the file, and `previous` is the head
/// the cut frame would have chained from. The answer is `true` when either
///
/// - some payload length under the bytes present reproduces the 32 bytes stored
///   behind it, which is the cut frame itself under its true length (the final
///   frame `L -> L + k` case, and an inflated length on a frame with frames behind
///   it, since the true frame is still the first thing in `suffix`); or
/// - a later complete frame authenticates, which is what still refuses when the
///   cut frame itself is damaged as well and so cannot be matched, because failing
///   to match one candidate does not prove that no acknowledged frame follows it.
///   The frame behind a candidate chains from the head the candidate's payload
///   implies (its own stored head may be the damaged part), and a frame anywhere
///   later chains from the 32 bytes just before it (its payload may be the damaged
///   part).
///
/// `head_of` is the store's own head function, over the previous head and the
/// archived payload, and answers `None` for a payload it cannot make a head from.
/// A genuine torn tail is a prefix of one frame, so it holds neither.
///
/// The work is bounded without a budget: a read that came up short was short of
/// at most `max_frame_bytes` plus a head, so `suffix` is never longer than that,
/// the first search is one pass of at most that many candidates, and the second
/// examines each offset once. A hostile tail can make the second search hash a
/// candidate per offset (quadratic in that bound, a fraction of a second at the
/// journal's 64 KiB ceiling) and no more.
pub(crate) fn holds_acknowledged_frame<H>(
    suffix: &[u8],
    previous: &Digest,
    max_frame_bytes: usize,
    head_of: H,
) -> bool
where
    H: Fn(&Digest, &[u8]) -> Option<Digest>,
{
    let authenticates = |previous: &Digest, payload: &[u8], stored: &[u8]| {
        head_of(previous, payload).is_some_and(|head| head.as_bytes().as_slice() == stored)
    };
    let room = suffix.len().saturating_sub(HEAD_BYTES);
    for len in 1..=room.min(max_frame_bytes) {
        let (Some(payload), Some(stored)) = (
            suffix.get(..len),
            suffix.get(len..len.saturating_add(HEAD_BYTES)),
        ) else {
            continue;
        };
        if authenticates(previous, payload, stored) {
            return true;
        }
        // The payload may be whole behind a damaged stored head: the frame after it,
        // if there is one, still chains from the head this payload implies.
        if let Some(implied) = head_of(previous, payload)
            && frame_chains_from(
                suffix,
                len.saturating_add(HEAD_BYTES),
                &implied,
                max_frame_bytes,
                &authenticates,
            )
        {
            return true;
        }
    }
    // The earliest a later frame can begin: one payload byte and its head.
    let mut at = HEAD_BYTES.saturating_add(1);
    while at
        .saturating_add(LENGTH_BYTES)
        .saturating_add(1)
        .saturating_add(HEAD_BYTES)
        <= suffix.len()
    {
        if let Some(before) = suffix
            .get(at.saturating_sub(HEAD_BYTES)..at)
            .and_then(|bytes| <[u8; HEAD_BYTES]>::try_from(bytes).ok())
            && frame_chains_from(
                suffix,
                at,
                &Digest::from_bytes(before),
                max_frame_bytes,
                &authenticates,
            )
        {
            return true;
        }
        at = at.saturating_add(1);
    }
    false
}

/// Whether a whole frame that begins at `at` in `suffix` chains from `previous`.
fn frame_chains_from(
    suffix: &[u8],
    at: usize,
    previous: &Digest,
    max_frame_bytes: usize,
    authenticates: &impl Fn(&Digest, &[u8], &[u8]) -> bool,
) -> bool {
    let payload_at = at.saturating_add(LENGTH_BYTES);
    let Some(prefix) = suffix
        .get(at..payload_at)
        .and_then(|bytes| <[u8; LENGTH_BYTES]>::try_from(bytes).ok())
    else {
        return false;
    };
    let declared = declared_length(&prefix);
    if !is_possible_length(declared, max_frame_bytes) {
        return false;
    }
    let head_at = payload_at.saturating_add(declared);
    let (Some(payload), Some(stored)) = (
        suffix.get(payload_at..head_at),
        suffix.get(head_at..head_at.saturating_add(HEAD_BYTES)),
    ) else {
        return false;
    };
    authenticates(previous, payload, stored)
}

/// Whether the file ended inside a frame it had acknowledged, read from the file.
///
/// The one door each store takes once a scan has read a complete, possible length
/// prefix at `offset` and then hit the end of the file before the payload or the
/// head was whole. It reads everything behind that prefix and asks
/// [`holds_acknowledged_frame`]; `previous` is the head the cut frame would have
/// chained from. A refused answer is the caller's to word, so the journal and the
/// run stores each report it in their own vocabulary and neither can trim what the
/// other would have refused.
///
/// A short read was short of at most one frame, so more behind the prefix than
/// `max_frame_bytes` plus a head is not the tail the scan saw: someone wrote past
/// the advisory lock while it ran. Nothing is decided on that, and it is reported
/// through `storage` rather than guessed at.
///
/// # Errors
///
/// Whatever `storage` makes of the device's refusal, or of the file having grown
/// past what a cut frame could leave.
pub(crate) fn cut_holds_acknowledged<R, E, H>(
    file: &mut R,
    offset: u64,
    previous: &Digest,
    max_frame_bytes: usize,
    storage: fn(std::io::Error) -> E,
    head_of: H,
) -> Result<bool, E>
where
    R: Read + Seek,
    H: Fn(&Digest, &[u8]) -> Option<Digest>,
{
    let start = offset.saturating_add(u64::try_from(LENGTH_BYTES).unwrap_or(u64::MAX));
    let len = file.seek(SeekFrom::End(0)).map_err(storage)?;
    let behind = len.saturating_sub(start);
    let ceiling = u64::try_from(max_frame_bytes.saturating_add(HEAD_BYTES)).unwrap_or(u64::MAX);
    if behind > ceiling {
        let grew = Err(storage(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the file grew while its tail was being resolved; reopen it",
        )));
        lgwks_std::trace::debug!(
            behind,
            ceiling,
            "cut_holds_acknowledged: more behind the prefix than a cut frame leaves"
        );
        return grew;
    }
    file.seek(SeekFrom::Start(start)).map_err(storage)?;
    let mut suffix = vec![0u8; usize::try_from(behind).unwrap_or(usize::MAX)];
    file.read_exact(&mut suffix).map_err(storage)?;
    Ok(holds_acknowledged_frame(
        &suffix,
        previous,
        max_frame_bytes,
        head_of,
    ))
}

/// The bytes of one whole frame: the payload and the head that follows it.
#[cfg(feature = "script")]
pub(crate) struct Raw {
    /// The payload bytes, which the chain head is computed over.
    pub(crate) payload: Vec<u8>,
    /// The chain head the frame recorded for itself.
    pub(crate) head: [u8; HEAD_BYTES],
}

/// An aligned copy of `payload` when it does not already sit where an archive can be
/// read from it, and `None` when it does.
///
/// A payload found by the tail search lies at whatever offset the bytes before it
/// happened to leave, and an archive read from a misaligned slice is refused rather
/// than decoded, which would make a frame the writer really framed look like noise.
/// The copy is made only for a misaligned candidate, so the search's many
/// candidates that start where the buffer does cost nothing extra.
#[cfg(feature = "script")]
pub(crate) fn misaligned_copy(payload: &[u8]) -> Option<lgwks_std::wire::AlignedVec> {
    const ARCHIVE_ALIGN: usize = 16;
    if payload.as_ptr().align_offset(ARCHIVE_ALIGN) == 0 {
        return None;
    }
    let mut copy = lgwks_std::wire::AlignedVec::with_capacity(payload.len());
    copy.extend_from_slice(payload);
    Some(copy)
}

/// Which frame a reader is at: its ordinal, where its length prefix begins, and the
/// head it chains from. The last two are what a reader needs to tell a cut append
/// from a frame whose length lies.
#[cfg(feature = "script")]
pub(crate) struct Cursor<'chain> {
    /// The frame's index, from zero.
    pub(crate) at: u64,
    /// The offset of the frame's length prefix.
    pub(crate) offset: u64,
    /// The head the frame chains from.
    pub(crate) previous: &'chain Digest,
}

#[cfg(feature = "script")]
impl<'chain> Cursor<'chain> {
    /// The cursor for frame `at`, which begins at `offset` and chains from `previous`.
    pub(crate) fn new(at: u64, offset: u64, previous: &'chain Digest) -> Self {
        Self {
            at,
            offset,
            previous,
        }
    }
}

/// Read the next whole frame, or `None` where the file stops holding one.
///
/// A partial length prefix is an append that never finished: it was never
/// anyone's answer, so the scan stops and the caller trims. A complete prefix
/// naming a length the writer never produces cannot be an interrupted append, and
/// is `corrupt`. A payload or head cut short under a complete prefix is either an
/// append that never finished or an acknowledged frame whose prefix was changed,
/// and only the stored head tells them apart ([`cut_holds_acknowledged`]):
/// the first is `None` and the second is `corrupt`, with the file untouched.
/// `head_of` is the store's head over the previous head and an archived payload,
/// `None` for a payload it cannot decode.
#[cfg(feature = "script")]
pub(crate) fn read_raw<R, E, H>(
    file: &mut R,
    at: &Cursor<'_>,
    max_frame_bytes: usize,
    storage: fn(std::io::Error) -> E,
    corrupt: impl FnOnce() -> E,
    head_of: H,
) -> Result<Option<Raw>, E>
where
    R: Read + Seek,
    H: Fn(&Digest, &[u8]) -> Option<Digest>,
{
    let mut prefix = [0u8; LENGTH_BYTES];
    let declared = match read_prefix(file, &mut prefix).map_err(storage)? {
        Prefix::Eof | Prefix::Torn => return Ok(None),
        Prefix::Full => declared_length(&prefix),
    };
    if !is_possible_length(declared, max_frame_bytes) {
        lgwks_std::trace::debug!(
            declared,
            "read_raw: the declared length is one the writer never produces"
        );
        return Err(corrupt());
    }
    let mut payload = vec![0u8; declared];
    let mut head = [0u8; HEAD_BYTES];
    for piece in [&mut payload[..], &mut head[..]] {
        if let Piece::Interrupted = read_piece(file, piece).map_err(storage)? {
            let lies = cut_holds_acknowledged(
                file,
                at.offset,
                at.previous,
                max_frame_bytes,
                storage,
                head_of,
            )?;
            return if lies { Err(corrupt()) } else { Ok(None) };
        }
    }
    Ok(Some(Raw { payload, head }))
}

/// Byte surgery on a framed file, which every store's tail tests share: where the
/// frames begin, what a prefix declares, and a file with one prefix changed.
#[cfg(test)]
pub(crate) mod probe {
    use super::{LENGTH_BYTES, framed_len};
    #[cfg(feature = "script")]
    use std::path::{Path, PathBuf};
    #[cfg(feature = "script")]
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Gives concurrent tests distinct scratch names.
    #[cfg(feature = "script")]
    static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A scratch file path unique to one test, removed when the test ends.
    ///
    /// Best effort on removal: a scratch file the system refuses to remove is
    /// litter, not a failed observation, so the error does not mask the test's own
    /// verdict. Only the run store and ledger tests (the `script` feature) use it.
    #[cfg(feature = "script")]
    pub(crate) struct Scratch(PathBuf);

    #[cfg(feature = "script")]
    impl Scratch {
        /// A fresh path under the temp directory, named for `name`.
        pub(crate) fn new(name: &str) -> Self {
            let unique = SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| since.as_nanos())
                .unwrap_or_default();
            Self(std::env::temp_dir().join(format!("lgwks-frame-{name}-{nanos}-{unique}")))
        }

        /// Where the file is.
        pub(crate) fn path(&self) -> &Path {
            &self.0
        }
    }

    #[cfg(feature = "script")]
    impl Drop for Scratch {
        fn drop(&mut self) {
            drop(std::fs::remove_file(&self.0));
        }
    }

    /// Where each frame of a valid file begins, walked from the prefixes after
    /// `header` bytes of file header.
    pub(crate) fn frame_starts(
        bytes: &[u8],
        header: usize,
    ) -> Result<Vec<usize>, Box<dyn std::error::Error>> {
        let mut starts = Vec::new();
        let mut at = header;
        while at < bytes.len() {
            starts.push(at);
            at = at.saturating_add(usize::try_from(framed_len(usize::try_from(declared_at(
                bytes, at,
            ))?))?);
        }
        Ok(starts)
    }

    /// `bytes` with the length prefix of the frame at `at` replaced.
    pub(crate) fn with_prefix(bytes: &[u8], at: usize, declared: u32) -> Vec<u8> {
        let mut out = bytes.to_vec();
        for (slot, byte) in out.iter_mut().skip(at).zip(declared.to_be_bytes()) {
            *slot = byte;
        }
        out
    }

    /// The declared payload length of the frame at `at`.
    pub(crate) fn declared_at(bytes: &[u8], at: usize) -> u32 {
        let mut prefix = [0u8; LENGTH_BYTES];
        for (slot, byte) in prefix.iter_mut().zip(bytes.iter().skip(at)) {
            *slot = *byte;
        }
        u32::from_be_bytes(prefix)
    }
}

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

    /// The head a store of plain chaining would store: a hash of the previous head
    /// and the payload, over the bytes as written.
    fn chained(previous: &Digest, payload: &[u8]) -> Option<Digest> {
        let mut hasher = lgwks_std::hash::Hasher::new();
        hasher.update(previous.as_bytes());
        hasher.update(payload);
        Some(hasher.finalize())
    }

    /// `count` chained frames laid out end to end, and the head each chained from.
    fn chain_of(payloads: &[&[u8]]) -> (Vec<u8>, Vec<Digest>) {
        let mut bytes = Vec::new();
        let mut previous = Digest::from_bytes([0u8; HEAD_BYTES]);
        let mut chained_from = Vec::new();
        for payload in payloads {
            let head = chained(&previous, payload).unwrap_or(previous);
            let length = writable_length(payload.len(), usize::MAX).unwrap_or(0);
            bytes.extend_from_slice(&encode(length, payload, &head));
            chained_from.push(previous);
            previous = head;
        }
        (bytes, chained_from)
    }

    const CEILING: usize = 64 * 1024;

    /// The cut frame under its true length authenticates, so the prefix lied; every
    /// shorter cut of the same bytes is a prefix of an append and holds nothing.
    #[test]
    fn the_cut_frame_authenticates_only_when_it_is_whole_behind_a_lying_prefix() {
        let payload: Vec<u8> = (0u8..100).collect();
        let (bytes, from) = chain_of(&[&payload]);
        let previous = from[0];
        let behind_prefix = &bytes[LENGTH_BYTES..];
        assert!(super::holds_acknowledged_frame(
            behind_prefix,
            &previous,
            CEILING,
            chained
        ));
        for cut in 0..behind_prefix.len() {
            assert!(
                !super::holds_acknowledged_frame(
                    &behind_prefix[..cut],
                    &previous,
                    CEILING,
                    chained
                ),
                "a frame cut {cut} bytes in, short of its head, is an interrupted append"
            );
        }
    }

    /// A frame cut off from its head by one or more bytes, with its payload whole,
    /// holds nothing: the head that would authenticate it is not there.
    #[test]
    fn a_whole_payload_with_a_short_head_authenticates_nothing() {
        let payload = vec![9u8; 64];
        let (bytes, from) = chain_of(&[&payload]);
        let behind_prefix = &bytes[LENGTH_BYTES..];
        for missing in 1..=HEAD_BYTES {
            let cut = behind_prefix.len() - missing;
            assert!(!super::holds_acknowledged_frame(
                &behind_prefix[..cut],
                &from[0],
                CEILING,
                chained
            ));
        }
    }

    /// With the cut frame itself damaged, a later whole frame still authenticates
    /// against the head stored just before it.
    #[test]
    fn a_later_frame_authenticates_when_the_cut_frame_cannot() {
        let (bytes, from) = chain_of(&[&[1u8; 40], &[2u8; 50], &[3u8; 60]]);
        let mut behind_prefix = bytes[LENGTH_BYTES..].to_vec();
        behind_prefix[5] ^= 0x80;
        assert!(super::holds_acknowledged_frame(
            &behind_prefix,
            &from[0],
            CEILING,
            chained
        ));
        // The same bytes with the later frames cut away leave nothing to find.
        let first_only = &behind_prefix[..40 + HEAD_BYTES];
        assert!(!super::holds_acknowledged_frame(
            first_only, &from[0], CEILING, chained
        ));
    }

    /// With the cut frame's own head damaged and exactly one frame behind it, that
    /// frame still chains from the head the cut frame's payload implies.
    #[test]
    fn a_frame_behind_a_damaged_head_chains_from_the_head_its_payload_implies() {
        let (bytes, from) = chain_of(&[&[1u8; 40], &[2u8; 50]]);
        let mut behind_prefix = bytes[LENGTH_BYTES..].to_vec();
        behind_prefix[40 + 7] ^= 0x01;
        assert!(super::holds_acknowledged_frame(
            &behind_prefix,
            &from[0],
            CEILING,
            chained
        ));
    }

    /// Bytes that never framed anything hold nothing, at the ceiling and empty, and
    /// the search answers in bounded time for the longest tail a short read can leave.
    #[test]
    fn noise_and_the_longest_possible_tail_hold_nothing() {
        let previous = Digest::from_bytes([5u8; HEAD_BYTES]);
        assert!(!super::holds_acknowledged_frame(
            &[],
            &previous,
            CEILING,
            chained
        ));
        let longest: Vec<u8> = (0..CEILING + HEAD_BYTES)
            .map(|index| u8::try_from(index % 251).unwrap_or(0))
            .collect();
        assert!(!super::holds_acknowledged_frame(
            &longest, &previous, CEILING, chained
        ));
        // A head function that refuses every payload can authenticate nothing.
        let (bytes, from) = chain_of(&[&[7u8; 30]]);
        assert!(!super::holds_acknowledged_frame(
            &bytes[LENGTH_BYTES..],
            &from[0],
            CEILING,
            |_previous, _payload| None
        ));
    }
}
