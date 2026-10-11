//! Admitted inputs and the pending-work vocabulary a caller resolves.

use std::any::Any;
use std::fmt;
use std::time::Duration;

use crate::effect::InputIdentity;
use lgwks_deps::bevy_ecs::prelude::World;
#[cfg(doc)]
use lgwks_deps::bevy_ecs::schedule::IntoScheduleConfigs;
use lgwks_std::hash::Hasher;

use crate::effect::{ActionId, EffectKey};
use crate::error::{BotError, Escaped};
use crate::verb::Observe;

#[cfg(doc)]
use super::bot::EcsBot;
use super::identity::OUTPUT_IDENTITY_DOMAIN;
use super::resources::TickError;

/// Compare two erased outputs as `S::Output`.
///
/// A downcast that fails is reported as *different* rather than equal: treating
/// an unreadable value as unchanged would silently suppress an effect, and the
/// failure mode this whole substrate is built to avoid is an effect that does
/// not happen with nothing to show for it.
/// The admitted-input identity of an observation, and how a returning one reads.
///
/// The two fields answer two different questions (issue #129). `identity` is
/// the content identity a digest binds: the same value of the same type derives
/// the same bytes (so a restart retires work that already landed), and a
/// different value derives different ones (so a restart does not read new work
/// as already applied). `event` says whether those bytes name an *event*
/// rather than only content — see [`InputIdentity::names_an_event`].
///
/// Keeping them apart is what lets a state watch `0 → 1 → 0` fire three times
/// while a redelivered [`EventId`] still retires.
#[derive(Clone, Copy)]
pub(crate) struct AdmittedInput {
    /// The content identity a dispatch digest binds.
    pub(super) identity: [u8; 16],
    /// Whether `identity` names an event rather than only content.
    pub(super) event: bool,
}

/// The admitted-input identity of an erased binding.
///
/// A type-qualified content identity: the same value of the same type always
/// derives the same bytes (so a restart retires work that already landed), and
/// a different value derives different ones (so a restart does not read new
/// work as already applied). Two *events* with equal content must still be
/// distinguishable, which is the caller's to express via
/// [`EventId`] or their own [`InputIdentity`] impl —
/// content equality alone is not event identity (issue #101).
pub(crate) fn identify_output<S>(value: &dyn Any) -> AdmittedInput
where
    S: Observe,
    S::Output: InputIdentity + 'static,
{
    let mut hasher = Hasher::new();
    hasher
        .write_framed(OUTPUT_IDENTITY_DOMAIN)
        .write_framed(<S::Output as InputIdentity>::SCHEMA_ID);
    let event = match value.downcast_ref::<S::Output>() {
        Some(typed) => {
            typed.write_identity(&mut hasher);
            typed.names_an_event()
        }
        None => {
            // A downcast that fails means the binding is not what this chain
            // claims. Hash the miss rather than returning a default: a constant
            // identity would make every mismatched binding look like the same
            // admitted input. Reported as content-only, so the stricter of the
            // two retire readings applies and a mismatch is not quietly retired.
            hasher.write_framed(b"downcast-miss");
            false
        }
    };
    let digest = hasher.finalize();
    let mut wide = [0_u8; 16];
    wide.copy_from_slice(&digest.as_bytes()[..16]);
    AdmittedInput {
        identity: wide,
        event,
    }
}

/// Compare two erased outputs as `S::Output`.
///
/// A downcast that fails is reported as *different* rather than equal: treating
/// an unreadable value as unchanged would silently suppress an effect, and the
/// failure mode this whole substrate is built to avoid is an effect that does
/// not happen with nothing to show for it.
pub(crate) fn same_output<S>(left: &dyn Any, right: &dyn Any) -> bool
where
    S: Observe + 'static,
    S::Output: PartialEq + InputIdentity + 'static,
{
    match (
        left.downcast_ref::<S::Output>(),
        right.downcast_ref::<S::Output>(),
    ) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

/// Whether the tick is parked, so its steps commit and run nothing.
///
/// A **domain** failure parks a tick: the substrate could not act on what a chain
/// told it, and the tick's contract is that such a tick commits nothing and fires
/// nothing. A **cancelled** poll does not, and that distinction is the whole of
/// T06's slow-source half. A source that stopped answering said nothing, so the
/// chains beside it have real observations to commit — withholding them because
/// one unrelated source was slow is exactly the starvation the per-poll deadline
/// exists to prevent.
///
/// The report still names every cancelled chain, and the tick still returns the
/// typed cancellation; what it does not do is stop the bot.
pub(super) fn parked(world: &World) -> bool {
    world.get_resource::<TickError>().is_some_and(|error| {
        error
            .0
            .as_ref()
            .is_some_and(|error| !matches!(error, BotError::PollStalled { .. }))
    })
}

// ── The work ledger: eligible work, separate from change detection ─────────

/// One `(chain, entry)` slot of a chain.
///
/// An address, not an identity, and the distinction is the whole of what
/// [`PendingWork`] exists to carry. A slot is reused by every generation over
/// its chain and by every attempt within one, so two `WorkId`s comparing equal
/// says only that the *address* is the same — never that the work standing
/// there is. It names a place in a report; it does not settle one.
///
/// It is deliberately not constructible outside this crate. That is not what
/// makes settlement safe — a [`PendingWork`] is what does — but a caller that
/// could mint one could name work it never observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct WorkId {
    /// The chain's index, in declaration order.
    pub(super) chain: usize,
    /// The entry's index within its chain.
    pub(super) entry: usize,
}

impl WorkId {
    /// The chain's index, in declaration order.
    #[must_use]
    pub const fn chain(self) -> usize {
        self.chain
    }

    /// The entry's index within its chain.
    #[must_use]
    pub const fn entry(self) -> usize {
        self.entry
    }
}

/// Why an entry was given up on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AbandonReason {
    /// The attempt budget for this entry is spent. The effect definitely did
    /// not happen on any of the attempts made; nothing further is attempted
    /// without evidence, which `EcsBot::resolve_effect` supplies.
    AttemptsExhausted {
        /// How many attempts were made.
        attempts: u32,
    },
    /// The retry policy's total deadline would pass before the next attempt
    /// could begin. The effect definitely did not happen on any attempt made;
    /// like [`Self::AttemptsExhausted`], nothing further is attempted without
    /// evidence.
    DeadlineExhausted {
        /// How many attempts were made.
        attempts: u32,
        /// Bot-clock time from the first attempt to the instant the bot found
        /// the deadline spent: the last failure, or the tick a late retry came
        /// due on.
        elapsed: Duration,
    },
    /// The failure is one no retry can fix: a refused capability, a type
    /// mismatch behind an erased verb, a malformed action input. Retrying would
    /// spend an attempt to learn the same answer.
    Terminal,
}

/// What is holding one entry of a transition back.
///
/// The `Running` state the ledger tracks is called [`Self::Unrecorded`] here:
/// at rest — which is the only time a caller can observe it — "an attempt is
/// in flight" and "an attempt began and no outcome was ever recorded" are the
/// same fact, and the second is the one that decides what happens next.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TransitionHold {
    /// Never attempted. An entry ahead of it is unresolved, and the chain's
    /// declared order is part of what it means.
    NotStarted,
    /// An attempt began and no outcome was recorded — the tick that started it
    /// ended without one. Held exactly as an unknown outcome is: the effect may
    /// be live, so it is never attempted again without evidence.
    Unrecorded,
    /// An attempt on this entry's action was recorded as dispatched earlier in
    /// this run, and no outcome was ever recorded for it.
    ///
    /// Distinct from [`Self::Unrecorded`], which is an attempt *this* process
    /// began and whose outcome the same process never wrote, and from
    /// [`Self::OutcomeUnknown`], which is an attempt it ran and watched become
    /// indeterminate. This one was found, not made: the bot was assembled
    /// against a journal that already said the bytes may be live, and the
    /// substrate will not begin another attempt on that action until a caller
    /// settles it. The key to settle it with is
    /// [`PendingWork::key`].
    UnsettledByRecovery {
        /// The action whose earlier attempt never settled.
        action: ActionId,
    },
    /// The effect definitely did not happen, and the attempt budget remains.
    Failed {
        /// When, on the bot's declared clock, the policy's backoff ends. No
        /// tick attempts the entry before then; the first tick at or after it
        /// does.
        retry_at: Duration,
        /// Attempts made so far, including the failed one.
        attempts: u32,
        /// The budget this entry is allowed.
        budget: u32,
        /// The failure the attempt produced.
        cause: String,
    },
    /// The effect may or may not have happened. Never attempted again without
    /// evidence.
    OutcomeUnknown {
        /// Attempts made so far, including the indeterminate one.
        attempts: u32,
        /// Why the outcome could not be settled.
        cause: String,
    },
    /// The action produced a known outcome and the journal refused to record
    /// it. Only the append is retried; the action is never re-entered.
    RecordingFailed {
        /// What the action did — the occurrence is a fact.
        evidence: EffectEvidence,
        /// Why the last append was refused.
        cause: String,
    },
    /// Given up on, and reported rather than dropped.
    Abandoned {
        /// Why it was given up on.
        reason: AbandonReason,
        /// The failure that ended it.
        cause: String,
    },
}

impl fmt::Display for TransitionHold {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The scrutinee is `*self`, so each pattern's type is the enum's own
        // type rather than a reference to it, and the causes are bound by
        // `ref`. They are rendered through `Escaped`, which is what keeps a
        // foreign payload from forging a log record; the pass is idempotent
        // (see `error::Escaped`), so a cause that arrived already escaped is
        // rendered exactly once.
        match *self {
            Self::NotStarted => f.write_str("not attempted: an entry ahead of it is unresolved"),
            Self::Unrecorded => {
                f.write_str("an attempt began and no outcome was recorded: the effect may be live")
            }
            Self::UnsettledByRecovery { action } => write!(
                f,
                "may have happened: an earlier attempt on action {action} was dispatched \
                 and nothing recorded its outcome, so nothing is sent again until it is \
                 settled"
            ),
            Self::Failed {
                retry_at,
                attempts,
                budget,
                ref cause,
            } => write!(
                f,
                "did not happen: attempt {attempts} of {budget} failed with {}; \
                 retried from {retry_at:?} on the bot clock",
                Escaped(cause)
            ),
            Self::OutcomeUnknown {
                attempts,
                ref cause,
            } => write!(
                f,
                "may have happened: attempt {attempts} was indeterminate ({})",
                Escaped(cause)
            ),
            Self::RecordingFailed {
                evidence,
                ref cause,
            } => write!(
                f,
                "the effect is {evidence} and only its record is missing: {}. \
                 The journal append is retried; the action is not",
                Escaped(cause)
            ),
            Self::Abandoned { reason, ref cause } => match reason {
                AbandonReason::AttemptsExhausted { attempts } => write!(
                    f,
                    "given up on after {attempts} attempts: {}",
                    Escaped(cause)
                ),
                AbandonReason::DeadlineExhausted { attempts, elapsed } => write!(
                    f,
                    "given up on after {attempts} attempts in {elapsed:?}: the next would \
                     begin past the retry deadline: {}",
                    Escaped(cause)
                ),
                AbandonReason::Terminal => {
                    write!(f, "given up on, no retry can fix: {}", Escaped(cause))
                }
            },
        }
    }
}

/// One entry of a transition that is not finished, and the attempt it is about.
///
/// This is the report a caller reads, and [`Self::key`] is what a settlement is
/// made against. The two are one value because they are two halves of one fact:
/// the address ([`WorkId`]) tells a caller *where* to look in its own books and
/// the key tells the substrate *which* attempt, over *which* observed input, the
/// evidence is about.
///
/// The key is why settlement takes this value's key rather than the slot. A
/// chain retries within one generation, so `NotApplied` for the first attempt
/// makes the entry eligible again and a second attempt runs against the same
/// address over the same binding. A report about the first attempt that is
/// *delivered twice* — which settlement is explicitly designed to tolerate,
/// because a caller that never saw its first delivery acknowledged has to be
/// able to repeat it — would otherwise land on the second attempt and make an
/// attempt whose effect may be live eligible to run a third time. Naming the
/// attempt is what separates "send that again" from "that was about work that is
/// over".
///
/// [`Self::key`] is `None` for an entry that has never been attempted. There is
/// then nothing for evidence to settle, and the absence is the same absence
/// rather than a zero attempt that would have to be special-cased at every
/// settlement site.
///
/// The fields are private and there is no public constructor, so a caller can
/// only ever settle work it read from [`EcsBot::pending`]. That is a seal, not
/// a formality: it makes "settle an attempt I invented" unrepresentable rather
/// than merely refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingWork {
    /// Which entry this is.
    pub(super) id: WorkId,
    /// The attempt this entry is on, and the four facts that name it.
    ///
    /// `None` when the entry has not been attempted, which is the only state
    /// that has no attempt identity to hand out.
    ///
    /// Boxed because a key is a fixed-width identity — seven fields, 128 bytes
    /// — and this type is embedded in [`BotError::PendingTransition`], which is
    /// returned by value from every tick that leaves work unfinished. Held
    /// inline, one key would put that error over the size at which the
    /// `result_large_err` lint fires for every caller in the workspace; the
    /// indirection costs one allocation on a path that already produces a
    /// report, and buys back the error's size for all of them.
    ///
    /// [`BotError::PendingTransition`]: crate::BotError::PendingTransition
    pub(super) key: Option<Box<EffectKey>>,
    /// What is holding it.
    pub(super) hold: TransitionHold,
}

impl PendingWork {
    /// Which entry this is.
    ///
    /// The address in the bot's own declarations, not an identity: a slot is
    /// reused by every generation over its chain, so two reports comparing
    /// equal here say only that the *place* is the same. [`Self::key`] is what
    /// names the work.
    #[must_use]
    pub const fn id(&self) -> WorkId {
        self.id
    }

    /// The attempt this entry is on, when there has been one.
    ///
    /// This is the value [`EcsBot::resolve_effect`] takes. It carries the run,
    /// the action, the attempt, the flow, the binding, the environment and the
    /// generation, and all seven are compared before anything is read or
    /// written — which is what makes the delivery safe to repeat.
    ///
    /// `None` means nothing has been attempted on this entry yet, so there is
    /// no attempt for evidence to be about.
    #[must_use]
    pub fn key(&self) -> Option<EffectKey> {
        self.key.as_deref().copied()
    }

    /// What is holding it back.
    #[must_use]
    pub const fn hold(&self) -> &TransitionHold {
        &self.hold
    }
}

impl fmt::Display for PendingWork {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Written in two passes rather than through an intermediate `String`:
        // this renders on every tick that leaves work unfinished, and a
        // diagnostic that allocates twice to say which entry is stuck is a cost
        // paid on the path that is already the unhappy one.
        write!(f, "chain {} entry {} (", self.id.chain(), self.id.entry())?;
        match self.key.as_deref() {
            Some(key) => write!(
                f,
                "attempt {}, binding {}",
                key.attempt().get(),
                key.digest()
            )?,
            None => f.write_str("no attempt yet")?,
        }
        write!(f, "): {}", self.hold)
    }
}

/// What a caller knows about an effect the substrate could not settle.
///
/// Two arms, because there are two facts a caller can establish, and the
/// decision that matters — attempt it again or not — follows from which one
/// they are. "Unknown and staying unknown" is not evidence and has no arm: an
/// entry in that state stays reported, which is the honest outcome.
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
#[rkyv(attr(non_exhaustive), crate = lgwks_std::wire::rkyv, compare(PartialEq), derive(Debug))]
#[non_exhaustive]
pub enum EffectEvidence {
    /// The effect happened. The entry is acknowledged and never attempted
    /// again.
    Applied,
    /// The effect did not happen. This is the evidence a retry requires: the
    /// entry becomes eligible for an attempt again, with a fresh budget,
    /// because a new fact is not a repeat of the attempt that failed.
    NotApplied,
}

/// Renders the fact the caller established, not the enum arm, because these
/// appear in refusal messages a person reads to decide whether their own
/// observation was wrong: `applied` and `not applied` are what they were asked
/// for, and `Applied`/`NotApplied` would make them translate.
///
/// Lives here rather than in `error.rs` for the same reason
/// [`PendingWork`]'s does — the type owns its rendering, so a later variant
/// cannot be interpolated anywhere unhandled.
impl fmt::Display for EffectEvidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Matched through a dereference rather than through the reference: the
        // type is `Copy`, and `clippy::pattern_type_mismatch` is forbidden in
        // this workspace.
        f.write_str(match *self {
            Self::Applied => "applied",
            Self::NotApplied => "not applied",
        })
    }
}
