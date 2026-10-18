//! The walk's and the diagnostics' tests: node budgets, depth, and the recovery reports (INV-AST-1 to INV-AST-3).

use super::walk::{inspect_ast_with_pending, push_syntax_diagnostic, syntax_position};
use super::*;

#[test]
fn a_small_node_budget_does_not_retain_a_wide_sibling_frontier() {
    // INV-AST-1, on the adversarial width: 4 096 immediate siblings under
    // the root, which is the shape a sibling frontier would have to be
    // sized for. The walk stops at the cap after two nodes, so the retained
    // state must be the two levels of cursor stack it descended through and
    // nothing resembling the 4 096 children behind the first of them.
    let source = (0..4_096)
        .map(|index| format!("fn f{index}() {{}}\n"))
        .collect::<String>();
    let parsed = parse(&source, Language::Rust);
    assert_eq!(
        parsed.root().children().len(),
        4_096,
        "the fixture presents 4,096 immediate siblings to the traversal"
    );

    let (metrics, peak_depth, _, _) = inspect_ast_with_pending(&parsed.root(), Some(1), None);

    assert_eq!(metrics.nodes, 2, "one node beyond the cap proves refusal");
    assert_eq!(
        inspect_ast(&parsed.root(), Some(1)).nodes,
        2,
        "the public inspector preserves the budget's overflow witness"
    );
    assert_eq!(
        peak_depth, 2,
        "retained a cursor stack {peak_depth} deep for a 4,096-child root, \
         which is a sibling frontier rather than a depth"
    );
    assert!(
        peak_depth < 16,
        "the retained depth must not scale with the root's {width} children",
        width = parsed.root().children().len()
    );
}

#[test]
fn a_node_budget_charges_children_in_source_order() {
    // Which node is the `limit + 1` witness is the one thing the cursor walk
    // changed, and it is stated rather than left implicit: children arrive
    // in source order, so a cap below the tree size charges the *earliest*
    // children. Both halves matter — a witness taken from the end of the
    // file could report damage a caller would never see, and one taken from
    // the front cannot.
    let parsed = parse(
        "fn first() {}\nfn second() {}\nfn third() {}\n",
        Language::Rust,
    );
    let root = parsed.root();
    assert!(
        root.children().len() >= 3,
        "the fixture presents at least three root children to the traversal"
    );

    let metrics = inspect_ast(&root, Some(1));

    assert_eq!(metrics.nodes, 2, "the overflow witness is counted");
    assert!(
        !metrics.has_syntax_issues,
        "the first child of valid source is clean, so the cap charged a clean \
         witness rather than one from the end of the file"
    );
    assert_eq!(
        metrics.stop_reason,
        Some(InspectionStopReason::NodeLimitExceeded),
        "the cap, not a clean tree, is what stopped the walk"
    );
}

#[test]
fn a_completed_walk_retains_one_frame_per_active_ancestor() {
    // The other half of INV-AST-1, and the half the budget test above
    // cannot reach. That test stops at the cap before descending, so it
    // proves the early break is cheap; it says nothing about the steady
    // state, which is the path every real walk takes (the production cap is
    // 2,000,000 nodes and is never hit). This one runs to completion.
    //
    // A deep, narrow tree: each level has one child, so the number of active
    // ancestors is the depth, and the state retained must be that depth
    // rather than the total node count.
    let depth = 200;
    let source = format!(
        "{}fn f() {{}}{}",
        "fn f() {".repeat(depth),
        "}".repeat(depth)
    );
    let parsed = parse(&source, Language::Rust);
    let (metrics, peak_depth, _, _) = inspect_ast_with_pending(&parsed.root(), None, None);

    assert!(
        metrics.max_depth > 50,
        "the fixture is {} deep, so the walk really does descend",
        metrics.max_depth
    );
    assert_eq!(
        peak_depth, metrics.max_depth,
        "retained a stack {peak_depth} deep for a tree {} deep; the walk is \
         not retaining exactly the active ancestors",
        metrics.max_depth
    );
    assert!(
        metrics.nodes > peak_depth.saturating_mul(4),
        "retained a stack {peak_depth} deep for a tree of {} nodes, which is \
         a sibling frontier rather than a depth",
        metrics.nodes
    );
}

/// The walk this crate replaced, kept as the model the cursor walk is
/// checked against.
///
/// It is the frame walk from before #277, verbatim in its traversal: a
/// stack of `(node, depth, next child index)` with children taken by index,
/// descending into the highest index first. It exists so the replacement can
/// be shown to visit the same nodes at the same depths rather than merely
/// to be shown green, and it is `#[cfg(test)]` because nothing ships it.
fn positional_walk<L: LanguageExt>(root: &AstNode<'_, L>) -> (usize, usize, usize, bool) {
    let mut frames = vec![(root.clone(), 1_usize, root.children().len())];
    let mut nodes = 1_usize;
    let mut peak_frames = 1_usize;
    let mut deepest = 1_usize;
    let mut issues = root.is_error() || root.is_missing();
    while let Some(frame) = frames.last_mut() {
        let Some(child_index) = frame.2.checked_sub(1) else {
            frames.pop();
            continue;
        };
        frame.2 = child_index;
        let Some(child) = frame.0.child(child_index) else {
            continue;
        };
        let child_depth = frame.1.saturating_add(1);
        nodes = nodes.saturating_add(1);
        deepest = deepest.max(child_depth);
        issues = issues || child.is_error() || child.is_missing();
        let child_count = child.children().len();
        frames.push((child, child_depth, child_count));
        peak_frames = peak_frames.max(frames.len());
    }
    (nodes, deepest, peak_frames, issues)
}

#[test]
fn the_cursor_walk_visits_exactly_the_nodes_the_positional_walk_did() {
    // The differential evidence for #277. The traversal changed; what it
    // must not have changed is the answer. Five trees chosen for the shapes
    // that separate the two: flat and wide, deep and narrow, recovery-heavy,
    // empty, and a real function.
    let wide = (0..4_096)
        .map(|index| format!("fn f{index}() {{}}\n"))
        .collect::<String>();
    let deep = format!("{}fn f() {{}}{}", "fn f() {".repeat(200), "}".repeat(200));
    let fixtures = [
        String::new(),
        String::from("fn main() {}\n"),
        wide,
        deep,
        String::from("@\nfn broken( {\n"),
    ];
    for (index, source) in fixtures.iter().enumerate() {
        let parsed = parse(source.as_str(), Language::Rust);
        let root = parsed.root();
        let (model_nodes, model_depth, model_peak, model_issues) = positional_walk(&root);
        let (metrics, peak_depth, _, _) = inspect_ast_with_pending(&root, None, None);

        assert_eq!(
            metrics.nodes, model_nodes,
            "fixture {index}: {} nodes visited against the model's {model_nodes}",
            metrics.nodes
        );
        assert_eq!(
            metrics.max_depth, model_depth,
            "fixture {index}: depth {} against the model's {model_depth}",
            metrics.max_depth
        );
        assert_eq!(
            peak_depth, model_peak,
            "fixture {index}: retained {peak_depth} against the model's \
             {model_peak} active ancestors"
        );
        assert_eq!(
            metrics.has_syntax_issues, model_issues,
            "fixture {index}: recovery state disagrees with the model"
        );
    }
}

#[test]
fn the_walk_costs_the_same_per_node_however_wide_the_tree_is()
-> Result<(), Box<dyn std::error::Error>> {
    // The measured regression behind #277, asserted as a bound rather than
    // a timing: the old walk indexed children, which is `O(index)` on a
    // recovery-heavy tree, so the per-node cost grew with the root's width.
    // Four widths of unbalanced delimiters — the shape that produced
    // 85 microseconds per node — must now cost within a small constant of
    // each other. The factor is generous on purpose: this asserts a
    // complexity class, and a wall-clock ratio is evidence of one, not a
    // benchmark. A regression to index-addressed children would multiply
    // the wide case by the width ratio (16x here) and fail by an order of
    // magnitude rather than by a hair.
    //
    // Each width is walked in `ROUNDS` windows interleaved with the others
    // and judged at its minimum, because descheduling noise is one-sided:
    // a descheduled window can only inflate a width's cost, never deflate
    // it, so the minimum is the width's cost with the host's interference
    // removed. The median was tried first and failed on a loaded CI
    // runner (run 37635420955): five of nine 1,024-node windows caught a
    // deschedule, each a tens-of-microseconds window where one preemption
    // multiplies the reading a hundredfold, and the median moved 6.5x
    // while no width scaled. A regression to index-addressed children
    // would multiply the wide case by the width ratio (16x here) at every
    // window including the minimum, and still fail the 4x bound by an
    // order of magnitude rather than by a hair.
    const WIDTHS: [usize; 4] = [1_024, 2_048, 4_096, 16_384];
    const ROUNDS: usize = 9;
    let sources = WIDTHS.map(|width| "(".repeat(width));
    let parsed = sources
        .iter()
        .map(|source| parse(source, Language::Rust))
        .collect::<Vec<_>>();
    let roots = parsed.iter().map(Parsed::root).collect::<Vec<_>>();
    let mut divisors = Vec::with_capacity(roots.len());
    for root in &roots {
        // The root counts as a node, so the model never reports zero and the
        // divisor below is a count rather than a stand-in for one.
        let nodes = positional_walk(root).0.max(1);
        divisors.push((nodes, nanos_per(nodes)?));
    }
    let mut windows = [[0_u128; WIDTHS.len()]; ROUNDS];
    for round in &mut windows {
        for ((window, root), &(nodes, divisor)) in round.iter_mut().zip(&roots).zip(&divisors) {
            let started = std::time::Instant::now();
            let metrics = inspect_ast(root, None);
            let elapsed = started.elapsed().as_nanos();
            assert_eq!(metrics.nodes, nodes, "the model and the walk disagree");
            *window = elapsed.saturating_div(divisor);
        }
    }
    let mut per_node_nanos = Vec::with_capacity(WIDTHS.len());
    for width in 0..WIDTHS.len() {
        let cheapest_window = windows
            .iter()
            .map(|round| round[width])
            .min()
            .ok_or("nine rounds produced no window for a width")?;
        per_node_nanos.push(cheapest_window);
    }
    let cheapest = per_node_nanos
        .iter()
        .copied()
        .min()
        .ok_or("four widths produced no per-node cost")?;
    let dearest = per_node_nanos
        .iter()
        .copied()
        .max()
        .ok_or("four widths produced no per-node cost")?;
    assert!(
        dearest <= cheapest.saturating_mul(4),
        "per-node walk cost {cheapest}..={dearest} ns across widths \
         {WIDTHS:?}; the cost must not scale with fan-out"
    );
    Ok(())
}

/// `nodes` as the divisor a per-node cost is taken over.
///
/// The node count is a `usize` because the walk counts in one, and a per-node
/// nanosecond figure is a `u128` because a wide tree's elapsed time is; the
/// conversion is stated here rather than substituted for at each call site.
fn nanos_per(nodes: usize) -> Result<u128, Box<dyn std::error::Error>> {
    let divisor = u128::try_from(nodes)?;
    if divisor == 0 {
        let refusal = Err("a per-node cost over no nodes is not a cost".into());
        tracing::debug!(error = ?refusal.as_ref().err(), "nanos_per: returning an error to the caller");
        return refusal;
    }
    Ok(divisor)
}

#[test]
fn inspection_metrics_name_complete_exact_and_over_limit_walks() {
    let parsed = parse("fn a() {}\nfn b() {}", Language::Rust);
    let root = parsed.root();
    let full = inspect_ast(&root, None);
    assert!(
        full.complete,
        "an unlimited traversal inspects the whole tree"
    );
    assert_eq!(
        full.node_limit, None,
        "unlimited inspection has no applied cap"
    );
    assert_eq!(
        full.stop_reason, None,
        "complete inspection has no stop reason"
    );

    let exact = inspect_ast(&root, Some(full.nodes));
    assert!(
        exact.complete,
        "visiting exactly the cap is a complete traversal"
    );
    assert_eq!(
        exact.node_limit,
        Some(full.nodes),
        "the applied cap is retained"
    );

    let over = inspect_ast(&root, Some(full.nodes - 1));
    assert!(
        !over.complete,
        "a cap below the tree size marks partial metrics"
    );
    assert_eq!(over.nodes, full.nodes, "the overflow witness is counted");
    assert_eq!(
        over.node_limit,
        Some(full.nodes - 1),
        "partial metrics preserve the configured limit"
    );
    assert_eq!(
        over.stop_reason,
        Some(InspectionStopReason::NodeLimitExceeded),
        "partial metrics identify the stop reason"
    );

    let zero = inspect_ast(&root, Some(0));
    assert!(
        !zero.complete,
        "the root is the limit-plus-one witness at zero cap"
    );
    assert_eq!(zero.nodes, 1, "zero cap still records the root observation");
}

#[test]
fn recovery_beyond_the_visit_cap_is_not_reported_as_clean() {
    // The recovery node has to be *beyond* the cap now that children are
    // charged in source order: `fn a() {}` on the first line puts two clean
    // nodes in front of it, so a cap of one stops before the `@`.
    let parsed = parse("fn a() {}\nfn b() {}\n@\n", Language::Rust);
    let root = parsed.root();
    assert!(
        root.children().last().is_some_and(|node| node.is_error()),
        "the fixture puts a recovery node in the last root-child position"
    );

    let partial = inspect_ast(&root, Some(1));
    assert!(
        !partial.complete,
        "the small cap stops before the late recovery node"
    );
    assert!(
        !partial.has_syntax_issues,
        "unvisited nodes are not called clean or faulty"
    );

    let complete = inspect_ast(&parsed.root(), None);
    assert!(
        complete.complete,
        "the uncapped walk reaches the whole tree"
    );
    assert!(
        complete.has_syntax_issues,
        "the full walk observes the recovery node"
    );
}

#[test]
fn syntax_diagnostics_distinguish_recovery_kinds_and_use_source_byte_ranges()
-> Result<(), Box<dyn std::error::Error>> {
    let source = "fn main() {\n    let café = ;\n}\n";
    let invalid = try_parse(source, Language::Rust);
    let diagnostics = match invalid {
        Err(ParseError::InvalidSyntax { diagnostics, .. }) => diagnostics,
        other => {
            return Err(format!("expected syntax diagnostics, got {:?}", other.err()).into());
        }
    };
    let error = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.kind == SyntaxIssueKind::Error)
        .ok_or_else(|| std::io::Error::other("the malformed expression has no ERROR node"))?;
    assert!(
        source.is_char_boundary(error.start_byte),
        "start byte is a UTF-8 boundary"
    );
    assert!(
        source.is_char_boundary(error.end_byte),
        "end byte is a UTF-8 boundary"
    );
    assert!(
        error.start_byte <= error.end_byte,
        "diagnostic span is ordered"
    );
    assert!(
        source.get(error.start_byte..error.end_byte).is_some(),
        "span slices the source"
    );
    assert!(
        source[..error.start_byte].contains('é'),
        "span follows the multibyte prefix"
    );

    let missing = try_parse("fn f() { let x = 1 }", Language::Rust);
    assert!(
        matches!(
            missing,
            Err(ParseError::InvalidSyntax { ref diagnostics, .. })
                if diagnostics.iter().any(|diagnostic| diagnostic.kind == SyntaxIssueKind::Missing)
        ),
        "an omitted let semicolon is reported as MISSING"
    );
    Ok(())
}

/// Issue #211: a `MISSING` diagnostic's span is zero-width, and the README
/// must not call it a caret span that underlines the fault.
///
/// This is the negative control for the documentation repair. The shipped
/// README promised diagnostics carry "a caret span, so a caller reports
/// where the source stopped making sense", but a `MISSING` node marks an
/// insertion point rather than text: it has no bytes to underline. The
/// measured width is asserted here rather than asserted in prose, so a
/// future change to the span calculation has to update this test and the
/// sentence together instead of silently making the docs true or false.
#[test]
fn a_missing_recovery_node_carries_a_zero_width_span() -> Result<(), Box<dyn std::error::Error>> {
    // A missing `;` after a `let` binding: the grammar recovered by
    // inserting the token, so there is no source text at the node.
    let source = "fn main() {\n    let x = 1\n}\n";
    let diagnostics = match try_parse(source, Language::Rust) {
        Err(ParseError::InvalidSyntax { diagnostics, .. }) => diagnostics,
        other => {
            return Err(format!("expected InvalidSyntax, got {:?}", other.err()).into());
        }
    };
    let missing = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.kind == SyntaxIssueKind::Missing)
        .ok_or("an omitted semicolon must produce a MISSING node")?;
    assert_eq!(
        missing.start_byte, missing.end_byte,
        "a MISSING node marks an insertion point, so its span must be empty"
    );
    assert_eq!(
        source
            .get(missing.start_byte..missing.end_byte)
            .map(str::len),
        Some(0),
        "the span must cover no source text, so it cannot underline anything"
    );

    // The offset is still addressable, so the line and column a caller
    // renders from it remain meaningful even though the span is empty.
    assert!(
        missing.start_byte <= source.len() && source.is_char_boundary(missing.start_byte),
        "the insertion point is a character boundary inside the source"
    );
    Ok(())
}

#[test]
fn an_invalid_syntax_diagnostic_points_at_the_earliest_recovery_node()
-> Result<(), Box<dyn std::error::Error>> {
    let source = "fn main() {}\nfn broken( {\nfn also( {\n";
    let refusal = match try_parse(source, Language::Rust) {
        Err(refusal @ ParseError::InvalidSyntax { .. }) => refusal,
        other => {
            return Err(format!("expected InvalidSyntax, got {:?}", other.err()).into());
        }
    };
    let reported = refusal.to_diagnostic("broken.rs", source);
    assert_eq!(reported.severity(), Severity::Error);
    assert_eq!(
        reported.span().start.line,
        2,
        "the rendered refusal names the first broken line, not end of file"
    );
    assert!(
        reported.span().byte_range().start < source.len(),
        "the span is inside the source"
    );
    assert!(
        reported.render().starts_with("broken.rs:2:"),
        "the rendered form carries the location: {}",
        reported.render()
    );

    let whole_file = ParseError::AstTooLarge {
        language: "Rust",
        observed: 9,
        limit: 8,
    }
    .to_diagnostic("big.rs", source);
    assert!(
        whole_file.span().is_empty() && whole_file.span().start.byte == source.len(),
        "a whole-file refusal stays zero-width at the end"
    );
    Ok(())
}

#[test]
fn a_truncated_syntax_report_keeps_the_earliest_errors_in_source_order()
-> Result<(), Box<dyn std::error::Error>> {
    // Far more broken items than the ceiling, one per line, so the error
    // on the first line is the one a reader needs and the walk, which runs
    // siblings in reverse, meets it last.
    let source = "fn broken( {\n".repeat(MAX_SYNTAX_DIAGNOSTICS * 3);
    let (diagnostics, truncated) = match try_parse(&source, Language::Rust) {
        Err(ParseError::InvalidSyntax {
            diagnostics,
            diagnostics_truncated,
            ..
        }) => (diagnostics, diagnostics_truncated),
        other => {
            return Err(format!("expected InvalidSyntax, got {:?}", other.err()).into());
        }
    };
    assert!(
        truncated,
        "more recovery nodes than the ceiling were observed"
    );
    assert_eq!(diagnostics.len(), MAX_SYNTAX_DIAGNOSTICS);
    let first = diagnostics
        .first()
        .ok_or("a truncated report is not empty")?;
    assert!(
        first.start_byte < "fn broken( {\n".len(),
        "the first retained error is on the first line, not {first:?}"
    );
    assert!(
        diagnostics.windows(2).all(|pair| matches!(
            pair,
            [earlier, later] if syntax_position(earlier) <= syntax_position(later)
        )),
        "the retained errors are in source order"
    );
    Ok(())
}

#[test]
fn the_refusal_and_the_report_count_the_same_recovery_nodes()
-> Result<(), Box<dyn std::error::Error>> {
    // Under the ceiling, the refusal's diagnostics and the rendered report
    // are two views of one walk's node set; they must not disagree.
    let source = "fn main() {\n    let x = ;\n}\nfn broken( {\n";
    let refused = match try_parse(source, Language::Rust) {
        Err(ParseError::InvalidSyntax {
            diagnostics,
            diagnostics_truncated: false,
            ..
        }) => diagnostics,
        other => {
            return Err(format!("expected an untruncated refusal, got {:?}", other.err()).into());
        }
    };
    let tree = parse(source, Language::Rust);
    assert_eq!(tree_recovery_count(&tree), refused.len());
    assert_eq!(
        tree_diagnostics("main.rs", &tree, Language::Rust.name()).len(),
        refused.len()
    );
    Ok(())
}

#[test]
fn syntax_diagnostics_stop_at_the_declared_bound() {
    let mut diagnostics = Vec::new();
    let mut truncated = false;
    let diagnostic = SyntaxDiagnostic {
        kind: SyntaxIssueKind::Error,
        start_byte: 0,
        end_byte: 0,
    };
    for _ in 0..MAX_SYNTAX_DIAGNOSTICS + 1 {
        push_syntax_diagnostic(diagnostic, &mut diagnostics, &mut truncated);
    }
    assert_eq!(
        diagnostics.len(),
        MAX_SYNTAX_DIAGNOSTICS,
        "retained diagnostic memory is bounded by the public ceiling"
    );
    assert!(truncated, "omitted diagnostics are reported as truncated");
}

#[test]
fn tree_diagnostics_reads_the_source_from_the_tree() {
    let source = "fn main() {}\nfn broken( {\n";
    let language = Language::Rust;
    let tree = parse(source, language);
    let found = tree_diagnostics("main.rs", &tree, language.name());
    assert!(
        !found.is_empty(),
        "the malformed function reports a recovery"
    );
    assert_eq!(
        found[0].span().start.line,
        2,
        "the source comes from the tree, so no span can resolve wrongly"
    );
}

#[test]
fn a_whole_file_refusal_reports_an_empty_error_span() {
    let refusal = ParseError::SourceTooLarge {
        actual: 9,
        limit: 8,
    };
    let reported = refusal.to_diagnostic("big.rs", "too big");
    assert_eq!(reported.severity(), Severity::Error);
    assert!(
        reported.span().is_empty(),
        "a whole-file refusal has no width"
    );
}
