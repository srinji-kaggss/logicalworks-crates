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
//! | `Execute::execute_action` | the `fire` system |
//! | `Bot::tick()` | `EcsBot::tick()`, one `Schedule::run` |
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

/// Observe: poll every source, then bump `Revision` for the ones that moved.
///
/// Exclusive because the verbs are non-`Send`. All sources are polled before
/// any `Revision` is written, matching `Bot::tick`'s documented error ordering:
/// the first error in declaration order is returned, and no source is left
/// half-updated.
fn observe(world: &mut World) {
    if parked(world) {
        return;
    }

    // Bounded waves, joined concurrently: a source poll may occupy one
    // `spawn_blocking` thread, so polling them one at a time would make a tick
    // as slow as the sum of its sources rather than as slow as its slowest. The
    // wave cap is what keeps that from becoming unbounded blocking-thread
    // fan-out. Determinism is unaffected: the results are collected in
    // declaration order, and actions still run sequentially.
    let polled: Vec<Result<Box<dyn Any>, BotError>> = {
        let chains = world.non_send::<Chains>();
        let grants = world.resource::<Grants>();
        let mut polled = Vec::with_capacity(chains.0.len());
        for wave in chains.0.chunks(MAX_IN_FLIGHT_POLLS) {
            let batch = lgwks_std::task::join_all_boxed(
                wave.iter().map(|chain| chain.source.poll_any(&grants.0)),
            );
            polled.extend(lgwks_std::task::block_on(batch));
        }
        polled
    };

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

/// Execute: run the entries of every source whose value moved this tick.
///
/// The capability check is not repeated here: `poll_any` and `run_any` each
/// mint a fresh `Auth` from the retained `GrantSet`, which is where the proof
/// belongs. A grant revoked after build therefore cannot fire, exactly as on
/// `Bot`.
fn fire(world: &mut World) {
    if parked(world) {
        return;
    }

    let moved: Vec<usize> = {
        let mut query = world.query_filtered::<&SourceId, Changed<Revision>>();
        query.iter(world).map(|id| id.chain).collect()
    };

    let mut fired: usize = 0;
    let mut failure: Option<BotError> = None;
    {
        let chains = world.non_send::<Chains>();
        let observed = world.non_send::<Observed>();
        let grants = world.resource::<Grants>();

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
            for entry in &chain.entries {
                match entry.condition.check_any(value.as_ref()) {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(error) => {
                        failure = Some(error);
                        break 'chains;
                    }
                }
                match lgwks_std::task::block_on(entry.action.run_any(&grants.0, value.as_ref())) {
                    Ok(_) => fired = fired.saturating_add(1),
                    Err(error) => {
                        failure = Some(error);
                        break 'chains;
                    }
                }
            }
        }
    }

    world.resource_mut::<Fired>().0 = fired;
    if let Some(error) = failure {
        world.resource_mut::<TickError>().0 = Some(error);
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
    // `.chain()` is the declared order: poll, then fire.
    schedule.add_systems((observe, fire).chain());
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
/// Stepped by hand: [`EcsBot::tick`] is one `Schedule::run`. No framework owns
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

    /// Run one tick. Returns the number of actions fired.
    ///
    /// Synchronous: the systems drive the non-`Send` verb futures on this
    /// thread, so there is nothing for a caller to await.
    pub fn tick(&mut self) -> Result<usize, BotError> {
        self.world.resource_mut::<TickError>().0 = None;
        self.world.resource_mut::<Fired>().0 = 0;
        self.schedule.run(&mut self.world);
        match self.world.resource_mut::<TickError>().0.take() {
            Some(error) => Err(error),
            None => Ok(self.world.resource::<Fired>().0),
        }
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
            .on(|value: &u16| *value >= 500, Count(counter))
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
        ambiguous.add_systems((observe, fire));

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
        ordered.add_systems((observe, fire).chain());
        validate(&mut ordered, &mut world)?;
        Ok(())
    }
}
