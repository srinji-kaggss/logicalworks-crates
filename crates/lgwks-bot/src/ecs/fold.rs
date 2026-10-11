//! The observe fold and the fire plan, the schedule's two systems.

// `self` is load-bearing: the `Component` and `Resource` derives expand to
// `bevy_ecs::…` paths, so the crate name has to be in scope at the use site even
// though every edge in this crate is written through the `lgwks_deps` facade.
use lgwks_deps::bevy_ecs::{
    prelude::{Changed, World},
    schedule::{
        IntoScheduleConfigs, LogLevel, Schedule, ScheduleBuildSettings, SingleThreadedExecutor,
    },
};

use crate::error::{BotError, DispatchCertainty};
use crate::spec::Erased;
#[cfg(doc)]
use crate::verb::Observe;

// The one declared clock every deadline in this crate names (INV-BOT-30). Only
// its wall watchdog is read below — a source that stopped answering is not
// waiting for time, so a logical counter a caller can advance is the wrong
// authority for it — but it is reached through the crate's clock rather than
// through a free-floating `Instant`, which is what makes the elapsed time that
// cancelled a poll traceable to a named authority.

#[cfg(feature = "profile")]
use super::profile::{Charge, TickStage};

use super::admit::Admit;
use super::admit::AdmitKind;
use super::admit::plan_admissions;
use super::admit::plan_inputs;
use super::admit::resume;
use super::admit::revision_of;
use super::entry::EntryState;
use super::entry::Transition;
use super::ledger::Ledger;
use super::resources::AdmitScratch;
use super::resources::Chains;
use super::resources::ChangeTicks;
use super::resources::Committed;
use super::resources::Decision;
use super::resources::EcsChain;
use super::resources::Invalidated;
use super::resources::Moved;
use super::resources::Observed;
use super::resources::Order;
use super::resources::Plan;
use super::resources::PollBudget;
use super::resources::Polled;
use super::resources::Revision;
use super::resources::SlotState;
use super::resources::SourceId;
use super::resources::Step;
use super::resources::SupersededObservation;
use super::resources::TickError;
use super::resources::TickReport;
use super::work::WorkId;
use super::work::parked;
#[cfg(doc)]
use crate::spec::ObserveAny;

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
pub(super) fn observe_fold(world: &mut World) {
    if parked(world) {
        return;
    }

    let mut polled = std::mem::take(&mut world.non_send_mut::<Polled>().0);

    // Two passes over the results, and neither allocates. The first inspects
    // every result and commits nothing, which is what makes a failed tick leave
    // the world exactly as it was; the second applies the movements the first
    // cleared.
    //
    // The passes are separate for the same reason `collect::<Result<Vec<_>,_>>`
    // was here before: the first error in declaration order is the one `tick`
    // reports, and finding it may not have written a `Revision` or replaced an
    // observation on the way. Building the values into a fresh `Vec` to do that
    // — which is what the collect did — is an allocation per tick bought for
    // nothing, since the values already sit in a buffer that outlives the call.

    // Pass one: the first error, in declaration order. The failing slot is
    // replaced with a benign `Ok(None)` and the error moved out, because
    // `BotError` is the caller's evidence and is deliberately not `Clone` —
    // copying it to report it would let a caller settle an effect against a
    // duplicate of the failure rather than the failure itself.
    //
    // A cancelled poll is **not** one of those errors, and that is the whole
    // difference between the two rows it could otherwise be confused for. A
    // source that refused reported a fact about itself, so the tick that heard
    // it has nothing to commit and reports the failure. A source that stopped
    // answering told the substrate nothing at all, so the chains *beside* it
    // still have real observations to commit — the same rule that already lets
    // one chain's failing action leave the effects before it live. Treating the
    // cancellation as a failure here would mean one wedged source stopped every
    // chain on the bot from committing for as long as it stayed wedged, which is
    // precisely the starvation this deadline exists to prevent.
    let mut first_error = None;
    let mut first_stall = None;
    for (index, result) in polled.iter_mut().enumerate() {
        let stalled = matches!(result, Err(BotError::PollStalled { .. }));
        if stalled {
            first_stall.get_or_insert(index);
            *result = Ok(None);
        } else if result.is_err() && first_error.is_none() {
            first_error = std::mem::replace(result, Ok(None)).err();
        }
    }
    // One stall is reported and the rest are already named in the tick report, so
    // the tick's own `Result` carries a single typed cancellation the way it
    // carries a single first domain failure. The report is the complete list; this
    // is the one a caller matching on the variant can act on.
    match (first_stall, first_error) {
        // A cancellation is reported and the commit *proceeds*: the chains beside
        // the wedged one have real observations, and withholding them is the
        // starvation this deadline exists to prevent.
        (Some(chain), _) => {
            world.resource_mut::<TickError>().0 = Some(BotError::PollStalled {
                chain,
                deadline: world.resource::<PollBudget>().0,
            });
        }
        // A domain failure stops the commit, as it always did: the tick's contract
        // is that a tick which could not act on what a chain said commits nothing.
        (None, Some(error)) => {
            world.resource_mut::<TickError>().0 = Some(error);
            put_polled(world, polled);
            return;
        }
        (None, None) => {}
    }

    // The rendezvous. The results arrived in chain order because `poll_sources`
    // appended them that way, and the pairing from here on is by index — a
    // vector position, which carries no type. The witness is what proves the
    // two halves still agree, and it is checked *before* anything is committed
    // or compared, so a mis-pairing cannot reach a downcast.
    //
    // A miss is a defect in this crate — a chain whose index no longer names the
    // source it was built with — not a domain failure and not something a retry
    // can change. It is reported as `TypeMismatch`, which is classified terminal,
    // so it does not spend a budget; and it commits nothing, so the world is left
    // exactly as the previous tick left it.
    //
    // Only a value that moved is inspected. A source that answered "equal"
    // allocated nothing and has no witness to offer — and needs none: to answer
    // that, it had to downcast the baseline to its own output type, which is a
    // stricter proof of the pairing than comparing two names.
    {
        let chains = world.non_send::<Chains>();
        let mismatch = polled.iter().enumerate().find_map(|(index, result)| {
            let next = result.as_ref().ok()?.as_ref()?;
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
            put_polled(world, polled);
            return;
        }
    }

    // Pass two: commit. A slot that takes a value has moved; a slot left empty
    // has not, and the value the substrate already held for that chain stays
    // exactly where it is.
    //
    // Holding the older value is not a compromise, it is the rule the substrate
    // already follows one step later: `admits` refuses to let a newer
    // observation displace the payload a retained transition is bound to when
    // the two compare equal, and `resume` keeps that binding. A chain whose
    // values compare equal is a chain whose payload has not changed, and the
    // observation the entries were selected against is the one they still run
    // against.
    //
    // The consequence to know about: `PartialEq` that is narrower than identity
    // — a type that compares only a status field and ignores a timestamp — will
    // now hold the *earlier* of two equal values rather than the later one. That
    // is the same value the action was already going to receive through the
    // binding, so nothing an effect observes changes; but a caller reading the
    // observation back after an equal-valued tick gets the older instance. A
    // type whose equality is not its identity should not be a chain's output
    // type, and this is the point where that shows up.
    let mut changed = std::mem::take(&mut world.non_send_mut::<Moved>().0);
    changed.clear();
    changed.resize(polled.len(), false);
    let mut superseded: Vec<SupersededObservation> = Vec::new();
    // The change tick commits **here and only here**: beside the arm that put a
    // new value in the slot. A tick that parked, stopped on a failure, or left a
    // result equal to the value it already held reaches none of it, so the
    // revision stays where it was and the next tick asks again — the
    // fingerprint-commit rule (#99 / INV-BOT-5) applied to a change tick rather
    // than to a digest.
    let revisions = std::mem::take(&mut world.non_send_mut::<ChangeTicks>().polled);
    {
        let mut observed = world.non_send_mut::<Observed>();
        for (index, result) in polled.iter_mut().enumerate() {
            let moved = result
                .as_mut()
                .ok()
                .and_then(Option::take)
                .is_some_and(|value| {
                    if let Some(slot) = observed.0.get_mut(index) {
                        *slot = Some(value);
                        return true;
                    }
                    false
                });
            if let Some(flag) = changed.get_mut(index) {
                *flag = moved;
            }
        }
    }
    // A second pass, with the observation slot released, so the commit reads the
    // same `changed` the revision walk below reads. One flag is one chain: a
    // chain that took a value commits the revision that value was read at, and
    // every other chain's revision stays exactly where it was.
    for index in 0..changed.len() {
        if changed.get(index).copied().is_some_and(|held| held) {
            let reported = revisions.get(index).copied().flatten();
            world.non_send_mut::<ChangeTicks>().commit(index, reported);
        }
    }
    world.non_send_mut::<ChangeTicks>().polled = revisions;
    put_polled(world, polled);

    // Walked by index rather than over a cloned `Order`: the entity is one
    // `copy` away and the clone was a heap allocation per tick to avoid it.
    for (index, moved) in changed.iter().copied().enumerate() {
        if !moved {
            continue;
        }
        let Some(entity) = world.resource::<Order>().0.get(index).copied() else {
            continue;
        };
        if let Some(mut revision) = world.get_mut::<Revision>(entity) {
            revision.0 = revision.0.wrapping_add(1);
        }
    }

    // The revision the *replaced* value was committed under, read before the
    // bump above. A value superseded by this tick carries the revision it was
    // committed at, which is what a caller needs to line the pass-over up with
    // the generation that did or did not run on it.
    //
    // Only `Unacted` is a pass-over. `Nothing` is a first observation, which
    // replaced nothing; `Admitted` is a value a generation already ran on, which
    // also replaced nothing that was still owed. Reading either of those as a
    // pass-over would make a healthy bot claim it had skipped a state.
    for index in changed
        .iter()
        .copied()
        .enumerate()
        .filter_map(|(index, moved)| moved.then_some(index))
    {
        if let Some(previous) = world.non_send::<Committed>().slot(index)
            && previous.state == SlotState::Unacted
        {
            superseded.push(SupersededObservation {
                chain: index,
                revision: previous.revision,
            });
        }
        let revision = revision_of(world, index);
        world.non_send_mut::<Committed>().commit(index, revision);
        // The mark is spent by the read it forced, and only by it. This is the
        // point at which a forced poll has provably committed a value, so the
        // baseline this tick holds is one the source produced rather than one the
        // source disowned. A tick that never reaches here — parked, or stopped on
        // a rendezvous miss — leaves the mark standing, which is the correct
        // outcome for a refresh that did not land.
        if let Some(slot) = world.non_send_mut::<Invalidated>().reasons.get_mut(index) {
            *slot = None;
        }
    }
    world.resource_mut::<TickReport>().superseded = superseded;
    world.non_send_mut::<Moved>().0 = changed;
}

/// Put the staging buffer back, capacity and all.
///
/// The counterpart to the `mem::take` at the top of [`observe_fold`]. Handing
/// the same allocation back is the point: a staging area rebuilt per tick is an
/// allocation per tick, and the observation phase is the one phase guaranteed to
/// run on every tick of every bot.
pub(super) fn put_polled(world: &mut World, polled: Vec<Result<Option<Erased>, BotError>>) {
    world.non_send_mut::<Polled>().0 = polled;
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
pub(super) fn fire_plan(world: &mut World) {
    // The plan's buffers are moved out and put back rather than refilled. This
    // used to clear the plan and then assign it a freshly collected `Vec` for
    // each field — so the `clear` bought nothing and every tick paid two heap
    // allocations per chain to rebuild buffers it was about to overwrite. The
    // buffers are scratch: allocated once, cleared (which keeps capacity), and
    // reused until the chain count actually changes.
    let (mut steps, mut failures) = take_plan_buffers(world);
    steps.clear();
    failures.clear();

    if parked(world) {
        put_plan_buffers(world, steps, failures);
        return;
    }

    let count = world.non_send::<Chains>().0.len();

    // A flag per chain rather than a search of the moved set: the walk below is
    // in declaration order, and membership has to be answerable in constant time
    // for that order to be the one that decides what runs.
    //
    // Taken out of the world because the query below borrows the world
    // immutably; `clear` + `resize` keeps the allocation, where the
    // `vec![false; count]` this replaces made a new zeroed one every tick. The
    // intermediate `Vec<usize>` of moved chains is gone with it: filling the
    // flags directly from the query is one pass instead of two and allocates
    // nothing.
    //
    // The admission plan rides in the same resource, because it is the same
    // per-tick scratch for the same phase and a second resource for it would be
    // a second place for the two halves of change detection to be resized
    // independently.
    let mut scratch = AdmitScratch::take(world, count);
    let AdmitScratch {
        ref mut moving,
        ref mut kinds,
        ref mut inputs,
    } = scratch;

    // Which chains moved, and whether each newest observation is admitted over
    // the payload a retained transition holds. Both are change detection outside
    // the poll, and both are answered here for every chain at once so the walk
    // below reads a decision rather than making one.
    #[cfg(feature = "profile")]
    let compare_charge = Charge::new(TickStage::Compare);
    {
        let mut query = world.query_filtered::<&SourceId, Changed<Revision>>();
        for id in query.iter(world) {
            if let Some(flag) = moving.get_mut(id.chain) {
                *flag = true;
            }
        }
    }
    plan_admissions(world, moving, kinds);
    #[cfg(feature = "profile")]
    drop(compare_charge);

    // The admitted-input identity of every value a chain will bind, derived in
    // one batch so the stamps stay in declaration order.
    #[cfg(feature = "profile")]
    let fingerprint_charge = Charge::new(TickStage::Fingerprint);
    plan_inputs(world, kinds, inputs);
    #[cfg(feature = "profile")]
    drop(fingerprint_charge);

    failures.resize_with(count, || None);

    #[cfg(feature = "profile")]
    let decide_charge = Charge::new(TickStage::Decide);
    for index in 0..count {
        let held = world.non_send_mut::<Ledger>().take(index);
        let admit = Admit::bind(
            // A chain the compare pass did not reach is idle, which is the same
            // answer the buffer's own initial value carries.
            match kinds.get(index).copied() {
                Some(kind) => kind,
                None => AdmitKind::Idle,
            },
            inputs.get(index).copied().flatten(),
        );
        let Some(mut transition) = resume(world, index, admit, held) else {
            continue;
        };

        // The borrow of the world for the walk is scoped: `Ledger::take` above
        // and `Ledger::put` below each need it mutably, while the walk needs the
        // chains and the transition's binding immutably. Nothing is written to
        // the transition here — every state that depends on an attempt is
        // written by the driver, which is the only place that knows the outcome.
        {
            let chains = world.non_send::<Chains>();
            let ledger = world.non_send::<Ledger>();
            if let (Some(chain), Some(value), Some(failure)) = (
                chains.0.get(index),
                transition.value.as_ref(),
                failures.get_mut(index),
            ) {
                plan_chain(
                    chain,
                    &transition,
                    value,
                    index,
                    ledger,
                    &mut steps,
                    failure,
                );
            }
            // No chain, or a transition bound to nothing: the work is kept,
            // not discarded. A transition is opened only for a source that was
            // polled, so a chain with no value bound to it is a world that was
            // mutated behind the schedule's back.
        }

        // ── The handover that keeps the last-seen value alive ─────────────
        //
        // A transition holds its payload out on loan: `Transition::opened`
        // *takes* it out of `Observed`, and while the transition is retained
        // that binding is where the chain's newest value lives. When the
        // transition is finished and dropped instead, the binding goes with
        // it — and unless it is handed back, the chain is left with no
        // baseline at all. The next tick would then compare a fresh
        // observation against nothing, read every source as moved, re-open
        // every chain and fire every entry again, forever.
        //
        // That is not a slowdown, it is the substrate running work it had
        // already done: the fairness gate in `bench/` reports it as the bot
        // firing 896,000 effects where the hand-rolled baseline fires 17,920
        // for identical input. The value goes back only when the slot is
        // empty, because a slot the observation phase filled this tick holds a
        // newer value than the one being handed back.
        let retained = transition.is_retained();
        let returned = if retained {
            None
        } else {
            std::mem::take(&mut transition.value)
        };
        world
            .non_send_mut::<Ledger>()
            .put(index, if retained { Some(transition) } else { None });
        if let Some(returned) = returned {
            // Handed back only when the slot is empty, because a slot the
            // observation phase filled this tick holds a newer value than the
            // one being returned. Nothing to mark here: the value was admitted
            // when its generation took it out of the slot, and a value standing
            // in the slot again is *still* admitted — it has been acted on.
            let mut observed = world.non_send_mut::<Observed>();
            if let Some(slot) = observed.0.get_mut(index).filter(|slot| slot.is_none()) {
                *slot = Some(returned);
            }
        }
    }

    #[cfg(feature = "profile")]
    drop(decide_charge);
    *world.non_send_mut::<AdmitScratch>() = scratch;
    put_plan_buffers(world, steps, failures);
}

/// Take the plan's two buffers, leaving empty ones behind.
///
/// They are taken rather than borrowed so the decision walk can push into them
/// while the world is borrowed immutably for the chains and the transition
/// bindings. Empty `Vec`s are left in their place, so a tick that dies before
/// putting them back stages nothing.
pub(super) fn take_plan_buffers(world: &mut World) -> (Vec<Step>, Vec<Option<BotError>>) {
    let mut plan = world.non_send_mut::<Plan>();
    (
        std::mem::take(&mut plan.steps),
        std::mem::take(&mut plan.failures),
    )
}

/// Put the plan's buffers back, capacity and all.
///
/// The counterpart to [`take_plan_buffers`]. Handing the same allocations back
/// is the whole point: a plan rebuilt from empty every tick allocates once per
/// chain for each of its two fields, which is the cost this pair exists to
/// remove.
pub(super) fn put_plan_buffers(
    world: &mut World,
    steps: Vec<Step>,
    failures: Vec<Option<BotError>>,
) {
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
pub(super) fn plan_chain(
    chain: &EcsChain,
    transition: &Transition,
    value: &Erased,
    index: usize,
    ledger: &Ledger,
    steps: &mut Vec<Step>,
    failure: &mut Option<BotError>,
) {
    for (entry_index, entry) in chain.entries.iter().enumerate() {
        let Some(state) = transition.entries.get(entry_index) else {
            continue;
        };
        // A recovered unknown is a barrier *before* the condition is read.
        // The current observation is not evidence that the earlier effect did
        // not occur, so a false condition may not skip the entry and a true
        // one may not re-enter it; either way the successor does not run
        // (issue #104).
        if ledger
            .action_of(WorkId {
                chain: index,
                entry: entry_index,
            })
            .is_some_and(|action| ledger.effects.blocks(action))
        {
            break;
        }
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
            // A known outcome whose record is missing is walked: the retry is
            // an append, not a dispatch, and `run_chain` takes that path.
            EntryState::RecordingFailed { .. } => {}
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

        steps.push(Step {
            chain: index,
            entry: entry_index,
            decision: Decision::Attempt,
        });
    }
}

/// Build the schedule this substrate runs, with the two settings that make its
/// order a property of the graph rather than of thread arrival.
pub(super) fn schedule() -> Schedule {
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
pub(super) fn validate(schedule: &mut Schedule, world: &mut World) -> Result<(), BotError> {
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
