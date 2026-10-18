//! Typed in-process structural code inspection (R8): check risky code over its
//! structure, never by executing it.
//!
//! An AI that needs risky code checked calls the bot to check it **in process**.
//! This module is that operation. It reads the subject's bytes, asks
//! [`lgwks_ast`] to parse them, and walks the resulting tree against a fixed,
//! versioned structural rule set. It never compiles the subject, never imports
//! it, never runs a build script, never invokes a shell, and never loads a
//! dynamic library from it: the only library it calls is the parser, and the
//! subject's instructions stay data at every step.
//!
//! # The verdict is typed and never free text
//!
//! [`Inspection::verdict`] is one of [`Verdict::Clean`], [`Verdict::Violations`],
//! [`Verdict::Unsupported`], [`Verdict::Undecidable`], [`Verdict::Incomplete`] or
//! [`Verdict::InfrastructureFailure`]. A refusal to decide is a state, not a
//! clean report: a parse that recovered, a budget that ran out, a grammar that
//! is not compiled, and a rule set this build does not implement each land on
//! their own arm, and none of them can be read as "no violation found".
//!
//! # What a clean result is worth
//!
//! [`Verdict::Clean`] means *no configured structural rule matched, within an
//! explicitly complete inspected scope*. It is **not** proof that the subject
//! is free of logical bugs, free of vulnerabilities, or safe to execute. The
//! grammar a subject parsed under is not evidence that a rule speaks its
//! syntax; [`Inspection::coverage`] reports, per rule, whether this build
//! supported it for the observed language, and that is the whole of the claim.
//! [`ASSURANCE`] states the ceiling in one sentence a caller can quote.
//!
//! # Bounds are separate axes
//!
//! Source bytes, parser nodes, traversal depth, rule evaluations and emitted
//! findings are **different** resources and are bounded by different fields on
//! [`Budgets`]. Refusing before avoidable amplification is the rule: the byte
//! bound is checked before the parser runs, the node/depth/work bounds while
//! walking, and the findings/output bounds before a finding is retained.
//! [`Inspection::resources`] reports what was actually charged.
//!
//! This does not claim in-process preemption of arbitrary stuck *foreign* code:
//! a hostile grammar can still spend time inside the parser, which this crate
//! does not own. What it promises is that **this crate** adds no execution of
//! the subject and no unbounded walk of its own.
//!
//! # The parser boundary
//!
//! The parser is third-party Rust (`ast-grep-core` over `tree-sitter`
//! grammars). It is trusted to be memory-safe under `#![forbid(unsafe_code)]`
//! at this crate's own root, but it is not vouched for by this module beyond
//! that: a grammar bug is a parser fault, not an inspection verdict, and
//! [`Verdict::InfrastructureFailure`] is where such a refusal is reported
//! rather than manufactured into a clean result.
//!
//! [`Verdict::Clean`]: crate::inspect::Verdict::Clean
//! [`Verdict::Violations`]: crate::inspect::Verdict::Violations
//! [`Verdict::Unsupported`]: crate::inspect::Verdict::Unsupported
//! [`Verdict::Undecidable`]: crate::inspect::Verdict::Undecidable
//! [`Verdict::Incomplete`]: crate::inspect::Verdict::Incomplete
//! [`Verdict::InfrastructureFailure`]: crate::inspect::Verdict::InfrastructureFailure
//! [`ASSURANCE`]: crate::inspect::ASSURANCE
//!
//! # Example
//!
//! `a_panic_invocation_is_the_panic_rules_whole_subject` (`inspect/tests.rs`) runs it: a request
//! for `src/lib.rs` holding `fn f() { panic!("boom"); }` is a `Verdict::Violations`, and its first
//! finding is the shipped rule `rust/no-panic`.

mod rules;
mod types;
mod walk;

use lgwks_ast::{Language, ParseError};
use lgwks_std::hash;

use rules::{coverage_for, rules_speak};
#[doc(hidden)]
pub use types::{
    Budgets, Finding, IncompleteReason, InspectRequest, Inspection, Resources, RuleCoverage,
    RuleSet, Scope, UnsupportedReason, Verdict,
};

/// The wire version of the inspection report.
pub const INSPECTION_VERSION: u32 = 1;

/// The assurance ceiling a clean report carries, in one quotable sentence.
///
/// A finite structural rule set cannot prove a program correct, and a report
/// that let a reader hear otherwise would be the one claim this operation must
/// never make.
pub const ASSURANCE: &str = "structural rule set only: a clean result means no configured \
     structural rule matched within a complete inspected scope, and is not proof the subject is \
     free of logical bugs, free of vulnerabilities, or safe to execute";

/// The most findings one inspection retains before it is marked incomplete.
pub const MAX_FINDINGS: usize = 1_024;

/// The most bytes of one finding's source preview.
pub const MAX_PREVIEW_BYTES: usize = 200;

/// The most bytes of emitted finding text (rule id plus preview) one
/// inspection retains before it is marked incomplete.
pub const MAX_OUTPUT_BYTES: usize = 256 * 1024;

#[cfg(test)]
mod tests;

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
    let walked = walk::walk(&parsed, request.subject, &request.budgets, enforce);
    let resources = Resources {
        nodes: walked.nodes,
        max_depth: walked.max_depth,
        work: walked.work,
        findings: walked.findings.len(),
        output_bytes: walked.output_bytes,
    };
    // Findings and the verdict are independent: an incomplete walk still
    // carries every finding it retained, and the verdict names the incompleteness
    // rather than a clean scope.
    let verdict = match walked.incomplete {
        Some(reason) => Verdict::Incomplete { reason },
        None if walked.findings.is_empty() => Verdict::Clean {
            scope: request.scope,
        },
        None => Verdict::Violations {
            count: walked.findings.len(),
        },
    };
    base(
        verdict,
        Some(language_name),
        coverage,
        walked.findings,
        resources,
    )
}
