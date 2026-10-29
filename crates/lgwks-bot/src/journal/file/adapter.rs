//! The EffectJournal implementation for FileJournal, and the replay cursor reset.

use super::{
    ContinuationWatermark, DurabilityPromise, DurableAck, EffectEvent, EffectEvidence,
    EffectJournal, File, FileJournal, JournalEntry, JournalError, JournalPosition,
    MAX_JOURNAL_BYTES, MAX_JOURNAL_EVENTS, SaturatingFrom,
};

impl FileJournal {
    /// The sequence this file's first held entry follows: genesis for a fresh
    /// journal, the seal frame's for a successor, whose sequences continue from
    /// the predecessor's.
    fn chain_base(&self) -> u64 {
        self.chain_from.sequence()
    }
}

impl EffectJournal for FileJournal {
    fn durability(&self) -> DurabilityPromise {
        DurabilityPromise::ProcessCrash
    }

    fn tail(&self) -> JournalPosition {
        self.position
    }

    committed_reads!();

    /// Reserve room for the whole three-rung handoff before any of it is
    /// written, so an attempt can never be left admitted and unprepared to
    /// settle. The ceiling is the same [`MAX_JOURNAL_EVENTS`] the appends
    /// enforce, checked here against the count this serialized path reads.
    fn reserve_handoff_capacity(&self, rungs: u64) -> Result<(), JournalError> {
        self.bound_events(rungs)
    }

    /// How much of each ceiling this journal has used, and whether it continues.
    ///
    /// Read from the handle's own committed count and acknowledged byte length,
    /// which are the two numbers the append fence already keeps exact. There is no
    /// third source and no rounding: the watermark falls where the constants say,
    /// and a caller reading this can compare it against the same ceiling every
    /// other refusal names.
    fn continuation_watermark(&self) -> Result<ContinuationWatermark, JournalError> {
        let events = u64::saturating_from(self.committed.len());
        let limits = (u64::saturating_from(MAX_JOURNAL_EVENTS), MAX_JOURNAL_BYTES);
        let watermark = ContinuationWatermark::measured(
            events,
            limits.0,
            self.disk_len,
            limits.1,
            self.continuation,
        );
        Ok(match self.continuation_continues() {
            true => watermark,
            false => ContinuationWatermark::inert(events, limits.0, self.disk_len, limits.1),
        })
    }

    /// Seal this journal and hand back the successor opened from the checkpoint.
    ///
    /// One ordered step on the storage owner, because a continuation writes two
    /// files and the fence that guards the predecessor's length has to run on the
    /// thread that holds it. The step is: the seal frame into this journal, the
    /// same bytes into the successor, the successor's flush, its directory entry's
    /// flush, and then the batch's one flush over the predecessor's seal — which
    /// is the order the crash table in [`super::super::continuation`] reasons about, in
    /// that order.
    ///
    /// `None` for a journal opened without a lifecycle, which is what leaves every
    /// existing behaviour of this adapter unchanged.
    ///
    /// # Errors
    ///
    /// [`JournalError::Superseded`] when this handle already sealed its journal,
    /// [`JournalError::CapacityExceeded`] when the carried state is past a declared
    /// bound or the frame is past the shared ceiling,
    /// [`JournalError::ContinuationPaused`] when an armed fault injector stopped the
    /// seal, and [`JournalError::OutcomeUnknown`] when the device's answer was lost
    /// after bytes may have moved.
    fn continue_as_new(&mut self) -> Result<Option<Box<dyn EffectJournal>>, JournalError> {
        let opened = self.continue_as_file()?;
        Ok(opened.map(|journal| {
            let boxed: Box<dyn EffectJournal> = Box::new(journal);
            boxed
        }))
    }

    /// Append one event at `expected_tail`, or refuse.
    ///
    /// The order of the checks is the order the promises are made: the fence
    /// first, then the ladder, then the disk. Nothing is written until every
    /// check that can refuse in memory has passed, and the acknowledgment is
    /// minted only after `sync_all` returns, which is the whole reason this
    /// adapter may promise `ProcessCrash`.
    ///
    /// # Errors
    ///
    /// Every [`JournalError`] the fence, the ladder or the disk can produce.
    fn compare_and_append(
        &mut self,
        expected_tail: JournalPosition,
        event: &EffectEvent,
    ) -> Result<DurableAck, JournalError> {
        // The staleness fence, the ladder, the bounds and the framing are the
        // two doors' shared decision, so they are made once. The file's byte
        // length is checked against this write on the thread that holds it.
        let (position, frame) = self.prepare_append(expected_tail, event)?;

        // Through the shared door before the acknowledgment: after this
        // returns, the bytes are through the file system, and a kill of this
        // process cannot take the fact back out of the file.
        self.write_and_sync(&frame)?;

        self.accept(event, position, frame.len());
        Ok(DurableAck::new(position, self.durability()))
    }

    /// The batch as this file's one write and one flush, all or nothing.
    fn append_batch(&mut self, events: &[EffectEvent]) -> Result<Vec<DurableAck>, JournalError> {
        self.compare_and_append_all(events)
    }

    /// The same append, waited for rather than sat through.
    ///
    /// The decision — the fence, the ladder and the bounds — is the
    /// synchronous form's; only the wait differs. The
    /// caller may go away while the bytes are moving, and the storage owner
    /// latches its poison when it does, so a handle that lost its waiter
    /// refuses the next append rather than continue from a view that may be
    /// behind the file.
    fn compare_and_append_async<'a>(
        &'a mut self,
        expected_tail: JournalPosition,
        event: &'a EffectEvent,
    ) -> crate::BoxFuture<'a, Result<DurableAck, JournalError>> {
        Box::pin(async move {
            let (position, frame) = self.prepare_append(expected_tail, event)?;
            self.write_and_sync_async(frame.clone()).await?;
            self.accept(event, position, frame.len());
            Ok(DurableAck::new(position, self.durability()))
        })
    }

    super::super::confirm_outcome_by!(
        /// Attest that an outcome this journal committed is durable.
        ///
        /// The receipt names a position; this journal answers by finding that
        /// position among its own committed entries and checking it is exactly
        /// the named outcome. A receipt this journal cannot find names a
        /// different append than the one being settled, which is
        /// [`JournalError::ReceiptMismatch`]; a grade above what this journal can
        /// promise is [`JournalError::ReceiptUnavailable`].
        ///
        /// # Errors
        ///
        /// [`JournalError::ReceiptUnavailable`] when `required` exceeds this
        /// journal's promise; [`JournalError::ReceiptMismatch`] when the position
        /// does not name this outcome as committed.
        self, key, evidence, position, required, {
        if !self.durability().meets(required) {
            let refusal = Err(JournalError::ReceiptUnavailable { required });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "confirm_outcome: returning an error to the caller");
            return refusal;
        }
        let settled = self
            .committed
            .iter()
            .find(|entry| entry.position() == position)
            .map(JournalEntry::event)
            .copied();
        match settled {
            Some(EffectEvent::OutcomeObserved {
                key: observed_key,
                evidence: observed_evidence,
            }) if observed_key == key && observed_evidence == evidence => {
                Ok(DurableAck::new(position, self.durability()))
            }
            _ => Err(JournalError::ReceiptMismatch {
                expected: position,
                actual: self.tail(),
            }),
        }
    });
}

/// Cursor reset for the replay, spelled on the type so the call reads as
/// intent rather than as a magic constant.
pub(super) trait ReplayCursor {
    /// Move the read cursor back to the first byte of the file.
    ///
    /// # Errors
    ///
    /// [`JournalError::Storage`] when the seek is refused.
    fn seek_read_zero(&mut self) -> Result<(), JournalError>;
}

impl ReplayCursor for File {
    fn seek_read_zero(&mut self) -> Result<(), JournalError> {
        use std::io::Seek;
        use std::io::SeekFrom;
        self.seek(SeekFrom::Start(0))
            .map(|_: u64| ())
            .map_err(JournalError::Storage)
    }
}
