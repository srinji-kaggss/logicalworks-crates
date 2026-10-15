//! The write path: the fence, framing, the byte and event bounds, and the synced write both append paths share.

use super::{
    AttemptStatus, EffectEvent, EffectJournal, EventKind, FileJournal, HashMap, JournalEntry,
    JournalError, JournalLimitKind, JournalPosition, MAX_FRAME_BYTES, MAX_JOURNAL_BYTES, PathBuf,
    SaturatingFrom, SettledAttempt, UnresolvedAttempt, check_append_order, commit, successor_path,
    write_refusal,
};

impl FileJournal {
    /// The one door both append paths take before they are allowed to write.
    ///
    /// A handle whose own write failed is stale by definition: bytes may be on
    /// the disk that no acknowledgment names, and only a reopen replays the
    /// truth. A file that is no longer exactly the acknowledged prefix means
    /// something else moved it, and an append from here would branch the chain
    /// rather than extend it.
    ///
    /// The length half here is a precondition, not the guarantee: it is a
    /// `stat` against a file another writer may still be moving. The
    /// authoritative check is inside the storage owner's ordered step, where it
    /// cannot be overtaken by another append. This one exists so a stale
    /// handle is refused before it spends anything, and so an append with no
    /// frames to write is still answered for itself.
    ///
    /// # Errors
    ///
    /// [`JournalError::Storage`] when this handle is stale and must be reopened.
    pub(super) fn fence(&self) -> Result<(), JournalError> {
        self.refuse_sealed()?;
        if self.storage.poisoned() {
            let refusal = Err(JournalError::Storage(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "a previous append failed while writing; the file may hold \
             unacknowledged bytes, reopen to replay",
            )));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "fence: returning an error to the caller");
            return refusal;
        }
        if self.view.len()? != self.disk_len {
            let refusal = Err(JournalError::Storage(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the journal file moved under this controller; reopen before appending",
            )));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "fence: returning an error to the caller");
            return refusal;
        }
        Ok(())
    }

    /// Refuse every write on a handle that sealed its own journal.
    ///
    /// Asked **first**, ahead of the ladder and the length fence, because a sealed
    /// journal has no ladder answer left to give: its successor is the journal,
    /// and a caller that got `OutOfOrder` or `AttemptAlreadyWalked` instead would
    /// be told about a fact about *this* file when the truth is that this file is
    /// not the one to write to any more. The other refusals still name what they
    /// found, and they are the right answer on a handle that has not sealed.
    pub(super) fn refuse_sealed(&self) -> Result<(), JournalError> {
        let Some(next) = self.sealed_by.as_ref() else {
            return Ok(());
        };
        let refusal = Err(JournalError::Superseded {
            path: next.display().to_string(),
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "refuse_sealed: this handle sealed its journal and is read-only");
        refusal
    }

    /// Frame one event: its length prefix, its archived bytes, and the chain
    /// head it commits to.
    ///
    /// The head is stored with the frame rather than recomputed on faith at
    /// read time. A frame over [`MAX_FRAME_BYTES`] is refused here rather than
    /// written, because the bytes behind it may be acknowledged.
    ///
    /// # Errors
    ///
    /// [`JournalError::Exhausted`] when the position cannot advance,
    /// [`JournalError::Encoding`] when the event cannot be archived, and
    /// [`JournalError::Storage`] when the frame is over the bound.
    pub(super) fn frame(
        &self,
        event: &EffectEvent,
        from: JournalPosition,
    ) -> Result<(JournalPosition, Vec<u8>), JournalError> {
        let sequence = from
            .sequence()
            .checked_add(1)
            .ok_or(JournalError::Exhausted)?;
        // Archive, bound, chain and lay out — the shared step, which the run store
        // takes too. Only the event's own archiving and its position-chaining head
        // are this journal's, and both arrive as the closures they are.
        let (framed, head) = super::super::frame::frame_record(
            event,
            &from.head,
            MAX_FRAME_BYTES,
            |event| {
                event
                    .to_bytes()
                    .map(|bytes| bytes.as_ref().to_vec())
                    .map_err(JournalError::Encoding)
            },
            |_event, previous, archived| super::super::chain_over_bytes(previous, archived),
            |_len| {
                JournalError::Storage(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "the event exceeds this journal's frame bound",
                ))
            },
        )?;
        Ok((JournalPosition { sequence, head }, framed))
    }

    /// Refuse an append that would take the file past [`MAX_JOURNAL_BYTES`].
    ///
    /// The file is refused rather than compacted, so no acknowledged evidence
    /// is evicted to make room.
    ///
    /// # Errors
    ///
    /// [`JournalError::CapacityExceeded`] when the staged write is over the
    /// bound.
    pub(super) fn bound_bytes(&self, staged: usize) -> Result<(), JournalError> {
        let requested = self.disk_len.saturating_add(u64::saturating_from(staged));
        if requested > MAX_JOURNAL_BYTES {
            let refusal = Err(JournalError::CapacityExceeded {
                resource: JournalLimitKind::Bytes,
                limit: MAX_JOURNAL_BYTES,
                requested,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "bound_bytes: returning an error to the caller");
            return refusal;
        }
        Ok(())
    }

    /// Hand the frames to the storage owner, which is where the write and the
    /// sync happen, and wait for it to answer.
    ///
    /// A failure of either half is [`JournalError::OutcomeUnknown`], because a
    /// prefix of the bytes, or all of them, may be on the disk with only the
    /// reply lost — never a bare [`JournalError::Storage`], which would claim
    /// the journal is unchanged when the write may have landed. The owner
    /// latches the poison, so this handle refuses every later append until a
    /// reopen replays the truth.
    ///
    /// # Errors
    ///
    /// [`JournalError::OutcomeUnknown`] from either the write or the sync.
    pub(super) fn write_and_sync(&mut self, frames: &[u8]) -> Result<(), JournalError> {
        self.write_and_sync_blocking(frames)
    }

    /// Whether this handle continues at its declared trigger points.
    ///
    /// The `continuing` flag and the policy are one fact read twice, so they are
    /// kept in step at construction and this is the only place that compares them.
    /// A handle opened by [`FileJournal::open`] carries the declared policy and
    /// `false`, which is why it answers `false` rather than consulting the policy
    /// it never asked for.
    pub(super) fn continuation_continues(&self) -> bool {
        self.continuing
    }

    /// Where a continuation of this handle would go.
    pub(super) fn successor(&self) -> PathBuf {
        successor_path(&self.path)
    }

    /// The fold every continuation carries, read from this handle's own history.
    ///
    /// Two lists, and the split between them is the whole design: the **unresolved**
    /// attempts in full identity, because dropping one would be the one thing this
    /// mechanism must never do, and one folded record per **action** for everything
    /// already settled, because `AttemptId` is monotonic per action so that one
    /// record answers "was this walked?" for every attempt behind it.
    pub(super) fn carried(&self) -> (Vec<SettledAttempt>, Vec<UnresolvedAttempt>) {
        let recovered = self.recover();
        let mut settled: Vec<SettledAttempt> = Vec::new();
        let mut unresolved: Vec<UnresolvedAttempt> = Vec::new();
        // One record per *action*, kept in first-seen order, which is what bounds
        // the carry by the shape of a bot rather than by how long it has run. An
        // action already folded is replaced in place, so the vector's order stays
        // the admission order a recovery fold reads in and the record is the
        // latest attempt of that action rather than the first one seen.
        let mut folded_at: HashMap<crate::effect::ActionId, usize> = HashMap::new();
        for attempt in recovered.attempts() {
            match attempt.status() {
                AttemptStatus::Prepared => {
                    unresolved.push(UnresolvedAttempt::new(
                        attempt.key(),
                        EventKind::IntentAdmitted,
                    ));
                }
                AttemptStatus::OutcomeUnknown => {
                    unresolved.push(UnresolvedAttempt::new(
                        attempt.key(),
                        EventKind::DispatchPrepared,
                    ));
                }
                status => {
                    let rung = if recovered
                        .history(attempt.key())
                        .last()
                        .is_some_and(|change| {
                            change.to() == AttemptStatus::Verified
                                || change.to() == AttemptStatus::VerificationFailed
                        }) {
                        EventKind::Verified
                    } else {
                        EventKind::OutcomeObserved
                    };
                    let record =
                        SettledAttempt::new(attempt.key(), rung, status, attempt.verification());
                    match folded_at.get(&record.action()) {
                        Some(at) => {
                            if let Some(slot) = settled.get_mut(*at) {
                                *slot = record;
                            }
                        }
                        None => {
                            let _ = folded_at.insert(record.action(), settled.len());
                            settled.push(record);
                        }
                    }
                }
            }
        }
        (settled, unresolved)
    }

    /// [`Self::write_and_sync`] for the door a task awaits rather than sits through.
    ///
    /// # Errors
    ///
    /// As [`Self::write_and_sync`].
    pub(super) fn write_and_sync_async<'a>(
        &'a mut self,
        frames: Vec<u8>,
    ) -> crate::BoxFuture<'a, Result<(), JournalError>> {
        let expected = self.disk_len;
        Box::pin(async move {
            self.storage
                .submit_async(move |file, _state| commit(file, expected, &frames))
                .await
                .map(|_| ())
                .map_err(write_refusal)
        })
    }

    /// The synchronous door: perform the append on the owner thread and block for
    /// its answer.
    ///
    /// # Errors
    ///
    /// [`JournalError::OutcomeUnknown`] from the write or the sync, and
    /// [`JournalError::Storage`] when the owner refuses a request outright.
    pub(super) fn write_and_sync_blocking(&self, frames: &[u8]) -> Result<(), JournalError> {
        let staged = frames.to_vec();
        let expected = self.disk_len;
        self.storage
            .submit(move |file, _state| commit(file, expected, &staged))
            .map(|_| ())
            .map_err(write_refusal)
    }

    /// Fold a committed frame into this handle's view of the journal.
    ///
    /// The acknowledgment is minted from the position the frame was chained at,
    /// so the two cannot disagree.
    pub(super) fn accept(
        &mut self,
        event: &EffectEvent,
        position: JournalPosition,
        frame_len: usize,
    ) {
        self.committed.push(JournalEntry::new(position, *event));
        self.position = position;
        self.ladder.insert(event.key(), event.kind());
        if let EffectEvent::OutcomeObserved { key, evidence } = *event {
            self.outcomes.insert(key, (position, evidence));
        }
        self.disk_len = self
            .disk_len
            .saturating_add(u64::saturating_from(frame_len));
    }

    /// Everything one append checks and frames before the disk is touched.
    ///
    /// The synchronous and asynchronous forms differ only in how they wait for
    /// the storage owner, so everything that decides *whether* to append lives
    /// here and is reached by both: the tail fence, the ladder, the event
    /// bound, the frame, and the byte bound. A check that exists once cannot
    /// drift between the two doors.
    ///
    /// # Errors
    ///
    /// [`JournalError::TailMismatch`] when the caller's view of the tail is
    /// not this journal's, [`JournalError::OutOfOrder`] when the rung cannot
    /// follow what is committed for the key, [`JournalError::Storage`] when
    /// this handle is stale or the frame is over the bound, and
    /// [`JournalError::CapacityExceeded`] when the append is over an event or
    /// byte ceiling. Nothing is written by any of them.
    pub(super) fn prepare_append(
        &self,
        expected_tail: JournalPosition,
        event: &EffectEvent,
    ) -> Result<(JournalPosition, Vec<u8>), JournalError> {
        self.refuse_sealed()?;
        let actual = self.tail();
        check_append_order(
            expected_tail,
            actual,
            event,
            self.ladder.get(&event.key()).copied(),
        )?;
        self.refuse_walked(event)?;
        self.fence()?;
        self.bound_events(1)?;
        let (position, frame) = self.frame(event, actual)?;
        self.bound_bytes(frame.len())?;
        Ok((position, frame))
    }
}
