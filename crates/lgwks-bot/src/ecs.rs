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
//! | `Observe` | a `Chains` entry, polled by the `observe` system |
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

use super::error::BotError;
use super::gate::GrantSet;
use super::spec::{ChainEntry, ObserveAny, typed_entry};
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
}

/// Every chain, in declaration order.
#[derive(Default)]
struct Chains(Vec<EcsChain>);

/// The observed value per chain, from the most recent successful poll.
#[derive(Default)]
struct Observed(Vec<Option<Box<dyn Any>>>);

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
struct Polled(Vec<Result<Box<dyn Any>, BotError>>);

/// One effect the decision phase selected.
struct Step {
    /// Index into [`Chains`].
    chain: usize,
    /// Index into that chain's `entries`, in declaration order.
    entry: usize,
}

/// The effect program the decision phase recorded, plus the first condition
/// failure it met on the way.
///
/// The failure is carried here rather than parked in [`TickError`] because it
/// happened at a *position* in the walk rather than at the end of it: the
/// [`Step`]s alongside it are exactly the effects the walk reached before that
/// position, and they still run. Whichever failure comes first in walk order is
/// the one the tick reports, which is the ordering the synchronous loop had
/// when it evaluated a condition and ran an action in the same iteration.
#[derive(Default)]
struct Plan {
    /// The actions to run, in declaration order.
    steps: Vec<Step>,
    /// The first condition failure, if the walk met one.
    failure: Option<BotError>,
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

    let changed: Vec<bool> = {
        let chains = world.non_send::<Chains>();
        let seen = world.non_send::<Observed>();
        values
            .iter()
            .enumerate()
            .map(
                |(index, next)| match seen.0.get(index).and_then(Option::as_ref) {
                    // No remembered value: this source has, by definition, changed.
                    None => true,
                    Some(previous) => !(chains.0[index].same)(previous.as_ref(), next.as_ref()),
                },
            )
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

/// Fire, decide half: record the effects this tick should run, in declaration
/// order.
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
/// observe an action's effect, which makes the selected list and the first
/// failure the walk meets a pure function of the observations. The plan is
/// therefore the exact effect program the synchronous loop would have walked,
/// and the failure it carries stops the walk where that loop stopped.
fn fire_plan(world: &mut World) {
    {
        let mut plan = world.non_send_mut::<Plan>();
        plan.steps.clear();
        plan.failure = None;
    }

    if parked(world) {
        return;
    }

    let moved: Vec<usize> = {
        let mut query = world.query_filtered::<&SourceId, Changed<Revision>>();
        query.iter(world).map(|id| id.chain).collect()
    };

    let mut steps: Vec<Step> = Vec::new();
    let mut failure: Option<BotError> = None;
    {
        let chains = world.non_send::<Chains>();
        let observed = world.non_send::<Observed>();

        'chains: for index in moved {
            // Two steps, not `Some(Some(value))`: the pattern would be matched
            // against `Option<&Option<_>>`, which `pattern_type_mismatch`
            // refuses.
            let Some(slot) = observed.0.get(index) else {
                continue;
            };
            let Some(value) = slot.as_ref() else {
                continue;
            };
            let Some(chain) = chains.0.get(index) else {
                continue;
            };
            for (entry_index, entry) in chain.entries.iter().enumerate() {
                match entry.condition.check_any(value.as_ref()) {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(error) => {
                        failure = Some(error);
                        break 'chains;
                    }
                }
                steps.push(Step {
                    chain: index,
                    entry: entry_index,
                });
            }
        }
    }

    let mut plan = world.non_send_mut::<Plan>();
    plan.steps = steps;
    plan.failure = failure;
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
    ///    moved, and records the effect program (`observe_fold`, `fire_plan`);
    /// 3. the recorded effects run one at a time, in declaration order;
    /// 4. the count and the first failure are written back.
    ///
    /// The middle two phases are what keep this deterministic: the schedule is
    /// a total order validated at build, the observations are folded
    /// positionally, and the effect program is fixed before any effect runs.
    ///
    /// # Failure
    ///
    /// The same two cases [`EcsBot::tick`] documents, and they still mean
    /// different things: a poll that failed fires nothing (no `Revision` was
    /// written), while an action that failed leaves the effects before it live
    /// and returns the first error. A condition that fails stops the walk where
    /// it failed, so the effects the walk had already reached still run — the
    /// plan carries the failure and the steps that precede it, and the tick
    /// reports whichever comes first in walk order.
    ///
    /// # Cancellation
    ///
    /// Dropping this future before it resolves cancels the tick at the point it
    /// was dropped. Between phases 2 and 3 that loses the effects phase 2
    /// selected, and the next tick does not re-fire them: the revisions were
    /// committed in phase 2, so the sources no longer read as moved. This is
    /// the same "unattempted work is lost, not queued" rule the failure path
    /// documents, and it is stated rather than discovered because it is the
    /// price of a tick that is a future.
    ///
    /// # Errors
    ///
    /// The first domain failure of the tick, in the order described above.
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
        let Plan { steps, failure } = plan;
        let (fired, action_failure) = self.run_steps(&steps).await;
        // An action failure happens *earlier* in walk order than a condition
        // failure that follows it, so it is the more specific report.
        let failure = action_failure.or(failure);

        self.world.resource_mut::<Fired>().0 = fired;
        if let Some(error) = failure {
            self.world.resource_mut::<TickError>().0 = Some(error);
        }

        match self.world.resource_mut::<TickError>().0.take() {
            Some(error) => Err(error),
            None => Ok(fired),
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
    async fn poll_sources(&self) -> Vec<Result<Box<dyn Any>, BotError>> {
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
    /// Returns the number that fired and the first action failure, or `None`
    /// when every one of them resolved. The first failure stops the walk: the
    /// effects after it are not attempted, which is the "unattempted work is
    /// lost, not queued" rule a caller reading `Err` as "nothing happened"
    /// needs to know.
    async fn run_steps(&self, steps: &[Step]) -> (usize, Option<BotError>) {
        let chains = self.world.non_send::<Chains>();
        let observed = self.world.non_send::<Observed>();
        let grants = self.world.resource::<Grants>();

        let mut fired: usize = 0;
        let mut failure: Option<BotError> = None;
        for step in steps {
            let Some(chain) = chains.0.get(step.chain) else {
                continue;
            };
            let Some(entry) = chain.entries.get(step.entry) else {
                continue;
            };
            let Some(value) = observed.0.get(step.chain).and_then(Option::as_ref) else {
                continue;
            };
            match entry.action.run_any(&grants.0, value.as_ref()).await {
                Ok(_) => fired = fired.saturating_add(1),
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        (fired, failure)
    }
}

/// Builder for [`EcsBot`].
pub struct EcsBuilder {
    /// The bot name.
    name: String,
    /// Chains finished so far.
    chains: Vec<EcsChain>,
}

impl EcsBuilder {
    /// Bind a source to observe.
    ///
    /// The `PartialEq` bound is the substrate's one extra requirement, and it
    /// is semantic rather than incidental: the condition on this substrate *is*
    /// change detection, and a value that cannot be compared cannot be detected
    /// as changed. [`Bot::builder`](crate::Bot::builder) carries no such bound
    /// because its condition is re-evaluated every tick and needs no equality.
    pub fn observe<S>(self, source: S) -> EcsObserveBuilder
    where
        S: Observe + 'static,
        S::Output: PartialEq + 'static,
    {
        EcsObserveBuilder {
            name: self.name,
            prior: self.chains,
            source: Box::new(source),
            same: same_output::<S>,
            entries: Vec::new(),
        }
    }

    /// Build with no observation chains: a bot that only serves direct
    /// `Query` and `Execute` calls.
    pub fn build(self, grants: &GrantSet) -> Result<EcsBot, BotError> {
        EcsBot::assemble(self.name, self.chains, grants)
    }
}

impl EcsObserveBuilder {
    /// Add a `(condition, action)` tuple to this chain.
    ///
    /// The tuple's erasure is shared with the `Bot` executor rather than
    /// re-implemented, so the `Auth` issue, the downcast and the type-mismatch
    /// error cannot drift between the two substrates.
    pub fn on<C, A, T>(mut self, condition: C, action: A) -> Self
    where
        T: 'static,
        C: Evaluate<T> + 'static,
        A: Execute + 'static,
        A::Input: 'static,
        A::Output: 'static,
    {
        self.entries.push(typed_entry(condition, action));
        self
    }

    /// Finish this chain and start another.
    pub fn observe<S>(self, source: S) -> EcsObserveBuilder
    where
        S: Observe + 'static,
        S::Output: PartialEq + 'static,
    {
        // Destructured rather than moved field by field: taking `prior` by
        // `mem::take` and then moving `source` out leaves `self` partially
        // moved, and the borrow checker will not let a method call finish the
        // chain in between.
        let EcsObserveBuilder {
            name,
            mut prior,
            source: previous,
            same,
            entries,
        } = self;
        prior.push(EcsChain {
            source: previous,
            same,
            entries,
        });
        EcsObserveBuilder {
            name,
            prior,
            source: Box::new(source),
            same: same_output::<S>,
            entries: Vec::new(),
        }
    }

    /// Assemble the bot, admitting every capability and validating the schedule.
    pub fn build(self, grants: &GrantSet) -> Result<EcsBot, BotError> {
        let EcsObserveBuilder {
            name,
            mut prior,
            source,
            same,
            entries,
        } = self;
        prior.push(EcsChain {
            source,
            same,
            entries,
        });
        EcsBot::assemble(name, prior, grants)
    }
}

/// A chain being assembled.
pub struct EcsObserveBuilder {
    /// Carried from [`EcsBuilder`]; the chain being built does not consume it.
    name: String,
    /// Chains finished by an earlier `observe` call, in order.
    prior: Vec<EcsChain>,
    /// The source being bound.
    source: Box<dyn ObserveAny>,
    /// Equality for `source`'s output.
    same: fn(&dyn Any, &dyn Any) -> bool,
    /// Tuples attached so far.
    entries: Vec<ChainEntry>,
}

impl EcsBot {
    /// Validate and assemble. Shared by both terminal builder calls so
    /// admission cannot drift between them, the same rule
    /// [`spec::assemble`](crate::spec) follows for `Bot`.
    fn assemble(name: String, chains: Vec<EcsChain>, grants: &GrantSet) -> Result<Self, BotError> {
        if name.is_empty() {
            return Err(BotError::IncompleteSpec { field: "name" });
        }
        // The same admission gate `Bot::build` applies: a bot requiring a
        // capability it was not granted fails before it runs, not at tick.
        for chain in &chains {
            grants.admit(chain.source.required_caps())?;
            for entry in &chain.entries {
                grants.admit(entry.action.required_caps())?;
            }
        }

        let mut world = World::new();
        world.insert_resource(Grants(grants.clone()));
        world.insert_resource(Fired::default());
        world.insert_resource(TickError::default());

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
    use crate::cap::{Auth, Cap};

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

    /// An action that is always refused: the second half of a chain whose first
    /// half already took effect.
    struct Fails;

    impl Execute for Fails {
        type Input = u16;
        type Output = ();

        fn required_caps(&self) -> &[Cap] {
            &[]
        }

        async fn execute_action(&self, call: (Auth, &u16)) -> Result<(), BotError> {
            call.0.check(&[])?;
            Err(BotError::DomainError {
                domain: "test::fails".into(),
                cause: "refused after the first action ran".into(),
            })
        }

        fn domain_id(&self) -> &str {
            "test::fails"
        }
    }

    /// A condition that fails structurally rather than evaluating to `false`.
    struct Refuses;

    impl Evaluate<u16> for Refuses {
        fn check(&self, observed: &u16) -> Result<bool, BotError> {
            Err(BotError::EvaluateError {
                cause: format!("this condition cannot decide about {observed}"),
            })
        }

        fn condition_id(&self) -> &str {
            "test::refuses"
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
    fn an_action_failure_leaves_earlier_effects_and_returns_err() -> TestResult {
        // The counterexample to "a tick is all-or-nothing", kept as a
        // regression because the README claimed it for long enough to be
        // retrieved as fact. Two actions on one chain: the first records an
        // effect, the second is refused. `tick` returns Err *and* the first
        // effect is live, so a caller reading Err as "nothing happened" retries
        // into a duplicate.
        //
        // The script holds still at 200 so the second tick has an unchanged
        // source, which is what makes the "not retried" half falsifiable: a
        // moving source would fire again for reasons unrelated to the failure.
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut bot = EcsBot::builder("partial")
            .observe(Script::new(vec![200, 200]))
            .on(|value: &u16| *value >= 200, Record(Rc::clone(&log)))
            .on(|value: &u16| *value >= 200, Fails)
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
            "the action before the failure ran and its effect is not rolled back"
        );

        // The unattempted work is not queued for the next tick: revisions are
        // committed in the observe phase, before any action runs, so an
        // unchanged source does not re-fire the chain. This is a limitation,
        // not a repair — it is what "partial run" means here.
        let revisions = bot.revisions();
        assert_eq!(
            bot.tick()?,
            0,
            "the chain must not re-fire on a still source"
        );
        assert_eq!(
            bot.revisions(),
            revisions,
            "the source did not move, so no new Revision was committed"
        );
        assert_eq!(
            log.borrow().len(),
            1,
            "the refused action's predecessor ran exactly once in total"
        );
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
            .on(Refuses, Count(Rc::clone(&after)))
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
}
