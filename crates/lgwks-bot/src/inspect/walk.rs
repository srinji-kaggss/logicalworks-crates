//! The bounded walk: one iterative pass over the parsed tree that charges every node against the budgets and
//! evaluates the shipped rules on it. Split from `inspect.rs` (#526).

use super::rules::{RULES, Rule, bounded_preview, rule_matches};
use super::{Budgets, Finding, IncompleteReason};

/// What one bounded walk accumulated.
pub(super) struct WalkOutcome {
    /// Nodes visited.
    pub(super) nodes: usize,
    /// Deepest node visited.
    pub(super) max_depth: usize,
    /// Rule evaluations performed.
    pub(super) work: usize,
    /// Findings retained.
    pub(super) findings: Vec<Finding>,
    /// Bytes of finding text retained.
    pub(super) output_bytes: usize,
    /// Why the walk stopped early, if it did.
    pub(super) incomplete: Option<IncompleteReason>,
}

/// One visited node's frame: the node, its remaining child index, and its depth.
type Frame<'t> = (lgwks_ast::AstNode<'t>, usize, usize);

/// Walk `parsed` against the shipped rules under `budgets`.
///
/// The walk is iterative with one frame per active ancestor, so its resident
/// state follows depth rather than sibling fan-out, and every budget is charged
/// as the work happens rather than after it.
pub(super) fn walk(
    parsed: &lgwks_ast::Parsed,
    source: &str,
    budgets: &Budgets,
    enforce: bool,
) -> WalkOutcome {
    let root = parsed.root();
    let mut outcome = WalkOutcome {
        nodes: 0,
        max_depth: 0,
        work: 0,
        findings: Vec::new(),
        output_bytes: 0,
        incomplete: None,
    };

    // The root is visited first and charged like any other node, so a budget
    // that cannot even admit the root is a refusal rather than an empty walk.
    if !charge_and_evaluate(&root, 1, source, budgets, &mut outcome, enforce) {
        return outcome;
    }
    let root_children = root.children().len();
    let mut frames: Vec<Frame<'_>> = vec![(root, root_children, 1)];
    while let Some(frame) = frames.last_mut() {
        let Some(child_index) = frame.1.checked_sub(1) else {
            frames.pop();
            continue;
        };
        frame.1 = child_index;
        let next_child = frame.0.child(child_index);
        let Some(child) = next_child else {
            continue;
        };
        let depth = frame.2.saturating_add(1);
        if !charge_and_evaluate(&child, depth, source, budgets, &mut outcome, enforce) {
            return outcome;
        }
        let child_count = child.children().len();
        frames.push((child, child_count, depth));
    }
    outcome
}

/// Charge one node against every budget and evaluate the shipped rules on it.
///
/// Returns `false` when a budget stopped the walk, having recorded the reason
/// on `outcome`.
fn charge_and_evaluate(
    node: &lgwks_ast::AstNode<'_>,
    depth: usize,
    source: &str,
    budgets: &Budgets,
    outcome: &mut WalkOutcome,
    enforce: bool,
) -> bool {
    outcome.nodes = outcome.nodes.saturating_add(1);
    if enforce && outcome.nodes > budgets.max_nodes {
        outcome.incomplete = Some(IncompleteReason::NodeBudgetExceeded {
            observed: outcome.nodes,
            limit: budgets.max_nodes,
        });
        return false;
    }
    if enforce && depth > budgets.max_depth {
        outcome.max_depth = outcome.max_depth.max(depth);
        outcome.incomplete = Some(IncompleteReason::DepthBudgetExceeded {
            reached: depth,
            limit: budgets.max_depth,
        });
        return false;
    }
    outcome.max_depth = outcome.max_depth.max(depth);

    // The node's kind and text borrow the tree, so they are held for the whole
    // evaluation and never copied.
    let kind = node.kind();
    let node_kind: &str = kind.as_ref();
    let text = node.text();
    let node_text: &str = text.as_ref();
    for rule in RULES {
        outcome.work = outcome.work.saturating_add(1);
        if enforce && outcome.work > budgets.max_work {
            outcome.incomplete = Some(IncompleteReason::WorkBudgetExceeded {
                observed: outcome.work,
                limit: budgets.max_work,
            });
            return false;
        }
        if rule_matches(rule.rule_id, node_kind, node_text)
            && !retain_finding(rule, node, source, budgets, outcome, enforce)
        {
            return false;
        }
    }
    true
}

/// Retain one finding, charging the findings and output budgets first.
///
/// Returns `false` when a budget stopped the walk.
fn retain_finding(
    rule: &Rule,
    node: &lgwks_ast::AstNode<'_>,
    source: &str,
    budgets: &Budgets,
    outcome: &mut WalkOutcome,
    enforce: bool,
) -> bool {
    if enforce && outcome.findings.len() >= budgets.max_findings {
        outcome.incomplete = Some(IncompleteReason::FindingsBudgetExceeded {
            limit: budgets.max_findings,
        });
        return false;
    }
    let range = node.range();
    let (preview, preview_truncated) = bounded_preview(source, range.start, range.end);
    let cost = rule.rule_id.len().saturating_add(preview.len());
    if enforce && outcome.output_bytes.saturating_add(cost) > budgets.max_output_bytes {
        outcome.incomplete = Some(IncompleteReason::OutputBudgetExceeded {
            limit: budgets.max_output_bytes,
        });
        return false;
    }
    outcome.output_bytes = outcome.output_bytes.saturating_add(cost);
    outcome.findings.push(Finding {
        rule_id: rule.rule_id.to_owned(),
        start_byte: range.start,
        end_byte: range.end,
        preview,
        preview_truncated,
    });
    true
}
