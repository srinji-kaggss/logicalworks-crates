//! Flow validation: the structural and declaration checks a document passes at load. Split from `session.rs` (#526).

use super::{
    AnswerDomain, BTreeMap, BTreeSet, BotError, CompiledTemplate, FlowSpec, NodeId, NodeKind,
    Predicate, ResourceAxis, ResourceLimits, TemplatePart, Value, ValueExpr, VarScope, VarType,
    VecDeque, decode_integer,
};

/// Validates one `branch` node: its variable, its predicate and both edges.
///
/// A helper so every kind's arm in [`validate_node`] is one call. The order
/// inside is the order a refusal should name things in: the variable before the
/// predicate that reads it, and the predicate before either edge it chooses
/// between.
fn validate_branch_node(
    spec: &FlowSpec,
    node_id: &str,
    var: &str,
    when: &Predicate,
    then: &str,
    otherwise: &str,
) -> Result<(), BotError> {
    validate_variable_reference(spec, node_id, var)?;
    validate_predicate(spec, node_id, when)?;
    check_edge_pair(&spec.nodes, node_id, then, otherwise)
}

/// Validates one `refer` node: its template, and that it reaches a terminal.
///
/// A helper for the same reason as the other kinds. A `refer` hands the
/// conversation to someone else, so its target has to be a place the flow can
/// actually end -- validated here rather than discovered when the session had
/// already given the person away.
fn validate_refer_node(
    spec: &FlowSpec,
    node_id: &str,
    text: &str,
    limits: ResourceLimits,
) -> Result<(), BotError> {
    validate_template(spec, node_id, text, "refer.text", limits)?;
    for target in spec.edge_targets(node_id) {
        check_target(&spec.nodes, node_id, &target)?;
    }
    Ok(())
}

/// Validates one `say` node: its template, and that it has exactly one way out.
///
/// A helper so every kind's arm in [`validate_node`] is one call. A `say` node
/// that continues along two edges is ambiguous at run time -- which one the
/// driver follows is not written down anywhere -- and a `say` node with no
/// continuation and no terminal leaves the conversation with nowhere to go, so
/// both are refused at load rather than at the moment a person is waiting.
fn validate_say_node(
    spec: &FlowSpec,
    node_id: &str,
    text: &str,
    limits: ResourceLimits,
) -> Result<(), BotError> {
    validate_template(spec, node_id, text, "say.text", limits)?;
    if spec.edge_targets(node_id).len() > 1 {
        let refusal = Err(BotError::MalformedFlow {
            cause: format!("say node {node_id:?} has multiple continuations"),
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "validate_say_node: returning an error to the caller");
        return refusal;
    }
    if spec.edge_targets(node_id).is_empty() && !spec.terminals.contains_key(node_id) {
        let refusal = Err(BotError::MissingTransition {
            node: node_id.to_owned(),
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "validate_say_node: returning an error to the caller");
        return refusal;
    }
    Ok(())
}

/// Validates one `ask` node: its variable, its options and its routes.
///
/// A helper rather than the arm inline: as one block the arm carried the
/// variable lookup, the option decode, the byte check and the two route passes,
/// so `validate_node` read as a list of kinds when what it needed to say was
/// which parts of an `ask` node are checked and in what order -- decode with
/// the runtime's own decoder first, then the session's own ceiling, then the
/// routes, so a refusal names the earliest thing wrong rather than an arbitrary
/// one.
fn validate_ask_node(
    spec: &FlowSpec,
    node_id: &str,
    var: &str,
    options: &[String],
    routes: &BTreeMap<String, NodeId>,
    limits: ResourceLimits,
    writers: &mut BTreeSet<String>,
) -> Result<(), BotError> {
    let declared = declared_variable(spec, var)?;
    writers.insert(var.to_owned());
    if options.is_empty() || has_duplicate_strings(options) {
        let refusal = Err(BotError::MalformedFlow {
            cause: format!("ask node {node_id:?} has invalid options"),
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "validate_ask_node: returning an error to the caller");
        return refusal;
    }
    // Distinct strings are not distinct answers. Two options the answer
    // policy cannot tell apart are one option the person cannot choose
    // between, and the resolver can only ever report the tie — so the
    // authoring mistake is refused here, where it names an option,
    // rather than at run time, where it names none.
    if let Some((first, second)) = colliding_options(options, answer_domain_of(spec, var)) {
        let refusal = Err(BotError::MalformedFlow {
            cause: format!(
                "ask node {node_id:?} offers {first:?} and {second:?} as the same \
                         {} answer",
                answer_domain_of(spec, var)
            ),
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "validate_ask_node: returning an error to the caller");
        return refusal;
    }
    for option in options {
        let Some(target) = routes.get(option) else {
            let refusal = Err(BotError::MissingAskRoute {
                node: node_id.to_owned(),
                option: option.clone(),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "validate_ask_node: returning an error to the caller");
            return refusal;
        };
        check_target(&spec.nodes, node_id, target)?;
        // Every candidate is decoded with the same decoder the runtime
        // assigns with, so "the flow asks a question the variable
        // cannot hold an answer to" is a load-time refusal rather than
        // a conversation the person cannot complete.
        let value = match declared.decode_answer(option) {
            Ok(value) => value,
            Err(rejection) => {
                let refusal = Err(BotError::AskOptionNotAssignable {
                    node: node_id.to_owned(),
                    variable: var.to_owned(),
                    option: option.clone(),
                    expected: declared.label(),
                    cause: rejection.to_string(),
                });
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "validate_ask_node: returning an error to the caller");
                return refusal;
            }
        };
        // Then that the value fits the session that will store it.
        // "The candidate cannot be held" and "the candidate cannot be
        // held *here*" are different statements, and both are knowable
        // while the document is loading: the second one is a session
        // whose value ceiling is below the option it asked for, and
        // deferring it to the person's answer would charge them a step
        // for an operator's configuration.
        let value_bytes = limits.get(ResourceAxis::Value);
        if value.rendered_bytes() > value_bytes {
            let refusal = Err(BotError::AskOptionTooLarge {
                node: node_id.to_owned(),
                variable: var.to_owned(),
                option: option.clone(),
                bytes: value.rendered_bytes(),
                limit: value_bytes,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "validate_ask_node: returning an error to the caller");
            return refusal;
        }
    }
    for (option, target) in routes {
        if !options.iter().any(|candidate| candidate == option) {
            let refusal = Err(BotError::MalformedFlow {
                cause: format!("ask node {node_id:?} routes undeclared option {option:?}"),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "validate_ask_node: returning an error to the caller");
            return refusal;
        }
        check_target(&spec.nodes, node_id, target)?;
    }
    Ok(())
}

/// Checks both edges of a branch, in the order the branch names them.
///
/// A helper for the pair rather than two calls: the branch's whole contract is
/// that both targets resolve, and a reader checking one edge had no reason to
/// look for the other three lines below it.
fn check_edge_pair(
    nodes: &BTreeMap<NodeId, NodeKind>,
    node_id: &str,
    first: &str,
    second: &str,
) -> Result<(), BotError> {
    check_target(nodes, node_id, first)?;
    check_target(nodes, node_id, second)
}

/// Validate a flow's graph, variables, templates, and budget against the
/// operator's shipped byte ceilings.
pub(super) fn validate_flow(spec: &FlowSpec) -> Result<(), BotError> {
    validate_flow_within(spec, ResourceLimits::shipped())
}

/// Validate a flow's graph, variables, templates, budget, and byte ceilings.
///
/// The ceilings are a parameter because two callers enforce different ones:
/// [`FlowSpec::validate`] applies the shipped ceilings a document must satisfy
/// on its own, and a session applies the effective ceilings it will run under —
/// its own, narrowed by whatever the document asked for. Both runs are the same
/// validation, so "the document is valid" and "the document can be run here"
/// cannot disagree about a candidate list or a template.
pub(super) fn validate_flow_within(
    spec: &FlowSpec,
    limits: ResourceLimits,
) -> Result<(), BotError> {
    // First, and before any structural work: a document that asks for a
    // ceiling above the operator's gets a diagnostic naming the axis it asked
    // about, not a run under a bound it did not choose.
    let requested = spec.bounds.effective_resources().within_ceiling()?;
    let effective = requested.narrowed(limits);
    if spec.nodes.is_empty() {
        let refusal = Err(BotError::MalformedFlow {
            cause: "flow declares no nodes".into(),
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "validate_flow_within: returning an error to the caller");
        return refusal;
    }
    if !spec.nodes.contains_key(&spec.entry) {
        let refusal = Err(BotError::InvalidTransitionTarget {
            from: "<entry>".into(),
            target: spec.entry.clone(),
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "validate_flow_within: returning an error to the caller");
        return refusal;
    }
    if spec.bounds.budget == 0 || spec.nodes.len() > spec.bounds.budget {
        let refusal = Err(BotError::FlowBudgetExceeded {
            steps: spec.nodes.len(),
            budget: spec.bounds.budget,
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "validate_flow_within: returning an error to the caller");
        return refusal;
    }

    validate_declarations(&spec.vars)?;
    validate_terminals(spec)?;

    let mut writers = BTreeSet::new();
    for (node_id, kind) in &spec.nodes {
        validate_node(spec, node_id, kind, effective, &mut writers)?;
    }
    for name in spec.vars.keys() {
        if !writers.contains(name) {
            let refusal = Err(BotError::VariableNeverWritten { name: name.clone() });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "validate_flow_within: returning an error to the caller");
            return refusal;
        }
    }
    for edge in &spec.edges {
        let (from, to) = edge.endpoints();
        if !spec.nodes.contains_key(from) {
            let refusal = Err(BotError::InvalidTransitionTarget {
                from: from.to_owned(),
                target: from.to_owned(),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "validate_flow_within: returning an error to the caller");
            return refusal;
        }
        check_target(&spec.nodes, from, to)?;
    }
    reject_uninitialized_reads(spec)?;
    reject_unreachable(spec)
}

/// Reject every read of a variable that is not definitely assigned on all
/// paths reaching the reading node.
///
/// A global writer set answers "does some node write this variable", which is
/// not the question a read has to pass: a session starts with an empty
/// [`VarScope`], so a writer that executes only after the read, or only on a
/// branch the read is not reached through, leaves the read with no value and
/// [`TemplateInterpolator`] returning [`BotError::VariableUnset`] on the first
/// step. This is a forward *must* (definite-assignment) analysis over the
/// execution graph instead of a membership test over the document.
///
/// The state of a node is the set of variables assigned on **every** path from
/// the entry to that node, which is a greatest fixed point: every node is
/// seeded with the full declaration set and each step intersects an outgoing
/// state into its successors, so states only shrink and a cycle converges. A
/// node no entry path reaches keeps that top element and is never accused
/// here — reachability is [`reject_unreachable`]'s single verdict, and one
/// defect must not surface as two.
///
/// Supported assignments and guards, stated so the boundary is not guessed at:
/// an [`NodeKind::Ask`] assigns its variable on each outgoing route and
/// nowhere earlier; every other node kind passes its incoming state through
/// unchanged. No symbolic reasoning about [`Predicate`] values is attempted,
/// so a branch is treated as able to take both successors, and a variable
/// assigned on one predecessor of a join but not another is *not* available at
/// the join. Both choices fail toward silence rather than rejecting a flow the
/// runner would have executed.
fn reject_uninitialized_reads(spec: &FlowSpec) -> Result<(), BotError> {
    let universe: BTreeSet<String> = spec.vars.keys().cloned().collect();
    let mut available: BTreeMap<NodeId, BTreeSet<String>> = spec
        .nodes
        .keys()
        .map(|node_id| (node_id.clone(), universe.clone()))
        .collect();
    available.insert(spec.entry.clone(), BTreeSet::new());

    let mut pending: VecDeque<NodeId> = spec.nodes.keys().cloned().collect();
    while let Some(node_id) = pending.pop_front() {
        let Some(kind) = spec.nodes.get(&node_id) else {
            continue;
        };
        let Some(incoming) = available.get(&node_id).cloned() else {
            continue;
        };
        let mut outgoing = incoming;
        if let NodeKind::Ask { ref var, .. } = *kind {
            outgoing.insert(var.clone());
        }
        for target in successor_targets(spec, &node_id, kind) {
            let Some(state) = available.get_mut(&target) else {
                continue;
            };
            let before = state.len();
            state.retain(|name| outgoing.contains(name));
            if state.len() != before {
                pending.push_back(target);
            }
        }
    }

    for (node_id, kind) in &spec.nodes {
        let Some(state) = available.get(node_id) else {
            continue;
        };
        for name in node_reads(spec, node_id, kind)? {
            if !state.contains(&name) {
                let refusal = Err(BotError::VariableReadBeforeInit {
                    node: node_id.clone(),
                    name,
                });
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "reject_uninitialized_reads: returning an error to the caller");
                return refusal;
            }
        }
    }
    Ok(())
}

/// Collect every variable a node reads when it executes.
///
/// "Reads" means the references the executor actually resolves, not every name
/// a node mentions. [`NodeKind::Branch`] is the case that separates the two:
/// its `var` field is documented as the branch subject, but the executor
/// destructures `when` alone and never resolves `var` against the scope, so a
/// branch subject that some path leaves unassigned cannot fail a run and is
/// not counted here. The predicate's own [`ValueExpr::Var`] references are
/// where a branch reads, and they are counted.
///
/// Only declared names are returned. An undeclared one is already
/// [`BotError::UndeclaredVariable`] from `validate_node`, which runs first, so
/// the definite-assignment pass cannot preempt it with a second verdict on the
/// same defect.
fn node_reads(
    spec: &FlowSpec,
    node_id: &str,
    kind: &NodeKind,
) -> Result<BTreeSet<String>, BotError> {
    match *kind {
        NodeKind::Say { ref text } => declared_template_reads(spec, node_id, text, "say.text"),
        NodeKind::Refer { ref text, .. } => {
            declared_template_reads(spec, node_id, text, "refer.text")
        }
        NodeKind::Branch { ref when, .. } => {
            let mut names = BTreeSet::new();
            collect_predicate_variables(when, &mut names);
            names.retain(|name| spec.vars.contains_key(name));
            Ok(names)
        }
        NodeKind::Ask { .. }
        | NodeKind::Handoff { .. }
        | NodeKind::Route { .. }
        | NodeKind::End => Ok(BTreeSet::new()),
    }
}

/// Compiles `text`, attributing a malformed template to the node and field it
/// was reached through.
///
/// `compile` refuses with `MalformedTemplate` naming the placeholder
/// `"<runtime>"`, because it does not know its caller. The only thing this adds
/// is which node and field it was reached through, so the match is on the
/// variant rather than a map that would discard it: there is no detail in the
/// cause to keep, and a map would read as though there were.
fn compile_for_node<'a>(
    node_id: &str,
    field: &'static str,
    text: &'a str,
) -> Result<CompiledTemplate<'a>, BotError> {
    match CompiledTemplate::compile(text) {
        Ok(compiled) => Ok(compiled),
        Err(BotError::MalformedTemplate { .. }) => {
            let refusal = Err(BotError::MalformedTemplate {
                node: node_id.to_owned(),
                field,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), node = node_id, field, "compile_for_node: the template is malformed");
            refusal
        }
        Err(other) => {
            lgwks_std::trace::debug!(error = ?other, node = node_id, field, "compile_for_node: the template did not compile");
            Err(other)
        }
    }
}

/// Looks `name` up in `scope`, refusing with [`BotError::VariableUnset`] when it
/// holds no value.
///
/// One place for the refusal both the sizing pass and the render pass make, so a
/// marker with no value reads the same whichever of the two reached it first.
pub(super) fn bound<'s>(scope: &'s VarScope, name: &str) -> Result<&'s Value, BotError> {
    match scope.get(name) {
        Some(value) => Ok(value),
        None => {
            let refusal = Err(BotError::VariableUnset {
                name: name.to_owned(),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), name, "bound: the marker has no value");
            refusal
        }
    }
}

/// Return the declared variables a template interpolates.
fn declared_template_reads(
    spec: &FlowSpec,
    node_id: &str,
    text: &str,
    field: &'static str,
) -> Result<BTreeSet<String>, BotError> {
    let compiled = compile_for_node(node_id, field, text)?;
    let mut names = BTreeSet::new();
    for part in compiled.parts() {
        if let TemplatePart::Variable(name) = *part
            && spec.vars.contains_key(name)
        {
            names.insert(name.to_owned());
        }
    }
    Ok(names)
}

/// The two operands of a comparison predicate, or `None` for anything else.
///
/// Every comparison — `Eq`, `Ne`, `Lt`, `Le`, `Gt`, `Ge` — binds the same two
/// operands and nothing else, and two walks over a predicate (the one that
/// collects its variables and the one that validates them) have to agree on
/// which variants those are. They read the list from here instead of each
/// restating six variants, which is how a new comparison would be collected by
/// one and skipped by the other.
fn comparison_operands(predicate: &Predicate) -> Option<(&ValueExpr, &ValueExpr)> {
    match *predicate {
        Predicate::Eq(ref left, ref right)
        | Predicate::Ne(ref left, ref right)
        | Predicate::Lt(ref left, ref right)
        | Predicate::Le(ref left, ref right)
        | Predicate::Gt(ref left, ref right)
        | Predicate::Ge(ref left, ref right) => Some((left, right)),
        Predicate::Const(_) | Predicate::And(_) | Predicate::Or(_) | Predicate::Not(_) => None,
    }
}

/// Collect the variable references of a predicate expression.
fn collect_predicate_variables(predicate: &Predicate, into: &mut BTreeSet<String>) {
    if let Some((left, right)) = comparison_operands(predicate) {
        collect_expr_variables(left, into);
        collect_expr_variables(right, into);
    } else {
        match *predicate {
            Predicate::Const(_) => {}
            Predicate::And(ref items) | Predicate::Or(ref items) => {
                for item in items {
                    collect_predicate_variables(item, into);
                }
            }
            Predicate::Not(ref inner) => collect_predicate_variables(inner, into),
            // Every comparison was taken above, and `comparison_operands` names
            // all six, so this arm can only be reached if that list and this
            // match have drifted apart — which is the point of naming it there.
            Predicate::Eq(..)
            | Predicate::Ne(..)
            | Predicate::Lt(..)
            | Predicate::Le(..)
            | Predicate::Gt(..)
            | Predicate::Ge(..) => {}
        }
    }
}

/// Collect the variable references of one predicate operand.
fn collect_expr_variables(expression: &ValueExpr, into: &mut BTreeSet<String>) {
    if let ValueExpr::Var(ref name) = *expression {
        into.insert(name.clone());
    }
}

/// Validate variable declaration names and choice contents.
pub(super) fn validate_declarations(vars: &BTreeMap<String, VarType>) -> Result<(), BotError> {
    for (name, kind) in vars {
        if !valid_identifier(name) {
            let refusal = Err(BotError::MalformedFlow {
                cause: format!("invalid variable name {name:?}"),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "validate_declarations: returning an error to the caller");
            return refusal;
        }
        match *kind {
            VarType::Choice(ref options)
                if options.is_empty() || has_duplicate_strings(options) =>
            {
                let refusal = Err(BotError::MalformedFlow {
                    cause: format!("choice variable {name:?} has invalid options"),
                });
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "validate_declarations: returning an error to the caller");
                return refusal;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Validate one terminal declaration and its node kind.
///
/// Two invariants, and the second is the one that was missing: a declaration
/// must name a terminal node, and on a node whose kind already fixes the
/// outcome it must not contradict it. `End` completes with nothing said about
/// where the person goes, so every declared outcome is a genuine override and
/// is accepted. A `Handoff` or `Refer` node already names its target, so a
/// declaration of a different outcome — or of the same kind aimed somewhere
/// else — is an authoring contradiction between two statements in one
/// document, and refusing it is the only reading that cannot be wrong. A
/// declaration that repeats the node's own outcome is accepted; it is then
/// read by [`FlowSpec::effective_terminal`] rather than ignored.
fn validate_terminals(spec: &FlowSpec) -> Result<(), BotError> {
    for (node_id, declared) in &spec.terminals {
        let Some(kind) = spec.nodes.get(node_id) else {
            let refusal = Err(BotError::InvalidTransitionTarget {
                from: "<terminal>".into(),
                target: node_id.clone(),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "validate_terminals: returning an error to the caller");
            return refusal;
        };
        if !matches!(
            *kind,
            NodeKind::End | NodeKind::Handoff { .. } | NodeKind::Refer { .. }
        ) {
            let refusal = Err(BotError::MalformedFlow {
                cause: format!("terminal declaration {node_id:?} names a non-terminal node"),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "validate_terminals: returning an error to the caller");
            return refusal;
        }
        // `End` has an intrinsic outcome too, but it is `Completed` — the
        // absence of a destination rather than a claim about one — so a
        // declaration replaces it instead of conflicting with it. Only the two
        // node kinds that name a target can be contradicted.
        if matches!(*kind, NodeKind::End) {
            continue;
        }
        let Some(intrinsic) = spec.intrinsic_terminal(node_id) else {
            // Unreachable on a validated document: the kind was just checked to
            // be one of the three terminal kinds, and every one of them has an
            // intrinsic outcome. Reported rather than skipped so a future node
            // kind added to the check above cannot silently lose the rule.
            let refusal = Err(BotError::MalformedFlow {
                cause: format!("terminal declaration {node_id:?} has no intrinsic outcome"),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "validate_terminals: returning an error to the caller");
            return refusal;
        };
        if intrinsic != *declared {
            let refusal = Err(BotError::ConflictingTerminalDeclaration {
                node: node_id.clone(),
                intrinsic,
                declared: declared.clone(),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "validate_terminals: returning an error to the caller");
            return refusal;
        }
    }
    Ok(())
}

/// Validates one node's references, variables, and text.
///
/// The two kinds this skips are `handoff` and `end`, which carry no text, no
/// variable and no edge to resolve: there is nothing about them that a load-time
/// check could refuse. That is a claim about the kinds, so it is stated here
/// rather than left as two empty arms inside the dispatch.
///
/// One call rather than a `match` of seven: every arm was a single validator
/// call, which is the right shape for the reader and the wrong shape for a gate
/// that reads the whole `match` as one statement.
fn validate_node(
    spec: &FlowSpec,
    node_id: &str,
    kind: &NodeKind,
    limits: ResourceLimits,
    writers: &mut BTreeSet<String>,
) -> Result<(), BotError> {
    match *kind {
        NodeKind::Say { ref text } => validate_say_node(spec, node_id, text, limits),
        NodeKind::Ask {
            ref var,
            ref options,
            ref routes,
        } => validate_ask_node(spec, node_id, var, options, routes, limits, writers),
        NodeKind::Branch {
            ref var,
            ref when,
            ref then,
            ref otherwise,
        } => validate_branch_node(spec, node_id, var, when, then, otherwise),
        NodeKind::Refer { ref text, .. } => validate_refer_node(spec, node_id, text, limits),
        NodeKind::Route {
            ref dispatch,
            ref fallback,
        } => {
            check_target(&spec.nodes, node_id, dispatch)?;
            check_target(&spec.nodes, node_id, fallback)
        }
        NodeKind::Handoff { .. } | NodeKind::End => Ok(()),
    }
}

/// Returns how answers writing `name` are read in `spec`.
///
/// The flow's own reading of a node's answers, so validation and resolution
/// cannot disagree about which policy a question is under — the same reason
/// there is one decoder rather than two.
fn answer_domain_of(spec: &FlowSpec, name: &str) -> AnswerDomain {
    match spec.vars.get(name) {
        Some(declared) => declared.answer_domain(),
        None => AnswerDomain::Label,
    }
}

/// Returns the first pair of options the answer policy cannot tell apart.
///
/// `None` under [`AnswerDomain::Integer`] for an option that is not a whole
/// number: an option that cannot produce the declared value is a different
/// defect, owned by the check that reports it, and folding it into a collision
/// report would name the wrong cause.
fn colliding_options(options: &[String], domain: AnswerDomain) -> Option<(String, String)> {
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for option in options {
        let identity = match domain {
            AnswerDomain::Label => crate::language::normalize(option),
            AnswerDomain::Integer => match decode_integer(option) {
                Some(value) => value.to_string(),
                None => continue,
            },
        };
        if let Some(first) = seen.get(&identity) {
            return Some((first.clone(), option.clone()));
        }
        seen.insert(identity, option.clone());
    }
    None
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

/// Resolve a variable's declared type, refusing an undeclared name.
fn declared_variable<'a>(spec: &'a FlowSpec, name: &str) -> Result<&'a VarType, BotError> {
    spec.vars
        .get(name)
        .ok_or_else(|| BotError::UndeclaredVariable {
            name: name.to_owned(),
        })
}

/// Validate a predicate's variable references and non-empty boolean lists.
fn validate_predicate(
    spec: &FlowSpec,
    node_id: &str,
    predicate: &Predicate,
) -> Result<(), BotError> {
    if let Some((left, right)) = comparison_operands(predicate) {
        validate_expr(spec, node_id, left)?;
        return validate_expr(spec, node_id, right);
    }
    match *predicate {
        Predicate::Const(_) => Ok(()),
        Predicate::And(ref items) | Predicate::Or(ref items) => {
            if items.is_empty() {
                let refusal = Err(BotError::MalformedFlow {
                    cause: format!("node {node_id:?} has an empty predicate"),
                });
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "validate_predicate: returning an error to the caller");
                return refusal;
            }
            for item in items {
                validate_predicate(spec, node_id, item)?;
            }
            Ok(())
        }
        Predicate::Not(ref inner) => validate_predicate(spec, node_id, inner),
        // Every comparison returned above, through the one list of them.
        Predicate::Eq(..)
        | Predicate::Ne(..)
        | Predicate::Lt(..)
        | Predicate::Le(..)
        | Predicate::Gt(..)
        | Predicate::Ge(..) => Ok(()),
    }
}

/// Validate one predicate expression.
fn validate_expr(spec: &FlowSpec, _node_id: &str, expression: &ValueExpr) -> Result<(), BotError> {
    if let ValueExpr::Var(ref name) = *expression
        && !spec.vars.contains_key(name)
    {
        let refusal = Err(BotError::UndeclaredVariable { name: name.clone() });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "validate_expr: returning an error to the caller");
        return refusal;
    }
    Ok(())
}

/// Validate one template's syntax, its declared names, and its static floor.
///
/// The floor is the part of the expansion that is known without running the
/// flow: the literal bytes, which every render must contain whatever the
/// variables turn out to hold. A template whose literals alone exceed the
/// record ceiling can never render, so it is refused here rather than at the
/// node that would speak it. That is the only static size claim available —
/// the rest of the expansion depends on answers this cannot see — and it is
/// exactly the claim that is true unconditionally.
fn validate_template(
    spec: &FlowSpec,
    node_id: &str,
    template: &str,
    field: &'static str,
    limits: ResourceLimits,
) -> Result<(), BotError> {
    let compiled = compile_for_node(node_id, field, template)?;
    let mut literals = 0usize;
    for part in compiled.parts() {
        match *part {
            TemplatePart::Literal(text) => {
                literals = literals.checked_add(text.len()).ok_or(
                    BotError::TemplateExpansionTooLarge {
                        node: node_id.to_owned(),
                        bytes: usize::MAX,
                        limit: limits.get(ResourceAxis::Record),
                    },
                )?;
            }
            TemplatePart::Variable(name) => {
                if !spec.vars.contains_key(name) {
                    let refusal = Err(BotError::UndeclaredVariable {
                        name: name.to_owned(),
                    });
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "validate_template: returning an error to the caller");
                    return refusal;
                }
            }
        }
    }
    let record_bytes = limits.get(ResourceAxis::Record);
    if literals > record_bytes {
        let refusal = Err(BotError::TemplateExpansionTooLarge {
            node: node_id.to_owned(),
            bytes: literals,
            limit: record_bytes,
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "validate_template: returning an error to the caller");
        return refusal;
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
            let refusal = Err(BotError::UnreachableNode {
                node: node_id.clone(),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "reject_unreachable: returning an error to the caller");
            return refusal;
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
pub(super) fn valid_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|character| character.is_ascii_alphanumeric() || character == '_')
}
