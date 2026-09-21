//! The four verbs on a `bevy_ecs` substrate.
//!
//! [`Bot`](crate::Bot) executes a spec by polling every source on every tick and
//! evaluating every condition on the freshly polled value. That is correct and
//! it re-derives an answer it already had: a source that did not move is
//! evaluated anyway. This module keeps the same four verbs and the same
//! `(condition, action)` tuples, and moves the *execution* onto an ECS where the
//! condition is bevy's own change detection.
//!
//! # The mapping
//!
//! | Verb | On this substrate |
//! |---|---|
//! | `GrantSet` | the `Grants` resource: one authority per world |
//! | `Observe` | a `Chains` entry, polled by the `observe_fold` system |
//! | `Evaluate::check` | `Changed<Revision>` on the source entity |
//! | `Execute::execute_action` | the `fire_plan` system, whose effects run between schedule steps |
//! | `Bot::tick()` | [`EcsBot::tick_async`], one `Schedule::run` between two awaited phases |
//!
//! # One execution path, and who drives it
//!
//! A tick is four phases, and only the middle one is a synchronous schedule
//! step:
//!
//! 1. **observe** — every source is polled, in bounded waves, awaited on the
//!    caller's executor.
//! 2. **decide** — one `Schedule::run` folds those observations into the world
//!    (committing each value and bumping `Revision` where it moved) and records
//!    the effect program for this tick.
//! 3. **act** — the recorded effects run in declaration order, awaited on the
//!    caller's executor.
//! 4. **report** — the count and the first failure are written back.
//!
//! [`EcsBot::tick_async`] is the whole of it, and it is the way to run a bot
//! from async code: the verb futures are awaited *by* the caller's runtime, so
//! a source that waits on a timer, a socket, or a sibling task has a driver
//! making progress underneath it. [`EcsBot::tick`] is a synchronous adapter
//! over the same four phases for verbs that need no reactor; it is refused,
//! not hung, when an async runtime is already driving the calling thread.
//!
//! # One semantic delta, stated rather than discovered
//!
//! [`Bot::tick`](crate::Bot::tick) evaluates a condition on **every** tick.
//! `EcsBot::tick` evaluates it only on a tick where the observed value
//! **moved**. For a condition that stays true ("status is 500" while the
//! endpoint stays down) the first fires on every tick and the second fires
//! once, on the transition. That is the point of the substrate, and it is a
//! behaviour change rather than a re-implementation: a bot that must act on a
//! *held* state belongs on `Bot`, and a bot that acts on a *transition* belongs
//! here.
//!
//! # Work outlives the change filter
//!
//! `Changed<Revision>` says a chain is *eligible*; it is not a record of what
//! still has to happen. Committing the revision and then failing halfway marks
//! the source handled, so the entries the failure skipped are never reached on
//! an unchanged source, and a failure in the first chain stops every later
//! chain as well, because the loop that stops is outside the per-chain one.
//!
//! Eligible work therefore lives in its own structure — the ledger — keyed by
//! `(chain, entry)` and by the revision that made it due, and `fire` walks
//! *that* rather than the change set. The filter is consulted only to open a
//! transition; a chain with work outstanding is walked on every tick until the
//! work is settled, whether or not its source moves. One transition per chain
//! at a time: a source that moves again while its work is outstanding joins the
//! transition in progress, because the observed value is the only one the
//! substrate keeps (`Box<dyn Any>` is compared, not cloned) and a second
//! transition would replay effects the first already acknowledged.
//!
//! An entry is never skipped to reach a later one. The walk resumes at the
//! first entry that is not resolved and stops at the first it cannot settle, so
//! an effect that already happened is never replayed to get past a successor —
//! the failure mode where retrying a refused second entry duplicates the first
//! entry's merge. An entry that *may* have happened
//! ([`BotError::EffectIndeterminate`]) is held outright: it is attempted again
//! only when the caller supplies evidence, through `EcsBot::resolve_effect`.
//!
//! A tick is clean only when nothing is left holding a transition. When
//! something is, `EcsBot::tick` reports it — the entry, what is holding it, how
//! many entries are outstanding — so a clean subsequent tick cannot be read as
//! "the transition was handled". An error a source or an action produced keeps
//! precedence over that report, because it carries the typed variant a retry
//! classifier reads; the abandonment it caused is named by `EcsBot::pending`,
//! which lists every entry that is not finished, given-up entries included, with
//! the reason and the source revision. A tick that gives up on work is never
//! clean, and an entry it gave up on is never silently dropped.
//!
//! # Limits
//!
//! A panic that unwinds out of an action's future unwinds out of `fire` and out
//! of `tick` with it. The walk holds the chain's transition while it runs, so
//! that unwind loses the transition for the chain in flight — the entries it had
//! already resolved with it. Nothing in this module catches an unwind, and the
//! in-flight record it writes before an attempt (`EntryState::Unrecorded`) is
//! therefore durable for a tick that returns, not for a thread that panics. A
//! caller that catches an unwind around `tick` must treat that chain's work as
//! unknown rather than as handled.
//!
//! # What is measured, and what is deliberately not done
//!
//! Three findings from the measurement behind this module (`bevy_ecs` 0.19.1,
//! `default-features = false, features = ["std"]`), each with a control:
//!
//! - **`NonSend` is the only route for a verb.** `Component: Send + Sync +
//!   'static` is unconditional in this version and there is no `non_send`
//!   feature to relax it, so a `Box<dyn Any>` (which the verb erasure produces
//!   and which is not `Send`) cannot be a component. The observers, the
//!   entries and the observed values all live in `NonSend` resources, reachable
//!   only from an exclusive system. The crate's non-`Send` contract is
//!   preserved rather than tightened.
//! - **`Changed<T>` is visible to a second exclusive system in the same tick,
//!   and it is precise.** Against an input that holds still the change filter
//!   matched `[2, 0, 2, 0, 2]` (the ticks where the value actually moved),
//!   which is the whole reason a `Revision` marker exists rather than a
//!   "this system ran" flag.
//! - **Effects do not go through `Commands`.** `auto_insert_apply_deferred`
//!   defaults to `true` but inserts the sync point only where some system is
//!   ordered *after* the writer; a `Commands`-writing system with no successor
//!   has its queue dropped under a manually stepped `Schedule::run`, measured
//!   as 0 effects where 3 were expected. Adding a bare `ApplyDeferred` is not a
//!   fix either: unordered relative to the writer it runs first and defers the
//!   effect by a tick, giving 2 of 3. Both are silent. Here the effect is a
//!   direct resource write from an exclusive system, ordered by `.chain()`.
//!
//! # One path
//!
//! This is how a bot executes, not a candidate the caller may decline. There is
//! no `ecs` feature: `Bot` *is* this, and the project's build and test gate
//! compiles and tests it like any other code. A default-off flag would have left
//! it unexercised and unowned.
//!
//! # Build-time validation
//!
//! `EcsObserveBuilder::build` runs `Schedule::initialize()` with
//! `ambiguity_detection: LogLevel::Error`, so a schedule whose systems cannot be
//! totally ordered is a refusal at build rather than a silent misordering at
//! tick. An exclusive system cannot return an error, so a failure at *tick* time
//! is recorded in the `TickError` resource and surfaced by
//! `EcsBot::tick`: the same error ordering `Bot::tick` documents, where the
//! first error in declaration order is returned.

use std::any::Any;
use std::fmt;
use std::num::NonZeroU32;

// `self` is load-bearing: the `Component` and `Resource` derives expand to
// `bevy_ecs::…` paths, so the crate name has to be in scope at the use site even
// though every edge in this crate is written through the `lgwks_deps` facade.
use lgwks_deps::bevy_ecs::{
    self,
    prelude::{Changed, Component, Entity, Resource, World},
    schedule::{
        IntoScheduleConfigs, LogLevel, Schedule, ScheduleBuildSettings, SingleThreadedExecutor,
    },
};

use super::cap::{Deficit, Demand, Shortage};
use super::error::{BotError, DispatchCertainty, Escaped, RetryClass};
use super::gate::GrantSet;
use super::spec::{ChainEntry, Erased, ObserveAny, Witness, typed_entry};
use super::verb::{Evaluate, Execute, Observe};

// ── Components: identity and the change marker are separate ────────────────

/// A source's identity in the world.
///
/// Written once at spawn and never again. `docs/bot-on-ecs.md` §4 is the reason
/// this is not fused with the observed value: a component that carries both is
/// marked changed by every poll, and `Changed<T>` degenerates to "always true".
#[derive(Component, Debug, Clone)]
pub(crate) struct SourceId {
    /// The chain's index in declaration order; the key into `Chains` and
    /// `Observed`.
    pub(crate) chain: usize,
    /// The observer's own `domain_id()`, for diagnostics.
    domain: String,
}

impl SourceId {
    /// The observed source's domain identifier (e.g. `"net::endpoint"`).
    pub(crate) fn domain(&self) -> &str {
        &self.domain
    }
}

/// The change marker, bumped only when the observed value actually differs.
///
/// This is the component a condition filters on. It is one component for every
/// domain rather than one per domain, because the *value* is heterogeneous and
/// only the fact of its movement is shared, which keeps the archetype count
/// bounded, as `docs/bot-on-ecs.md` §4 requires.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Revision(u64);

// ── Resources ──────────────────────────────────────────────────────────────

/// The capability proof, as a world resource: one authority per world, no
/// ambient singleton.
#[derive(Resource, Debug)]
struct Grants(GrantSet);

/// Effects fired on the most recent tick.
#[derive(Resource, Debug, Default)]
struct Fired(usize);

/// The first tick-time failure, if any.
///
/// An exclusive system returns `()` and cannot propagate, so the error is
/// parked here and `EcsBot::tick` takes it. This is not a workaround for a
/// missing feature: it is the shape that keeps the failure ordered, because the
/// systems stop at the first error and the driver reports that one.
#[derive(Resource, Debug, Default)]
struct TickError(Option<BotError>);

/// The source entities in chain order.
#[derive(Resource, Debug, Default)]
struct Order(Vec<Entity>);

/// The retry policy in force: one authority per world, like `Grants`.
#[derive(Resource, Debug)]
struct Policy(RetryPolicy);

// ── Non-send state: the verbs and the values ───────────────────────────────

/// One observation chain, holding the same erased halves a
/// [`Chain`](crate::Chain) does.
struct EcsChain {
    /// The observer. `Box<dyn ObserveAny>`'s `poll_any` is not `Send`, which is
    /// why this cannot live in a component.
    source: Box<dyn ObserveAny>,
    /// Equality for this observer's output, captured from the `PartialEq` bound
    /// on `EcsBuilder::observe`. Change detection needs to tell "the same
    /// value again" from "a new value", and the erased `Box<dyn Any>` cannot
    /// answer that on its own.
    same: fn(&dyn Any, &dyn Any) -> bool,
    /// The `(condition, action)` tuples, in declaration order.
    entries: Vec<ChainEntry>,
    /// What type this chain's source produces, taken where `S::Output` was
    /// still a type parameter and checked at every rendezvous.
    ///
    /// The chain is the *claim*: it is what the condition and the action were
    /// built against. The value that arrives each tick carries its own witness,
    /// and the rendezvous compares them. Neither half can be checked alone —
    /// both are erased — so the pair is the proof.
    witness: Witness,
}

/// Every chain, in declaration order.
#[derive(Default)]
struct Chains(Vec<EcsChain>);

/// The observed value per chain, from the most recent successful poll.
///
/// Paired to [`Chains`] by index, and by index only: a vector position carries
/// no type. Nothing here proves that `Observed[i]` belongs to `Chains[i]`, which
/// is why each value keeps its [`Witness`] and the fold compares it against the
/// chain's before committing anything.
#[derive(Default)]
struct Observed(Vec<Option<Erased>>);

// ── Staging between the awaited phases and the schedule steps ──────────────
//
// The verb futures are non-`Send` and may need an executor, so they are awaited
// by the driver *outside* the schedule. What crosses into the schedule is
// therefore data, in declaration order, and the two resources below are that
// handover. Both are overwritten wholesale at the point they are consumed, so
// a tick that is dropped between phases leaves nothing behind for the next one.

/// The results of the observation phase, staged for `observe_fold`.
///
/// In chain order, one entry per chain, exactly as `poll_sources` collected
/// them: the fold commits them positionally, so a result that arrived early
/// cannot be committed to the wrong chain.
#[derive(Default)]
struct Polled(Vec<Result<Erased, BotError>>);

/// What the decision phase decided for one entry it reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    /// The condition held, so the entry is attempted. `attempts` is the attempt
    /// number this one will be, counted from the attempts the entry has already
    /// spent, and it is carried here rather than re-derived so the program the
    /// driver runs is the one the decision phase recorded.
    Attempt {
        /// Which attempt this will be.
        attempts: u32,
    },
    /// The condition did not hold, so the entry is recorded as skipped and the
    /// walk moves past it. Recorded rather than dropped: an entry whose
    /// condition is false owes nothing, and saying so is what lets a chain
    /// settle and stop being walked.
    Skip,
}

/// One entry the decision phase reached.
struct Step {
    /// Index into [`Chains`].
    chain: usize,
    /// Index into that chain's `entries`, in declaration order.
    entry: usize,
    /// What the walk decided for it.
    decision: Decision,
}

/// The effect program the decision phase recorded, plus the condition failures
/// it met on the way.
///
/// A failure is carried here rather than parked in [`TickError`] because it
/// happened at a *position* in the walk rather than at the end of it: the
/// [`Step`]s alongside it are exactly the effects the walk reached before that
/// position, and they still run. Whichever failure comes first in walk order is
/// the one the tick reports, which is the ordering the synchronous loop had when
/// it evaluated a condition and ran an action in the same iteration.
///
/// One failure per chain rather than one for the tick, because a condition that
/// cannot be evaluated stops its own chain: the chains after it are still
/// walked, and a failing action earlier in a chain is a more specific report than
/// a failing condition later in the same one. That is also why the failures are
/// indexed rather than ordered — the `chain` field of each [`Step`] is the order
/// they are reported in.
#[derive(Default)]
struct Plan {
    /// The decisions to apply, in declaration order: one chain's entries in
    /// walk order, then the next chain's.
    steps: Vec<Step>,
    /// The condition failure each chain's walk met, indexed by chain. A chain
    /// whose walk met none has `None`, and an action failure recorded by the
    /// driver displaces the condition failure of the chain it happened in.
    failures: Vec<Option<BotError>>,
}

/// Upper bound on sources polled simultaneously by one tick. A source poll may
/// occupy one `spawn_blocking` thread, so this caps the blocking-thread fan-out
/// regardless of how many chains a spec declares. Chains beyond the cap are
/// polled in additional waves.
const MAX_IN_FLIGHT_POLLS: usize = 32;

/// Compare two erased outputs as `S::Output`.
///
/// A downcast that fails is reported as *different* rather than equal: treating
/// an unreadable value as unchanged would silently suppress an effect, and the
/// failure mode this whole substrate is built to avoid is an effect that does
/// not happen with nothing to show for it.
fn same_output<S>(left: &dyn Any, right: &dyn Any) -> bool
where
    S: Observe + 'static,
    S::Output: PartialEq + 'static,
{
    match (
        left.downcast_ref::<S::Output>(),
        right.downcast_ref::<S::Output>(),
    ) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

/// The parked error, if the tick has already failed.
fn parked(world: &World) -> bool {
    world
        .get_resource::<TickError>()
        .is_some_and(|error| error.0.is_some())
}

// ── The work ledger: eligible work, separate from change detection ─────────

/// The identity of one `(chain, entry)` tuple.
///
/// Identity, not position: the ledger is keyed by it, `resolve_effect` takes
/// it, and it means the same thing on every tick. It is deliberately not
/// constructible outside this crate — a caller that could mint one could
/// settle work that does not exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct WorkId {
    /// The chain's index, in declaration order.
    chain: usize,
    /// The entry's index within its chain.
    entry: usize,
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
    /// The effect definitely did not happen, and the attempt budget remains.
    Failed {
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
            Self::Failed {
                attempts,
                budget,
                ref cause,
            } => write!(
                f,
                "did not happen: attempt {attempts} of {budget} failed with {}",
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
            Self::Abandoned { reason, ref cause } => match reason {
                AbandonReason::AttemptsExhausted { attempts } => write!(
                    f,
                    "given up on after {attempts} attempts: {}",
                    Escaped(cause)
                ),
                AbandonReason::Terminal => {
                    write!(f, "given up on, no retry can fix: {}", Escaped(cause))
                }
            },
        }
    }
}

/// One entry of a transition that is not finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingWork {
    /// Which entry this is.
    id: WorkId,
    /// The revision that opened the transition it belongs to.
    revision: u64,
    /// What is holding it.
    hold: TransitionHold,
}

impl PendingWork {
    /// Which entry this is.
    #[must_use]
    pub const fn id(&self) -> WorkId {
        self.id
    }

    /// The revision that opened the transition this entry belongs to.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// What is holding it back.
    #[must_use]
    pub const fn hold(&self) -> &TransitionHold {
        &self.hold
    }
}

impl fmt::Display for PendingWork {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "chain {} entry {} (revision {}): {}",
            self.id.chain(),
            self.id.entry(),
            self.revision,
            self.hold
        )
    }
}

/// What a caller knows about an effect the substrate could not settle.
///
/// Two arms, because there are two facts a caller can establish, and the
/// decision that matters — attempt it again or not — follows from which one
/// they are. "Unknown and staying unknown" is not evidence and has no arm: an
/// entry in that state stays reported, which is the honest outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// How many times an entry whose effect definitely did not happen is attempted.
///
/// A policy rather than a constant, because the right number is a property of
/// the action: a merge that transiently refuses wants several attempts, a
/// process launch wants one. There is no measured basis for a default, so
/// [`Self::DEFAULT`] is a declared guess and is named as one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Attempts per entry, the first included. Non-zero: an entry that may
    /// never be attempted is not a policy, it is pre-abandoned work.
    max_attempts: NonZeroU32,
}

impl RetryPolicy {
    /// One attempt per entry: the first definite failure abandons it.
    pub const ONE_ATTEMPT: Self = Self {
        max_attempts: NonZeroU32::MIN,
    };

    /// The declared default: three attempts per entry, counting the first.
    pub const DEFAULT: Self = Self {
        // Total rather than panicking: a `const` has no error channel and
        // `unwrap` is refused crate-wide. The `None` arm is unreachable, since
        // three is not zero.
        max_attempts: match NonZeroU32::new(3) {
            Some(three) => three,
            None => NonZeroU32::MIN,
        },
    };

    /// A policy allowing `max_attempts` attempts per entry.
    #[must_use]
    pub const fn new(max_attempts: NonZeroU32) -> Self {
        Self { max_attempts }
    }

    /// Attempts allowed per entry.
    #[must_use]
    pub const fn max_attempts(self) -> u32 {
        self.max_attempts.get()
    }
}

/// The state of one entry within a transition.
#[derive(Debug, Clone)]
enum EntryState {
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
    },
    /// The effect may or may not have happened
    /// ([`BotError::EffectIndeterminate`]).
    OutcomeUnknown {
        /// Attempts made so far.
        attempts: u32,
        /// Why the outcome could not be settled.
        cause: String,
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
    fn is_open(&self) -> bool {
        matches!(
            self,
            Self::NotStarted
                | Self::Unrecorded
                | Self::DefinitelyFailed { .. }
                | Self::OutcomeUnknown { .. }
        )
    }

    /// Whether the entry was given up on: terminal for this transition, and
    /// reported rather than dropped.
    fn is_abandoned(&self) -> bool {
        matches!(self, Self::Abandoned { .. })
    }

    /// How this entry reads to a caller, `None` when it is resolved and has
    /// nothing to report.
    fn hold(&self, budget: u32) -> Option<TransitionHold> {
        match *self {
            Self::Succeeded | Self::Skipped => None,
            Self::NotStarted => Some(TransitionHold::NotStarted),
            Self::Unrecorded => Some(TransitionHold::Unrecorded),
            Self::DefinitelyFailed {
                attempts,
                ref cause,
            } => Some(TransitionHold::Failed {
                attempts,
                budget,
                cause: cause.clone(),
            }),
            Self::OutcomeUnknown {
                attempts,
                ref cause,
            } => Some(TransitionHold::OutcomeUnknown {
                attempts,
                cause: cause.clone(),
            }),
            Self::Abandoned { reason, ref cause } => Some(TransitionHold::Abandoned {
                reason,
                cause: cause.clone(),
            }),
        }
    }
}

/// The work of one source transition: one state per entry of its chain.
///
/// Deliberately neither `Clone` nor a derived `Debug`: it owns the observed
/// payload it is bound to, and that payload is erased. Copying a transition
/// would quietly copy the payload with it — the very copy the sources are not
/// required to support — and printing one would demand `Debug` of a type the
/// source never promised it for. The manual [`fmt::Debug`] below renders the
/// binding as its presence, which is the part a reader of a log wants.
struct Transition {
    /// The revision that opened it.
    revision: u64,
    /// One state per entry of the chain, in declaration order.
    entries: Vec<EntryState>,
    /// What evidence settled each entry, for the generation [`Self::revision`]
    /// names, indexed as [`Self::entries`] is.
    ///
    /// The record is what makes a settlement idempotent and what makes a
    /// contradicting one refusable, and neither is answerable from
    /// [`EntryState`] alone: `Applied` and an attempt that returned `Ok` both
    /// land on [`EntryState::Succeeded`], and `NotApplied` and an entry that
    /// has not been reached both land on [`EntryState::NotStarted`]. Without
    /// this, a caller whose first delivery was ambiguous cannot safely repeat
    /// it — the repeat would read as evidence about an entry that was never
    /// settled at all.
    ///
    /// Per generation, not per entry: it is cleared when a transition is opened
    /// or resumed, because a settlement is a statement about one attempt and it
    /// does not carry into the next.
    settled: Vec<Option<EffectEvidence>>,
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
    value: Option<Erased>,
}

impl fmt::Debug for Transition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Transition")
            .field("revision", &self.revision)
            .field("entries", &self.entries)
            .field("settled", &self.settled)
            .field("bound", &self.value.is_some())
            .finish()
    }
}

impl Transition {
    /// A transition with every entry outstanding, bound to `value`.
    fn opened(revision: u64, entries: usize, value: Option<Erased>) -> Self {
        Self {
            revision,
            entries: vec![EntryState::NotStarted; entries],
            settled: vec![None; entries],
            value,
        }
    }

    /// Continue into a new revision, from a transition with nothing open.
    ///
    /// Every entry is outstanding again for the new value, *except* the ones
    /// given up on: those are terminal until evidence revives them, and
    /// carrying their record forward is what keeps a lost effect reported
    /// instead of silently dropped the moment the source moves.
    fn resumed(revision: u64, previous: &Self, value: Option<Erased>) -> Self {
        Self {
            revision,
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
            settled: vec![None; previous.entries.len()],
            value,
        }
    }

    /// Whether any entry is still unresolved.
    fn has_open(&self) -> bool {
        self.entries.iter().any(EntryState::is_open)
    }

    /// Whether the ledger must keep this transition: it has work to do, or a
    /// fact to report.
    fn is_retained(&self) -> bool {
        self.entries
            .iter()
            .any(|state| state.is_open() || state.is_abandoned())
    }
}

/// What a settlement did to the ledger.
///
/// Four answers, because a caller's next move differs for each: [`Decided`] and
/// [`Duplicate`] are both success, and the other three are three distinct
/// refusals that must not be collapsed into one. Only [`NoSuchWork`] means
/// "there is no held effect at this address"; a caller told that about an
/// address it has a `PendingWork` for is being told its evidence is stale or
/// contradictory, which is a different repair.
///
/// [`Decided`]: Settled::Decided
/// [`Duplicate`]: Settled::Duplicate
/// [`NoSuchWork`]: Settled::NoSuchWork
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Settled {
    /// The named generation's entry was outstanding, and this evidence is now
    /// its outcome.
    Decided,
    /// The named generation was already given exactly this evidence for this
    /// entry. A caller that could not tell whether its first delivery landed
    /// repeats it and must not be punished for the repetition, nor may the
    /// repeat move the entry a second time.
    Duplicate,
    /// The named generation is not the one in the slot now: the work the caller
    /// is reporting on was superseded, and the live transition is about a
    /// different attempt with a different revision.
    Superseded {
        /// The revision the slot holds now.
        current: u64,
    },
    /// The named generation's entry is settled, and this evidence says the
    /// opposite of what settled it.
    Contradicted {
        /// The evidence already recorded for this generation.
        settled: EffectEvidence,
    },
    /// No transition at this chain, or no such entry in the one that is there.
    NoSuchWork,
}

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
#[derive(Debug, Default)]
struct Ledger {
    /// One live transition per chain, in declaration order.
    transitions: Vec<Option<Transition>>,
}

impl Ledger {
    /// A ledger with one idle slot per chain.
    fn with_chains(chains: usize) -> Self {
        Self {
            transitions: (0..chains).map(|_| None).collect(),
        }
    }

    /// Take a chain's transition out, leaving its slot empty. The walk needs it
    /// owned: the ledger is mutated while the chains, the observed values and
    /// the granted authority are borrowed from the same world.
    fn take(&mut self, chain: usize) -> Option<Transition> {
        self.transitions.get_mut(chain).and_then(Option::take)
    }

    /// Put a chain's transition back, `None` when nothing is retained.
    fn put(&mut self, chain: usize, transition: Option<Transition>) {
        if let Some(slot) = self.transitions.get_mut(chain) {
            *slot = transition;
        }
    }

    /// The entry a [`WorkId`] names, when the ledger holds it.
    fn entry_mut(&mut self, id: WorkId) -> Option<&mut EntryState> {
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
    /// `false` when the entry is not in a state an attempt can start from, which
    /// means the world moved behind the schedule's back.
    fn begin(&mut self, id: WorkId) -> bool {
        let Some(state) = self.entry_mut(id) else {
            return false;
        };
        if !matches!(
            *state,
            EntryState::NotStarted | EntryState::DefinitelyFailed { .. }
        ) {
            return false;
        }
        *state = EntryState::Unrecorded;
        true
    }

    /// Record that the entry's condition did not hold, so it owes nothing.
    fn skip(&mut self, id: WorkId) -> bool {
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
    fn succeed(&mut self, id: WorkId) -> bool {
        let Some(state) = self.entry_mut(id) else {
            return false;
        };
        if !matches!(*state, EntryState::Unrecorded) {
            return false;
        }
        *state = EntryState::Succeeded;
        true
    }

    /// Record what a failed attempt means for the entry it was made on.
    fn fail(&mut self, id: WorkId, error: &BotError, attempts: u32, budget: u32) -> bool {
        let Some(state) = self.entry_mut(id) else {
            return false;
        };
        // Only an attempt in flight can fail: an outcome recorded for an entry
        // that was never begun is not this tick's to write.
        if !matches!(*state, EntryState::Unrecorded) {
            return false;
        }
        *state = failure_state(error, attempts, budget);
        true
    }

    /// Settle an entry whose effect may or may not have happened.
    ///
    /// The caller names the generation the evidence is about — the revision the
    /// [`PendingWork`] they read carried — and nothing is read or written until
    /// it matches the transition's own. This is not a formality. A chain has one
    /// transition at a time and a slot is reused by every generation over it, so
    /// a `(chain, entry)` that matches says only that the *address* is the same;
    /// it says nothing about which attempt the caller is reporting on. A report
    /// computed against revision N and delivered after the slot was re-opened at
    /// revision N+1 would otherwise be accepted against work the caller has
    /// never seen: `Applied` would acknowledge an effect at a generation that
    /// never happened, and `NotApplied` — worse, because it authorises a retry —
    /// would make an unknown attempt eligible to run again.
    ///
    /// Returns a decision about the named generation, not a bool, because the
    /// four answers lead a caller to four different next actions and two of them
    /// are not errors.
    fn settle(&mut self, id: WorkId, revision: u64, evidence: EffectEvidence) -> Settled {
        let Some(transition) = self
            .transitions
            .get_mut(id.chain())
            .and_then(Option::as_mut)
        else {
            return Settled::NoSuchWork;
        };
        // The generation first, before the entry is even looked up: an entry
        // that matches inside a transition that does not is exactly the case
        // this exists to refuse.
        if transition.revision != revision {
            return Settled::Superseded {
                current: transition.revision,
            };
        }
        let recorded = transition.settled.get(id.entry()).copied().flatten();
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
            return match recorded {
                Some(previous) if previous == evidence => Settled::Duplicate,
                Some(previous) => Settled::Contradicted { settled: previous },
                None => Settled::NoSuchWork,
            };
        }
        let next = match evidence {
            EffectEvidence::Applied => EntryState::Succeeded,
            EffectEvidence::NotApplied => EntryState::NotStarted,
        };
        if let Some(state) = transition.entries.get_mut(id.entry()) {
            *state = next;
        }
        if let Some(slot) = transition.settled.get_mut(id.entry()) {
            *slot = Some(evidence);
        }
        Settled::Decided
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
    fn bound(&self, chain: usize) -> Option<&Erased> {
        self.transitions
            .get(chain)
            .and_then(Option::as_ref)
            .and_then(|transition| transition.value.as_ref())
    }

    /// The first entry that is unresolved, in `(chain, entry)` order.
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
    /// Taking the first in declaration order is deliberate: the abandonment is
    /// the entry that explains every successor blocked behind it, so naming it
    /// is naming the reason.
    fn first_unresolved(&self, budget: u32) -> Option<PendingWork> {
        for (chain, transition) in self.transitions.iter().enumerate() {
            let Some(transition) = transition.as_ref() else {
                continue;
            };
            for (entry, state) in transition.entries.iter().enumerate() {
                if !(state.is_open() || state.is_abandoned()) {
                    continue;
                }
                // Unreachable in practice: an open or abandoned state always
                // renders a hold.
                let Some(hold) = state.hold(budget) else {
                    continue;
                };
                return Some(PendingWork {
                    id: WorkId { chain, entry },
                    revision: transition.revision,
                    hold,
                });
            }
        }
        None
    }

    /// Every entry that is not finished, in `(chain, entry)` order: open, or
    /// given up on and still reported.
    fn pending(&self, budget: u32) -> Vec<PendingWork> {
        let mut pending = Vec::new();
        for (chain, transition) in self.transitions.iter().enumerate() {
            let Some(transition) = transition.as_ref() else {
                continue;
            };
            for (entry, state) in transition.entries.iter().enumerate() {
                if let Some(hold) = state.hold(budget) {
                    pending.push(PendingWork {
                        id: WorkId { chain, entry },
                        revision: transition.revision,
                        hold,
                    });
                }
            }
        }
        pending
    }

    /// How many entries are still unresolved, across every chain: open, or
    /// abandoned and so still blocking whatever is behind them.
    ///
    /// Counted the same way [`Self::first_unresolved`] scans, because the two
    /// are rendered together in [`BotError::PendingTransition`]: a count that
    /// omitted abandoned entries would report a non-zero entry alongside an
    /// "0 outstanding", which reads as the one thing the pair is there to rule
    /// out — a chain that is somehow both stuck and finished.
    fn unresolved_count(&self) -> usize {
        self.transitions
            .iter()
            .flatten()
            .flat_map(|transition| transition.entries.iter())
            .filter(|state| state.is_open() || state.is_abandoned())
            .count()
    }
}

/// The revision the chain's source last moved to, or zero if it never has.
fn revision_of(world: &World, chain: usize) -> u64 {
    world
        .resource::<Order>()
        .0
        .get(chain)
        .and_then(|entity| world.get::<Revision>(*entity))
        .map_or(0, |revision| revision.0)
}

/// Move the newest observation for `chain` out of its slot.
///
/// Moved, not copied. A source's `Output` carries no `Clone` bound and this is
/// the reason it does not need one: the value is not wanted in two places at
/// once. Once a transition is bound to it, the transition is what speaks for
/// it, and the slot being empty is not a loss — it is the record that the value
/// is out on loan, which [`observe_fold`] reads back through the binding.
fn take_observed(world: &mut World, chain: usize) -> Option<Erased> {
    let mut observed = world.non_send_mut::<Observed>();
    observed.0.get_mut(chain).and_then(Option::take)
}

/// Whether the newest observation for `chain` should be admitted over the
/// payload a retained transition is bound to.
///
/// The comparison is the chain's own, the same one the change filter uses, so
/// "moved" means one thing in this substrate rather than two. It stands in for
/// a `Changed<Revision>` test rather than supplementing one: a movement that
/// arrives while the transition is open does bump the revision, but the change
/// filter is consumed on the tick it is seen, so by the time the transition
/// drains there is nothing left to consult — the observation itself is the only
/// surviving record that the source moved at all.
fn admits(world: &World, chain: usize, bound: Option<&Erased>) -> bool {
    let seen = world.non_send::<Observed>();
    let Some(next) = seen.0.get(chain).and_then(|slot| slot.as_ref()) else {
        // Nothing to admit. An empty slot beside a transition that has nothing
        // open is not a movement, and reading it as one would reopen the chain
        // on every tick and run its entries forever.
        return false;
    };
    let Some(bound) = bound else {
        // Bound to nothing, and now there is something: that is a movement.
        return true;
    };
    let chains = world.non_send::<Chains>();
    chains
        .0
        .get(chain)
        .is_some_and(|chain| !(chain.same)(bound.as_any(), next.as_any()))
}

/// The transition a chain should be walking this tick, if any.
///
/// Three ways one exists: work is outstanding (walked whether or not the source
/// moved — that is the whole point), the transition is kept only for an
/// abandonment record and the newest observation is not admitted over it, or a
/// transition is opened or resumed under an observation that is.
///
/// Opening and resuming are where the payload is bound, and both *take* it out
/// of the observation slot. That is the mechanism that keeps a transition on
/// one input: the entries it evaluates and runs read the binding, so a value
/// that arrives mid-transition cannot reach them, however many ticks the
/// transition takes to drain. The admission itself is then deferred rather than
/// dropped — the newer observation is still sitting in the slot, and the tick
/// after the transition stops being retained is the tick it becomes work.
fn resume(
    world: &mut World,
    chain: usize,
    moving: bool,
    held: Option<Transition>,
) -> Option<Transition> {
    match held {
        // Outstanding work keeps the payload it was opened under. A newer
        // observation is not admitted here, and deliberately: the entries
        // still running were acknowledged against this one.
        Some(transition) if transition.has_open() => Some(transition),
        // Nothing open: the transition is only a name for the work of a
        // generation that has finished. It gives way to the newest observation
        // when that observation has actually moved away from its binding, and
        // is otherwise kept exactly as it stands — abandonment record and all.
        Some(transition) if !admits(world, chain, transition.value.as_ref()) => Some(transition),
        Some(transition) => Some(Transition::resumed(
            revision_of(world, chain),
            &transition,
            take_observed(world, chain),
        )),
        None if moving => {
            let entries = world
                .non_send::<Chains>()
                .0
                .get(chain)
                .map_or(0, |chain| chain.entries.len());
            Some(Transition::opened(
                revision_of(world, chain),
                entries,
                take_observed(world, chain),
            ))
        }
        None => None,
    }
}

/// What a failed attempt means for the entry it was made on.
///
/// The three cases are the three things a caller has to be able to tell apart
/// afterwards: a retry is a retry, a retry may be a duplicate, and a retry is
/// pointless because the failure is not something another attempt can change. The
/// budget is consulted only for the first of them.
///
/// The classification is [`BotError::retry_class`] and nothing else. The error's
/// *variant* is not the question: `DomainError` is where a permanent refusal, a
/// connection that never opened, and a wiring defect all used to land, and they
/// want three different answers. Reading the rendered `cause` to tell them apart
/// is worse still — it makes a retry policy depend on the wording of a message.
/// The producer states what the failure establishes about the effect, the
/// certainty carries it here, and this function is a total map over that.
fn failure_state(error: &BotError, attempts: u32, budget: u32) -> EntryState {
    let cause = error.to_string();
    match error.retry_class() {
        // The effect definitely did not happen: a retry is a retry, and the
        // budget bounds it.
        RetryClass::Safe if attempts < budget => EntryState::DefinitelyFailed { attempts, cause },
        RetryClass::Safe => EntryState::Abandoned {
            reason: AbandonReason::AttemptsExhausted { attempts },
            cause,
        },
        // It may have happened. Never attempted again without evidence, whatever
        // the budget says.
        RetryClass::RequiresEvidence => EntryState::OutcomeUnknown { attempts, cause },
        // No retry fixes a refused capability, a parse refusal, or a type
        // mismatch — and the budget is untouched, so the disposition reports
        // what actually happened rather than "we ran out of attempts".
        RetryClass::Never => EntryState::Abandoned {
            reason: AbandonReason::Terminal,
            cause,
        },
    }
}

// ── Systems ────────────────────────────────────────────────────────────────

/// Observe, fold half: commit the polled values, then bump `Revision` for the
/// sources that moved.
///
/// Exclusive because the verbs are non-`Send`. Every source has already been
/// polled and awaited by the driver by the time this runs, so all that is left
/// is the commit — and a poll that failed is reported *before* any `Revision`
/// is written, which is `Bot::tick`'s documented error ordering: the first
/// error in declaration order, with no source left half-updated.
///
/// That the polls themselves happen outside is what makes a timer- or
/// socket-backed source work: this system is a synchronous step, and a source
/// awaiting a reactor inside it would have no reactor making progress.
fn observe_fold(world: &mut World) {
    if parked(world) {
        return;
    }

    let polled = std::mem::take(&mut world.non_send_mut::<Polled>().0);

    // `collect` into a `Result<Vec<_>, _>` keeps the first error and drops the
    // rest, which is the ordering `Bot::tick` promises. Nothing is committed on
    // failure, so a tick that errors leaves every observed value as it was.
    let values = match polled.into_iter().collect::<Result<Vec<_>, _>>() {
        Ok(values) => values,
        Err(error) => {
            world.resource_mut::<TickError>().0 = Some(error);
            return;
        }
    };

    // The rendezvous. `values` arrived in chain order because `poll_sources`
    // collected them that way, and the pairing from here on is by index — a
    // vector position, which carries no type. The witness is what proves the
    // two halves still agree, and it is checked *before* anything is committed
    // or compared, so a mis-pairing cannot reach a downcast.
    //
    // A miss is a defect in this crate — a chain whose index no longer names the
    // source it was built with — not a domain failure and not something a retry
    // can change. It is reported as `TypeMismatch`, which is classified terminal,
    // so it does not spend a budget; and it commits nothing, so the world is left
    // exactly as the previous tick left it.
    {
        let chains = world.non_send::<Chains>();
        let mismatch = values.iter().enumerate().find_map(|(index, next)| {
            let chain = chains.0.get(index)?;
            (!chain.witness.agrees_with(next.witness)).then_some(BotError::TypeMismatch {
                site: "observe_fold rendezvous",
                chain: Some(index),
                expected: chain.witness.name(),
                observed: next.witness.name(),
            })
        });
        if let Some(error) = mismatch {
            world.resource_mut::<TickError>().0 = Some(error);
            return;
        }
    }

    let changed: Vec<bool> = {
        let chains = world.non_send::<Chains>();
        let seen = world.non_send::<Observed>();
        let ledger = world.non_send::<Ledger>();
        values
            .iter()
            .enumerate()
            .map(|(index, next)| {
                // The comparison baseline is what the chain last acted on, and
                // once a transition owns the payload its slot is empty. Reading
                // that emptiness as "changed" would be a trap the whole binding
                // falls into: every tick after an admission would count as a
                // movement, bump the revision again, and open the chain afresh
                // forever. So the fall back is to the binding itself, which is
                // the same value by a different route.
                let baseline = seen
                    .0
                    .get(index)
                    .and_then(|slot| slot.as_ref())
                    .or_else(|| ledger.bound(index));
                match baseline {
                    // No remembered value, and no transition holding one: this
                    // source has, by definition, changed.
                    None => true,
                    Some(previous) => !(chains.0[index].same)(previous.as_any(), next.as_any()),
                }
            })
            .collect()
    };

    world.non_send_mut::<Observed>().0 = values.into_iter().map(Some).collect();

    let order = world.resource::<Order>().0.clone();
    for (index, entity) in order.iter().enumerate() {
        if !changed.get(index).copied().unwrap_or(false) {
            continue;
        }
        if let Some(mut revision) = world.get_mut::<Revision>(*entity) {
            revision.0 = revision.0.wrapping_add(1);
        }
    }
}

/// Fire, decide half: walk the eligible work of every chain, in declaration
/// order, and record the effects this tick should run.
///
/// The capability check is not repeated when the effect runs: `run_any` mints a
/// fresh `Auth` from the retained `GrantSet`, which is where the proof belongs.
///
/// The load-bearing word is *retained*. `assemble` clones the set into the world
/// as `Grants(grants.clone())` and nothing revokes it, so this substrate offers
/// exactly the snapshot boundary `Bot` documents and not a live lease: changing
/// or dropping the caller's `GrantSet` after build cannot narrow a bot that is
/// already running. An earlier version of this comment claimed the opposite.
///
/// Conditions are evaluated here, in the schedule, and the effects they select
/// are run by the driver afterwards. That is a reordering of *when* each verb
/// runs, not of which effects happen or in what order: a condition takes only
/// the observed value (`Evaluate::check` has no other input) and so cannot
/// observe an action's effect, which makes the selected list a pure function of
/// the observations. The plan is therefore the exact effect program the
/// synchronous loop would have walked.
///
/// What is walked is the *ledger*, not the change set. `Changed<Revision>`
/// decides only which chains *open* a transition; a chain whose ledger entry is
/// outstanding is walked whether or not it moved, which is what keeps an
/// unattempted effect from being lost when its source holds still. A failure
/// stops its own chain rather than every chain after it, so the chains behind a
/// failing one still record their work.
fn fire_plan(world: &mut World) {
    {
        let mut plan = world.non_send_mut::<Plan>();
        plan.steps.clear();
        plan.failures.clear();
    }
    if parked(world) {
        return;
    }

    let moved: Vec<usize> = {
        let mut query = world.query_filtered::<&SourceId, Changed<Revision>>();
        query.iter(world).map(|id| id.chain).collect()
    };

    let count = world.non_send::<Chains>().0.len();

    // A flag per chain rather than a search of `moved`: the walk below is in
    // declaration order, and membership has to be answerable in constant time
    // for that order to be the one that decides what runs.
    let mut moving = vec![false; count];
    for index in moved {
        if let Some(flag) = moving.get_mut(index) {
            *flag = true;
        }
    }

    let mut steps: Vec<Step> = Vec::new();
    let mut failures: Vec<Option<BotError>> = (0..count).map(|_| None).collect();

    for index in 0..count {
        let held = world.non_send_mut::<Ledger>().take(index);
        let Some(transition) = resume(
            world,
            index,
            moving.get(index).copied().unwrap_or(false),
            held,
        ) else {
            continue;
        };

        // The borrow of the world for the walk is scoped: `Ledger::take` above
        // and `Ledger::put` below each need it mutably, while the walk needs the
        // chains and the transition's binding immutably. Nothing is written to
        // the transition here — every state that depends on an attempt is
        // written by the driver, which is the only place that knows the outcome.
        {
            let chains = world.non_send::<Chains>();
            if let (Some(chain), Some(value), Some(failure)) = (
                chains.0.get(index),
                transition.value.as_ref(),
                failures.get_mut(index),
            ) {
                plan_chain(chain, &transition, value, index, &mut steps, failure);
            }
            // No chain, or a transition bound to nothing: the work is kept,
            // not discarded. A transition is opened only for a source that was
            // polled, so a chain with no value bound to it is a world that was
            // mutated behind the schedule's back.
        }

        let retained = transition.is_retained();
        world
            .non_send_mut::<Ledger>()
            .put(index, if retained { Some(transition) } else { None });
    }

    let mut plan = world.non_send_mut::<Plan>();
    plan.steps = steps;
    plan.failures = failures;
}

/// Record the decisions one chain's walk reaches, in entry order.
///
/// Only the decisions a walk can make without attempting anything are taken
/// here: an entry the walk is finished with, and an entry it reached whose
/// condition did not hold. Everything that depends on an attempt — marking the
/// entry in flight, and what its outcome means — is left to the driver, which is
/// the only place that knows it.
///
/// A skipped entry is recorded as a [`Decision::Skip`] rather than written to the
/// ledger here so that the condition behind it is evaluated against the value the
/// attempt will actually see. An entry the walk never reaches because an earlier
/// action failed keeps the state that makes it reachable, instead of being marked
/// decided by a condition evaluated ahead of the failure that stopped the walk
/// before it.
fn plan_chain(
    chain: &EcsChain,
    transition: &Transition,
    value: &Erased,
    index: usize,
    steps: &mut Vec<Step>,
    failure: &mut Option<BotError>,
) {
    for (entry_index, entry) in chain.entries.iter().enumerate() {
        let Some(state) = transition.entries.get(entry_index) else {
            continue;
        };
        match *state {
            // Decided, and the chain continues past it: an entry that ran, and
            // one whose condition was false. Both are facts about the entry that
            // leave the entry behind it with nothing standing in its way.
            EntryState::Succeeded | EntryState::Skipped => continue,
            // Given up on, and a *barrier* to everything behind it. This is the
            // line between declaration order and success dependency, and it is
            // the one this walk used to get wrong: an entry in a chain is a
            // prerequisite of the next, so an abandonment is the strongest
            // possible statement that the entry behind it must not run — the
            // draft was never reserved, so there is nothing to send. Continuing
            // past it completed a command whose prerequisite had been given up
            // on.
            //
            // The successors are not lost or forgotten by stopping here. They
            // stay `NotStarted`, so [`Ledger::pending`] keeps reporting them and
            // the tick that finds them names the abandonment in front of them.
            // The way past it is evidence: `NotApplied` revives the entry, and
            // the successors with it.
            EntryState::Abandoned { .. } => break,
            // Held: the effect may be live and only evidence settles that. A
            // later entry is not run ahead of it.
            EntryState::Unrecorded | EntryState::OutcomeUnknown { .. } => break,
            EntryState::NotStarted | EntryState::DefinitelyFailed { .. } => {}
        }

        // The condition reads the value as it stands *now*, which is why an
        // outstanding transition is walked on a tick where the source held
        // still: the value it has not been evaluated against is the one from the
        // movement that joined the transition.
        match entry.condition.check_any(value) {
            Ok(true) => {}
            Ok(false) => {
                steps.push(Step {
                    chain: index,
                    entry: entry_index,
                    decision: Decision::Skip,
                });
                continue;
            }
            Err(error) => {
                // A condition that cannot be evaluated has decided nothing about
                // the effect: the entry stays where it is, this chain stops, and
                // the tick reports the error. Nothing is abandoned, because
                // nothing was attempted. Recorded at most once per chain, since
                // the walk stops here.
                *failure = Some(error);
                break;
            }
        }

        let attempts = match *state {
            EntryState::DefinitelyFailed { attempts, .. } => attempts.saturating_add(1),
            _ => 1,
        };
        steps.push(Step {
            chain: index,
            entry: entry_index,
            decision: Decision::Attempt { attempts },
        });
    }
}

/// Build the schedule this substrate runs, with the two settings that make its
/// order a property of the graph rather than of thread arrival.
fn schedule() -> Schedule {
    let mut schedule = Schedule::default();
    // Explicit, not inherited: the multithreaded executor admits any system
    // whose access set is disjoint from the running set, so which of two
    // non-conflicting systems runs first is not fixed.
    schedule.set_executor(SingleThreadedExecutor::default());
    schedule.set_build_settings(ScheduleBuildSettings {
        ambiguity_detection: LogLevel::Error,
        ..Default::default()
    });
    // `.chain()` is the declared order: commit the observations, then decide
    // the effects.
    schedule.add_systems((observe_fold, fire_plan).chain());
    schedule
}

/// Initialize and validate a schedule, turning a build failure into a
/// [`BotError`]. Factored out so the refusal is testable without constructing a
/// bot whose schedule happens to be ambiguous.
fn validate(schedule: &mut Schedule, world: &mut World) -> Result<(), BotError> {
    schedule
        .initialize(world)
        .map(|_| ())
        .map_err(|error| BotError::DomainError {
            domain: "ecs::schedule".into(),
            certainty: DispatchCertainty::Refused,
            // `ScheduleBuildError` carries an inherent `to_string(&self, graph,
            // world)` that shadows `Display::to_string`; the inherent one is the
            // one that resolves system names against the graph.
            cause: error.to_string(schedule.graph(), world),
        })
}

// ── The bot ────────────────────────────────────────────────────────────────

/// A bot executing on a `bevy_ecs` world.
///
/// Stepped by hand: one tick is one [`EcsBot::tick_async`], which is one
/// `Schedule::run` between the awaited observe and act phases. No framework owns
/// a loop, and no `App::run` is ever called.
pub struct EcsBot {
    /// The name the spec declared.
    name: String,
    /// The world the schedule runs against.
    world: World,
    /// The validated schedule.
    schedule: Schedule,
}

impl EcsBot {
    /// Start building a named bot on this substrate.
    #[must_use]
    pub fn builder(name: impl Into<String>) -> EcsBuilder {
        EcsBuilder {
            name: name.into(),
            chains: Vec::new(),
            policy: RetryPolicy::DEFAULT,
        }
    }

    /// The name the spec declared.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Effects fired on the most recent tick.
    #[must_use]
    pub fn fired(&self) -> usize {
        self.world.resource::<Fired>().0
    }

    /// The world, for tests and instrumentation. Read-only: mutating it behind
    /// the schedule's back would break the ordering the build validated.
    #[must_use]
    pub fn world(&self) -> &World {
        &self.world
    }

    /// The `Revision` of every source, in chain order.
    #[must_use]
    pub fn revisions(&self) -> Vec<u64> {
        let order = self.world.resource::<Order>().0.clone();
        order
            .iter()
            .filter_map(|entity| self.world.get::<Revision>(*entity))
            .map(|revision| revision.0)
            .collect()
    }

    /// The observed sources' domain identifiers, in chain order.
    ///
    /// Replaces the old `chains()` accessor: what a caller wanted from it was
    /// "what is this bot watching", and that is identity, not the erased
    /// observer objects.
    #[must_use]
    pub fn source_domains(&self) -> Vec<String> {
        let order = self.world.resource::<Order>().0.clone();
        order
            .iter()
            .filter_map(|entity| self.world.get::<SourceId>(*entity))
            .map(|id| id.domain().to_owned())
            .collect()
    }

    /// Run one tick, awaiting the verb futures on the caller's executor.
    ///
    /// Returns the number of actions that fired. The bot is `mut` because a
    /// tick advances its world.
    ///
    /// # What this is for
    ///
    /// This is the tick to call from async code, and the reason is that the
    /// verb futures are awaited **by the caller's runtime** rather than by a
    /// thread parked inside a synchronous system. A source that waits on
    /// `rt::time`, on a socket, on a channel fed by a sibling task, or on
    /// anything else that needs a driver resolves while this future is pending,
    /// because the runtime that owns it is the one driving.
    ///
    /// # Phases
    ///
    /// 1. every source is polled in bounded waves of `MAX_IN_FLIGHT_POLLS`,
    ///    concurrently, in declaration order;
    /// 2. one `Schedule::run` commits those observations, detects which values
    ///    moved, and records the effect program (`observe_fold`, `fire_plan`),
    ///    walking the ledger's outstanding work rather than the change set;
    /// 3. the recorded effects run one at a time, in declaration order, and each
    ///    entry is written into the ledger as it goes;
    /// 4. the count, the first failure, and any entry still held are reported.
    ///
    /// The middle two phases are what keep this deterministic: the schedule is
    /// a total order validated at build, the observations are folded
    /// positionally, and the effect program is fixed before any effect runs.
    ///
    /// # Failure
    ///
    /// A poll that failed fires nothing (no `Revision` was written), while an
    /// action that failed leaves the effects before it live and returns the first
    /// error. A condition that fails stops its own chain where it failed, so the
    /// effects that chain's walk had already reached still run — the plan carries
    /// the decisions and the chains after it are still walked, and the tick
    /// reports the first failure in declaration order. `Err` therefore means "this
    /// run did not finish", never "nothing happened".
    ///
    /// The failure keeps precedence over the pending report when a tick has both,
    /// because it carries the typed variant a retry classifier reads: an
    /// [`BotError::EffectIndeterminate`] written into a string would stop being
    /// the distinction the whole error type exists to draw. So the tick that gives
    /// up on an entry reports the error that caused it, and the abandonment —
    /// never a silent drop — is reported by [`Self::pending`], which names every
    /// abandoned entry, its reason, and its source revision for as long as it
    /// stands.
    ///
    /// # Cancellation
    ///
    /// Dropping this future before it resolves cancels the tick at the point it
    /// was dropped. Between phases 2 and 3 that loses the effects phase 2
    /// selected, and the next tick does not re-fire them: the revisions were
    /// committed in phase 2, so the sources no longer read as moved. This is the
    /// same "unattempted work is lost, not queued" rule the failure path
    /// documents, and it is stated rather than discovered because it is the price
    /// of a tick that is a future.
    ///
    /// Dropping it *during* phase 3 is the case the ledger exists for. An entry
    /// whose attempt was in flight was written to the ledger before the effect
    /// was awaited, so the next tick finds it held as
    /// [`TransitionHold::Unrecorded`] — "an attempt began and no outcome was
    /// recorded" — and does not replay it. An entry whose turn had not come is
    /// left `NotStarted`, so it is walked again. Which of the two an entry is
    /// depends only on whether the effect was awaited, which is the same fact the
    /// caller is missing; the difference is that the substrate no longer has to
    /// guess.
    ///
    /// # Errors
    ///
    /// The first domain failure of the tick, in the order described above, or
    /// [`BotError::PendingTransition`] when nothing failed and work is still held.
    pub async fn tick_async(&mut self) -> Result<usize, BotError> {
        self.begin_tick();

        let polled = self.poll_sources().await;
        self.world.non_send_mut::<Polled>().0 = polled;

        self.schedule.run(&mut self.world);

        // Taken rather than borrowed: the effects are awaited below, and a
        // borrow of the plan would outlive the schedule that wrote it. An empty
        // plan is left behind, so a tick dropped here stages nothing.
        let plan = {
            let mut guard = self.world.non_send_mut::<Plan>();
            std::mem::take(&mut *guard)
        };
        let Plan {
            steps,
            mut failures,
        } = plan;
        let budget = self.world.resource::<Policy>().0.max_attempts();
        let (fired, failure) = self.run_steps(&steps, &mut failures, budget).await;

        self.world.resource_mut::<Fired>().0 = fired;
        if let Some(error) = failure {
            self.world.resource_mut::<TickError>().0 = Some(error);
        }

        if let Some(error) = self.world.resource_mut::<TickError>().0.take() {
            return Err(error);
        }

        // A tick that reported no failure is not necessarily a tick with nothing
        // left. An entry the substrate cannot settle on its own — an attempt in
        // flight whose outcome was never recorded, an effect that may be live, or
        // one the budget gave up on — is held, and a held entry is reported
        // rather than passed over in silence: a caller that read a clean `Ok` as
        // "the transition was handled" would be reading something that is not
        // true. That includes an abandonment, which is the case this used to
        // miss: it asks the tick for nothing, so it looks finished from inside
        // the walk, but it is work nobody resolved.
        let ledger = self.world.non_send::<Ledger>();
        match ledger.first_unresolved(budget) {
            Some(work) => Err(BotError::PendingTransition {
                work,
                outstanding: ledger.unresolved_count(),
            }),
            None => Ok(self.world.resource::<Fired>().0),
        }
    }

    /// Run one tick on this thread, without an async runtime.
    ///
    /// # Scope
    ///
    /// This is the adapter for **runtime-independent futures**: polls and
    /// actions that complete without a timer or an I/O driver. Immediate
    /// futures, `lgwks_std::task::spawn_blocking` work, and a channel fed by
    /// another thread all belong here; it drives the same four phases as
    /// [`EcsBot::tick_async`] with `lgwks_std::task::block_on`, which parks
    /// this thread until they resolve.
    ///
    /// A verb that needs this crate's timer (`rt::time`) or a driver cannot run
    /// here, because there is no reactor for it to register with: on this
    /// adapter `rt::time::sleep` reaches the runtime-context panic its own
    /// documentation names. Await [`EcsBot::tick_async`] from inside a runtime
    /// instead.
    ///
    /// # The refusal
    ///
    /// [`BotError::TickInsideRuntime`] when an async runtime is already driving
    /// the calling thread. Parking a thread that owns a runtime's driver is not
    /// a slow tick, it is a deadlock — the timer never fires, the socket never
    /// reports ready, and the tick never returns — so the one thing this
    /// adapter must not do is what its name suggests. The check is made here
    /// rather than left to the caller because the calling mode is not knowable
    /// from the verb API.
    ///
    /// This refuses even on a runtime with several worker threads, where
    /// parking one worker may happen to work. Which thread the caller was
    /// handed is not knowable from here, and a deadlock that appears only on
    /// the current-thread runtime is the failure this exists to prevent rather
    /// than to mask.
    ///
    /// # Errors
    ///
    /// [`BotError::TickInsideRuntime`] as above, or the first domain failure of
    /// the tick.
    pub fn tick(&mut self) -> Result<usize, BotError> {
        #[cfg(feature = "rt")]
        if lgwks_deps::tokio::runtime::Handle::try_current().is_ok() {
            return Err(BotError::TickInsideRuntime);
        }
        lgwks_std::task::block_on(self.tick_async())
    }

    /// Reset the per-tick scratch, so a tick starts from the same state however
    /// the previous one ended.
    ///
    /// The staged observations and the effect program are not cleared here:
    /// both are overwritten wholesale by the phase that produces them, which is
    /// what makes a *dropped* tick safe as well as a completed one.
    fn begin_tick(&mut self) {
        self.world.resource_mut::<Fired>().0 = 0;
        self.world.resource_mut::<TickError>().0 = None;
    }

    /// Poll every source, `MAX_IN_FLIGHT_POLLS` at a time, awaiting each wave
    /// on the caller's executor.
    ///
    /// Bounded waves, joined concurrently: a source poll may occupy one
    /// `spawn_blocking` thread, so polling them one at a time would make a tick
    /// as slow as the sum of its sources rather than as slow as its slowest.
    /// The wave cap is what keeps that from becoming unbounded blocking-thread
    /// fan-out. Determinism is unaffected: the results are collected in
    /// declaration order whatever order they resolve in.
    async fn poll_sources(&self) -> Vec<Result<Erased, BotError>> {
        let chains = self.world.non_send::<Chains>();
        let grants = self.world.resource::<Grants>();
        let mut polled = Vec::with_capacity(chains.0.len());
        for wave in chains.0.chunks(MAX_IN_FLIGHT_POLLS) {
            let batch = lgwks_std::task::join_all_boxed(
                wave.iter().map(|chain| chain.source.poll_any(&grants.0)),
            );
            polled.extend(batch.await);
        }
        polled
    }

    /// Run the effects the decision phase selected, in the order it selected
    /// them, awaiting each on the caller's executor.
    ///
    /// Returns the number that fired and the first failure in declaration order,
    /// or `None` when nothing failed.
    ///
    /// Every chain is run on its own, because a failure stops its own chain rather
    /// than every chain after it: the chains behind a failing one still run and
    /// still record their work. Iterating chains rather than steps is also what
    /// keeps a chain that recorded *no* step from being skipped: a condition the
    /// walk could not evaluate stops that chain before it records anything, and
    /// its failure is still the tick's to report. The failure of the first chain
    /// in declaration order that has one is that report, which is the ordering the
    /// synchronous walk had.
    ///
    /// States are written to the ledger as the walk reaches them — an attempt in
    /// flight *before* its effect is awaited, and its outcome after — so a tick
    /// dropped in the middle holds what it had begun instead of leaving a
    /// `NotStarted` entry for the next tick to replay.
    async fn run_steps(
        &mut self,
        steps: &[Step],
        failures: &mut [Option<BotError>],
        budget: u32,
    ) -> (usize, Option<BotError>) {
        let mut fired: usize = 0;
        let mut failure: Option<BotError> = None;
        let mut start = 0;
        for chain in 0..failures.len() {
            // `steps` is in `(chain, entry)` walk order, so the steps of `chain`
            // are the run at `start` — and a chain that recorded none contributes
            // the empty run, because its first step is a later chain's.
            let end = steps[start..]
                .iter()
                .position(|step| step.chain != chain)
                .map_or(steps.len(), |offset| start.saturating_add(offset));
            let run = match steps.get(start) {
                Some(step) if step.chain == chain => &steps[start..end],
                _ => &[],
            };

            // Taken, so the condition failure belongs to the chain it was recorded
            // for and cannot be reported for a chain that never reached it.
            let condition_failure = failures.get_mut(chain).and_then(Option::take);
            let (chain_fired, chain_failure) = self.run_chain(run, condition_failure, budget).await;
            fired = fired.saturating_add(chain_fired);
            // Chain order, so "the first error in declaration order" is the one a
            // caller gets, exactly as the synchronous walk had it — except that
            // the chains after it still ran.
            if failure.is_none() {
                failure = chain_failure;
            }
            start = end;
        }
        (fired, failure)
    }

    /// Apply one chain's walk, in entry order, awaiting each attempt.
    ///
    /// A [`Decision::Skip`] is applied without an attempt. A
    /// [`Decision::Attempt`] is written to the ledger as in flight, awaited, and
    /// then recorded. The run stops at the first failure, leaving the entries
    /// behind it exactly as they were, so an acknowledged effect is never
    /// replayed to reach a successor.
    ///
    /// The report falls back to the condition failure the decision phase recorded
    /// for this chain when no attempt of its own failed: a condition the walk
    /// could not evaluate is a failure of the chain it is in, and the decisions
    /// before it are exactly the effects the walk had already cleared.
    async fn run_chain(
        &mut self,
        steps: &[Step],
        condition_failure: Option<BotError>,
        budget: u32,
    ) -> (usize, Option<BotError>) {
        let mut fired: usize = 0;
        let mut failure: Option<BotError> = None;
        for step in steps {
            let work = WorkId {
                chain: step.chain,
                entry: step.entry,
            };
            let attempts = match step.decision {
                Decision::Skip => {
                    self.world.non_send_mut::<Ledger>().skip(work);
                    continue;
                }
                Decision::Attempt { attempts } => attempts,
            };

            // Written *before* the attempt, not after: a tick dropped while the
            // effect is in flight leaves this behind, and a record that says
            // "begun, outcome unknown" is what makes the next tick hold the
            // effect instead of replaying it. A panic unwinding out of the action
            // is a limit this substrate does not close, and it is stated in the
            // module documentation.
            if !self.world.non_send_mut::<Ledger>().begin(work) {
                // The entry is not in a state an attempt can start from, so the
                // world moved behind the schedule's back. The run stops here
                // rather than guessing what the entry now means.
                break;
            }

            // Scoped, so the world is borrowed immutably only across the await
            // and the ledger is reachable mutably on either side of it.
            //
            // The value is the transition's binding, not the newest observation.
            // That is the whole of the invariant: this entry was *selected*
            // against the payload its transition was opened under, so it has to
            // be *run* against that same payload. Reading the observation slot
            // here is how a chain comes to refuse against one input and then
            // retry against another, combining two command inputs into one
            // transition's effects. The slot is empty for as long as the binding
            // is out on loan, so there is nothing to reach for by accident.
            let outcome = {
                let chains = self.world.non_send::<Chains>();
                let grants = self.world.resource::<Grants>();
                let bound = self.world.non_send::<Ledger>().bound(step.chain);
                match (chains.0.get(step.chain), bound) {
                    (Some(chain), Some(value)) => match chain.entries.get(step.entry) {
                        Some(entry) => Some(entry.action.run_any(&grants.0, value).await),
                        None => None,
                    },
                    // No chain, or a transition bound to nothing: the entry
                    // stays held by the record written above rather than being
                    // guessed at.
                    _ => None,
                }
            };

            let Some(outcome) = outcome else {
                break;
            };
            match outcome {
                Ok(_) => {
                    if self.world.non_send_mut::<Ledger>().succeed(work) {
                        fired = fired.saturating_add(1);
                    }
                }
                Err(error) => {
                    self.world
                        .non_send_mut::<Ledger>()
                        .fail(work, &error, attempts, budget);
                    failure = Some(error);
                    break;
                }
            }
        }
        // An action failure happens *earlier* in walk order than a condition
        // failure that follows it, so it is the more specific report.
        (fired, failure.or(condition_failure))
    }

    /// Every entry that is not finished, in `(chain, entry)` order.
    ///
    /// Two kinds are reported and [`TransitionHold`] distinguishes them: an
    /// entry still being held — waiting for an attempt or for evidence — and
    /// one given up on, which stays listed so lost work has a name after the
    /// tick that lost it. An empty result means the last transition is fully
    /// handled.
    #[must_use]
    pub fn pending(&self) -> Vec<PendingWork> {
        let budget = self.world.resource::<Policy>().0.max_attempts();
        self.world.non_send::<Ledger>().pending(budget)
    }

    /// Settle an entry whose effect may or may not have happened.
    ///
    /// A held entry is one the substrate cannot decide about on its own, and
    /// retrying it blind is how a bot duplicates a merge, a message, or a
    /// launch. Supply what you know — [`EffectEvidence::Applied`] acknowledges
    /// the effect, [`EffectEvidence::NotApplied`] makes the entry eligible for
    /// an attempt again — and the entry moves.
    ///
    /// An entry that was given up on accepts evidence too, which is how a
    /// caller revives work the attempt budget abandoned.
    ///
    /// `revision` names the generation the evidence is about: the
    /// [`PendingWork::revision`] of the [`pending`](Self::pending) entry the
    /// caller read. It is required, and compared before anything is read or
    /// written, because a chain reuses one slot for every generation over it. A
    /// delayed report about revision N delivered after the slot re-opened at
    /// revision N+1 would otherwise land on an attempt the caller has never
    /// seen — `Applied` acknowledging an effect at a generation that never
    /// happened, `NotApplied` making an unknown attempt eligible to run again.
    /// Passing the revision that came with the work is what makes the delivery
    /// safe to retry.
    ///
    /// # Errors
    ///
    /// [`BotError::NoSuchWork`] when the entry is not held: it ran, its
    /// condition was false, it has not been reached yet, or its chain holds no
    /// transition at all. Evidence cannot contradict an attempt whose outcome
    /// was recorded, and accepting it silently would let a caller believe an
    /// effect was acknowledged when nothing was.
    ///
    /// [`BotError::EvidenceSuperseded`] when the chain holds a transition whose
    /// revision is not `revision` — the work the caller is reporting on was
    /// superseded, and nothing was changed. Re-read
    /// [`pending`](Self::pending) and report against the generation that is
    /// there now.
    ///
    /// [`BotError::EvidenceContradicted`] when this generation's entry was
    /// already settled with the opposite evidence. Nothing was changed:
    /// [`Applied`](EffectEvidence::Applied) does not become
    /// [`NotApplied`](EffectEvidence::NotApplied), or the reverse, after the
    /// fact. Repeating the *same* evidence is not a contradiction and succeeds
    /// idempotently, so a caller that never saw its first delivery through can
    /// safely send it again.
    pub fn resolve_effect(
        &mut self,
        work: WorkId,
        revision: u64,
        evidence: EffectEvidence,
    ) -> Result<(), BotError> {
        match self
            .world
            .non_send_mut::<Ledger>()
            .settle(work, revision, evidence)
        {
            // Both are success, and deliberately one arm: to the caller, a
            // repeat that landed a second time is the same fact as one that
            // landed the first.
            Settled::Decided | Settled::Duplicate => Ok(()),
            Settled::Superseded { current } => Err(BotError::EvidenceSuperseded {
                work,
                named: revision,
                current,
            }),
            Settled::Contradicted { settled } => Err(BotError::EvidenceContradicted {
                work,
                settled,
                submitted: evidence,
            }),
            Settled::NoSuchWork => Err(BotError::NoSuchWork { work }),
        }
    }
}

/// Builder for [`EcsBot`].
pub struct EcsBuilder {
    /// The bot name.
    name: String,
    /// Chains finished so far.
    chains: Vec<EcsChain>,
    /// The retry policy every entry will run under.
    policy: RetryPolicy,
}

impl EcsBuilder {
    /// Set the retry budget for entries whose effect definitely did not happen.
    ///
    /// Additive and defaulted: a caller that never calls this gets
    /// [`RetryPolicy::DEFAULT`], which is what makes the budget a policy rather
    /// than a constant. Without it, "the work was given up on" would be a fact
    /// the caller could not influence, and a bot whose actions are idempotent
    /// would abandon work it could perfectly well retry.
    #[must_use]
    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Bind a source to observe.
    ///
    /// The source is *not* erased here. It stays a concrete `S` inside the
    /// returned builder for as long as the chain is being declared, so `on` can
    /// type its condition and its action against `S::Output`; the erasure
    /// happens in [`EcsObserveBuilder::observe`] and
    /// [`EcsObserveBuilder::build`], which are the two points where the chain
    /// stops being a declaration and becomes a chain.
    pub fn observe<S>(self, source: S) -> EcsObserveBuilder<S>
    where
        S: Observe,
    {
        EcsObserveBuilder {
            name: self.name,
            prior: self.chains,
            source,
            entries: Vec::new(),
            policy: self.policy,
        }
    }

    /// Build with no observation chains: a bot that only serves direct
    /// `Query` and `Execute` calls.
    pub fn build(self, grants: &GrantSet) -> Result<EcsBot, BotError> {
        EcsBot::assemble(self.name, self.chains, grants, self.policy)
    }
}

impl<S: Observe> EcsObserveBuilder<S> {
    /// Add a `(condition, action)` tuple to this chain.
    ///
    /// Both halves are typed against the source this chain is observing: the
    /// condition reads `S::Output`, and the action takes exactly it. There is no
    /// third type parameter, and that is the point — the shape this replaced had
    /// one (`on<C, A, T>`) which was tied to nothing at all, so a chain whose
    /// condition read one type and whose action expected another compiled, built,
    /// and failed at tick time with a downcast miss. A wiring defect that
    /// compiles is a defect that ships.
    ///
    /// The tuple's erasure is shared with the `Bot` executor rather than
    /// re-implemented, so the `Auth` issue, the downcast and the type-mismatch
    /// error cannot drift between the two substrates.
    ///
    /// # The rule this relies on
    ///
    /// Equal types across the chain is sound only because every verb so far
    /// consumes its input and produces a caller-visible one: [`Evaluate::check`]
    /// returns a `bool`, a pure predicate with no derived output, so
    /// `Source::Output == Condition::Input == Action::Input` is already the real
    /// semantics rather than a simplification of it.
    ///
    /// **No stage may introduce a caller-selected type parameter disconnected
    /// from its input.** A stage that transforms the value must carry the
    /// transformation as an associated type — `Transform<I>::Output` — so the
    /// next stage's input is a consequence of the previous one's output. A free
    /// parameter here is how this defect happened once, and it will happen again
    /// the first time a verb takes a type the chain does not determine.
    pub fn on<C, A>(mut self, condition: C, action: A) -> Self
    where
        S::Output: 'static,
        C: Evaluate<S::Output> + 'static,
        A: Execute<Input = S::Output> + 'static,
        A::Output: 'static,
    {
        self.entries
            .push(typed_entry::<C, A, S::Output>(condition, action));
        self
    }

    /// Finish this chain and start another.
    ///
    /// The erasure boundary for the chain being closed: `S` stops being a type
    /// parameter here, and the witness is taken in the same breath, while
    /// `S::Output` can still be named.
    ///
    /// The `PartialEq` bound is the substrate's one extra requirement, and it is
    /// semantic rather than incidental: the condition on this substrate *is*
    /// change detection, and a value that cannot be compared cannot be detected
    /// as changed. [`Bot::builder`](crate::Bot::builder) carries no such bound
    /// because its condition is re-evaluated every tick and needs no equality.
    pub fn observe<U>(self, source: U) -> EcsObserveBuilder<U>
    where
        S: 'static,
        S::Output: PartialEq + 'static,
        U: Observe,
    {
        // Destructured rather than moved field by field: taking `prior` by
        // `mem::take` and then moving `source` out leaves `self` partially
        // moved, and the borrow checker will not let a method call finish the
        // chain in between.
        let Self {
            name,
            mut prior,
            source: previous,
            entries,
            policy,
        } = self;
        prior.push(EcsChain {
            source: Box::new(previous),
            same: same_output::<S>,
            witness: Witness::of::<S::Output>(),
            entries,
        });
        EcsObserveBuilder {
            name,
            prior,
            source,
            entries: Vec::new(),
            policy,
        }
    }

    /// Assemble the bot, admitting every capability and validating the schedule.
    ///
    /// The erasure boundary for the last chain, for the same reason as
    /// [`observe`](Self::observe), and the one call that turns a declaration into
    /// a running bot.
    pub fn build(self, grants: &GrantSet) -> Result<EcsBot, BotError>
    where
        S: 'static,
        S::Output: PartialEq + 'static,
    {
        let Self {
            name,
            mut prior,
            source,
            entries,
            policy,
        } = self;
        prior.push(EcsChain {
            source: Box::new(source),
            same: same_output::<S>,
            witness: Witness::of::<S::Output>(),
            entries,
        });
        EcsBot::assemble(name, prior, grants, policy)
    }
}

/// A chain being assembled, holding its source as the concrete type it is.
///
/// Generic over the source so that [`on`](Self::on) can type a condition and an
/// action against `S::Output`. The type parameter is the chain's one claim about
/// its own wiring, and it is checked by the compiler rather than by a downcast:
/// a `(condition, action)` tuple attached here has both halves typed against the
/// source, so a chain that cannot work does not build.
///
/// `S` is erased at [`observe`](Self::observe) and [`build`](Self::build), which
/// are the only two points where the chain is finished. There is no third state
/// between "declaring a chain" and "a chain": the builder cannot be stored,
/// serialized, or passed to anything that expects an `EcsChain`, because it is
/// not one until the erasure runs.
pub struct EcsObserveBuilder<S> {
    /// Carried from [`EcsBuilder`]; the chain being built does not consume it.
    name: String,
    /// Chains finished by an earlier `observe` call, in order.
    prior: Vec<EcsChain>,
    /// The source being bound, still concrete.
    source: S,
    /// Tuples attached so far, each already erased for storage.
    entries: Vec<ChainEntry>,
    /// Carried from [`EcsBuilder`] alongside `name`.
    policy: RetryPolicy,
}

impl EcsBot {
    /// Validate and assemble. Shared by both terminal builder calls so
    /// admission cannot drift between them, the same rule
    /// [`spec::assemble`](crate::spec) follows for `Bot`.
    fn assemble(
        name: String,
        chains: Vec<EcsChain>,
        grants: &GrantSet,
        policy: RetryPolicy,
    ) -> Result<Self, BotError> {
        if name.is_empty() {
            return Err(BotError::IncompleteSpec { field: "name" });
        }
        // The same admission gate `Bot::build` applies: a bot requiring a
        // capability it was not granted fails before it runs, not at tick.
        //
        // Every unmet requirement in the bot, from one pass, each attributed to
        // the domain that declared it. Admission that returned at the first one
        // made "what does this bot need" a loop: each answer revealed the next
        // question, so a bot short of four capabilities took four refusals to
        // diagnose and the fourth was the first time the caller had the whole
        // picture. The gate already computes the whole difference — it is the
        // required set minus the granted set — so it reports the whole of it,
        // and the caller has one list to act on rather than a sequence to
        // discover.
        let mut shortages: Vec<Shortage> = Vec::new();
        for chain in &chains {
            shortages.extend(grants.uncovered(
                chain.source.required_caps(),
                &Demand::new(chain.source.domain_id()),
            ));
            for entry in &chain.entries {
                shortages.extend(grants.uncovered(
                    entry.action.required_caps(),
                    &Demand::new(entry.action.domain_id()),
                ));
            }
        }
        if let Some(deficit) = Deficit::from_shortages(shortages) {
            return Err(BotError::CapabilityDenied { deficit });
        }

        let mut world = World::new();
        world.insert_resource(Grants(grants.clone()));
        world.insert_resource(Fired::default());
        world.insert_resource(TickError::default());
        world.insert_resource(Policy(policy));

        let mut order = Vec::with_capacity(chains.len());
        for (index, chain) in chains.iter().enumerate() {
            order.push(
                world
                    .spawn((
                        SourceId {
                            chain: index,
                            domain: chain.source.domain_id().to_owned(),
                        },
                        Revision::default(),
                    ))
                    .id(),
            );
        }
        world.insert_resource(Order(order));

        let count = chains.len();
        world.insert_non_send(Chains(chains));
        // Not `vec![None; count]`: `Box<dyn Any>` is not `Clone`, so the
        // repeat-form macro cannot build this.
        world.insert_non_send(Observed((0..count).map(|_| None).collect()));
        // The eligible work of the bot, one idle slot per chain. Non-send,
        // because each transition owns the observed payload it is bound to.
        world.insert_non_send(Ledger::with_chains(count));
        // The two staging resources the awaited phases hand to the schedule.
        // Inserted here rather than at first use so every phase can name them
        // unconditionally: a phase that found one missing would have to decide
        // what that means, and there is no answer that is better than "this
        // cannot happen".
        world.insert_non_send(Polled::default());
        world.insert_non_send(Plan::default());

        let mut schedule = schedule();
        validate(&mut schedule, &mut world)?;

        Ok(Self {
            name,
            world,
            schedule,
        })
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────
//
// These run under the ordinary workspace test run: there is no feature to turn
// on, because a substrate whose tests only run when someone remembers a flag is
// a substrate whose guarantees are claims.

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use super::*;
    use crate::cap::{Auth, Cap, Demand};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// A scripted source: yields the next value from `values` on each poll.
    ///
    /// Deliberately `Rc`-based and therefore `!Send`, which is the property the
    /// verbs actually have and the reason this whole module routes through
    /// `NonSend` resources.
    struct Script {
        values: Rc<RefCell<Vec<u16>>>,
        cursor: Rc<Cell<usize>>,
        caps: Vec<Cap>,
    }

    impl Script {
        fn new(values: Vec<u16>) -> Self {
            Self {
                values: Rc::new(RefCell::new(values)),
                cursor: Rc::new(Cell::new(0)),
                caps: vec![Cap::net()],
            }
        }
    }

    impl Observe for Script {
        type Output = u16;

        fn required_caps(&self) -> &[Cap] {
            &self.caps
        }

        async fn poll(&self, call: (Auth, ())) -> Result<u16, BotError> {
            call.0.check(&self.caps)?;
            let index = self.cursor.get();
            self.cursor.set(index.saturating_add(1));
            Ok(self.values.borrow().get(index).copied().unwrap_or(0))
        }

        fn domain_id(&self) -> &str {
            "test::script"
        }
    }

    /// A source that fails once its script runs out.
    struct Exhausting {
        remaining: Rc<Cell<usize>>,
        caps: Vec<Cap>,
    }

    impl Observe for Exhausting {
        type Output = u16;

        fn required_caps(&self) -> &[Cap] {
            &self.caps
        }

        async fn poll(&self, call: (Auth, ())) -> Result<u16, BotError> {
            call.0.check(&self.caps)?;
            let left = self.remaining.get();
            self.remaining.set(left.saturating_sub(1));
            if left == 0 {
                return Err(BotError::DomainError {
                    domain: "test::exhausting".into(),
                    certainty: DispatchCertainty::NotDelivered,
                    cause: "script exhausted".into(),
                });
            }
            Ok(200)
        }

        fn domain_id(&self) -> &str {
            "test::exhausting"
        }
    }

    /// An action that counts how many times it ran.
    struct Count(Rc<Cell<usize>>);

    impl Execute for Count {
        type Input = u16;
        type Output = ();

        fn required_caps(&self) -> &[Cap] {
            &[]
        }

        async fn execute_action(&self, call: (Auth, &u16)) -> Result<(), BotError> {
            call.0.check(&[])?;
            self.0.set(self.0.get().saturating_add(1));
            Ok(())
        }

        fn domain_id(&self) -> &str {
            "test::count"
        }
    }

    /// An action that records what it saw, so a test can assert on a
    /// *persistent* effect rather than on a call count.
    struct Record(Rc<RefCell<Vec<u16>>>);

    impl Execute for Record {
        type Input = u16;
        type Output = ();

        fn required_caps(&self) -> &[Cap] {
            &[]
        }

        async fn execute_action(&self, call: (Auth, &u16)) -> Result<(), BotError> {
            call.0.check(&[])?;
            self.0.borrow_mut().push(*call.1);
            Ok(())
        }

        fn domain_id(&self) -> &str {
            "test::record"
        }
    }

    /// An action that fails a fixed number of times and then records: the
    /// retry half of a chain, with a persistent effect to assert on.
    struct Flaky {
        remaining: Rc<Cell<usize>>,
        log: Rc<RefCell<Vec<u16>>>,
        kind: FlakyKind,
    }

    /// Which failure the action reports, because the two are not the same
    /// question and the substrate must treat them differently.
    #[derive(Clone, Copy)]
    enum FlakyKind {
        /// The effect definitely did not happen.
        Refused,
        /// The effect may or may not have happened.
        Indeterminate,
    }

    impl Execute for Flaky {
        type Input = u16;
        type Output = ();

        fn required_caps(&self) -> &[Cap] {
            &[]
        }

        async fn execute_action(&self, call: (Auth, &u16)) -> Result<(), BotError> {
            call.0.check(&[])?;
            let left = self.remaining.get();
            if left == 0 {
                self.log.borrow_mut().push(*call.1);
                return Ok(());
            }
            self.remaining.set(left.saturating_sub(1));
            Err(match self.kind {
                FlakyKind::Refused => BotError::DomainError {
                    domain: "test::flaky".into(),
                    certainty: DispatchCertainty::NotDelivered,
                    cause: "refused, and the effect did not happen".into(),
                },
                FlakyKind::Indeterminate => BotError::EffectIndeterminate {
                    domain: "test::flaky".into(),
                    cause: "the acknowledgment never arrived".into(),
                },
            })
        }

        fn domain_id(&self) -> &str {
            "test::flaky"
        }
    }

    /// The `(chain, entry)` identity of a pending report.
    fn identity(work: &PendingWork) -> (usize, usize) {
        (work.id().chain(), work.id().entry())
    }

    /// The identities of every pending report, in order.
    fn identities(bot: &EcsBot) -> Vec<(usize, usize)> {
        bot.pending().iter().map(identity).collect()
    }

    /// What is holding the first pending report, if anything is pending.
    fn first_hold(bot: &EcsBot) -> Option<TransitionHold> {
        bot.pending().first().map(|work| work.hold().clone())
    }

    /// An action that always refuses, counting the attempts it received.
    struct Refuses(Rc<Cell<usize>>);

    impl Execute for Refuses {
        type Input = u16;
        type Output = ();

        fn required_caps(&self) -> &[Cap] {
            &[]
        }

        async fn execute_action(&self, call: (Auth, &u16)) -> Result<(), BotError> {
            call.0.check(&[])?;
            self.0.set(self.0.get().saturating_add(1));
            Err(BotError::DomainError {
                domain: "test::refuses".into(),
                certainty: DispatchCertainty::NotDelivered,
                cause: "refused".into(),
            })
        }

        fn domain_id(&self) -> &str {
            "test::refuses"
        }
    }

    /// A condition that fails structurally rather than evaluating to `false`.
    ///
    /// Named apart from the refusing *action* above rather than sharing a name
    /// with it: the two refuse in different halves of a chain, one is a condition
    /// and one is an action, and a single name for both would make a call site
    /// that reads `.on(Refuses, ..)` ambiguous about which half it is asserting on.
    struct RefusesToEvaluate;

    impl Evaluate<u16> for RefusesToEvaluate {
        fn check(&self, observed: &u16) -> Result<bool, BotError> {
            Err(BotError::EvaluateError {
                cause: format!("this condition cannot decide about {observed}"),
            })
        }

        fn condition_id(&self) -> &str {
            "test::refuses"
        }
    }

    /// A source requiring exactly the capabilities it is handed, so a test can
    /// put more than one unmet requirement in front of admission. `Script`
    /// fixes its own list to `bot.net`, which is one requirement and therefore
    /// cannot distinguish a whole shortfall from its first element.
    struct Needs {
        caps: Vec<Cap>,
        value: u16,
    }

    impl Observe for Needs {
        type Output = u16;

        fn required_caps(&self) -> &[Cap] {
            &self.caps
        }

        async fn poll(&self, call: (Auth, ())) -> Result<u16, BotError> {
            call.0.check(&self.caps)?;
            Ok(self.value)
        }

        fn domain_id(&self) -> &str {
            "test::needs"
        }
    }

    /// The action half of the same arrangement: `Count` requires nothing, so a
    /// test built on it can only ever put a source's requirements in front of
    /// the gate.
    struct NeedsCaps {
        caps: Vec<Cap>,
        domain: &'static str,
    }

    impl Execute for NeedsCaps {
        type Input = u16;
        type Output = ();

        fn required_caps(&self) -> &[Cap] {
            &self.caps
        }

        async fn execute_action(&self, call: (Auth, &u16)) -> Result<(), BotError> {
            call.0.check(&self.caps)?;
            Ok(())
        }

        fn domain_id(&self) -> &str {
            self.domain
        }
    }

    fn net_grants() -> GrantSet {
        GrantSet::empty().grant(Cap::net())
    }

    /// The sequence every test uses: two ticks of 200, two of 503, one of 200.
    /// It holds still for a tick at a time, which is what makes a change filter
    /// falsifiable: a sequence that moved on every tick would make "changed"
    /// and "ran" indistinguishable.
    const SCRIPT: [u16; 5] = [200, 200, 503, 503, 200];

    #[test]
    fn a_condition_fires_only_on_the_tick_its_source_moves() -> TestResult {
        let counter = Rc::new(Cell::new(0));
        let mut bot = EcsBot::builder("scripted")
            .observe(Script::new(SCRIPT.to_vec()))
            .on(|value: &u16| *value >= 500, Count(Rc::clone(&counter)))
            .build(&net_grants())?;

        let mut fired = Vec::new();
        let mut revisions = Vec::new();
        for _ in 0..5 {
            fired.push(bot.tick()?);
            revisions.push(bot.revisions().first().copied().unwrap_or_default());
        }

        // `Revision` moves on the ticks the value actually moved: the first
        // poll, the step to 503, and the step back to 200.
        assert_eq!(
            revisions,
            vec![1, 1, 2, 2, 3],
            "Revision must track value movement, not system execution"
        );
        // 503 is held for two ticks; the condition is true on both, and the
        // effect runs once, on the transition.
        assert_eq!(fired, vec![0, 0, 1, 0, 0]);
        assert_eq!(counter.get(), 1);
        Ok(())
    }

    #[test]
    fn building_without_the_grant_is_refused() -> TestResult {
        let counter = Rc::new(Cell::new(0));
        match EcsBot::builder("ungranted")
            .observe(Script::new(SCRIPT.to_vec()))
            .on(|value: &u16| *value >= 500, Count(counter))
            .build(&GrantSet::empty())
        {
            Ok(_) => Err("a source requiring `bot.net` built without it".into()),
            Err(error) => {
                assert!(
                    matches!(error, BotError::CapabilityDenied { .. }),
                    "expected CapabilityDenied, got {error:?}"
                );
                Ok(())
            }
        }
    }

    /// Admission as one answer rather than a loop.
    ///
    /// This bot is short of `bot.net` on its source and `bot.fs` on its action.
    /// Admission that returned at the first unmet requirement told the caller
    /// `bot.net`, and `bot.fs` only appeared on the next build — so a person
    /// repairing a bot discovered its requirements one refusal at a time. The
    /// gate already knows both; one refusal now carries both, and each says
    /// which domain declared it.
    #[test]
    fn admission_names_every_unmet_requirement_and_the_domain_that_declared_it() -> TestResult {
        match EcsBot::builder("short")
            .observe(Needs {
                caps: vec![Cap::net()],
                value: 1,
            })
            .on(
                |value: &u16| *value > 0,
                NeedsCaps {
                    caps: vec![Cap::fs()],
                    domain: "test::writes",
                },
            )
            .build(&GrantSet::empty())
        {
            Ok(_) => Err("a bot requiring two ungranted capabilities built".into()),
            Err(BotError::CapabilityDenied { deficit }) => {
                let named: Vec<(&str, Option<&str>)> = deficit
                    .shortages()
                    .map(|shortage| {
                        (
                            shortage.required().as_str(),
                            shortage.demand().map(Demand::domain),
                        )
                    })
                    .collect();
                assert_eq!(
                    named,
                    vec![
                        (Cap::NET, Some("test::needs")),
                        (Cap::FS, Some("test::writes")),
                    ],
                    "one refusal must name both requirements and both domains: {deficit}"
                );
                // And the repair is one call, not one per refusal.
                assert!(
                    deficit
                        .to_grant_set()
                        .admit(&[Cap::net(), Cap::fs()])
                        .is_ok(),
                    "the deficit must derive the whole repair: {deficit}"
                );
                Ok(())
            }
            Err(other) => Err(format!("expected a capability denial, got {other:?}").into()),
        }
    }

    #[test]
    fn a_poll_error_is_returned_and_commits_nothing() -> TestResult {
        let counter = Rc::new(Cell::new(0));
        let mut bot = EcsBot::builder("exhausting")
            .observe(Exhausting {
                remaining: Rc::new(Cell::new(1)),
                caps: vec![Cap::net()],
            })
            .on(|value: &u16| *value >= 500, Count(Rc::clone(&counter)))
            .build(&net_grants())?;

        assert_eq!(
            bot.tick()?,
            0,
            "the first poll answers 200 and fires nothing"
        );
        let after_success = bot.revisions();

        // The second poll fails. The tick reports it, and the observed value
        // from the first tick must survive: a failed tick leaves the world as
        // it was rather than half-updated.
        match bot.tick() {
            Ok(_) => return Err("a failing poll was reported as a successful tick".into()),
            Err(error) => assert!(
                matches!(error, BotError::DomainError { .. }),
                "expected the observer's own error, got {error:?}"
            ),
        }
        assert_eq!(
            bot.revisions(),
            after_success,
            "a failed tick must not commit a new Revision"
        );
        assert_eq!(
            counter.get(),
            0,
            "a failed poll fires nothing: the action must not have run"
        );
        Ok(())
    }

    #[test]
    fn a_failure_mid_chain_leaves_the_untouched_entries_pending() -> TestResult {
        // The counterexample to "a tick is all-or-nothing", and the regression
        // for the repair. Three entries: the first records an effect, the
        // second is refused once, the third is never reached on the failing
        // tick.
        //
        // Before the repair this test asserted the *loss*: the revision was
        // committed before any action ran, so the unchanged second tick fired
        // nothing, the refused entry was never retried, and the third entry's
        // effect was absent forever with no record of it. That assertion
        // encoded the defect as correct and has been replaced.
        //
        // The script holds still at 200 so the second tick has an unchanged
        // source, which is what makes both halves falsifiable: a moving source
        // would fire again for reasons unrelated to the failure.
        let first = Rc::new(RefCell::new(Vec::new()));
        let second = Rc::new(RefCell::new(Vec::new()));
        let third = Rc::new(RefCell::new(Vec::new()));
        let mut bot = EcsBot::builder("partial")
            .observe(Script::new(vec![200, 200]))
            .on(|value: &u16| *value >= 200, Record(Rc::clone(&first)))
            .on(
                |value: &u16| *value >= 200,
                Flaky {
                    remaining: Rc::new(Cell::new(1)),
                    log: Rc::clone(&second),
                    kind: FlakyKind::Refused,
                },
            )
            .on(|value: &u16| *value >= 200, Record(Rc::clone(&third)))
            .build(&net_grants())?;

        match bot.tick() {
            Ok(fired) => {
                return Err(format!("a refused action was reported as {fired} fired").into());
            }
            Err(error) => assert!(
                matches!(error, BotError::DomainError { .. }),
                "expected the action's own error, got {error:?}"
            ),
        }
        assert_eq!(
            *first.borrow(),
            vec![200],
            "the action before the failure ran and its effect is not rolled back"
        );
        assert!(
            second.borrow().is_empty(),
            "the refused action recorded nothing"
        );
        assert!(
            third.borrow().is_empty(),
            "the entry after the failure was not attempted on the failing tick"
        );
        assert_eq!(
            identities(&bot),
            vec![(0, 1), (0, 2)],
            "the refused entry and the entry behind it are both still outstanding"
        );
        assert!(
            matches!(
                first_hold(&bot),
                Some(TransitionHold::Failed {
                    attempts: 1,
                    budget: 3,
                    ..
                })
            ),
            "the refused entry is held as a definite failure with the attempt counted: {:?}",
            first_hold(&bot)
        );

        // The source has not moved. The retry and the untouched third entry run
        // anyway, because eligible work is retained rather than re-derived from
        // the change filter.
        assert_eq!(
            bot.tick()?,
            2,
            "the refused entry is retried and the entry behind it runs, on a still source"
        );
        assert_eq!(
            *first.borrow(),
            vec![200],
            "the acknowledged first effect is not replayed to reach the third entry"
        );
        assert_eq!(
            *second.borrow(),
            vec![200],
            "the retried action ran exactly once, on the second tick"
        );
        assert_eq!(
            *third.borrow(),
            vec![200],
            "the effect the old code lost ran once the entry ahead of it settled"
        );
        assert!(
            bot.pending().is_empty(),
            "a clean tick means nothing is left holding the transition: {:?}",
            bot.pending()
        );
        Ok(())
    }

    #[test]
    fn a_failure_before_the_first_effect_still_runs_it_once() -> TestResult {
        // The narrowest version of the defect: nothing has happened yet, the
        // first entry never stops failing, and the entry behind it must still
        // run — after the attempt budget is spent and the entry is abandoned,
        // which is reported rather than silent.
        let attempts = Rc::new(Cell::new(0));
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut bot = EcsBot::builder("refusing")
            .observe(Script::new(vec![200, 200, 200, 200]))
            .on(|value: &u16| *value >= 200, Refuses(Rc::clone(&attempts)))
            .on(|value: &u16| *value >= 200, Record(Rc::clone(&log)))
            .build(&net_grants())?;

        for round in 1..=2 {
            match bot.tick() {
                Ok(fired) => return Err(format!("round {round} reported {fired} fired").into()),
                Err(error) => assert!(
                    matches!(error, BotError::DomainError { .. }),
                    "round {round}: expected the action's own error, got {error:?}"
                ),
            }
        }
        assert_eq!(attempts.get(), 2, "two ticks, two attempts");
        assert!(
            log.borrow().is_empty(),
            "the second entry is not run ahead of the entry holding it back"
        );
        assert_eq!(
            identities(&bot),
            vec![(0, 0), (0, 1)],
            "both entries are outstanding while the first keeps failing"
        );

        // Third attempt: the budget is spent and the entry is given up on
        // rather than retried forever. The tick reports the action's own typed
        // error, not a summary of it: a caller classifies a retry by matching
        // the variant, and rendering it into a string would throw that away.
        match bot.tick() {
            Ok(fired) => return Err(format!("a spent budget was reported as {fired} fired").into()),
            Err(error) => assert!(
                matches!(error, BotError::DomainError { .. }),
                "the abandonment does not replace the error that caused it: {error:?}"
            ),
        }
        assert_eq!(attempts.get(), 3, "exactly the declared budget was spent");

        // Giving up is reported, never silent: the entry is named with the
        // reason and the cause, and stays named.
        assert_eq!(
            first_hold(&bot),
            Some(TransitionHold::Abandoned {
                reason: AbandonReason::AttemptsExhausted { attempts: 3 },
                cause: "test::refuses: refused".into(),
            }),
            "the given-up-on entry is named after the tick that gave up on it"
        );

        // And the work it was holding back is not lost with it — but it is not
        // run either. The walk stops at the attempt that spent the budget, and
        // the abandonment it leaves is a barrier: the successor stays
        // `NotStarted` behind it and stays reported.
        //
        // These two assertions used to read `assert_eq!(bot.tick()?, 1, "the
        // unattempted work runs as soon as the entry ahead of it is decided")`
        // followed by `assert_eq!(*log.borrow(), vec![200])`, and later
        // `assert_eq!(bot.tick()?, 0, "the resolved transition fires nothing")`
        // while `pending()` was non-empty. Both are the F02 defect written down
        // as the intent — a successor executed past an abandoned prerequisite,
        // and a clean tick reported over work the ledger still names. They are
        // corrected rather than loosened: the tick is now expected to refuse
        // where it was expected to succeed.
        assert!(
            log.borrow().is_empty(),
            "the successor is not run ahead of the entry that was just decided"
        );
        match bot.tick() {
            Ok(fired) => {
                return Err(format!(
                    "a tick with an abandoned prerequisite reported {fired} fired and ran {:?}",
                    log.borrow()
                )
                .into());
            }
            Err(error) => assert!(
                matches!(error, BotError::PendingTransition { .. }),
                "expected the unresolved chain to be reported, got {error:?}"
            ),
        }
        assert!(
            log.borrow().is_empty(),
            "the unattempted work does not run past the entry that was given up on"
        );

        // Terminal, and the rest of the chain is blocked behind it, so the
        // transition stays reported: the abandonment first, because it is the
        // entry that explains the one behind it.
        match bot.tick() {
            Ok(fired) => {
                return Err(format!(
                    "an abandoned entry was reported as {fired} fired while pending() still \
                     names it: {:?}",
                    bot.pending()
                )
                .into());
            }
            Err(error) => assert!(
                matches!(error, BotError::PendingTransition { .. }),
                "expected the unresolved chain to be reported, got {error:?}"
            ),
        }
        assert!(
            identities(&bot) == vec![(0, 0), (0, 1)],
            "the abandoned entry and the entry it blocks are what is left to report: {:?}",
            bot.pending()
        );
        Ok(())
    }

    #[test]
    fn one_attempt_is_a_policy_a_caller_can_declare() -> TestResult {
        // The budget is a policy, not a constant: with one attempt the same
        // chain abandons on the first refusal instead of the third.
        let attempts = Rc::new(Cell::new(0));
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut bot = EcsBot::builder("impatient")
            .with_retry_policy(RetryPolicy::ONE_ATTEMPT)
            .observe(Script::new(vec![200, 200, 200]))
            .on(|value: &u16| *value >= 200, Refuses(Rc::clone(&attempts)))
            .on(|value: &u16| *value >= 200, Record(Rc::clone(&log)))
            .build(&net_grants())?;

        match bot.tick() {
            Ok(fired) => return Err(format!("a refusal was reported as {fired} fired").into()),
            Err(error) => assert!(
                matches!(error, BotError::DomainError { .. }),
                "expected the action's own error, got {error:?}"
            ),
        }
        assert_eq!(attempts.get(), 1, "the declared budget is one attempt");
        assert!(
            log.borrow().is_empty(),
            "the entry behind the abandoned one is not run in the tick that abandoned it"
        );

        // The next tick does not run what the abandoned entry was holding back,
        // and does not report a clean tick either. This assertion used to read
        // `assert_eq!(bot.tick()?, 1, "the next tick runs the work the abandoned
        // entry was holding back")`, which is the defect written down as the
        // intent: an abandonment is a prerequisite that is *not* satisfied, so
        // the send behind an unreserved draft must not run. It is corrected
        // rather than loosened — the tick is now expected to refuse, which is
        // strictly more than it was expected to do before.
        match bot.tick() {
            Ok(fired) => {
                return Err(format!(
                    "a tick with an abandoned prerequisite reported {fired} fired and ran {:?}",
                    log.borrow()
                )
                .into());
            }
            Err(error) => assert!(
                matches!(error, BotError::PendingTransition { .. }),
                "expected the unresolved chain to be reported, got {error:?}"
            ),
        }
        assert_eq!(attempts.get(), 1, "the abandoned entry is not retried");
        assert!(
            log.borrow().is_empty(),
            "the successor of an abandoned entry is not attempted while it stands abandoned"
        );
        assert_eq!(
            identities(&bot),
            vec![(0, 0), (0, 1)],
            "the abandonment and the entry it blocks are both still reported: {:?}",
            bot.pending()
        );

        // Evidence that the effect did not happen is the way past the barrier:
        // the entry becomes eligible again and its successor with it. The
        // action refuses once more, so this tick abandons it again — which is
        // the point of asserting it: evidence reopens the chain, it does not
        // promise the retry will succeed.
        let blocked = bot.pending().into_iter().next().ok_or("held")?;
        bot.resolve_effect(blocked.id(), blocked.revision(), EffectEvidence::NotApplied)?;
        match bot.tick() {
            Ok(fired) => {
                return Err(format!("a refusal was reported as {fired} fired").into());
            }
            Err(error) => assert!(
                matches!(error, BotError::DomainError { .. }),
                "expected the retry's own error, got {error:?}"
            ),
        }
        assert_eq!(attempts.get(), 2, "the revived entry attempted again");
        assert!(
            log.borrow().is_empty(),
            "and the successor is behind the barrier again, because the retry failed too"
        );
        Ok(())
    }

    #[test]
    fn a_failure_does_not_stop_a_later_chain() -> TestResult {
        // The second half of the defect: the old loop broke out of *every*
        // chain on the first error, so an independent chain behind a broken one
        // never ran, and never would.
        let attempts = Rc::new(Cell::new(0));
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut bot = EcsBot::builder("two-chains")
            .observe(Script::new(vec![200, 200]))
            .on(|value: &u16| *value >= 200, Refuses(Rc::clone(&attempts)))
            .observe(Script::new(vec![200, 200]))
            .on(|value: &u16| *value >= 200, Record(Rc::clone(&log)))
            .build(&net_grants())?;

        match bot.tick() {
            Ok(fired) => {
                return Err(format!("a refused action was reported as {fired} fired").into());
            }
            Err(error) => assert!(
                matches!(error, BotError::DomainError { .. }),
                "expected the action's own error, got {error:?}"
            ),
        }
        assert_eq!(
            *log.borrow(),
            vec![200],
            "the chain after the failure ran: a failure is contained to its own chain"
        );
        assert_eq!(
            identities(&bot),
            vec![(0, 0)],
            "only the refusing chain has work outstanding: {:?}",
            bot.pending()
        );
        Ok(())
    }

    #[test]
    fn an_indeterminate_effect_is_held_until_evidence_says_what_happened() -> TestResult {
        // The effect may have happened, so a retry is a possible duplicate. The
        // substrate refuses to guess: it holds the entry, reports it every
        // tick, and waits for evidence.
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut bot = EcsBot::builder("indeterminate")
            .observe(Script::new(vec![200, 200, 200]))
            .on(
                |value: &u16| *value >= 200,
                Flaky {
                    remaining: Rc::new(Cell::new(1)),
                    log: Rc::clone(&log),
                    kind: FlakyKind::Indeterminate,
                },
            )
            .build(&net_grants())?;

        match bot.tick() {
            Ok(fired) => {
                return Err(
                    format!("an indeterminate effect was reported as {fired} fired").into(),
                );
            }
            Err(error) => assert!(
                matches!(error, BotError::EffectIndeterminate { .. }),
                "expected the action's own error, got {error:?}"
            ),
        }
        assert!(
            log.borrow().is_empty(),
            "an indeterminate effect is not an effect: nothing was recorded"
        );

        // The source has not moved, and the entry is not re-attempted: a blind
        // retry is exactly the duplicate this state exists to prevent.
        match bot.tick() {
            Ok(fired) => {
                return Err(format!("a held effect was reported as {fired} fired").into());
            }
            Err(error) => {
                let BotError::PendingTransition { work, outstanding } = error else {
                    return Err(format!("expected PendingTransition, got {error:?}").into());
                };
                assert_eq!(identity(&work), (0, 0), "the held entry is named");
                assert!(
                    matches!(
                        work.hold(),
                        TransitionHold::OutcomeUnknown { attempts: 1, .. }
                    ),
                    "the hold carries the attempt that could not be settled: {:?}",
                    work.hold()
                );
                assert_eq!(outstanding, 1, "one entry is still open");
                assert!(
                    bot.pending()
                        .iter()
                        .all(|work| !matches!(work.hold(), TransitionHold::Abandoned { .. })),
                    "nothing was given up on: {:?}",
                    bot.pending()
                );
            }
        }
        assert!(
            log.borrow().is_empty(),
            "a held effect is not attempted again without evidence"
        );

        // Evidence that it did not happen: the entry becomes eligible again and
        // the attempt finally lands. The revision the report carries is passed
        // back with the identity, which is what makes this delivery sound: the
        // evidence is about the generation the caller was shown.
        let held = bot
            .pending()
            .into_iter()
            .next()
            .ok_or("the held entry disappeared from the report")?;
        bot.resolve_effect(held.id(), held.revision(), EffectEvidence::NotApplied)?;
        assert_eq!(bot.tick()?, 1, "the entry runs once evidence authorises it");
        assert_eq!(*log.borrow(), vec![200], "the effect happened, once");
        assert!(
            bot.pending().is_empty(),
            "and nothing is left holding the transition: {:?}",
            bot.pending()
        );
        Ok(())
    }

    #[test]
    fn evidence_that_the_effect_happened_records_it_without_replaying_it() -> TestResult {
        // The other half of the evidence question, and the one that saves the
        // duplicate: the caller knows the effect is live, so it is acknowledged
        // and never attempted again.
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut bot = EcsBot::builder("acknowledged")
            .observe(Script::new(vec![200, 200, 200]))
            .on(
                |value: &u16| *value >= 200,
                Flaky {
                    remaining: Rc::new(Cell::new(1)),
                    log: Rc::clone(&log),
                    kind: FlakyKind::Indeterminate,
                },
            )
            .build(&net_grants())?;

        match bot.tick() {
            Ok(fired) => {
                return Err(
                    format!("an indeterminate effect was reported as {fired} fired").into(),
                );
            }
            Err(error) => assert!(
                matches!(error, BotError::EffectIndeterminate { .. }),
                "expected the action's own error, got {error:?}"
            ),
        }

        let held = bot
            .pending()
            .into_iter()
            .next()
            .ok_or("the held entry is not reported")?;
        bot.resolve_effect(held.id(), held.revision(), EffectEvidence::Applied)?;
        assert_eq!(
            bot.tick()?,
            0,
            "an acknowledged effect is never attempted again"
        );
        assert!(
            log.borrow().is_empty(),
            "the action that may already have run was not run a second time"
        );
        assert_eq!(bot.revisions(), vec![1], "and the transition is resolved");
        assert!(
            bot.pending().is_empty(),
            "nothing is left pending: {:?}",
            bot.pending()
        );

        // Evidence about an entry that is not held is refused rather than
        // silently accepted: a caller that thinks it acknowledged something
        // needs to find out that it did not. The generation is still named
        // here, and the refusal is still `NoSuchWork` rather than a generation
        // complaint — the tick above resolved the transition and dropped its
        // slot, so there is genuinely no held effect at this address. That
        // distinction is the whole reason the two new refusals exist.
        match bot.resolve_effect(held.id(), held.revision(), EffectEvidence::Applied) {
            Ok(()) => Err("evidence was accepted for an entry that is not held".into()),
            Err(error) => {
                assert!(
                    matches!(error, BotError::NoSuchWork { work } if work == held.id()),
                    "expected NoSuchWork for {:?}, got {error:?}",
                    held.id()
                );
                Ok(())
            }
        }
    }

    #[test]
    fn a_source_that_moves_while_work_is_outstanding_neither_loses_nor_duplicates_it() -> TestResult
    {
        // Two movements while work is outstanding, and then a movement after it
        // resolves. An entry the transition is *holding* reads the payload the
        // transition was opened under — not the newest one — while an entry in a
        // new transition reads the newest; the effect that already happened is
        // not replayed; and the chain is not wedged afterwards.
        let first = Rc::new(RefCell::new(Vec::new()));
        let second = Rc::new(RefCell::new(Vec::new()));
        let mut bot = EcsBot::builder("moving")
            .observe(Script::new(vec![200, 503, 200, 200]))
            .on(|value: &u16| *value >= 200, Record(Rc::clone(&first)))
            .on(
                |value: &u16| *value >= 200,
                Flaky {
                    remaining: Rc::new(Cell::new(1)),
                    log: Rc::clone(&second),
                    kind: FlakyKind::Refused,
                },
            )
            .build(&net_grants())?;

        match bot.tick() {
            Ok(fired) => return Err(format!("a refusal was reported as {fired} fired").into()),
            Err(error) => assert!(
                matches!(error, BotError::DomainError { .. }),
                "expected the action's own error, got {error:?}"
            ),
        }
        assert_eq!(*first.borrow(), vec![200], "the first entry ran once");
        assert!(second.borrow().is_empty(), "the second entry refused");

        // 503 arrives while the second entry is still outstanding. The
        // acknowledged first entry is not replayed to reach it, and the retry
        // runs against the value the transition is *bound to*: 200, the value
        // the first entry already succeeded on.
        //
        // This is the invariant, and it used to read the other way — the retry
        // took the value as it stood, and this test asserted `vec![503]` under
        // the words "the outstanding entry reads the newest value, not the one
        // it failed on". That is F04 stated as intent: entry zero had produced
        // an effect of 200, entry one would then produce an effect of 503, and
        // the transition would have acted on two command inputs while reporting
        // itself as one unit of work. A transition is one input.
        assert_eq!(bot.tick()?, 1, "only the outstanding entry runs");
        assert_eq!(
            *first.borrow(),
            vec![200],
            "an effect the transition already acknowledged is not replayed for a later movement"
        );
        assert_eq!(
            *second.borrow(),
            vec![200],
            "the outstanding entry reads the payload its transition was opened under, \
             not the one that arrived since"
        );
        assert!(
            bot.pending().is_empty(),
            "the transition is fully resolved: {:?}",
            bot.pending()
        );

        // The source is back at 200 by the next tick, which is the value the
        // transition was opened under and acknowledged against. Nothing is
        // admitted, so nothing fires: the 503 the source passed through on the
        // way is superseded, and re-running both entries here would replay an
        // effect that is already acknowledged — the duplicate half of this
        // test's name.
        //
        // This assertion used to read `assert_eq!(bot.tick()?, 2, "a new
        // movement re-evaluates both entries")` with `vec![503, 200]` logged for
        // the second entry, and it was only true because the retry above had
        // already jumped to 503. Under the binding it is 200 that is current,
        // 200 that the chain has already handled, and 503 that nothing ever
        // acted on — and nothing claims it did.
        //
        // What a source that *stays* moved does is the other half of the
        // invariant, and `an_open_transition_is_bound_to_the_payload_it_was_opened_under`
        // is where it is pinned: the new value is admitted as soon as the
        // transition has nothing open, over every entry, against itself.
        assert_eq!(
            bot.tick()?,
            0,
            "a source that has returned to the value the transition was acknowledged \
             for is not new work"
        );
        assert_eq!(
            *first.borrow(),
            vec![200],
            "and the acknowledged effect is not replayed to make it look like one"
        );
        assert_eq!(*second.borrow(), vec![200], "nor is the second entry");
        // A movement that did not move is still nothing at all.
        assert_eq!(
            bot.tick()?,
            0,
            "a still source with no outstanding work fires nothing"
        );
        assert_eq!(
            bot.revisions(),
            vec![3],
            "three polls moved the value: 200, 503, 200"
        );
        Ok(())
    }

    #[test]
    fn an_attempt_whose_outcome_was_never_recorded_is_held_not_replayed() -> TestResult {
        // The state a tick leaves behind when an attempt was begun and no
        // outcome was recorded. The effect may be live, so the next tick holds
        // it — replaying it is the duplicate this state exists to prevent.
        //
        // Constructed directly rather than by killing a tick: the ledger is the
        // record of what happened, and this is what that record looks like when
        // a tick's walk found the entry unrecorded. Nothing in the substrate
        // produces the state and returns — the write sits immediately before the
        // attempt and is overwritten by the outcome — which is why the record is
        // exercised here rather than through an action.
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut bot = EcsBot::builder("interrupted")
            .observe(Script::new(vec![200, 200, 200]))
            .on(|value: &u16| *value >= 200, Record(Rc::clone(&log)))
            .build(&net_grants())?;

        assert_eq!(bot.tick()?, 1, "the first tick runs the action");
        {
            let revision = *bot.revisions().first().ok_or("the source has a revision")?;
            // The payload is part of the record: a transition is bound to the
            // observation it was opened under, and the value here is the one the
            // script has been answering with. `None` would be a transition
            // opened with nothing observed, which holds its entries rather than
            // evaluating them against a value nobody read — a real state, but
            // not this one.
            let mut transition = Transition::opened(revision, 1, Some(Erased::new(200_u16)));
            *transition
                .entries
                .first_mut()
                .ok_or("the chain declares no entries")? = EntryState::Unrecorded;
            bot.world.non_send_mut::<Ledger>().put(0, Some(transition));
        }

        assert!(
            matches!(first_hold(&bot), Some(TransitionHold::Unrecorded)),
            "the interrupted attempt is reported as unrecorded: {:?}",
            first_hold(&bot)
        );
        match bot.tick() {
            Ok(fired) => return Err(format!("an unrecorded attempt fired {fired}").into()),
            Err(error) => {
                let BotError::PendingTransition { work, .. } = error else {
                    return Err(format!("expected PendingTransition, got {error:?}").into());
                };
                assert!(
                    matches!(work.hold(), TransitionHold::Unrecorded),
                    "the tick reports it as held, not as handled: {:?}",
                    work.hold()
                );
            }
        }
        assert_eq!(
            *log.borrow(),
            vec![200],
            "the action was not run a second time: the first attempt may have taken effect"
        );

        // Evidence that it did not take effect is what unlocks the retry, and
        // it is evidence about the generation the caller was shown.
        let held = bot
            .pending()
            .into_iter()
            .next()
            .ok_or("the held entry is not reported")?;
        bot.resolve_effect(held.id(), held.revision(), EffectEvidence::NotApplied)?;
        assert_eq!(bot.tick()?, 1, "and then it runs");
        assert_eq!(*log.borrow(), vec![200, 200], "exactly once more");
        Ok(())
    }

    #[test]
    fn a_condition_that_cannot_be_evaluated_holds_the_chain_without_claiming_anything() -> TestResult
    {
        // A condition failure decides nothing about the effect. The entry has
        // not been attempted, nothing about it is claimed, and the entry behind
        // it is not run ahead of a question that has no answer — the failure
        // the old substrate turned into "this chain is done".
        //
        // The mismatched condition is built *behind* the builder now, because
        // the typed `on` refuses it: a `u16` source cannot carry a condition
        // that reads a `String`. That refusal is the front half of this fix, and
        // the back half is still needed, because erasure is not the only way to
        // reach a mis-paired chain — anything holding this world can insert an
        // entry. `typed_entry` is the erasure helper and keeps its free `T`, so
        // it can still express the defect; this test is the one place that does,
        // on purpose, to pin what happens when it is reached.
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut bot = EcsBot::builder("mismatched")
            .observe(Script::new(vec![200, 200]))
            .on(|value: &u16| *value >= 200, Record(Rc::clone(&log)))
            .build(&net_grants())?;
        bot.world.non_send_mut::<Chains>().0[0].entries.insert(
            0,
            typed_entry::<_, _, String>(|value: &String| value.len() >= 3, Record(Rc::clone(&log))),
        );

        match bot.tick() {
            Ok(fired) => {
                return Err(format!("an evaluate error was reported as {fired} fired").into());
            }
            Err(error) => assert!(
                matches!(error, BotError::EvaluateError { .. }),
                "expected the condition's own error, got {error:?}"
            ),
        }
        assert!(log.borrow().is_empty(), "nothing was attempted");
        assert_eq!(
            identities(&bot),
            vec![(0, 0), (0, 1)],
            "the entry whose condition failed is still outstanding, and so is the entry \
             behind it: {:?}",
            bot.pending()
        );
        assert_eq!(
            first_hold(&bot),
            Some(TransitionHold::NotStarted),
            "a condition that cannot be evaluated is not an attempt, so nothing is abandoned"
        );

        // And it stays that way rather than decaying into an abandonment: the
        // report is the truth, not a stage on the way to losing the work.
        match bot.tick() {
            Ok(fired) => return Err(format!("the second tick reported {fired} fired").into()),
            Err(error) => assert!(
                matches!(error, BotError::EvaluateError { .. }),
                "the condition is evaluated again, not silently given up on: {error:?}"
            ),
        }
        assert!(log.borrow().is_empty(), "and still nothing was attempted");
        Ok(())
    }

    #[test]
    fn a_condition_failure_stops_the_walk_after_the_effects_it_cleared() -> TestResult {
        // The other half of "partial run", on the condition side, and a
        // characterisation rather than a new guarantee: this passes before and
        // after the decision phase was split out of the effect phase, and it is
        // here because that split is only sound if it does.
        //
        // Conditions are pure over the observed value, so the whole effect
        // program can be recorded before any of it runs. The equivalence to the
        // interleaved loop it replaced rests on exactly three things: the
        // entries before a failing condition still run, the entries after it
        // are not attempted, and the tick reports the condition's own error.
        let log = Rc::new(RefCell::new(Vec::new()));
        let after = Rc::new(Cell::new(0));
        let mut bot = EcsBot::builder("condition-failure")
            .observe(Script::new(vec![200, 200]))
            .on(|value: &u16| *value >= 200, Record(Rc::clone(&log)))
            .on(RefusesToEvaluate, Count(Rc::clone(&after)))
            .on(|value: &u16| *value >= 200, Count(Rc::clone(&after)))
            .build(&net_grants())?;

        match bot.tick() {
            Ok(fired) => {
                return Err(format!("a refused condition was reported as {fired} fired").into());
            }
            Err(error) => assert!(
                matches!(error, BotError::EvaluateError { .. }),
                "expected the condition's own error, got {error:?}"
            ),
        }
        assert_eq!(
            *log.borrow(),
            vec![200],
            "the effect the walk cleared before the failing condition ran"
        );
        assert_eq!(
            after.get(),
            0,
            "the entries after the failing condition were not attempted"
        );
        Ok(())
    }

    #[test]
    fn an_ambiguous_schedule_is_refused_at_build() -> TestResult {
        let mut world = World::new();

        // Two exclusive systems conflict with everything, including each other,
        // so their relative order is indeterminate.
        let mut ambiguous = Schedule::default();
        ambiguous.set_build_settings(ScheduleBuildSettings {
            ambiguity_detection: LogLevel::Error,
            ..Default::default()
        });
        ambiguous.add_systems((observe_fold, fire_plan));

        match validate(&mut ambiguous, &mut world) {
            Ok(()) => return Err("an ambiguous schedule was accepted at build".into()),
            Err(error) => assert!(
                matches!(error, BotError::DomainError { .. }),
                "a schedule-build failure must surface as a typed BotError, got {error:?}"
            ),
        }

        // Control: the same two systems, ordered, must validate. Without this
        // the refusal above could be any failure at all.
        let mut ordered = Schedule::default();
        ordered.set_build_settings(ScheduleBuildSettings {
            ambiguity_detection: LogLevel::Error,
            ..Default::default()
        });
        ordered.add_systems((observe_fold, fire_plan).chain());
        validate(&mut ordered, &mut world)?;
        Ok(())
    }

    /// An action that declares `Input = u32` while counting its attempts.
    ///
    /// Paired below with a `u16` source: the pairing is the fault, and the
    /// count is how the test tells "one attempt" from "the whole budget".
    struct CountsU32(Rc<Cell<usize>>);

    impl Execute for CountsU32 {
        type Input = u32;
        type Output = ();

        fn required_caps(&self) -> &[Cap] {
            &[]
        }

        async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
            call.0.check(&[])?;
            self.0.set(self.0.get().saturating_add(1));
            Ok(())
        }

        fn domain_id(&self) -> &str {
            "test::counts_u32"
        }
    }

    /// An action whose failure is permanent and happens before dispatch: the
    /// shape every "binding required" domain has.
    struct RefusesPermanently(Rc<Cell<usize>>);

    impl Execute for RefusesPermanently {
        type Input = u16;
        type Output = ();

        fn required_caps(&self) -> &[Cap] {
            &[]
        }

        async fn execute_action(&self, call: (Auth, &u16)) -> Result<(), BotError> {
            call.0.check(&[])?;
            self.0.set(self.0.get().saturating_add(1));
            Err(BotError::DomainError {
                domain: "test::permanent".into(),
                certainty: DispatchCertainty::Refused,
                cause: "no binding is installed for this domain".into(),
            })
        }

        fn domain_id(&self) -> &str {
            "test::permanent"
        }
    }

    #[test]
    fn a_mispairing_behind_the_erasure_is_a_witness_miss() -> TestResult {
        // The pairing from `Chains` to `Observed` is by index, and a vector
        // position carries no type. This is that pairing gone wrong: the chain
        // declares a `u16` source, and the staged value is a `u32`.
        //
        // Written into the staging resource directly, because the shipped path
        // cannot produce this state: `poll_sources` boxes the output of the very
        // source it took the witness from, so a disagreement means the world
        // moved behind the schedule's back. That is precisely the state the
        // witness exists to refuse rather than to downcast, and it is why the
        // test has to construct it rather than trigger it.
        //
        // The assertions are the three things the witness is for. It fires
        // *before* the value is used, so nothing is committed and no condition
        // is asked. It names both types and the site. And it is a mismatch, not
        // a domain failure, so the disposition is terminal and no retry budget
        // is touched.
        let ran = Rc::new(Cell::new(0));
        let mut bot = EcsBot::builder("mispairing")
            .observe(Script::new(vec![200]))
            .on(|value: &u16| *value >= 200, Refuses(Rc::clone(&ran)))
            .build(&net_grants())?;

        bot.world.non_send_mut::<Polled>().0 = vec![Ok(Erased::new(300u32))];
        bot.schedule.run(&mut bot.world);

        match bot.world.resource::<TickError>().0 {
            Some(BotError::TypeMismatch {
                site,
                chain,
                expected,
                observed,
            }) => {
                assert_eq!(site, "observe_fold rendezvous", "the site is greppable");
                assert_eq!(chain, Some(0), "the report names which chain disagreed");
                assert_eq!(expected, "u16", "the chain's claim");
                assert_eq!(observed, "u32", "what actually arrived");
            }
            ref other => {
                return Err(format!("expected a witness miss, got {other:?}").into());
            }
        }

        assert!(
            bot.world
                .non_send::<Observed>()
                .0
                .iter()
                .all(Option::is_none),
            "the fold commits nothing on a mismatch, so the previous tick's state survives"
        );
        assert_eq!(ran.get(), 0, "the action was never reached");
        Ok(())
    }

    #[test]
    fn a_mismatched_pairing_costs_one_attempt_not_the_retry_budget() -> TestResult {
        // The chain is wired for `u16` while the action declares `u32`. That is
        // a wiring defect behind the erasure, not a domain failure: no number
        // of retries reaches a different answer, so it costs one attempt and
        // lands as terminal rather than spending the whole budget and being
        // reported as "we ran out of budget" — which names the budget as the
        // reason when the reason is that the two halves disagree.
        //
        // The report names both types, which is what makes it a diagnosis
        // rather than a report that something is wrong somewhere. It names no
        // domain, because no domain was reached.
        //
        // The attempt count is read from the ledger rather than counted by the
        // action, because the action must never be reached: the downcast is
        // checked before the proof is issued, so a mismatch costs no side
        // effect. `ran` is that second half.
        // Built behind the builder for the same reason as the condition
        // mismatch above: `on` no longer admits `CountsU32` on a `u16` source,
        // which is the fix, and the downcast arm is still what reports the
        // mis-pairing that reaches the erasure boundary anyway.
        let ran = Rc::new(Cell::new(0));
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut bot = EcsBot::builder("mismatched")
            .observe(Script::new(vec![200, 200, 200, 200]))
            .on(|value: &u16| *value >= 200, Record(Rc::clone(&log)))
            .build(&net_grants())?;
        bot.world.non_send_mut::<Chains>().0[0].entries[0] =
            typed_entry::<_, _, u16>(|value: &u16| *value >= 200, CountsU32(Rc::clone(&ran)));

        match bot.tick() {
            Ok(fired) => {
                return Err(format!("a wiring mismatch was reported as {fired} fired").into());
            }
            Err(error) => assert!(
                matches!(
                    error,
                    BotError::TypeMismatch {
                        site: "spec::typed_entry",
                        expected: "u32",
                        observed: "u16",
                        ..
                    }
                ),
                "a wiring defect is a type mismatch naming both types, not a domain failure: {error:?}"
            ),
        }
        assert_eq!(
            ran.get(),
            0,
            "the downcast is checked before the action runs, so the mismatch was not a side effect"
        );
        assert!(
            matches!(
                first_hold(&bot),
                Some(TransitionHold::Abandoned {
                    reason: AbandonReason::Terminal,
                    ..
                })
            ),
            "one attempt, and the disposition is terminal rather than `AttemptsExhausted`: {:?}",
            first_hold(&bot)
        );

        // The second tick must not spend a second attempt on the same answer.
        // That is the observable difference the budget makes: `AttemptsExhausted`
        // is a fact about how many times the substrate was willing to ask, and
        // asking twice here buys nothing.
        match bot.tick() {
            Ok(fired) => {
                return Err(format!("a retried wiring mismatch reported {fired} fired").into());
            }
            Err(error) => assert!(
                matches!(error, BotError::PendingTransition { .. }),
                "the abandoned entry is the barrier the next tick reports, got {error:?}"
            ),
        }
        Ok(())
    }

    #[test]
    fn a_permanent_refusal_does_not_burn_the_retry_budget() -> TestResult {
        // A refusal that happens before dispatch and cannot be changed by
        // repeating it. Retrying spends attempts to learn the same answer, and
        // the report then blames the budget for a failure the budget was never
        // the reason for.
        let attempts = Rc::new(Cell::new(0));
        let mut bot = EcsBot::builder("permanent")
            .observe(Script::new(vec![200, 200, 200, 200]))
            .on(
                |value: &u16| *value >= 200,
                RefusesPermanently(Rc::clone(&attempts)),
            )
            .build(&net_grants())?;

        match bot.tick() {
            Ok(fired) => {
                return Err(format!("a permanent refusal was reported as {fired} fired").into());
            }
            Err(error) => assert!(
                matches!(error, BotError::DomainError { .. }),
                "expected the action's own error, got {error:?}"
            ),
        }
        assert_eq!(attempts.get(), 1, "a permanent refusal is attempted once");
        assert!(
            matches!(
                first_hold(&bot),
                Some(TransitionHold::Abandoned {
                    reason: AbandonReason::Terminal,
                    ..
                })
            ),
            "the disposition is terminal, not `AttemptsExhausted`: {:?}",
            first_hold(&bot)
        );
        Ok(())
    }
}
