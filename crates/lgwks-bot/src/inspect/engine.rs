//! The inspection engine: the shipped rule table, the parse-and-decide operation, and the bounded walk that charges every budget as it goes.

use lgwks_ast::{Language, ParseError};
use lgwks_std::hash;

use super::{
    Budgets, Finding, INSPECTION_VERSION, IncompleteReason, InspectRequest, Inspection,
    MAX_PREVIEW_BYTES, Resources, RuleCoverage, RuleSet, UnsupportedReason, Verdict,
};

/// One shipped structural rule.
pub(super) struct Rule {
    /// The stable rule id.
    rule_id: &'static str,
    /// The syntax the rule inspects, in one line.
    evidence: &'static str,
}

/// The shipped rule set, in report order.
pub(super) const RULES: &[Rule] = &[
    Rule {
        rule_id: "rust/no-unwrap",
        evidence: "an identifier named `unwrap` or `expect` (a fallible-operation panic site)",
    },
    Rule {
        rule_id: "rust/no-todo",
        evidence: "a `todo!` or `unimplemented!` macro invocation",
    },
    Rule {
        rule_id: "rust/no-panic",
        evidence: "a `panic!` macro invocation",
    },
];

/// The language the shipped rules speak.
fn rules_speak(language: Language) -> bool {
    language == Language::Rust
}

/// Whether `rule_id` matches the node described by `node_kind` and `node_text`.
fn rule_matches(rule_id: &str, node_kind: &str, node_text: &str) -> bool {
    match rule_id {
        "rust/no-unwrap" => {
            matches!(node_kind, "identifier" | "field_identifier")
                && matches!(node_text, "unwrap" | "expect")
        }
        "rust/no-todo" => {
            node_kind == "macro_invocation"
                && matches!(macro_head(node_text), "todo" | "unimplemented")
        }
        "rust/no-panic" => node_kind == "macro_invocation" && macro_head(node_text) == "panic",
        _ => false,
    }
}

/// The bare macro name at the head of a macro invocation's text.
///
/// `todo!("x")` yields `todo`; the optional space before `!` that some
/// formatters insert is absorbed. Anything with no head yields `""`, which
/// matches no rule.
fn macro_head(node_text: &str) -> &str {
    // Two answers, both real: a terminator ends the head there, and text with no
    // terminator *is* its own head. Nothing here is a value standing in for a
    // piece that was missing.
    match node_text.split_once(['!', '(', ' ', '\t', '\n']) {
        Some((head, _)) => head,
        None => node_text,
    }
}

/// A bounded, char-boundary-safe preview of `source[start..end]`.
fn bounded_preview(source: &str, start: usize, end: usize) -> (String, bool) {
    // A byte range that names nothing in this source — a stale node, a parser
    // reporting past the end — has no text to preview. That is the empty preview
    // *with* the truncated flag set, so a caller cannot read it as a node whose
    // text was genuinely empty.
    let Some(slice) = source.get(start..end) else {
        return (String::new(), true);
    };
    // Fewer than MAX_PREVIEW_BYTES chars means the whole slice fits.
    let cutoff = match slice.char_indices().nth(MAX_PREVIEW_BYTES) {
        Some((at, _)) => at,
        None => slice.len(),
    };
    let truncated = cutoff < slice.len();
    (slice[..cutoff].to_owned(), truncated)
}

/// Milliseconds in `duration`, saturating at the representable ceiling.
///
/// Split through the two infallible `Duration` projections so the ceiling comes
/// out of saturating arithmetic rather than out of a narrowing conversion whose
/// failure arm would have had to report a deadline the parser never reached.
/// `u64::MAX` milliseconds is about half a million years, so the ceiling is
/// unreachable by a real parse and exists so a caller asking past it gets a
/// stated bound rather than a wrapped one.
fn deadline_millis(duration: std::time::Duration) -> u64 {
    const MILLIS_PER_SEC: u64 = 1_000;
    duration
        .as_secs()
        .saturating_mul(MILLIS_PER_SEC)
        .saturating_add(u64::from(duration.subsec_millis()))
}

/// The parser this operation calls, as a seam.
///
/// The shipped value is [`lgwks_ast::try_parse`], and the only other value is a
/// fault injected by the crate's own tests: R8 requires the
/// [`Verdict::InfrastructureFailure`] arm to be exercised by a producer, not
/// merely declared, and the compiled Rust grammar never fails. The seam is
/// crate-private, so no caller can substitute a parser and no second parsing
/// implementation exists.
type ParseFn = fn(&str, Language) -> Result<lgwks_ast::Parsed, ParseError>;

/// Inspect `request`'s subject in process and return a typed report.
///
/// This is the whole operation: parse, walk, decide. It performs no effect
/// outside this process, spawns no task, opens no file and runs no subject
/// code.
#[must_use]
pub fn inspect(request: &InspectRequest<'_>) -> Inspection {
    inspect_with(request, lgwks_ast::try_parse)
}

/// The whole operation, parameterized on the parser it calls.
///
/// [`inspect`] passes the shipped parser; the crate's own tests pass a fault to
/// exercise [`Verdict::InfrastructureFailure`]. Every budget is enforced.
fn inspect_with(request: &InspectRequest<'_>, parse: ParseFn) -> Inspection {
    inspect_mode(request, parse, true)
}

/// The whole operation, with the budget walk optionally disarmed.
///
/// `enforce` is `true` for every shipped path. The eager mutant the budget
/// oracles are tested against is `false`: it still parses, counts and records,
/// but never stops on a budget, so a test can show the behavioral oracle rejects
/// it. It is `#[cfg(test)]` and cannot be reached from a shipped build.
#[cfg(test)]
fn inspect_eager(request: &InspectRequest<'_>) -> Inspection {
    inspect_mode(request, lgwks_ast::try_parse, false)
}

/// Run one inspection, parsing through `parse` and enforcing budgets when
/// `enforce` is set.
fn inspect_mode(request: &InspectRequest<'_>, parse: ParseFn, enforce: bool) -> Inspection {
    let digest = hash::blake3(request.subject.as_bytes()).to_hex();
    let coverage = coverage_for(&request.rules, request.language, false);
    let base = |verdict: Verdict,
                language: Option<String>,
                coverage: Vec<RuleCoverage>,
                findings: Vec<Finding>,
                resources: Resources| Inspection {
        version: INSPECTION_VERSION,
        artifact: request.artifact.to_owned(),
        subject_digest: digest.clone(),
        language,
        rule_identity: request.rules.identity().to_owned(),
        rule_revision: request.rules.revision(),
        scope: request.scope,
        verdict,
        coverage,
        findings,
        resources,
    };
    let empty = Resources {
        nodes: 0,
        max_depth: 0,
        work: 0,
        findings: 0,
        output_bytes: 0,
    };

    // A rule set this build does not implement is refused before any parse: the
    // caller asked for rules that do not exist here, and running different ones
    // would answer a different question.
    if request.rules.identity() != RuleSet::STRUCTURAL_V1.identity() {
        return base(
            Verdict::Unsupported {
                reason: UnsupportedReason::UnknownRuleSet {
                    identity: request.rules.identity().to_owned(),
                },
            },
            None,
            coverage,
            Vec::new(),
            empty,
        );
    }
    if request.rules.revision() != RuleSet::STRUCTURAL_V1.revision() {
        return base(
            Verdict::Unsupported {
                reason: UnsupportedReason::RuleSetRevision {
                    found: request.rules.revision(),
                    supported: RuleSet::STRUCTURAL_V1.revision(),
                },
            },
            None,
            coverage,
            Vec::new(),
            empty,
        );
    }

    // Resolve the language. An explicit language wins; otherwise the artifact's
    // extension chooses.
    let resolved = request
        .language
        .or_else(|| lgwks_ast::detect(request.artifact));
    let Some(language) = resolved else {
        return base(
            Verdict::Unsupported {
                reason: UnsupportedReason::NoCompiledGrammar {
                    artifact: request.artifact.to_owned(),
                },
            },
            None,
            coverage,
            Vec::new(),
            empty,
        );
    };
    let language_name = language.name().to_owned();
    // Coverage is reported as supported-but-not-yet-evaluated until a parse
    // succeeds; a refusal must not claim any rule ran.
    let coverage = coverage_for(&request.rules, Some(language), false);

    // A declared language version cannot be confirmed: the compiled grammar
    // carries no version identity, so the operation says the property is
    // undecidable rather than checking the subject against a version it cannot
    // name. This is the "cannot decide returns undecidable" arm of R8.
    if let Some(version) = request.language_version {
        return base(
            Verdict::Undecidable {
                property: format!(
                    "language version `{version}`: the compiled grammar carries no version \
                     identity, so this build will not claim to have checked that exact version"
                ),
            },
            Some(language_name),
            coverage,
            Vec::new(),
            empty,
        );
    }

    // A compiled grammar the rules do not speak is not a clean subject; it is a
    // subject this rule set cannot see.
    if !rules_speak(language) {
        return base(
            Verdict::Unsupported {
                reason: UnsupportedReason::RulesNotSupportedForLanguage {
                    language: language_name.clone(),
                },
            },
            Some(language_name),
            coverage,
            Vec::new(),
            empty,
        );
    }

    // The byte bound is checked before the parser is handed anything.
    if request.subject.len() > request.budgets.max_source_bytes {
        return base(
            Verdict::Incomplete {
                reason: IncompleteReason::SourceTooLarge {
                    actual: request.subject.len(),
                    limit: request.budgets.max_source_bytes,
                },
            },
            Some(language_name),
            coverage,
            Vec::new(),
            empty,
        );
    }

    let parsed = match parse(request.subject, language) {
        Ok(parsed) => parsed,
        Err(ParseError::InvalidSyntax {
            diagnostics,
            diagnostics_truncated,
            ..
        }) => {
            return base(
                Verdict::Incomplete {
                    reason: IncompleteReason::SyntaxRecovery {
                        recovery_nodes: diagnostics.len(),
                        diagnostics_truncated,
                    },
                },
                Some(language_name),
                coverage,
                Vec::new(),
                empty,
            );
        }
        Err(ParseError::SourceTooLarge { actual, limit }) => {
            return base(
                Verdict::Incomplete {
                    reason: IncompleteReason::SourceTooLarge { actual, limit },
                },
                Some(language_name),
                coverage,
                Vec::new(),
                empty,
            );
        }
        Err(ParseError::AstTooLarge {
            observed, limit, ..
        }) => {
            return base(
                Verdict::Incomplete {
                    reason: IncompleteReason::NodeBudgetExceeded { observed, limit },
                },
                Some(language_name),
                coverage,
                Vec::new(),
                empty,
            );
        }
        // The parser's own depth refusals — the tree it built, or the markdown
        // containers it was never handed — are a depth budget reached, so they
        // are incomplete rather than an infrastructure fault: a deeply nested
        // file is a fact about the subject.
        Err(ParseError::AstTooDeep {
            observed, limit, ..
        })
        | Err(ParseError::ContainerNestingTooDeep {
            observed, limit, ..
        }) => {
            return base(
                Verdict::Incomplete {
                    reason: IncompleteReason::DepthBudgetExceeded {
                        reached: observed,
                        limit,
                    },
                },
                Some(language_name),
                coverage,
                Vec::new(),
                empty,
            );
        }
        Err(ParseError::TimedOut { after, .. }) => {
            return base(
                Verdict::Incomplete {
                    reason: IncompleteReason::ParseDeadlineExceeded {
                        deadline_ms: deadline_millis(after),
                    },
                },
                Some(language_name),
                coverage,
                Vec::new(),
                empty,
            );
        }
        Err(ParseError::ParserUnavailable { language, detail }) => {
            return base(
                Verdict::InfrastructureFailure {
                    cause: format!("{language} parser unavailable: {detail}"),
                },
                Some(language_name),
                coverage,
                Vec::new(),
                empty,
            );
        }
        // `ParseError` is `#[non_exhaustive]`: a refusal variant added upstream
        // is an infrastructure fault this build cannot classify, and it is
        // reported as one rather than folded into a clean verdict.
        Err(other) => {
            return base(
                Verdict::InfrastructureFailure {
                    cause: other.to_string(),
                },
                Some(language_name),
                coverage,
                Vec::new(),
                empty,
            );
        }
    };

    // The parse succeeded, so every supported rule is now evaluated.
    let coverage = coverage_for(&request.rules, Some(language), true);
    let walk = walk(&parsed, request.subject, &request.budgets, enforce);
    let resources = Resources {
        nodes: walk.nodes,
        max_depth: walk.max_depth,
        work: walk.work,
        findings: walk.findings.len(),
        output_bytes: walk.output_bytes,
    };
    // Findings and the verdict are independent: an incomplete walk still
    // carries every finding it retained, and the verdict names the incompleteness
    // rather than a clean scope.
    let verdict = match walk.incomplete {
        Some(reason) => Verdict::Incomplete { reason },
        None if walk.findings.is_empty() => Verdict::Clean {
            scope: request.scope,
        },
        None => Verdict::Violations {
            count: walk.findings.len(),
        },
    };
    base(
        verdict,
        Some(language_name),
        coverage,
        walk.findings,
        resources,
    )
}

/// Per-rule coverage for `rules` under `language`.
fn coverage_for(rules: &RuleSet, language: Option<Language>, parsed: bool) -> Vec<RuleCoverage> {
    // A rule set other than the shipped one has no coverage to describe.
    let known = rules.identity() == RuleSet::STRUCTURAL_V1.identity()
        && rules.revision() == RuleSet::STRUCTURAL_V1.revision();
    RULES
        .iter()
        .map(|rule| {
            let supported = known && language.is_some_and(rules_speak);
            RuleCoverage {
                rule_id: rule.rule_id.to_owned(),
                supported,
                evaluated: supported && parsed,
                evidence: rule.evidence.to_owned(),
            }
        })
        .collect()
}

/// What one bounded walk accumulated.
struct WalkOutcome {
    /// Nodes visited.
    nodes: usize,
    /// Deepest node visited.
    max_depth: usize,
    /// Rule evaluations performed.
    work: usize,
    /// Findings retained.
    findings: Vec<Finding>,
    /// Bytes of finding text retained.
    output_bytes: usize,
    /// Why the walk stopped early, if it did.
    incomplete: Option<IncompleteReason>,
}

/// One visited node's frame: the node, its remaining child index, and its depth.
type Frame<'t> = (lgwks_ast::AstNode<'t>, usize, usize);

/// Walk `parsed` against the shipped rules under `budgets`.
///
/// The walk is iterative with one frame per active ancestor, so its resident
/// state follows depth rather than sibling fan-out, and every budget is charged
/// as the work happens rather than after it.
fn walk(parsed: &lgwks_ast::Parsed, source: &str, budgets: &Budgets, enforce: bool) -> WalkOutcome {
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

#[cfg(test)]
mod tests;
