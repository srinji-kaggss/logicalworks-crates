//! Recovery: what is known about each attempt after a replay, and the fold that computes it.

use core::fmt;
use std::collections::HashMap;

use super::{Continuation, EffectEvent, EffectEvidence, Verification, VerificationResult};
use crate::effect::EffectKey;

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
    /// The effect landed, and a named predicate was evaluated and did **not**
    /// hold.
    ///
    /// Distinct from [`Applied`](Self::Applied), which is also what an attempt
    /// reads as before any predicate has run: folding the two together made a
    /// failed verification indistinguishable from an unverified success, and the
    /// failure recoverable only by re-deriving it from the raw events, which is a
    /// second authority. The effect is still known to have happened, so a
    /// recovery path must not resend it.
    VerificationFailed,
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
            Self::VerificationFailed => "verification_failed",
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
    /// The latest verification the journal holds for it, which is what a
    /// `Verified` or `VerificationFailed` status is qualified by: which predicate,
    /// at which version, over which observations.
    verification: Option<Verification>,
}

impl Attempt {
    /// Build an attempt record.
    #[must_use]
    pub const fn new(key: EffectKey, status: AttemptStatus) -> Self {
        Self {
            key,
            status,
            verification: None,
        }
    }

    /// The latest verification recorded for this attempt, or `None` when no
    /// predicate has been evaluated.
    ///
    /// A `Verified` status is only as good as the predicate version and
    /// observation digest it was decided at; this is how a consumer reads them
    /// and compares them with the state it is about to act on.
    #[must_use]
    pub const fn verification(&self) -> Option<Verification> {
        self.verification
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

/// One change of an attempt's recovered status.
///
/// `position` is the event's place in the sequence that was folded, counted from
/// zero. It is a *journal position*, not a wall-clock time, and that is
/// deliberate: recovery is a pure fold that two replays must agree on, and a
/// clock read inside it would make them disagree. "When" is answered by the
/// order of facts, which is the only order a replayed journal has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Transition {
    /// The status before this change, or `None` for the attempt's first event.
    from: Option<AttemptStatus>,
    /// The status after it.
    to: AttemptStatus,
    /// The event's position in the folded sequence.
    position: usize,
}

impl Transition {
    /// The status this change superseded, or `None` for the first.
    #[must_use]
    pub const fn from(&self) -> Option<AttemptStatus> {
        self.from
    }

    /// The status this change produced.
    #[must_use]
    pub const fn to(&self) -> AttemptStatus {
        self.to
    }

    /// Where in the folded sequence the change happened.
    #[must_use]
    pub const fn position(&self) -> usize {
        self.position
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
    /// Direct lookup by key while preserving `attempts` as the report order.
    index: HashMap<EffectKey, usize>,
    /// Every status change of each attempt, parallel to `attempts`. Bounded by
    /// the number of events folded, which the journal's own ceiling bounds.
    transitions: Vec<Vec<Transition>>,
}

impl Recovered {
    /// What is known about `key`, or `None` when the journal never mentioned
    /// it.
    #[must_use]
    pub fn status(&self, key: EffectKey) -> Option<AttemptStatus> {
        self.index
            .get(&key)
            .and_then(|index| self.attempts.get(*index))
            .map(|attempt| attempt.status)
    }

    /// The latest verification recorded for `key`, or `None` when the journal
    /// never mentioned it or no predicate has run.
    #[must_use]
    pub fn verification(&self, key: EffectKey) -> Option<Verification> {
        self.index
            .get(&key)
            .and_then(|index| self.attempts.get(*index))
            .and_then(Attempt::verification)
    }

    /// Every change of `key`'s status in the order it happened, empty when the
    /// journal never mentioned it.
    ///
    /// The answer to "what changed", beside [`status`](Self::status)'s "what is
    /// true now": the current status stays a cheap closed fold, and the history
    /// is separate rather than folded into it.
    #[must_use]
    pub fn history(&self, key: EffectKey) -> &[Transition] {
        // The index and the transition list are written together in the fold,
        // so a key the index names always has its list; a key it does not name
        // was never mentioned, and its history is empty.
        match self
            .index
            .get(&key)
            .and_then(|index| self.transitions.get(*index))
        {
            Some(transitions) => transitions.as_slice(),
            None => &[],
        }
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
    recover_continued(None, events)
}

/// Fold a journal that may begin from a sealed checkpoint.
///
/// [`recover`] with the carried state of a successor folded in first, which is
/// what makes the answer *the same* on both sides of a continuation: the
/// predecessor reports an attempt from its own suffix, the successor reports it
/// from the checkpoint that carried it, and a caller cannot tell which journal
/// answered except by looking at the path.
///
/// Two things are seeded, and the difference matters. An **unresolved** attempt is
/// seeded as its own key at its own rung, so it reads back exactly as uncertain as
/// it was and the ladder refuses a resend. A **settled** attempt is seeded as the
/// action's latest key, so the status and the verification a caller compares
/// against survive the boundary; the older attempts that key stands for are not
/// carried, and are refused instead — `AttemptId` is monotonic per action, so the
/// folded record answers "was this walked?" without the checkpoint holding every
/// attempt ever.
///
/// A seed whose rung or status this build does not name is **skipped**, and the
/// successor's own open refuses the checkpoint rather than reading it. Reading an
/// unknown rung as the foot of the ladder would make a sealed attempt replayable,
/// and reading an unknown status as absent would make a verified attempt invisible.
#[must_use]
pub fn recover_continued<'a>(
    base: Option<&Continuation>,
    events: impl IntoIterator<Item = &'a EffectEvent>,
) -> Recovered {
    let mut recovered = Recovered::default();
    if let Some(checkpoint) = base {
        for carried in checkpoint.settled() {
            let (Some(status), Some(_rung)) = (carried.status(), carried.rung()) else {
                continue;
            };
            seed(
                &mut recovered,
                carried.key(),
                status,
                carried.verification(),
            );
        }
        for carried in checkpoint.unresolved() {
            let Some(status) = carried.recovered_status() else {
                continue;
            };
            seed(&mut recovered, carried.key(), status, None);
        }
    }
    fold_events(&mut recovered, events);
    recovered
}

/// Put one carried attempt into a fold, in the order the checkpoint lists them.
fn seed(
    recovered: &mut Recovered,
    key: EffectKey,
    status: AttemptStatus,
    verification: Option<Verification>,
) {
    let index = recovered.attempts.len();
    let mut attempt = Attempt::new(key, status);
    attempt.verification = verification;
    recovered.attempts.push(attempt);
    recovered.transitions.push(vec![Transition {
        from: None,
        to: status,
        position: index,
    }]);
    recovered.index.insert(key, index);
}

/// Fold events over a recovery, one at a time.
fn fold_events<'a>(recovered: &mut Recovered, events: impl IntoIterator<Item = &'a EffectEvent>) {
    for (position, event) in events.into_iter().enumerate() {
        let status = match *event {
            EffectEvent::IntentAdmitted { .. } => AttemptStatus::Prepared,
            EffectEvent::DispatchPrepared { .. } => AttemptStatus::OutcomeUnknown,
            EffectEvent::OutcomeObserved { evidence, .. } => match evidence {
                EffectEvidence::Applied => AttemptStatus::Applied,
                EffectEvidence::NotApplied => AttemptStatus::NotApplied,
            },
            EffectEvent::Verified { verification, .. } => match verification.result() {
                VerificationResult::Satisfied => AttemptStatus::Verified,
                VerificationResult::NotSatisfied => AttemptStatus::VerificationFailed,
            },
        };
        let verification = match *event {
            EffectEvent::Verified { verification, .. } => Some(verification),
            _ => None,
        };
        let key = event.key();
        match recovered.index.get(&key).copied() {
            Some(index) => {
                if let Some(attempt) = recovered.attempts.get_mut(index) {
                    if let Some(trail) = recovered.transitions.get_mut(index) {
                        trail.push(Transition {
                            from: Some(attempt.status),
                            to: status,
                            position,
                        });
                    }
                    attempt.status = status;
                    if verification.is_some() {
                        attempt.verification = verification;
                    }
                }
            }
            None => {
                let index = recovered.attempts.len();
                let mut attempt = Attempt::new(key, status);
                attempt.verification = verification;
                recovered.attempts.push(attempt);
                recovered.transitions.push(vec![Transition {
                    from: None,
                    to: status,
                    position,
                }]);
                recovered.index.insert(key, index);
            }
        }
    }
}
