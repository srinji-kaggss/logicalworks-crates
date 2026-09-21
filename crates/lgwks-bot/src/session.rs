//! Validated declarative guidance flows and a synchronous session runner.
//!
//! The module deliberately has no relation to [`crate::domain::flow::Pipeline`].
//! A pipeline composes capability-gated effects; this module interprets a
//! human-facing, validated graph and records the cursor's conversation.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use lgwks_std::hash::Hasher;
use lgwks_std::json::Value as JsonValue;
use lgwks_std::json::{Deserialize, Serialize};

use crate::error::BotError;
use crate::semantic::EmbedderIdentity;

/// Maximum JSON flow size accepted before parsing begins.
pub const MAX_FLOW_BYTES: usize = 2_097_152;

/// Alias for [`MAX_FLOW_BYTES`] with an explicit schema-oriented name.
pub const MAX_FLOW_SPEC_BYTES: usize = MAX_FLOW_BYTES;

/// Stable identifier of a flow node.
pub type NodeId = String;

/// The closed set of scalar values carried by a [`VarScope`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    crate = "lgwks_std::json::serde",
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
#[non_exhaustive]
pub enum Value {
    /// A free-form string.
    String(String),
    /// A signed integer.
    Integer(i64),
    /// A boolean.
    Boolean(bool),
    /// A selected member of a declared choice variable.
    Choice(String),
}

impl fmt::Display for Value {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::String(ref value) | Self::Choice(ref value) => formatter.write_str(value),
            Self::Integer(value) => write!(formatter, "{value}"),
            Self::Boolean(value) => write!(formatter, "{value}"),
        }
    }
}

/// Declared type of one variable in a flow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    crate = "lgwks_std::json::serde",
    tag = "kind",
    content = "options",
    rename_all = "snake_case"
)]
#[non_exhaustive]
pub enum VarType {
    /// A free-form string assigned by an ask.
    String,
    /// A signed integer parsed from an answer.
    Integer,
    /// A boolean parsed from `true`/`false` or `yes`/`no`.
    Boolean,
    /// A finite set of allowed answer values.
    Choice(Vec<String>),
}

/// A variable reference or literal used by a branch predicate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    crate = "lgwks_std::json::serde",
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
#[non_exhaustive]
pub enum ValueExpr {
    /// A literal scalar.
    Literal(Value),
    /// The current value of a named variable.
    Var(String),
}

impl ValueExpr {
    /// Construct a variable reference.
    #[must_use]
    pub fn variable(name: impl Into<String>) -> Self {
        Self::Var(name.into())
    }
}

/// Closed, side-effect-free branch predicate language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    crate = "lgwks_std::json::serde",
    tag = "op",
    content = "args",
    rename_all = "snake_case"
)]
#[non_exhaustive]
pub enum Predicate {
    /// A constant predicate.
    Const(bool),
    /// Equality comparison.
    Eq(ValueExpr, ValueExpr),
    /// Inequality comparison.
    Ne(ValueExpr, ValueExpr),
    /// Less-than comparison.
    Lt(ValueExpr, ValueExpr),
    /// Less-than-or-equal comparison.
    Le(ValueExpr, ValueExpr),
    /// Greater-than comparison.
    Gt(ValueExpr, ValueExpr),
    /// Greater-than-or-equal comparison.
    Ge(ValueExpr, ValueExpr),
    /// Conjunction of predicates.
    And(Vec<Predicate>),
    /// Disjunction of predicates.
    Or(Vec<Predicate>),
    /// Negation of one predicate.
    Not(Box<Predicate>),
}

impl Predicate {
    /// Evaluate this predicate against a variable scope.
    pub fn evaluate(&self, scope: &VarScope) -> Result<bool, BotError> {
        match *self {
            Self::Const(value) => Ok(value),
            Self::Eq(ref left, ref right) => {
                Ok(resolve_expr(left, scope)? == resolve_expr(right, scope)?)
            }
            Self::Ne(ref left, ref right) => {
                Ok(resolve_expr(left, scope)? != resolve_expr(right, scope)?)
            }
            Self::Lt(ref left, ref right) => {
                compare_expr(left, right, scope, |ordering| ordering.is_lt())
            }
            Self::Le(ref left, ref right) => {
                compare_expr(left, right, scope, |ordering| ordering.is_le())
            }
            Self::Gt(ref left, ref right) => {
                compare_expr(left, right, scope, |ordering| ordering.is_gt())
            }
            Self::Ge(ref left, ref right) => {
                compare_expr(left, right, scope, |ordering| ordering.is_ge())
            }
            Self::And(ref items) => {
                for item in items {
                    if !item.evaluate(scope)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            Self::Or(ref items) => {
                for item in items {
                    if item.evaluate(scope)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Self::Not(ref inner) => Ok(!inner.evaluate(scope)?),
        }
    }
}

/// A declared flow node and its closed operation kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    crate = "lgwks_std::json::serde",
    tag = "kind",
    rename_all = "snake_case"
)]
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
#[serde(
    crate = "lgwks_std::json::serde",
    tag = "kind",
    rename_all = "snake_case"
)]
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
    confirmed: usize,
    /// Effects whose occurrence could not be determined.
    unsettled: usize,
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
    disposition: Disposition,
    /// What is known about the effects it attempted.
    effects: EffectLedger,
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
    when: Predicate,
    /// Destination selected by the arm.
    then: NodeId,
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
#[serde(
    crate = "lgwks_std::json::serde",
    tag = "kind",
    rename_all = "snake_case"
)]
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
    budget: usize,
}

impl FlowBounds {
    /// Construct bounds with a declared step budget.
    #[must_use]
    pub const fn new(budget: usize) -> Self {
        Self { budget }
    }

    /// Return the declared step budget.
    #[must_use]
    pub const fn budget(self) -> usize {
        self.budget
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
    vars: BTreeMap<String, VarType>,
    /// Entry node id.
    entry: NodeId,
    /// Node map keyed by stable node id.
    nodes: BTreeMap<NodeId, NodeKind>,
    /// Explicit continuations, primarily used by [`NodeKind::Say`].
    #[serde(default)]
    edges: Vec<FlowEdge>,
    /// Explicit terminal outcomes keyed by node id.
    #[serde(default)]
    terminals: BTreeMap<NodeId, Terminal>,
    /// Static and runtime bounds.
    #[serde(default)]
    bounds: FlowBounds,
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

    /// Parse and validate a flow using the shared JSON facade.
    pub fn from_json(source: &str) -> Result<Self, BotError> {
        if source.len() > MAX_FLOW_BYTES {
            return Err(BotError::FlowTooLarge {
                bytes: source.len(),
                limit: MAX_FLOW_BYTES,
            });
        }
        let value: JsonValue =
            crate::json::from_str(source).map_err(|error| BotError::MalformedFlow {
                cause: error.to_string().escape_debug().to_string(),
            })?;
        reject_unknown_node_kinds(&value)?;
        let spec: Self =
            crate::json::from_value(value).map_err(|error| BotError::MalformedFlow {
                cause: error.to_string().escape_debug().to_string(),
            })?;
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

    /// Return all explicit edge destinations from one node.
    fn edge_targets(&self, from: &str) -> Vec<NodeId> {
        self.edges
            .iter()
            .filter_map(|edge| {
                let (source, target) = edge.endpoints();
                (source == from).then(|| target.to_owned())
            })
            .collect()
    }
}

/// Flow validation entry point.
pub mod flow {
    use super::{BotError, FlowSpec, validate_flow};

    /// Validate every structural invariant required before a session can run.
    pub fn validate(spec: &FlowSpec) -> Result<(), BotError> {
        validate_flow(spec)
    }
}

/// Validate a flow's graph, variables, templates, and budget.
fn validate_flow(spec: &FlowSpec) -> Result<(), BotError> {
    if spec.nodes.is_empty() {
        return Err(BotError::MalformedFlow {
            cause: "flow declares no nodes".into(),
        });
    }
    if !spec.nodes.contains_key(&spec.entry) {
        return Err(BotError::InvalidTransitionTarget {
            from: "<entry>".into(),
            target: spec.entry.clone(),
        });
    }
    if spec.bounds.budget == 0 || spec.nodes.len() > spec.bounds.budget {
        return Err(BotError::FlowBudgetExceeded {
            steps: spec.nodes.len(),
            budget: spec.bounds.budget,
        });
    }

    validate_declarations(&spec.vars)?;
    validate_terminals(spec)?;

    let mut writers = BTreeSet::new();
    for (node_id, kind) in &spec.nodes {
        validate_node(spec, node_id, kind, &mut writers)?;
    }
    for name in spec.vars.keys() {
        if !writers.contains(name) {
            return Err(BotError::VariableNeverWritten { name: name.clone() });
        }
    }
    for edge in &spec.edges {
        let (from, to) = edge.endpoints();
        if !spec.nodes.contains_key(from) {
            return Err(BotError::InvalidTransitionTarget {
                from: from.to_owned(),
                target: from.to_owned(),
            });
        }
        check_target(&spec.nodes, from, to)?;
    }
    reject_unreachable(spec)
}

/// Validate variable declaration names and choice contents.
fn validate_declarations(vars: &BTreeMap<String, VarType>) -> Result<(), BotError> {
    for (name, kind) in vars {
        if !valid_identifier(name) {
            return Err(BotError::MalformedFlow {
                cause: format!("invalid variable name {name:?}"),
            });
        }
        match *kind {
            VarType::Choice(ref options)
                if options.is_empty() || has_duplicate_strings(options) =>
            {
                return Err(BotError::MalformedFlow {
                    cause: format!("choice variable {name:?} has invalid options"),
                });
            }
            _ => {}
        }
    }
    Ok(())
}

/// Validate one terminal declaration and its node kind.
fn validate_terminals(spec: &FlowSpec) -> Result<(), BotError> {
    for node_id in spec.terminals.keys() {
        let Some(kind) = spec.nodes.get(node_id) else {
            return Err(BotError::InvalidTransitionTarget {
                from: "<terminal>".into(),
                target: node_id.clone(),
            });
        };
        if !matches!(
            *kind,
            NodeKind::End | NodeKind::Handoff { .. } | NodeKind::Refer { .. }
        ) {
            return Err(BotError::MalformedFlow {
                cause: format!("terminal declaration {node_id:?} names a non-terminal node"),
            });
        }
    }
    Ok(())
}

/// Validate one node's references, variables, and text.
fn validate_node(
    spec: &FlowSpec,
    node_id: &str,
    kind: &NodeKind,
    writers: &mut BTreeSet<String>,
) -> Result<(), BotError> {
    match *kind {
        NodeKind::Say { ref text } => {
            validate_template(spec, node_id, text, "say.text")?;
            if spec.edge_targets(node_id).len() > 1 {
                return Err(BotError::MalformedFlow {
                    cause: format!("say node {node_id:?} has multiple continuations"),
                });
            }
            if spec.edge_targets(node_id).is_empty() && !spec.terminals.contains_key(node_id) {
                return Err(BotError::MissingTransition {
                    node: node_id.to_owned(),
                });
            }
        }
        NodeKind::Ask {
            ref var,
            ref options,
            ref routes,
        } => {
            validate_variable_reference(spec, node_id, var)?;
            writers.insert(var.clone());
            if options.is_empty() || has_duplicate_strings(options) {
                return Err(BotError::MalformedFlow {
                    cause: format!("ask node {node_id:?} has invalid options"),
                });
            }
            for option in options {
                let Some(target) = routes.get(option) else {
                    return Err(BotError::MissingAskRoute {
                        node: node_id.to_owned(),
                        option: option.clone(),
                    });
                };
                check_target(&spec.nodes, node_id, target)?;
            }
            for (option, target) in routes {
                if !options.iter().any(|candidate| candidate == option) {
                    return Err(BotError::MalformedFlow {
                        cause: format!("ask node {node_id:?} routes undeclared option {option:?}"),
                    });
                }
                check_target(&spec.nodes, node_id, target)?;
            }
        }
        NodeKind::Branch {
            ref var,
            ref when,
            ref then,
            ref otherwise,
        } => {
            validate_variable_reference(spec, node_id, var)?;
            validate_predicate(spec, node_id, when)?;
            check_target(&spec.nodes, node_id, then)?;
            check_target(&spec.nodes, node_id, otherwise)?;
        }
        NodeKind::Handoff { .. } => {}
        NodeKind::Refer { ref text, .. } => {
            validate_template(spec, node_id, text, "refer.text")?;
        }
        NodeKind::Route {
            ref dispatch,
            ref fallback,
        } => {
            check_target(&spec.nodes, node_id, dispatch)?;
            check_target(&spec.nodes, node_id, fallback)?;
        }
        NodeKind::End => {}
    }
    Ok(())
}

/// Validate a variable reference in a node.
fn validate_variable_reference(spec: &FlowSpec, node_id: &str, name: &str) -> Result<(), BotError> {
    if spec.vars.contains_key(name) {
        Ok(())
    } else {
        let _ = node_id;
        Err(BotError::UndeclaredVariable {
            name: name.to_owned(),
        })
    }
}

/// Validate a predicate's variable references and non-empty boolean lists.
fn validate_predicate(
    spec: &FlowSpec,
    node_id: &str,
    predicate: &Predicate,
) -> Result<(), BotError> {
    match *predicate {
        Predicate::Const(_) => Ok(()),
        Predicate::Eq(ref left, ref right)
        | Predicate::Ne(ref left, ref right)
        | Predicate::Lt(ref left, ref right)
        | Predicate::Le(ref left, ref right)
        | Predicate::Gt(ref left, ref right)
        | Predicate::Ge(ref left, ref right) => {
            validate_expr(spec, node_id, left)?;
            validate_expr(spec, node_id, right)
        }
        Predicate::And(ref items) | Predicate::Or(ref items) => {
            if items.is_empty() {
                return Err(BotError::MalformedFlow {
                    cause: format!("node {node_id:?} has an empty predicate"),
                });
            }
            for item in items {
                validate_predicate(spec, node_id, item)?;
            }
            Ok(())
        }
        Predicate::Not(ref inner) => validate_predicate(spec, node_id, inner),
    }
}

/// Validate one predicate expression.
fn validate_expr(spec: &FlowSpec, _node_id: &str, expression: &ValueExpr) -> Result<(), BotError> {
    if let ValueExpr::Var(ref name) = *expression
        && !spec.vars.contains_key(name)
    {
        return Err(BotError::UndeclaredVariable { name: name.clone() });
    }
    Ok(())
}

/// Validate interpolation markers and their declared names.
fn validate_template(
    spec: &FlowSpec,
    node_id: &str,
    template: &str,
    field: &'static str,
) -> Result<(), BotError> {
    let names = template_variables(template).map_err(|()| BotError::MalformedTemplate {
        node: node_id.to_owned(),
        field,
    })?;
    for name in names {
        if !spec.vars.contains_key(&name) {
            return Err(BotError::UndeclaredVariable { name });
        }
    }
    Ok(())
}

/// Validate that one target node exists.
fn check_target(
    nodes: &BTreeMap<NodeId, NodeKind>,
    from: &str,
    target: &str,
) -> Result<(), BotError> {
    if nodes.contains_key(target) {
        Ok(())
    } else {
        Err(BotError::InvalidTransitionTarget {
            from: from.to_owned(),
            target: target.to_owned(),
        })
    }
}

/// Reject a graph node that cannot be reached from the entry.
fn reject_unreachable(spec: &FlowSpec) -> Result<(), BotError> {
    let mut seen = BTreeSet::new();
    let mut pending = VecDeque::from([spec.entry.clone()]);
    while let Some(node_id) = pending.pop_front() {
        if !seen.insert(node_id.clone()) {
            continue;
        }
        let Some(kind) = spec.nodes.get(&node_id) else {
            continue;
        };
        for target in successor_targets(spec, &node_id, kind) {
            if !seen.contains(&target) {
                pending.push_back(target);
            }
        }
    }
    for node_id in spec.nodes.keys() {
        if !seen.contains(node_id) {
            return Err(BotError::UnreachableNode {
                node: node_id.clone(),
            });
        }
    }
    Ok(())
}

/// Collect every statically possible destination from one node.
fn successor_targets(spec: &FlowSpec, node_id: &str, kind: &NodeKind) -> Vec<NodeId> {
    let mut targets = spec.edge_targets(node_id);
    match *kind {
        NodeKind::Ask { ref routes, .. } => targets.extend(routes.values().cloned()),
        NodeKind::Branch {
            ref then,
            ref otherwise,
            ..
        } => {
            targets.push(then.clone());
            targets.push(otherwise.clone());
        }
        NodeKind::Route {
            ref dispatch,
            ref fallback,
        } => {
            targets.push(dispatch.clone());
            targets.push(fallback.clone());
        }
        NodeKind::Say { .. }
        | NodeKind::Handoff { .. }
        | NodeKind::Refer { .. }
        | NodeKind::End => {}
    }
    targets
}

/// Reject repeated strings in a finite declaration.
fn has_duplicate_strings(values: &[String]) -> bool {
    let mut seen = BTreeSet::new();
    values.iter().any(|value| !seen.insert(value))
}

/// Return whether a variable or node id uses the supported identifier shape.
fn valid_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

/// Find all interpolation names in a template.
fn template_variables(template: &str) -> Result<BTreeSet<String>, ()> {
    let mut names = BTreeSet::new();
    let mut cursor = 0;
    while let Some(relative_start) = template[cursor..].find("${") {
        let start = cursor.saturating_add(relative_start);
        let name_start = start.saturating_add(2);
        let Some(relative_end) = template[name_start..].find('}') else {
            return Err(());
        };
        let end = name_start.saturating_add(relative_end);
        let name = &template[name_start..end];
        if !valid_identifier(name) {
            return Err(());
        }
        names.insert(name.to_owned());
        cursor = end.saturating_add(1);
    }
    Ok(names)
}

/// Reject unknown tagged node kinds before serde turns them into a generic
/// malformed-document diagnostic.
fn reject_unknown_node_kinds(value: &JsonValue) -> Result<(), BotError> {
    let Some(nodes) = value.get("nodes").and_then(JsonValue::as_object) else {
        return Ok(());
    };
    for (node_id, node) in nodes {
        let Some(kind) = node.get("kind").and_then(JsonValue::as_str) else {
            continue;
        };
        if !matches!(
            kind,
            "say" | "ask" | "branch" | "handoff" | "refer" | "route" | "end"
        ) {
            return Err(BotError::UnknownNodeKind {
                node: node_id.clone(),
                kind: kind.to_owned(),
            });
        }
    }
    Ok(())
}

/// Named, typed variable declarations and their assigned values.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct VarScope {
    /// Declared variable types.
    declarations: BTreeMap<String, VarType>,
    /// Values assigned by accepted answers.
    values: BTreeMap<String, Value>,
}

impl VarScope {
    /// Construct an empty-value scope from declarations.
    pub fn new(declarations: BTreeMap<String, VarType>) -> Result<Self, BotError> {
        validate_declarations(&declarations)?;
        Ok(Self {
            declarations,
            values: BTreeMap::new(),
        })
    }

    /// Return the declared variable types.
    #[must_use]
    pub fn declarations(&self) -> &BTreeMap<String, VarType> {
        &self.declarations
    }

    /// Return all currently assigned values.
    #[must_use]
    pub fn values(&self) -> &BTreeMap<String, Value> {
        &self.values
    }

    /// Return one assigned value, if the variable has been written.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.values.get(name)
    }

    /// Assign a typed value after checking its declaration.
    pub fn set(&mut self, name: &str, value: Value) -> Result<(), BotError> {
        let Some(declared) = self.declarations.get(name) else {
            return Err(BotError::UndeclaredVariable {
                name: name.to_owned(),
            });
        };
        if value_matches_type(declared, &value) {
            self.values.insert(name.to_owned(), value);
            Ok(())
        } else {
            Err(BotError::VariableTypeMismatch {
                name: name.to_owned(),
            })
        }
    }

    /// Parse and assign an answer according to the variable declaration.
    pub fn set_from_answer(&mut self, name: &str, answer: &str) -> Result<(), BotError> {
        let Some(declared) = self.declarations.get(name) else {
            return Err(BotError::UndeclaredVariable {
                name: name.to_owned(),
            });
        };
        let value = match *declared {
            VarType::String => Value::String(answer.to_owned()),
            VarType::Integer => match answer.trim().parse::<i64>() {
                Ok(value) => Value::Integer(value),
                Err(_) => {
                    return Err(BotError::InvalidVariableValue {
                        variable: name.to_owned(),
                        value: answer.to_owned(),
                    });
                }
            },
            VarType::Boolean => match answer.trim().to_ascii_lowercase().as_str() {
                "true" | "yes" => Value::Boolean(true),
                "false" | "no" => Value::Boolean(false),
                _ => {
                    return Err(BotError::InvalidVariableValue {
                        variable: name.to_owned(),
                        value: answer.to_owned(),
                    });
                }
            },
            VarType::Choice(ref options) => {
                let Some(option) = options
                    .iter()
                    .find(|option| option.eq_ignore_ascii_case(answer.trim()))
                else {
                    return Err(BotError::InvalidVariableValue {
                        variable: name.to_owned(),
                        value: answer.to_owned(),
                    });
                };
                Value::Choice(option.clone())
            }
        };
        self.set(name, value)
    }

    /// Interpolate `${name}` markers using the assigned values.
    pub fn interpolate(&self, template: &str) -> Result<String, BotError> {
        TemplateInterpolator::new().interpolate(template, self)
    }
}

/// Check one scalar against its declared type.
fn value_matches_type(declared: &VarType, value: &Value) -> bool {
    match *declared {
        VarType::String => matches!(value, &Value::String(_)),
        VarType::Integer => matches!(value, &Value::Integer(_)),
        VarType::Boolean => matches!(value, &Value::Boolean(_)),
        VarType::Choice(ref options) => match *value {
            Value::Choice(ref value) => options.contains(value),
            _ => false,
        },
    }
}

/// Interpolation seam for replacing the template engine later.
pub trait Interpolate {
    /// Expand `${name}` markers using the supplied scope.
    fn interpolate(&self, template: &str, scope: &VarScope) -> Result<String, BotError>;
}

/// The shipped allocation-bounded interpolation implementation.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct TemplateInterpolator;

impl TemplateInterpolator {
    /// Construct the default interpolator.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Interpolate for TemplateInterpolator {
    fn interpolate(&self, template: &str, scope: &VarScope) -> Result<String, BotError> {
        let mut output = String::with_capacity(template.len());
        let mut cursor = 0;
        while let Some(relative_start) = template[cursor..].find("${") {
            let start = cursor.saturating_add(relative_start);
            output.push_str(&template[cursor..start]);
            let name_start = start.saturating_add(2);
            let Some(relative_end) = template[name_start..].find('}') else {
                return Err(BotError::MalformedTemplate {
                    node: "<runtime>".into(),
                    field: "template",
                });
            };
            let end = name_start.saturating_add(relative_end);
            let name = &template[name_start..end];
            if !valid_identifier(name) {
                return Err(BotError::MalformedTemplate {
                    node: "<runtime>".into(),
                    field: "template",
                });
            }
            let Some(value) = scope.get(name) else {
                return Err(BotError::VariableUnset {
                    name: name.to_owned(),
                });
            };
            output.push_str(&value.to_string());
            cursor = end.saturating_add(1);
        }
        output.push_str(&template[cursor..]);
        Ok(output)
    }
}

/// Free-text option resolver seam.
pub trait Resolver {
    /// Resolves an utterance against the candidate options.
    ///
    /// Returns a [`Verdict`] — the [`Resolution`] together with the policy and
    /// model that produced it — and never an `Option<usize>`. The two-way form
    /// collapses *nothing matched* and *several matched equally well* into one
    /// `None`, and the second is the dangerous one: an answer that fits two
    /// options as well as each other is reported as unrecognized, so the
    /// session re-asks the full list and the identical answer resolves the
    /// identical way forever. A caller that cannot see the tie cannot narrow.
    ///
    /// The provenance is part of the return value rather than a second call,
    /// because *this score came from that model under that rule* is one fact.
    /// A caller that has to ask a resolver afterwards what it just did is
    /// re-deriving a fact it was already handed, and the derivation is only as
    /// good as the resolver's willingness to keep answering the same way.
    fn resolve(&self, utterance: &str, options: &[String]) -> Verdict;
}

/// The identity of the decision policy that produced a verdict.
///
/// Two fields rather than one string, because they answer different questions.
/// `label` names the tier whose parameters were in force, so a reader knows
/// what kind of decision this was before reading any score. `revision` is a
/// digest over the parameters themselves, so a threshold that moved is a
/// different version **without anyone remembering to bump a number by hand**:
/// a hand-bumped version is a promise, and a receipt cannot rest on a promise.
///
/// The parameters are hashed by their bit patterns rather than by their decimal
/// renderings, so two thresholds differing in the last bit are two versions and
/// no rounding stands between the value used and the value recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields)]
#[non_exhaustive]
pub struct PolicyVersion {
    /// The tier whose policy was in force, such as `lexicon` or `semantic`.
    label: String,
    /// Content digest over the parameters in force, as lowercase hex.
    revision: String,
}

impl PolicyVersion {
    /// Versions a policy from its label and the parameters in force.
    ///
    /// The label is hashed in as well, so two tiers that happen to declare the
    /// same parameters still have different revisions: a revision alone
    /// identifies one policy, which is what lets it be compared without reading
    /// the label beside it.
    #[must_use]
    pub fn new(label: impl Into<String>, parameters: &[f64]) -> Self {
        let label = label.into();
        let mut hasher = Hasher::new();
        hasher.update(label.as_bytes());
        for parameter in parameters {
            hasher.update(&parameter.to_bits().to_le_bytes());
        }
        Self {
            revision: hasher.finalize().to_hex(),
            label,
        }
    }

    /// Returns the tier label.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Returns the content digest over the policy's parameters.
    #[must_use]
    pub fn revision(&self) -> &str {
        &self.revision
    }
}

impl fmt::Display for PolicyVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}", self.label, self.revision)
    }
}

/// What produced a verdict: the policy in force, and the model if there was one.
///
/// The two travel together because they are one fact. A receipt that named a
/// score and a tier but not the rule that scored it cannot answer the question
/// a score actually raises, which is *would this have decided the same way
/// yesterday*.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields)]
#[non_exhaustive]
pub struct Provenance {
    /// The decision policy in force when the verdict was reached.
    policy: PolicyVersion,
    /// The model consulted, when the verdict came from one.
    model: Option<EmbedderIdentity>,
}

impl Provenance {
    /// The deterministic tiers decided; no model was consulted.
    #[must_use]
    pub fn without_model(policy: PolicyVersion) -> Self {
        Self {
            policy,
            model: None,
        }
    }

    /// A model was consulted under a policy, whether or not it answered.
    ///
    /// A degraded verdict names its model deliberately. [`DegradedReason`] says
    /// the resolver never reached a verdict, and *which* model was unavailable
    /// is exactly what the repair needs; a record that dropped the identity on
    /// the failure path would describe the one case an operator has to act on
    /// as though no model had been involved.
    #[must_use]
    pub fn with_model(policy: PolicyVersion, model: EmbedderIdentity) -> Self {
        Self {
            policy,
            model: Some(model),
        }
    }

    /// Returns the policy in force.
    #[must_use]
    pub fn policy(&self) -> &PolicyVersion {
        &self.policy
    }

    /// Returns the model consulted, or `None` when none was.
    ///
    /// `None` is a finding rather than a missing value: the deterministic tiers
    /// answered, and naming a model here would claim a comparison that never
    /// happened.
    #[must_use]
    pub fn model(&self) -> Option<&EmbedderIdentity> {
        self.model.as_ref()
    }
}

/// A resolver's answer: the verdict, and what produced it.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Verdict {
    /// The three-way verdict.
    resolution: Resolution,
    /// The policy and model behind it.
    provenance: Provenance,
}

impl Verdict {
    /// Pairs a resolution with what produced it.
    #[must_use]
    pub const fn new(resolution: Resolution, provenance: Provenance) -> Self {
        Self {
            resolution,
            provenance,
        }
    }

    /// Returns the verdict.
    #[must_use]
    pub const fn resolution(&self) -> &Resolution {
        &self.resolution
    }

    /// Returns what produced the verdict.
    #[must_use]
    pub const fn provenance(&self) -> &Provenance {
        &self.provenance
    }

    /// Consumes the verdict and returns the resolution alone.
    #[must_use]
    pub fn into_resolution(self) -> Resolution {
        self.resolution
    }
}

/// Which match tier produced a resolution.
///
/// Reported rather than inferred, because it is what makes a wrong resolution
/// explicable and therefore repairable. An `Exact` miss is a learned alias
/// pointing at the wrong option; a `Phonetic` miss is the English bias of
/// [`crate::language::phonetic_key`]; a `Fuzzy` miss is a threshold. Three
/// different repairs, and a bare `usize` names none of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", rename_all = "snake_case")]
#[non_exhaustive]
pub enum MatchTier {
    /// The utterance matched exactly, directly or through a learned alias.
    Exact,
    /// The utterance and the option reduce to the same sound-alike key.
    Phonetic,
    /// The utterance and the option are lexically close.
    Fuzzy,
    /// The utterance and the option are close in a model's embedding space.
    Semantic,
}

/// Why a resolver could not reach a verdict.
///
/// A closed set rather than a string, so a caller can branch on the cause
/// without parsing prose, and so the set of ways a resolver can fail is stated
/// where a reviewer reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", rename_all = "snake_case")]
#[non_exhaustive]
pub enum DegradedReason {
    /// The semantic tier's embedder returned an error, or a vector of the
    /// wrong length.
    EmbedderUnavailable,
}

impl fmt::Display for DegradedReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::EmbedderUnavailable => formatter.write_str("the embedder is unavailable"),
        }
    }
}

/// The outcome of resolving an utterance against the candidate options.
///
/// Serializable, and not only for convenience: this is the shape a
/// [`DecisionReceipt`] carries verbatim, so a receipt cannot hold a verdict the
/// journal could not write down.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Resolution {
    /// One option matched and led the runner-up by the required margin.
    Resolved {
        /// Index of the selected option.
        index: usize,
        /// The tier that produced the match.
        tier: MatchTier,
        /// The match score.
        score: f64,
        /// The lead held over the runner-up.
        lead: f64,
    },
    /// Options matched but none led by the required margin.
    ///
    /// The caller narrows its re-ask to [`Self::Ambiguous::tied`] rather than
    /// repeating the full list. This is the state `Option<usize>` cannot
    /// express, and the one that made a repeated ambiguous answer a loop.
    Ambiguous {
        /// Every option index still in play, lowest first.
        tied: Vec<usize>,
        /// The highest score observed.
        score: f64,
    },
    /// No option matched.
    Absent {
        /// The highest score observed.
        best_score: f64,
    },
    /// The resolver could not reach a verdict at all.
    ///
    /// This is [`Self::Absent`]'s neighbour and not a spelling of it. `Absent`
    /// means the options were all considered and none fit: the person was not
    /// understood, and asking again in different words can help. `Degraded`
    /// means the resolver never got to consider them: a dependency it needs is
    /// unavailable, and asking the person again cannot help — the same
    /// utterance will degrade identically.
    ///
    /// Carries no `best_score`, deliberately. There is no score; a failed
    /// resolver observed no similarity, and reporting `0.0` would be a
    /// measurement that was never taken. That is the same two-valued record
    /// [`Self::Ambiguous`] exists to remove — *nothing matched* versus *may have
    /// matched, and we could not look* — arriving a fourth time.
    Degraded {
        /// The cause, for the caller to record and route on.
        reason: DegradedReason,
    },
}

/// Schema version of the [`DecisionReceipt`] this crate writes.
///
/// Bumped when a field's meaning changes or a field is removed. A reader that
/// finds a version it does not know can refuse the record rather than
/// misinterpret it, which is the only reason to version it at all: a receipt
/// outlives the code that wrote it, and the code that reads it is not always
/// the same revision.
pub const RECEIPT_VERSION: u32 = 1;

/// One structured record of a decision, written as part of the transition it
/// describes.
///
/// The receipt exists because a transcript cannot be the audit record. A
/// transcript is text: it says a person said something and the session moved,
/// and it cannot say which option was considered, on whose authority the
/// comparison was made, or whether a score of nothing meant *no similarity* or
/// *no measurement*. Those are the questions a run has to be able to answer
/// afterwards, and each field here is one of them:
///
/// - `session` and `flow` — which session, running which revision of the flow.
///   A receipt that names a node but not the document it came from cannot be
///   read against the flow a reader has on disk.
/// - `node` and `options` — where the decision was taken, and the *ordered*
///   candidate set it was taken against. The option list is digested rather
///   than counted, because a reordering is a different question with a
///   different right answer.
/// - `resolution` — the verdict verbatim, including the tier. A score is not
///   comparable across tiers, so a receipt holding a score without its tier
///   holds an unreadable number.
/// - `provenance` — the policy in force, and the model when one was consulted.
/// - `selected` and `route` — what was chosen, by the option's own text, and
///   where the session went. The text is what survives a reordering; the route
///   is what the session actually did.
///
/// Fields that were not measured are absent rather than zero. A [`Resolution`]
/// carries a score where a score was observed, and a [`Resolution::Degraded`]
/// carries none at all, which is the difference between *the options were
/// compared and nothing was close* and *nothing was compared*.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields)]
#[non_exhaustive]
pub struct DecisionReceipt {
    /// Schema version; see [`RECEIPT_VERSION`].
    version: u32,
    /// The session the decision was taken in.
    session: SessionId,
    /// Digest of the flow document the session is running.
    flow: String,
    /// The ask node the decision was taken at.
    node: NodeId,
    /// Digest of the ordered candidate options the verdict was reached against.
    options: String,
    /// The verdict, verbatim.
    resolution: Resolution,
    /// The policy in force, and the model when one was consulted.
    provenance: Provenance,
    /// The selected option's own text, when the verdict selected one.
    ///
    /// The text rather than the index: an index is a position in a list that
    /// can be reordered, and the text is the only thing that still names the
    /// choice once it is.
    selected: Option<String>,
    /// The node the accepted transition moved to, when it moved.
    route: Option<NodeId>,
}

impl DecisionReceipt {
    /// Returns the schema version.
    #[must_use]
    pub const fn version(&self) -> u32 {
        self.version
    }

    /// Returns the session identity.
    #[must_use]
    pub fn session(&self) -> &SessionId {
        &self.session
    }

    /// Returns the flow revision digest.
    #[must_use]
    pub fn flow(&self) -> &str {
        &self.flow
    }

    /// Returns the ask node the decision was taken at.
    #[must_use]
    pub fn node(&self) -> &str {
        &self.node
    }

    /// Returns the digest of the ordered candidate options.
    #[must_use]
    pub fn options(&self) -> &str {
        &self.options
    }

    /// Returns the verdict.
    #[must_use]
    pub fn resolution(&self) -> &Resolution {
        &self.resolution
    }

    /// Returns the policy and model behind the verdict.
    #[must_use]
    pub fn provenance(&self) -> &Provenance {
        &self.provenance
    }

    /// Returns the selected option's text, when the verdict selected one.
    #[must_use]
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// Returns the node the accepted transition moved to, when it moved.
    #[must_use]
    pub fn route(&self) -> Option<&str> {
        self.route.as_deref()
    }
}

/// Where a journal accepted a decision receipt.
///
/// Reported by the sink rather than assumed by the session, because the two are
/// different facts and only the sink knows which one is true. A receipt held in
/// memory is evidence for the lifetime of the process; a receipt held durably
/// is evidence after it. A run that could not tell them apart would put the
/// second claim on the first record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReceiptAcceptance {
    /// Held only in this process, and gone when it ends.
    InMemory,
    /// Written to a sink that outlives the process.
    Durable,
}

/// The failure a journal reports when it cannot accept a receipt.
///
/// A named sink and its own description rather than a closed enum of causes:
/// the failure belongs to the sink, which owns its vocabulary — a full disk, a
/// refused connection, a closed file — and this crate cannot enumerate another
/// component's failures without inventing a taxonomy the sink would then have
/// to translate into. What is fixed here is that the failure crosses the seam
/// as a value the session must handle, rather than a diagnostic it may print
/// and continue past.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct JournalError {
    /// The sink that refused the receipt.
    sink: String,
    /// The sink's own description of the failure.
    cause: String,
}

impl JournalError {
    /// Records a sink's failure to accept a receipt.
    #[must_use]
    pub fn new(sink: impl Into<String>, cause: impl Into<String>) -> Self {
        Self {
            sink: sink.into(),
            cause: cause.into(),
        }
    }

    /// Returns the sink that refused.
    #[must_use]
    pub fn sink(&self) -> &str {
        &self.sink
    }

    /// Returns the sink's description of the failure.
    #[must_use]
    pub fn cause(&self) -> &str {
        &self.cause
    }
}

impl fmt::Display for JournalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "journal {} refused a decision receipt: {}",
            self.sink, self.cause
        )
    }
}

impl std::error::Error for JournalError {}

/// One receipt, and the way the journal accepted it.
///
/// The pair rather than a receipt carrying its own acceptance: the receipt is
/// what both sides hold, and the acceptance is the journal's answer about it.
/// Folding the answer into the record the journal was already given would mean
/// the two copies differ on the field that matters most.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct RecordedDecision {
    /// The receipt the journal accepted.
    receipt: DecisionReceipt,
    /// Where the journal said it accepted it.
    acceptance: ReceiptAcceptance,
}

impl RecordedDecision {
    /// Returns the receipt.
    #[must_use]
    pub fn receipt(&self) -> &DecisionReceipt {
        &self.receipt
    }

    /// Returns where the journal accepted it.
    #[must_use]
    pub const fn acceptance(&self) -> ReceiptAcceptance {
        self.acceptance
    }
}

/// Journal seam for session path records.
pub trait Journal {
    /// Receive a path node, role label, and rendered text record.
    fn record(&mut self, path_node: &str, role: &str, text: &str);

    /// Accept one decision receipt, reporting where it was accepted.
    ///
    /// Fallible, and required rather than defaulted. [`Self::record`] cannot
    /// report persistence failure, and neither could a defaulted version of
    /// this method: a sink that has to do nothing to look successful is a sink
    /// that silently drops every receipt, and the session would report an
    /// accepted transition whose audit record does not exist. Requiring it
    /// makes every implementation state what it does with a receipt — including
    /// [`MemoryJournal`], which says `InMemory` in as many words.
    ///
    /// `Err` aborts the transition. A receipt is not a log line written beside
    /// the work; it is part of accepting the answer, so a session that cannot
    /// record the decision has not taken it.
    fn record_decision(
        &mut self,
        receipt: &DecisionReceipt,
    ) -> Result<ReceiptAcceptance, JournalError>;
}

/// One in-memory transcript record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields)]
#[non_exhaustive]
pub struct TranscriptEntry {
    /// Node id at which the record was produced.
    path_node: String,
    /// Role label, such as `assistant` or `user`.
    role: String,
    /// Rendered record text.
    text: String,
}

impl TranscriptEntry {
    /// Return the path node id.
    #[must_use]
    pub fn path_node(&self) -> &str {
        &self.path_node
    }

    /// Return the role label.
    #[must_use]
    pub fn role(&self) -> &str {
        &self.role
    }

    /// Return the rendered record text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Bounded-by-session in-memory journal implementation.
///
/// `PartialEq` but not `Eq`: the receipts it holds carry the scores a verdict
/// was reached on, and `f64` is not `Eq`. That is a property of the record
/// rather than a gap — `NaN` is not a score any tier produces, and `DecisionReceipt`
/// has no `Eq` for the same reason.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct MemoryJournal {
    /// Records in insertion order.
    records: Vec<TranscriptEntry>,
    /// Decision receipts in insertion order.
    receipts: Vec<DecisionReceipt>,
}

impl MemoryJournal {
    /// Construct an empty journal.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            records: Vec::new(),
            receipts: Vec::new(),
        }
    }

    /// Return records in insertion order.
    #[must_use]
    pub fn records(&self) -> &[TranscriptEntry] {
        &self.records
    }

    /// Return decision receipts in insertion order.
    #[must_use]
    pub fn receipts(&self) -> &[DecisionReceipt] {
        &self.receipts
    }

    /// Consume the journal and return its records.
    #[must_use]
    pub fn into_records(self) -> Vec<TranscriptEntry> {
        self.records
    }

    /// Consume the journal and return its decision receipts.
    #[must_use]
    pub fn into_receipts(self) -> Vec<DecisionReceipt> {
        self.receipts
    }
}

impl Journal for MemoryJournal {
    fn record(&mut self, path_node: &str, role: &str, text: &str) {
        self.records.push(TranscriptEntry {
            path_node: path_node.to_owned(),
            role: role.to_owned(),
            text: text.to_owned(),
        });
    }

    /// Accepts a receipt, and says so: `InMemory` is the honest answer for a
    /// sink whose whole storage is a `Vec` in this process. A caller that needs
    /// a durable record has to install a journal that says `Durable`, and can
    /// see from the receipt that this one did not.
    fn record_decision(
        &mut self,
        receipt: &DecisionReceipt,
    ) -> Result<ReceiptAcceptance, JournalError> {
        self.receipts.push(receipt.clone());
        Ok(ReceiptAcceptance::InMemory)
    }
}

/// Public identity of one in-memory session.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde")]
#[non_exhaustive]
pub struct SessionId(String);

impl SessionId {
    /// Construct an id from its stable string representation.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Return the stable string representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for SessionId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for SessionId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Synchronous cursor over one validated flow and its conversation state.
#[non_exhaustive]
pub struct Session {
    /// Stable session identity.
    id: SessionId,
    /// Validated flow document owned by this session.
    flow: FlowSpec,
    /// Digest of the flow document, computed once at construction.
    flow_revision: String,
    /// Current node, or `None` after terminal completion.
    current: Option<NodeId>,
    /// Typed variable scope.
    scope: VarScope,
    /// Nodes executed in order, including repeated cycle visits.
    visited: Vec<NodeId>,
    /// Transcript mirrored to the journal seam.
    transcript: Vec<TranscriptEntry>,
    /// Pluggable free-text resolver.
    resolver: Box<dyn Resolver>,
    /// Pluggable record sink.
    journal: Box<dyn Journal>,
    /// Decision receipts in insertion order, with how each was accepted.
    decisions: Vec<RecordedDecision>,
    /// Terminal outcome, if the cursor has stopped.
    terminal: Option<Terminal>,
    /// Number of charged execution and answer steps.
    steps: usize,
    /// Last accepted answer, used by [`NodeKind::Route`].
    last_utterance: Option<String>,
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
        flow::validate(&flow)?;
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
        };
        session.drive()?;
        Ok(session)
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
            return Err(BotError::SessionTerminated);
        }
        let Some(node_id) = self.current.clone() else {
            return Err(BotError::SessionNotAwaitingAnswer);
        };
        let Some(NodeKind::Ask {
            var,
            options,
            routes,
        }) = self.flow.node(&node_id).cloned()
        else {
            return Err(BotError::SessionNotAwaitingAnswer);
        };
        self.charge_step()?;
        let verdict = self.resolver.resolve(utterance, &options);
        match verdict.resolution().clone() {
            Resolution::Resolved { index, .. } => {
                let Some(option) = options.get(index) else {
                    return Err(BotError::ResolverReturnedInvalidOption { node: node_id });
                };
                let Some(target) = routes.get(option).cloned() else {
                    return Err(BotError::MissingAskRoute {
                        node: node_id,
                        option: option.clone(),
                    });
                };
                self.write_receipt(
                    &node_id,
                    &options,
                    &verdict,
                    Some(option.clone()),
                    Some(&target),
                )?;
                self.scope.set_from_answer(&var, option)?;
                self.last_utterance = Some(utterance.to_owned());
                self.record(&node_id, "user", utterance);
                self.current = Some(target);
                self.drive()
            }
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
                self.write_receipt(&node_id, &options, &verdict, None, None)?;
                self.record(&node_id, "user", utterance);
                if narrowed.len() >= 2 {
                    self.record_prompt(&node_id, &narrowed);
                } else {
                    self.record_prompt(&node_id, &options);
                }
                Ok(())
            }
            Resolution::Absent { .. } => {
                self.write_receipt(&node_id, &options, &verdict, None, None)?;
                self.record(&node_id, "user", utterance);
                self.record_prompt(&node_id, &options);
                Ok(())
            }
            Resolution::Degraded { reason } => {
                // Re-ask, as for `Absent`, but record the cause under its own
                // role: a transcript that renders a degraded re-ask exactly as
                // an unclear one is how an operator concludes the person was
                // being difficult while the embedder was down.
                self.write_receipt(&node_id, &options, &verdict, None, None)?;
                self.record(&node_id, "user", utterance);
                self.record_degraded(&node_id, reason);
                self.record_prompt(&node_id, &options);
                Ok(())
            }
        }
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
            return Err(BotError::SessionBudgetExceeded {
                steps: attempted,
                budget: self.flow.bounds.budget,
            });
        }
        self.steps = attempted;
        Ok(())
    }

    /// Execute deterministic nodes until an ask or terminal is reached.
    fn drive(&mut self) -> Result<(), BotError> {
        loop {
            let Some(node_id) = self.current.clone() else {
                return Ok(());
            };
            self.charge_step()?;
            self.visited.push(node_id.clone());
            let Some(kind) = self.flow.node(&node_id).cloned() else {
                return Err(BotError::InvalidTransitionTarget {
                    from: "<session>".into(),
                    target: node_id,
                });
            };
            match kind {
                NodeKind::Say { text } => {
                    let rendered = TemplateInterpolator::new().interpolate(&text, &self.scope)?;
                    self.record(&node_id, "assistant", &rendered);
                    let Some(target) = self.flow.edge_targets(&node_id).into_iter().next() else {
                        return Err(BotError::MissingTransition { node: node_id });
                    };
                    self.current = Some(target);
                }
                NodeKind::Ask { options, .. } => {
                    self.record_prompt(&node_id, &options);
                    return Ok(());
                }
                NodeKind::Branch {
                    when,
                    then,
                    otherwise,
                    ..
                } => {
                    self.current = Some(if when.evaluate(&self.scope)? {
                        then
                    } else {
                        otherwise
                    });
                }
                NodeKind::Handoff { target } => {
                    self.terminal = Some(Terminal::HandedOff { target });
                    self.current = None;
                }
                NodeKind::Refer { target, text } => {
                    let rendered = TemplateInterpolator::new().interpolate(&text, &self.scope)?;
                    self.record(&node_id, "assistant", &rendered);
                    self.terminal = Some(Terminal::Referred { target });
                    self.current = None;
                }
                NodeKind::Route { dispatch, fallback } => {
                    self.current = Some(if self.last_utterance.is_some() {
                        dispatch
                    } else {
                        fallback
                    });
                }
                NodeKind::End => {
                    self.terminal = Some(
                        self.flow
                            .terminals
                            .get(&node_id)
                            .cloned()
                            .unwrap_or(Terminal::Completed),
                    );
                    self.current = None;
                }
            }
        }
    }

    /// Append one transcript record and forward it to the journal seam.
    fn record(&mut self, node_id: &str, role: &str, text: &str) {
        self.journal.record(node_id, role, text);
        self.transcript.push(TranscriptEntry {
            path_node: node_id.to_owned(),
            role: role.to_owned(),
            text: text.to_owned(),
        });
    }

    /// Record the standard prompt for an ask node.
    fn record_prompt(&mut self, node_id: &str, options: &[String]) {
        let prompt = format!("Choose one: {}", options.join(", "));
        self.record(node_id, "assistant", &prompt);
    }

    /// Records that the resolver could not reach a verdict.
    ///
    /// A distinct role rather than a second assistant prompt, because the two
    /// failures need different repairs and a transcript is where that is
    /// decided: an absent resolution says the person's words did not fit the
    /// options, and a degraded one says the resolver never got to compare them.
    fn record_degraded(&mut self, node_id: &str, reason: DegradedReason) {
        let text = format!("Resolver unavailable: {reason}");
        self.record(node_id, "resolver-degraded", &text);
    }
}

/// Digest a flow document so a receipt names the revision it was decided under.
///
/// The document is digested through its JSON rendering rather than field by
/// field, so a field added to [`FlowSpec`] later is covered without anyone
/// remembering to extend a walk. `FlowSpec` holds `BTreeMap`s, so the rendering
/// is canonical: two flows that differ only in the order their maps were built
/// from digest identically, which is what makes this a *revision* rather than a
/// serialization accident.
fn revision_of(flow: &FlowSpec) -> Result<String, BotError> {
    crate::json::to_string(flow)
        .map(|rendered| lgwks_std::hash::blake3(rendered.as_bytes()).to_hex())
        .map_err(|error| BotError::MalformedFlow {
            cause: error.to_string().escape_debug().to_string(),
        })
}

/// Digest an ordered candidate list, so a reordering is a different identity.
///
/// Each option is length-prefixed rather than separated, because an option is
/// free text and could contain any separator: a digest that concatenated
/// `["a", "b"]` and `["ab"]` to the same bytes would report two different
/// questions as one identity, and the whole point of the field is to tell
/// whether the question changed.
fn options_revision(options: &[String]) -> String {
    let mut hasher = Hasher::new();
    for option in options {
        hasher.update(&option.len().to_le_bytes());
        hasher.update(option.as_bytes());
    }
    hasher.finalize().to_hex()
}

/// Resolve one scalar expression from a variable scope.
fn resolve_expr(expression: &ValueExpr, scope: &VarScope) -> Result<Value, BotError> {
    match *expression {
        ValueExpr::Literal(ref value) => Ok(value.clone()),
        ValueExpr::Var(ref name) => scope
            .get(name)
            .cloned()
            .ok_or_else(|| BotError::VariableUnset { name: name.clone() }),
    }
}

/// Compare two scalar expressions with a typed ordering operation.
fn compare_expr<F>(
    left: &ValueExpr,
    right: &ValueExpr,
    scope: &VarScope,
    predicate: F,
) -> Result<bool, BotError>
where
    F: FnOnce(std::cmp::Ordering) -> bool,
{
    let left = resolve_expr(left, scope)?;
    let right = resolve_expr(right, scope)?;
    let ordering = match (left, right) {
        (Value::String(left), Value::String(right))
        | (Value::Choice(left), Value::Choice(right)) => left.cmp(&right),
        (Value::Integer(left), Value::Integer(right)) => left.cmp(&right),
        (Value::Boolean(left), Value::Boolean(right)) => left.cmp(&right),
        _ => return Err(BotError::PredicateTypeMismatch),
    };
    Ok(predicate(ordering))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Return a small validated flow used by focused unit tests.
    fn end_flow() -> Result<FlowSpec, BotError> {
        FlowSpec::new(
            BTreeMap::new(),
            "end",
            BTreeMap::from([(String::from("end"), NodeKind::End)]),
            Vec::new(),
            BTreeMap::new(),
            FlowBounds::new(4),
        )
    }

    #[test]
    fn predicates_compare_typed_values() -> Result<(), BotError> {
        let mut declarations = BTreeMap::new();
        declarations.insert(String::from("answer"), VarType::Integer);
        let mut scope = VarScope::new(declarations)?;
        scope.set_from_answer("answer", "7")?;
        let predicate = Predicate::Gt(
            ValueExpr::Var(String::from("answer")),
            ValueExpr::Literal(Value::Integer(3)),
        );
        assert!(predicate.evaluate(&scope)?);
        Ok(())
    }

    #[test]
    fn templates_use_scope_values() -> Result<(), BotError> {
        let mut declarations = BTreeMap::new();
        declarations.insert(String::from("name"), VarType::String);
        let mut scope = VarScope::new(declarations)?;
        scope.set_from_answer("name", "Ada")?;
        let rendered = scope.interpolate("Hello ${name}")?;
        assert_eq!(rendered, "Hello Ada");
        Ok(())
    }

    #[test]
    fn the_default_resolver_has_an_explicit_unrecognized_case() {
        let options = vec![String::from("yes"), String::from("no")];
        let resolver = crate::language::LanguageResolver::new();
        assert!(
            matches!(
                resolver.resolve("maybe", &options).into_resolution(),
                Resolution::Absent { .. }
            ),
            "an unrecognized answer is Absent, which is not the same as Ambiguous"
        );
        assert_eq!(
            resolver.resolve("yes", &options).into_resolution(),
            Resolution::Resolved {
                index: 0,
                tier: MatchTier::Exact,
                score: 1.0,
                lead: 1.0,
            }
        );
    }

    #[test]
    fn end_session_stops_at_terminal() -> Result<(), BotError> {
        let session = Session::new("test", end_flow()?)?;
        assert_eq!(session.current(), None);
        assert_eq!(session.terminal(), Some(&Terminal::Completed));
        Ok(())
    }

    /// The four terminals, each paired with the disposition it must report.
    fn terminals() -> [(Terminal, Disposition); 4] {
        [
            (Terminal::Completed, Disposition::Completed),
            (
                Terminal::Referred {
                    target: String::from("tier-2"),
                },
                Disposition::Referred,
            ),
            (
                Terminal::HandedOff {
                    target: String::from("agent"),
                },
                Disposition::HandedOff,
            ),
            (
                Terminal::Refused {
                    reason: String::from("out of scope"),
                },
                Disposition::Refused,
            ),
        ]
    }

    #[test]
    fn disposition_and_effect_knowledge_are_reported_independently() {
        // The cross-product: every terminal against every effect history. Each
        // axis comes back unchanged whatever the other says, because they are
        // two facts rather than two arms of one choice.
        let histories = [
            EffectLedger::new(0, 0), // nothing attempted
            EffectLedger::new(1, 0), // one effect, confirmed
            EffectLedger::new(0, 1), // one attempted, occurrence unknown
            EffectLedger::new(2, 1), // confirmed and unknown at once
            EffectLedger::new(3, 2),
        ];

        for (terminal, expected) in terminals() {
            for ledger in histories {
                let report = terminal.outcome(ledger);
                assert_eq!(
                    report.disposition(),
                    expected,
                    "no effect history may change what the workflow decided"
                );
                assert_eq!(
                    report.effects(),
                    ledger,
                    "no disposition may change what is known about the effects"
                );
                assert_eq!(
                    report.needs_reconciliation(),
                    ledger.unsettled() > 0,
                    "reconciliation follows the unsettled count and nothing else"
                );
            }
        }
    }

    #[test]
    fn a_refusal_no_longer_erases_an_unsettled_effect() {
        // The counterexample from the review, kept as a regression. Two runs
        // refused for the same reason, differing only in whether an earlier
        // effect was left in doubt. The old classifier returned `Failure` for
        // both, which is the one answer that cannot be acted on: only the
        // second one needs an effect reconciled before anyone retries.
        let refused = Terminal::Refused {
            reason: String::from("later step denied"),
        };

        let settled = refused.outcome(EffectLedger::new(0, 0));
        let unknown = refused.outcome(EffectLedger::new(0, 1));

        assert_eq!(
            settled.disposition(),
            unknown.disposition(),
            "both runs were refused, and the report says so in both cases"
        );
        assert!(
            !settled.needs_reconciliation(),
            "nothing was attempted, so there is nothing to reconcile"
        );
        assert!(
            unknown.needs_reconciliation(),
            "an earlier effect may be live; a refusal does not settle it"
        );
        assert_ne!(
            settled, unknown,
            "histories differing on a possibly-live effect must stay distinguishable"
        );
    }

    #[test]
    fn an_unsettled_effect_does_not_imply_a_confirmed_one() {
        // The second inference error the review named. `unsettled > 0` was
        // documented as "part of the output exists", which does not follow: one
        // attempted effect whose response was lost may have produced none, and
        // the report has to say what is known rather than the flattering half.
        let report = Terminal::Completed.outcome(EffectLedger::new(0, 1));

        assert_eq!(
            report.effects().confirmed(),
            0,
            "nothing was confirmed, so nothing may be claimed to exist"
        );
        assert_eq!(
            report.effects().unsettled(),
            1,
            "the unknown effect is still counted"
        );
        assert!(
            report.needs_reconciliation(),
            "the run needs reconciling, which is not the same statement as having \
             partly succeeded"
        );
    }

    #[test]
    fn an_indeterminate_effect_is_what_puts_a_run_in_doubt() {
        // The law tying the two halves of this change together: the error says
        // an effect may have happened, and that is exactly what puts the run in
        // doubt. Neither half is reachable in practice without the other.
        let errors = [BotError::EffectIndeterminate {
            domain: String::from("gh::merge"),
            cause: String::from("request timed out"),
        }];
        let unsettled = errors
            .iter()
            .filter(|error| matches!(**error, BotError::EffectIndeterminate { .. }))
            .count();

        assert_eq!(unsettled, 1, "the run left one effect unsettled");
        let report = Terminal::Completed.outcome(EffectLedger::new(0, unsettled));
        assert_eq!(
            report.disposition(),
            Disposition::Completed,
            "an unsettled effect does not change what the workflow decided"
        );
        assert!(
            report.needs_reconciliation(),
            "an indeterminate effect is what puts a completed run in doubt"
        );
    }

    #[test]
    fn a_report_round_trips_through_json() -> Result<(), Box<dyn std::error::Error>> {
        // A report is written to a journal and read back by whoever reconciles
        // it, so it has to survive the trip with both facts intact.
        let report = Terminal::Refused {
            reason: String::from("denied"),
        }
        .outcome(EffectLedger::new(2, 1));

        let json = crate::json::to_string(&report)?;
        let back: Outcome = crate::json::from_str(&json)?;

        assert_eq!(
            back, report,
            "a report has to survive the journal it is written to"
        );
        assert_eq!(
            back.disposition(),
            Disposition::Refused,
            "the refusal survives the round trip"
        );
        assert!(back.needs_reconciliation(), "so does the unresolved effect");
        Ok(())
    }
}
