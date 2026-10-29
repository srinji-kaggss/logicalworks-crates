//! What the shipped journal adapters share: the event ceiling and the trait
//! methods both answer the same way.
//!
//! One definition of each, so the memory and file adapters cannot disagree on
//! where the ceiling is or on how a committed position reads back. Declared
//! with `#[macro_use]` ahead of the adapter modules, so the macros are in scope
//! where each `impl EffectJournal` invokes them.

use super::frame::SaturatingFrom;
use super::{JournalError, JournalLimitKind, MAX_JOURNAL_EVENTS};

/// Refuse taking a journal that holds `held` events past [`MAX_JOURNAL_EVENTS`]
/// by `additional` more.
///
/// One check for both adapters and both questions they answer (may this append
/// land, and may this handoff begin), so the two cannot disagree on where the
/// ceiling is.
///
/// # Errors
///
/// [`JournalError::CapacityExceeded`] when the total would be over the bound.
/// The history is not compacted to make room, so nothing acknowledged is
/// evicted to admit more work.
pub(crate) fn bound_events(held: usize, additional: u64) -> Result<(), JournalError> {
    let requested = u64::saturating_from(held).saturating_add(additional);
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

/// The committed-history reads both adapters answer from their own
/// `committed: Vec<JournalEntry>`, `outcomes` index, `events()` and
/// `chain_base()` (the sequence the first held entry follows).
///
/// Copied out rather than borrowed, because the trait hands the caller a value
/// it owns: `recover` folds over the sequence, and a journal that handed back a
/// borrow would tie the fold to the journal's own lifetime. A position is
/// indexed from the chain's base rather than from genesis: a continued file's
/// first entry follows its seal frame, not sequence zero, and indexing it from
/// genesis read every acknowledged position back as absent.
macro_rules! committed_reads {
    () => {
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
            let Some(index) = position
                .sequence()
                .checked_sub(self.chain_base())
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
    };
}

/// [`EffectJournal::confirm_outcome`](super::EffectJournal::confirm_outcome) as
/// an adapter that can attest writes it: the trait's signature once, with the
/// adapter's documentation, receiver, argument names and body.
///
/// The receiver is passed in (`self`) so the body, written at the adapter, can
/// name it. Every in-crate journal that attests receipts goes through here, so
/// the shape of the question is fixed in one place and each adapter states only
/// how it answers.
#[cfg(feature = "wire")]
macro_rules! confirm_outcome_by {
    (
        $(#[$attr:meta])*
        $this:ident,
        $key:ident,
        $evidence:ident,
        $position:ident,
        $required:ident,
        $body:block
    ) => {
        $(#[$attr])*
        fn confirm_outcome(
            &mut $this,
            $key: crate::effect::EffectKey,
            $evidence: EffectEvidence,
            $position: JournalPosition,
            $required: DurabilityPromise,
        ) -> Result<DurableAck, JournalError> $body
    };
}
#[cfg(feature = "wire")]
pub(crate) use confirm_outcome_by;
