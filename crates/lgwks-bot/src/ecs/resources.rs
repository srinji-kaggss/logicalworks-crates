//! World resources and components the schedule reads and writes each tick.

use std::any::Any;
use std::time::Duration;

// `self` is load-bearing: the `Component` and `Resource` derives expand to
// `bevy_ecs::…` paths, so the crate name has to be in scope at the use site even
// though every edge in this crate is written through the `lgwks_deps` facade.
#[cfg(doc)]
use lgwks_deps::bevy_ecs::schedule::IntoScheduleConfigs;
use lgwks_deps::bevy_ecs::{
    self,
    prelude::{Component, Entity, Resource, World},
};

use crate::error::BotError;
use crate::gate::GrantSet;
use crate::registry::Source;
use crate::spec::{ChainEntry, Erased, ObserveAny, Witness};
#[cfg(doc)]
use crate::verb::Observe;
use crate::verb::RefreshReason;

use super::admit::AdmitKind;
#[cfg(doc)]
use super::bot::EcsBot;
use super::entry::RetryPolicy;
use super::work::AdmittedInput;

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
    pub(super) domain: String,
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
pub(crate) struct Revision(pub(super) u64);

// ── Resources ──────────────────────────────────────────────────────────────

/// The capability proof, as a world resource: one authority per world, no
/// ambient singleton.
#[derive(Resource, Debug)]
pub(super) struct Grants(pub(super) GrantSet);

/// Effects fired on the most recent tick.
#[derive(Resource, Debug, Default)]
pub(super) struct Fired(pub(super) usize);

/// The first tick-time failure, if any.
///
/// An exclusive system returns `()` and cannot propagate, so the error is
/// parked here and `EcsBot::tick` takes it. This is not a workaround for a
/// missing feature: it is the shape that keeps the failure ordered, because the
/// systems stop at the first error and the driver reports that one.
#[derive(Resource, Debug, Default)]
pub(super) struct TickError(pub(super) Option<BotError>);

/// The source entities in chain order.
#[derive(Resource, Debug, Default)]
pub(super) struct Order(pub(super) Vec<Entity>);

/// The observation phase's reusable buffers.
///
/// Every buffer here was a fresh heap allocation on every tick, and each of them
/// was paid by a tick that changed nothing: a quiet tick allocated a
/// `Vec<Option<RefreshReason>>` the size of the world, a `Vec<usize>` for stalls
/// that stayed empty, and a `Vec<(usize, &EcsChain)>` holding the whole wave
/// before deciding that none of the wave had to be polled. A substrate that asks
/// "has anything moved?" once per chain per tick and then allocates several
/// times per chain to ask it is not measuring the question.
///
/// So the buffers live here, are taken out for the phase, and go back with their
/// capacity: the first tick pays for them and no tick after pays again. The
/// contents are cleared every tick and never retained — a scratch that kept its
/// entries would be a cache, and what these hold is per-tick state whose whole
/// meaning is that it does not survive.
#[derive(Resource, Debug, Default)]
pub(super) struct PollScratch {
    /// One entry per chain: what that chain's source declared about its own
    /// caching, read once per tick.
    pub(super) declared: Vec<Option<RefreshReason>>,
    /// The chains whose poll the wave gave up on, in declaration order.
    pub(super) stalled: Vec<usize>,
    /// The current wave's own indices that have to be polled.
    pub(super) slots: Vec<usize>,
}

/// The change ticks: the revision each chain's committed value was read at, and
/// the revision each source reported on this tick.
///
/// Two buffers rather than one because they answer two different questions, and
/// conflating them is the mistake that makes a poll look like a commit: `seen` is
/// what the substrate **holds**, and moves only where a value was committed;
/// `polled` is what the source **said** on the tick the value was read. A poll
/// that failed, was cancelled at its deadline, or produced a value equal to the
/// one already held writes `polled` and leaves `seen` alone, so the next tick
/// asks again — the fingerprint-commit rule (#99 / INV-BOT-5) applied to a change
/// tick rather than to a digest.
#[derive(Debug, Default)]
pub(super) struct ChangeTicks {
    /// The revision of the value the substrate currently holds, per chain.
    pub(super) seen: Vec<Option<u64>>,
    /// The revision each source reported on this tick, per chain.
    pub(super) polled: Vec<Option<u64>>,
}

impl ChangeTicks {
    /// Whether `chain`'s source reported the revision its committed value was
    /// read at, so the poll can be skipped.
    ///
    /// Equality is the whole comparison. A revision that wraps, or regresses, is a
    /// *different* revision and therefore a change: there is no ordering here that
    /// a wrap could invalidate, and a source is free to use a saturating counter,
    /// a timestamp in its own units, or a content digest as its revision.
    pub(super) fn quiet(&self, chain: usize, reported: Option<u64>) -> bool {
        reported.is_some() && self.seen.get(chain).copied().flatten() == reported
    }

    /// Commit the revision a value was read at, and only then.
    ///
    /// Called from the commit pass of [`observe_fold`] and nowhere else, which is
    /// what makes the rule a fact about one place rather than an intention.
    pub(super) fn commit(&mut self, chain: usize, polled: Option<u64>) {
        if let Some(slot) = self.seen.get_mut(chain) {
            *slot = polled;
        }
    }
}

/// The decision phase's per-tick scratch: which chains moved, what the walk will
/// do with each chain's live transition, and the admitted input each binding
/// takes.
///
/// A world resource rather than a local, because it is per-tick scratch and the
/// point of scratch is that it is allocated once. Built as `vec![false; count]`
/// inside the decision system, this was a fresh zeroed allocation every tick —
/// one of a set of them, added together, that made the schedule's own bookkeeping
/// the larger half of a tick's cost.
///
/// One resource for all three rather than three, because they are resized
/// together: a plan that grows its chain count is a bot whose chain count grew,
/// and three buffers resized independently could hold three different widths for
/// one tick.
#[derive(Default)]
pub(super) struct AdmitScratch {
    /// Which chains the change filter reported as moved.
    pub(super) moving: Vec<bool>,
    /// What the walk will do with each chain, in declaration order.
    pub(super) kinds: Vec<AdmitKind>,
    /// The admitted-input identity each binding takes, `None` where it binds none.
    pub(super) inputs: Vec<Option<AdmittedInput>>,
}

/// The decision pass's scratch, taken out whole so the walk can read and write it
/// while the world is borrowed for the query.
///
/// The one step rather than three because the borrow is the whole reason:
/// `fire_plan` holds `&mut World`, and a mutable borrow of a single resource
/// cannot survive the immutable borrow the change query needs. One `mem::take`
/// moves all three buffers and keeps all three allocations.
impl AdmitScratch {
    /// Take the scratch out of the world, resized to `count` chains.
    pub(super) fn take(world: &mut World, count: usize) -> Self {
        let mut scratch = std::mem::take(&mut *world.non_send_mut::<AdmitScratch>());
        scratch.moving.clear();
        scratch.moving.resize(count, false);
        scratch.kinds.clear();
        scratch.kinds.resize(count, AdmitKind::Idle);
        scratch.inputs.clear();
        scratch.inputs.resize(count, None);
        scratch
    }
}

/// The retry policy in force: one authority per world, like `Grants`.
#[derive(Resource, Debug)]
pub(super) struct Policy(pub(super) RetryPolicy);

/// The chains whose cached baseline cannot be trusted, and why.
///
/// One flag per chain, indexed exactly as [`Chains`] is, and one entry per tick
/// per chain — so this is bounded by the chain count for the life of the bot and
/// cannot grow with the number of ticks. The lifecycle is deliberately one tick
/// long: a chain marked here is polled with `None` handed to it as its
/// baseline, so whatever the source reports *is* committed as its new baseline,
/// and the flag is spent. A flag that outlived the forced poll would keep the
/// baseline out of the comparison indefinitely, which is the permanent quiet
/// state this resource exists to prevent — a slower version of the defect.
///
/// Spent rather than cleared for the same reason: a chain whose forced poll
/// *failed* has committed nothing, so its baseline is still whatever it was,
/// and the next tick must be forced again. Clearing the flag there would return
/// the bot to comparing against the unsound baseline it just failed to replace.
#[derive(Debug, Default)]
pub(super) struct Invalidated {
    /// The reason each chain declared, or `None` where it declared nothing.
    pub(super) reasons: Vec<Option<RefreshReason>>,
}

/// The bot's declared per-poll deadline, as the commit step reads it.
///
/// A resource beside `EcsBot::poll_deadline` rather than a second source of
/// truth: `observe_fold` is a schedule step and can only reach the world, so the
/// value it needs to put on a typed cancellation has to be there. It is inserted
/// from the same field the observation phase reads, so the number on the error and
/// the number that bounded the poll cannot differ.
#[derive(Debug, Clone, Copy, Resource)]
pub(super) struct PollBudget(pub(super) Duration);

impl Invalidated {
    /// Mark `chain` invalid for `reason`, keeping the reason it already had.
    ///
    /// First-write-wins rather than last-write-wins, because the driver walks the
    /// chains in declaration order and a report that varied with poll resolution
    /// order would not be comparable across two runs of one seed. One reason per
    /// chain is what makes the report a statement about the run rather than
    /// about how its sources happened to resolve.
    pub(super) fn mark(&mut self, chain: usize, reason: RefreshReason) {
        if let Some(slot) = self.reasons.get_mut(chain)
            && slot.is_none()
        {
            *slot = Some(reason);
        }
    }

    /// Whether `chain` must be polled without a baseline this tick.
    pub(super) fn is_invalid(&self, chain: usize) -> bool {
        self.reasons
            .get(chain)
            .is_some_and(|reason| reason.is_some())
    }
}

/// One chain's entry on a tick report: a baseline the source itself declared
/// unsound, and the reason it named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForcedRefresh {
    /// The chain whose cached baseline was refused.
    pub(super) chain: usize,
    /// The source's own domain identifier, read through `domain`.
    pub(super) domain: String,
    /// The reason, read through `reason`.
    pub(super) reason: RefreshReason,
}

impl ForcedRefresh {
    /// The chain whose cached baseline was refused.
    ///
    /// The chain's index in declaration order, which is what `pending()` and the
    /// settlement reports name too.
    #[must_use]
    pub const fn chain(&self) -> usize {
        self.chain
    }

    /// The source's own domain identifier.
    ///
    /// The same spelling `Observe::domain_id` returns, so a report is readable
    /// without the caller having to hold the declaration order.
    #[must_use]
    pub fn domain(&self) -> &str {
        &self.domain
    }

    /// Why the source declared its baseline unsound.
    #[must_use]
    pub const fn reason(&self) -> RefreshReason {
        self.reason
    }
}

/// One source whose poll this tick cancelled at its per-poll deadline.
///
/// Reported rather than dropped, and beside [`ForcedRefresh`] rather than folded
/// into it, because the two are different facts with different repairs: a forced
/// refresh is a source that *told* the substrate its baseline was unsound, and a
/// stall is a source that *stopped answering*. The first is fixed by the domain
/// reconnecting; the second is bounded by the deadline, and a bot that reported
/// one as the other would send a reader to the wrong place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StalledSource {
    /// The chain whose poll was cancelled, read through `chain`.
    pub(super) chain: usize,
    /// The source's own domain identifier, read through `domain`.
    pub(super) domain: String,
    /// The budget that was applied, read through `deadline`.
    pub(super) deadline: Duration,
}

impl StalledSource {
    /// The chain whose poll was cancelled.
    ///
    /// The chain's index in declaration order, which is what `pending()` and the
    /// settlement reports name too, so one index identifies a chain across every
    /// report this bot produces.
    #[must_use]
    pub const fn chain(&self) -> usize {
        self.chain
    }

    /// The source's own domain identifier.
    ///
    /// The same spelling `Observe::domain_id` returns, so a caller triaging a
    /// stall can name the domain without holding the declaration order — which is
    /// the whole reason the report carries it.
    #[must_use]
    pub fn domain(&self) -> &str {
        &self.domain
    }

    /// The per-poll budget that was applied when this poll was cancelled.
    #[must_use]
    pub const fn deadline(&self) -> Duration {
        self.deadline
    }
}

/// What one tick observed about its own sources, beside the effects it fired.
///
/// The count of fired effects answers "what did this tick do"; this answers
/// "what did it have to look at again, and why", which is a different question
/// and the one a caller needs when a bot has gone quiet. A tick that fired
/// nothing because every source legitimately held still is a healthy tick. A
/// tick that fired nothing because a source's baseline stopped being a fact is
/// a bug the count cannot see, and this is where it becomes visible.
///
/// Read through [`EcsBot::tick_report`] rather than off the tick's return
/// value, because the tick's `Result` carries an *error* and this carries what
/// was true about the sources either way: a tick that failed still observed, and
/// a caller triaging the failure needs to know whether the sources were the
/// reason.
#[derive(Debug, Clone, Default, PartialEq, Eq, Resource)]
pub struct TickReport {
    /// Effects that fired on this tick, read through `fired`.
    pub(super) fired: usize,
    /// The chains whose baseline this tick refused, read through `forced`.
    pub(super) forced: Vec<ForcedRefresh>,
    /// The observations this tick passed over, read through `superseded`.
    pub(super) superseded: Vec<SupersededObservation>,
    /// The sources this tick stopped waiting for, read through `stalled`.
    pub(super) stalled: Vec<StalledSource>,
    /// Deadline watchdogs this tick started, read through `watchdogs`.
    pub(super) watchdogs: u32,
}

impl TickReport {
    /// Effects that fired on this tick.
    #[must_use]
    pub const fn fired(&self) -> usize {
        self.fired
    }

    /// Chains whose cached baseline this tick refused and re-read.
    ///
    /// In chain order, at most one entry per chain: the first reason the tick
    /// recorded for a chain, which is the one whose forced read has not landed.
    /// Empty when every source legitimately held still — which is the answer a
    /// caller most needs to be able to distinguish from the next one.
    #[must_use]
    pub fn forced(&self) -> &[ForcedRefresh] {
        &self.forced
    }

    /// Intermediate observations replaced before any entry acted on them.
    ///
    /// In chain order, at most one entry per chain. Reported here rather than
    /// as a fired effect or as a retired one, because it is neither: the value
    /// was never due. Counting it as work done would overstate what the tick
    /// did, and counting it as work retired would understate how many values the
    /// run passed over.
    #[must_use]
    pub fn superseded(&self) -> &[SupersededObservation] {
        &self.superseded
    }

    /// Whether this tick re-read anything it had decided it could keep.
    #[must_use]
    pub fn forced_any(&self) -> bool {
        !self.forced.is_empty()
    }

    /// Whether this tick passed over an intermediate observation.
    #[must_use]
    pub fn superseded_any(&self) -> bool {
        !self.superseded.is_empty()
    }

    /// Sources whose poll this tick cancelled at the per-poll deadline.
    ///
    /// In chain order, at most one entry per chain. Every one of them committed
    /// nothing, so a caller reconciling "what did the tick see" against the fired
    /// count must read this beside `fired` rather than instead of it: the chains
    /// named here are the ones whose absence from the effects is explained, not
    /// the ones that had nothing to say.
    ///
    /// Empty is the ordinary answer, and it is distinguishable from a tick that
    /// never happened because this is published by the tick that ran, whatever
    /// that tick's own result was.
    #[must_use]
    pub fn stalled(&self) -> &[StalledSource] {
        &self.stalled
    }

    /// Whether this tick gave up on any source.
    #[must_use]
    pub fn stalled_any(&self) -> bool {
        !self.stalled.is_empty()
    }

    /// Deadline watchdogs this tick started.
    ///
    /// An OS thread is a real cost on a host with a real thread ceiling, so the
    /// count belongs in the operator's view of the tick beside the stall it may
    /// have produced. It is zero for the ordinary tick — every source answered
    /// on its first poll, so no poll ever needed watching — and at most one per
    /// observation wave that had a source still pending, because a wave shares
    /// one deadline and therefore one watcher.
    ///
    /// A watchdog that could not be started is not counted here: the count is
    /// what the host actually started, and a source reported stalled beside a
    /// zero is the one honest shape that combination can have.
    #[must_use]
    pub const fn watchdogs(&self) -> u32 {
        self.watchdogs
    }
}

/// One intermediate observation the substrate passed over in latest-state mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupersededObservation {
    /// The chain, read through `chain`.
    pub(super) chain: usize,
    /// The revision, read through `revision`.
    pub(super) revision: u64,
}

impl SupersededObservation {
    /// The chain whose value was replaced.
    #[must_use]
    pub const fn chain(&self) -> usize {
        self.chain
    }

    /// The revision the replaced value was committed under.
    ///
    /// The revision of the *replaced* value, not of the one that took its place:
    /// what a caller wants to correlate is "the generation for revision 4 never
    /// ran", and revision 4 is the one this names.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }
}

/// Whether each chain's committed observation has been admitted into a
/// generation yet, and under which revision.
///
/// One entry per chain, so the answer is per-chain constant time rather than a
/// walk of the transitions. The distinction it records is the one that decides
/// what a replaced value *was*:
///
/// - **Admitted.** The value is sitting in the slot because the generation that
///   ran on it has finished and handed it back. Replacing it loses nothing: it
///   has already been acted on.
/// - **Not admitted.** The value is sitting in the slot because a generation was
///   still being walked when it arrived, so it could not be admitted yet — and a
///   newer one arrived before the walk finished. This value will *never* be
///   acted on, because the substrate admits only the newest observation once the
///   walk releases the chain. Replacing it passes over an intermediate state,
///   which is what latest-state mode means and what has to be reported.
///
/// Without this, the two are indistinguishable from the outside and the second
/// is silently dropped: a caller watching a counter see it jump 1 → 2 → 3 has no
/// way to learn that 2 was never observed as a state, only that it was never
/// acted on. Those are different facts and only one of them is a loss.
#[derive(Debug, Default)]
pub(super) struct Committed {
    /// Per chain: has the value in the observation slot been admitted, and
    /// under which revision was it committed.
    pub(super) slots: Vec<SlotAdmission>,
}

/// What one chain's committed observation is waiting for.
///
/// Three states rather than one boolean, because "was replaced before it was
/// acted on" and "has never been observed at all" both read as `false` in the
/// two-state form, and conflating them reports the *first* observation of every
/// chain as a pass-over. That is not cosmetic: it would make a fresh bot claim
/// it had skipped a state it had never seen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum SlotState {
    /// Nothing has been committed for this chain yet.
    #[default]
    Nothing,
    /// A value is committed and a generation has run over it.
    Admitted,
    /// A value is committed and no generation has run over it.
    Unacted,
}

/// What one chain's committed observation is waiting for.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct SlotAdmission {
    /// Where the chain's committed value stands.
    pub(super) state: SlotState,
    /// The revision the value in the slot was committed under.
    pub(super) revision: u64,
}

impl Committed {
    /// Note that `chain` committed a value under `revision`, not yet admitted.
    pub(super) fn commit(&mut self, chain: usize, revision: u64) {
        if let Some(slot) = self.slots.get_mut(chain) {
            slot.state = SlotState::Unacted;
            slot.revision = revision;
        }
    }

    /// Note that a generation has been opened over `chain`'s committed value.
    ///
    /// Called from the handover, where the binding leaves the transition and
    /// becomes the slot's standing baseline. The value is then *acted on*, and
    /// replacing it later is not a supersession.
    pub(super) fn admit(&mut self, chain: usize) {
        if let Some(slot) = self.slots.get_mut(chain) {
            slot.state = SlotState::Admitted;
        }
    }

    /// The admission of `chain`'s committed value, for the supersession check.
    ///
    /// `None` for a chain this bot does not have: the slots are sized to the
    /// chains at build, so that is a caller's index error, and it is answered as
    /// absent rather than as a chain that has committed nothing.
    pub(super) fn slot(&self, chain: usize) -> Option<SlotAdmission> {
        self.slots.get(chain).copied()
    }
}

// ── Non-send state: the verbs and the values ───────────────────────────────

/// One observation chain, holding the same erased halves a
/// [`Chain`] does.
pub(crate) struct EcsChain {
    /// The observer. `Box<dyn ObserveAny>`'s `poll_any` is not `Send`, which is
    /// why this cannot live in a component.
    pub(super) source: Box<dyn ObserveAny>,
    /// Equality for this observer's output, captured from the `PartialEq` bound
    /// on `EcsBuilder::observe`. Change detection needs to tell "the same
    /// value again" from "a new value", and the erased `Box<dyn Any>` cannot
    /// answer that on its own.
    pub(super) same: fn(&dyn Any, &dyn Any) -> bool,
    /// The admitted-input identity of an erased binding, captured from
    /// `S::Output: InputIdentity` where the type was still a parameter.
    pub(super) identify: fn(&dyn Any) -> AdmittedInput,
    /// The `(condition, action)` tuples, in declaration order.
    pub(super) entries: Vec<ChainEntry>,
    /// What type this chain's source produces, taken where `S::Output` was
    /// still a type parameter and checked at every rendezvous.
    ///
    /// The chain is the *claim*: it is what the condition and the action were
    /// built against. The value that arrives each tick carries its own witness,
    /// and the rendezvous compares them. Neither half can be checked alone —
    /// both are erased — so the pair is the proof.
    pub(super) witness: Witness,
}

impl EcsChain {
    /// Assemble a chain from a registry-built source and its erased entries.
    ///
    /// The materializer's seam, and the only place a chain's metadata is taken
    /// from something other than a concrete `S` parameter. The four pieces are
    /// captured by [`Source::new`] at the one point where
    /// `S::Output` was still a type parameter, so a chain built this way is the
    /// same value a native chain erases to — the tick path cannot tell them
    /// apart, which is exactly T25's claim.
    pub(super) fn from_registry(source: Source, entries: Vec<ChainEntry>) -> Self {
        let (inner, same, identify, witness) = source.into_parts();
        Self {
            source: inner,
            same,
            identify,
            witness,
            entries,
        }
    }
}

/// Every chain, in declaration order.
#[derive(Default)]
pub(super) struct Chains(pub(super) Vec<EcsChain>);

/// The observed value per chain, from the most recent successful poll.
///
/// Paired to [`Chains`] by index, and by index only: a vector position carries
/// no type. Nothing here proves that `Observed[i]` belongs to `Chains[i]`, which
/// is why each value keeps its [`Witness`] and the fold compares it against the
/// chain's before committing anything.
#[derive(Default)]
pub(super) struct Observed(pub(super) Vec<Option<Erased>>);

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
/// An `Ok(None)` is not "no observation": it is the source's answer that the
/// value it just produced is **equal** to the baseline it was handed, so the
/// substrate keeps the value it already holds. Only `Ok(Some(_))` carries a new
/// payload, and only those are boxed.
#[derive(Default)]
pub(super) struct Polled(pub(super) Vec<Result<Option<Erased>, BotError>>);

/// Which chains moved on this tick, one flag per chain.
///
/// Per-tick scratch, like [`Moving`], and a resource for the same reason: it is
/// rebuilt on every tick of every bot and a rebuilt buffer is an allocation.
///
/// Named `Moved` and not `Changed` because `Changed` in this module is bevy's
/// query filter, and shadowing it would silently retarget every
/// `Changed<Revision>` in the file at this struct.
#[derive(Default)]
pub(super) struct Moved(pub(super) Vec<bool>);

/// What the decision phase decided for one entry it reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Decision {
    /// The condition held, so the entry is attempted.
    ///
    /// It does not say *which* attempt this will be. The number belongs to the
    /// ledger, which is the only thing that can keep it monotonic across a
    /// settlement, and it is handed back by [`Ledger::begin`]. Deriving it here
    /// from [`EntryState`] gave a settled entry the number one all over again,
    /// so two attempts at one address could answer to the same name.
    Attempt,
    /// The condition did not hold, so the entry is recorded as skipped and the
    /// walk moves past it. Recorded rather than dropped: an entry whose
    /// condition is false owes nothing, and saying so is what lets a chain
    /// settle and stop being walked.
    Skip,
}

/// One entry the decision phase reached.
pub(super) struct Step {
    /// Index into [`Chains`].
    pub(super) chain: usize,
    /// Index into that chain's `entries`, in declaration order.
    pub(super) entry: usize,
    /// What the walk decided for it.
    pub(super) decision: Decision,
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
pub(super) struct Plan {
    /// The decisions to apply, in declaration order: one chain's entries in
    /// walk order, then the next chain's.
    pub(super) steps: Vec<Step>,
    /// The condition failure each chain's walk met, indexed by chain. A chain
    /// whose walk met none has `None`, and an action failure recorded by the
    /// driver displaces the condition failure of the chain it happened in.
    pub(super) failures: Vec<Option<BotError>>,
}
