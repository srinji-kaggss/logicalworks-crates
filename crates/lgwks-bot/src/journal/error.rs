//! What a journal can refuse.

use core::fmt;
use std::io;

use lgwks_std::wire::WireError;

use super::{
    Corruption, DurabilityPromise, EffectEvent, EventKind, JournalLimitKind, JournalPosition,
    SealPause,
};
use crate::effect::EffectKey;

/// What a journal can refuse.
#[derive(Debug)]
#[non_exhaustive]
pub enum JournalError {
    /// The tail the caller expected is not the tail that is committed.
    ///
    /// Not merged and not retried in place: it means another writer appended, so
    /// the caller's view of the run is stale and it has to re-read before it can
    /// decide anything.
    TailMismatch {
        /// The tail the caller stated.
        expected: JournalPosition,
        /// The tail the journal holds.
        actual: JournalPosition,
    },
    /// Recovery folded one history, but the journal advanced before that
    /// history could become the controller's append fence.
    SnapshotStale {
        /// Number of events recovery folded.
        recovered_events: u64,
        /// Number of events the journal reported afterwards.
        committed_events: u64,
    },
    /// The journal cannot promise the durability the caller requires.
    PromiseUnmet {
        /// What the caller needed.
        required: DurabilityPromise,
        /// What the journal offers.
        offered: DurabilityPromise,
    },
    /// The adapter cannot attest that an already-recorded outcome now meets
    /// the requested durability grade.
    ReceiptUnavailable {
        /// What the caller needs before it can settle the attempt.
        required: DurabilityPromise,
    },
    /// A receipt names a different append than the outcome being settled.
    ReceiptMismatch {
        /// The committed outcome position the caller is settling.
        expected: JournalPosition,
        /// The position the adapter returned in its receipt.
        actual: JournalPosition,
    },
    /// The position exists, but it contains a different event than the one
    /// the caller is trying to acknowledge.
    EntryMismatch {
        /// The exact committed position inspected.
        position: JournalPosition,
        /// The event the caller expected there.
        expected: Box<EffectEvent>,
        /// The event the adapter returned at that position.
        actual: Box<EffectEvent>,
    },
    /// The event cannot follow what is committed for that key.
    OutOfOrder {
        /// The attempt the refused event was about.
        ///
        /// Boxed because [`EffectKey`] is 128 bytes and an error carrying one by
        /// value would make every `Result` in this module pay for a case that is
        /// refused before it changes anything.
        key: Box<EffectKey>,
        /// What the ladder allows next, or `None` when the key is complete.
        expected: Option<EventKind>,
        /// What the caller tried to append.
        attempted: EventKind,
    },
    /// The run has more events than a journal position can address.
    ///
    /// Reachable only after 2^64 appends to one run. Named rather than wrapped,
    /// because a wrapped sequence would make two positions compare equal and
    /// defeat the guard the position exists to provide.
    Exhausted,
    /// A sealed journal's successor is complete, so this journal is not the
    /// authoritative one.
    ///
    /// The refusal carries where the authoritative journal is rather than a
    /// rendered string, because the caller's whole next move is to open that path
    /// and a caller that had to parse a message to learn it would be one message
    /// away from opening the sealed predecessor and appending to it.
    ///
    /// This is never a fault and never a loss: the predecessor's committed history
    /// is intact and readable, and the successor carries every attempt it left
    /// unresolved.
    Superseded {
        /// Where the authoritative journal lives.
        path: String,
    },
    /// An append names an attempt of an action the journal's chain already walked.
    ///
    /// Distinct from [`Self::OutOfOrder`] because it is a different fact. An
    /// out-of-order append names a rung that cannot follow a rung the journal
    /// holds *for that exact key*. This names an attempt number at or below the
    /// one a continuation folded for its action, which is a replay of an attempt
    /// the journal sealed: the key itself may never have been carried, because
    /// the sealed history, not the checkpoint, is what remembers it. Collapsing
    /// the two would report a replay of a sealed attempt as a ladder mistake on a
    /// key the journal has never heard of.
    AttemptAlreadyWalked {
        /// The attempt the refused event was about.
        key: Box<EffectKey>,
        /// The latest attempt the journal's chain walked for that action.
        latest: crate::effect::AttemptId,
    },
    /// A continuation was armed to stop at a boundary, and did.
    ///
    /// The fault-injection door a crash harness kills a process on. Nothing was
    /// acknowledged and the journal will not append again, so a caller that sees
    /// this reopens and reads the file back; a caller that was not expecting it
    /// sees a continuation that did not happen rather than a journal that quietly
    /// continued.
    ContinuationPaused {
        /// The boundary the continuation stopped at.
        boundary: SealPause,
    },
    /// Appending or opening would exceed a shipped journal's hard history cap.
    ///
    /// The refusal is non-destructive: committed and unresolved evidence is
    /// retained, and an over-limit file is not truncated to make it fit.
    CapacityExceeded {
        /// Which finite resource would exceed its ceiling.
        resource: JournalLimitKind,
        /// The resource's configured hard ceiling.
        limit: u64,
        /// The amount required by the refused operation or existing file.
        requested: u64,
    },
    /// Another writer holds the journal file's exclusive fence.
    ///
    /// The refusal carries the path because the fix is about *when* to open,
    /// not what to fix: wait for the holder to finish or die, then reopen.
    /// Appending under a second unfenced handle would let two controllers
    /// acknowledge histories that cannot replay as one chain, so this is
    /// refused before a single byte of the file is read — the replay and the
    /// torn-tail repair are the owner's alone.
    ///
    /// The fence is the operating system's advisory file lock: it binds
    /// cooperating writers that go through this constructor, and it is
    /// released by the kernel when the holder closes the file or dies. It is
    /// not containment against a writer that never asks for the lock, and it
    /// does not coordinate writers on other hosts — a distributed lease is a
    /// different mechanism.
    Locked {
        /// Where the contended journal lives.
        path: String,
    },
    /// The backing store refused the append before the event could be written.
    ///
    /// The journal is unchanged: the event is not committed, and a retry may
    /// re-append. Named apart from [`Self::OutcomeUnknown`] so a caller can
    /// tell "nothing landed, try again" from "something may have landed,
    /// read back" without knowing the adapter.
    Storage(io::Error),
    /// The append may have committed, and the acknowledgment was lost.
    ///
    /// This is the reply a real store produces when bytes reach the disk and
    /// the confirmation does not reach the caller: a write that fails part
    /// way, a sync whose result never arrives. The committed state of the
    /// event is *unknown* — it may be in the store, it may not — and the
    /// contract for a caller is reconciliation by readback, never a blind
    /// re-append: re-appending an event that did land is how an effect gets
    /// executed twice on the strength of a lost reply.
    ///
    /// An adapter returns this only for its own append's uncertain outcome.
    /// Every other refusal — a stale tail, a ladder violation, a store that
    /// refused before touching the log — leaves the journal unchanged and is
    /// reported as its own variant.
    OutcomeUnknown {
        /// The store error the reply was lost behind, when there was one.
        cause: io::Error,
    },
    /// The event could not be encoded for the chain.
    ///
    /// Not a caller error and not reachable by anything the caller controls:
    /// the encoding allocates, and this is that allocation failing. Named
    /// rather than folded into [`Self::Storage`] because nothing was stored —
    /// the append is refused before the journal is touched at all.
    Encoding(WireError),
    /// The journal's committed bytes do not re-derive the history they claim.
    ///
    /// This is not an interrupted append — an interrupted append is a torn
    /// tail, and a torn tail was never acknowledged, so repairing it costs
    /// nothing. This is committed bytes that no longer mean what the chain
    /// says: bit rot, or a hand on the file. It is refused rather than
    /// trimmed, because the record may have been acknowledged, and an
    /// acknowledgment the journal quietly rewrites is not a record.
    ///
    /// Boxed for the same reason [`Self::OutOfOrder`] boxes its key: an error
    /// carrying the payload by value would make every `Result` in this module
    /// pay for a case that is refused before it changes anything.
    Corrupt(Box<Corruption>),
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The scrutinee is `*self` so each pattern's type is the enum's own type
        // rather than a reference to it, and the two non-`Copy` payloads are
        // bound by `ref`. `clippy::pattern_type_mismatch` is forbidden in this
        // workspace, and this is the form it asks for.
        match *self {
            Self::TailMismatch { expected, actual } => write!(
                f,
                "journal tail moved: expected {expected}, committed {actual}"
            ),
            Self::SnapshotStale {
                recovered_events,
                committed_events,
            } => write!(
                f,
                "journal advanced during recovery: folded {recovered_events} events, committed {committed_events}"
            ),
            Self::PromiseUnmet { required, offered } => write!(
                f,
                "journal promises {offered}, which is below the {required} this needs"
            ),
            Self::ReceiptUnavailable { required } => write!(
                f,
                "journal cannot attest that the recorded outcome meets {required}"
            ),
            Self::ReceiptMismatch { expected, actual } => write!(
                f,
                "journal receipt names {actual}, not the outcome committed at {expected}"
            ),
            Self::EntryMismatch {
                position,
                ref expected,
                ref actual,
            } => write!(
                f,
                "journal entry at {position} is {actual}, not the acknowledged {expected}"
            ),
            Self::OutOfOrder {
                ref key,
                expected,
                attempted,
            } => match expected {
                Some(next) => write!(
                    f,
                    "{attempted} cannot follow what is committed for {key}; \
                     the next recorded fact is {next}"
                ),
                None => write!(
                    f,
                    "{attempted} cannot be appended for {key}; \
                     its recorded facts are complete"
                ),
            },
            Self::Exhausted => f.write_str("journal position exhausted"),
            Self::Superseded { ref path } => write!(
                f,
                "this journal is sealed and its successor is authoritative; open {path}"
            ),
            Self::AttemptAlreadyWalked { ref key, latest } => write!(
                f,
                "{key} is an attempt at or below the latest this journal walked \
                 for its action, which is {latest}; the sealed history already \
                 records it"
            ),
            Self::ContinuationPaused { boundary } => write!(
                f,
                "the continuation was armed to stop at {boundary}, so no successor \
                 was established"
            ),
            Self::CapacityExceeded {
                resource,
                limit,
                requested,
            } => write!(
                f,
                "journal {resource:?} limit exceeded: limit {limit}, requested {requested}"
            ),
            Self::Locked { ref path } => write!(
                f,
                "journal at {path} is held by another writer; \
                 wait for the holder to finish, then reopen"
            ),
            Self::Storage(ref cause) => {
                write!(f, "journal storage refused the append: {cause}")
            }
            Self::OutcomeUnknown { ref cause } => write!(
                f,
                "journal append may have committed, acknowledgment lost: {cause}; \
                 reconcile by readback rather than re-appending"
            ),
            Self::Encoding(ref cause) => {
                write!(
                    f,
                    "journal could not encode the event for the chain: {cause}"
                )
            }
            Self::Corrupt(ref corruption) => {
                write!(f, "journal refused its own committed bytes: {corruption}")
            }
        }
    }
}

impl std::error::Error for JournalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Storage(ref cause) => Some(cause),
            Self::OutcomeUnknown { ref cause } => Some(cause),
            Self::Encoding(ref cause) => Some(cause),
            Self::Corrupt(ref cause) => Some(cause),
            _ => None,
        }
    }
}
