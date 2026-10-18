//! The flow document: nodes, edges, terminals, bounds and the spec that owns them. Split from `session.rs` (#526).

use super::{
    BTreeMap, BotError, DeclaredNodes, Deserialize, NodeId, Predicate, ResourceLimits,
    SHIPPED_LIMITS, Serialize, VarType, check_flow_size, flow, malformed_flow, merge_read_kinds,
    parse_json, parse_ron, reject_unknown_node_kinds,
};

/// A declared flow node and its closed operation kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", rename_all = "snake_case")]
#[non_exhaustive]
pub enum NodeKind {
    /// Emit interpolated text and follow its [`FlowEdge`] continuation.
    Say {
        /// Text shown to the user.
        text: String,
    },
    /// Ask for a value and route the recognized option.
    Ask {
        /// Variable receiving the selected option.
        var: String,
        /// Candidate answer strings passed to the resolver.
        options: Vec<String>,
        /// Route keyed by the selected option string.
        routes: BTreeMap<String, NodeId>,
    },
    /// Evaluate a predicate and choose one of two destinations.
    Branch {
        /// Variable read by the branch.
        var: String,
        /// Predicate deciding the `then` destination.
        when: Predicate,
        /// Destination when `when` is true.
        then: NodeId,
        /// Destination when `when` is false.
        otherwise: NodeId,
    },
    /// End the session with a handoff outcome.
    Handoff {
        /// Human or system target receiving the handoff.
        target: String,
    },
    /// End the session with a referral outcome after emitting text.
    Refer {
        /// Human or system target receiving the referral.
        target: String,
        /// Text shown before the referral is recorded.
        text: String,
    },
    /// Choose `dispatch` after an accepted answer, or `fallback` at a cold
    /// route with no accepted answer.
    Route {
        /// Destination used after a recognized answer.
        dispatch: NodeId,
        /// Destination used when no answer has been accepted.
        fallback: NodeId,
    },
    /// End the session. The flow terminal map can override the default
    /// [`Terminal::Completed`] outcome with a refusal.
    End,
}

/// Compatibility name for the reference flow representation.
pub type FlowNodeKind = NodeKind;

/// One explicit continuation in a flow graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", rename_all = "snake_case")]
#[non_exhaustive]
pub enum FlowEdge {
    /// Continue from one node to another.
    Next {
        /// Source node.
        from: NodeId,
        /// Destination node.
        to: NodeId,
    },
    /// Reference-compatible spelling for an unconditional continuation.
    After {
        /// Source node.
        from: NodeId,
        /// Destination node.
        to: NodeId,
    },
}

impl FlowEdge {
    /// Construct an unconditional continuation.
    #[must_use]
    pub fn next(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self::Next {
            from: from.into(),
            to: to.into(),
        }
    }

    /// Return the source and destination node ids.
    #[must_use]
    pub fn endpoints(&self) -> (&str, &str) {
        match *self {
            Self::Next { ref from, ref to } | Self::After { ref from, ref to } => (from, to),
        }
    }
}

/// How a run's workflow ended, without its payload.
///
/// The routing class: a consumer acting on a refusal, a referral or a handoff
/// matches here, and reads the detail from [`Terminal`]. Payload-free so it is
/// `Copy` and can be matched in a guard.
///
/// This says nothing about effects. It is deliberately a separate fact from
/// [`EffectLedger`] rather than an arm of one enum, because "the flow was
/// refused" and "an earlier effect may be live" are independent, and a single
/// classifier that prioritises either one destroys the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Disposition {
    /// The flow ran to its own end.
    Completed,
    /// The flow referred the caller to another tier.
    Referred,
    /// The flow handed ownership to another actor.
    HandedOff,
    /// The flow refused to continue. A *decided* outcome, which settles nothing
    /// about an effect an earlier step already attempted.
    Refused,
}

/// What a run knows about the effects it attempted.
///
/// Two counts, because they answer different questions and neither can be
/// inferred from the other. `confirmed` is what the run watched take effect.
/// `unsettled` is what it attempted and could not settle — the effects a domain
/// reported as [`BotError::EffectIndeterminate`].
///
/// A non-zero `unsettled` does **not** imply that anything succeeded. One
/// attempted effect whose response was lost may have produced zero effects or
/// one, and that ambiguity is what this type keeps rather than resolves: the
/// honest report is "nothing is confirmed, and one is in doubt", not "part of
/// the output exists".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields)]
#[non_exhaustive]
pub struct EffectLedger {
    /// Effects the run watched take effect.
    pub(super) confirmed: usize,
    /// Effects whose occurrence could not be determined.
    pub(super) unsettled: usize,
}

impl EffectLedger {
    /// Record a run's effect history.
    #[must_use]
    pub const fn new(confirmed: usize, unsettled: usize) -> Self {
        Self {
            confirmed,
            unsettled,
        }
    }

    /// Effects the run watched take effect.
    #[must_use]
    pub const fn confirmed(&self) -> usize {
        self.confirmed
    }

    /// Effects whose occurrence could not be determined.
    #[must_use]
    pub const fn unsettled(&self) -> usize {
        self.unsettled
    }

    /// Whether any attempted effect's occurrence is unknown, and therefore
    /// whether a retry is a possible duplicate rather than a repair.
    ///
    /// Reads the count and never the disposition: a refusal does not settle an
    /// earlier network outcome, so this stays `true` for a refused run that
    /// left an effect in doubt.
    #[must_use]
    pub const fn needs_reconciliation(&self) -> bool {
        self.unsettled > 0
    }
}

/// A run's report: what the workflow decided, and what is known about the
/// effects it attempted.
///
/// Two independent fields rather than arms of one enum. A refused run that also
/// leaves an effect in doubt is both *refused* and *in need of reconciliation*,
/// and no single arm can say that: an earlier version of this type returned
/// `Failure` for every refusal and erased the uncertainty, which is the exact
/// information [`BotError::EffectIndeterminate`] was introduced to preserve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields)]
#[non_exhaustive]
pub struct Outcome {
    /// What the workflow decided.
    pub(super) disposition: Disposition,
    /// What is known about the effects it attempted.
    pub(super) effects: EffectLedger,
}

impl Outcome {
    /// What the workflow decided.
    #[must_use]
    pub const fn disposition(&self) -> Disposition {
        self.disposition
    }

    /// What is known about the effects the run attempted.
    #[must_use]
    pub const fn effects(&self) -> EffectLedger {
        self.effects
    }

    /// Whether any attempted effect's occurrence is unknown.
    #[must_use]
    pub const fn needs_reconciliation(&self) -> bool {
        self.effects.needs_reconciliation()
    }
}

/// One predicate arm used by callers building a higher-level choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields)]
#[non_exhaustive]
pub struct ChoiceArm {
    /// Predicate guarding the arm.
    pub(super) when: Predicate,
    /// Destination selected by the arm.
    pub(super) then: NodeId,
}

impl ChoiceArm {
    /// Construct one guarded destination.
    #[must_use]
    pub fn new(when: Predicate, then: impl Into<String>) -> Self {
        Self {
            when,
            then: then.into(),
        }
    }

    /// Return the guard predicate.
    #[must_use]
    pub fn when(&self) -> &Predicate {
        &self.when
    }

    /// Return the destination node id.
    #[must_use]
    pub fn then(&self) -> &str {
        &self.then
    }
}

/// A terminal outcome returned by a completed session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Terminal {
    /// The flow completed normally.
    Completed,
    /// The flow referred the user to another target.
    Referred {
        /// Referral target.
        target: String,
    },
    /// The flow handed the user to another target.
    HandedOff {
        /// Handoff target.
        target: String,
    },
    /// The flow refused to continue.
    Refused {
        /// Human-readable refusal reason.
        reason: String,
    },
}

impl Terminal {
    /// Report this terminal together with the run's effect history.
    ///
    /// Both facts are carried through unchanged, and that is the whole of the
    /// method. It does not classify, because classifying is where the
    /// information was being lost: an earlier version returned a single enum
    /// and had to choose, for a refused run that also left an effect unsettled,
    /// whether to report the refusal or the uncertainty. Both are true, both
    /// matter to different consumers, and a report that drops either one is
    /// wrong for whoever needed it.
    #[must_use]
    pub fn outcome(&self, effects: EffectLedger) -> Outcome {
        let disposition = match *self {
            Self::Completed => Disposition::Completed,
            Self::Referred { .. } => Disposition::Referred,
            Self::HandedOff { .. } => Disposition::HandedOff,
            Self::Refused { .. } => Disposition::Refused,
        };
        Outcome {
            disposition,
            effects,
        }
    }
}

/// Bounds applied to a flow and every session executing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields)]
#[non_exhaustive]
pub struct FlowBounds {
    /// Maximum number of runtime steps, including answer attempts.
    pub(super) budget: usize,
    /// Byte ceilings the document asks to be held to, if any.
    ///
    /// Absent means "whatever the operator enforces". Present means "no more
    /// than this", and the requested ceilings are checked against the
    /// operator's hard ceiling when the document loads: an author may declare
    /// that this flow's text is small and may not declare that it is allowed to
    /// be large.
    #[serde(default)]
    resources: Option<ResourceLimits>,
}

impl FlowBounds {
    /// Construct bounds with a declared step budget and the operator's shipped
    /// byte ceilings.
    #[must_use]
    pub const fn new(budget: usize) -> Self {
        Self {
            budget,
            resources: None,
        }
    }

    /// Return the declared step budget.
    #[must_use]
    pub const fn budget(self) -> usize {
        self.budget
    }

    /// Return the byte ceilings the document asks for, if it asks for any.
    #[must_use]
    pub const fn resources(self) -> Option<ResourceLimits> {
        self.resources
    }

    /// The byte ceilings in force for this document.
    ///
    /// Two cases, both declared by the type: a document that asked for ceilings
    /// gets the ones it asked for, and one that asked for none gets
    /// [`ResourceLimits::shipped`] — which is what "whatever the operator
    /// enforces" means, and is not a value standing in for a missing request.
    /// Either way the result goes through `within_ceiling` before anything runs,
    /// because an author's request above the operator's is refused rather than
    /// honoured.
    #[must_use]
    pub fn effective_resources(self) -> ResourceLimits {
        match self.resources {
            Some(requested) => requested,
            None => SHIPPED_LIMITS,
        }
    }

    /// Return these bounds with a declared byte-ceiling request attached.
    #[must_use]
    pub const fn with_resources(mut self, resources: ResourceLimits) -> Self {
        self.resources = Some(resources);
        self
    }
}

impl Default for FlowBounds {
    fn default() -> Self {
        Self::new(256)
    }
}

/// A validated, declarative flow document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields)]
#[non_exhaustive]
pub struct FlowSpec {
    /// Variable declarations.
    pub(super) vars: BTreeMap<String, VarType>,
    /// Entry node id.
    pub(super) entry: NodeId,
    /// Node map keyed by stable node id.
    pub(super) nodes: BTreeMap<NodeId, NodeKind>,
    /// Explicit continuations, primarily used by [`NodeKind::Say`].
    #[serde(default)]
    pub(super) edges: Vec<FlowEdge>,
    /// Explicit terminal outcomes keyed by node id.
    #[serde(default)]
    pub(super) terminals: BTreeMap<NodeId, Terminal>,
    /// Static and runtime bounds.
    #[serde(default)]
    pub(super) bounds: FlowBounds,
}

impl FlowSpec {
    /// Construct and validate a flow document.
    pub fn new(
        vars: BTreeMap<String, VarType>,
        entry: impl Into<String>,
        nodes: BTreeMap<NodeId, NodeKind>,
        edges: Vec<FlowEdge>,
        terminals: BTreeMap<NodeId, Terminal>,
        bounds: FlowBounds,
    ) -> Result<Self, BotError> {
        let spec = Self {
            vars,
            entry: entry.into(),
            nodes,
            edges,
            terminals,
            bounds,
        };
        spec.validate()?;
        Ok(spec)
    }

    /// Construct a flow without explicit edges or terminal declarations.
    /// `Say` nodes still require a continuation edge, so this helper is useful
    /// only for flows made entirely from direct-transition node kinds.
    pub fn from_nodes(
        vars: BTreeMap<String, VarType>,
        entry: impl Into<String>,
        nodes: BTreeMap<NodeId, NodeKind>,
        bounds: FlowBounds,
    ) -> Result<Self, BotError> {
        Self::new(vars, entry, nodes, Vec::new(), BTreeMap::new(), bounds)
    }

    /// Serialize the validated flow using the shared JSON facade.
    pub fn to_json(&self) -> Result<String, lgwks_std::json::Error> {
        crate::json::to_string_pretty(self)
    }

    /// Serialize the validated flow as RON.
    ///
    /// RON is the estate's notation for human-facing documents, and a flow is
    /// one: a file a person writes and reads. The encoding is serde's, so the
    /// document is the same one [`to_json`](Self::to_json) writes, spelled with
    /// the unquoted keys, comments and trailing commas RON allows.
    pub fn to_ron(&self) -> Result<String, lgwks_std::ron::Error> {
        lgwks_std::ron::to_string_pretty(self)
    }

    /// Parse and validate a flow using the shared JSON facade.
    pub fn from_json(source: &str) -> Result<Self, BotError> {
        check_flow_size(source)?;
        reject_unknown_node_kinds(&merge_read_kinds(
            parse_json::<DeclaredNodes<false>>(source),
            parse_json::<DeclaredNodes<true>>(source),
        ))?;
        let spec = parse_json::<Self>(source).map_err(malformed_flow)?;
        spec.validate()?;
        Ok(spec)
    }

    /// Parse and validate a flow written in RON.
    ///
    /// Both codecs converge on the same validation, so the size limit, the
    /// unknown-node-kind diagnostic and the structural checks are identical
    /// whichever spelling a document arrives in. Only the parse differs, which
    /// is what keeps one document from being acceptable in one notation and
    /// refused in the other.
    ///
    /// Every enum in the document is externally tagged, so a variant is written
    /// with its own name, which is what makes a flow read as a script rather
    /// than as a tagged record:
    ///
    /// ```ron
    /// FlowSpec(
    ///     vars: {},
    ///     entry: "end",
    ///     nodes: { "end": End },
    /// )
    /// ```
    ///
    /// The cost is on the JSON side, where the same variant becomes
    /// `{"end": {}}`, and a unit variant becomes the bare string `"end"`.
    pub fn from_ron(source: &str) -> Result<Self, BotError> {
        check_flow_size(source)?;
        // No node-kind guard here, and the reason is RON's rather than a
        // choice: RON checks a variant name against the list it is handed, so
        // an unknown name is refused before any visitor of ours sees it, and
        // the name reaches the surface only through RON's own error. That error
        // names both the variant and the enum it was not a member of, which is
        // more than serde says on the JSON path. There, a name is not checked
        // against anything, so the guard is what supplies the name at all, and
        // with it the node that carried it.
        let spec = parse_ron::<Self>(source).map_err(malformed_flow)?;
        spec.validate()?;
        Ok(spec)
    }

    /// Re-run the structural validation for a flow already held in memory.
    pub fn validate(&self) -> Result<(), BotError> {
        flow::validate(self)
    }

    /// Return the declared variable map.
    #[must_use]
    pub fn vars(&self) -> &BTreeMap<String, VarType> {
        &self.vars
    }

    /// Return the entry node id.
    #[must_use]
    pub fn entry(&self) -> &str {
        &self.entry
    }

    /// Return the node map.
    #[must_use]
    pub fn nodes(&self) -> &BTreeMap<NodeId, NodeKind> {
        &self.nodes
    }

    /// Return the explicit continuation list.
    #[must_use]
    pub fn edges(&self) -> &[FlowEdge] {
        &self.edges
    }

    /// Return the explicit terminal map.
    #[must_use]
    pub fn terminals(&self) -> &BTreeMap<NodeId, Terminal> {
        &self.terminals
    }

    /// Return the applied bounds.
    #[must_use]
    pub const fn bounds(&self) -> FlowBounds {
        self.bounds
    }

    /// Look up one node by id.
    #[must_use]
    pub fn node(&self, id: &str) -> Option<&NodeKind> {
        self.nodes.get(id)
    }

    /// The outcome one node ends a session with, after the terminal map is
    /// consulted.
    ///
    /// The one calculation of "what does this document say happens here",
    /// shared by validation and execution. `None` means the node is not a
    /// terminal node — an `End`, `Handoff`, or `Refer` — and no outcome is
    /// defined for it.
    ///
    /// A declared terminal is authoritative for every terminal node, which is
    /// what makes an `End` node's refusal override work. On a `Handoff` or
    /// `Refer` node that same authority would silently discard the target the
    /// node was written to hand to, so a declaration that *contradicts* the
    /// node's own outcome is refused at load
    /// ([`BotError::ConflictingTerminalDeclaration`]) rather than accepted and
    /// ignored: a document that says a referral is refused is either corrected
    /// or rejected, never run as a permissive referral. A declaration that
    /// repeats the node's own outcome is accepted and is what this method
    /// returns, so the declaration is read rather than merely tolerated.
    #[must_use]
    pub fn effective_terminal(&self, node_id: &str) -> Option<Terminal> {
        // A `?` here is the non-terminal case, and it is why the declared
        // lookup cannot be written first: an `End`-only declaration on a `Say`
        // node is refused by validation and has no effective outcome.
        let intrinsic = self.intrinsic_terminal(node_id)?;
        match self.terminals.get(node_id) {
            Some(declared) => Some(declared.clone()),
            None => Some(intrinsic),
        }
    }

    /// The outcome a terminal node carries before the terminal map is
    /// consulted: `End` completes, `Handoff` names its target, and `Refer`
    /// names its target.
    ///
    /// Private because it is the half of [`Self::effective_terminal`] that is
    /// only meaningful inside the crate: a caller that wants the document's
    /// outcome wants the declared one, and getting the intrinsic half would be
    /// getting the answer this method exists to be corrected from.
    pub(super) fn intrinsic_terminal(&self, node_id: &str) -> Option<Terminal> {
        match *self.nodes.get(node_id)? {
            NodeKind::End => Some(Terminal::Completed),
            NodeKind::Handoff { ref target } => Some(Terminal::HandedOff {
                target: target.clone(),
            }),
            NodeKind::Refer { ref target, .. } => Some(Terminal::Referred {
                target: target.clone(),
            }),
            _ => None,
        }
    }

    /// Return all explicit edge destinations from one node.
    pub(super) fn edge_targets(&self, from: &str) -> Vec<NodeId> {
        self.edges
            .iter()
            .filter_map(|edge| {
                let (source, target) = edge.endpoints();
                (source == from).then(|| target.to_owned())
            })
            .collect()
    }
}
