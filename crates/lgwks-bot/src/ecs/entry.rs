//! Per-entry retry schedule, attempt state and transitions.

use std::fmt;
use std::time::Duration;

use lgwks_std::retry::RetryPolicy;

use crate::clock::Clock;
use crate::effect::{ActionDigest, ActionId, AttemptId, EffectKey};
use crate::spec::Erased;
#[cfg(doc)]
use crate::verb::Observe;

use super::work::AbandonReason;
use super::work::AdmittedInput;
use super::work::EffectEvidence;
use super::work::TransitionHold;
use super::work::WorkId;
#[cfg(doc)]
use crate::spec::ObserveAny;

/// The policy a bot runs when the caller declares none: three attempts per
/// entry, no backoff, no total deadline.
///
/// The behaviour every bot had before the policy carried time, kept as the
/// default so a bot that never names a policy retries exactly as it did. There
/// is no measured basis for three; it is a declared guess and is named as one.
pub(super) fn default_policy() -> RetryPolicy {
    RetryPolicy::new(3, Duration::ZERO, Duration::MAX)
}

/// The retry policy a bot declared, and the clock its backoff is measured on.
///
/// One value because they are one authority: a backoff is a duration, and a
/// duration means nothing until a clock says when it started. The clock is the
/// bot's declared one (INV-BOT-30), so a test that drives a virtual clock
/// drives every backoff with it and a three-hour backoff costs one `advance`.
#[derive(Debug, Clone)]
pub(super) struct RetrySchedule {
    /// What the caller declared.
    pub(super) policy: RetryPolicy,
    /// What its durations are measured on.
    pub(super) clock: Clock,
}

impl RetrySchedule {
    /// The schedule a bot runs when the caller declares neither part.
    pub(super) fn declared_default() -> Self {
        Self {
            policy: default_policy(),
            clock: Clock::wall(),
        }
    }
}

/// Whether a definitely-failed entry may be retried at one clock reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RetryAdmission {
    /// Its backoff has not ended: the entry is held.
    Wait,
    /// Its backoff has ended inside the deadline: the retry may begin.
    Begin,
    /// Its backoff has ended, but past the deadline: no retry may begin.
    Expired {
        /// Bot-clock time from the first attempt to this reading.
        elapsed: Duration,
    },
}

/// The retry schedule as one tick reads it: the policy, and one reading of the
/// clock taken when the tick began.
///
/// One reading per tick rather than one per decision, so every entry a tick
/// walks is judged against the same instant and a tick's decisions do not
/// depend on how long the entries before them took to run.
#[derive(Debug, Clone, Copy)]
pub(super) struct RetryReading {
    /// What the caller declared.
    pub(super) policy: RetryPolicy,
    /// The bot clock when the tick began.
    pub(super) now: Duration,
}

impl RetryReading {
    /// Attempts allowed per entry, the first included. A policy that names
    /// zero means one, which is how the std policy reads its own field.
    pub(super) fn budget(&self) -> u32 {
        self.policy.max_attempts.max(1)
    }

    /// Whether an entry whose backoff ends at `retry_at`, and whose first
    /// attempt began at `first`, may be retried now.
    ///
    /// The deadline is checked again here, at the instant a retry would begin,
    /// and not only when the failure scheduled it: a bot ticked less often than
    /// its backoffs reaches a due retry late, and late can be past the deadline.
    pub(super) fn admission(&self, retry_at: Duration, first: Option<Duration>) -> RetryAdmission {
        if retry_at > self.now {
            return RetryAdmission::Wait;
        }
        let Some(first) = first else {
            return RetryAdmission::Begin;
        };
        let elapsed = self.now.saturating_sub(first);
        if self.policy.deadline_exceeded(elapsed) {
            RetryAdmission::Expired { elapsed }
        } else {
            RetryAdmission::Begin
        }
    }

    /// What a definite failure means for an entry: held for its backoff, or
    /// given up on.
    ///
    /// `attempts` counts the failed one, and `first` is when the entry's first
    /// attempt in this generation began. The backoff is the policy's delay for
    /// the retry this failure would lead to, jittered by `entropy`; the
    /// deadline bounds the retry rather than the failure, so a retry whose
    /// backoff would end at or past the deadline is refused now instead of
    /// being scheduled for a moment the policy already forbids.
    pub(super) fn after_definite_failure(
        &self,
        attempts: u32,
        first: Duration,
        entropy: u64,
        cause: String,
    ) -> EntryState {
        if attempts >= self.budget() {
            return EntryState::Abandoned {
                reason: AbandonReason::AttemptsExhausted { attempts },
                cause,
            };
        }
        let retry_at = self
            .now
            .saturating_add(self.policy.delay(attempts.saturating_sub(1), entropy));
        if self.policy.deadline_exceeded(retry_at.saturating_sub(first)) {
            return EntryState::Abandoned {
                reason: AbandonReason::DeadlineExhausted {
                    attempts,
                    elapsed: self.now.saturating_sub(first),
                },
                cause,
            };
        }
        EntryState::DefinitelyFailed {
            attempts,
            cause,
            retry_at,
        }
    }
}

/// The jitter entropy for one retry of one action: deterministic, so a replay
/// of the same run schedules the same backoffs.
///
/// Both halves of the action identity and the attempt count are folded in, so
/// two entries that failed on the same tick do not retry in lockstep and one
/// entry's successive retries do not all land on the same fraction of their
/// backoff.
pub(super) fn backoff_entropy(action: ActionId, attempts: u32) -> u64 {
    let bytes = action.id().get().get().to_le_bytes();
    let [low @ .., _, _, _, _, _, _, _, _] = bytes;
    let [_, _, _, _, _, _, _, _, high @ ..] = bytes;
    u64::from_le_bytes(low) ^ u64::from_le_bytes(high).rotate_left(attempts)
}

/// The state of one entry within a transition.
#[derive(Debug, Clone)]
pub(super) enum EntryState {
    /// Never attempted. An entry ahead of it may be holding it back.
    NotStarted,
    /// An attempt is under way, or was begun and its outcome never recorded.
    /// This is the issue's `Running`, named for the fact that survives the
    /// call.
    Unrecorded,
    /// The action ran and returned.
    Succeeded,
    /// The condition was false when it was evaluated against the value.
    Skipped,
    /// The effect definitely did not happen ([`BotError::DomainError`]), and
    /// attempts remain in the budget.
    DefinitelyFailed {
        /// Attempts made so far.
        attempts: u32,
        /// The failure.
        cause: String,
        /// When, on the bot clock, the backoff ends and a retry may begin.
        retry_at: Duration,
    },
    /// The effect may or may not have happened
    /// ([`BotError::EffectIndeterminate`]).
    OutcomeUnknown {
        /// Attempts made so far.
        attempts: u32,
        /// Why the outcome could not be settled.
        cause: String,
    },
    /// The action produced a known outcome and the journal refused to record
    /// it ([`BotError::EffectUnrecorded`]).
    ///
    /// Held for an **append-only** retry. The action is never re-entered: the
    /// occurrence is already a fact, and a second dispatch would turn that
    /// fact into a duplicate. The key and the evidence travel with the state so
    /// the next tick can write exactly the record that failed to land.
    RecordingFailed {
        /// The attempt identity the append is for.
        key: EffectKey,
        /// What the action did.
        evidence: EffectEvidence,
        /// Why the last append was refused.
        cause: String,
        /// Attempts made so far, including the one that produced the outcome.
        attempts: u32,
    },
    /// Given up on, and reported.
    Abandoned {
        /// Why.
        reason: AbandonReason,
        /// The failure that ended it.
        cause: String,
    },
}

impl EntryState {
    /// Whether the entry still needs something: an attempt, evidence, or the
    /// entry ahead of it to be resolved.
    pub(super) fn is_open(&self) -> bool {
        matches!(
            self,
            Self::NotStarted
                | Self::Unrecorded
                | Self::DefinitelyFailed { .. }
                | Self::OutcomeUnknown { .. }
                | Self::RecordingFailed { .. }
        )
    }

    /// Whether the entry was given up on: terminal for this transition, and
    /// reported rather than dropped.
    pub(super) fn is_abandoned(&self) -> bool {
        matches!(self, Self::Abandoned { .. })
    }

    /// How this entry reads to a caller, `None` when it is resolved and has
    /// nothing to report.
    pub(super) fn hold(&self, budget: u32) -> Option<TransitionHold> {
        match *self {
            Self::Succeeded | Self::Skipped => None,
            Self::NotStarted => Some(TransitionHold::NotStarted),
            Self::Unrecorded => Some(TransitionHold::Unrecorded),
            Self::DefinitelyFailed {
                attempts,
                ref cause,
                retry_at,
            } => Some(TransitionHold::Failed {
                attempts,
                budget,
                cause: cause.clone(),
                retry_at,
            }),
            Self::OutcomeUnknown {
                attempts,
                ref cause,
            } => Some(TransitionHold::OutcomeUnknown {
                attempts,
                cause: cause.clone(),
            }),
            Self::RecordingFailed {
                ref evidence,
                ref cause,
                ..
            } => Some(TransitionHold::RecordingFailed {
                evidence: *evidence,
                cause: cause.clone(),
            }),
            Self::Abandoned { reason, ref cause } => Some(TransitionHold::Abandoned {
                reason,
                cause: cause.clone(),
            }),
        }
    }
}

/// What the ledger knows about the attempts made on one entry of one
/// generation.
///
/// Kept beside [`EntryState`] rather than inside it, because the state is
/// overwritten by every settlement and every failure and this is not: an entry
/// settled `NotApplied` and attempted again is a *later* attempt at the same
/// address, and the ordinal is what says so.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct AttemptRecord {
    /// The latest attempt begun on this entry in this generation, or `None`
    /// while none has been.
    ///
    /// Monotonic by construction: only [`Ledger::begin_attempt`] moves it, and
    /// nothing resets it while the transition stands. It is the entry's attempt
    /// identity, and it is what a [`PendingWork`] hands out — an identity
    /// rather than a count, so the number a caller is handed and the number a
    /// settlement compares against are the same value rather than two
    /// derivations of one.
    pub(super) begun: Option<AttemptId>,
    /// The attempt the latest settlement was about, and what it said.
    ///
    /// The ordinal is stored rather than assumed to be the latest, because a
    /// settlement is what *permits* the next attempt: recording it against
    /// whichever attempt happened to be current when a repeat arrived is how a
    /// report about one attempt comes to authorise another.
    pub(super) settled: Option<SettledAttempt>,
    /// When, on the bot clock, the first attempt of this generation began.
    ///
    /// What the policy's total deadline is measured from. Set once by the
    /// first [`Ledger::begin_attempt`] and never moved, so a retry cannot buy
    /// itself a fresh deadline by being a retry.
    pub(super) first_begun: Option<Duration>,
}

/// The attempt one settlement was about, and the evidence it carried.
#[derive(Debug, Clone, Copy)]
pub(super) struct SettledAttempt {
    /// The attempt the evidence was submitted for.
    pub(super) attempt: AttemptId,
    /// What the evidence said.
    pub(super) evidence: EffectEvidence,
}

/// The work of one source transition: one state per entry of its chain.
///
/// Deliberately neither `Clone` nor a derived `Debug`: it owns the observed
/// payload it is bound to, and that payload is erased. Copying a transition
/// would quietly copy the payload with it — the very copy the sources are not
/// required to support — and printing one would demand `Debug` of a type the
/// source never promised it for. The manual [`fmt::Debug`] below renders the
/// binding as its presence, which is the part a reader of a log wants.
pub(super) struct Transition {
    /// The revision that opened it, for the caller-facing revision report.
    ///
    /// Not the digest input: this counter is process-local and reopens at 1
    /// after every reconstruction. [`Self::input`] is what a digest binds.
    pub(super) revision: u64,
    /// The admitted input identity this transition is bound to.
    ///
    /// A content identity extracted from the observation when it was admitted
    /// (issue #101), so a restart that sees the same input retires work that
    /// already landed and a restart that sees a different one does not.
    pub(super) input: [u8; 16],
    /// Whether [`Self::input`] names an event rather than only content.
    ///
    /// Settled at admission from [`InputIdentity::names_an_event`] and carried
    /// so a later retire check reads a returning identity the way the type
    /// says to read it (issue #129).
    pub(super) event: bool,
    /// One state per entry of the chain, in declaration order.
    pub(super) entries: Vec<EntryState>,
    /// What the ledger knows about the attempts made on each entry, for the
    /// generation [`Self::revision`] names, indexed as [`Self::entries`] is.
    ///
    /// Two facts live here, and each answers a question [`EntryState`] cannot.
    /// The settlement is what makes evidence idempotent and a contradiction
    /// refusable: `Applied` and an attempt that returned `Ok` both land on
    /// [`EntryState::Succeeded`], and `NotApplied` and an entry that has not
    /// been reached both land on [`EntryState::NotStarted`], so without it a
    /// caller whose first delivery was ambiguous cannot safely repeat it. The
    /// attempt ordinal is what makes the entry's attempts *numerable*: a
    /// settled entry walks back to [`EntryState::NotStarted`], which carries no
    /// count, so a number read off the state alone would restart at one and two
    /// different attempts would answer to the same name.
    ///
    /// Per generation: both are reset when a transition is opened, because a
    /// settlement is a statement about one attempt within one generation and
    /// neither fact carries into the next.
    pub(super) attempts: Vec<AttemptRecord>,
    /// The observed payload this transition is bound to.
    ///
    /// The binding is the whole point of the field. One transition is one
    /// generation of work over a chain, and every entry of it — the conditions
    /// that are evaluated and the actions that are run — must see the value the
    /// generation was opened under. Reading the newest observation instead
    /// splits a transition across two inputs: an entry that was acknowledged
    /// against the old value is retried against the new one, and the run ends
    /// up reporting an effect of the new value that was actually produced from
    /// the old.
    ///
    /// It is *moved* here out of the observation slot rather than copied from
    /// it, which is why nothing in this module asks a source's `Output` to be
    /// `Clone`. The observation slot is empty for exactly as long as a live
    /// transition owns the value, and [`observe_fold`] reads the binding back
    /// when it needs to know what the chain last acted on.
    ///
    /// `None` only for a transition opened while nothing had been observed —
    /// there is then no payload to bind, and the entries are held rather than
    /// evaluated against a value nobody read.
    pub(super) value: Option<Erased>,
}

impl fmt::Debug for Transition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Transition")
            .field("revision", &self.revision)
            .field("entries", &self.entries)
            .field("attempts", &self.attempts)
            .field("bound", &self.value.is_some())
            .finish()
    }
}

impl Transition {
    /// The latest attempt begun on `entry`, or `None` when this transition has
    /// no such entry or none has been begun.
    ///
    /// Read through here rather than off [`EntryState`] so that the identity a
    /// caller is handed and the identity settlement checks are the same one: a
    /// settled entry walks back to `NotStarted`, which carries no attempt, and
    /// an identity derived from the state would restart at the first one.
    pub(super) fn attempt_of(&self, entry: usize) -> Option<AttemptId> {
        self.attempts.get(entry).and_then(|record| record.begun)
    }

    /// A transition with every entry outstanding, bound to `value`.
    pub(super) fn opened(
        input: AdmittedInput,
        revision: u64,
        entries: usize,
        value: Option<Erased>,
    ) -> Self {
        Self {
            revision,
            input: input.identity,
            event: input.event,
            entries: vec![EntryState::NotStarted; entries],
            attempts: vec![AttemptRecord::default(); entries],
            value,
        }
    }

    /// Continue into a new revision, from a transition with nothing open.
    ///
    /// Every entry is outstanding again for the new value, *except* the ones
    /// given up on: those are terminal until evidence revives them, and
    /// carrying their record forward is what keeps a lost effect reported
    /// instead of silently dropped the moment the source moves.
    pub(super) fn resumed(
        input: AdmittedInput,
        revision: u64,
        previous: &Self,
        value: Option<Erased>,
    ) -> Self {
        Self {
            revision,
            input: input.identity,
            event: input.event,
            entries: previous
                .entries
                .iter()
                .map(|state| match *state {
                    EntryState::Abandoned { reason, ref cause } => EntryState::Abandoned {
                        reason,
                        cause: cause.clone(),
                    },
                    _ => EntryState::NotStarted,
                })
                .collect(),
            // Attempts as well as settlements: both are statements about one
            // attempt within one generation, and this is a new generation.
            attempts: vec![AttemptRecord::default(); previous.entries.len()],
            value,
        }
    }

    /// How a retry of `entry` stands at `retry`'s reading, or `None` when the
    /// entry is not one a retry is scheduled for.
    pub(super) fn retry_admission(&self, entry: usize, retry: &RetryReading) -> Option<RetryAdmission> {
        let EntryState::DefinitelyFailed { retry_at, .. } = *self.entries.get(entry)? else {
            return None;
        };
        let first = self.attempts.get(entry).and_then(|record| record.first_begun);
        Some(retry.admission(retry_at, first))
    }

    /// Whether any entry is still unresolved.
    pub(super) fn has_open(&self) -> bool {
        self.entries.iter().any(EntryState::is_open)
    }

    /// Whether the ledger must keep this transition: it has work to do, or a
    /// fact to report.
    pub(super) fn is_retained(&self) -> bool {
        self.entries
            .iter()
            .any(|state| state.is_open() || state.is_abandoned())
    }
}

/// What a settlement did to the ledger.
///
/// Six answers, because a caller's next move differs for each: [`Decided`] and
/// [`Duplicate`] are both success, and the other four are four distinct
/// refusals that must not be collapsed into one. Only [`NoSuchWork`] means
/// "there is no held effect for this action"; a caller told that about an action
/// it has a `PendingWork` for is being told its evidence is stale or
/// contradictory, which is a different repair.
///
/// Each refusal carries the address it was about, worked out where the ledger
/// already had it. A refusal that made the caller look the entry up again would
/// have to answer what to do when the lookup fails, and there is no honest
/// answer: the ledger just knew, and a second query is a chance for the two
/// answers to differ.
///
/// [`Decided`]: Settled::Decided
/// [`Duplicate`]: Settled::Duplicate
/// [`NoSuchWork`]: Settled::NoSuchWork
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Settled {
    /// The key's generation's entry was outstanding, and this evidence is now
    /// its outcome.
    Decided,
    /// The key's generation was already given exactly this evidence for this
    /// entry. A caller that could not tell whether its first delivery landed
    /// repeats it and must not be punished for the repetition, nor may the
    /// repeat move the entry a second time.
    Duplicate,
    /// The key names a binding that is not the one in the slot now: the work
    /// the caller is reporting on was superseded, and the live transition is
    /// about a different attempt over a different observed input.
    Superseded {
        /// The entry the action is declared on.
        id: WorkId,
        /// The binding the slot holds now.
        current: ActionDigest,
    },
    /// The named attempt is not the one outstanding on that entry: a later
    /// attempt has been begun since the caller read the work.
    ///
    /// Distinct from [`Self::Superseded`], which is about the generation, and
    /// the distinction is the point of carrying an attempt at all. The
    /// generation can be the one the caller named and the attempt still be
    /// gone — that is the retry-inside-one-generation case — and accepting the
    /// report there would apply it to an attempt the caller never observed.
    StaleAttempt {
        /// The entry the action is declared on.
        id: WorkId,
        /// The attempt the evidence was about.
        reported: AttemptId,
        /// The attempt the entry is on now.
        outstanding: AttemptId,
    },
    /// The named generation's entry is settled, and this evidence says the
    /// opposite of what settled it.
    Contradicted {
        /// The entry the action is declared on.
        id: WorkId,
        /// The evidence already recorded for this generation.
        settled: EffectEvidence,
    },
    /// The key has no work to settle: either the action is not declared here at
    /// all, or it is declared at an entry whose outcome is already decided.
    ///
    /// The address is absent only in the first case, which is why it is an
    /// `Option` rather than a fabricated slot: a caller told "no such work, at
    /// chain 0 entry 0" would be reading a place the key never named.
    NoSuchWork {
        /// The entry the action is declared on, when it is declared at all.
        id: Option<WorkId>,
    },
}
