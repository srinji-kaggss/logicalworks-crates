//! The ECS bot: construction, introspection and the tick entry points.

use std::time::Duration;

use lgwks_deps::bevy_ecs::{prelude::World, schedule::Schedule};

use crate::error::BotError;
use crate::gate::{GrantReplacement, GrantSet};
use crate::journal::EffectJournal;
use crate::registry::DomainRegistry;
use crate::spec::{Admission, BotSpec, Need, NeedSet};

#[cfg(feature = "profile")]
use super::profile::{self, Charge, TickProfile, TickStage};

use super::builder::EcsBuilder;
use super::builder::materialize_chain;
use super::entry::RetrySchedule;
use super::fold::put_plan_buffers;
use super::fold::settle_quiet_tick;
use super::ledger::Ledger;
use super::resources::ChangeTicks;
use super::resources::EcsChain;
use super::resources::Fired;
use super::resources::Order;
use super::resources::Plan;
use super::resources::Policy;
use super::resources::PollScratch;
use super::resources::Polled;
use super::resources::Revision;
use super::resources::SourceId;
use super::resources::TickError;
use super::resources::TickReport;
use super::scope::EffectScope;
use super::watchdog::DEFAULT_POLL_DEADLINE;
#[cfg(doc)]
use super::work::TransitionHold;
#[cfg(doc)]
use lgwks_deps::bevy_ecs::schedule::IntoScheduleConfigs;

/// A bot executing on a `bevy_ecs` world.
///
/// Stepped by hand: one tick is one [`EcsBot::tick_async`], which is one
/// `Schedule::run` between the awaited observe and act phases. No framework owns
/// a loop, and no `App::run` is ever called.
pub struct EcsBot {
    /// The name the spec declared.
    pub(super) name: String,
    /// The world the schedule runs against.
    pub(super) world: World,
    /// The validated schedule.
    pub(super) schedule: Schedule,
    /// How long one source poll may take before the tick stops waiting for it.
    ///
    /// A field rather than a resource because nothing in the schedule reads it:
    /// it bounds the observation phase alone, and a resource would put a second
    /// writer next to the phase that owns it. The declared default is
    /// [`DEFAULT_POLL_DEADLINE`] and the builder's override is the only other
    /// value a bot can carry.
    pub(super) poll_deadline: Duration,
}

impl EcsBot {
    /// Start building a named bot on this substrate.
    #[must_use]
    pub fn builder(name: impl Into<String>) -> EcsBuilder {
        EcsBuilder {
            name: name.into(),
            chains: Vec::new(),
            policy: RetrySchedule::declared_default(),
            effects: None,
            poll_deadline: DEFAULT_POLL_DEADLINE,
        }
    }

    /// Materialize a runnable bot from a serializable spec and a registry.
    ///
    /// This is `from_spec`, and it is deliberately the *same* bot the builder
    /// chain produces: the spec's sources and actions are built through the
    /// registry, erased into chains, and handed to the same `assemble` step
    /// that [`ObserveBuilder::build`](crate::spec::ObserveBuilder::build)
    /// reaches with a natively declared chain. There is no second interpreter
    /// and no second execution path, so a materialized bot and a native one
    /// built from the same domains produce the same operation trace (T25).
    ///
    /// # Authority
    ///
    /// A spec cannot grant itself anything. Capabilities come only from the
    /// `grants` argument: the registry supplies constructors, never authority,
    /// and every erased verb still checks its own caps at the call site against
    /// the proof the bot mints from this set. This is what makes it safe to
    /// accept a spec from wire data at all — naming a domain buys no reach.
    ///
    /// # All-or-nothing admission
    ///
    /// Nothing runs and nothing is half-built. Every presently knowable unmet
    /// need is collected in one [`NeedSet`] — an unknown source or action
    /// domain, a constructor that rejects its target, a missing capability, an
    /// unknown condition — each attributed to the chain (and action) that needs
    /// it, and the whole materialization is refused with [`Admission::Needs`].
    /// A malformed document or registry is refused with [`Admission::Refused`]
    /// before any need is computed. Because the refusal happens before the
    /// builder runs, no source is polled and no action executes.
    ///
    /// # Errors
    ///
    /// [`Admission::Refused`] for a registry with a duplicate identifier, an
    /// unsupported [`version`](BotSpec::version), an empty chain list, or the
    /// builder's own admission (which an all-admitted spec does not reach);
    /// [`Admission::Needs`] for the complete report of unmet needs.
    ///
    /// # Example
    ///
    /// `one_chain_traces_match_t25` (`tests/it/spec_materialize.rs`) runs it: a source and an
    /// action domain are declared with `domains!`, a JSON spec naming both is parsed with
    /// `BotSpec::from_json`, `Bot::from_spec` admits it against an empty grant set with an in-memory
    /// effect scope, the one chain fires once on the first tick, and its trace equals a native bot's.
    pub fn from_spec(
        spec: &BotSpec,
        registry: &DomainRegistry,
        grants: &GrantSet,
        effects: EffectScope,
    ) -> Result<Self, Admission> {
        // A registry that declares one identifier twice cannot be dispatched
        // against: lookup would depend on declaration order. Refused before any
        // chain is walked, because the repair is the registry's, not the
        // document's.
        registry.validate().map_err(Admission::Refused)?;
        if spec.version != BotSpec::CURRENT_VERSION {
            let refusal = Err(Admission::Refused(BotError::UnsupportedSpecVersion {
                found: spec.version,
                supported: BotSpec::CURRENT_VERSION,
            }));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "from_spec: returning an error to the caller");
            return refusal;
        }
        // A materialized bot with nothing to observe is almost always a
        // truncated document rather than an intentional one; the builder
        // tolerates it, the materializer does not.
        if spec.chains.is_empty() {
            let refusal = Err(Admission::Refused(BotError::IncompleteSpec {
                field: "chains",
                cause: String::from("the document declares no observation chains at all"),
            }));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "from_spec: returning an error to the caller");
            return refusal;
        }

        // One pass over every chain, collecting *every* need rather than
        // stopping at the first. Each chain that fully resolves contributes its
        // erased form; a chain with any need contributes to the report and no
        // chain at all. The two lists cannot disagree: `needs` non-empty means
        // the build below is never reached.
        let mut needs: Vec<Need> = Vec::new();
        let mut chains: Vec<EcsChain> = Vec::with_capacity(spec.chains.len());
        for (chain_index, chain) in spec.chains.iter().enumerate() {
            if let Some(built) = materialize_chain(chain_index, chain, registry, grants, &mut needs)
            {
                chains.push(built);
            }
        }
        if !needs.is_empty() {
            let refusal = Err(Admission::Needs(NeedSet::new(needs)));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "from_spec: returning an error to the caller");
            return refusal;
        }

        // The same `assemble` a native bot reaches, through the same builder.
        let mut builder = Self::builder(spec.name.clone());
        for chain in chains {
            builder = builder.chain(chain);
        }
        builder
            .with_effects(effects)
            .build(grants)
            .map_err(Admission::Refused)
    }

    /// Take the journal back out of the bot.
    ///
    /// `None` when the ledger is not in the world, which is the state after a
    /// previous call already took it. What a host calls on its way out, so the
    /// record outlives the process that wrote it — and what a restart test
    /// calls to hand the same journal to a second bot.
    #[must_use]
    pub fn into_journal(mut self) -> Option<Box<dyn EffectJournal>> {
        self.world
            .remove_non_send::<Ledger>()
            .map(Ledger::into_journal)
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
    ///    walking the ledger's outstanding work rather than the change set. A tick
    ///    that polled nothing, holds no transition and is not parked skips the run
    ///    and records the same two facts, nothing moved and an empty plan;
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

        // The staging buffers are moved out, refilled and put back, so the
        // observation phase reuses one allocation across every tick rather than
        // building a fresh vector per tick. They are taken rather than borrowed
        // because `poll_sources` borrows the world immutably across its awaits
        // and a mutable borrow of a resource cannot be held across them.
        let mut polled = std::mem::take(&mut self.world.non_send_mut::<Polled>().0);
        polled.clear();
        // The revisions the sources reported, taken out with the rest of the
        // observation phase's scratch: this method holds `&self` across every
        // poll, so it cannot write the world it is reading.
        let mut revisions = std::mem::take(&mut self.world.non_send_mut::<ChangeTicks>().polled);
        // Taken rather than borrowed for the same reason `revisions` is, and
        // because the observation phase is the tick's only writer of these
        // buffers. It goes back with its capacity, which is the entire point:
        // a quiet tick fills the buffers, publishes them, and hands them to the
        // next tick instead of asking the allocator for them again.
        let mut scratch = {
            let mut guard = self.world.non_send_mut::<PollScratch>();
            std::mem::take(&mut *guard)
        };
        #[cfg(feature = "profile")]
        let poll_charge = Charge::new(TickStage::Poll);
        let watchdogs = self
            .poll_sources(&mut polled, &mut revisions, &mut scratch)
            .await;
        #[cfg(feature = "profile")]
        drop(poll_charge);
        self.world.non_send_mut::<Polled>().0 = polled;
        // Handed back before the schedule runs, because `observe_fold` commits
        // each chain's revision from it and that commit is the whole rule.
        self.world.non_send_mut::<ChangeTicks>().polled = revisions;

        // Published before the schedule runs, so the report describes this tick's
        // observation phase even when the schedule step stops on a failure and
        // the effects phase never runs. A caller reading the report after a
        // failed tick is exactly the caller who needs to know a source had
        // already declared its cache unsound when it failed.
        self.record_invalidations(&scratch.declared);
        self.publish_refreshes();
        self.publish_stalls(&scratch.stalled, watchdogs);
        *self.world.non_send_mut::<PollScratch>() = scratch;

        #[cfg(feature = "profile")]
        let schedule_charge = Charge::new(TickStage::Schedule);
        // A tick that polled nothing and holds nothing is settled in place: the
        // schedule's two systems would write "nothing moved" and an empty plan.
        if !settle_quiet_tick(&mut self.world) {
            self.schedule.run(&mut self.world);
        }
        #[cfg(feature = "profile")]
        drop(schedule_charge);

        // Taken rather than borrowed: the effects are awaited below, and a
        // borrow of the plan would outlive the schedule that wrote it. An empty
        // plan is left behind, so a tick dropped here stages nothing.
        let plan = {
            let mut guard = self.world.non_send_mut::<Plan>();
            std::mem::take(&mut *guard)
        };
        let Plan {
            mut steps,
            mut failures,
        } = plan;
        let retry = self.world.resource::<Policy>().reading();
        let budget = retry.budget();
        #[cfg(feature = "profile")]
        let act_charge = Charge::new(TickStage::Act);
        let (fired, failure) = self.run_steps(&steps, &mut failures, &retry).await;
        #[cfg(feature = "profile")]
        drop(act_charge);

        // Handed back with their capacity, not dropped. The plan is taken out of
        // the world to run because the driver needs it mutably while it awaits,
        // and a buffer that is taken out and then dropped is a buffer the next
        // tick has to allocate again — which is exactly what made a tick cost a
        // heap allocation per chain before it ran a single effect.
        steps.clear();
        failures.clear();
        put_plan_buffers(&mut self.world, steps, failures);

        self.world.resource_mut::<Fired>().0 = fired;
        self.world.resource_mut::<TickReport>().fired = fired;
        if let Some(error) = failure {
            self.world.resource_mut::<TickError>().0 = Some(error);
        }

        if let Some(error) = self.world.resource_mut::<TickError>().0.take() {
            let refusal = Err(error);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "tick_async: returning an error to the caller");
            return refusal;
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
                work: Box::new(work),
                outstanding: ledger.unresolved_count(budget),
            }),
            None => Ok(self.world.resource::<Fired>().0),
        }
    }

    /// One tick, with each stage of it measured.
    ///
    /// The same [`tick_async`](Self::tick_async) on the same four phases, with the
    /// per-stage instrument armed around it; the `Result` is the identical value
    /// that call would have returned. Nothing here is a reimplementation of the
    /// tick, so a profile is a decomposition of the real path rather than of a
    /// model of it.
    ///
    /// # The instrument is not free, and this is how its cost is known
    ///
    /// Measuring six stages costs seven clock reads. A caller that wants both a
    /// wall time and a profile therefore times the *same* workload through both
    /// doors and takes the difference as the instrument's cost on that tick —
    /// which is what `bench/` does, and the only way the two figures can be read
    /// together. The **shares** the profile reports are the claim it supports;
    /// the absolute per-stage times carry the instrument.
    ///
    /// Stages are a partition of the tick and nest in one direction: the
    /// schedule step wraps the stages inside it, so [`TickStage::Schedule`] is
    /// the *residual* — the dispatch of the two systems plus the commit
    /// bookkeeping inside `observe_fold` that nothing else claims.
    #[cfg(feature = "profile")]
    pub async fn tick_profiled(&mut self) -> (Result<usize, BotError>, TickProfile) {
        profile::begin();
        let outcome = self.tick_async().await;
        (outcome, profile::end())
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
        if crate::rt::runtime::inside_runtime() {
            let refusal = Err(BotError::TickInsideRuntime);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "tick: returning an error to the caller");
            return refusal;
        }
        lgwks_std::task::block_on(self.tick_async())
    }

    /// Reset the per-tick scratch, so a tick starts from the same state however
    /// the previous one ended.
    ///
    /// The staged observations and the effect program are not cleared here:
    /// both are overwritten wholesale by the phase that produces them, which is
    /// what makes a *dropped* tick safe as well as a completed one.
    pub(super) fn begin_tick(&mut self) {
        self.world.resource_mut::<Fired>().0 = 0;
        self.world.resource_mut::<TickError>().0 = None;
        // One reading of the bot clock for the whole tick, so every backoff a
        // tick judges is judged at the same instant.
        self.world.resource_mut::<Policy>().begin_tick();
        // The previous tick's report is history, not this tick's. Overwritten
        // rather than appended so a caller reading it before the next tick sees
        // the tick it just ran and not a union of every tick the bot has ever
        // run — which would answer "was a refresh ever forced", a different and
        // much less useful question.
        *self.world.resource_mut::<TickReport>() = TickReport::default();
    }

    /// What the most recent tick observed about its own sources.
    ///
    /// The companion to [`Self::tick`]'s return value rather than a second
    /// channel into the same fact: the count says what fired, this says what the
    /// bot had to re-read to decide, and a bot that has gone quiet is exactly
    /// where only the first of those is uninformative.
    ///
    /// Reports the last tick that ran, whatever its result. A tick that returned
    /// `Err` still observed, and the caller triaging the error needs to know
    /// whether a declared cache failure is behind it. Reading this before the
    /// first tick returns the empty report, which says no source was forced —
    /// true, and true for a tick that never happened.
    #[must_use]
    pub fn tick_report(&self) -> TickReport {
        self.world.resource::<TickReport>().clone()
    }

    /// Swap the grant set this bot mints proofs from, reporting what changed.
    ///
    /// The live-revocation operation (#716): the framework mints a fresh proof
    /// per call from the set it retained at build, so replacing that set is
    /// what narrows a running bot. Atomic with respect to a tick by
    /// construction — both take `&mut self`, so no tick observes a half-swapped
    /// set, and ticks are stepped by hand rather than on a background thread.
    /// Returns what the swap withdrew and added, each sorted by dotted name,
    /// so an operator can audit the narrowing.
    ///
    /// What it does not do: proofs already minted (`Auth` in hand) keep the
    /// caps they were minted with until their lifetimes lapse — a credential
    /// is judged by what it carries, not by the set that minted it — and
    /// replacing the *caller's* set still changes nothing for a built bot;
    /// only this call narrows it.
    ///
    /// It lives at the end of this `impl` so the line numbers the guides cite
    /// above it never move for it.
    #[must_use]
    pub fn replace_grants(&mut self, next: &GrantSet) -> GrantReplacement {
        let held = self.world.resource::<super::resources::Grants>();
        let report = held.0.difference(next);
        self.world
            .insert_resource(super::resources::Grants(next.clone()));
        report
    }
}
