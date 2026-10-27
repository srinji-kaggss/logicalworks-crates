//! Streaming replay: the read-only file view and the Replay iterator.

#[cfg(feature = "wire")]
use super::EffectEvent;
#[cfg(feature = "wire")]
use super::resolve_ambiguous_tail;
use super::{BufReader, File, JournalError, JournalPosition, OpenOptions, Path, ReplayCursor};
#[cfg(feature = "wire")]
use super::{Continuation, verify_frame};
#[cfg(feature = "wire")]
use super::{
    Corruption, CorruptionKind, FramePiece, HEAD_BYTES, JournalLimitKind, LENGTH_BYTES,
    MAX_FRAME_BYTES, MAX_JOURNAL_EVENTS, SaturatingFrom, read_exact_classified, read_exact_or_eof,
};
#[cfg(feature = "wire")]
use super::{WireError, from_bytes};

/// A read-only window onto the journal's file, for the handle's own fence.///
/// It exists so the fence can ask how long the file is without asking the
/// storage owner, and asking the owner would mean waiting for the device
/// before deciding whether to append at all. It cannot write: it is opened
/// without write permission, so the owner stays the only writer even though
/// two descriptors exist.
pub(super) struct FileView {
    /// The read-only descriptor.
    file: File,
}

impl FileView {
    /// Open a read-only descriptor onto `path`.
    pub(super) fn read_only(path: &Path) -> Result<Self, JournalError> {
        Ok(Self {
            file: OpenOptions::new()
                .read(true)
                .open(path)
                .map_err(JournalError::Storage)?,
        })
    }

    /// The file's current byte length.
    pub(super) fn len(&self) -> Result<u64, JournalError> {
        self.file
            .metadata()
            .map_err(JournalError::Storage)
            .map(|meta| meta.len())
    }
}

/// A bounded, streaming replay of a journal file.
///
/// [`FileJournal::open`](crate::journal::FileJournal::open) must materialize the complete history, because the
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
/// [`FileJournal::checkpoint`](crate::journal::FileJournal::checkpoint), and [`FileJournal::recover`](crate::journal::FileJournal::recover) is the fold that
/// includes it. This file's own closing seal ends the stream, and a frame after it
/// is refused. So the stream is exactly [`FileJournal::events`](crate::journal::FileJournal::events) on every file of a
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
    pub(super) fn open(path: &Path) -> Result<Self, JournalError> {
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
    #[cfg(feature = "wire")]
    pub(super) fn read_one(&mut self) -> Option<Result<EffectEvent, JournalError>> {
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
    #[cfg(feature = "wire")]
    pub(super) fn read_frame(&mut self) -> Option<Result<Option<EffectEvent>, JournalError>> {
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
            .saturating_add(super::super::frame::framed_len(payload_len));
        Some(admitted)
    }

    /// Decide one whole frame the way the open scan decides it.
    #[cfg(feature = "wire")]
    pub(super) fn admit(
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
    #[cfg(feature = "wire")]
    pub(super) fn end_of_acknowledged_prefix(
        &mut self,
    ) -> Option<Result<Option<EffectEvent>, JournalError>> {
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

#[cfg(feature = "wire")]
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
