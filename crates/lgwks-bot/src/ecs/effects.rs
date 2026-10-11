//! The write-ahead effect path: fencing, recording and recovery of attempts.

use std::collections::{HashMap, HashSet};
use std::fmt;

// `self` is load-bearing: the `Component` and `Resource` derives expand to
// `bevy_ecs::…` paths, so the crate name has to be in scope at the use site even
// though every edge in this crate is written through the `lgwks_deps` facade.

use crate::broker::{Authority, Broker, DispatchError, prepare_dispatch};
use crate::effect::{
    ActionDigest, ActionId, AttemptId, EffectIdentity, EffectKey, EnvironmentEpoch,
};
use crate::journal::{
    DurabilityPromise, DurableAck, EffectEvent, EffectJournal, EventKind, JournalError,
    JournalPosition,
};
use crate::verb::EffectLifetime;

// The one declared clock every deadline in this crate names (INV-BOT-30). Only
// its wall watchdog is read below — a source that stopped answering is not
// waiting for time, so a logical counter a caller can advance is the wrong
// authority for it — but it is reached through the crate's clock rather than
// through a free-floating `Instant`, which is what makes the elapsed time that
// cancelled a poll traceable to a named authority.

use super::identity::changed_outcome;
use super::identity::derive_action_digest;
use super::scope::EffectScope;
use super::work::EffectEvidence;

/// The effect path as the kernel holds it, with the recovered state folded in.
///
/// Private, and deliberately not the public [`EffectScope`]: the scope is what
/// a host declares, and this is what the kernel does with it — including the
/// one piece of state the host cannot supply, which is what the journal already
/// says about attempts this bot has not made yet.
pub(super) struct Effects {
    /// What the host declared.
    pub(super) scope: EffectScope,
    /// The committed position represented by this controller's recovery fold.
    ///
    /// This advances only from acknowledgements this controller accepted. A
    /// fresh tail would launder another controller's writes into stale state.
    pub(super) tail: JournalPosition,
    /// Attempts the journal records as dispatched with no outcome, and which
    /// nothing has settled since.
    ///
    /// Seeded once, at assembly, from [`crate::journal::recover`]. Held as keys
    /// rather than as a count because settling one has to name it, and an
    /// attempt that is only a number in a total cannot be named.
    pub(super) unsettled: Vec<EffectKey>,
    /// Durable outcomes whose positioned receipt has not been verified.
    pub(super) recording: Vec<RecordedOutcome>,
    /// Durability required by outcomes this process has admitted.
    ///
    /// This is intentionally process-local. The v1 journal wire contract does
    /// not encode an admitted grade, so recovery falls back to the journal's
    /// declared durability rather than changing persisted event bytes.
    pub(super) requirements: Vec<(EffectKey, DurabilityPromise)>,
    /// The most recently applied key for each action, keyed by action.
    ///
    /// Written when an attempt is settled `Applied`, and seeded at assembly
    /// from the attempts [`crate::journal::recover`] reads back as applied or
    /// verified, and read by the walk before it dispatches: the record says the
    /// effect landed, so the entry it was about is done *for the generation the
    /// key names*. A source that moves opens a new generation, and new work,
    /// which is the one thing that may legitimately re-run an acknowledged
    /// action.
    ///
    /// Held as the *latest* key per action, not as every key ever applied: the
    /// contract identity question ("does the value the run now sees match the
    /// one the last applied episode carried?") is a question about the last
    /// episode for that action, and keeping older ones would both grow
    /// unboundedly and mis-answer it. One entry per action for the life of the
    /// controller is the retention horizon.
    ///
    /// Held as whole keys rather than as `(action, revision)` pairs because a
    /// key carries the generation as a digest, and a recovered key has no
    /// integer revision to pair with it — the digest is one-way. Comparing the
    /// digest the run would mint for its current revision against the digest it
    /// holds answers the same question, and it answers it for an
    /// acknowledgement this process never made.
    pub(super) applied_latest: HashMap<ActionId, EffectKey>,
    /// The event identities that have landed, as `(action, digest)`.
    ///
    /// An *event* identity answers a different question from the contract
    /// identity: a redelivery of the same event retires however many other
    /// episodes came between (issue #101), so membership over the tagged
    /// history is the right shape. Stored as the digest rather than the whole
    /// key because the digest is exactly what the question compares — the
    /// attempt number is deliberately not part of it.
    ///
    /// The set is bounded by the distinct `(action, digest)` pairs the run has
    /// applied, which is the journal's own deduplication horizon; older event
    /// identities are not evicted, because doing so would let a redelivery of
    /// an older event re-run (#129). This is the same retention the previous
    /// `Vec<EffectKey>` had; the change is that both questions are now answered
    /// in constant time instead of by scanning a growing vector, which was
    /// Θ(N²) over a run that applied N distinct keys.
    pub(super) applied_events: HashSet<(ActionId, ActionDigest)>,
    /// The latest attempt the journal records for each action it names.
    ///
    /// Seeded once, at assembly, from [`crate::journal::recover`], and read
    /// when the attempt after it is minted. A restart is not a fresh run: the
    /// journal's ladder allows one walk of `IntentAdmitted → DispatchPrepared →
    /// OutcomeObserved → Verified` per key, so minting an attempt the journal
    /// already records reproduces the key it was recorded under and the append
    /// is refused as out of order. Continuing a run therefore means continuing
    /// its attempt sequence, and the number to continue from is in the record.
    pub(super) attempted: Vec<(ActionId, AttemptId)>,
}

/// A recovered outcome that remains blocked until its journal receipt can be
/// bound to the exact recorded event.
pub(super) struct RecordedOutcome {
    /// The exact effect attempt whose outcome was recorded.
    pub(super) key: EffectKey,
    /// What that record says happened.
    pub(super) evidence: EffectEvidence,
    /// Why the durable receipt could not be accepted during recovery.
    pub(super) cause: JournalError,
}

impl Effects {
    /// A scope with nothing recovered yet.
    pub(super) fn new(scope: EffectScope, tail: JournalPosition) -> Self {
        Self {
            scope,
            tail,
            unsettled: Vec::new(),
            recording: Vec::new(),
            requirements: Vec::new(),
            applied_latest: HashMap::new(),
            applied_events: HashSet::new(),
            attempted: Vec::new(),
        }
    }

    /// Note that `key`'s effect is recorded as landed.
    ///
    /// The one place both indexes move, so the two answers the walk asks of
    /// them cannot drift: the contract identity keeps the latest key per
    /// action, and the event identity keeps the `(action, digest)` membership.
    /// Idempotent, because an outcome may be settled once live and once by a
    /// reopen.
    pub(super) fn note_applied(&mut self, key: EffectKey) {
        self.applied_events.insert((key.action(), key.digest()));
        self.applied_latest.insert(key.action(), key);
    }

    /// The position, evidence, and typed journal refusal behind one held
    /// outcome. Keeping the error typed avoids collapsing recovery failures
    /// into an indistinguishable pending state.
    pub(super) fn recording_for(&self, action: ActionId) -> Option<&RecordedOutcome> {
        self.recording
            .iter()
            .find(|entry| entry.key.action() == action)
    }

    /// The host's three run-level facts.
    pub(super) const fn identity(&self) -> EffectIdentity {
        self.scope.identity()
    }

    /// The environment broker this run's dispatches are fenced against.
    pub(super) const fn broker(&self) -> &Broker {
        self.scope.broker()
    }

    /// Give the scope's journal back.
    pub(super) fn into_journal(self) -> Box<dyn EffectJournal> {
        self.scope.into_journal()
    }

    /// Obtain the warrant for `key` and record the attempt as prepared.
    ///
    /// One method rather than two calls at the dispatch site, because the two
    /// borrows it needs — the broker to authorize and the journal to append —
    /// are two fields of one scope, and a call site that reached through
    /// `&self` for one and `&mut self` for the other would have to restructure
    /// itself to prove they do not overlap. The split is stated once, here.
    ///
    /// # Errors
    ///
    /// [`DispatchError::Broker`] when the environment refuses, and
    /// [`DispatchError::Journal`] when the append is refused.
    pub(super) async fn prepare(
        &mut self,
        key: EffectKey,
        lifetime: EffectLifetime,
    ) -> Result<Authority, DispatchError> {
        // The durability admission is on the actual handoff path, not a
        // helper a test can call: an external effect must not leave on a
        // record that cannot outlive the process (issue #100).
        let required = match lifetime {
            EffectLifetime::Local => DurabilityPromise::Ephemeral,
            EffectLifetime::External => {
                self.scope
                    .journal()
                    .admit_external_handoff()
                    .map_err(DispatchError::Journal)?;
                // Reserve the whole handoff — intent, preparation and the
                // settlement that lands only after the effect has left the
                // process — before the first rung is written. A journal that
                // admitted the first two and refused the third would leave an
                // attempt that cannot be settled without deleting unresolved
                // evidence (#122 item 2 / #156).
                self.scope
                    .journal()
                    .reserve_handoff_capacity(3)
                    .map_err(DispatchError::Journal)?;
                DurabilityPromise::ProcessCrash
            }
        };
        // Intent first. Its acknowledgment is the first durability fact on the
        // handoff path: a store that cannot outlive the process is refused here,
        // before `DispatchPrepared` exists for recovery to misread as a live
        // dispatch (issue #100).
        let intent = EffectEvent::IntentAdmitted { key };
        let intent_ack = self.append_async(&intent).await?;
        self.note_journal_attempt(key.action(), key.attempt());
        if !intent_ack.promise().meets(required) {
            let refusal = Err(DispatchError::Journal(JournalError::PromiseUnmet {
                required,
                offered: intent_ack.promise(),
            }));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "prepare: returning an error to the caller");
            return refusal;
        }
        self.accept_position(&intent, intent_ack.position())?;
        let expected_tail = self.tail;
        let scope = &mut self.scope;
        let (authority, ack) =
            prepare_dispatch(&scope.broker, &mut *scope.journal, expected_tail, key)
                .await?
                .into_parts();
        // The acknowledgment, not the advertisement. A journal that offers
        // less on this append than the handoff requires is refused even when
        // its `durability()` claimed enough.
        if !ack.promise().meets(required) {
            // The prepare record may already have committed under a weaker
            // promise. Do not append a best-effort `NotApplied`: its receipt
            // would be just as unverified and could turn an ambiguous durable
            // state into a false no-handoff claim. Recovery must hold it as
            // unknown until an operator establishes the outcome.
            let refusal = Err(DispatchError::Journal(JournalError::PromiseUnmet {
                required,
                offered: ack.promise(),
            }));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "prepare: returning an error to the caller");
            return refusal;
        }
        self.accept_position(&EffectEvent::DispatchPrepared { key }, ack.position())?;
        self.requirements.push((key, required));
        Ok(authority)
    }

    /// The generation the broker currently holds for this run's environment.
    ///
    /// `None` when the broker has never been told about the environment. That
    /// is a declared state and not an accident: a broker that owns no
    /// environment mints no authority, which is the refusal
    /// [`prepare_dispatch`] reports as [`BrokerError::UnknownEnvironment`].
    ///
    /// [`BrokerError::UnknownEnvironment`]: crate::broker::BrokerError::UnknownEnvironment
    pub(super) fn epoch(&self) -> Option<EnvironmentEpoch> {
        self.scope.broker().epoch(self.identity().environment())
    }

    /// The key for one attempt of one entry, or `None` when the broker does not
    /// own the environment this run acts on.
    pub(super) fn key(
        &self,
        action: ActionId,
        chain: usize,
        entry: usize,
        input: [u8; 16],
        attempt: AttemptId,
    ) -> Option<EffectKey> {
        Some(self.identity().key(
            action,
            attempt,
            derive_action_digest(self.identity().flow(), chain, entry, &input),
            self.epoch()?,
        ))
    }

    /// Continue the run's journal if it has reached its watermark.
    ///
    /// Asked on the shipped append path rather than by a caller a test wrote,
    /// because a lifecycle policy nobody consults is a policy that does not exist
    /// (issue #267). The question is one call and one answer on every append: a
    /// journal that does not continue says so and the append proceeds exactly as
    /// it always did, which is why adding this changed no existing behaviour.
    ///
    /// The whole controller state moves with the journal, and that is the point:
    /// `unsettled`, `recording`, `applied_latest` and `attempted` are this
    /// controller's fold of the history, and the successor carries the same facts
    /// in its checkpoint, so the two agree by construction. Only the *fence* moves
    /// and it moves to the successor's own tail, which is the position after its
    /// seal frame — the append that follows is the successor's second event, and
    /// it is fenced on the successor's chain rather than the predecessor's.
    ///
    /// # Errors
    ///
    /// Whatever the continuation reports. A journal that cannot continue at this
    /// size refuses the *continuation*, not the append, and the append then lands
    /// in the journal it was always going to land in — which is what leaves the
    /// ladder's own reservation the last line of defence rather than the first.
    pub(super) fn continue_if_due(&mut self) -> Result<(), JournalError> {
        if !self.scope.journal().continuation_watermark()?.is_due() {
            return Ok(());
        }
        let successor = match self.scope.journal_mut().continue_as_new()? {
            Some(successor) => successor,
            None => return Ok(()),
        };
        let adopted = successor.tail();
        self.scope.journal = successor;
        self.tail = adopted;
        Ok(())
    }

    /// Append one fact, fencing on the tail represented by this controller's
    /// recovery fold.
    ///
    /// A refusal is never retried with a newly-read tail: another controller
    /// may have created an unknown effect that this controller has not folded.
    ///
    /// # Errors
    ///
    /// Whatever the journal refuses.
    pub(super) fn append(&mut self, event: &EffectEvent) -> Result<DurableAck, JournalError> {
        self.continue_if_due()?;
        self.scope
            .journal_mut()
            .compare_and_append(self.tail, event)
    }

    /// The same append, for a caller on an executor.
    ///
    /// The two doors differ only in how they wait, so which one a caller uses
    /// is a statement about that caller and not about the append. A dispatch
    /// path is on an executor, where a journal whose durability path blocks
    /// would charge the device's latency to everything else on the thread.
    ///
    /// # Errors
    ///
    /// Whatever the journal refuses.
    pub(super) async fn append_async(
        &mut self,
        event: &EffectEvent,
    ) -> Result<DurableAck, JournalError> {
        self.continue_if_due()?;
        self.scope
            .journal_mut()
            .compare_and_append_async(self.tail, event)
            .await
    }

    /// Advance the fold fence only after the journal confirms the exact
    /// acknowledged position is still current. An adapter-supplied position is
    /// not authority by itself.
    pub(super) fn accept_position(
        &mut self,
        event: &EffectEvent,
        position: JournalPosition,
    ) -> Result<(), JournalError> {
        self.event_position(event, position)?;
        let actual = self.scope.journal().tail();
        if actual != position {
            let refusal = Err(JournalError::TailMismatch {
                expected: position,
                actual,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "accept_position: returning an error to the caller");
            return refusal;
        }
        self.tail = position;
        Ok(())
    }

    /// Whether an attempt on `action` is recorded as dispatched with no
    /// outcome, and nothing has settled it.
    ///
    /// This is the question a dispatch asks before it resends anything, and the
    /// answer that stops it. An unknown outcome is not permission to try again:
    /// nothing established that the bytes did not arrive.
    pub(super) fn blocks(&self, action: ActionId) -> bool {
        self.unsettled.iter().any(|key| key.action() == action)
            || self.recording_for(action).is_some()
    }

    /// The unsettled key for `action`, when there is one.
    pub(super) fn unsettled_for(&self, action: ActionId) -> Option<EffectKey> {
        self.unsettled
            .iter()
            .find(|key| key.action() == action)
            .copied()
    }

    /// Whether `action` was acknowledged applied for the generation `revision`
    /// of `(chain, entry)`.
    ///
    /// The comparison is between the digest the run would mint for its current
    /// revision and the digest a held key carries, rather than between two
    /// integers, because the held key may have come from a process that is
    /// gone: the digest is what the record has, so the digest is what the
    /// question is asked in.
    pub(super) fn applied_in(
        &self,
        action: ActionId,
        chain: usize,
        entry: usize,
        input: [u8; 16],
        event: bool,
    ) -> bool {
        let digest = derive_action_digest(self.identity().flow(), chain, entry, &input);
        if event {
            // The identity names an event. A returning one is the same event
            // again: a redelivery of work that already landed, and it retires
            // however many other episodes came between (issue #101).
            return self.applied_events.contains(&(action, digest));
        }
        // The identity names only content. A restart that still sees the value
        // the last applied episode carried retires — that is the #101 property
        // content identity exists for. A later transition *back* to it after a
        // different value is a new episode of work and must run (issue #129):
        // a `0 → 1 → 0` watch fires three times, not twice.
        //
        // "Latest for this action" is "latest for this entry": `ActionId` is
        // derived from the bot name, the chain, the entry and the domain, so
        // one entry has one action and one action has one entry.
        self.applied_latest
            .get(&action)
            .is_some_and(|key| key.digest() == digest)
    }

    /// The attempt the journal already records for `action`, when it names one.
    ///
    /// The highest such attempt rather than the last seen, although the fold
    /// that seeds this reaches them in ascending order: an ordering assumption
    /// that a later record shape could break is not worth the two lines it
    /// saves, and the answer being wrong is a refused append rather than a
    /// loud failure.
    pub(super) fn recorded_attempt(&self, action: ActionId) -> Option<AttemptId> {
        self.attempted
            .iter()
            .find(|entry| entry.0 == action)
            .map(|entry| entry.1)
    }

    /// Note that the journal already records `attempt` for `action`.
    pub(super) fn note_journal_attempt(&mut self, action: ActionId, attempt: AttemptId) {
        match self.attempted.iter_mut().find(|entry| entry.0 == action) {
            Some(slot) => {
                if attempt.get() > slot.1.get() {
                    slot.1 = attempt;
                }
            }
            None => self.attempted.push((action, attempt)),
        }
    }

    /// Settle one recovered attempt, appending the outcome that settles it.
    ///
    /// The append lands first: the journal is the record, and a bot that
    /// dropped the key from its own list and then crashed before writing the
    /// outcome would come back with the same unknown it just cleared.
    ///
    /// # Errors
    ///
    /// Whatever the journal refuses.
    pub(super) fn settle_recovered(
        &mut self,
        key: EffectKey,
        evidence: EffectEvidence,
    ) -> Result<(), JournalError> {
        self.ensure_outcome(key, evidence)?;
        self.unsettled.retain(|held| *held != key);
        Ok(())
    }

    /// Upgrade a recovered outcome receipt without appending or re-entering
    /// the action that produced it.
    pub(super) fn settle_recording(
        &mut self,
        key: EffectKey,
        evidence: EffectEvidence,
    ) -> Result<(), JournalError> {
        self.confirm_recorded_outcome(key, evidence)?;
        self.recording.retain(|entry| entry.key != key);
        self.fold_outcome(key, evidence);
        Ok(())
    }

    /// Return the recorded position only when it holds the exact outcome.
    pub(super) fn outcome_position(
        &self,
        key: EffectKey,
        evidence: EffectEvidence,
    ) -> Result<JournalPosition, JournalError> {
        let event = EffectEvent::OutcomeObserved { key, evidence };
        let Some((position, recorded)) = self.scope.journal().outcome_at(key)? else {
            let refusal = Err(JournalError::OutOfOrder {
                key: Box::new(key),
                expected: Some(EventKind::OutcomeObserved),
                attempted: EventKind::OutcomeObserved,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "outcome_position: returning an error to the caller");
            return refusal;
        };
        if recorded != evidence {
            let refusal = Err(changed_outcome(key));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "outcome_position: the journal holds a different outcome");
            return refusal;
        }
        self.event_position(&event, position)?;
        Ok(position)
    }

    /// Require the acknowledged position to hold the exact event.
    ///
    /// A tail equality check alone proves only that *something* occupies the
    /// acknowledged position. Binding the event prevents an adapter from
    /// laundering a concurrent writer's tail into this controller's fold.
    pub(super) fn event_position(
        &self,
        expected: &EffectEvent,
        position: JournalPosition,
    ) -> Result<(), JournalError> {
        let Some(entry) = self.scope.journal().committed_entry(position)? else {
            let refusal = Err(JournalError::OutOfOrder {
                key: Box::new(expected.key()),
                expected: Some(expected.kind()),
                attempted: expected.kind(),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "event_position: returning an error to the caller");
            return refusal;
        };
        if entry.event() != expected {
            let refusal = Err(JournalError::EntryMismatch {
                position,
                expected: Box::new(*expected),
                actual: Box::new(*entry.event()),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "event_position: returning an error to the caller");
            return refusal;
        }
        Ok(())
    }

    /// The outcome already committed for `key`, when there is one.
    ///
    /// The durable half of idempotent settlement: a repeat after reconstruction
    /// has no live `AttemptRecord` to consult, and answering `NoSuchWork` there
    /// is how the same evidence stops being a safe retry across a restart.
    ///
    /// # Errors
    ///
    /// Whatever the journal refuses when read. An unreadable journal is not an
    /// absent outcome: collapsing the two is how a transient storage failure
    /// becomes a terminal "this work never existed" (issue #123).
    pub(super) fn outcome_for(
        &self,
        key: &EffectKey,
    ) -> Result<Option<EffectEvidence>, JournalError> {
        Ok(self
            .scope
            .journal()
            .outcome_at(*key)?
            .map(|(_, evidence)| evidence))
    }

    /// Require the receipt for an outcome to meet the grade admitted for its
    /// attempt. A weak append can have occupied the ladder already; the
    /// journal's explicit receipt operation is the only safe retry in that
    /// case.
    ///
    /// The receipt binds to the keyed outcome, not to whatever the
    /// acknowledgment names: an ack for a position that holds no outcome for
    /// this key is a [`JournalError::ReceiptMismatch`], never an acceptance.
    pub(super) fn confirm_outcome(
        &mut self,
        key: EffectKey,
        evidence: EffectEvidence,
        required: DurabilityPromise,
        acknowledgment: DurableAck,
    ) -> Result<(), JournalError> {
        let position = self.outcome_position(key, evidence)?;
        if acknowledgment.position() != position {
            let refusal = Err(JournalError::ReceiptMismatch {
                expected: position,
                actual: acknowledgment.position(),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "confirm_outcome: returning an error to the caller");
            return refusal;
        }
        if acknowledgment.promise().meets(required) {
            return Ok(());
        }
        self.obtain_receipt(key, evidence, position, required)
    }

    /// Ask the journal for its own receipt for the outcome at `position` and
    /// require it to name that position at the grade `required`.
    ///
    /// The one receipt check both [`Self::confirm_outcome`] (a weak append whose
    /// acknowledgment fell short) and [`Self::confirm_recorded_outcome`] (recovery
    /// of an outcome whose acknowledgment is gone) end in, so the two cannot
    /// disagree about what a receipt must prove.
    pub(super) fn obtain_receipt(
        &mut self,
        key: EffectKey,
        evidence: EffectEvidence,
        position: JournalPosition,
        required: DurabilityPromise,
    ) -> Result<(), JournalError> {
        let receipt = self
            .scope
            .journal_mut()
            .confirm_outcome(key, evidence, position, required)?;
        if receipt.position() != position {
            let refusal = Err(JournalError::ReceiptMismatch {
                expected: position,
                actual: receipt.position(),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "obtain_receipt: the receipt names another position");
            return refusal;
        }
        if receipt.promise().meets(required) {
            Ok(())
        } else {
            Err(JournalError::PromiseUnmet {
                required,
                offered: receipt.promise(),
            })
        }
    }

    /// Fold a receipted outcome into the local state.
    pub(super) fn fold_outcome(&mut self, key: EffectKey, evidence: EffectEvidence) {
        self.requirements.retain(|entry| entry.0 != key);
        self.note_journal_attempt(key.action(), key.attempt());
        if evidence == EffectEvidence::Applied {
            self.note_applied(key);
        }
    }

    /// Verify a previously committed outcome before recovery treats it as
    /// settled. A durable adapter must provide positioned readback; otherwise
    /// the action remains blocked rather than being re-entered.
    ///
    /// An ephemeral journal skips the receipt operation: readback is the
    /// strongest proof its grade can offer, and the default
    /// [`EffectJournal::confirm_outcome`] refuses exactly because readback
    /// bytes prove nothing about survival. For a journal that promises to
    /// survive anything, the record must be attested, not merely present.
    pub(super) fn confirm_recorded_outcome(
        &mut self,
        key: EffectKey,
        evidence: EffectEvidence,
    ) -> Result<(), JournalError> {
        let required = self.scope.journal().durability();
        if required == DurabilityPromise::Ephemeral {
            return Ok(());
        }
        let position = self.outcome_position(key, evidence)?;
        self.obtain_receipt(key, evidence, position, required)
    }

    /// Settle an outcome the journal already holds.
    ///
    /// The committed record is the fact; what is left is the receipt the
    /// attempt was admitted under. The journal's own grade must meet it — a
    /// weaker store is refused rather than settled below the grade the
    /// dispatch was admitted with — and a store that grades itself above
    /// ephemeral must re-attest through its receipt operation, because
    /// readback proves only that bytes are present, not that they will
    /// survive what the grade promises.
    ///
    /// # Errors
    ///
    /// [`JournalError::ReceiptUnavailable`] when the journal's grade is below
    /// `required` or it has no receipt operation, and whatever the readback or
    /// the receipt operation refuses.
    pub(super) fn settle_committed_outcome(
        &mut self,
        key: EffectKey,
        evidence: EffectEvidence,
        required: DurabilityPromise,
    ) -> Result<(), JournalError> {
        let offered = self.scope.journal().durability();
        if !offered.meets(required) {
            let refusal = Err(JournalError::ReceiptUnavailable { required });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "settle_committed_outcome: returning an error to the caller");
            return refusal;
        }
        if offered != DurabilityPromise::Ephemeral {
            self.confirm_recorded_outcome(key, evidence)?;
        }
        self.accept_position(
            &EffectEvent::OutcomeObserved { key, evidence },
            self.outcome_position(key, evidence)?,
        )?;
        self.fold_outcome(key, evidence);
        Ok(())
    }

    /// Record an outcome for `key` if one is not already committed, and fold
    /// it into the in-memory applied set.
    ///
    /// Idempotent: a repeat of the same evidence does not append twice. The
    /// ladder's next step after `OutcomeObserved` is `Verified`, so an
    /// `OutOfOrder` that expected `Verified` (or nothing) means this exact
    /// record is already in the journal and the call is a success. This is the
    /// one settlement write, shared by the live and recovered paths so a
    /// restart cannot resurrect an attempt a live resolve already settled.
    ///
    /// # Errors
    ///
    /// Whatever the journal refuses, other than the idempotent repeat above.
    pub(super) fn ensure_outcome(
        &mut self,
        key: EffectKey,
        evidence: EffectEvidence,
    ) -> Result<(), JournalError> {
        // A key with no requirement of its own is held to the journal's grade:
        // that is the promise every other key in the scope is held to.
        let required = match self.requirements.iter().find(|entry| entry.0 == key) {
            Some(&(_, declared)) => declared,
            None => self.scope.journal().durability(),
        };
        if let Some(recorded) = self.outcome_for(&key)? {
            if recorded != evidence {
                let refusal = Err(changed_outcome(key));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "ensure_outcome: the journal holds a different outcome");
                return refusal;
            }
            // The record is already in the journal. The only open question is
            // the receipt: the original append's acknowledgment is not in hand
            // on this path, so the journal's own receipt operation is what
            // settles a graded attempt (issue #118).
            return self.settle_committed_outcome(key, evidence, required);
        }
        match self.append(&EffectEvent::OutcomeObserved { key, evidence }) {
            Ok(acknowledgment) => self.record_new_outcome(key, evidence, required, acknowledgment),
            Err(JournalError::OutOfOrder { expected, .. })
                if expected == Some(EventKind::Verified) || expected.is_none() =>
            {
                // Idempotent success only when the *same* evidence is already
                // recorded. A ladder-complete `OutOfOrder` alone is not proof of
                // which outcome landed, and treating it as success is how a
                // contradictory `Applied` gets acknowledged (issue #106).
                match self.outcome_for(&key)? {
                    Some(recorded) if recorded == evidence => {
                        // The original append acknowledgment is unavailable
                        // after an out-of-order retry. The journal's receipt
                        // operation, not a minted replacement, is what
                        // settles a graded attempt.
                        self.settle_committed_outcome(key, evidence, required)
                    }
                    _ => Err(JournalError::OutOfOrder {
                        key: Box::new(key),
                        expected,
                        attempted: EventKind::OutcomeObserved,
                    }),
                }
            }
            Err(cause @ JournalError::OutcomeUnknown { .. }) => {
                // The store may hold the event; the reply was lost. Read back
                // before anything else: a matching record means the effect and
                // its record both landed, and the attempt settles instead of
                // reporting an ambiguity the next retry can never resolve.
                match self.outcome_for(&key)? {
                    Some(recorded) if recorded == evidence => {
                        self.settle_committed_outcome(key, evidence, required)
                    }
                    Some(_) => Err(JournalError::OutOfOrder {
                        key: Box::new(key),
                        expected: Some(EventKind::Verified),
                        attempted: EventKind::OutcomeObserved,
                    }),
                    // Nothing landed. The ambiguous failure is the honest
                    // report: the caller is post-effect and must surface
                    // `EffectUnrecorded` with `Occurred` certainty, and a
                    // recovery pass reconciles from the journal's own replay.
                    None => Err(cause),
                }
            }
            Err(cause) => {
                // A refusal that left the journal unchanged. Surface it now:
                // the caller is post-effect and must report `EffectUnrecorded`,
                // not a clean settle.
                Err(cause)
            }
        }
    }
}

impl Effects {
    /// Records a freshly appended outcome and folds it into the in-memory view.
    ///
    /// A helper rather than three statements in the arm: confirming the outcome,
    /// accepting its position and folding it are one transition, and a reader of
    /// `ensure_outcome` had to know that all three happen before the outcome
    /// counts. The other two arms settle the same outcome by a different route,
    /// so the three are now one named step with one exit.
    pub(super) fn record_new_outcome(
        &mut self,
        key: EffectKey,
        evidence: EffectEvidence,
        required: DurabilityPromise,
        acknowledgment: DurableAck,
    ) -> Result<(), JournalError> {
        self.confirm_outcome(key, evidence, required, acknowledgment)?;
        let position = self.outcome_position(key, evidence)?;
        self.accept_position(&EffectEvent::OutcomeObserved { key, evidence }, position)?;
        self.fold_outcome(key, evidence);
        Ok(())
    }
}

impl fmt::Debug for Effects {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Effects")
            .field("identity", &self.identity())
            .field("journal", &self.scope.journal().durability())
            .field("unsettled", &self.unsettled.len())
            .field("applied", &self.applied_events.len())
            .field("attempted", &self.attempted.len())
            .finish()
    }
}

// ── Components: identity and the change marker are separate ────────────────
