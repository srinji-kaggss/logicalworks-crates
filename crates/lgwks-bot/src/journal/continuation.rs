//! Continue-as-new for the effect journal: a sealed checkpoint, and the
//! successor opened from it.
//!
//! # The shape of the fix
//!
//! `MAX_JOURNAL_EVENTS` and `MAX_JOURNAL_BYTES` are hard ceilings, and an
//! unattended bot that reaches one stops taking effects. It stops *safely* —
//! nothing is truncated, nothing unresolved is dropped — and it still stops.
//! This module is the way past the ceiling that keeps the two properties the
//! journal already has:
//!
//! - the journal stays **append-only**, so a chain head still commits to every
//!   fact behind it and a tampered frame is still a refusal rather than a trim;
//! - **unresolved work is never dropped**, so an attempt whose outcome is unknown
//!   stays unknown in the successor until evidence settles it.
//!
//! The two production references agree on the mechanism. Temporal's
//! continue-as-new closes a workflow run whose history nears its limit and
//! starts a fresh run carrying only the state it needs; the old history is
//! closed, not rewritten. Raft and etcd take a snapshot at an index, keep the
//! entries after it, and drop the entries before it only once the snapshot is
//! durable. Both answer the same question — *where does a reopen start?* — and
//! both make the answer decidable from the bytes on the disk rather than from a
//! pointer somebody has to keep in step.
//!
//! # One frame grammar, one writer thread
//!
//! The checkpoint is a **second record type in the same chain**, framed by
//! `crate::journal::frame` with the same chain function every effect event
//! uses. It is not a second grammar and not a second writer: the seal runs on
//! the journal's existing storage-owner thread, so the length fence, the write
//! and the flush that cover it are the same ordered step every append already
//! takes (INV-BOT-50, INV-BOT-130).
//!
//! A reader tells the two record types apart by a magic prefix on the payload
//! (`CHECKPOINT_MAGIC`), never by trying one decode and falling back to the
//! other: a payload that does not carry the magic is an event, and a payload
//! that carries it is a checkpoint. See [`Continuation::is_payload`].
//!
//! # What a crash leaves
//!
//! One durable fact decides, and it is the **successor's first frame**: the seal
//! frame's bytes are written into the predecessor *and* into the successor, byte
//! for byte, so the successor's chain starts at the predecessor's tail and its
//! first frame authenticates against it.
//!
//! | crash point | what a reopen finds | who is authoritative |
//! |---|---|---|
//! | before the predecessor's seal frame is whole | a torn tail, repaired | the predecessor |
//! | after the predecessor's seal, before the successor exists | a seal frame whose successor is absent | the predecessor, which resumes appending |
//! | after the successor's first frame is whole | the seal is in force | the successor |
//!
//! Exactly one journal is authoritative in every row, and the decision is made
//! from the bytes: a journal holding a seal frame whose successor is complete
//! refuses itself as [`JournalError::Superseded`] and names where the
//! authoritative one is. A seal whose successor is absent or torn is *not* in
//! force — the continuation never committed — so the predecessor resumes and the
//! interrupted continuation is completed rather than abandoned. Nothing is ever
//! rewritten: the predecessor's seal frame stays in its history either way.
//!
//! # What the checkpoint carries, and what it compacts
//!
//! Carried, in full identity, and never dropped:
//!
//! - **every unresolved attempt** — a `DispatchPrepared` with no outcome, or an
//!   `IntentAdmitted` with no preparation — with the rung it reached. A key that
//!   is `OutcomeUnknown` in the predecessor is `OutcomeUnknown` in the
//!   successor, and the ladder refuses a fresh `IntentAdmitted` for it, so it
//!   cannot be resent;
//! - **the latest attempt of every action**, with its key, its resolved
//!   `AttemptStatus` including `VerificationFailed`, and the verification digest
//!   a `Verified` or `VerificationFailed` status is qualified by;
//! - **the predecessor's chain head** the successor's own chain starts from.
//!
//! Compacted, and the compaction is what makes reopen bounded: an attempt *older*
//! than the one its action reached is refused rather than remembered.
//! `AttemptId` is monotonic per `ActionId` and never reused, so "at or below the
//! latest attempt of that action" *is* "the predecessor already walked it", and
//! the refusal is exactly the ladder's own answer. Carrying one record per action
//! instead of one per attempt is what makes a successor's reopen cost and RSS
//! flat in the number of attempts the *predecessor* ever made — measured in
//! `examples/journal_continuation.rs`.

use core::fmt;
use std::path::PathBuf;

use lgwks_std::wire::WireError;

use crate::effect::{ActionId, AttemptId, EffectKey};

use super::frame::SaturatingFrom;
use super::{
    AttemptStatus, EventKind, JournalError, JournalLimitKind, JournalPosition, Verification,
};

/// The prefix that marks a frame payload as a checkpoint rather than an event.
///
/// Padded to a multiple of eight so the archived record behind it starts on the
/// alignment a decoder expects, and declared rather than written inline because
/// the reader's first question about every frame is this one.
///
/// The padding is not decoration: these bytes are compared, so a magic that ran
/// into the archived record would make a checkpoint's own first field part of the
/// discriminator.
pub(crate) const CHECKPOINT_MAGIC: &[u8; 32] = b"lgwks.journal.checkpoint.v1\0\0\0\0\0";

/// Eight tenths of either ceiling: the declared continuation watermark.
///
/// A constant rather than a parameter because the point of it is that the
/// decision is made *before* anything depends on it. One fifth of each ceiling is
/// left as settlement headroom, so an attempt already handed to the outside world
/// can always record its outcome: the ladder's third rung is admitted at the
/// watermark and refused only at the hard cap, and a continuation happens before
/// either.
pub const CONTINUATION_WATERMARK_NUMERATOR: u64 = 8;

/// The denominator of [`CONTINUATION_WATERMARK_NUMERATOR`].
pub const CONTINUATION_WATERMARK_DENOMINATOR: u64 = 10;

/// The most unresolved attempts one checkpoint may carry.
///
/// Checked before the checkpoint is archived, so an over-large carry is a typed
/// refusal that leaves both journals untouched rather than a frame the writer
/// cannot produce. The archived bytes are bounded independently by the shared
/// grammar's own frame ceiling, and whichever bound trips first is the one
/// reported.
pub const MAX_CHECKPOINT_UNRESOLVED: usize = 128;

/// The most actions one checkpoint may fold.
///
/// One record per *action*, not per attempt — see the module's note on what the
/// checkpoint compacts — so this is bounded by the shape of a bot rather than by
/// how long it has run.
///
/// Sized so the widest checkpoint both counts admit fits the one frame it is
/// sealed in: a settled record archives to at most 240 bytes and an unresolved one
/// to 160, so this many settled records beside a full [`MAX_CHECKPOINT_UNRESOLVED`]
/// carry is 58,976 bytes of a 65,536-byte frame. It was 256, which archives to
/// 90,208 bytes: a count no checkpoint could reach, refused by the frame as a
/// storage fault instead of by the count it names.
pub const MAX_CHECKPOINT_SETTLED: usize = 160;

/// The suffix that turns one journal's file name into the directory its
/// successors live in.
const GENERATION_DIRECTORY_SUFFIX: &str = ".cont";

/// The fixed width of a generation's file name inside that directory.
///
/// Fixed rather than appended, and that is the whole reason the naming uses a
/// directory at all: a chain that grew its name by one token per generation passes
/// 255 characters after fifty continuations, so a run meant to last weeks would
/// stop for a reason that has nothing to do with its journal. A fixed-width ordinal
/// inside a per-journal directory costs the same six characters at generation one
/// and at generation 999,999.
const GENERATION_DIGITS: usize = 6;

/// The highest generation a name can hold.
///
/// The width above, so it is a stated bound rather than a wrap: a chain that
/// reaches it is refused by the open rather than pointed at a name that already
/// exists, because "the successor is this file" is not an answer a caller can act on.
const MAX_GENERATION_NAME: u64 = 999_999;

/// How far along a continuation may be driven, for a harness that must kill the
/// process exactly there.
///
/// The four points are the four places bytes become durable in order, and a
/// crash between any two of them leaves a different file to reopen. A
/// continuation armed to stop at one of them refuses *after* the named step has
/// completed and before the next byte moves, so a process killed on that refusal
/// is killed exactly at the boundary rather than somewhere near it.
///
/// Public rather than `cfg(test)` for the reason
/// [`crate::journal::FileJournal::open_with_stalled_storage`] is: "what does this
/// bot's record look like after the disk dies mid-continuation" is a question
/// about a real process, and a probe that only exists inside the crate's own test
/// binary cannot answer it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SealPause {
    /// The predecessor's seal frame is written and not yet flushed, and the
    /// successor does not exist.
    AfterSealWrite,
    /// The successor's first frame is written and not yet flushed.
    AfterSuccessorWrite,
    /// The successor's first frame is flushed and its directory entry is not.
    AfterSuccessorSync,
    /// The successor's directory entry is flushed and the predecessor's seal is
    /// not yet flushed.
    AfterDirectorySync,
}

impl SealPause {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AfterSealWrite => "after_seal_write",
            Self::AfterSuccessorWrite => "after_successor_write",
            Self::AfterSuccessorSync => "after_successor_sync",
            Self::AfterDirectorySync => "after_directory_sync",
        }
    }

    /// Every boundary, in the order the continuation reaches them.
    ///
    /// A crash harness sweeps this rather than spelling four values by hand, so a
    /// boundary added to this enum is swept the day it exists.
    #[must_use]
    pub const fn all() -> [Self; 4] {
        [
            Self::AfterSealWrite,
            Self::AfterSuccessorWrite,
            Self::AfterSuccessorSync,
            Self::AfterDirectorySync,
        ]
    }
}

impl fmt::Display for SealPause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The watermark count for a ceiling of `limit` events or bytes.
///
/// Integer arithmetic in the declared order, so the watermark is the same number
/// on every host: `limit * 8 / 10`, with the multiplication first because it is
/// exact for every ceiling this crate declares. The `checked_div` arm is
/// unreachable for the constant denominator above; it is here rather than an
/// `unwrap` because the crate forbids both.
const fn watermark_of(limit: u64) -> u64 {
    match limit
        .saturating_mul(CONTINUATION_WATERMARK_NUMERATOR)
        .checked_div(CONTINUATION_WATERMARK_DENOMINATOR)
    {
        Some(at) => at,
        None => limit,
    }
}

/// Where one journal continues.
///
/// Declared rather than derived from the file, so the trigger is a policy a host
/// can state, read back and measure against rather than a constant buried in a
/// comparison. [`ContinuationPolicy::declared`] is the shipped default — eight
/// tenths of each ceiling — and it is the one an unattended run should use; the
/// constructor exists because a host with a shorter retention budget than this
/// crate's defaults, and a simulation that has to reach a hundred continuations
/// inside a test's wall clock, both need to move the trigger rather than fake the
/// ceilings.
///
/// Moving it is always *earlier*. A policy past a ceiling would ask a journal to
/// continue when it already cannot admit another event, so the constructor
/// refuses rather than clamping: a silently clamped trigger is indistinguishable
/// from the one the caller asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContinuationPolicy {
    /// The committed event count at which a continuation is due.
    events_at: u64,
    /// The acknowledged byte length at which a continuation is due.
    bytes_at: u64,
}

impl ContinuationPolicy {
    /// The shipped policy: eight tenths of each ceiling, which leaves the
    /// remaining fifth as the settlement headroom #143 asked for.
    #[must_use]
    pub const fn declared() -> Self {
        Self {
            events_at: watermark_of(SHIPPED_EVENT_CEILING),
            bytes_at: watermark_of(SHIPPED_BYTE_CEILING),
        }
    }

    /// A policy with explicit trigger points.
    ///
    /// # Errors
    ///
    /// [`JournalError::CapacityExceeded`] when either point is at or past its
    /// ceiling, because such a policy would ask for a continuation the journal
    /// could not afford to perform.
    pub fn new(events_at: u64, bytes_at: u64) -> Result<Self, JournalError> {
        if events_at >= SHIPPED_EVENT_CEILING || bytes_at >= SHIPPED_BYTE_CEILING {
            let requested = events_at.max(bytes_at);
            let refusal = Err(JournalError::CapacityExceeded {
                resource: JournalLimitKind::ContinuationWatermark,
                limit: SHIPPED_EVENT_CEILING,
                requested,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "ContinuationPolicy::new: returning an error to the caller");
            return refusal;
        }
        Ok(Self {
            events_at,
            bytes_at,
        })
    }

    /// The committed event count at which a continuation is due.
    #[must_use]
    pub const fn events_at(&self) -> u64 {
        self.events_at
    }

    /// The acknowledged byte length at which a continuation is due.
    #[must_use]
    pub const fn bytes_at(&self) -> u64 {
        self.bytes_at
    }
}

impl Default for ContinuationPolicy {
    fn default() -> Self {
        Self::declared()
    }
}

/// The event ceiling a policy is measured against.
const SHIPPED_EVENT_CEILING: u64 = 100_000;

/// The byte ceiling a policy is measured against.
const SHIPPED_BYTE_CEILING: u64 = 64 * 1024 * 1024;

/// How much of each ceiling a journal has used, and whether it continues.
///
/// The question a caller asks before an append, and the answer is a measurement
/// rather than a guess: it is read from the journal's own committed count and
/// acknowledged byte length, both of which the append fence already keeps exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContinuationWatermark {
    /// Whether this journal continues at all.
    continuing: bool,
    /// Committed events and the ceiling they are measured against.
    events: u64,
    /// The event ceiling this journal was opened against.
    events_limit: u64,
    /// Committed bytes and the ceiling they are measured against.
    bytes: u64,
    /// The byte ceiling this journal was opened against.
    bytes_limit: u64,
    /// The event count at which this journal's watermark falls.
    events_at: u64,
    /// The byte length at which this journal's watermark falls.
    bytes_at: u64,
}

impl ContinuationWatermark {
    /// A watermark for a journal that does not continue.
    ///
    /// The answer every adapter but the continuing file journal gives. It is a
    /// real measurement of a real journal with `continuing` false rather than a
    /// zeroed placeholder: "this journal will not continue" is the answer, and a
    /// sentinel would be a measurement nobody took.
    #[must_use]
    pub const fn inert(events: u64, events_limit: u64, bytes: u64, bytes_limit: u64) -> Self {
        Self {
            continuing: false,
            events,
            events_limit,
            bytes,
            bytes_limit,
            events_at: 0,
            bytes_at: 0,
        }
    }

    /// A watermark for a journal that continues at its declared policy's two
    /// trigger points.
    #[must_use]
    pub const fn measured(
        events: u64,
        events_limit: u64,
        bytes: u64,
        bytes_limit: u64,
        policy: ContinuationPolicy,
    ) -> Self {
        Self {
            continuing: true,
            events,
            events_limit,
            bytes,
            bytes_limit,
            events_at: policy.events_at,
            bytes_at: policy.bytes_at,
        }
    }

    /// Whether this journal continues at the watermark rather than refusing.
    #[must_use]
    pub const fn continues(&self) -> bool {
        self.continuing
    }

    /// Whether a continuation is due now.
    ///
    /// `false` for a journal that does not continue, which is what leaves every
    /// non-continuing adapter's behaviour exactly as it was: the append after
    /// this question is the append that always happened.
    #[must_use]
    pub const fn is_due(&self) -> bool {
        self.continuing && (self.events >= self.events_at || self.bytes >= self.bytes_at)
    }

    /// Committed events and the ceiling they are measured against.
    #[must_use]
    pub const fn events(&self) -> (u64, u64) {
        (self.events, self.events_limit)
    }

    /// Committed bytes and the ceiling they are measured against.
    #[must_use]
    pub const fn bytes(&self) -> (u64, u64) {
        (self.bytes, self.bytes_limit)
    }

    /// The event count at which the watermark falls.
    #[must_use]
    pub const fn events_watermark(&self) -> u64 {
        self.events_at
    }

    /// The byte length at which the watermark falls.
    #[must_use]
    pub const fn bytes_watermark(&self) -> u64 {
        self.bytes_at
    }

    /// Events still admissible before the hard cap, which is the settlement
    /// headroom a continuation leaves behind.
    #[must_use]
    pub const fn event_headroom(&self) -> u64 {
        self.events_limit.saturating_sub(self.events)
    }

    /// Bytes still admissible before the hard cap.
    #[must_use]
    pub const fn byte_headroom(&self) -> u64 {
        self.bytes_limit.saturating_sub(self.bytes)
    }
}

/// One action's folded state at a continuation.
///
/// The latest attempt the predecessor walked, and what it resolved to. Every
/// attempt *older* than this one for the same action is refused by the successor
/// rather than carried, which is what keeps the carry bounded by actions rather
/// than by attempts; see the module's note on what the checkpoint compacts.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    lgwks_std::wire::Archive,
    lgwks_std::wire::Serialize,
    lgwks_std::wire::Deserialize,
)]
#[rkyv(crate = lgwks_std::wire::rkyv, compare(PartialEq), derive(Debug))]
pub struct SettledAttempt {
    /// The latest attempt the predecessor walked for its action, whose action and
    /// attempt number are read out of it rather than stored a second time: two
    /// copies of one identity in a durable record are two answers a crafted
    /// checkpoint could make disagree.
    key: EffectKey,
    /// The rung the predecessor's ladder had reached for it.
    rung: u8,
    /// What the predecessor's fold resolved it to.
    status: u8,
    /// The verification the status is qualified by, when one was evaluated.
    verification: Option<Verification>,
}

impl SettledAttempt {
    /// Fold one action's latest attempt.
    #[must_use]
    pub fn new(
        key: EffectKey,
        rung: EventKind,
        status: AttemptStatus,
        verification: Option<Verification>,
    ) -> Self {
        Self {
            key,
            rung: rung_index(rung),
            status: status_index(status),
            verification,
        }
    }

    /// The action this record is about.
    #[must_use]
    pub const fn action(&self) -> ActionId {
        self.key.action()
    }

    /// The latest attempt the predecessor walked for that action.
    #[must_use]
    pub const fn attempt(&self) -> AttemptId {
        self.key.attempt()
    }

    /// That attempt's exact identity.
    #[must_use]
    pub const fn key(&self) -> EffectKey {
        self.key
    }

    /// The rung the ladder had reached, or `None` for a rung this build does not
    /// name.
    ///
    /// The decode is fallible because the record is durable: a checkpoint written
    /// by a build that named a rung this one does not is not silently read as the
    /// foot of the ladder, which would make a settled attempt replayable.
    #[must_use]
    pub const fn rung(&self) -> Option<EventKind> {
        rung_of(self.rung)
    }

    /// What the predecessor's fold resolved it to, or `None` for a status this
    /// build does not name.
    #[must_use]
    pub const fn status(&self) -> Option<AttemptStatus> {
        status_of(self.status)
    }

    /// The verification the status is qualified by.
    #[must_use]
    pub const fn verification(&self) -> Option<Verification> {
        self.verification
    }

    /// Whether `key` is this action's latest attempt or one the predecessor walked
    /// before it.
    ///
    /// The comparison is on `attempt` because `AttemptId` is monotonic per action
    /// and never reused, which is what makes a folded record answer "was this
    /// already walked?" without carrying every attempt that ever was.
    #[must_use]
    pub fn already_walked(&self, key: EffectKey) -> bool {
        key.action() == self.key.action() && key.attempt() <= self.key.attempt()
    }
}

/// One attempt the predecessor handed over and nothing has settled.
///
/// Carried in full, because dropping it would be the one thing this module must
/// never do: an attempt whose bytes may have reached the outside world and whose
/// outcome nobody established has to be unknown in the successor exactly as it
/// was in the predecessor.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    lgwks_std::wire::Archive,
    lgwks_std::wire::Serialize,
    lgwks_std::wire::Deserialize,
)]
#[rkyv(crate = lgwks_std::wire::rkyv, compare(PartialEq), derive(Debug))]
pub struct UnresolvedAttempt {
    /// The attempt.
    key: EffectKey,
    /// The rung it reached.
    rung: u8,
}

impl UnresolvedAttempt {
    /// Carry one unresolved attempt at the rung it reached.
    #[must_use]
    pub const fn new(key: EffectKey, rung: EventKind) -> Self {
        Self {
            key,
            rung: rung_index(rung),
        }
    }

    /// The attempt.
    #[must_use]
    pub const fn key(&self) -> EffectKey {
        self.key
    }

    /// The rung it reached, or `None` for a rung this build does not name.
    #[must_use]
    pub const fn rung(&self) -> Option<EventKind> {
        rung_of(self.rung)
    }

    /// What a recovery fold reports for this attempt.
    ///
    /// `None` for a rung this build does not name, and the caller refuses the
    /// checkpoint rather than reading an unknown rung as "nothing happened", which
    /// would turn an unresolved attempt into an absent one.
    #[must_use]
    pub const fn recovered_status(&self) -> Option<AttemptStatus> {
        match rung_of(self.rung) {
            Some(EventKind::IntentAdmitted) => Some(AttemptStatus::Prepared),
            Some(EventKind::DispatchPrepared) => Some(AttemptStatus::OutcomeUnknown),
            _ => None,
        }
    }
}

/// The archived index of a rung, for a record this module writes durably.
///
/// A byte rather than the enum's archived form, and the choice is the lint
/// contract's: an archived `pub enum` emits an exported resolver this workspace
/// cannot document (`journal::wire_form` records why), and a byte fails closed
/// instead — a rung index a later build does not name reads as `None`, never as
/// the foot of the ladder. The match is exhaustive inside this crate, and a rung
/// added to the enum therefore breaks the build here rather than reaching a
/// writer that would encode it as something no reader knows.
const fn rung_index(rung: EventKind) -> u8 {
    match rung {
        EventKind::IntentAdmitted => 0,
        EventKind::DispatchPrepared => 1,
        EventKind::OutcomeObserved => 2,
        EventKind::Verified => 3,
    }
}

/// The archived index of a status, for the same reason as [`rung_index`].
const fn status_index(status: AttemptStatus) -> u8 {
    match status {
        AttemptStatus::Prepared => 0,
        AttemptStatus::OutcomeUnknown => 1,
        AttemptStatus::Applied => 2,
        AttemptStatus::NotApplied => 3,
        AttemptStatus::Verified => 4,
        AttemptStatus::VerificationFailed => 5,
    }
}

/// The rung an archived index names, or `None` for one this build does not name.
const fn rung_of(index: u8) -> Option<EventKind> {
    match index {
        0 => Some(EventKind::IntentAdmitted),
        1 => Some(EventKind::DispatchPrepared),
        2 => Some(EventKind::OutcomeObserved),
        3 => Some(EventKind::Verified),
        _ => None,
    }
}

/// The status an archived index names, or `None` for one this build does not name.
const fn status_of(index: u8) -> Option<AttemptStatus> {
    match index {
        0 => Some(AttemptStatus::Prepared),
        1 => Some(AttemptStatus::OutcomeUnknown),
        2 => Some(AttemptStatus::Applied),
        3 => Some(AttemptStatus::NotApplied),
        4 => Some(AttemptStatus::Verified),
        5 => Some(AttemptStatus::VerificationFailed),
        _ => None,
    }
}

/// What a sealed journal hands to its successor.
///
/// The record a checkpoint writes: the predecessor's chain head, where the
/// successor's own chain starts, every unresolved attempt, and the folded state
/// of every action. Archived through [`lgwks_std::wire`] like every other estate
/// record that crosses a byte boundary, so a change to its shape changes every
/// head computed after it — which is what makes a checkpoint an audit record
/// rather than a hint.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    lgwks_std::wire::Archive,
    lgwks_std::wire::Serialize,
    lgwks_std::wire::Deserialize,
)]
#[rkyv(crate = lgwks_std::wire::rkyv, compare(PartialEq), derive(Debug))]
pub struct Continuation {
    /// Which generation this successor is. One is the first successor of a journal
    /// that never continued.
    generation: u64,
    /// The predecessor's chain head, and the position the successor's first frame
    /// chains from.
    predecessor: JournalPosition,
    /// The latest attempt of every action the predecessor walked.
    settled: Vec<SettledAttempt>,
    /// Every attempt the predecessor handed over and nothing settled.
    unresolved: Vec<UnresolvedAttempt>,
}

impl Continuation {
    /// Build a checkpoint, refusing a carry this journal cannot frame.
    ///
    /// The bounds are charged here, before anything is archived or written, so a
    /// refused continuation leaves both files byte-identical.
    ///
    /// There is deliberately **no successor path** in this record. The path is
    /// [`successor_path`] of the file the reader is already holding, which is why
    /// the seal frame can be byte-identical in the predecessor and the successor:
    /// a record that named where it was going would have to name a different place
    /// in each of the two files, and a checkpoint whose meaning depends on which
    /// file it was read from is a second authority rather than a fact.
    ///
    /// # Errors
    ///
    /// [`JournalError::CapacityExceeded`] when either carried list is past its
    /// declared count.
    pub fn new(
        generation: u64,
        predecessor: JournalPosition,
        settled: Vec<SettledAttempt>,
        unresolved: Vec<UnresolvedAttempt>,
    ) -> Result<Self, JournalError> {
        if settled.len() > MAX_CHECKPOINT_SETTLED {
            let requested = u64::saturating_from(settled.len());
            let refusal = Err(JournalError::CapacityExceeded {
                resource: JournalLimitKind::CheckpointActions,
                limit: u64::saturating_from(MAX_CHECKPOINT_SETTLED),
                requested,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "Continuation::new: returning an error to the caller");
            return refusal;
        }
        if unresolved.len() > MAX_CHECKPOINT_UNRESOLVED {
            let requested = u64::saturating_from(unresolved.len());
            let refusal = Err(JournalError::CapacityExceeded {
                resource: JournalLimitKind::CheckpointUnresolved,
                limit: u64::saturating_from(MAX_CHECKPOINT_UNRESOLVED),
                requested,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "Continuation::new: returning an error to the caller");
            return refusal;
        }
        Ok(Self {
            generation,
            predecessor,
            settled,
            unresolved,
        })
    }

    /// Which generation this successor is.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// The predecessor's chain head, and the position this successor continues
    /// from.
    #[must_use]
    pub const fn predecessor(&self) -> JournalPosition {
        self.predecessor
    }

    /// The folded state of every action the predecessor walked.
    #[must_use]
    pub fn settled(&self) -> &[SettledAttempt] {
        &self.settled
    }

    /// Every attempt the predecessor handed over and nothing settled.
    #[must_use]
    pub fn unresolved(&self) -> &[UnresolvedAttempt] {
        &self.unresolved
    }

    /// The fold for `action`, when this checkpoint carries one.
    ///
    /// Linear rather than indexed because the list is bounded by actions and is
    /// read once per carried key while an open seeds it, not once per append.
    #[must_use]
    pub fn settled_for(&self, action: ActionId) -> Option<&SettledAttempt> {
        self.settled.iter().find(|entry| entry.action() == action)
    }

    /// The frame payload: the magic, then the archived record.
    ///
    /// # Errors
    ///
    /// [`JournalError::Encoding`] if the archive cannot be allocated.
    pub fn to_payload(&self) -> Result<Vec<u8>, JournalError> {
        let archived =
            lgwks_std::wire::to_bytes::<WireError>(self).map_err(JournalError::Encoding)?;
        let mut payload = Vec::with_capacity(CHECKPOINT_MAGIC.len().saturating_add(archived.len()));
        payload.extend_from_slice(CHECKPOINT_MAGIC);
        payload.extend_from_slice(archived.as_ref());
        Ok(payload)
    }

    /// Whether a frame payload is a checkpoint rather than an event.
    #[must_use]
    pub fn is_payload(payload: &[u8]) -> bool {
        payload.starts_with(CHECKPOINT_MAGIC)
    }

    /// Read a frame payload this journal's writer produced.
    ///
    /// `None` when the payload does not carry `CHECKPOINT_MAGIC`, so it is an
    /// event rather than a checkpoint: the magic is asked once, here, and a
    /// caller cannot decode a payload it did not first classify.
    ///
    /// # Errors
    ///
    /// `Some(Err(JournalError::Encoding))` when the bytes behind the magic are
    /// not the archived record this build writes. A checkpoint is written whole
    /// or not at all, so this is a refusal rather than a truncated read.
    pub fn from_payload(payload: &[u8]) -> Option<Result<Self, JournalError>> {
        let archived = payload.strip_prefix(CHECKPOINT_MAGIC.as_slice())?;
        match lgwks_std::wire::from_bytes::<Self, WireError>(archived) {
            Ok(checkpoint) => Some(Ok(checkpoint)),
            Err(cause) => {
                let refusal = Err(JournalError::Encoding(cause));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "Continuation::from_payload: returning an error to the caller");
                Some(refusal)
            }
        }
    }
}

/// Where one journal's continuation lives.
///
/// A successor path is a *property of the file*, not of the run that happens to be
/// holding it: a restarting controller that only has the original path has to be
/// able to find the live journal by following it, and a name derived from anything
/// the process was doing would make that a guess.
///
/// So the rule is two fixed shapes and no counter anybody keeps: a base journal
/// continues into `<name>.cont/000001`, and a generation continues into the next
/// six-digit name beside it. Two tenants sharing one directory therefore keep two
/// directories apart, and a chain of any length keeps names of the same width.
#[must_use]
pub fn successor_path(path: &std::path::Path) -> std::path::PathBuf {
    // A path with a file name always has a parent (`""` for a bare name), so the
    // one shape that has neither is a root or an empty path, which names no
    // journal and continues into itself.
    let (Some(name), Some(parent)) = (path.file_name(), path.parent()) else {
        return path.to_path_buf();
    };
    if let Some(number) = name_number(name) {
        let next = number.saturating_add(1).min(MAX_GENERATION_NAME);
        return parent.join(format!("{next:0GENERATION_DIGITS$}"));
    }
    let mut directory = path.as_os_str().to_os_string();
    directory.push(GENERATION_DIRECTORY_SUFFIX);
    PathBuf::from(directory).join(format!("{:0GENERATION_DIGITS$}", 1))
}

/// The journal generation a path names, counted from the base journal.
///
/// One for a base journal. Derived from the name rather than stored, so there is no
/// second place a generation is written down and nothing for two continuations to
/// disagree about.
#[must_use]
pub fn generation_of(path: &std::path::Path) -> u64 {
    // The base journal is generation one and its first successor, `000001`, is
    // generation two: the name counts continuations, the generation counts files.
    // Read from the name alone, `000001` would be generation one, and its own
    // successor's checkpoint would claim the generation it already holds.
    match path.file_name().and_then(name_number) {
        Some(number) if is_generation_path(path) => number.saturating_add(1),
        Some(_) | None => 1,
    }
}

/// The number a generation name spells, or `None` for any other name.
///
/// A fold rather than a parse: [`is_generation_name`] admitted exactly six ASCII
/// digits, so there is no failure left for a parse to report.
fn name_number(name: &std::ffi::OsStr) -> Option<u64> {
    if !is_generation_name(name) {
        return None;
    }
    let number = name.to_string_lossy().bytes().fold(0_u64, |number, digit| {
        number
            .saturating_mul(10)
            .saturating_add(u64::from(digit.saturating_sub(b'0')))
    });
    Some(number)
}

/// Whether a name is one this scheme generates.
///
/// Six ASCII digits, exactly. A base journal whose own file name happens to look
/// like a generation is refused by the continuing constructors rather than
/// tolerated, because a name that can be read two ways is a name whose successor
/// cannot be derived.
fn is_generation_name(name: &std::ffi::OsStr) -> bool {
    let text = name.to_string_lossy();
    text.len() == GENERATION_DIGITS && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// Whether a path is one this scheme generated rather than a base journal.
///
/// A generation is a six-digit name **inside a `.cont` directory**, and both halves
/// are load-bearing: the directory is what a base journal cannot already be, so a
/// base named `000123` beside its journal's files is still a base and is refused,
/// while the successor `bot.jrnl.cont/000001` is this scheme's own.
#[must_use]
pub fn is_generation_path(path: &std::path::Path) -> bool {
    let named = path.file_name().is_some_and(is_generation_name);
    let in_directory = path
        .parent()
        .and_then(std::path::Path::file_name)
        .is_some_and(|name| {
            name.to_string_lossy()
                .ends_with(GENERATION_DIRECTORY_SUFFIX)
        });
    named && in_directory
}

/// Refuse a base journal whose name could be read as a generation.
///
/// The alternative is a chain that quietly continues into a neighbouring number,
/// and "which file is authoritative?" must have exactly one answer.
pub(crate) fn refuse_ambiguous_base(path: &std::path::Path) -> Result<(), JournalError> {
    let Some(number) = path.file_name().and_then(name_number) else {
        return Ok(());
    };
    if is_generation_path(path) {
        return Ok(());
    }
    let refusal = Err(JournalError::CapacityExceeded {
        resource: JournalLimitKind::Generation,
        limit: MAX_GENERATION_NAME,
        requested: number,
    });
    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "refuse_ambiguous_base: this name could be read as a generation");
    refusal
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::{
        ActionDigest, EffectIdentity, EnvironmentEpoch, EnvironmentId, FlowRevision, RunId,
    };
    use crate::journal::EffectEvidence;
    use lgwks_std::hash::blake3;
    use std::path::Path;

    /// A test that needs a parsed value returns `Result` and propagates, because
    /// the crate forbids a panicking path anywhere, tests included.
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const RUN: &str = "0102030405060708090a0b0c0d0e0f10";
    const ACTION: &str = "1112131415161718191a1b1c1d1e1f20";
    const ENV: &str = "2122232425262728292a2b2c2d2e2f30";
    const FLOW_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    const DIGEST_HEX: &str = "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeffe0e1e2e3e4e5e6e7e8e9eaebecedeeef";

    /// A key for attempt `attempt` of the shared action.
    fn key(attempt: &str) -> Result<EffectKey, Box<dyn std::error::Error>> {
        let run = RunId::from_hex(RUN)?;
        let environment = EnvironmentId::from_hex(ENV)?;
        let flow = FlowRevision::from_tagged("blake3_256", FLOW_HEX)?;
        let action = ActionId::from_hex(ACTION)?;
        let attempt = AttemptId::from_decimal(attempt)?;
        let digest = ActionDigest::from_tagged("blake3_256", DIGEST_HEX)?;
        let epoch = EnvironmentEpoch::from_decimal("1")?;
        Ok(EffectIdentity::new(run, environment, flow).key(action, attempt, digest, epoch))
    }

    /// The watermark of the shipped ceilings, which is what the headroom claim in
    /// the module's docs is about.
    #[test]
    fn the_watermark_leaves_a_fifth_of_each_ceiling() {
        assert_eq!(watermark_of(100), 80);
        assert_eq!(watermark_of(100_000), 80_000);
        assert_eq!(watermark_of(64 * 1024 * 1024), 53_687_091);
        assert_eq!(watermark_of(0), 0);
        assert_eq!(watermark_of(u64::MAX), watermark_of(u64::MAX));
    }

    #[test]
    fn a_journal_that_does_not_continue_is_never_due() -> TestResult {
        let mut watermark = ContinuationWatermark::inert(1, 2, 1, 2);
        assert!(!watermark.continues());
        assert!(
            !watermark.is_due(),
            "an inert journal is never due, at any size"
        );
        watermark =
            ContinuationWatermark::measured(80, 100, 80, 100, ContinuationPolicy::new(80, 80)?);
        assert!(
            watermark.is_due(),
            "eighty percent of either ceiling is due"
        );
        assert_eq!(watermark.event_headroom(), 20);
        assert_eq!(watermark.byte_headroom(), 20);
        Ok(())
    }

    #[test]
    fn the_watermark_is_either_ceiling_not_both() -> TestResult {
        let policy = ContinuationPolicy::new(80, 80)?;
        let events = ContinuationWatermark::measured(100, 100, 0, 1_000_000, policy);
        assert!(
            events.is_due(),
            "a full event ceiling is due whatever the byte count says"
        );
        let bytes = ContinuationWatermark::measured(0, 1_000_000, 1_000_000, 1_000_000, policy);
        assert!(
            bytes.is_due(),
            "a full byte ceiling is due whatever the event count says"
        );
        Ok(())
    }

    /// The declared policy is the shipped fraction of the shipped ceilings, and a
    /// policy at a ceiling is refused rather than clamped to one.
    #[test]
    fn the_declared_policy_leaves_the_shipped_headroom_and_a_late_one_is_refused() -> TestResult {
        let declared = ContinuationPolicy::declared();
        assert_eq!(declared, ContinuationPolicy::default());
        assert_eq!(declared.events_at(), 80_000);
        assert_eq!(declared.bytes_at(), 53_687_091);
        assert!(
            declared.events_at() < SHIPPED_EVENT_CEILING
                && declared.bytes_at() < SHIPPED_BYTE_CEILING,
            "the watermark is strictly inside both ceilings, so settlement always has room"
        );
        assert!(
            ContinuationPolicy::new(SHIPPED_EVENT_CEILING, 1).is_err(),
            "a trigger at the ceiling would ask for a continuation the journal cannot afford"
        );
        Ok(())
    }

    #[test]
    fn a_checkpoint_round_trips_through_its_payload() -> TestResult {
        let key = key("7")?;
        let verification = Verification::new(
            crate::effect::Id128::from_hex(&"42".repeat(16))?,
            3,
            blake3(b"observed"),
            crate::journal::VerificationResult::NotSatisfied,
        );
        let checkpoint = Continuation::new(
            4,
            JournalPosition::genesis(),
            vec![SettledAttempt::new(
                key,
                EventKind::Verified,
                AttemptStatus::VerificationFailed,
                Some(verification),
            )],
            vec![UnresolvedAttempt::new(key, EventKind::DispatchPrepared)],
        )?;

        let payload = checkpoint.to_payload()?;
        assert!(
            Continuation::is_payload(&payload),
            "a checkpoint payload carries the magic a reader asks for first"
        );
        assert!(
            !Continuation::is_payload(&[0x00, 0x01, 0x02]),
            "an event payload does not"
        );

        let read =
            Continuation::from_payload(&payload).ok_or("a checkpoint payload reads as one")??;
        assert_eq!(read, checkpoint, "the archive is what a writer wrote");
        assert_eq!(read.generation(), 4);
        assert_eq!(read.settled().len(), 1);
        assert_eq!(read.unresolved().len(), 1);
        let carried = read.settled().first().ok_or("the fold is missing")?;
        assert_eq!(carried.status(), Some(AttemptStatus::VerificationFailed));
        assert_eq!(
            carried.verification().map(Verification::predicate_version),
            Some(3),
            "the verification digest's predicate version crosses the boundary"
        );
        assert_eq!(
            read.unresolved()
                .first()
                .map(UnresolvedAttempt::recovered_status),
            Some(Some(AttemptStatus::OutcomeUnknown)),
            "an unresolved attempt stays unknown on the far side"
        );
        Ok(())
    }

    #[test]
    fn a_folded_action_refuses_an_older_attempt_and_admits_a_newer_one()
    -> Result<(), Box<dyn std::error::Error>> {
        let latest = key("9")?;
        let folded = SettledAttempt::new(latest, EventKind::Verified, AttemptStatus::Applied, None);
        assert!(
            folded.already_walked(latest),
            "the latest attempt itself is already walked"
        );
        assert!(
            folded.already_walked(key("3")?),
            "an older attempt of the same action is already walked"
        );
        assert!(
            !folded.already_walked(key("10")?),
            "a newer attempt is new work, not a replay"
        );
        Ok(())
    }

    /// The largest checkpoint the two declared counts admit fits one frame.
    ///
    /// Both counts are charged before the checkpoint is archived and the frame
    /// ceiling after it, so a count the frame cannot hold is a bound no writer
    /// reaches: the carry is refused by the frame, as a storage fault, instead of
    /// by the count it names. Measured at the widest record each list can hold — a
    /// settled attempt carrying a verification, at the largest generation.
    #[test]
    fn the_largest_checkpoint_the_counts_admit_fits_one_frame() -> TestResult {
        let one = key("1")?;
        let verification = Verification::new(
            crate::effect::Id128::from_hex(ACTION)?,
            u64::MAX,
            blake3(b"the widest verification a settled record carries"),
            crate::journal::VerificationResult::NotSatisfied,
        );
        let settled = (0..MAX_CHECKPOINT_SETTLED)
            .map(|_| {
                SettledAttempt::new(
                    one,
                    EventKind::Verified,
                    AttemptStatus::VerificationFailed,
                    Some(verification),
                )
            })
            .collect::<Vec<_>>();
        let unresolved = (0..MAX_CHECKPOINT_UNRESOLVED)
            .map(|_| UnresolvedAttempt::new(one, EventKind::DispatchPrepared))
            .collect::<Vec<_>>();
        let widest = Continuation::new(u64::MAX, JournalPosition::genesis(), settled, unresolved)?;
        let archived = widest.to_payload()?.len();
        assert!(
            archived <= super::super::file::MAX_FRAME_BYTES,
            "the widest checkpoint the counts admit archives to {archived} bytes, past \
             the {} byte frame it must be sealed in",
            super::super::file::MAX_FRAME_BYTES
        );
        Ok(())
    }

    #[test]
    fn a_carry_past_its_declared_bound_is_refused_before_anything_is_written() -> TestResult {
        let one = key("1")?;
        let folded = (1..=MAX_CHECKPOINT_SETTLED + 1)
            .map(|_| SettledAttempt::new(one, EventKind::Verified, AttemptStatus::Applied, None))
            .collect::<Vec<_>>();
        let refused = Continuation::new(1, JournalPosition::genesis(), folded, Vec::new());
        assert!(
            matches!(
                refused,
                Err(JournalError::CapacityExceeded {
                    resource: JournalLimitKind::CheckpointActions,
                    ..
                })
            ),
            "an over-large carry is refused, not framed: {refused:?}"
        );

        let unresolved = (0..MAX_CHECKPOINT_UNRESOLVED + 1)
            .map(|_| UnresolvedAttempt::new(one, EventKind::DispatchPrepared))
            .collect::<Vec<_>>();
        let refused = Continuation::new(1, JournalPosition::genesis(), Vec::new(), unresolved);
        assert!(
            matches!(
                refused,
                Err(JournalError::CapacityExceeded {
                    resource: JournalLimitKind::CheckpointUnresolved,
                    ..
                })
            ),
            "an over-large unresolved carry is refused: {refused:?}"
        );

        Ok(())
    }

    #[test]
    fn an_index_this_build_does_not_name_reads_as_absent_not_as_the_ladder_foot() {
        assert_eq!(rung_of(u8::MAX), None);
        assert_eq!(status_of(u8::MAX), None);
        assert_eq!(rung_of(0), Some(EventKind::IntentAdmitted));
        assert_eq!(
            status_index(AttemptStatus::VerificationFailed),
            status_of(5).map_or(u8::MAX, status_index)
        );
    }

    #[test]
    fn a_successor_is_a_fixed_width_name_in_the_predecessors_own_directory() -> TestResult {
        let first = Path::new("/var/run/bot.jrnl");
        let second = successor_path(first);
        assert_eq!(second, Path::new("/var/run/bot.jrnl.cont/000001"));
        assert_eq!(generation_of(first), 1);
        assert_eq!(generation_of(&second), 2);
        let third = successor_path(&second);
        assert_eq!(third, Path::new("/var/run/bot.jrnl.cont/000002"));
        assert_eq!(generation_of(&third), 3);

        // The point of the fixed width: a chain a hundred long has names the same
        // length as a chain one long, which is what a run measured in weeks needs
        // and what an appended token per generation would lose at fifty.
        let mut here = first.to_path_buf();
        for _ in 0..500 {
            here = successor_path(&here);
        }
        assert_eq!(
            here.file_name().map(std::ffi::OsStr::len),
            Some(GENERATION_DIGITS),
            "a five-hundredth generation's name is still {} characters",
            GENERATION_DIGITS
        );
        assert_eq!(generation_of(&here), 501);

        // And a base journal whose own name reads as a generation is refused,
        // because its successor would be un-derivable.
        assert!(
            refuse_ambiguous_base(Path::new("/var/run/000123")).is_err(),
            "a six-digit base name would be read as a generation"
        );
        assert!(refuse_ambiguous_base(first).is_ok());
        assert!(refuse_ambiguous_base(&third).is_ok());
        assert!(
            is_generation_path(&second),
            "and the successor is this scheme's own, both halves of the name"
        );
        assert!(!is_generation_path(Path::new("/var/run/bot.jrnl.cont/abc")));
        Ok(())
    }

    /// Every `SealPause` names itself and the enum's sweep is the whole enum, so
    /// a boundary added later cannot escape a crash sweep.
    #[test]
    fn every_boundary_names_itself() {
        let mut seen = std::collections::BTreeSet::new();
        for pause in SealPause::all() {
            assert!(
                seen.insert(pause.as_str()),
                "two boundaries share a spelling"
            );
            assert!(!pause.as_str().is_empty());
        }
        assert_eq!(seen.len(), SealPause::all().len());
    }

    /// The two carried statuses a successor must be able to report, so the enum
    /// this module folds to has a witness for each.
    #[test]
    fn a_carried_rung_reads_back_as_the_status_a_recovery_fold_reports() -> TestResult {
        assert_eq!(
            UnresolvedAttempt::new(key("1")?, EventKind::IntentAdmitted).recovered_status(),
            Some(AttemptStatus::Prepared)
        );
        assert_eq!(
            UnresolvedAttempt::new(key("1")?, EventKind::DispatchPrepared).recovered_status(),
            Some(AttemptStatus::OutcomeUnknown)
        );
        assert_eq!(
            UnresolvedAttempt::new(key("1")?, EventKind::Verified).recovered_status(),
            None,
            "a resolved rung is not an unresolved attempt and reads as absent"
        );
        let evidence = EffectEvidence::Applied;
        assert_eq!(
            format!("{evidence:?}"),
            "Applied",
            "the evidence arm exists"
        );
        Ok(())
    }
}
