//! The cursor walk: inspection metrics, the retained recovery diagnostics and the name readers.

use ast_grep_core::tree_sitter::LanguageExt;

use super::{
    AstMetrics, AstNode, InspectionStopReason, MAX_SYNTAX_DIAGNOSTICS, SyntaxDiagnostic,
    SyntaxIssueKind,
};

impl AstMetrics {
    /// Fold one visited node into the running metrics.
    ///
    /// Called once per node of a walk, so this is where the three accumulators
    /// are kept monotone: `nodes` saturates instead of wrapping (unreachable at
    /// any real tree size, but the bound is stated rather than assumed),
    /// `max_depth` keeps the deepest branch seen, and `has_syntax_issues` is
    /// sticky: once an `ERROR` or `MISSING` node is seen, no later clean node
    /// may clear it, because the walk has no way to unsee it.
    fn charging(mut self, visit: &Visit) -> Self {
        self.nodes = self.nodes.saturating_add(1);
        self.max_depth = self.max_depth.max(visit.depth);
        self.has_syntax_issues = self.has_syntax_issues || visit.recovery.is_some();
        self
    }
}

/// Whether the tree holds an `ERROR` or `MISSING` node. Ask before reporting:
/// on unreadable source, no finding means nothing parsed, not nothing wrong.
#[must_use]
pub fn has_syntax_issues<L: LanguageExt>(root: &AstNode<'_, L>) -> bool {
    inspect_ast(root, None).has_syntax_issues
}

/// Deepest branch depth, root counting as 1.
#[must_use]
pub fn max_depth<L: LanguageExt>(root: &AstNode<'_, L>) -> usize {
    inspect_ast(root, None).max_depth
}

/// Node count, depth, recovery state, and traversal completeness in one walk.
///
/// With a node cap, traversal stops at `limit + 1`: enough to prove refusal
/// without letting validation itself go unbounded on a hostile tree. The
/// returned metrics preserve that cap and identify incomplete inspection;
/// `has_syntax_issues == false` is not evidence of a clean tree unless
/// `complete` is also true.
///
/// This walk is uncapped in depth, so `max_depth` is the real depth of the
/// tree and a malformed one is measured rather than refused. [`MAX_AST_DEPTH`](crate::MAX_AST_DEPTH)
/// is the checked parse's ceiling and has no parameter here on purpose: a
/// caller inspecting a tree on purpose is not the hostile-input case, and
/// refusing it would make the reported depth a constant.
#[must_use]
pub fn inspect_ast<'t, L: LanguageExt>(
    root: &AstNode<'t, L>,
    stop_after_nodes: Option<usize>,
) -> AstMetrics {
    inspect_ast_with_pending(root, stop_after_nodes, None).0
}

/// Inspect in one cursor-driven pre-order pass, retaining no sibling frontier.
///
/// The walk drives a tree-sitter cursor directly. The cursor owns a stack of the
/// ancestors it has descended through and nothing else, so the traversal
/// retains state proportional to active depth and never to a node's fan-out
/// (#277). The previous hand-rolled frame walk reached each child through
/// `ast_grep_core::Node::child` by index, and that call is `O(index)` on a node
/// whose visible children are not its structural ones — which is exactly what a
/// recovery-heavy parse produces. The measured cost was quadratic in fan-out: a
/// 16 KiB source of unbalanced delimiters, 16 385 nodes wide and 2 deep, spent
/// 1.39 s in the walk against 1.06 ms in the parser, and doubling the width
/// quadrupled the walk. The cursor walk spends 40 ns per node on that same
/// source, at every width from 2 KiB to 2 MiB.
///
/// The cursor is reached through the node the walk was handed, so the walk
/// names no `tree-sitter` type: every call below is an inherent method on a type
/// this crate already holds. (The crate's one direct `tree-sitter` use is the
/// parse deadline in `parse_tree`, which `ast-grep-core` has no door for.)
///
/// Two properties move with the traversal and are worth stating rather than
/// leaving to be discovered:
///
/// - **Order.** Nodes arrive in source order, not reverse-sibling order. Every
///   published guarantee survives it: [`AstMetrics`] folds order-independently,
///   and the retained diagnostics are the earliest ones under
///   [`MAX_SYNTAX_DIAGNOSTICS`] and are sorted by source position before they
///   are returned. What changes is only which node is the `limit + 1` witness,
///   and it is now the one earliest in the file rather than the last.
/// - **Frames.** The second return value is `peak_depth`: the deepest cursor
///   stack the walk held, which is the number of active ancestors and therefore
///   the memory the traversal actually retains. It is kept private as a
///   directly asserted resource invariant rather than exported as a public
///   metric whose callers might mistake it for a configured limit.
pub(super) fn inspect_ast_with_pending<'t, L: LanguageExt>(
    root: &AstNode<'t, L>,
    stop_after_nodes: Option<usize>,
    stop_after_depth: Option<usize>,
) -> (AstMetrics, usize, Vec<SyntaxDiagnostic>, bool) {
    let mut metrics = AstMetrics {
        nodes: 0,
        max_depth: 0,
        has_syntax_issues: false,
        complete: true,
        node_limit: stop_after_nodes,
        stop_reason: None,
    };
    let mut diagnostics = Vec::new();
    let mut diagnostics_truncated = false;
    let mut cursor = root.get_inner_node().walk();
    // The cursor starts on the root, which counts as depth 1, so its zero-based
    // depth is always one less than the depth a caller reads.
    let mut cursor_depth = 0_usize;
    let mut peak_depth = 0_usize;
    loop {
        let node = cursor.node();
        let span = node.range();
        let recovery = if node.is_error() {
            Some(SyntaxIssueKind::Error)
        } else if node.is_missing() {
            Some(SyntaxIssueKind::Missing)
        } else {
            None
        };
        let visit = Visit {
            depth: cursor_depth.saturating_add(1),
            recovery,
            span,
        };
        peak_depth = peak_depth.max(visit.depth);
        metrics = metrics.charging(&visit);
        // The witness node is charged before the walk ends, so a refusal that
        // stops at a ceiling still reports the damage it saw on the way to the
        // ceiling rather than dropping it.
        push_recovery_span(&visit, &mut diagnostics, &mut diagnostics_truncated);
        if let Some(reason) = charge_reason(&metrics, stop_after_nodes, stop_after_depth) {
            mark_inspection_incomplete(&mut metrics, reason);
            break;
        }
        // Descend into the first child, or climb until a sibling exists. The
        // cursor's own stack is the ancestor record, so unwinding costs one step
        // per level rather than a search for the parent.
        if cursor.goto_first_child() {
            cursor_depth = cursor_depth.saturating_add(1);
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                diagnostics.sort_unstable_by_key(syntax_position);
                return (metrics, peak_depth, diagnostics, diagnostics_truncated);
            }
            cursor_depth = cursor_depth.saturating_sub(1);
        }
    }
    diagnostics.sort_unstable_by_key(syntax_position);
    (metrics, peak_depth, diagnostics, diagnostics_truncated)
}

/// One node as the walk read it, detached from the cursor that produced it.
///
/// The walk asks three questions of every node and keeps nothing else, so the
/// facts are copied out here rather than a node handle: a handle borrows the
/// cursor's position, and a list of them would be the sibling frontier this
/// traversal exists not to build.
struct Visit {
    /// One-based branch depth, the root counting as 1.
    depth: usize,
    /// The recovery kind this node carries, or `None` for a clean node.
    recovery: Option<SyntaxIssueKind>,
    /// Inclusive start and exclusive end byte offsets in the source, in the
    /// node type's own range so no conversion sits in the walk's inner loop.
    span: ast_grep_core::tree_sitter::TSRange,
}

/// Which cap this observation already exceeded, if either.
///
/// Evaluated after every node, the root included, so a ceiling at or below one
/// stops the walk there and the overflow witness is charged before the walk
/// ends — the accounting the previous root-only check gave, applied at every
/// level. With both ceilings configured the node one is reported, because the
/// node charge is the first the root trips and a caller reading one reason
/// wants the one that fired first.
fn charge_reason(
    metrics: &AstMetrics,
    stop_after_nodes: Option<usize>,
    stop_after_depth: Option<usize>,
) -> Option<InspectionStopReason> {
    if stop_after_nodes.is_some_and(|limit| metrics.nodes > limit) {
        return Some(InspectionStopReason::NodeLimitExceeded);
    }
    stop_after_depth
        .is_some_and(|limit| metrics.max_depth > limit)
        .then_some(InspectionStopReason::DepthLimitExceeded)
}

/// Mark a walk as partial after it observes the value beyond its configured cap.
fn mark_inspection_incomplete(metrics: &mut AstMetrics, reason: InspectionStopReason) {
    metrics.complete = false;
    metrics.stop_reason = Some(reason);
}

/// Retain one recovery-node span until the public diagnostic ceiling is reached.
fn push_recovery_span(
    visit: &Visit,
    diagnostics: &mut Vec<SyntaxDiagnostic>,
    diagnostics_truncated: &mut bool,
) {
    let Some(kind) = visit.recovery else {
        return;
    };
    let span = visit.span;
    push_syntax_diagnostic(
        SyntaxDiagnostic {
            kind,
            start_byte: span.start_byte,
            end_byte: span.end_byte,
        },
        diagnostics,
        diagnostics_truncated,
    );
}

/// Where a diagnostic sits in the source, for keeping and ordering them.
pub(super) fn syntax_position(diagnostic: &SyntaxDiagnostic) -> (usize, usize) {
    (diagnostic.start_byte, diagnostic.end_byte)
}

/// Retain a diagnostic under the fixed per-parse ceiling, keeping the earliest.
///
/// The ceiling is a constant, so something has to be dropped once it is
/// reached, and the error a reader needs first — where the source stopped
/// parsing — is the one that must not be it. At the ceiling a new diagnostic
/// replaces the latest one retained when it starts earlier; the retained set is
/// always the earliest in source order, in constant memory, whatever order the
/// walk reached them in. (The cursor-driven walk reaches them in source order
/// already; the rule is what keeps the property true of any traversal order,
/// including a caller-supplied one.)
pub(super) fn push_syntax_diagnostic(
    diagnostic: SyntaxDiagnostic,
    diagnostics: &mut Vec<SyntaxDiagnostic>,
    diagnostics_truncated: &mut bool,
) {
    if diagnostics.len() < MAX_SYNTAX_DIAGNOSTICS {
        diagnostics.push(diagnostic);
        return;
    }
    *diagnostics_truncated = true;
    let latest = diagnostics
        .iter_mut()
        .max_by_key(|retained| syntax_position(retained));
    if let Some(latest) = latest
        && syntax_position(&diagnostic) < syntax_position(latest)
    {
        *latest = diagnostic;
    }
}

/// The owned text of the first direct child whose `kind` equals one of `kinds`,
/// or `None` when no direct child matches. Descendants are not searched, so a
/// caller hunting a nested identifier must walk to that level first. The text
/// is owned because the node's source borrow is tied to the parsed tree.
#[must_use]
pub fn child_text_with_kind<L: LanguageExt>(
    node: &AstNode<'_, L>,
    kinds: &[&str],
) -> Option<String> {
    node.children()
        .find(|child| kinds.contains(&child.kind().as_ref()))
        .map(|child| child.text().to_string())
}

/// The name a definition node declares, if a direct child kind in
/// `name_kinds` names it.
///
/// With [`callee_name`] and [`child_text_with_kind`], the name-resolution
/// surface. These three are the only way to read a declared name without
/// writing a tree walk by hand, so they stay on the surface deliberately. That
/// is a judgement about this crate's own API, not a claim about callers: no
/// crate in this workspace depends on `lgwks_ast` today, and these three carry
/// no callers outside this crate's tests.
#[must_use]
pub fn definition_name<L: LanguageExt>(
    node: &AstNode<'_, L>,
    name_kinds: &[&str],
) -> Option<String> {
    child_text_with_kind(node, name_kinds)
}

/// The callee a call node names, or `None` when `node` is not a call kind or
/// names none of `name_kinds`. See [`definition_name`] for why this trio is
/// retained.
#[must_use]
pub fn callee_name<L: LanguageExt>(
    node: &AstNode<'_, L>,
    call_kinds: &[&str],
    name_kinds: &[&str],
) -> Option<String> {
    call_kinds
        .contains(&node.kind().as_ref())
        .then(|| child_text_with_kind(node, name_kinds))
        .flatten()
}
