//! Building a bot: a spec's chains materialised, and the two builders.

// imports
use super::ActionId;
use super::ActionSpec;
use super::AttemptStatus;
use super::BotError;
use super::ChainEntry;
use super::ChainSpec;
use super::Deficit;
use super::Demand;
use super::DispatchError;
use super::DomainRegistry;
use super::DurabilityPromise;
use super::Duration;
use super::Evaluate;
use super::Execute;
use super::GrantSet;
use super::InputIdentity;
use super::JournalError;
use super::Need;
use super::Observe;
use super::Shortage;
use super::Witness;
use super::World;
use super::attempts::RetryPolicy;
use super::bot::EcsBot;
use super::components::AdmitScratch;
use super::components::Chains;
use super::components::ChangeTicks;
use super::components::Committed;
use super::components::EcsChain;
use super::components::Fired;
use super::components::Grants;
use super::components::Invalidated;
use super::components::MAX_POLL_DEADLINE;
use super::components::Moved;
use super::components::Observed;
use super::components::Order;
use super::components::Plan;
use super::components::Policy;
use super::components::PollBudget;
use super::components::PollScratch;
use super::components::Polled;
use super::components::Revision;
use super::components::SlotAdmission;
use super::components::SourceId;
use super::components::TickError;
use super::components::TickReport;
use super::effects::Effects;
use super::effects::RecordedOutcome;
use super::identity::EffectScope;
use super::identity::derive_action_id;
use super::ledger::Ledger;
use super::recover;
use super::systems::schedule;
use super::systems::validate;
use super::typed_entry;
use super::work::EffectEvidence;
use super::work::identify_output;
use super::work::same_output;

/// Resolve one spec chain into an erased chain, or record every need it has.
///
/// Returns `None` when the chain contributed any need, so the caller knows not
/// to build it; the needs are appended to `needs` in declaration order. The
/// walk is deliberately exhaustive: an unknown source stops the chain (nothing
/// downstream of a source this binary cannot build is knowable), but a known
/// source with an unknown action still has its condition and its capability
/// requirements checked, because those are facts about the document rather than
/// about the missing action.
pub(super) fn materialize_chain(
    chain_index: usize,
    chain: &ChainSpec,
    registry: &DomainRegistry,
    grants: &GrantSet,
    needs: &mut Vec<Need>,
) -> Option<EcsChain> {
    let before = needs.len();

    let Some(source_ctor) = registry.source(&chain.source) else {
        needs.push(Need::UnknownSource {
            chain: chain_index,
            domain: chain.source.clone(),
        });
        return None;
    };
    let source = match source_ctor(&chain.target) {
        Ok(source) => source,
        Err(cause) => {
            needs.push(Need::SourceTargetRejected {
                chain: chain_index,
                domain: chain.source.clone(),
                cause: cause.to_string(),
            });
            return None;
        }
    };
    for shortage in grants.uncovered(source.required_caps(), &Demand::new(chain.source.clone())) {
        needs.push(Need::MissingCapability {
            chain: chain_index,
            action: None,
            domain: chain.source.clone(),
            capability: shortage.required().clone(),
        });
    }

    let mut entries: Vec<ChainEntry> = Vec::with_capacity(chain.on.len());
    for (action_index, entry) in chain.on.iter().enumerate() {
        let condition_id: &String = &entry.0;
        let action_spec: &ActionSpec = &entry.1;
        let action = match registry.action(&action_spec.domain) {
            Some(ctor) => match ctor(&action_spec.target) {
                Ok(action) => Some(action),
                Err(cause) => {
                    needs.push(Need::ActionTargetRejected {
                        chain: chain_index,
                        action: action_index,
                        domain: action_spec.domain.clone(),
                        cause: cause.to_string(),
                    });
                    None
                }
            },
            None => {
                needs.push(Need::UnknownAction {
                    chain: chain_index,
                    action: action_index,
                    domain: action_spec.domain.clone(),
                });
                None
            }
        };
        if let Some(ref action) = action {
            for shortage in grants.uncovered(
                action.required_caps(),
                &Demand::new(action_spec.domain.clone()),
            ) {
                needs.push(Need::MissingCapability {
                    chain: chain_index,
                    action: Some(action_index),
                    domain: action_spec.domain.clone(),
                    capability: shortage.required().clone(),
                });
            }
        }
        let condition = match source.condition(condition_id) {
            Ok(condition) => Some(condition),
            Err(_) => {
                needs.push(Need::UnknownCondition {
                    chain: chain_index,
                    action: action_index,
                    condition: condition_id.clone(),
                });
                None
            }
        };
        if let (Some(action), Some(condition)) = (action, condition) {
            entries.push(ChainEntry::erased(
                condition.into_evaluate_any(),
                action.into_execute_any(),
            ));
        }
    }

    if needs.len() == before {
        Some(EcsChain::from_registry(source, entries))
    } else {
        None
    }
}

/// Builder for [`EcsBot`].
pub struct EcsBuilder {
    /// The bot name.
    pub(super) name: String,
    /// Chains finished so far.
    pub(super) chains: Vec<EcsChain>,
    /// The retry policy every entry will run under.
    pub(super) policy: RetryPolicy,
    /// The identity, the fence and the journal every dispatch goes through.
    ///
    /// `None` until [`Self::with_effects`] supplies one, and `build` refuses
    /// while it is. There is no default: a default would be a default
    /// *identity*, and an identity a caller did not choose is one they cannot
    /// recover against — which is precisely the state that makes a restart
    /// resend a merge. A default-off variant of the same bot would be worse
    /// still, because nothing would exercise it.
    pub(super) effects: Option<EffectScope>,
    /// How long one source poll may take before the tick stops waiting for it.
    ///
    /// A declared bound rather than a hidden one, so a caller who has been
    /// waiting on a wedged source can read the budget from the builder instead of
    /// discovering it as a mystery. Defaults to [`DEFAULT_POLL_DEADLINE`].
    pub(super) poll_deadline: Duration,
}

impl EcsBuilder {
    /// Set how long one source poll may take before the tick stops waiting for it.
    ///
    /// The bound that makes T06's slow-source half true. A poll that misses it is
    /// dropped mid-flight, commits nothing, leaves its chain's baseline and its
    /// forced-refresh mark standing, and is reported in
    /// [`TickReport::stalled`] — so the chains beside it commit and act in the
    /// same tick rather than waiting on a source that will not answer.
    ///
    /// Bounded at both ends, because both ends are the same fact: a zero budget
    /// cancels every poll before its first poll, and a budget past
    /// [`MAX_POLL_DEADLINE`] is a caller asking for a bot that can still hang.
    /// Both are [`BotError`]s at [`build`](Self::build), not panics and not
    /// silently clamped values, because a clamp is indistinguishable from the
    /// budget the caller asked for.
    ///
    /// ```
    /// # use std::time::Duration;
    /// # use lgwks_bot::broker::Broker;
    /// # use lgwks_bot::effect::{EnvironmentId, FlowRevision, RunId};
    /// # use lgwks_bot::journal::MemoryJournal;
    /// # use lgwks_bot::spec::{EffectIdentity, EffectScope};
    /// # use lgwks_bot::{Bot, BotError, GrantSet};
    /// # fn effects() -> Result<EffectScope, Box<dyn std::error::Error>> {
    /// #     let environment = EnvironmentId::from_hex("2122232425262728292a2b2c2d2e2f30")?;
    /// #     let mut broker = Broker::new();
    /// #     broker.register(environment)?;
    /// #     let identity = EffectIdentity::new(
    /// #         RunId::from_hex("0102030405060708090a0b0c0d0e0f10")?,
    /// #         environment,
    /// #         FlowRevision::from_tagged(
    /// #             "blake3_256",
    /// #             "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
    /// #         )?,
    /// #     );
    /// #     Ok(EffectScope::new(identity, broker, Box::new(MemoryJournal::new())))
    /// # }
    /// let scope = effects()?;
    /// let refused = Bot::builder("zero")
    ///     .with_poll_deadline(Duration::ZERO)
    ///     .with_effects(scope)
    ///     .build(&GrantSet::empty());
    /// assert!(matches!(
    ///     refused,
    ///     Err(BotError::PollDeadlineUnbounded { .. })
    /// ));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// Defaults to [`DEFAULT_POLL_DEADLINE`](super::components::DEFAULT_POLL_DEADLINE) for a caller that never calls this,
    /// which is what makes it a declared policy rather than a hidden constant.
    ///
    /// # Errors
    ///
    /// [`BotError::PollDeadlineUnbounded`] for [`Duration::ZERO`], and
    /// [`BotError::PollDeadlineExceeded`] for a budget above
    /// [`MAX_POLL_DEADLINE`]. Both surface from [`build`](Self::build), so this
    /// call itself cannot fail.
    #[must_use]
    pub fn with_poll_deadline(mut self, deadline: Duration) -> Self {
        self.poll_deadline = deadline;
        self
    }

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

    /// Bind the identity, the environment fence and the journal every dispatch
    /// goes through.
    ///
    /// Required: [`build`](Self::build) refuses without it, reporting
    /// [`BotError::IncompleteSpec`] with `field: "effects"`. That is the same
    /// idiom the name check uses, and it is deliberate rather than a typestate —
    /// a bot whose dispatch path is optional is a bot with two dispatch paths,
    /// and the estate does not keep two implementations of one job.
    #[must_use]
    pub fn with_effects(mut self, effects: EffectScope) -> Self {
        self.effects = Some(effects);
        self
    }

    /// Append a fully-erased chain, as a materializer builds one.
    ///
    /// The native path finishes a chain through
    /// [`ObserveBuilder::build`], which
    /// erases a concrete source and hands the result to
    /// [`EcsBot::assemble`]. This is the other seam into the same list: a chain
    /// whose source and entries are already erased, built from wire data by
    /// [`Bot::from_spec`]. Both reach `assemble`, so a
    /// materialized bot and a native one are the same value and share one
    /// execution path — there is no second interpreter here.
    ///
    /// Crate-internal: a caller outside cannot build an `EcsChain`, so this is
    /// not a second public builder.
    #[must_use]
    pub(crate) fn chain(mut self, chain: EcsChain) -> Self {
        self.chains.push(chain);
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
    #[must_use]
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
            effects: self.effects,
            poll_deadline: self.poll_deadline,
        }
    }

    /// Build with no observation chains: a bot that only serves direct
    /// `Query` and `Execute` calls.
    ///
    /// # Errors
    ///
    /// Whatever [`EcsBuilder::with_poll_deadline`] documents, plus the same
    /// admission refusals every other build of a bot raises.
    pub fn build(self, grants: &GrantSet) -> Result<EcsBot, BotError> {
        EcsBot::assemble(
            self.name,
            self.chains,
            grants,
            self.policy,
            self.effects,
            self.poll_deadline,
        )
    }
}

impl<S: Observe> EcsObserveBuilder<S> {
    /// Bind the effect path, as [`EcsBuilder::with_effects`] does.
    ///
    /// Offered here as well so the call can sit anywhere in the declaration
    /// chain — after the last `on` as naturally as before the first `observe` —
    /// rather than only at the one point the builder happens to hand it over.
    #[must_use]
    pub fn with_effects(mut self, effects: EffectScope) -> Self {
        self.effects = Some(effects);
        self
    }

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
    #[must_use]
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
    #[must_use]
    pub fn observe<U>(self, source: U) -> EcsObserveBuilder<U>
    where
        S: 'static,
        S::Output: PartialEq + InputIdentity + 'static,
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
            effects,
            poll_deadline,
        } = self;
        prior.push(EcsChain {
            source: Box::new(previous),
            same: same_output::<S>,
            identify: identify_output::<S>,
            witness: Witness::of::<S::Output>(),
            entries,
        });
        EcsObserveBuilder {
            name,
            prior,
            source,
            entries: Vec::new(),
            policy,
            effects,
            poll_deadline,
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
        S::Output: PartialEq + InputIdentity + 'static,
    {
        let Self {
            name,
            mut prior,
            source,
            entries,
            policy,
            effects,
            poll_deadline,
        } = self;
        prior.push(EcsChain {
            source: Box::new(source),
            same: same_output::<S>,
            identify: identify_output::<S>,
            witness: Witness::of::<S::Output>(),
            entries,
        });
        EcsBot::assemble(name, prior, grants, policy, effects, poll_deadline)
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
    /// Carried from [`EcsBuilder`] alongside `name`. Taken by the terminal
    /// `build`, which is the only place it is needed.
    effects: Option<EffectScope>,
    /// Chains finished by an earlier `observe` call, in order.
    prior: Vec<EcsChain>,
    /// The source being bound, still concrete.
    source: S,
    /// Tuples attached so far, each already erased for storage.
    entries: Vec<ChainEntry>,
    /// Carried from [`EcsBuilder`] alongside `policy`.
    policy: RetryPolicy,
    /// Carried from [`EcsBuilder`] alongside `policy`.
    poll_deadline: Duration,
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
        effects: Option<EffectScope>,
        poll_deadline: Duration,
    ) -> Result<Self, BotError> {
        if name.is_empty() {
            let refusal = Err(BotError::IncompleteSpec {
                field: "name",
                cause: String::from("the bot was given an empty name"),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "assemble: returning an error to the caller");
            return refusal;
        }
        // Refused before anything is built or polled, and for the same reason the
        // name is: a budget of zero cancels every poll before its first poll, so
        // a bot built with one observes nothing forever and reports every chain
        // as stalled. A refusal here names the number and the repair rather than
        // producing a bot whose every tick is a cancellation.
        if poll_deadline.is_zero() {
            let refusal = Err(BotError::PollDeadlineUnbounded {
                deadline: poll_deadline,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "assemble: returning an error to the caller");
            return refusal;
        }
        if poll_deadline > MAX_POLL_DEADLINE {
            let refusal = Err(BotError::PollDeadlineExceeded {
                deadline: poll_deadline,
                ceiling: MAX_POLL_DEADLINE,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "assemble: returning an error to the caller");
            return refusal;
        }
        // Refused before the capability gate, because a bot that cannot record
        // a dispatch is not a bot that is missing a capability — it is one that
        // was never given a dispatch path, and the repair is different.
        let Some(effects) = effects else {
            let refusal = Err(BotError::IncompleteSpec {
                field: "effects",
                cause: String::from(
                    "no effect scope was supplied, so a dispatch could not be recorded",
                ),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "assemble: returning an error to the caller");
            return refusal;
        };
        // What recovery found, folded in before the world exists. A key naming
        // an action this bot does not declare is a foreign journal — the
        // record belongs to a different bot or a different revision of this
        // one — and it is refused here rather than ignored, because ignoring it
        // means dispatching an action whose earlier attempt the journal says
        // may be live.
        let committed = effects
            .journal()
            .committed()
            .map_err(|cause| BotError::EffectRefused {
                cause: DispatchError::Journal(cause),
            })?;
        let tail = effects.journal().tail();
        // A committed count that cannot be a sequence number means the journal
        // holds more events than the sequence can name, which is exhaustion.
        let recovered_events = match u64::try_from(committed.len()) {
            Ok(count) => count,
            Err(_too_many_events) => {
                let refusal = Err(BotError::EffectRefused {
                    cause: DispatchError::Journal(JournalError::Exhausted),
                });
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "assemble: returning an error to the caller");
                return refusal;
            }
        };
        if recovered_events != tail.sequence() {
            let refusal = Err(BotError::EffectRefused {
                cause: DispatchError::Journal(JournalError::SnapshotStale {
                    recovered_events,
                    committed_events: tail.sequence(),
                }),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "assemble: returning an error to the caller");
            return refusal;
        }
        let recovered = recover(committed.iter());
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
            let refusal = Err(BotError::CapabilityDenied { deficit });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "assemble: returning an error to the caller");
            return refusal;
        }

        let mut world = World::new();
        world.insert_resource(Grants(grants.clone()));
        world.insert_resource(Fired::default());
        world.insert_resource(TickError::default());
        world.insert_resource(Policy(policy));
        world.insert_resource(PollBudget(poll_deadline));

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
        // The declared action identity of every entry, derived once here and
        // indexed exactly as the chains are. `derive_action_id` is the single
        // producer of an `ActionId` in this crate: a second derivation anywhere
        // else would be a second identity for the same entry.
        let actions: Vec<Vec<ActionId>> = chains
            .iter()
            .enumerate()
            .map(|(chain, held)| {
                held.entries
                    .iter()
                    .enumerate()
                    .map(|(entry, declared)| {
                        derive_action_id(&name, chain, entry, declared.action.domain_id())
                    })
                    .collect()
            })
            .collect();
        // The next admitted-input identity continues past whatever the journal
        // already holds, so a reconstructed bot cannot reissue a stamp an
        // earlier process bound a dispatch to (issue #101).
        let next_input = tail.sequence().saturating_add(1);
        let mut ledger = Ledger::new(actions, Effects::new(effects, tail), next_input);
        // Every attempt the journal names, folded in. Not only the uncertain
        // ones: an attempt whose outcome is recorded is a fact about this run
        // too, and the two things this loop derives from it are what let a
        // restarted run continue instead of stopping at the first entry the
        // previous process touched.
        //
        //   - the attempt the action must mint next, because the journal has
        //     already walked its ladder for the attempts below it and a
        //     repeated `IntentAdmitted` for one of them is an out-of-order
        //     append rather than a dispatch;
        //   - the attempts whose effect landed, which the walk retires rather
        //     than dispatching again.
        for attempt in recovered.attempts() {
            let key = attempt.key();
            let Some(id) = ledger.locate(key.action()) else {
                let refusal = Err(BotError::ActionNotDeclared {
                    action: key.action(),
                });
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "assemble: returning an error to the caller");
                return refusal;
            };
            // The journal's key must name this run, this flow, and this
            // environment. A journal that holds keys from any of the others is
            // the foreign-journal case one step finer: the action matches, the
            // ownership identity does not, and folding it in would dispatch an
            // action whose earlier attempt belongs to a different bot (issue
            // #101).
            let identity = ledger.effects.identity();
            if key.run() != identity.run()
                || key.flow() != identity.flow()
                || key.environment() != identity.environment()
            {
                let refusal = Err(BotError::ActionNotDeclared {
                    action: key.action(),
                });
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "assemble: returning an error to the caller");
                return refusal;
            }
            let _ = id;
            ledger
                .effects
                .note_journal_attempt(key.action(), key.attempt());
            match attempt.status() {
                // A dispatch with no outcome. Nothing established that the
                // bytes did not arrive, so the action is held until a caller
                // settles it.
                AttemptStatus::OutcomeUnknown => ledger.effects.unsettled.push(key),
                // A durable journal needs a positioned receipt before replay
                // treats a known outcome as settled. The record alone may have
                // landed below its advertised grade.
                // A failed predicate is an effect that landed, so it is never
                // resent; the failure is read from the status, not from here.
                AttemptStatus::Applied
                | AttemptStatus::Verified
                | AttemptStatus::VerificationFailed => {
                    if ledger.effects.scope.journal().durability() == DurabilityPromise::Ephemeral {
                        ledger.effects.note_applied(key);
                    } else if let Err(cause) = ledger
                        .effects
                        .confirm_recorded_outcome(key, EffectEvidence::Applied)
                    {
                        ledger.effects.recording.push(RecordedOutcome {
                            key,
                            evidence: EffectEvidence::Applied,
                            cause,
                        });
                    } else {
                        ledger.effects.note_applied(key);
                    }
                }
                // Nothing to hold. `Prepared` means the intent was admitted and
                // nothing was handed over, and `NotApplied` means exactly that
                // the bytes did not arrive; both leave the entry free to be
                // attempted again, under the next attempt identity.
                AttemptStatus::Prepared => {}
                AttemptStatus::NotApplied => {
                    if ledger.effects.scope.journal().durability() != DurabilityPromise::Ephemeral
                        && let Err(cause) = ledger
                            .effects
                            .confirm_recorded_outcome(key, EffectEvidence::NotApplied)
                    {
                        ledger.effects.recording.push(RecordedOutcome {
                            key,
                            evidence: EffectEvidence::NotApplied,
                            cause,
                        });
                    }
                }
            }
        }
        world.insert_non_send(Chains(chains));
        // Not `vec![None; count]`: `Box<dyn Any>` is not `Clone`, so the
        // repeat-form macro cannot build this.
        world.insert_non_send(Observed((0..count).map(|_| None).collect()));
        // The eligible work of the bot, one idle slot per chain. Non-send,
        // because each transition owns the observed payload it is bound to.
        world.insert_non_send(ledger);
        // The two staging resources the awaited phases hand to the schedule.
        // Inserted here rather than at first use so every phase can name them
        // unconditionally: a phase that found one missing would have to decide
        // what that means, and there is no answer that is better than "this
        // cannot happen".
        world.insert_non_send(Polled::default());
        // The change ticks, sized here so neither buffer is ever resized on the
        // observation path. `seen` starts empty of revisions: a chain with no
        // committed value has nothing a source's revision could equal, so its
        // first tick is a poll like any other.
        world.insert_non_send(ChangeTicks {
            seen: vec![None; count],
            polled: vec![None; count],
        });
        // The change flags and the plan's buffers are per-tick scratch, inserted
        // here so the first tick allocates them once and every later tick reuses
        // the same allocations. Nothing else writes them.
        world.insert_non_send(AdmitScratch::default());
        world.insert_non_send(Moved::default());
        world.insert_non_send(Plan::default());
        // The observation phase's own scratch, empty on purpose: the first tick
        // sizes these to the world and every tick after reuses that capacity,
        // which is the whole of how a tick that changed nothing came to cost no
        // allocations at all.
        world.insert_non_send(PollScratch::default());
        // One slot per chain, sized here so the observation phase never resizes
        // it: a `resize` per tick would be an allocation the first time and a
        // capacity check every tick after, for a buffer whose length is the
        // chain count and is already known.
        world.insert_non_send(Invalidated {
            reasons: vec![None; count],
        });
        // Same shape and the same reason: the supersession check asks one
        // question per chain, and a walk of the transitions to answer it would
        // be a per-chain scan on every tick for a question that has one.
        world.insert_non_send(Committed {
            slots: vec![SlotAdmission::default(); count],
        });
        // A resource rather than a field on the bot, so the observation phase
        // and the schedule step — two functions that can only reach the world —
        // write one record and `tick_report` reads the same one they wrote.
        world.insert_resource(TickReport::default());

        let mut schedule = schedule();
        validate(&mut schedule, &mut world)?;

        Ok(Self {
            name,
            world,
            schedule,
            poll_deadline,
        })
    }
}

// ── `Debug` for the assembled bot and its two builders ─────────────────────
//
// Written as impls rather than derives because a derive line added above a type
// would renumber the citations the module's own docs make by line number, and
// because `EcsObserveBuilder<S>` and `EcsBuilder` hold `dyn` seams that no
// derive can cross. Each rendering reports the shape a caller can act on — the
// name, how many chains or entries are staged, the retry policy — and stops
// short of the engine's own state through `finish_non_exhaustive`, which marks
// the omission instead of hiding it.

impl core::fmt::Debug for EcsBot {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EcsBot")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl core::fmt::Debug for EcsBuilder {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EcsBuilder")
            .field("name", &self.name)
            .field("chains", &self.chains.len())
            .field("policy", &self.policy)
            .finish()
    }
}

impl<S> core::fmt::Debug for EcsObserveBuilder<S> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EcsObserveBuilder")
            .field("name", &self.name)
            .field("prior", &self.prior.len())
            .field("entries", &self.entries.len())
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}
