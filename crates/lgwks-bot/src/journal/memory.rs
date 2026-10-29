//! The chain check and the in-memory journal: recomputing the hash chain, the append-order rule, and MemoryJournal.

use core::fmt;

#[cfg(feature = "wire")]
use lgwks_std::hash::{Digest, Hasher};

use super::frame::SaturatingFrom;
use super::{
    ContinuationWatermark, DurabilityPromise, EffectEvent, EffectEvidence, EffectJournal,
    JournalEntry, JournalError, JournalPosition, MAX_JOURNAL_BYTES, MAX_JOURNAL_EVENTS, Recovered,
    recover,
};
#[cfg(feature = "wire")]
use super::{DurableAck, EventKind, JournalLimitKind};
use crate::effect::EffectKey;

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
#[cfg(feature = "wire")]
pub(super) fn chain(
    previous: JournalPosition,
    event: &EffectEvent,
) -> Result<Digest, JournalError> {
    let mut hasher = Hasher::new();
    hasher.update(previous.head().as_bytes());
    hasher.update(&event.to_bytes().map_err(JournalError::Encoding)?);
    Ok(hasher.finalize())
}

/// `chain` over bytes the caller has already archived.
///
/// The two-step split exists because the shared frame step archives an event once
/// and then needs its head: calling [`chain`] there would encode the same value
/// twice and hand the head a second chance to disagree with the bytes that were
/// actually framed. Infallible, because the bytes are already in hand and nothing
/// can fail.
#[cfg(feature = "wire")]
pub(crate) fn chain_over_bytes(previous: &Digest, payload: &[u8]) -> Digest {
    let mut hasher = Hasher::new();
    hasher.update(previous.as_bytes());
    hasher.update(payload);
    hasher.finalize()
}

/// What the ladder allows next for `key`, given the entries committed so far.
///
/// Both shipped adapters keep a per-key index beside the sequence, so this
/// walk is what an index *means* rather than what an append *does*: the last
/// kind recorded for the key names the next allowed one, and a key never
/// seen is at the ladder's foot.
#[cfg(feature = "wire")]
pub(super) fn next_allowed_of(last: Option<EventKind>) -> Option<EventKind> {
    match last {
        Some(kind) => kind.next(),
        None => Some(EventKind::IntentAdmitted),
    }
}

/// Refuse an append whose tail is stale or whose kind does not follow what is
/// already committed for its key.
///
/// The two checks every adapter's append makes before it touches storage, once:
/// the memory journal and the file journal used to spell them out separately,
/// which is how two stores could come to disagree about what an out-of-order
/// append is. `last` is the last kind committed for the event's key.
///
/// # Errors
///
/// `JournalError::TailMismatch` when `expected_tail` is not `actual`, and
/// `JournalError::OutOfOrder` when the event's kind is not the next one the
/// ladder allows for its key.
#[cfg(feature = "wire")]
pub(super) fn check_append_order(
    expected_tail: JournalPosition,
    actual: JournalPosition,
    event: &EffectEvent,
    last: Option<EventKind>,
) -> Result<(), JournalError> {
    if expected_tail != actual {
        let refusal = Err(JournalError::TailMismatch {
            expected: expected_tail,
            actual,
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "check_append_order: the caller's view of the tail is stale");
        return refusal;
    }
    let attempted = event.kind();
    let expected = next_allowed_of(last);
    if expected != Some(attempted) {
        let refusal = Err(JournalError::OutOfOrder {
            key: Box::new(event.key()),
            expected,
            attempted,
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "check_append_order: the event does not follow the ladder");
        return refusal;
    }
    Ok(())
}

/// Recompute the chain over `entries` and report the first disagreement.
///
/// # Errors
///
/// [`ChainBreak`] naming the first entry whose recorded position is not the one
/// its events produce.
#[cfg(feature = "wire")]
pub fn verify_chain(entries: &[JournalEntry]) -> Result<JournalPosition, ChainBreak> {
    verify_chain_from(JournalPosition::genesis(), entries)
}

/// [`verify_chain`] over a chain that starts at `base` rather than at the genesis.
///
/// A successor's history begins at its predecessor's chain head, not at the
/// genesis: the seal frame it was opened from is not an event and is not in
/// `committed_entries`, so folding from the genesis would report a disagreement at
/// the very first entry. `base` is what the handle's
/// [`FileJournal::base`](crate::journal::FileJournal::base) reports, which is the
/// position its own scan already verified every frame against.
///
/// # Errors
///
/// [`ChainBreak`] naming the first entry whose recorded position is not the one
/// its events produce.
#[cfg(feature = "wire")]
pub fn verify_chain_from(
    base: JournalPosition,
    entries: &[JournalEntry],
) -> Result<JournalPosition, ChainBreak> {
    let mut position = base;
    for entry in entries {
        let sequence = position.sequence().saturating_add(1);
        let head = match chain(position, entry.event()) {
            Ok(head) => head,
            Err(_) => {
                let refusal = Err(ChainBreak::Unencodable { at: sequence });
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "verify_chain: returning an error to the caller");
                return refusal;
            }
        };
        let recomputed = JournalPosition { sequence, head };
        // The recorded head may have been written by an adversary, so the
        // comparison is the named constant-time one rather than an ordering.
        let recorded = entry.position();
        if recorded.sequence() != sequence || !recorded.head().ct_eq(&head) {
            let refusal = Err(ChainBreak::Disagreement {
                at: sequence,
                recorded,
                recomputed,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "verify_chain: returning an error to the caller");
            return refusal;
        }
        position = recomputed;
    }
    Ok(position)
}

/// An in-memory journal, for tests and for runs whose effects never leave the
/// process.
///
/// It retains at most [`MAX_JOURNAL_EVENTS`] events and refuses the next append
/// without changing committed state. It reports [`DurabilityPromise::Ephemeral`]
/// and therefore cannot be the record behind an external handoff. That is the
/// whole reason it exists as a named type rather than as a default: a caller
/// that reaches for it gets a refusal at the boundary instead of a green test
/// that means nothing.
#[derive(Debug, Clone)]
pub struct MemoryJournal {
    /// Every committed entry, in append order.
    committed: Vec<JournalEntry>,
    /// The last kind recorded per key, the append fence's index.
    ///
    /// Without it every append would walk the whole sequence to find the
    /// key's last event, and an append would cost the journal's own length:
    /// measured at 521 ns per append against an empty journal and 50,886 ns
    /// against 8,000 prior attempts, which is the O(n²) a long-lived bot
    /// would grind against. The index makes the ladder check constant. Only an
    /// append reads it, and appends chain through the wire encoding.
    #[cfg(feature = "wire")]
    ladder: std::collections::HashMap<EffectKey, EventKind>,
    /// Latest durable outcome and its exact position per attempt.
    outcomes: std::collections::HashMap<EffectKey, (JournalPosition, EffectEvidence)>,
    /// Encoded frame bytes admitted, kept in step with `committed`.
    committed_bytes: u64,
}

impl Default for MemoryJournal {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryJournal {
    /// An empty journal at the genesis.
    #[must_use]
    pub fn new() -> Self {
        Self {
            committed: Vec::new(),
            #[cfg(feature = "wire")]
            ladder: std::collections::HashMap::new(),
            outcomes: std::collections::HashMap::new(),
            committed_bytes: 0,
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
    #[cfg(feature = "wire")]
    pub fn verify(&self) -> Result<JournalPosition, ChainBreak> {
        verify_chain(&self.committed)
    }

    /// What the journal has learned about each attempt.
    #[must_use]
    pub fn recover(&self) -> Recovered {
        recover(self.events())
    }

    /// The sequence the first held entry follows, read from that entry: an
    /// in-memory journal never continues from a predecessor, so this is genesis.
    fn chain_base(&self) -> u64 {
        match self.committed.first() {
            Some(entry) => entry.position().sequence().saturating_sub(1),
            None => 0,
        }
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

    committed_reads!();

    /// The same admission test the file adapter makes, so a handoff the file
    /// would refuse before its first rung is refused here too, not after it.
    fn reserve_handoff_capacity(&self, rungs: u64) -> Result<(), JournalError> {
        super::bound_events(self.committed.len(), rungs)
    }

    /// This journal's own counts against the shipped ceilings, reporting that it
    /// does not continue.
    ///
    /// Measured rather than zeroed: an in-memory journal refuses at exactly the
    /// same ceilings the file adapter does, so a caller that wants to know how
    /// close it is is asking a real question and gets a real number. The
    /// continuation answer is `false`, which is the whole reason an ephemeral
    /// run behaves exactly as it did before continuation existed.
    fn continuation_watermark(&self) -> Result<ContinuationWatermark, JournalError> {
        let events = u64::saturating_from(self.committed.len());
        let limit = u64::saturating_from(MAX_JOURNAL_EVENTS);
        Ok(ContinuationWatermark::inert(
            events,
            limit,
            self.committed_bytes,
            MAX_JOURNAL_BYTES,
        ))
    }

    #[cfg(feature = "wire")]
    fn compare_and_append(
        &mut self,
        expected_tail: JournalPosition,
        event: &EffectEvent,
    ) -> Result<DurableAck, JournalError> {
        let actual = self.tail();
        let key = event.key();
        check_append_order(expected_tail, actual, event, self.ladder.get(&key).copied())?;
        super::bound_events(self.committed.len(), 1)?;
        let payload_len =
            u64::saturating_from(event.to_bytes().map_err(JournalError::Encoding)?.len());
        let requested_bytes = self
            .committed_bytes
            .saturating_add(payload_len)
            .saturating_add(36);
        if requested_bytes > MAX_JOURNAL_BYTES {
            let refusal = Err(JournalError::CapacityExceeded {
                resource: JournalLimitKind::Bytes,
                limit: MAX_JOURNAL_BYTES,
                requested: requested_bytes,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "compare_and_append: returning an error to the caller");
            return refusal;
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
        self.committed_bytes = requested_bytes;
        self.ladder.insert(key, event.kind());
        if let EffectEvent::OutcomeObserved { evidence, .. } = *event {
            self.outcomes.insert(key, (position, evidence));
        }
        Ok(DurableAck::new(position, self.durability()))
    }
}
