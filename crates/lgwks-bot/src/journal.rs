//! Durable journal: the append that has to land before the irreversible
//! boundary.
//!
//! [`crate::effect`] supplies the identity a settlement is about. This module
//! supplies the place that identity is written down *before* anything
//! irreversible happens, because an identity held only in memory is lost at the
//! exact moment it is needed. A controller that hands bytes to an external
//! system and then crashes has no way to say whether the bytes arrived, and a
//! controller that guesses will either duplicate a non-idempotent effect or
//! silently drop one.
//!
//! # The promise a durability profile makes
//!
//! [`DurabilityPromise`] is graded, and the grade is the whole point: an
//! in-memory adapter can promise nothing across a process exit, so it reports
//! [`DurabilityPromise::Ephemeral`] and [`EffectJournal::admit_external_handoff`]
//! refuses it. That refusal is the load-bearing part of this module. A journal
//! that accepts an append and loses it on restart is worse than no journal,
//! because it converts "I do not know" into a false "I never sent it".
//!
//! Nothing here can verify a promise. A filesystem, a device cache or a
//! virtualization layer can each accept a write and lose it anyway, so a
//! [`DurableAck`] records the promise that was claimed rather than implying one
//! was proven. The tests that matter for that claim are crash tests against a
//! real backing store, and they are not unit tests.
//!
//! # Why compare-and-append
//!
//! Two controllers recovering the same run must not both dispatch. `&mut self`
//! fences within one process, which is not enough: the failure this guards is a
//! second process. So every append states the tail it believes is committed, and
//! an append whose belief is stale is refused rather than merged. A refused
//! append has changed nothing, which is what makes it safe to recover from.
//!
//! [`EffectKey`]: crate::effect::EffectKey
//! [`DurableAck`]: crate::journal::DurableAck
//! [`DurabilityPromise`]: crate::journal::DurabilityPromise
//! [`EffectJournal`]: crate::journal::EffectJournal
//! [`DurabilityPromise::Ephemeral`]: crate::journal::DurabilityPromise::Ephemeral
//! [`EffectJournal::admit_external_handoff`]: crate::journal::EffectJournal::admit_external_handoff

use core::fmt;
use std::io;

use lgwks_std::hash::{Digest, Hasher, blake3};
use lgwks_std::wire::{AlignedVec, WireError};

use crate::effect::{EffectKey, Id128};

/// The evidence vocabulary is `crate::ecs`'s, re-exported here rather than
/// restated.
///
/// The journal and the ledger answer the same question, "what does the evidence
/// say about whether the effect landed", and a second enum with the same two
/// arms would drift from this one the first time an arm was added. Re-exporting
/// keeps one definition with two consumers.
pub use crate::ecs::EffectEvidence;

mod wire_form;

/// The two journal records whose archived form is an enum, re-exported from the
/// private `wire_form` module, which records why they are declared there.
pub use wire_form::{
    ArchivedEffectEvent, ArchivedVerificationResult, EffectEvent, VerificationResult,
};

mod file;

/// The file-backed journal, re-exported from the private `file` module beside
/// the in-memory one: the trait's second shipped adapter, and the one whose
/// promises a process kill can check.
pub use file::{Corruption, CorruptionKind, FileJournal};

/// The domain separator hashed into the genesis position.
///
/// A chain has to start somewhere, and "started from 32 zero bytes" is a value
/// any other hash could coincide with. Separating the domain means the genesis
/// head cannot be mistaken for the hash of an empty or zeroed event.
const GENESIS_DOMAIN: &[u8] = b"lgwks.journal.v1.genesis";

/// What a journal can promise about an append that it has acknowledged.
///
/// Ordered from weakest to strongest, and compared by that order: a caller that
/// needs process-crash survival is satisfied by a power-loss promise, never the
/// reverse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum DurabilityPromise {
    /// Survives neither a process exit nor a crash. An in-memory adapter
    /// reports this, and this is the only honest thing it can report.
    Ephemeral,
    /// Survives the writing process dying, including a kill. May still be lost
    /// if the machine loses power before the store flushes.
    ProcessCrash,
    /// Survives power loss, to the extent the device honours its own flush
    /// contract.
    PowerLoss,
}

impl DurabilityPromise {
    /// Whether an append under this promise survives the writer dying.
    ///
    /// This is the threshold an external handoff requires. Below it, a
    /// recovered controller cannot distinguish "the effect never left" from
    /// "the record of it leaving was lost", and that is the state in which a
    /// blind resend happens.
    #[must_use]
    pub const fn survives_process_crash(self) -> bool {
        matches!(self, Self::ProcessCrash | Self::PowerLoss)
    }

    /// Whether this promise is at least as strong as `required`.
    ///
    /// A per-append acknowledgment is checked against the promise the handoff
    /// needs, not against the journal's advertisement: a store that claims
    /// `ProcessCrash` and acks `Ephemeral` is weaker than it says, and the
    /// weaker fact is the one that matters (issue #100).
    #[must_use]
    pub const fn meets(self, required: Self) -> bool {
        match (self, required) {
            (_, Self::Ephemeral) => true,
            (Self::ProcessCrash | Self::PowerLoss, Self::ProcessCrash) => true,
            (Self::PowerLoss, Self::PowerLoss) => true,
            (Self::Ephemeral, _) => false,
            (Self::ProcessCrash, Self::PowerLoss) => false,
        }
    }

    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ephemeral => "ephemeral",
            Self::ProcessCrash => "process_crash",
            Self::PowerLoss => "power_loss",
        }
    }
}

impl fmt::Display for DurabilityPromise {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a committed event sits in one run's journal.
///
/// The pair of sequence and head is the whole point. A sequence alone says
/// which append this was and nothing about what it contained, so two journals
/// that diverged would agree on their sequence numbers. The head is a hash over
/// every event up to and including this one, so a position is a commitment to
/// the entire history behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct JournalPosition {
    /// How many events are committed at or before this position. Zero is the
    /// genesis, which holds no events.
    sequence: u64,
    /// Hash over every committed event, in order, up to and including this one.
    head: Digest,
}

impl JournalPosition {
    /// The position before the first append.
    ///
    /// Its head is a domain-separated constant rather than a zero digest, so a
    /// journal that was never written cannot be confused with one whose first
    /// event hashed to zero.
    #[must_use]
    pub fn genesis() -> Self {
        Self {
            sequence: 0,
            head: blake3(GENESIS_DOMAIN),
        }
    }

    /// How many events are committed at or before this position.
    #[must_use]
    pub const fn sequence(self) -> u64 {
        self.sequence
    }

    /// The hash over every committed event up to and including this one.
    #[must_use]
    pub const fn head(self) -> Digest {
        self.head
    }
}

impl fmt::Display for JournalPosition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} @ {}", self.sequence, self.head)
    }
}

/// Which kind of fact an [`EffectEvent`] records.
///
/// Separate from the event because the ordering ladder is stated in kinds, and
/// a refusal that names the two kinds involved reads in a log line in a way
/// that a rendered event carrying its full key does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EventKind {
    /// The intent was frozen and admitted. Nothing has left the process.
    IntentAdmitted,
    /// Broker authority was obtained and this exact attempt was prepared. This
    /// is the last append before the irreversible boundary.
    DispatchPrepared,
    /// What the evidence says about whether the effect landed.
    OutcomeObserved,
    /// A named predicate was evaluated and produced a result.
    Verified,
}

impl EventKind {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IntentAdmitted => "intent_admitted",
            Self::DispatchPrepared => "dispatch_prepared",
            Self::OutcomeObserved => "outcome_observed",
            Self::Verified => "verified",
        }
    }

    /// The kind that may follow this one for the same key, or `None` when the
    /// key's ladder is complete.
    ///
    /// One attempt at one intent walks this ladder exactly once. A retry is a
    /// new [`crate::effect::AttemptId`] and therefore a new key, which is what
    /// stops a second `DispatchPrepared` for an existing key from being
    /// representable at all.
    #[must_use]
    pub const fn next(self) -> Option<Self> {
        match self {
            Self::IntentAdmitted => Some(Self::DispatchPrepared),
            Self::DispatchPrepared => Some(Self::OutcomeObserved),
            Self::OutcomeObserved => Some(Self::Verified),
            Self::Verified => None,
        }
    }
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl VerificationResult {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Satisfied => "satisfied",
            Self::NotSatisfied => "not_satisfied",
        }
    }
}

impl fmt::Display for VerificationResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A named predicate's answer, with what it was evaluated against.
///
/// The observations are carried as a digest rather than an inline record so the
/// journal stays bounded, and as a digest rather than a summary so the answer
/// cannot be re-read under a different input set. A bare "done" has no place
/// here: it names no predicate, so nothing can re-evaluate it.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    lgwks_std::wire::Archive,
    lgwks_std::wire::Serialize,
    lgwks_std::wire::Deserialize,
)]
#[rkyv(crate = lgwks_std::wire::rkyv, compare(PartialEq), derive(Debug))]
pub struct Verification {
    /// Which predicate was evaluated.
    predicate: Id128,
    /// Which revision of that predicate. A changed predicate is a different
    /// claim over the same observations, so the version is part of the record.
    predicate_version: u64,
    /// Digest of the observations the predicate was evaluated against.
    observations: Digest,
    /// The answer.
    result: VerificationResult,
}

impl Verification {
    /// Build a verification record.
    #[must_use]
    pub const fn new(
        predicate: Id128,
        predicate_version: u64,
        observations: Digest,
        result: VerificationResult,
    ) -> Self {
        Self {
            predicate,
            predicate_version,
            observations,
            result,
        }
    }

    /// Which predicate was evaluated.
    #[must_use]
    pub const fn predicate(self) -> Id128 {
        self.predicate
    }

    /// Which revision of that predicate.
    #[must_use]
    pub const fn predicate_version(self) -> u64 {
        self.predicate_version
    }

    /// Digest of the observations the predicate was evaluated against.
    #[must_use]
    pub const fn observations(self) -> Digest {
        self.observations
    }

    /// The answer.
    #[must_use]
    pub const fn result(self) -> VerificationResult {
        self.result
    }
}

impl EffectEvent {
    /// The attempt this fact is about.
    #[must_use]
    pub const fn key(self) -> EffectKey {
        match self {
            Self::IntentAdmitted { key }
            | Self::DispatchPrepared { key }
            | Self::OutcomeObserved { key, .. }
            | Self::Verified { key, .. } => key,
        }
    }

    /// Which kind of fact this is.
    #[must_use]
    pub const fn kind(self) -> EventKind {
        match self {
            Self::IntentAdmitted { .. } => EventKind::IntentAdmitted,
            Self::DispatchPrepared { .. } => EventKind::DispatchPrepared,
            Self::OutcomeObserved { .. } => EventKind::OutcomeObserved,
            Self::Verified { .. } => EventKind::Verified,
        }
    }

    /// The event's encoding, which is what the chain hashes.
    ///
    /// Public because an adapter outside this crate has to compute the same
    /// head to be a journal at all. The encoding is the estate's, not this
    /// module's: the record is archived by [`lgwks_std::wire`] exactly as every
    /// other estate type that crosses a byte boundary, so the discriminant, the
    /// identity and the payload are one encode rather than a framing this module
    /// maintains beside it. A reader accesses the archive in place instead of
    /// decoding it, which is the reason the format exists.
    ///
    /// The bytes are what a chain head commits to, so they are part of this
    /// module's durable contract: a change to the record's shape changes every
    /// head computed after it.
    ///
    /// # Errors
    ///
    /// [`WireError`] if the archive cannot be allocated.
    pub fn to_bytes(self) -> Result<AlignedVec, WireError> {
        lgwks_std::wire::to_bytes::<WireError>(&self)
    }
}

impl fmt::Display for EffectEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.kind(), self.key())
    }
}

/// One committed entry: the event and the position it landed at.
///
/// A named struct rather than a tuple because the two fields answer different
/// questions and a reader should not have to remember which came first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct JournalEntry {
    /// Where the entry sits in the chain.
    position: JournalPosition,
    /// The fact that was appended.
    event: EffectEvent,
}

impl JournalEntry {
    /// Build an entry.
    #[must_use]
    pub const fn new(position: JournalPosition, event: EffectEvent) -> Self {
        Self { position, event }
    }

    /// Where the entry sits in the chain.
    #[must_use]
    pub const fn position(&self) -> JournalPosition {
        self.position
    }

    /// The fact that was appended.
    ///
    /// By reference: [`EffectEvent`] carries a whole [`EffectKey`], and a
    /// journal replay walks every entry, so copying one per step would be a
    /// 192-byte move for no gain.
    #[must_use]
    pub const fn event(&self) -> &EffectEvent {
        &self.event
    }
}

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
    /// The backing store refused the append.
    Storage(io::Error),
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
            Self::Storage(ref cause) => {
                write!(f, "journal storage refused the append: {cause}")
            }
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
            Self::Encoding(ref cause) => Some(cause),
            Self::Corrupt(ref cause) => Some(cause),
            _ => None,
        }
    }
}

/// Proof that an append is committed under a named durability promise.
///
/// An acknowledgment is not a buffer acceptance, a log line or a `Drop`. It is
/// the statement that the configured promise has been met and that the journal's
/// head is now at [`Self::position`]. An adapter mints this only after the
/// promise is met, and nothing here can check that it did, which is why the
/// promise travels on the acknowledgment: a caller can at least see what was
/// claimed, and a test against a real store is what decides whether the claim
/// was true.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DurableAck {
    /// The committed position, sequence and head together.
    position: JournalPosition,
    /// The promise the journal claims to have met.
    promise: DurabilityPromise,
}

impl DurableAck {
    /// Mint an acknowledgment.
    ///
    /// Public because an adapter implementing [`EffectJournal`] outside this
    /// crate is where an acknowledgment is produced, and the type cannot be
    /// built any other way.
    #[must_use]
    pub const fn new(position: JournalPosition, promise: DurabilityPromise) -> Self {
        Self { position, promise }
    }

    /// Where the append landed.
    #[must_use]
    pub const fn position(self) -> JournalPosition {
        self.position
    }

    /// What the journal claims about surviving a crash.
    #[must_use]
    pub const fn promise(self) -> DurabilityPromise {
        self.promise
    }
}

/// An append-only record of a run's effects.
///
/// Implementors are adapters over a host's durable store. The trait is the
/// seam; the durability grade is what a caller checks before it uses one.
pub trait EffectJournal {
    /// What this journal promises about an acknowledged append.
    fn durability(&self) -> DurabilityPromise;

    /// The last committed position, or the genesis when nothing is committed.
    fn tail(&self) -> JournalPosition;

    /// Every committed event, in commit order.
    ///
    /// Required rather than optional, and read rather than streamed, because
    /// this is what [`recover`] consumes: a controller that comes back after a
    /// crash learns what was in flight by replaying its own journal, and a
    /// journal that cannot be read back cannot be the record behind an external
    /// handoff. An adapter over a store that can only be appended to is a
    /// *sink*, not a journal, and this trait is not a sink.
    ///
    /// The whole sequence rather than a page, because [`recover`] folds over it
    /// and a partial replay answers "what is uncertain" with a subset — which
    /// reads as "nothing is uncertain" for every attempt in the part that was
    /// not read.
    ///
    /// # Errors
    ///
    /// [`JournalError::Storage`] when the backing store refused to be read.
    fn committed(&self) -> Result<Vec<EffectEvent>, JournalError>;

    /// Every committed entry, including the position each event occupies.
    ///
    /// An acknowledgement is meaningful only when positioned readback proves
    /// it names the exact appended fact. A later tail can belong to another
    /// controller and must never become this controller's append fence.
    ///
    /// Adapters that cannot provide positioned readback must refuse dispatch,
    /// including local effects, rather than authorize an unbound append.
    fn committed_entries(&self) -> Result<Vec<JournalEntry>, JournalError> {
        Err(JournalError::ReceiptUnavailable {
            required: self.durability(),
        })
    }

    /// Append `event` if and only if `expected_tail` is still the committed
    /// tail.
    ///
    /// The `&mut self` receiver fences writers within one process; the tail
    /// check fences writers across processes, which is the case that actually
    /// happens. A refusal leaves the journal unchanged.
    ///
    /// # Errors
    ///
    /// [`JournalError::TailMismatch`] when another writer appended,
    /// [`JournalError::OutOfOrder`] when the event cannot follow what is
    /// committed for its key, [`JournalError::Exhausted`] when the position
    /// space is spent, and [`JournalError::Storage`] when the backing store
    /// refused.
    fn compare_and_append(
        &mut self,
        expected_tail: JournalPosition,
        event: &EffectEvent,
    ) -> Result<DurableAck, JournalError>;

    /// Confirm that an existing outcome record now meets `required`.
    ///
    /// An append can return a weak acknowledgment after it has already moved
    /// the per-key ladder to `OutcomeObserved`. A retry cannot append that
    /// outcome again, so an adapter that can flush, replicate, or otherwise
    /// obtain a stronger receipt implements this method. The default refuses
    /// rather than treating read-back of event bytes as a durability proof.
    ///
    /// # Errors
    ///
    /// [`JournalError::ReceiptUnavailable`] when the adapter has no receipt
    /// operation, or another journal error when it cannot obtain one.
    fn confirm_outcome(
        &mut self,
        _key: EffectKey,
        _evidence: EffectEvidence,
        _position: JournalPosition,
        required: DurabilityPromise,
    ) -> Result<DurableAck, JournalError> {
        Err(JournalError::ReceiptUnavailable { required })
    }

    /// Whether this journal may host an effect that has left the process.
    ///
    /// A journal that cannot survive its own writer dying must never be the
    /// record behind an external handoff, because after a crash it reports an
    /// empty run rather than an uncertain one.
    ///
    /// # Errors
    ///
    /// [`JournalError::PromiseUnmet`] when the journal is ephemeral.
    fn admit_external_handoff(&self) -> Result<DurabilityPromise, JournalError> {
        let offered = self.durability();
        if offered.survives_process_crash() {
            return Ok(offered);
        }
        Err(JournalError::PromiseUnmet {
            required: DurabilityPromise::ProcessCrash,
            offered,
        })
    }
}

/// What is known about one attempt after replaying a journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AttemptStatus {
    /// The intent was admitted and nothing was handed to an external system.
    Prepared,
    /// A dispatch was prepared and no outcome was ever recorded. This is the
    /// state a crash between the prepare append and the result recovers into,
    /// and it is deliberately not `NotApplied`: nothing established that the
    /// bytes did not arrive, and assuming they did not is how a duplicate
    /// non-idempotent effect gets sent.
    OutcomeUnknown,
    /// Evidence says the effect landed.
    Applied,
    /// Evidence says the effect did not land.
    NotApplied,
    /// A named predicate was evaluated and held over observations newer than
    /// the effect.
    Verified,
}

impl AttemptStatus {
    /// Whether an external system may or may not have acted.
    ///
    /// The one predicate a recovery path should branch on before it considers
    /// any resend.
    #[must_use]
    pub const fn is_uncertain(self) -> bool {
        matches!(self, Self::OutcomeUnknown)
    }

    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::OutcomeUnknown => "outcome_unknown",
            Self::Applied => "applied",
            Self::NotApplied => "not_applied",
            Self::Verified => "verified",
        }
    }
}

impl fmt::Display for AttemptStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One attempt and what the journal knows about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Attempt {
    /// The attempt.
    key: EffectKey,
    /// What is known about it.
    status: AttemptStatus,
}

impl Attempt {
    /// Build an attempt record.
    #[must_use]
    pub const fn new(key: EffectKey, status: AttemptStatus) -> Self {
        Self { key, status }
    }

    /// The attempt.
    #[must_use]
    pub const fn key(&self) -> EffectKey {
        self.key
    }

    /// What is known about it.
    #[must_use]
    pub const fn status(&self) -> AttemptStatus {
        self.status
    }
}

/// Every attempt a journal's events mention, in the order their intent was
/// first admitted.
///
/// Ordered by first appearance rather than by key, because key order is
/// arbitrary and a report a person reads should list attempts in the order the
/// run reached them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recovered {
    /// One entry per key, in admission order.
    attempts: Vec<Attempt>,
}

impl Recovered {
    /// What is known about `key`, or `None` when the journal never mentioned
    /// it.
    #[must_use]
    pub fn status(&self, key: EffectKey) -> Option<AttemptStatus> {
        self.attempts
            .iter()
            .find(|attempt| attempt.key == key)
            .map(|attempt| attempt.status)
    }

    /// How many attempts the journal mentions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.attempts.len()
    }

    /// Whether the journal mentions no attempts.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.attempts.is_empty()
    }

    /// The attempts whose external outcome is unknown.
    ///
    /// This is the list a recovery path must settle before it may dispatch
    /// anything again, so it is a first-class query rather than a fold each
    /// caller writes for itself.
    #[must_use]
    pub fn uncertain(&self) -> Vec<EffectKey> {
        self.attempts
            .iter()
            .filter(|attempt| attempt.status.is_uncertain())
            .map(|attempt| attempt.key)
            .collect()
    }

    /// Every attempt and its state, in admission order.
    #[must_use]
    pub fn attempts(&self) -> &[Attempt] {
        &self.attempts
    }
}

/// Fold a journal's events into what is known about each attempt.
///
/// The rule that makes this worth having: a `DispatchPrepared` with no later
/// outcome recovers as [`AttemptStatus::OutcomeUnknown`], never as
/// `NotApplied`. A controller that cannot positively prove no handoff occurred
/// has to stay uncertain, and losing availability is the cheaper error.
#[must_use]
pub fn recover<'a>(events: impl IntoIterator<Item = &'a EffectEvent>) -> Recovered {
    let mut recovered = Recovered::default();
    for event in events {
        let status = match *event {
            EffectEvent::IntentAdmitted { .. } => AttemptStatus::Prepared,
            EffectEvent::DispatchPrepared { .. } => AttemptStatus::OutcomeUnknown,
            EffectEvent::OutcomeObserved { evidence, .. } => match evidence {
                EffectEvidence::Applied => AttemptStatus::Applied,
                EffectEvidence::NotApplied => AttemptStatus::NotApplied,
            },
            EffectEvent::Verified { verification, .. } => match verification.result() {
                VerificationResult::Satisfied => AttemptStatus::Verified,
                VerificationResult::NotSatisfied => AttemptStatus::Applied,
            },
        };
        let key = event.key();
        match recovered
            .attempts
            .iter_mut()
            .find(|attempt| attempt.key == key)
        {
            Some(attempt) => attempt.status = status,
            None => recovered.attempts.push(Attempt::new(key, status)),
        }
    }
    recovered
}

/// Where a recomputed chain stopped agreeing with what was recorded, or why it
/// could not be recomputed at all.
///
/// Two arms rather than one, because encoding the events is a fallible step
/// standing between them and the comparison, and a chain that could not be
/// walked is *unknown* rather than sound. Folding that into "no disagreement
/// found" is the one answer this type must not give.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChainBreak {
    /// The recomputed head did not match what was recorded.
    Disagreement {
        /// The sequence number of the first entry that did not follow.
        at: u64,
        /// The position the journal recorded for that entry.
        recorded: JournalPosition,
        /// The position recomputed from the events.
        recomputed: JournalPosition,
    },
    /// The entry could not be re-encoded, so the chain could not be checked.
    ///
    /// Nothing was compared. The encoding allocates, and this is that
    /// allocation failing.
    Unencodable {
        /// The sequence number of the entry that could not be encoded.
        at: u64,
    },
}

impl ChainBreak {
    /// The sequence number of the entry the break is about.
    #[must_use]
    pub const fn at(self) -> u64 {
        match self {
            Self::Disagreement { at, .. } | Self::Unencodable { at } => at,
        }
    }

    /// The position the journal recorded, when the break is a disagreement.
    #[must_use]
    pub const fn recorded(self) -> Option<JournalPosition> {
        match self {
            Self::Disagreement { recorded, .. } => Some(recorded),
            Self::Unencodable { .. } => None,
        }
    }

    /// The position recomputed from the events, when the break is a
    /// disagreement.
    #[must_use]
    pub const fn recomputed(self) -> Option<JournalPosition> {
        match self {
            Self::Disagreement { recomputed, .. } => Some(recomputed),
            Self::Unencodable { .. } => None,
        }
    }
}

impl fmt::Display for ChainBreak {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Disagreement {
                at,
                recorded,
                recomputed,
            } => write!(
                f,
                "journal chain breaks at {at}: recorded {recorded}, recomputed {recomputed}"
            ),
            Self::Unencodable { at } => write!(
                f,
                "journal entry {at} could not be re-encoded, so the chain could not be checked"
            ),
        }
    }
}

impl std::error::Error for ChainBreak {}

/// The head that follows `previous` once `event` is appended.
///
/// The previous head is hashed in, so the chain commits to order and not only
/// to content: the same two events in the other order produce a different head,
/// which is what makes a reordered or dropped entry detectable rather than
/// merely suspicious.
fn chain(previous: JournalPosition, event: &EffectEvent) -> Result<Digest, JournalError> {
    let mut hasher = Hasher::new();
    hasher.update(previous.head().as_bytes());
    hasher.update(&event.to_bytes().map_err(JournalError::Encoding)?);
    Ok(hasher.finalize())
}

/// What the ladder allows next for `key`, given the entries committed so far.
fn next_allowed(committed: &[JournalEntry], key: EffectKey) -> Option<EventKind> {
    let mut last: Option<EventKind> = None;
    for entry in committed {
        if entry.event().key() == key {
            last = Some(entry.event().kind());
        }
    }
    match last {
        None => Some(EventKind::IntentAdmitted),
        Some(kind) => kind.next(),
    }
}

/// Recompute the chain over `entries` and report the first disagreement.
///
/// # Errors
///
/// [`ChainBreak`] naming the first entry whose recorded position is not the one
/// its events produce.
pub fn verify_chain(entries: &[JournalEntry]) -> Result<JournalPosition, ChainBreak> {
    let mut position = JournalPosition::genesis();
    for entry in entries {
        let sequence = position.sequence().saturating_add(1);
        let head = match chain(position, entry.event()) {
            Ok(head) => head,
            Err(_) => return Err(ChainBreak::Unencodable { at: sequence }),
        };
        let recomputed = JournalPosition { sequence, head };
        if recomputed != entry.position() {
            return Err(ChainBreak::Disagreement {
                at: sequence,
                recorded: entry.position(),
                recomputed,
            });
        }
        position = recomputed;
    }
    Ok(position)
}

/// An in-memory journal, for tests and for runs whose effects never leave the
/// process.
///
/// It reports [`DurabilityPromise::Ephemeral`] and therefore cannot be the
/// record behind an external handoff. That is the whole reason it exists as a
/// named type rather than as a default: a caller that reaches for it gets a
/// refusal at the boundary instead of a green test that means nothing.
#[derive(Debug, Clone)]
pub struct MemoryJournal {
    /// Every committed entry, in append order.
    committed: Vec<JournalEntry>,
}

impl Default for MemoryJournal {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryJournal {
    /// An empty journal at the genesis.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            committed: Vec::new(),
        }
    }

    /// Every committed entry, in append order.
    #[must_use]
    pub fn committed(&self) -> &[JournalEntry] {
        &self.committed
    }

    /// The committed events, in append order.
    ///
    /// No `#[must_use]` here: the iterator this returns already carries one, so
    /// repeating it would be a second attribute saying the same thing.
    pub fn events(&self) -> impl Iterator<Item = &EffectEvent> {
        self.committed.iter().map(JournalEntry::event)
    }

    /// Recompute the chain and report the first disagreement, if any.
    ///
    /// # Errors
    ///
    /// [`ChainBreak`] when a recorded position does not follow from the events.
    pub fn verify(&self) -> Result<JournalPosition, ChainBreak> {
        verify_chain(&self.committed)
    }

    /// What the journal has learned about each attempt.
    #[must_use]
    pub fn recover(&self) -> Recovered {
        recover(self.events())
    }
}

impl EffectJournal for MemoryJournal {
    fn durability(&self) -> DurabilityPromise {
        DurabilityPromise::Ephemeral
    }

    fn tail(&self) -> JournalPosition {
        match self.committed.last() {
            Some(entry) => entry.position(),
            None => JournalPosition::genesis(),
        }
    }

    /// Copied out rather than borrowed, because the trait hands the caller a
    /// value it owns: `recover` folds over the sequence and a journal that
    /// handed back a borrow would tie the fold to the journal's own lifetime,
    /// which is exactly the coupling a caller reading a journal it is about to
    /// give back does not want.
    fn committed(&self) -> Result<Vec<EffectEvent>, JournalError> {
        Ok(self.events().copied().collect())
    }

    fn committed_entries(&self) -> Result<Vec<JournalEntry>, JournalError> {
        Ok(self.committed.clone())
    }

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
        let expected = next_allowed(&self.committed, key);
        if expected != Some(attempted) {
            return Err(JournalError::OutOfOrder {
                key: Box::new(key),
                expected,
                attempted,
            });
        }
        let sequence = actual
            .sequence()
            .checked_add(1)
            .ok_or(JournalError::Exhausted)?;
        let position = JournalPosition {
            sequence,
            head: chain(actual, event)?,
        };
        self.committed.push(JournalEntry::new(position, *event));
        Ok(DurableAck::new(position, self.durability()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::{
        ActionDigest, ActionId, AttemptId, EnvironmentEpoch, EnvironmentId, FlowRevision, RunId,
    };

    const RUN: &str = "0102030405060708090a0b0c0d0e0f10";
    const ACTION: &str = "1112131415161718191a1b1c1d1e1f20";
    const OTHER_ACTION: &str = "3132333435363738393a3b3c3d3e3f40";
    const ENV: &str = "2122232425262728292a2b2c2d2e2f30";
    const FLOW_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    const DIGEST_HEX: &str = "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeffe0e1e2e3e4e5e6e7e8e9eaebecedeeef";
    const PREDICATE: &str = "4142434445464748494a4b4c4d4e4f50";

    /// The crate refuses a panicking path anywhere, tests included, so a test
    /// that needs a parsed value returns `Result` and propagates. A failure
    /// then names the cause instead of a line number in a macro.
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// A key for the shared run and action, at the given attempt and epoch.
    fn key(attempt: &str, epoch: &str) -> Result<EffectKey, Box<dyn std::error::Error>> {
        build(ACTION, attempt, epoch)
    }

    /// A key for a different action, so identity comparisons have something to
    /// differ from.
    fn other_key(attempt: &str, epoch: &str) -> Result<EffectKey, Box<dyn std::error::Error>> {
        build(OTHER_ACTION, attempt, epoch)
    }

    /// Assemble a key from wire-form parts.
    fn build(
        action: &str,
        attempt: &str,
        epoch: &str,
    ) -> Result<EffectKey, Box<dyn std::error::Error>> {
        Ok(EffectKey::new(
            RunId::from_hex(RUN)?,
            ActionId::from_hex(action)?,
            AttemptId::from_decimal(attempt)?,
            FlowRevision::from_tagged("blake3_256", FLOW_HEX)?,
            ActionDigest::from_tagged("blake3_256", DIGEST_HEX)?,
            EnvironmentId::from_hex(ENV)?,
            EnvironmentEpoch::from_decimal(epoch)?,
        ))
    }

    /// A verification record naming a real predicate and a real observation
    /// digest.
    fn satisfied() -> Result<Verification, Box<dyn std::error::Error>> {
        Ok(Verification::new(
            Id128::from_hex(PREDICATE)?,
            1,
            blake3(b"observations"),
            VerificationResult::Satisfied,
        ))
    }

    /// Append `event` to `journal` at its own tail, which is what a correct
    /// caller does.
    fn append(
        journal: &mut MemoryJournal,
        event: EffectEvent,
    ) -> Result<DurableAck, Box<dyn std::error::Error>> {
        let tail = journal.tail();
        Ok(journal.compare_and_append(tail, &event)?)
    }

    /// Walk one attempt to the point where the irreversible boundary is next.
    fn admit_and_prepare(
        journal: &mut MemoryJournal,
        key: EffectKey,
    ) -> Result<(), Box<dyn std::error::Error>> {
        append(journal, EffectEvent::IntentAdmitted { key })?;
        append(journal, EffectEvent::DispatchPrepared { key })?;
        Ok(())
    }

    #[test]
    fn genesis_is_not_a_zero_head() -> TestResult {
        let genesis = JournalPosition::genesis();
        assert_eq!(genesis.sequence(), 0);
        assert_ne!(genesis.head(), blake3(&[]));
        assert_eq!(genesis, JournalPosition::genesis());
        Ok(())
    }

    #[test]
    fn an_append_advances_the_sequence_and_moves_the_head() -> TestResult {
        let key = key("1", "1")?;
        let mut journal = MemoryJournal::new();
        let genesis = journal.tail();
        let ack = append(&mut journal, EffectEvent::IntentAdmitted { key })?;
        assert_eq!(ack.position().sequence(), 1);
        assert_ne!(ack.position().head(), genesis.head());
        assert_eq!(journal.tail(), ack.position());
        Ok(())
    }

    #[test]
    fn the_acknowledgment_names_the_promise_that_was_claimed() -> TestResult {
        let key = key("1", "1")?;
        let mut journal = MemoryJournal::new();
        let ack = append(&mut journal, EffectEvent::IntentAdmitted { key })?;
        assert_eq!(ack.promise(), DurabilityPromise::Ephemeral);
        Ok(())
    }

    #[test]
    fn two_events_in_another_order_are_a_different_head() -> TestResult {
        let first = key("1", "1")?;
        let second = other_key("1", "1")?;

        let mut forwards = MemoryJournal::new();
        append(&mut forwards, EffectEvent::IntentAdmitted { key: first })?;
        append(&mut forwards, EffectEvent::IntentAdmitted { key: second })?;

        let mut backwards = MemoryJournal::new();
        append(&mut backwards, EffectEvent::IntentAdmitted { key: second })?;
        append(&mut backwards, EffectEvent::IntentAdmitted { key: first })?;

        assert_eq!(forwards.tail().sequence(), backwards.tail().sequence());
        assert_ne!(forwards.tail().head(), backwards.tail().head());
        Ok(())
    }

    #[test]
    fn a_stale_tail_is_refused_and_changes_nothing() -> TestResult {
        let key = key("1", "1")?;
        let mut journal = MemoryJournal::new();
        let genesis = journal.tail();
        let committed = append(&mut journal, EffectEvent::IntentAdmitted { key })?;

        let refused = journal.compare_and_append(genesis, &EffectEvent::DispatchPrepared { key });
        match refused {
            Err(JournalError::TailMismatch { expected, actual }) => {
                assert_eq!(expected, genesis);
                assert_eq!(actual, committed.position());
            }
            other => return Err(format!("expected a tail mismatch, got {other:?}").into()),
        }
        assert_eq!(journal.committed().len(), 1);
        assert_eq!(journal.tail(), committed.position());
        Ok(())
    }

    #[test]
    fn a_second_controller_reading_the_same_tail_is_fenced() -> TestResult {
        let key = key("1", "1")?;
        let mut journal = MemoryJournal::new();
        let both_saw = journal.tail();

        let winner = journal.compare_and_append(both_saw, &EffectEvent::IntentAdmitted { key })?;

        let loser = journal.compare_and_append(both_saw, &EffectEvent::DispatchPrepared { key });
        assert!(
            matches!(loser, Err(JournalError::TailMismatch { .. })),
            "the second writer from a stale reading must be refused"
        );
        assert_eq!(journal.committed().len(), 1);
        assert_eq!(journal.tail(), winner.position());
        Ok(())
    }

    #[test]
    fn a_dispatch_before_its_intent_is_refused() -> TestResult {
        let key = key("1", "1")?;
        let mut journal = MemoryJournal::new();
        let refused =
            journal.compare_and_append(journal.tail(), &EffectEvent::DispatchPrepared { key });
        match refused {
            Err(JournalError::OutOfOrder {
                key: refused_key,
                expected,
                attempted,
            }) => {
                assert_eq!(*refused_key, key);
                assert_eq!(expected, Some(EventKind::IntentAdmitted));
                assert_eq!(attempted, EventKind::DispatchPrepared);
            }
            other => return Err(format!("expected an ordering refusal, got {other:?}").into()),
        }
        assert!(journal.committed().is_empty());
        Ok(())
    }

    #[test]
    fn the_ladder_is_walked_once_per_key() -> TestResult {
        let key = key("1", "1")?;
        let mut journal = MemoryJournal::new();
        append(&mut journal, EffectEvent::IntentAdmitted { key })?;
        let refused =
            journal.compare_and_append(journal.tail(), &EffectEvent::IntentAdmitted { key });
        assert!(
            matches!(
                refused,
                Err(JournalError::OutOfOrder {
                    expected: Some(EventKind::DispatchPrepared),
                    attempted: EventKind::IntentAdmitted,
                    ..
                })
            ),
            "a second intent for one key is how a blind resend becomes representable"
        );
        assert_eq!(journal.committed().len(), 1);
        Ok(())
    }

    #[test]
    fn a_complete_ladder_refuses_anything_further() -> TestResult {
        let key = key("1", "1")?;
        let mut journal = MemoryJournal::new();
        admit_and_prepare(&mut journal, key)?;
        append(
            &mut journal,
            EffectEvent::OutcomeObserved {
                key,
                evidence: EffectEvidence::Applied,
            },
        )?;
        append(
            &mut journal,
            EffectEvent::Verified {
                key,
                verification: satisfied()?,
            },
        )?;
        let refused = journal.compare_and_append(
            journal.tail(),
            &EffectEvent::OutcomeObserved {
                key,
                evidence: EffectEvidence::NotApplied,
            },
        );
        assert!(
            matches!(
                refused,
                Err(JournalError::OutOfOrder {
                    expected: None,
                    attempted: EventKind::OutcomeObserved,
                    ..
                })
            ),
            "nothing follows a verification"
        );
        Ok(())
    }

    #[test]
    fn an_ephemeral_journal_reports_itself_and_refuses_the_boundary() -> TestResult {
        let journal = MemoryJournal::new();
        assert_eq!(journal.durability(), DurabilityPromise::Ephemeral);
        match journal.admit_external_handoff() {
            Err(JournalError::PromiseUnmet { required, offered }) => {
                assert_eq!(required, DurabilityPromise::ProcessCrash);
                assert_eq!(offered, DurabilityPromise::Ephemeral);
            }
            other => {
                return Err(format!(
                    "an in-memory journal must not host an external handoff, got {other:?}"
                )
                .into());
            }
        }
        Ok(())
    }

    #[test]
    fn only_a_crash_surviving_promise_clears_the_boundary() {
        assert!(!DurabilityPromise::Ephemeral.survives_process_crash());
        assert!(DurabilityPromise::ProcessCrash.survives_process_crash());
        assert!(DurabilityPromise::PowerLoss.survives_process_crash());
        assert!(DurabilityPromise::PowerLoss > DurabilityPromise::ProcessCrash);
    }

    #[test]
    fn a_prepared_dispatch_with_no_result_recovers_unknown() -> TestResult {
        let key = key("1", "1")?;
        let mut journal = MemoryJournal::new();
        admit_and_prepare(&mut journal, key)?;

        let recovered = journal.recover();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered.status(key), Some(AttemptStatus::OutcomeUnknown));
        assert_eq!(recovered.uncertain(), vec![key]);
        Ok(())
    }

    #[test]
    fn an_observed_not_applied_settles_the_unknown() -> TestResult {
        let key = key("1", "1")?;
        let mut journal = MemoryJournal::new();
        admit_and_prepare(&mut journal, key)?;
        append(
            &mut journal,
            EffectEvent::OutcomeObserved {
                key,
                evidence: EffectEvidence::NotApplied,
            },
        )?;

        let recovered = journal.recover();
        assert_eq!(recovered.status(key), Some(AttemptStatus::NotApplied));
        assert!(
            recovered.uncertain().is_empty(),
            "evidence that the effect did not land is what ends the uncertainty"
        );
        Ok(())
    }

    #[test]
    fn an_admitted_intent_alone_is_not_uncertain() -> TestResult {
        let key = key("1", "1")?;
        let mut journal = MemoryJournal::new();
        append(&mut journal, EffectEvent::IntentAdmitted { key })?;
        assert_eq!(journal.recover().status(key), Some(AttemptStatus::Prepared));
        assert!(journal.recover().uncertain().is_empty());
        Ok(())
    }

    #[test]
    fn a_satisfied_predicate_is_the_only_verified_state() -> TestResult {
        let key = key("1", "1")?;
        let mut journal = MemoryJournal::new();
        admit_and_prepare(&mut journal, key)?;
        append(
            &mut journal,
            EffectEvent::OutcomeObserved {
                key,
                evidence: EffectEvidence::Applied,
            },
        )?;
        append(
            &mut journal,
            EffectEvent::Verified {
                key,
                verification: satisfied()?,
            },
        )?;
        assert_eq!(journal.recover().status(key), Some(AttemptStatus::Verified));
        Ok(())
    }

    #[test]
    fn a_predicate_that_did_not_hold_is_not_a_verification() -> TestResult {
        let key = key("1", "1")?;
        let mut journal = MemoryJournal::new();
        admit_and_prepare(&mut journal, key)?;
        append(
            &mut journal,
            EffectEvent::OutcomeObserved {
                key,
                evidence: EffectEvidence::Applied,
            },
        )?;
        append(
            &mut journal,
            EffectEvent::Verified {
                key,
                verification: Verification::new(
                    Id128::from_hex(PREDICATE)?,
                    1,
                    blake3(b"observations"),
                    VerificationResult::NotSatisfied,
                ),
            },
        )?;

        let status = journal.recover().status(key);
        assert_eq!(status, Some(AttemptStatus::Applied));
        assert_ne!(status, Some(AttemptStatus::Verified));
        Ok(())
    }

    #[test]
    fn recovery_lists_attempts_in_admission_order() -> TestResult {
        let first = key("1", "1")?;
        let second = other_key("2", "1")?;
        let mut journal = MemoryJournal::new();
        admit_and_prepare(&mut journal, second)?;
        admit_and_prepare(&mut journal, first)?;

        let attempts = journal.recover().attempts().to_vec();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].key(), second);
        assert_eq!(attempts[1].key(), first);
        Ok(())
    }

    #[test]
    fn verify_accepts_an_untampered_chain() -> TestResult {
        let key = key("1", "1")?;
        let mut journal = MemoryJournal::new();
        admit_and_prepare(&mut journal, key)?;
        assert_eq!(journal.verify()?, journal.tail());
        Ok(())
    }

    #[test]
    fn verify_rejects_a_rewritten_position() -> TestResult {
        let key = key("1", "1")?;
        let mut journal = MemoryJournal::new();
        admit_and_prepare(&mut journal, key)?;

        let mut tampered = journal.committed().to_vec();
        let last = tampered
            .pop()
            .ok_or("the journal should hold two entries after admit and prepare")?;
        tampered.push(JournalEntry::new(JournalPosition::genesis(), *last.event()));

        match verify_chain(&tampered) {
            Err(broken) => assert_eq!(broken.at(), 2),
            Ok(position) => {
                return Err(
                    format!("a rewritten position must be detected, got {position}").into(),
                );
            }
        }
        Ok(())
    }

    #[test]
    fn verify_rejects_a_reordered_chain() -> TestResult {
        let key = key("1", "1")?;
        let mut journal = MemoryJournal::new();
        admit_and_prepare(&mut journal, key)?;

        let mut reordered = journal.committed().to_vec();
        reordered.reverse();

        match verify_chain(&reordered) {
            Err(broken) => assert_eq!(broken.at(), 1),
            Ok(position) => {
                return Err(format!("a reordered chain must be detected, got {position}").into());
            }
        }
        Ok(())
    }

    #[test]
    fn a_key_encodes_deterministically() -> TestResult {
        let key = key("1", "1")?;
        let bytes = key.to_bytes()?;
        assert_eq!(
            bytes.as_slice(),
            key.to_bytes()?.as_slice(),
            "one key encodes the same way twice"
        );
        assert!(
            !bytes.is_empty(),
            "and the encoding carries something: {bytes:?}"
        );
        Ok(())
    }

    #[test]
    fn keys_differing_in_one_field_differ_in_bytes() -> TestResult {
        let base = key("1", "1")?;
        let later_attempt = key("2", "1")?;
        let later_epoch = key("1", "2")?;
        let other = other_key("1", "1")?;

        assert_ne!(
            base.to_bytes()?.as_slice(),
            later_attempt.to_bytes()?.as_slice()
        );
        assert_ne!(
            base.to_bytes()?.as_slice(),
            later_epoch.to_bytes()?.as_slice()
        );
        assert_ne!(base.to_bytes()?.as_slice(), other.to_bytes()?.as_slice());
        Ok(())
    }

    #[test]
    fn an_event_encoding_carries_its_kind_and_its_key() -> TestResult {
        let key = key("1", "1")?;
        let admitted = EffectEvent::IntentAdmitted { key }.to_bytes()?;
        let prepared = EffectEvent::DispatchPrepared { key }.to_bytes()?;
        let applied = EffectEvent::OutcomeObserved {
            key,
            evidence: EffectEvidence::Applied,
        }
        .to_bytes()?;
        let not_applied = EffectEvent::OutcomeObserved {
            key,
            evidence: EffectEvidence::NotApplied,
        }
        .to_bytes()?;

        assert_ne!(admitted.as_slice(), prepared.as_slice());
        assert_ne!(applied.as_slice(), not_applied.as_slice());

        // What the encoding says about the event is read back out of it. A
        // field that failed to encode would not shorten the buffer, since the
        // archived form is one width for every variant of a type, so comparing
        // lengths would not have caught it. Decoding does.
        for (bytes, event) in [
            (admitted, EffectEvent::IntentAdmitted { key }),
            (prepared, EffectEvent::DispatchPrepared { key }),
            (
                applied,
                EffectEvent::OutcomeObserved {
                    key,
                    evidence: EffectEvidence::Applied,
                },
            ),
            (
                not_applied,
                EffectEvent::OutcomeObserved {
                    key,
                    evidence: EffectEvidence::NotApplied,
                },
            ),
        ] {
            assert_eq!(
                lgwks_std::wire::from_bytes::<EffectEvent, WireError>(&bytes)?,
                event
            );
        }
        Ok(())
    }

    #[test]
    fn a_verification_encoding_names_its_predicate_and_its_version() -> TestResult {
        let key = key("1", "1")?;
        let first = EffectEvent::Verified {
            key,
            verification: satisfied()?,
        }
        .to_bytes()?;
        let next_version = EffectEvent::Verified {
            key,
            verification: Verification::new(
                Id128::from_hex(PREDICATE)?,
                2,
                blake3(b"observations"),
                VerificationResult::Satisfied,
            ),
        }
        .to_bytes()?;
        assert_ne!(first.as_slice(), next_version.as_slice());
        assert_eq!(
            lgwks_std::wire::from_bytes::<EffectEvent, WireError>(&first)?,
            EffectEvent::Verified {
                key,
                verification: satisfied()?,
            }
        );
        assert_eq!(
            lgwks_std::wire::from_bytes::<EffectEvent, WireError>(&next_version)?,
            EffectEvent::Verified {
                key,
                verification: Verification::new(
                    Id128::from_hex(PREDICATE)?,
                    2,
                    blake3(b"observations"),
                    VerificationResult::Satisfied,
                ),
            }
        );
        Ok(())
    }
}
