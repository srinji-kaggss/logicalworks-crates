//! Opening a journal: the constructors, replay on open, and the read-side accessors.

use super::{
    BufReader, Continuation, ContinuationPolicy, EffectEvent, EventKind, FileJournal, FileView,
    HashMap, JournalEntry, JournalError, JournalLimitKind, JournalPosition, MAX_GENERATION_WALK,
    MAX_JOURNAL_BYTES, MAX_JOURNAL_EVENTS, OpenKind, OpenOptions, Path, PathBuf, Recovered, Replay,
    ReplayCursor, SaturatingFrom, ScanStop, Scanned, SealPause, StorageOwner, ladder_hold,
    recover_continued, resolve_ambiguous_tail, scan, successor_is_complete, successor_path,
};

impl FileJournal {
    /// Open the journal at `path`, creating the file when it does not exist
    /// and replaying it when it does.
    ///
    /// The exclusive advisory file lock is taken before a single byte is
    /// read: the replay and the torn-tail repair belong to the lock's owner
    /// alone, and a second opener is refused with
    /// [`JournalError::Locked`] rather than allowed to scan or truncate a
    /// file someone else is appending to. The lock is held for the handle's
    /// whole life and released by the kernel when the handle closes or the
    /// process dies.
    ///
    /// A torn tail — an append a killed writer never finished — is truncated
    /// back to the last whole frame, because it was never acknowledged. A
    /// lying frame — undecodable bytes, a length no write of this journal
    /// can produce, or a head that does not follow — is refused, because it
    /// may have been. The distinction is reported: after a repair,
    /// [`FileJournal::torn_tail_repaired`] is `true`.
    ///
    /// This handle does **not** continue past its ceilings; use
    /// [`FileJournal::open_continuing`] for a run that is meant to be
    /// unattended. That is a policy rather than a default, so every existing
    /// behaviour of this constructor is unchanged.
    ///
    /// # Errors
    ///
    /// [`JournalError::Locked`] when another writer holds the file;
    /// [`JournalError::Storage`] when the file cannot be opened, read or
    /// repaired; [`JournalError::Corrupt`] when committed bytes are refused;
    /// [`JournalError::Superseded`] when this file has been continued and its
    /// successor is the authoritative journal; and
    /// [`JournalError::CapacityExceeded`] when the complete history exceeds
    /// [`MAX_JOURNAL_BYTES`] or [`MAX_JOURNAL_EVENTS`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        Self::open_impl(path.as_ref(), OpenKind::Plain)
    }

    /// Open a journal that continues at the declared watermark.
    ///
    /// The lifecycle an unattended run needs, and the same journal in every other
    /// respect: the same frame grammar, the same chain, the same storage-owner
    /// thread, the same refusals. What it adds is that
    /// [`EffectJournal::continue_as_new`](crate::journal::EffectJournal::continue_as_new) seals this journal at
    /// [`CONTINUATION_WATERMARK_NUMERATOR`](super::super::continuation::CONTINUATION_WATERMARK_NUMERATOR)
    /// of either ceiling and hands back the
    /// successor, so a bot that runs for weeks keeps acting instead of stopping
    /// safely at a ceiling.
    ///
    /// The watermark leaves the remaining fifth of each ceiling as settlement
    /// headroom, so an attempt already handed to the outside world can always
    /// record its outcome — which is #143's "near-full settlement-capacity
    /// reservation", the half that had no API behind it.
    ///
    /// A journal whose sealed successor is complete refuses itself with
    /// [`JournalError::Superseded`] rather than reporting an empty run, and
    /// [`FileJournal::open_active`] follows the chain for a caller that only has
    /// the original path.
    ///
    /// # Errors
    ///
    /// Whatever [`FileJournal::open`](crate::journal::FileJournal::open) reports.
    pub fn open_continuing(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        Self::open_impl(
            path.as_ref(),
            OpenKind::Continuing(ContinuationPolicy::declared()),
        )
    }

    /// Open a journal that continues at `policy`'s two trigger points.
    ///
    /// The same lifecycle as [`FileJournal::open_continuing`] with the trigger
    /// stated rather than derived, for a host whose retention budget is not this
    /// crate's default and for a simulation that has to reach a hundred
    /// continuations inside a test's wall clock. A policy at or past a ceiling is
    /// refused by [`ContinuationPolicy::new`] rather than clamped, so this
    /// constructor cannot open a journal whose trigger is unreachable.
    ///
    /// # Errors
    ///
    /// Whatever [`FileJournal::open`](crate::journal::FileJournal::open) reports.
    pub fn open_continuing_with(
        path: impl AsRef<Path>,
        policy: ContinuationPolicy,
    ) -> Result<Self, JournalError> {
        Self::open_impl(path.as_ref(), OpenKind::Continuing(policy))
    }

    /// Open the journal at `path`, following its continuations to the live one.
    ///
    /// What a restarting controller wants: it holds the path its host recorded, and
    /// the file at that path may be a hundred sealed predecessors old. The walk
    /// follows one [`JournalError::Superseded`] at a time and stops at the first
    /// journal that is not sealed, and it is bounded by
    /// `MAX_GENERATION_WALK` rather than by a loop that could follow a
    /// directory tree somebody else is generating.
    ///
    /// # Errors
    ///
    /// Whatever the last journal reports, and [`JournalError::Superseded`] naming
    /// the next successor when the walk ran out of generations.
    pub fn open_active(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        let mut here = path.as_ref().to_path_buf();
        for _ in 0..MAX_GENERATION_WALK {
            match Self::open_impl(&here, OpenKind::Continuing(ContinuationPolicy::declared())) {
                Ok(journal) => return Ok(journal),
                Err(JournalError::Superseded { path: next }) => here = PathBuf::from(next),
                Err(other) => {
                    let refusal = Err(other);
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "FileJournal::open_active: a generation on the walk refused to open");
                    return refusal;
                }
            }
        }
        let next = successor_path(&here).display().to_string();
        let refusal = Err(JournalError::Superseded { path: next });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "open_active: the generation walk ran out before reaching a live journal");
        refusal
    }

    /// Arm the next continuation to stop at `boundary`.
    ///
    /// A fault injector, and the reason it is public rather than hidden behind
    /// `cfg(test)` is the one [`FileJournal::open_with_stalled_storage`] gives: a
    /// continuation has four places where bytes become durable in order, and "which
    /// journal is authoritative after the disk dies at the third one" is a question
    /// about a real process. A continuation armed to stop refuses with
    /// [`JournalError::ContinuationPaused`] naming the boundary it reached,
    /// *before* the next byte moves, so a harness that kills the process on that
    /// refusal kills it exactly at the boundary.
    ///
    /// [`SealPause::all`] sweeps the whole set, so a boundary added later is swept
    /// the day it exists rather than the day somebody remembers.
    pub fn arm_continuation_pause(&mut self, boundary: SealPause) {
        self.pause = Some(boundary);
    }

    /// Which generation this journal is. One for a journal that never continued.
    ///
    /// Counted from this file's own name rather than read out of its checkpoint,
    /// so it answers the same for a predecessor that carries no checkpoint at all
    /// — and so two places never hold a generation that can disagree.
    #[must_use]
    pub fn generation(&self) -> u64 {
        super::super::continuation::generation_of(&self.path)
    }

    /// The predecessor's sealed checkpoint this journal's chain starts from, when
    /// this journal is a successor.
    #[must_use]
    pub fn checkpoint(&self) -> Option<&Continuation> {
        self.carried.as_ref()
    }

    /// This journal's own seal, when it has sealed itself.
    #[must_use]
    pub fn own_seal(&self) -> Option<&Continuation> {
        self.seal.as_ref()
    }

    /// The predecessor's chain head, which is the position this journal's first
    /// frame chains from.
    #[must_use]
    pub const fn base(&self) -> JournalPosition {
        self.base
    }

    /// The position this journal's committed events chain from.
    ///
    /// What [`verify_chain_from`](crate::journal::verify_chain_from) takes for this
    /// journal: the genesis for a base journal, and the seal frame's own position for
    /// a successor. Both are facts about the file rather than about a caller's view of
    /// it, which is what makes a successor's chain checkable at all.
    #[must_use]
    pub const fn chain_from(&self) -> JournalPosition {
        self.chain_from
    }

    /// Where this handle's own seal sent the authoritative journal.
    #[must_use]
    pub fn sealed_by(&self) -> Option<&Path> {
        self.sealed_by.as_deref()
    }

    /// The successor of this journal's path.
    ///
    /// The same name the checkpoint carries and the walk follows, so a caller
    /// predicting where a continuation will go and a reader following one to its
    /// end cannot disagree.
    #[must_use]
    pub fn successor_path(&self) -> PathBuf {
        successor_path(&self.path)
    }

    /// Open, with the device's answering behaviour chosen by the caller.
    ///
    /// One implementation for every constructor: a stalled device is a property of
    /// the storage owner and a continuing journal is a declared policy, and neither
    /// is a second way to open a journal.
    ///
    /// # Errors
    ///
    /// Whatever the file, the lock, the scan or the authority rule reports.
    pub(super) fn open_impl(path: &Path, kind: OpenKind) -> Result<Self, JournalError> {
        let path = path.to_path_buf();
        super::super::continuation::refuse_ambiguous_base(&path)?;
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&path)
            .map_err(JournalError::Storage)?;
        // Fail closed: a filesystem that cannot supply the lock refuses the
        // open rather than getting an unfenced journal. The lock is advisory
        // and binds writers through this constructor; see
        // [`JournalError::Locked`] for the exact reach of that promise.
        file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => JournalError::Locked {
                path: path.display().to_string(),
            },
            std::fs::TryLockError::Error(io) => JournalError::Storage(io),
        })?;

        let file_len = file.metadata().map_err(JournalError::Storage)?.len();
        if file_len > MAX_JOURNAL_BYTES {
            let refusal = Err(JournalError::CapacityExceeded {
                resource: JournalLimitKind::Bytes,
                limit: MAX_JOURNAL_BYTES,
                requested: file_len,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "open_impl: returning an error to the caller");
            return refusal;
        }

        file.seek_read_zero()?;
        let mut reader = BufReader::new(&mut file);
        let scanned = scan(&mut reader, MAX_JOURNAL_EVENTS)?;
        drop(reader);
        let Scanned {
            entries,
            carried,
            seal,
            chain_from,
            tail,
            stop,
        } = scanned;

        // The authority rule, asked before anything is repaired: a journal whose
        // successor is complete is not this run's journal, and repairing its tail
        // would be writing to a file this process does not own.
        if let Some(checkpoint) = seal.as_ref() {
            // The successor's name is a property of *this* file, read from the
            // path the caller already opened rather than from the checkpoint: that
            // is what lets the seal frame be byte-identical in both files.
            let next = successor_path(&path);
            if successor_is_complete(&next, checkpoint) {
                let refusal = Err(JournalError::Superseded {
                    path: next.display().to_string(),
                });
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "open_impl: this journal is sealed and its successor is authoritative");
                return refusal;
            }
        }

        let (acked_len, torn_tail_repaired) = match stop {
            ScanStop::Complete(len) => (len, false),
            ScanStop::Torn(offset) => {
                // The interrupted append was never acknowledged; taking it
                // back restores the file to the prefix every acknowledgment
                // still names.
                file.set_len(offset).map_err(JournalError::Storage)?;
                file.sync_all().map_err(JournalError::Storage)?;
                (offset, true)
            }
            ScanStop::AmbiguousTail { offset } => {
                let index = u64::saturating_from(entries.len());
                // The cut is resolved against the last whole frame, whatever it
                // was: the carried seal of a successor with no event of its own,
                // this file's own seal, or its last event. Reading it off the
                // events alone named the genesis for the first two, and a later
                // whole frame chained from a seal would not authenticate there, so
                // an acknowledged frame under a lying length read as a torn append.
                let offset = resolve_ambiguous_tail(&mut file, offset, tail, index)?;
                file.set_len(offset).map_err(JournalError::Storage)?;
                file.sync_all().map_err(JournalError::Storage)?;
                (offset, true)
            }
        };

        let mut ladder: HashMap<crate::effect::EffectKey, EventKind> = entries
            .iter()
            .map(|entry| (entry.event().key(), entry.event().kind()))
            .collect();
        let mut outcomes = HashMap::new();
        for entry in &entries {
            if let EffectEvent::OutcomeObserved { key, evidence } = *entry.event() {
                outcomes.insert(key, (entry.position(), evidence));
            }
        }
        // The carried state seeds the append fence's own indexes: the same ladder
        // and the same folded folds, so a successor's append check is literally the
        // predecessor's with the checkpoint as where the index was primed.
        let mut folded = Vec::new();
        let mut base = JournalPosition::genesis();
        if let Some(checkpoint) = carried.as_ref() {
            base = checkpoint.predecessor();
            folded = checkpoint.settled().to_vec();
            for held in checkpoint.unresolved() {
                if let Some(rung) = held.rung() {
                    ladder_hold(&mut ladder, held.key(), rung);
                }
            }
            for held in checkpoint.settled() {
                if let Some(rung) = held.rung() {
                    ladder_hold(&mut ladder, held.key(), rung);
                }
            }
        }

        let storage =
            StorageOwner::spawn(file, (), kind.stalled()).map_err(JournalError::Storage)?;
        let view = FileView::read_only(&path)?;
        Ok(Self {
            path,
            storage,
            view,
            committed: entries,
            position: tail,
            ladder,
            outcomes,
            disk_len: acked_len,
            torn_tail_repaired,
            continuing: kind.continues(),
            continuation: kind.policy(),
            carried,
            seal,
            base,
            chain_from,
            folded,
            sealed_by: None,
            pause: None,
        })
    }

    /// Refuse an append that would take the file past [`MAX_JOURNAL_EVENTS`].
    ///
    /// `additional` is how many events this append is about to add.
    ///
    /// # Errors
    ///
    /// [`JournalError::CapacityExceeded`] when the total would be over the
    /// bound. The history is not compacted to make room, so nothing
    /// acknowledged is evicted to admit more work.
    pub(super) fn bound_events(&self, additional: u64) -> Result<(), JournalError> {
        let requested = u64::saturating_from(self.committed.len()).saturating_add(additional);
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

    /// Where the journal lives.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether open repaired a torn tail to produce this journal.
    ///
    /// `true` means a killed writer's last append was taken back. The events
    /// this journal holds are exactly the ones that were acknowledged.
    #[must_use]
    pub const fn torn_tail_repaired(&self) -> bool {
        self.torn_tail_repaired
    }

    /// The committed events, in append order.
    pub fn events(&self) -> impl Iterator<Item = &EffectEvent> {
        self.committed.iter().map(JournalEntry::event)
    }

    /// Stream the committed events from the file, frame by frame.
    ///
    /// This is the bounded replay path: unlike [`Self::events`], which borrows
    /// the history `open` already materialized, a `Replay` reads from its own
    /// descriptor and retains one event at a time, so a caller folding a large
    /// log pays for the largest frame rather than the whole history. What it
    /// yields is exactly the acknowledged prefix: a torn tail ends the stream
    /// and an impossible or lying frame refuses it, the same dispositions the
    /// open scan gives them.
    ///
    /// # Errors
    ///
    /// [`JournalError::Storage`] when the read-only descriptor cannot be opened.
    pub fn replay(&self) -> Result<Replay, JournalError> {
        Replay::open(&self.path)
    }

    /// What the journal has learned about each attempt.
    ///
    /// A successor folds its carried checkpoint first, so the answer is the same
    /// one its predecessor would have given: an attempt that was
    /// `OutcomeUnknown` before a continuation is `OutcomeUnknown` after it, with
    /// the same verification digest on the settled ones.
    #[must_use]
    pub fn recover(&self) -> Recovered {
        recover_continued(self.carried.as_ref(), self.events())
    }
}
