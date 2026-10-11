//! The ledger of outstanding work, settled across ticks.

use std::time::Duration;

#[cfg(doc)]
use lgwks_deps::bevy_ecs::schedule::IntoScheduleConfigs;

use crate::journal::frame::SaturatingFrom;

use crate::broker::Authority;
use crate::effect::{ActionId, AttemptId, EffectKey};
use crate::error::BotError;
use crate::journal::{EffectJournal, JournalError};
use crate::spec::Erased;
use crate::verb::EffectLifetime;

use super::admit::failure_state;
use super::effects::Effects;
use super::entry::EntryState;
use super::entry::RetryAdmission;
use super::entry::RetryReading;
use super::entry::Settled;
use super::entry::SettledAttempt;
use super::entry::Transition;
use super::entry::backoff_entropy;
use super::identity::derive_action_digest;
use super::identity::derive_input_stamp;
use super::work::AbandonReason;
use super::work::EffectEvidence;
use super::work::PendingWork;
use super::work::TransitionHold;
use super::work::WorkId;

/// The eligible work of the bot, keyed by chain.
///
/// One transition per chain, and never one per revision: an entry's work cannot
/// be re-derived from the change filter, and two transitions for one chain
/// would replay an acknowledged effect on the older one to reach an entry the
/// newer one skipped. Bounded by construction — at most one state per entry of
/// the spec, so the ledger cannot grow with the number of ticks.
///
/// A non-send resource rather than a plain one, because a transition owns the
/// observed payload it is bound to and `Box<dyn Any>` is neither `Send` nor
/// `Sync`. The binding travels *inside* the transition deliberately: a second
/// structure holding the payloads would have to be kept in step with this one
/// through every `take`, `put`, `begin`, `skip` and `fail`, and the first path
/// that forgot would silently re-bind a live transition to the wrong value.
#[derive(Debug)]
pub(super) struct Ledger {
    /// One live transition per chain, in declaration order.
    pub(super) transitions: Vec<Option<Transition>>,
    /// The next admitted-input identity to mint.
    ///
    /// Seeded from the journal tail so a reconstructed bot cannot reissue a
    /// stamp an earlier process already bound a dispatch to.
    pub(super) next_input: u64,
    /// The action each entry of each chain runs, in declaration order.
    ///
    /// Derived once, at assembly, from the declarations — the bot's name, the
    /// chain's index, the entry's index and the action's own domain — rather
    /// than re-derived per attempt. Two derivations of one identity are two
    /// identities the moment they disagree, and an action identity that drifted
    /// between the key a dispatch was recorded under and the key a settlement
    /// names is a settlement that never lands.
    pub(super) actions: Vec<Vec<ActionId>>,
    /// The effect path: the identity, the fence and the journal, plus what
    /// recovery found in them.
    pub(super) effects: Effects,
}

impl Ledger {
    /// A ledger with one idle slot per chain, no work recorded, and the
    /// declared action identities indexed as the chains are.
    pub(super) fn new(actions: Vec<Vec<ActionId>>, effects: Effects, next_input: u64) -> Self {
        Self {
            transitions: (0..actions.len()).map(|_| None).collect(),
            actions,
            effects,
            next_input,
        }
    }

    /// Mint the fallback admitted-input identity.
    ///
    /// Used only when a binding cannot supply an [`InputIdentity`]. The
    /// watermark is seeded from the journal tail at assembly and only ever
    /// grows, so a restart cannot reissue a stamp an earlier process bound a
    /// dispatch to (issue #101). An identified binding supplies its content
    /// identity instead; that identity is unique per distinct value, which is
    /// not the same as unique per admitted episode (issue #129).
    pub(super) fn mint_input(&mut self) -> [u8; 16] {
        let stamp = self.next_input;
        self.next_input = self.next_input.saturating_add(1);
        derive_input_stamp(stamp)
    }

    /// The action one entry runs, when the ledger has that entry.
    pub(super) fn action_of(&self, id: WorkId) -> Option<ActionId> {
        self.actions.get(id.chain())?.get(id.entry()).copied()
    }

    /// Give the journal back, leaving the ledger unusable.
    ///
    /// The undo of the move `assemble` made, and the only way a record outlives
    /// the bot that wrote it.
    pub(super) fn into_journal(self) -> Box<dyn EffectJournal> {
        self.effects.into_journal()
    }

    /// The generation a chain's live transition was opened at, when it has one.
    ///
    /// What a caller needs to ask whether an acknowledgement covers the work in
    /// front of it: an `Applied` verdict retires its action for one generation
    /// and no other, so the comparison is between the acknowledgement's
    /// generation and this one.
    pub(super) fn generation(&self, chain: usize) -> Option<([u8; 16], bool)> {
        self.transitions
            .get(chain)
            .and_then(Option::as_ref)
            .map(|transition| (transition.input, transition.event))
    }

    /// The entry an action is declared on, when the ledger has one.
    ///
    /// The action is what a key carries and an address is what the ledger is
    /// indexed by, so this is the translation settlement runs first. It is a
    /// search rather than a map because the declaration is bounded by the spec
    /// — a handful of entries, scanned at most once per settlement and once per
    /// recovery — and a second index kept in step with `actions` is a second
    /// place for the two to disagree.
    pub(super) fn locate(&self, action: ActionId) -> Option<WorkId> {
        for (chain, entries) in self.actions.iter().enumerate() {
            if let Some(entry) = entries.iter().position(|declared| *declared == action) {
                return Some(WorkId { chain, entry });
            }
        }
        None
    }

    /// Take a chain's transition out, leaving its slot empty. The walk needs it
    /// owned: the ledger is mutated while the chains, the observed values and
    /// the granted authority are borrowed from the same world.
    pub(super) fn take(&mut self, chain: usize) -> Option<Transition> {
        self.transitions.get_mut(chain).and_then(Option::take)
    }

    /// Put a chain's transition back, `None` when nothing is retained.
    pub(super) fn put(&mut self, chain: usize, transition: Option<Transition>) {
        if let Some(slot) = self.transitions.get_mut(chain) {
            *slot = transition;
        }
    }

    /// The entry a [`WorkId`] names, when the ledger holds it.
    pub(super) fn entry_state(&self, id: WorkId) -> Option<&EntryState> {
        self.transitions
            .get(id.chain())
            .and_then(Option::as_ref)
            .and_then(|transition| transition.entries.get(id.entry()))
    }

    /// The entry a [`WorkId`] names, when the ledger holds it.
    pub(super) fn entry_mut(&mut self, id: WorkId) -> Option<&mut EntryState> {
        self.transitions
            .get_mut(id.chain())
            .and_then(Option::as_mut)
            .and_then(|transition| transition.entries.get_mut(id.entry()))
    }

    /// Record that an attempt on this entry is about to be made, *before* it is
    /// awaited.
    ///
    /// This is the write-ahead half of the ledger, and it is why the driver
    /// writes to the world between awaits at all: a tick dropped while the effect
    /// is in flight leaves this record behind, and "an attempt began and no
    /// outcome was recorded" is held exactly as an unknown outcome is. Without
    /// it, a dropped tick would leave a `NotStarted` entry behind and the next
    /// tick would replay an effect that may already be live.
    ///
    /// `Err` when the entry is not in a state an attempt can start from, when
    /// the action identity is unknown, or when the journal or the broker
    /// refused — which means the world moved behind the schedule's back.
    ///
    /// The two writes happen here, in this order, and neither can be moved
    /// later. `IntentAdmitted` is the decision to act, and `DispatchPrepared` is
    /// the warrant: both are committed *before* the caller builds the future that
    /// reaches the external system. A crash anywhere after this returns leaves a
    /// journal that says "this attempt was prepared and nothing recorded what
    /// became of it", which recovery reads as an unknown rather than as a
    /// never-sent — the exact distinction a duplicate merge turns on.
    ///
    /// The warrant is handed back rather than consumed here because the handoff
    /// is the caller's: it runs after the payload the attempt acts on is in
    /// hand, and it presents the warrant at [`Broker::revalidate`] on the way in.
    ///
    /// `retry` is the tick's reading of the retry schedule. An entry still
    /// inside its backoff, or whose deadline has passed, is refused here as well
    /// as stopped by the walk, so a second caller cannot retry it by reaching
    /// past the plan.
    pub(super) async fn begin_attempt(
        &mut self,
        id: WorkId,
        lifetime: EffectLifetime,
        retry: &RetryReading,
    ) -> Result<(EffectKey, Authority), BotError> {
        let no_such_work = || BotError::NoSuchWork { work: id };
        let action = self.action_of(id).ok_or_else(no_such_work)?;
        // Refused before anything is written, and refused for the reason
        // recovery exists: an attempt on this action is recorded as dispatched
        // with no outcome, so the bytes may already be live. Beginning another
        // one is the blind resend, and the guard is here rather than at the top
        // of the walk so it cannot be bypassed by a second caller.
        if self.effects.blocks(action) {
            let refusal = Err(BotError::EffectUnsettled { action });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "begin_attempt: returning an error to the caller");
            return refusal;
        }
        let transition = self
            .transitions
            .get_mut(id.chain())
            .and_then(Option::as_mut)
            .ok_or_else(no_such_work)?;
        let startable = match *transition
            .entries
            .get(id.entry())
            .ok_or_else(no_such_work)?
        {
            EntryState::NotStarted => true,
            EntryState::DefinitelyFailed { .. } => {
                transition.retry_admission(id.entry(), retry) == Some(RetryAdmission::Begin)
            }
            _ => false,
        };
        if !startable {
            let refusal = Err(no_such_work());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "begin_attempt: returning an error to the caller");
            return refusal;
        }
        let record = transition
            .attempts
            .get_mut(id.entry())
            .ok_or_else(no_such_work)?;
        // The attempt identity is minted here and nowhere else, which is what
        // makes it monotonic within the generation: `FIRST` for an entry that
        // has never been attempted, otherwise the successor of the last one. An
        // exhausted chain of successes is refused rather than wrapped, because
        // an identity that came round again would name two attempts.
        //
        // "Never been attempted" means *this run has not attempted it*, and the
        // two are not the same question after a restart. A transition is opened
        // fresh, so its record starts empty however far the journal got, and
        // minting attempt one again would name the attempt the record already
        // holds — which the journal refuses, and which is the refuse-forever
        // dead end a settled `NotApplied` would otherwise lead into. The
        // journal's own latest attempt for the action is therefore the floor,
        // and the successor of that floor is what this mints.
        let attempt = match record.begun {
            Some(previous) => previous
                .checked_next()
                .ok_or(BotError::EffectUnsettled { action })?,
            None => match self.effects.recorded_attempt(action) {
                Some(recorded) => recorded
                    .checked_next()
                    .ok_or(BotError::EffectUnsettled { action })?,
                None => AttemptId::FIRST,
            },
        };
        // Journal first, memory second. `record.begun` and `EntryState` are
        // moved only after the write-ahead pair is committed and its
        // acknowledgments are strong enough: a refusal before that point leaves
        // the entry as it was, instead of a false `Unrecorded` barrier for a
        // handoff that never left (issue #100).
        let input = transition.input;
        let key = self
            .effects
            .key(action, id.chain(), id.entry(), input, attempt)
            .ok_or(BotError::EffectUnsettled { action })?;
        let authority = self
            .effects
            .prepare(key, lifetime)
            .await
            .map_err(|cause| BotError::EffectRefused { cause })?;
        let transition = self
            .transitions
            .get_mut(id.chain())
            .and_then(Option::as_mut)
            .ok_or_else(no_such_work)?;
        let record = transition
            .attempts
            .get_mut(id.entry())
            .ok_or_else(no_such_work)?;
        record.begun = Some(attempt);
        if record.first_begun.is_none() {
            record.first_begun = Some(retry.now);
        }
        if let Some(state) = transition.entries.get_mut(id.entry()) {
            *state = EntryState::Unrecorded;
        }
        Ok((key, authority))
    }

    /// Record that the entry's condition did not hold, so it owes nothing.
    pub(super) fn skip(&mut self, id: WorkId) -> bool {
        let Some(state) = self.entry_mut(id) else {
            return false;
        };
        if !matches!(
            *state,
            EntryState::NotStarted | EntryState::DefinitelyFailed { .. }
        ) {
            return false;
        }
        *state = EntryState::Skipped;
        true
    }

    /// Record that the attempt returned without error.
    pub(super) fn succeed(&mut self, id: WorkId) -> bool {
        let Some(state) = self.entry_mut(id) else {
            return false;
        };
        if !matches!(*state, EntryState::Unrecorded) {
            return false;
        }
        *state = EntryState::Succeeded;
        true
    }

    /// The key, evidence and attempt count of an entry held for an append-only
    /// retry, when there is one.
    pub(super) fn recording_failed(
        &self,
        id: WorkId,
    ) -> Option<(crate::effect::EffectKey, EffectEvidence, u32)> {
        match *self.entry_state(id)? {
            EntryState::RecordingFailed {
                key,
                evidence,
                attempts,
                ..
            } => Some((key, evidence, attempts)),
            _ => None,
        }
    }

    /// Refresh a [`EntryState::RecordingFailed`] hold after another refused
    /// append, without touching the action or the attempt identity.
    pub(super) fn hold_recording(
        &mut self,
        id: WorkId,
        evidence: EffectEvidence,
        error: &BotError,
    ) -> bool {
        let Some(state) = self.entry_mut(id) else {
            return false;
        };
        let EntryState::RecordingFailed {
            ref mut cause,
            evidence: ref mut held,
            ..
        } = *state
        else {
            return false;
        };
        *cause = error.to_string();
        *held = evidence;
        true
    }

    /// Close an entry whose outcome is known and whose record has now landed.
    ///
    /// `Applied` retires the entry. `NotApplied` leaves it eligible for a fresh
    /// attempt under the retry schedule — a new attempt is a response to "the
    /// effect did not happen", not a retry of the recording failure.
    pub(super) fn finish_recording(
        &mut self,
        id: WorkId,
        evidence: EffectEvidence,
        attempts: u32,
        retry: &RetryReading,
    ) -> bool {
        let (first, entropy) = self.backoff_basis(id, attempts, retry.now);
        let Some(state) = self.entry_mut(id) else {
            return false;
        };
        if !matches!(*state, EntryState::RecordingFailed { .. }) {
            return false;
        }
        *state = match evidence {
            EffectEvidence::Applied => EntryState::Succeeded,
            EffectEvidence::NotApplied => retry.after_definite_failure(
                attempts,
                first,
                entropy,
                "effect not applied; its record landed on retry".into(),
            ),
        };
        true
    }

    /// Give up on an entry whose retry came due after its deadline, and say so.
    ///
    /// Called where a retry would begin. `true` when the entry was abandoned,
    /// which makes it the barrier the rest of its chain waits behind.
    pub(super) fn expire_retry(&mut self, id: WorkId, retry: &RetryReading) -> bool {
        let Some(transition) = self
            .transitions
            .get_mut(id.chain())
            .and_then(Option::as_mut)
        else {
            return false;
        };
        let Some(RetryAdmission::Expired { elapsed }) =
            transition.retry_admission(id.entry(), retry)
        else {
            return false;
        };
        let Some(state) = transition.entries.get_mut(id.entry()) else {
            return false;
        };
        let EntryState::DefinitelyFailed {
            attempts,
            ref mut cause,
            ..
        } = *state
        else {
            return false;
        };
        let cause = std::mem::take(cause);
        *state = EntryState::Abandoned {
            reason: AbandonReason::DeadlineExhausted { attempts, elapsed },
            cause,
        };
        true
    }

    /// What a definite failure's backoff is measured from, and what jitters it.
    ///
    /// The first attempt's start is the deadline's origin. An entry with no
    /// recorded start was begun by nothing this ledger saw, so the failure
    /// itself is the origin: the deadline then runs from the earliest instant
    /// this process can vouch for, never from one it would have to invent.
    fn backoff_basis(&self, id: WorkId, attempts: u32, now: Duration) -> (Duration, u64) {
        let first = match self
            .transitions
            .get(id.chain())
            .and_then(Option::as_ref)
            .and_then(|transition| transition.attempts.get(id.entry()))
            .and_then(|record| record.first_begun)
        {
            Some(first) => first,
            None => now,
        };
        let entropy = match self.action_of(id) {
            Some(action) => backoff_entropy(action, attempts),
            None => u64::from(attempts),
        };
        (first, entropy)
    }

    /// Record what a failed attempt means for the entry it was made on.
    ///
    /// The attempt arrives as the identity the key carries rather than as a
    /// count, because the count [`EntryState::DefinitelyFailed`] renders is a
    /// `u32` and the identity is not. The conversion is saturating rather than
    /// wrapping: an attempt ordinal past `u32::MAX` would take the maximum,
    /// which reports an entry as further along than it is, and that is the
    /// direction a retry budget can absorb.
    pub(super) fn fail(
        &mut self,
        id: WorkId,
        error: &BotError,
        attempt: AttemptId,
        retry: &RetryReading,
    ) -> bool {
        let attempts = u32::saturating_from(attempt.get());
        let (first, entropy) = self.backoff_basis(id, attempts, retry.now);
        let Some(state) = self.entry_mut(id) else {
            return false;
        };
        // Only an attempt in flight can fail: an outcome recorded for an entry
        // that was never begun is not this tick's to write.
        if !matches!(*state, EntryState::Unrecorded) {
            return false;
        }
        *state = failure_state(error, attempts, retry, first, entropy);
        true
    }

    /// Settle an attempt whose effect may or may not have happened.
    ///
    /// The caller hands over the [`EffectKey`] it read from
    /// [`PendingWork::key`], and nothing is read or written until that key's own
    /// facts match the ledger's: the run and the flow it acts under, the action
    /// that locates the entry, the binding the entry currently holds, and the
    /// attempt the entry is on.
    ///
    /// This is not a formality. A chain has one transition at a time and a slot
    /// is reused by every generation over it, so an action that matches says
    /// only that the *address* is the same. A report computed against one
    /// generation's binding and delivered after the slot was re-opened under the
    /// next would be accepted against work the caller has never seen: `Applied`
    /// would acknowledge an effect produced from an input that is gone, and
    /// `NotApplied` — worse, because it authorises a retry — would make an
    /// unknown attempt eligible to run against the new one.
    ///
    /// The binding is the digest, and it carries both halves of that check at
    /// once: it is derived from the generation's revision *and* from the entry
    /// it belongs to, so a key for one entry cannot be spent on another and a
    /// key for one generation cannot be spent on the next. Comparing it is what
    /// makes supersession a fact about the observed input rather than about a
    /// counter that a restarted process would begin again at one.
    ///
    /// The attempt closes the same hole one level down, where the binding does
    /// not help. An entry is retried *within* a generation: settling it
    /// `NotApplied` makes it eligible, and the next tick begins the next attempt
    /// over the same binding. A repeat of the earlier report — which must be
    /// tolerated, because a caller that never saw its first delivery
    /// acknowledged has to be able to send it again — is indistinguishable from
    /// a fresh one unless the attempt is named, and accepting it would reset an
    /// in-flight attempt to eligible.
    ///
    /// Returns a decision rather than a bool, because the answers lead a caller
    /// to different next actions and several of them are not errors.
    /// Validate an evidence report without changing any state.
    ///
    /// The whole of [`settle`](Self::settle)'s decision, minus the mutation.
    /// `resolve_effect` runs this first so the settlement fact can be journaled
    /// before the entry is allowed to move: a live settle that only touched
    /// memory is resurrected by recovery as an unknown (issue #106).
    pub(super) fn classify_settlement(
        &self,
        key: &EffectKey,
        evidence: EffectEvidence,
    ) -> Result<Settled, JournalError> {
        let Some(id) = self.locate(key.action()) else {
            return Ok(Settled::NoSuchWork { id: None });
        };
        // The run and the flow are the identity this ledger speaks for. A key
        // from another run is not this ledger's to settle, and answering it with
        // any of the finer refusals would suggest the evidence nearly applied.
        let identity = self.effects.identity();
        if key.run() != identity.run()
            || key.flow() != identity.flow()
            || key.environment() != identity.environment()
        {
            return Ok(Settled::NoSuchWork { id: None });
        }
        let Some(transition) = self.transitions.get(id.chain()).and_then(Option::as_ref) else {
            return self.durable_or_missing(key, id, evidence);
        };
        // The binding first, before the attempt is even looked up: a key that
        // names the right action inside a generation that is gone is exactly the
        // case this exists to refuse.
        let live = derive_action_digest(identity.flow(), id.chain(), id.entry(), &transition.input);
        if key.digest() != live {
            return Ok(Settled::Superseded { id, current: live });
        }
        let Some(record) = transition.attempts.get(id.entry()).copied() else {
            return self.durable_or_missing(key, id, evidence);
        };
        // The durable record is authoritative about *what the outcome is*. A
        // mismatch is a contradiction whatever the live slot says. A match is
        // deliberately not answered here: a live settleable slot still has to
        // be moved — an abandoned entry reopened by confirmation that its
        // effect did not land is exactly that — and answering `Duplicate`
        // before the move is how the reopen stops happening (issue #106).
        if let Some(recorded) = self.effects.outcome_for(key)?
            && recorded != evidence
        {
            return Ok(Settled::Contradicted {
                id,
                settled: recorded,
            });
        }
        let settleable = matches!(
            transition.entries.get(id.entry()),
            Some(
                EntryState::Unrecorded
                    | EntryState::OutcomeUnknown { .. }
                    | EntryState::Abandoned { .. }
            )
        );
        if !settleable {
            // Decided: an entry that ran, one whose condition was false, one
            // that has not been attempted yet, and one with a live budget all
            // have an answer, and evidence supplied after the fact cannot
            // contradict an attempt whose outcome was recorded.
            //
            // But an answer this *generation* was already given is not a
            // contradiction, it is the same answer twice, and a caller whose
            // first delivery it never saw an outcome for has to be able to
            // repeat it. Only the record distinguishes that from a fresh claim
            // about an entry that was never settled.
            // The record answers for the attempt it was made about and no
            // other. An entry can be decided while the record is about an
            // earlier attempt — an entry settled and then failed again is on
            // its second attempt with the first one's record still beside it —
            // and reading that record as an answer about *this* attempt is how
            // a report about attempt 2 comes back as a duplicate of attempt 1.
            return match record.settled {
                Some(previous)
                    if previous.attempt == key.attempt() && previous.evidence == evidence =>
                {
                    Ok(Settled::Duplicate)
                }
                Some(previous) if previous.attempt == key.attempt() => Ok(Settled::Contradicted {
                    id,
                    settled: previous.evidence,
                }),
                Some(_) | None => self.durable_or_missing(key, id, evidence),
            };
        }
        // Then the attempt, and this is the half that the slot and the
        // generation together do not cover. Within one generation an entry is
        // retried: settling it `NotApplied` makes it eligible and `begin` moves
        // it to the next attempt at the same address under the same revision. A
        // repeat of the earlier report — which has to be tolerated, because a
        // caller that never saw its first delivery acknowledged must be able to
        // send it again — would otherwise read as a statement about the attempt
        // now outstanding, and `NotApplied` would make an attempt whose effect
        // may be live eligible to run a third time.
        //
        // Placed after the decided case rather than before it, because a
        // decided entry is where the repeat is *supposed* to land: it is only
        // the live entry whose attempt has to match.
        // An entry with nothing begun cannot be settleable, so the `None` arm
        // is unreachable; it is answered with the same refusal as an entry the
        // ledger does not hold rather than being asserted away.
        let Some(outstanding) = record.begun else {
            return Ok(Settled::NoSuchWork { id: Some(id) });
        };
        if key.attempt() != outstanding {
            return Ok(Settled::StaleAttempt {
                id,
                reported: key.attempt(),
                outstanding,
            });
        }
        Ok(Settled::Decided)
    }

    /// The journal's answer when the live slot has none left to move.
    ///
    /// A reconstructed bot has no in-memory record to consult, and a caller
    /// repeating an acknowledgement it never saw land has no live attempt
    /// either. The durable fact is then the only answer (issue #106 scenario
    /// 3): the same evidence twice is [`Settled::Duplicate`], the other
    /// evidence is [`Settled::Contradicted`], and only a key the journal never
    /// recorded is [`Settled::NoSuchWork`]. Answering `NoSuchWork` for a
    /// settlement the journal already holds is how the same evidence stops
    /// being a safe retry across a restart.
    ///
    /// A journal that cannot be read answers neither: the error is propagated
    /// so an unreadable record is never recast as "the work never existed"
    /// (issue #123).
    pub(super) fn durable_or_missing(
        &self,
        key: &EffectKey,
        id: WorkId,
        evidence: EffectEvidence,
    ) -> Result<Settled, JournalError> {
        Ok(match self.effects.outcome_for(key)? {
            Some(recorded) if recorded == evidence => Settled::Duplicate,
            Some(recorded) => Settled::Contradicted {
                id,
                settled: recorded,
            },
            None => Settled::NoSuchWork { id: Some(id) },
        })
    }

    /// Apply a settlement [`classify_settlement`](Self::classify_settlement)
    /// answered [`Decided`](Settled::Decided) for.
    ///
    /// Called only after the settlement fact is committed, so the entry never
    /// moves on evidence the journal does not hold.
    pub(super) fn apply_settlement(&mut self, key: &EffectKey, evidence: EffectEvidence) {
        let Some(id) = self.locate(key.action()) else {
            return;
        };
        let Some(transition) = self
            .transitions
            .get_mut(id.chain())
            .and_then(Option::as_mut)
        else {
            return;
        };
        let Some(record) = transition.attempts.get(id.entry()).copied() else {
            return;
        };
        let Some(outstanding) = record.begun else {
            return;
        };
        let next = match evidence {
            EffectEvidence::Applied => EntryState::Succeeded,
            EffectEvidence::NotApplied => EntryState::NotStarted,
        };
        if let Some(state) = transition.entries.get_mut(id.entry()) {
            *state = next;
        }
        if let Some(slot) = transition.attempts.get_mut(id.entry()) {
            // Recorded against the attempt the evidence was *about*, never
            // against whichever is current: this record is what decides
            // whether a later report is a repeat, and a record that drifted
            // onto the next attempt would make two different attempts look
            // like one.
            slot.settled = Some(SettledAttempt {
                attempt: outstanding,
                evidence,
            });
        }
    }

    /// The payload the chain's live transition is bound to, if it has one.
    ///
    /// The binding is on loan from the observation slot, which is empty while
    /// the transition holds it, so this is the only way to read back what the
    /// chain last acted on. Two callers need that and they need it for the same
    /// reason: the change filter, which cannot compare a new poll against a
    /// slot that has nothing in it, and the executor, which must run an entry
    /// against the value its transition was opened under rather than against
    /// whatever has arrived since.
    pub(super) fn bound(&self, chain: usize) -> Option<&Erased> {
        self.transitions
            .get(chain)
            .and_then(Option::as_ref)
            .and_then(|transition| transition.value.as_ref())
    }

    /// The key for the attempt an entry is currently on, when it has one.
    ///
    /// `None` for an entry never attempted, and for a chain or entry the ledger
    /// does not hold. Both are the same answer to the caller — there is no
    /// attempt identity to hand out — and one absence rather than two keeps the
    /// settlement sites from having to tell them apart.
    pub(super) fn key_of(
        &self,
        chain: usize,
        entry: usize,
        transition: &Transition,
    ) -> Option<EffectKey> {
        let id = WorkId { chain, entry };
        self.effects.key(
            self.action_of(id)?,
            chain,
            entry,
            transition.input,
            transition.attempt_of(entry)?,
        )
    }

    /// Every entry that is still unresolved, in `(chain, entry)` order.
    ///
    /// One fold, and every report that used to disagree is rendered from it:
    /// [`Self::first_unresolved`] is the first item, [`Self::pending`] is the
    /// whole list, and [`Self::unresolved_count`] is its length.
    ///
    /// "Unresolved" is open *or* abandoned, and the second half is load-bearing.
    /// An abandoned entry asks nothing of the substrate — it will not be
    /// attempted again and no evidence is owed by the tick — so a scan that
    /// looked only for open entries walked straight past it and reported a clean
    /// tick. But abandoned work is not handled work: [`Self::pending`] names it,
    /// and a caller that read `Ok` as "the chain is done" was reading the
    /// opposite of what the ledger held. It is also a prerequisite that is *not*
    /// satisfied, so the entries behind it are unresolved through it.
    ///
    /// A recovered attempt nothing has settled is reported for its declared
    /// work whether or not a transition exists: the barrier is durable and does
    /// not wait for a poll, so a bot assembled against a journal that already
    /// says the bytes may be live is stuck before its first observation (issue
    /// #104). It is reported *instead of* the entry's own hold, because two
    /// reports for one entry would read as two problems — and it is why the
    /// three reports now agree: a skipped unknown used to produce `Ok` from
    /// `tick` while `pending` still named it.
    pub(super) fn unresolved(&self, budget: u32) -> Vec<PendingWork> {
        let mut unresolved = Vec::new();
        for chain in 0..self.actions.len().max(self.transitions.len()) {
            // The declared actions and the live transition can disagree in
            // width: a world mutated behind the builder's back is a real
            // state, and a scan that trusted only one of the two walked past
            // work the other one holds.
            let declared = match self.actions.get(chain) {
                Some(actions) => actions.len(),
                None => 0,
            };
            let held = match self.transitions.get(chain).and_then(Option::as_ref) {
                Some(transition) => transition.entries.len(),
                None => 0,
            };
            for entry in 0..declared.max(held) {
                let id = WorkId { chain, entry };
                if let Some(key) = self
                    .action_of(id)
                    .and_then(|action| self.effects.unsettled_for(action))
                {
                    let action = key.action();
                    unresolved.push(PendingWork {
                        id,
                        key: Some(Box::new(key)),
                        hold: TransitionHold::UnsettledByRecovery { action },
                    });
                    continue;
                }
                if let Some(recording) = self
                    .action_of(id)
                    .and_then(|action| self.effects.recording_for(action))
                {
                    unresolved.push(PendingWork {
                        id,
                        key: Some(Box::new(recording.key)),
                        hold: TransitionHold::RecordingFailed {
                            evidence: recording.evidence,
                            cause: recording.cause.to_string(),
                        },
                    });
                    continue;
                }
                let Some(transition) = self.transitions.get(chain).and_then(Option::as_ref) else {
                    continue;
                };
                let Some(state) = transition.entries.get(entry) else {
                    continue;
                };
                if !(state.is_open() || state.is_abandoned()) {
                    continue;
                }
                // Unreachable in practice: an open or abandoned state always
                // renders a hold.
                let Some(hold) = state.hold(budget) else {
                    continue;
                };
                unresolved.push(PendingWork {
                    id,
                    key: self.key_of(chain, entry, transition).map(Box::new),
                    hold,
                });
            }
        }
        unresolved
    }

    /// The first entry that is unresolved, in `(chain, entry)` order: the
    /// abandonment in front of its successors, the recovered unknown in front
    /// of a fresh attempt, the open entry itself otherwise. Taking the first
    /// in declaration order is deliberate: the barrier is the entry that
    /// explains every successor blocked behind it, so naming it is naming the
    /// reason.
    pub(super) fn first_unresolved(&self, budget: u32) -> Option<PendingWork> {
        self.unresolved(budget).into_iter().next()
    }

    /// Every entry that is not finished, in `(chain, entry)` order: open,
    /// given up on and still reported, or held by a recovered unknown.
    pub(super) fn pending(&self, budget: u32) -> Vec<PendingWork> {
        self.unresolved(budget)
    }

    /// How many entries are still unresolved, across every chain.
    ///
    /// Counted the same way [`Self::first_unresolved`] scans, because the two
    /// are rendered together in [`BotError::PendingTransition`]: a count that
    /// omitted abandoned entries would report a non-zero entry alongside an
    /// "0 outstanding", which reads as the one thing the pair is there to rule
    /// out — a chain that is somehow both stuck and finished. A recovered
    /// unknown is counted for the same reason: it is what is holding the work.
    pub(super) fn unresolved_count(&self, budget: u32) -> usize {
        self.unresolved(budget).len()
    }
}
