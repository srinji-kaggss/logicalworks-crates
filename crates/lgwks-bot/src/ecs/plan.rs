//! Admission and input planning: what a tick may poll, and what it resumes.

// imports
use super::BotError;
use super::Duration;
use super::Erased;
use super::RetryClass;
use super::World;
use super::attempts::EntryState;
use super::attempts::RetryTurn;
use super::attempts::Transition;
use super::components::Chains;
use super::components::Committed;
use super::components::Observed;
use super::components::Order;
use super::components::Revision;
use super::ledger::Ledger;
use super::work::AbandonReason;
use super::work::AdmittedInput;

/// The revision the chain's source last moved to, or zero if it never has.
pub(super) fn revision_of(world: &World, chain: usize) -> u64 {
    let revision = world
        .resource::<Order>()
        .0
        .get(chain)
        .and_then(|entity| world.get::<Revision>(*entity));
    match revision {
        Some(revision) => revision.0,
        // A source that has never moved is at revision zero.
        None => 0,
    }
}

/// Move the newest observation for `chain` out of its slot.
///
/// Moved, not copied. A source's `Output` carries no `Clone` bound and this is
/// the reason it does not need one: the value is not wanted in two places at
/// once. Once a transition is bound to it, the transition is what speaks for
/// it, and the slot being empty is not a loss — it is the record that the value
/// is out on loan, which [`observe_fold`] reads back through the binding.
///
/// The take is also what admits the value: a generation is being opened over
/// exactly this payload, so a later commit that overtakes it is superseding work
/// that is already owed and being done, not work that is still unclaimed. Marking
/// it here rather than on the handover is what makes a value that a *held*
/// generation is holding count as acted-on — the handover happens only once the
/// transition is finally dropped, which for an entry awaiting evidence is never
/// on the tick the pass-over occurs.
fn take_observed(world: &mut World, chain: usize) -> Option<Erased> {
    let taken = world
        .non_send_mut::<Observed>()
        .0
        .get_mut(chain)
        .and_then(Option::take);
    if taken.is_some() {
        world.non_send_mut::<Committed>().admit(chain);
    }
    taken
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

/// What the decision pass decided for one chain, before it walks any of them.
///
/// The value is computed in one batched pass over every chain — see
/// [`plan_admissions`] — so the walk that follows reads a decision rather than
/// making one. That is what lets the two halves of change detection be measured
/// apart (which chains moved, and which admitted inputs are new) and it is also
/// what keeps the admitted-input stamps in declaration order: the pass walks
/// chains by index, exactly as the walk this replaces did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AdmitKind {
    /// Keep the live transition exactly as it stands. No observation is taken
    /// and no input is minted: the entries still running were acknowledged
    /// against the payload the transition holds.
    Keep,
    /// Resume into a new revision over the newest observation, bound to
    /// [`AdmittedInput`].
    Resume,
    /// Open a fresh transition over the newest observation, bound to
    /// [`AdmittedInput`]. Only reachable for a chain that has no live
    /// transition.
    Open,
    /// Nothing to walk: no transition is held and the source did not move.
    Idle,
}

/// A chain's batched decision: its [`AdmitKind`] and the input it binds to.
#[derive(Clone, Copy)]
pub(super) enum Admit {
    /// Keep the live transition, or have nothing to walk.
    Keep,
    /// Resume or open, bound to this admitted input.
    Bind(AdmittedInput),
}

impl Admit {
    /// Compose a chain's decision from the two passes that produced it.
    ///
    /// A kind that needs an input the fingerprint pass did not derive reads as
    /// [`Self::Keep`] rather than as a fabricated one: a transition bound to an
    /// input nobody derived would mint dispatch identities that cannot be
    /// recovered against, and keeping the previous transition is the safe answer
    /// because the next tick re-plans from the same observations.
    pub(super) fn bind(kind: AdmitKind, input: Option<AdmittedInput>) -> Self {
        match (kind, input) {
            (AdmitKind::Resume | AdmitKind::Open, Some(input)) => Self::Bind(input),
            _ => Self::Keep,
        }
    }
}

/// Decide every chain's [`AdmitKind`] in one pass, in declaration order.
///
/// Two questions per chain, and the answers are cached rather than recomputed
/// inside the walk: whether the `Changed<Revision>` query reported the chain as
/// moved, and whether the newest observation is admitted over the payload a
/// retained transition holds. The second is the value comparison, and it is the
/// expensive one — computing it here and reading it there is what keeps the walk
/// from paying for it twice.
///
/// The `has_open` test is read here and again by the walk, and it is a scan of
/// one transition's entry states rather than a comparison of values, so the
/// repeat costs less than the caching saves.
pub(super) fn plan_admissions(world: &World, moving: &[bool], kinds: &mut [AdmitKind]) {
    for (chain, kind) in kinds.iter_mut().enumerate() {
        let ledger = world.non_send::<Ledger>();
        let held = ledger.transitions.get(chain).and_then(Option::as_ref);
        *kind = match held {
            Some(transition) if transition.has_open() => AdmitKind::Keep,
            Some(transition) => {
                if admits(world, chain, transition.value.as_ref()) {
                    AdmitKind::Resume
                } else {
                    AdmitKind::Keep
                }
            }
            // A transition is opened **over an observation** or not at all. The
            // change filter alone is not enough to open one: a component is
            // "changed" from the moment it is spawned, so a freshly assembled bot
            // reports every chain as moved with no committed observation behind
            // it, and a transition bound to nothing is never walked — its entries
            // stay `NotStarted` and the tick reports all of them pending.
            //
            // The change-tick skip is what makes this reachable rather than
            // theoretical: a quiet source commits nothing, so its slot stays empty
            // however many ticks the filter keeps calling it moved.
            None if moving.get(chain).copied().is_some_and(|held| held)
                && world
                    .non_send::<Observed>()
                    .0
                    .get(chain)
                    .is_some_and(|slot| slot.is_some()) =>
            {
                AdmitKind::Open
            }
            None => AdmitKind::Idle,
        };
    }
}

/// Derive the admitted-input identity for every chain whose kind binds one.
///
/// One batch, in two halves. The first half holds the world shared and reads the
/// value each identity is derived from; the second holds it exclusively and mints
/// the content-free fallback stamp for the chains whose binding cannot supply
/// one. The split is why the stamps are still consumed in declaration order: the
/// fallback is what a dispatch digest binds when nothing else can be, and two
/// derivations of one identity are two identities the moment they disagree.
pub(super) fn plan_inputs(
    world: &mut World,
    kinds: &[AdmitKind],
    inputs: &mut [Option<AdmittedInput>],
) {
    let binds = |kind: &AdmitKind| matches!(kind, AdmitKind::Resume | AdmitKind::Open);
    {
        let chains = world.non_send::<Chains>();
        let observed = world.non_send::<Observed>();
        for (chain, kind) in kinds.iter().enumerate() {
            if !binds(kind) {
                continue;
            }
            let (Some(held), Some(value)) = (
                chains.0.get(chain),
                observed.0.get(chain).and_then(|slot| slot.as_ref()),
            ) else {
                continue;
            };
            if let Some(slot) = inputs.get_mut(chain) {
                *slot = Some((held.identify)(value.as_any()));
            }
        }
    }
    let mut ledger = world.non_send_mut::<Ledger>();
    for (chain, kind) in kinds.iter().enumerate() {
        if !binds(kind) {
            continue;
        }
        if inputs.get(chain).is_some_and(Option::is_none) {
            // A fallback stamp is content-free, so it can never name an event.
            let minted = AdmittedInput {
                identity: ledger.mint_input(),
                event: false,
            };
            if let Some(slot) = inputs.get_mut(chain) {
                *slot = Some(minted);
            }
        }
    }
}

/// The transition a chain should be walking this tick, if any.
///
/// The decision is [`Admit`]'s, computed for every chain in one batched pass
/// rather than made here per chain: that is what makes the change detection and
/// the identity derivation two separately measurable stages, and it keeps the
/// admitted-input stamps in declaration order.
///
/// Opening and resuming are where the payload is bound, and both *take* it out
/// of the observation slot. That is the mechanism that keeps a transition on
/// one input: the entries it evaluates and runs read the binding, so a value
/// that arrives mid-transition cannot reach them, however many ticks the
/// transition takes to drain. The admission itself is then deferred rather than
/// dropped — the newer observation is still sitting in the slot, and the tick
/// after the transition stops being retained is the tick it becomes work.
pub(super) fn resume(
    world: &mut World,
    chain: usize,
    admit: Admit,
    held: Option<Transition>,
) -> Option<Transition> {
    match (admit, held) {
        // Outstanding work keeps the payload it was opened under. A newer
        // observation is not admitted here, and deliberately: the entries
        // still running were acknowledged against this one.
        (Admit::Keep, Some(transition)) => Some(transition),
        (Admit::Keep, None) => None,
        (Admit::Bind(input), Some(transition)) => {
            let value = take_observed(world, chain);
            Some(Transition::resumed(
                input,
                revision_of(world, chain),
                &transition,
                value,
            ))
        }
        (Admit::Bind(input), None) => {
            let entries = match world.non_send::<Chains>().0.get(chain) {
                Some(chain) => chain.entries.len(),
                // No chain at this index declares no entries to open.
                None => 0,
            };
            let value = take_observed(world, chain);
            Some(Transition::opened(
                input,
                revision_of(world, chain),
                entries,
                value,
            ))
        }
    }
}

/// What a failed attempt means for the entry it was made on.
///
/// The three cases are the three things a caller has to be able to tell apart
/// afterwards: a retry is a retry, a retry may be a duplicate, and a retry is
/// pointless because the failure is not something another attempt can change. The
/// retry policy — budget, backoff and deadline, read on the bot's clock at `turn`
/// — is consulted only for the first of them.
///
/// The classification is [`BotError::retry_class`] and nothing else. The error's
/// *variant* is not the question: `DomainError` is where a permanent refusal, a
/// connection that never opened, and a wiring defect all used to land, and they
/// want three different answers. Reading the rendered `cause` to tell them apart
/// is worse still — it makes a retry policy depend on the wording of a message.
/// The producer states what the failure establishes about the effect, the
/// certainty carries it here, and this function is a total map over that.
pub(super) fn failure_state(
    error: &BotError,
    attempts: u32,
    turn: RetryTurn,
    first: Duration,
    entropy: u64,
) -> EntryState {
    // A post-effect recording failure is not a dispatch failure and is not
    // abandoned: the occurrence is already known, and the only work left is the
    // append. Mapped here rather than through `RetryClass` so `Never` keeps its
    // meaning of "this failure will not change on a retry of the *action*".
    if let BotError::EffectUnrecorded {
        ref key,
        ref evidence,
        ref cause,
    } = *error
    {
        return EntryState::RecordingFailed {
            key: **key,
            evidence: *evidence,
            cause: cause.to_string(),
            attempts,
        };
    }
    let cause = error.to_string();
    match error.retry_class() {
        // The effect definitely did not happen: a retry is a retry, and the
        // policy bounds it in attempts and in time.
        RetryClass::Safe => turn.definite_failure(attempts, first, entropy, cause),
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
