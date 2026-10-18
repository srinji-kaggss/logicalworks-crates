//! The session runner: the cursor over a validated flow. Split from `session.rs` (#526).

use super::{
    BTreeMap, BotError, DecisionReceipt, DegradedReason, FlowSpec, Interpolate, Journal,
    MemoryJournal, NodeId, NodeKind, Predicate, Question, RECEIPT_VERSION, RecordedDecision,
    Resolution, Resolver, ResourceAxis, ResourceLimits, SessionId, TemplateInterpolator, Terminal,
    TranscriptEntry, VarScope, Verdict, options_revision, prompt_bytes, revision_of,
    validate_flow_within,
};

/// Synchronous cursor over one validated flow and its conversation state.
#[non_exhaustive]
pub struct Session {
    /// Stable session identity.
    pub(super) id: SessionId,
    /// Validated flow document owned by this session.
    pub(super) flow: FlowSpec,
    /// Digest of the flow document, computed once at construction.
    flow_revision: String,
    /// Current node, or `None` after terminal completion.
    pub(super) current: Option<NodeId>,
    /// Typed variable scope.
    pub(super) scope: VarScope,
    /// Nodes executed in order, including repeated cycle visits.
    pub(super) visited: Vec<NodeId>,
    /// Transcript mirrored to the journal seam.
    pub(super) transcript: Vec<TranscriptEntry>,
    /// Pluggable free-text resolver.
    pub(super) resolver: Box<dyn Resolver>,
    /// Pluggable record sink.
    pub(super) journal: Box<dyn Journal>,
    /// Decision receipts in insertion order, with how each was accepted.
    decisions: Vec<RecordedDecision>,
    /// Terminal outcome, if the cursor has stopped.
    pub(super) terminal: Option<Terminal>,
    /// Number of charged execution and answer steps.
    pub(super) steps: usize,
    /// Last accepted answer, used by [`NodeKind::Route`].
    last_utterance: Option<String>,
    /// Effective byte ceilings for this session.
    pub(super) limits: ResourceLimits,
    /// Bytes retained so far: every transcript record and visited node id.
    pub(super) retained: usize,
}

/// What one answer is being judged against: the ask it answers, the resolver's
/// verdict on it, and the person's own words.
///
/// Gathered so the helpers that apply a verdict take one borrowed value rather
/// than six positional arguments that can be passed in the wrong order.
struct Turn<'a> {
    /// The ask node being answered.
    pub(super) node_id: &'a str,
    /// The variable a resolved answer binds.
    pub(super) var: &'a str,
    /// The options the ask offered.
    pub(super) options: &'a [String],
    /// Where each option leads.
    pub(super) routes: &'a BTreeMap<String, NodeId>,
    /// The resolver's verdict.
    pub(super) verdict: &'a Verdict,
    /// What the person said.
    pub(super) utterance: &'a str,
}

impl Session {
    /// Start a session with the default language resolver and memory journal.
    pub fn new(id: impl Into<SessionId>, flow: FlowSpec) -> Result<Self, BotError> {
        Self::with_components(
            id,
            flow,
            crate::language::LanguageResolver::new(),
            MemoryJournal::new(),
        )
    }

    /// Start a session with a custom resolver and the default memory journal.
    pub fn with_resolver<R>(
        id: impl Into<SessionId>,
        flow: FlowSpec,
        resolver: R,
    ) -> Result<Self, BotError>
    where
        R: Resolver + 'static,
    {
        Self::with_components(id, flow, resolver, MemoryJournal::new())
    }

    /// Start a session with a custom resolver, a custom journal, and explicit
    /// byte ceilings.
    ///
    /// The operator's knob. `limits` may be tighter than
    /// [`ResourceLimits::shipped`] and may not be looser; a value above the
    /// shipped ceiling is refused with [`BotError::ResourceLimitAboveCeiling`]
    /// before the flow is even examined. What the session ends up enforcing is
    /// the narrower of `limits` and whatever the document asks for in its
    /// [`FlowBounds`], per axis, so neither side can widen what the other
    /// narrowed.
    ///
    /// # Errors
    ///
    /// Returns [`BotError::ResourceLimitAboveCeiling`] for a ceiling above the
    /// operator's, and every error [`Self::with_components`] returns.
    pub fn with_components_and_limits<R, J>(
        id: impl Into<SessionId>,
        flow: FlowSpec,
        resolver: R,
        journal: J,
        limits: ResourceLimits,
    ) -> Result<Self, BotError>
    where
        R: Resolver + 'static,
        J: Journal + 'static,
    {
        // Both sets are checked against the ceiling and then narrowed against
        // each other, in that order: a request above the ceiling is a refusal
        // wherever it comes from, and two in-range requests combine by taking
        // the smaller. Doing it the other way round would let a document's
        // in-range tightening hide an operator's out-of-range one.
        let operator = limits.within_ceiling()?;
        let requested = flow.bounds.effective_resources().within_ceiling()?;
        let effective = operator.narrowed(requested);
        // Re-validated here rather than trusted from construction, because the
        // public `json` façade parses a `FlowSpec` without calling
        // `FlowSpec::from_json`, and because this is the point at which the
        // effective ceilings — the ones this session will actually enforce —
        // become known to validation.
        validate_flow_within(&flow, effective)?;
        let scope = VarScope::new(flow.vars.clone())?;
        let flow_revision = revision_of(&flow)?;
        let mut session = Self {
            id: id.into(),
            current: Some(flow.entry.clone()),
            flow,
            flow_revision,
            scope,
            visited: Vec::new(),
            transcript: Vec::new(),
            resolver: Box::new(resolver),
            journal: Box::new(journal),
            decisions: Vec::new(),
            terminal: None,
            steps: 0,
            last_utterance: None,
            limits: effective,
            retained: 0,
        };
        session.drive()?;
        Ok(session)
    }

    /// Start a session with explicit byte ceilings and the default seams.
    ///
    /// # Errors
    ///
    /// As [`Self::with_components_and_limits`].
    pub fn with_limits(
        id: impl Into<SessionId>,
        flow: FlowSpec,
        limits: ResourceLimits,
    ) -> Result<Self, BotError> {
        Self::with_components_and_limits(
            id,
            flow,
            crate::language::LanguageResolver::new(),
            MemoryJournal::new(),
            limits,
        )
    }

    /// Start a session with both replacement seams installed.
    pub fn with_components<R, J>(
        id: impl Into<SessionId>,
        flow: FlowSpec,
        resolver: R,
        journal: J,
    ) -> Result<Self, BotError>
    where
        R: Resolver + 'static,
        J: Journal + 'static,
    {
        Self::with_components_and_limits(id, flow, resolver, journal, ResourceLimits::shipped())
    }

    /// Return the session identity.
    #[must_use]
    pub fn id(&self) -> &SessionId {
        &self.id
    }

    /// Return the current node id, or `None` once terminal.
    #[must_use]
    pub fn current(&self) -> Option<&str> {
        self.current.as_deref()
    }

    /// Return the current node kind, or `None` once terminal.
    #[must_use]
    pub fn current_node(&self) -> Option<&NodeKind> {
        self.current.as_deref().and_then(|id| self.flow.node(id))
    }

    /// Return the typed variable scope.
    #[must_use]
    pub fn scope(&self) -> &VarScope {
        &self.scope
    }

    /// Return the visited node path.
    #[must_use]
    pub fn visited(&self) -> &[NodeId] {
        &self.visited
    }

    /// Return transcript records in insertion order.
    #[must_use]
    pub fn transcript(&self) -> &[TranscriptEntry] {
        &self.transcript
    }

    /// Return the decision receipts written so far, in insertion order.
    ///
    /// This is the audit record. [`Self::transcript`] is a rendering of the
    /// same events for a person to read; these are what a consumer reconciles
    /// against, and they carry what a rendering cannot: the policy in force,
    /// the model if one was consulted, the candidate set, and where each
    /// receipt was accepted.
    #[must_use]
    pub fn decisions(&self) -> &[RecordedDecision] {
        &self.decisions
    }

    /// Return the digest of the flow document this session is running.
    #[must_use]
    pub fn flow_revision(&self) -> &str {
        &self.flow_revision
    }

    /// Return the terminal outcome, if reached.
    #[must_use]
    pub fn terminal(&self) -> Option<&Terminal> {
        self.terminal.as_ref()
    }

    /// Return the number of charged steps.
    #[must_use]
    pub fn steps(&self) -> usize {
        self.steps
    }

    /// Return the byte ceilings this session enforces.
    ///
    /// The effective set: the operator's shipped ceiling, narrowed by whatever
    /// this session's caller and this flow's [`FlowBounds`] asked for. Reported
    /// because a bound a caller cannot read back is a bound they have to
    /// discover by running into it.
    #[must_use]
    pub const fn limits(&self) -> ResourceLimits {
        self.limits
    }

    /// Return the bytes this session currently retains.
    ///
    /// The sum of every transcript record's payload, path node, and role, plus
    /// every visited node id. Each is an owned allocation the session holds, so
    /// the total is what a caller needs to bound their own memory rather than
    /// an estimate of it.
    #[must_use]
    pub const fn retained_bytes(&self) -> usize {
        self.retained
    }

    /// Receipts the verdict, then records the person's answer.
    ///
    /// Every re-ask arm begins with this pair, and the order is the contract: the
    /// receipt is the durable fact that a verdict was reached, and the transcript
    /// line is what a reader sees later. Recording first would put an utterance
    /// in the transcript for a verdict that was never durably reached.
    fn receipt_and_record(&mut self, turn: &Turn<'_>) -> Result<(), BotError> {
        self.write_receipt(turn.node_id, turn.options, turn.verdict, None, None)?;
        self.record(turn.node_id, "user", turn.utterance)
    }

    /// Applies a resolved verdict: receipt, bind, record, then continue.
    ///
    /// A helper rather than the arm inline: as one block the arm held the
    /// option lookup, the route lookup, the receipt, the binding, the
    /// transcript line and the transition -- six fallible steps in one
    /// statement. The order is the contract `answer` documents above and is
    /// enforced here: a journal that refuses the receipt aborts the answer with
    /// the session untouched.
    fn accept_resolved(&mut self, turn: &Turn<'_>, index: usize) -> Result<(), BotError> {
        let Turn {
            node_id,
            var,
            options,
            routes,
            verdict,
            utterance,
        } = *turn;
        let Some(option) = options.get(index) else {
            let refusal = Err(BotError::ResolverReturnedInvalidOption {
                node: node_id.to_owned(),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "accept_resolved: returning an error to the caller");
            return refusal;
        };
        let Some(target) = routes.get(option).cloned() else {
            let refusal = Err(BotError::MissingAskRoute {
                node: node_id.to_owned(),
                option: option.clone(),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "accept_resolved: returning an error to the caller");
            return refusal;
        };
        self.write_receipt(
            node_id,
            options,
            verdict,
            Some(option.clone()),
            Some(&target),
        )?;
        self.scope
            .set_from_answer_within(var, option, self.limits.get(ResourceAxis::Value))?;
        self.last_utterance = Some(utterance.to_owned());
        self.record(node_id, "user", utterance)?;
        self.current = Some(target);
        self.drive()
    }

    /// Applies one resolver verdict to the session.
    ///
    /// Every arm is its own refusal or its own transcript, and reading them side
    /// by side is what makes the differences between them -- a narrowed re-ask,
    /// a withdrawn binding, a degraded cause under its own role -- visible.
    /// Inside `answer` they read as one statement.
    /// Receipts a verdict, records the answer, then re-asks over `ask`.
    ///
    /// The shape every non-resolved verdict shares. `ask` is the option list the
    /// prompt should carry: the full list for an absent, stale or degraded answer,
    /// and a strictly smaller one for an ambiguous tie. That difference is the
    /// whole reason an ambiguous answer terminates rather than looping, so it is
    /// a parameter here instead of a branch repeated at each call site.
    fn reask(&mut self, turn: &Turn<'_>, ask: &[String]) -> Result<(), BotError> {
        self.receipt_and_record(turn)?;
        self.record_prompt(turn.node_id, ask)
    }

    /// Applies one resolver verdict to the session.
    ///
    /// Each arm is its own refusal or its own transcript, and reading them side
    /// by side is what makes the differences between them -- a narrowed re-ask,
    /// a withdrawn binding, a degraded cause under its own role -- visible.
    /// Inside `answer` they read as one statement.
    fn apply_resolution(
        &mut self,
        turn: &Turn<'_>,
        resolution: Resolution,
    ) -> Result<(), BotError> {
        let options = turn.options;
        match resolution {
            Resolution::Resolved { index, .. } => self.accept_resolved(turn, index),
            Resolution::Ambiguous { tied, .. } => {
                // Narrow the re-ask to the options still in play. Repeating the
                // full list is what a two-way verdict forced, and it is why an
                // ambiguous answer could loop: the same utterance resolves the
                // same way every time it is asked, so the session never leaves
                // the node. A re-ask that is strictly smaller terminates.
                let narrowed: Vec<String> = tied
                    .iter()
                    .filter_map(|candidate| options.get(*candidate).cloned())
                    .collect();
                // A narrowed set of one is not a question -- there is nothing
                // to choose between -- so the full list is asked again.
                let ask: &[String] = if narrowed.len() >= 2 {
                    &narrowed
                } else {
                    options
                };
                self.reask(turn, ask)
            }
            Resolution::Absent { .. } => self.reask(turn, options),
            Resolution::StaleAlias {
                question: bound_question,
                option,
            } => {
                // Re-ask, as for `Absent`, but record what was withdrawn under
                // its own role. The resolver is not asked to forget the binding:
                // the seam is read-only by design, and a session that silently
                // rewrote its resolver's learned vocabulary would be editing the
                // audit record it is supposed to be producing. The record names
                // the binding and the question; retiring it is the owner's call.
                // A re-ask is a decision, so it is receipted like every other
                // verdict; the receipt names the withdrawal, and the roles
                // below name it again for whoever reads the transcript.
                self.record_stale_alias(turn.node_id, &bound_question, &option)?;
                self.reask(turn, options)
            }
            Resolution::Degraded { reason } => {
                // Re-ask, as for `Absent`, but record the cause under its own
                // role: a transcript that renders a degraded re-ask exactly as
                // an unclear one is how an operator concludes the person was
                // being difficult while the embedder was down.
                self.record_degraded(turn.node_id, reason)?;
                self.reask(turn, options)
            }
        }
    }
    /// Submit one free-text answer. Unrecognized input is recorded, the same
    /// ask remains current, and the prompt is recorded again.
    ///
    /// Every verdict — resolved, ambiguous, absent, or degraded — writes a
    /// [`DecisionReceipt`], because a re-ask is a decision too and the one an
    /// operator most needs to see afterwards.
    ///
    /// The receipt is written through the journal before the transition it
    /// describes is applied, and a journal that refuses it aborts the answer
    /// with [`BotError::ReceiptNotRecorded`] leaving the session exactly as it
    /// was: not advanced, not terminated, the variable unwritten, the
    /// transcript untouched, and no receipt held. Recording is part of
    /// accepting an answer, not a report written beside it, so the one failure
    /// this ordering leaves behind is a decision that was reached and recorded
    /// whose answer could not then be stored — which is the direction an audit
    /// record must err in, and the opposite of an accepted answer with no
    /// record of why.
    pub fn answer(&mut self, utterance: &str) -> Result<(), BotError> {
        if self.terminal.is_some() {
            let refusal = Err(BotError::SessionTerminated);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "answer: returning an error to the caller");
            return refusal;
        }
        let Some(node_id) = self.current.clone() else {
            let refusal = Err(BotError::SessionNotAwaitingAnswer);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "answer: returning an error to the caller");
            return refusal;
        };
        let Some(NodeKind::Ask {
            var,
            options,
            routes,
        }) = self.flow.node(&node_id).cloned()
        else {
            let refusal = Err(BotError::SessionNotAwaitingAnswer);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "answer: returning an error to the caller");
            return refusal;
        };
        // The ingress ceiling, and it comes before the step is charged and
        // before the utterance is copied anywhere: a refused line must neither
        // consume budget nor leave a record that reads as part of the
        // conversation. The check is one comparison against a bounded length,
        // so refusing costs nothing that accepting would not have cost more of.
        let utterance_limit = self.limits.get(ResourceAxis::Utterance);
        if utterance.len() > utterance_limit {
            let refusal = Err(BotError::UtteranceTooLarge {
                bytes: utterance.len(),
                limit: utterance_limit,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "answer: returning an error to the caller");
            return refusal;
        }
        self.charge_step()?;
        let question =
            Question::new(&node_id, &options).with_domain(self.scope.answer_domain(&var));
        let verdict = self.resolver.resolve(utterance, &question);
        let turn = Turn {
            node_id: &node_id,
            var: &var,
            options: &options,
            routes: &routes,
            verdict: &verdict,
            utterance,
        };
        self.apply_resolution(&turn, verdict.resolution().clone())
    }

    /// Build one receipt and write it through the journal, or refuse the
    /// answer.
    ///
    /// The session's own copy is stored only after the sink has taken it, and
    /// the acceptance that comes back is stored beside it: what the journal
    /// said about the receipt is its own fact, and a receipt that carried its
    /// own acceptance would be the session answering a question only the sink
    /// can answer.
    fn write_receipt(
        &mut self,
        node_id: &str,
        options: &[String],
        verdict: &Verdict,
        selected: Option<String>,
        route: Option<&str>,
    ) -> Result<(), BotError> {
        let receipt = DecisionReceipt {
            version: RECEIPT_VERSION,
            session: self.id.clone(),
            flow: self.flow_revision.clone(),
            node: node_id.to_owned(),
            options: options_revision(options),
            resolution: verdict.resolution().clone(),
            provenance: verdict.provenance().clone(),
            selected,
            route: route.map(str::to_owned),
        };
        // The sink takes it first. The session's own copy is stored only once
        // the journal has accepted it, so a refusal leaves no receipt held.
        let acceptance = self.journal.record_decision(&receipt).map_err(|cause| {
            BotError::ReceiptNotRecorded {
                node: node_id.to_owned(),
                cause,
            }
        })?;
        self.decisions.push(RecordedDecision {
            receipt,
            acceptance,
        });
        Ok(())
    }

    /// Charge one bounded step or return [`BotError::SessionBudgetExceeded`].
    fn charge_step(&mut self) -> Result<(), BotError> {
        let attempted = self
            .steps
            .checked_add(1)
            .ok_or(BotError::SessionBudgetExceeded {
                steps: usize::MAX,
                budget: self.flow.bounds.budget,
            })?;
        if attempted > self.flow.bounds.budget {
            let refusal = Err(BotError::SessionBudgetExceeded {
                steps: attempted,
                budget: self.flow.bounds.budget,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "charge_step: returning an error to the caller");
            return refusal;
        }
        self.steps = attempted;
        Ok(())
    }

    /// Execute deterministic nodes until an ask or terminal is reached.
    /// Renders `text` and records it as the assistant's turn.
    ///
    /// `say` and `refer` both speak before either continues, and inline they
    /// each rendered and recorded as two statements, so the drive loop carried
    /// four of them across two arms and a reader could not see that the pair is
    /// one step. It matters that the record follows the render: a render that
    /// refuses must leave nothing said.
    pub(super) fn speak(&mut self, node_id: &str, text: &str) -> Result<(), BotError> {
        let rendered = self.render(node_id, text)?;
        self.record(node_id, "assistant", &rendered)
    }

    /// Advances the session until it needs an answer or reaches an outcome.
    fn drive(&mut self) -> Result<(), BotError> {
        loop {
            let Some(node_id) = self.current.clone() else {
                return Ok(());
            };
            self.charge_step()?;
            // The path is retained like a record is, and charged like one: a
            // visited id is an owned copy of the node id the document carries,
            // so the same loop that produces unbounded records would otherwise
            // produce an unbounded path out of the same repetition.
            self.charge_retention(node_id.len())?;
            self.visited.push(node_id.clone());
            let Some(kind) = self.flow.node(&node_id).cloned() else {
                let refusal = Err(BotError::InvalidTransitionTarget {
                    from: "<session>".into(),
                    target: node_id,
                });
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "drive: returning an error to the caller");
                return refusal;
            };
            if self.step(&node_id, kind)? {
                return Ok(());
            }
        }
    }

    /// Advances one node. Answers whether the session must stop and wait.
    ///
    /// A helper rather than the `match` inside the drive loop: as one block the
    /// loop carried the charge, the lookup and every arm's write as one
    /// statement, and the thing that actually varies -- what a node kind does --
    /// was indistinguishable from the thing that does not -- what every node
    /// does first.
    ///
    /// `Ok(true)` means the session is waiting for an answer, which is the only
    /// kind that stops the loop short of an outcome.
    /// Speaks a `say` node's text, then follows its single continuation.
    ///
    /// Speaking before the transition is the contract: a render that refuses must
    /// leave the session on the node it was already on, not halfway to the next.
    fn speak_then_follow(&mut self, node_id: &str, text: &str) -> Result<(), BotError> {
        self.speak(node_id, text)?;
        let Some(target) = self.flow.edge_targets(node_id).into_iter().next() else {
            let refusal = Err(BotError::MissingTransition {
                node: node_id.to_owned(),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "speak_then_follow: returning an error to the caller");
            return refusal;
        };
        self.current = Some(target);
        Ok(())
    }

    /// Moves to whichever edge the `branch` node's predicate selects.
    ///
    /// A `branch` has exactly two edges and its predicate chooses between them,
    /// so this is a lookup rather than a decision: the predicate is the whole of
    /// the choice and it has already been validated at load.
    fn take_branch(
        &mut self,
        when: &Predicate,
        then: NodeId,
        otherwise: NodeId,
    ) -> Result<(), BotError> {
        self.current = Some(if when.evaluate(&self.scope)? {
            then
        } else {
            otherwise
        });
        Ok(())
    }

    /// Speaks a `refer` node's text, then ends with the referral it declares.
    ///
    /// The text is emitted before the outcome is set, and the only declared
    /// outcome a `refer` node can carry is the referral itself -- validation
    /// refuses anything else -- so this text is never spoken for an outcome that
    /// contradicts it.
    pub(super) fn refer(&mut self, node_id: &str, text: &str) -> Result<(), BotError> {
        self.speak(node_id, text)?;
        self.finish(node_id)
    }

    /// Advances one node, and answers whether the session must stop and wait.
    ///
    /// `Ok(true)` is the ask: the only kind that stops the loop short of an
    /// outcome. Each arm is the one step its kind performs, named below.
    pub(super) fn step(&mut self, node_id: &str, kind: NodeKind) -> Result<bool, BotError> {
        match kind {
            NodeKind::Say { text } => self.speak_then_follow(node_id, &text),
            NodeKind::Ask { options, .. } => {
                self.record_prompt(node_id, &options)?;
                return Ok(true);
            }
            NodeKind::Branch {
                when,
                then,
                otherwise,
                ..
            } => self.take_branch(&when, then, otherwise),
            // The two terminal kinds that emit nothing before they end.
            // Both read the outcome from the document rather than
            // constructing it from the node, so a declared outcome is
            // executed here and not only checked at load.
            NodeKind::Handoff { .. } | NodeKind::End => self.finish(node_id),
            NodeKind::Refer { text, .. } => self.refer(node_id, &text),
            NodeKind::Route { dispatch, fallback } => {
                self.current = Some(if self.last_utterance.is_some() {
                    dispatch
                } else {
                    fallback
                });
                Ok(())
            }
        }?;
        Ok(false)
    }

    /// End the session with the outcome the document gives one terminal node.
    ///
    /// The one place a session reads a terminal outcome. `drive` reaches this
    /// for every terminal node kind, so the outcome validation checked and the
    /// outcome execution returns are the same value from the same calculation
    /// ([`FlowSpec::effective_terminal`]) and cannot drift: an earlier runner
    /// consulted the terminal map for `End` alone, and a refusal declared on a
    /// `handoff` node was accepted by validation and ignored here.
    fn finish(&mut self, node_id: &str) -> Result<(), BotError> {
        let Some(outcome) = self.flow.effective_terminal(node_id) else {
            let refusal = Err(BotError::MalformedFlow {
                cause: format!("node {node_id:?} reached with no terminal outcome"),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "finish: returning an error to the caller");
            return refusal;
        };
        self.terminal = Some(outcome);
        self.current = None;
        Ok(())
    }

    /// Render one node's template under this session's record ceiling.
    ///
    /// The ceiling is applied to the *computed* expansion, inside the
    /// interpolator, before the output buffer exists. What that buys is the
    /// whole of the amplification fix: a valid document can name a placeholder
    /// a million times, and the cost of discovering that is a million additions
    /// rather than a multi-gigabyte allocation.
    pub(super) fn render(&self, node_id: &str, template: &str) -> Result<String, BotError> {
        TemplateInterpolator::new().interpolate(
            template,
            &self.scope,
            node_id,
            self.limits.get(ResourceAxis::Record),
        )
    }

    /// Charge `bytes` against the session's retention ceiling.
    ///
    /// Charged before the allocation, so a refused write leaves nothing behind
    /// to clean up: the journal has not been told, the transcript has not grown,
    /// and the counter has not moved.
    fn charge_retention(&mut self, bytes: usize) -> Result<(), BotError> {
        let limit = self.limits.get(ResourceAxis::Session);
        let total = self
            .retained
            .checked_add(bytes)
            .ok_or(BotError::SessionRetentionExceeded {
                bytes: usize::MAX,
                limit,
            })?;
        if total > limit {
            let refusal = Err(BotError::SessionRetentionExceeded {
                bytes: total,
                limit,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "charge_retention: returning an error to the caller");
            return refusal;
        }
        self.retained = total;
        Ok(())
    }

    /// Append one transcript record and forward it to the journal seam.
    ///
    /// Two ceilings, in this order, and both before either sink is touched: the
    /// payload against the per-record ceiling, which is what makes the refusal
    /// name the node whose text is too long, and the whole record against the
    /// aggregate, which is what bounds a conversation the graph lets repeat.
    /// The record's cost counts the path node and the role as well as the text,
    /// because all three are owned copies this session retains.
    ///
    /// # Errors
    ///
    /// Returns [`BotError::RecordTooLarge`] or
    /// [`BotError::SessionRetentionExceeded`]. Neither has written anything.
    pub(super) fn record(&mut self, node_id: &str, role: &str, text: &str) -> Result<(), BotError> {
        let record_bytes = self.limits.get(ResourceAxis::Record);
        if text.len() > record_bytes {
            let refusal = Err(BotError::RecordTooLarge {
                node: node_id.to_owned(),
                bytes: text.len(),
                limit: record_bytes,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "record: returning an error to the caller");
            return refusal;
        }
        let cost = text
            .len()
            .saturating_add(node_id.len())
            .saturating_add(role.len());
        self.charge_retention(cost)?;
        self.journal.record(node_id, role, text);
        self.transcript.push(TranscriptEntry {
            path_node: node_id.to_owned(),
            role: role.to_owned(),
            text: text.to_owned(),
        });
        Ok(())
    }

    /// Record the standard prompt for an ask node.
    ///
    /// The prompt is built from the node's candidates, so its size is checked
    /// before it is built rather than after: a node with many long options
    /// would otherwise allocate the joined string and *then* discover that the
    /// record ceiling refuses it. `checked_mul` for the separators and
    /// `checked_add` for each option, with the overflow case reported as the
    /// same refusal rather than wrapped.
    ///
    /// # Errors
    ///
    /// Returns [`BotError::RecordTooLarge`] for a prompt over the ceiling.
    pub(super) fn record_prompt(
        &mut self,
        node_id: &str,
        options: &[String],
    ) -> Result<(), BotError> {
        let bytes = prompt_bytes(options);
        let record_bytes = self.limits.get(ResourceAxis::Record);
        if bytes > record_bytes {
            let refusal = Err(BotError::RecordTooLarge {
                node: node_id.to_owned(),
                bytes,
                limit: record_bytes,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "record_prompt: returning an error to the caller");
            return refusal;
        }
        let prompt = format!("Choose one: {}", options.join(", "));
        self.record(node_id, "assistant", &prompt)
    }

    /// Records that the resolver could not reach a verdict.
    ///
    /// A distinct role rather than a second assistant prompt, because the two
    /// failures need different repairs and a transcript is where that is
    /// decided: an absent resolution says the person's words did not fit the
    /// options, and a degraded one says the resolver never got to compare them.
    fn record_degraded(&mut self, node_id: &str, reason: DegradedReason) -> Result<(), BotError> {
        let text = format!("Resolver unavailable: {reason}");
        self.record(node_id, "resolver-degraded", &text)
    }

    /// Records that a confirmed phrase's option is no longer offered.
    ///
    /// A distinct role for the reason the degraded case has one: this record is
    /// read by whoever maintains the vocabulary, and it has to name both halves
    /// of the broken binding — the question it was confirmed in and the option
    /// that went away — because either one alone is unactionable. "A stale alias
    /// was ignored" tells that reader nothing they can repair.
    fn record_stale_alias(
        &mut self,
        node_id: &str,
        question: &str,
        option: &str,
    ) -> Result<(), BotError> {
        let text = format!("Superseded alias: question {question} no longer offers \"{option}\"");
        self.record(node_id, "resolver-stale-alias", &text)
    }
}

/// Reports the cursor's position and the session's accounting.
///
/// Manual rather than derived: the session holds its resolver and its journal
/// behind `dyn` seams, so a derive is not available, and it holds the transcript
/// in full, which a derived rendering would print entry by entry. What a reader
/// of a session's `Debug` wants is where the cursor is, what it is holding, and
/// what it has spent — so the collections are printed as the counts that answer
/// that, and `finish_non_exhaustive` marks the omission rather than hiding it.
impl core::fmt::Debug for Session {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Session")
            .field("id", &self.id)
            .field("current", &self.current)
            .field("terminal", &self.terminal)
            .field("steps", &self.steps)
            .field("visited", &self.visited.len())
            .field("transcript", &self.transcript.len())
            .field("decisions", &self.decisions.len())
            .field("retained", &self.retained)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}
