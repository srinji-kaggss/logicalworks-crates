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

impl FileJournal {
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

#[cfg(test)]
pub(super) mod tests;

// The scan, the seal step, streaming replay, the open and write halves of the
// handle, and its trait implementation, each in its own file. `Replay` is
// re-exported so its path stays `journal::Replay`.
mod adapter;
mod open;
mod replay;
mod scan;
mod seal;
mod write;

pub use replay::Replay;

// The pieces the halves of the handle share across these files.
use adapter::ReplayCursor;
use replay::FileView;
use scan::{ScanStop, Scanned, read_exact_classified, resolve_ambiguous_tail, scan, verify_frame};
#[cfg(test)]
use seal::write_successor;
use seal::{OpenKind, ladder_hold, seal_on_owner, successor_is_complete};
