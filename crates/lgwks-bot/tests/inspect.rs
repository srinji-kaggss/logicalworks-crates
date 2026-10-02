#![cfg(feature = "inspect")]
//! Public-API acceptance for typed in-process structural inspection (R8).
//!
//! Every test here drives the shipped operation through its public interface.
//! `inspect` is a host-only feature (it draws native grammars through
//! `lgwks_ast`), so these compile only in an `inspect` build and the gate's
//! `full` lane is what exercises them.

use lgwks_bot::inspect::{
    Budgets, IncompleteReason, InspectRequest, Inspection, MAX_PREVIEW_BYTES, RuleSet, Scope,
    UnsupportedReason, Verdict, inspect,
};

/// The subject every span assertion slices.
const UNWRAP_SUBJECT: &str = "fn f() { let x = g().unwrap(); }";

/// A wide subject with one violation per line, for density and budget tests.
fn wide_subject() -> String {
    (0..64)
        .map(|index| format!("fn f{index}() {{ let x = unwrap(); }}\n"))
        .collect()
}

#[test]
fn a_clean_subject_is_clean_with_full_rule_coverage() -> Result<(), Box<dyn std::error::Error>> {
    let report = inspect(&InspectRequest::new(
        "src/lib.rs",
        "pub fn add(a: u32, b: u32) -> u32 { a + b }\n",
    ));
    assert!(
        matches!(
            report.verdict(),
            Verdict::Clean {
                scope: Scope::Structural
            }
        ),
        "a subject with no matched rule must be clean, got {:?}",
        report.verdict()
    );
    assert_eq!(
        report.language(),
        Some("rust"),
        "the extension selects the grammar"
    );
    assert_eq!(report.rule_identity(), "lgwks/structural");
    assert_eq!(report.rule_revision(), 1);
    assert!(
        !report.coverage().is_empty(),
        "every shipped rule reports coverage"
    );
    assert!(
        report
            .coverage()
            .iter()
            .all(|coverage| coverage.supported() && coverage.evaluated()),
        "every shipped rule is supported and evaluated for Rust: {:?}",
        report.coverage()
    );
    assert!(
        report.findings().is_empty(),
        "a clean subject retains no finding"
    );
    Ok(())
}

#[test]
fn a_violation_carries_a_stable_rule_id_and_an_exact_span() -> Result<(), Box<dyn std::error::Error>>
{
    let report = inspect(&InspectRequest::new("src/lib.rs", UNWRAP_SUBJECT));
    assert!(
        matches!(report.verdict(), Verdict::Violations { count: 1 }),
        "one unwrap is one violation, got {:?}",
        report.verdict()
    );
    let found = report
        .findings()
        .first()
        .ok_or("the unwrap must be reported")?;
    assert_eq!(
        found.rule_id(),
        "rust/no-unwrap",
        "the rule id is the stable identity"
    );
    let (start, end) = found.byte_range();
    assert_eq!(
        &UNWRAP_SUBJECT[start..end],
        "unwrap",
        "the span is exactly the matched identifier"
    );
    assert!(!found.preview_truncated(), "a short match is not truncated");
    Ok(())
}

#[test]
fn invalid_syntax_is_incomplete_and_never_a_clean_report() -> Result<(), Box<dyn std::error::Error>>
{
    let report = inspect(&InspectRequest::new("src/lib.rs", "fn broken( {\n"));
    assert!(
        matches!(
            report.verdict(),
            Verdict::Incomplete {
                reason: IncompleteReason::SyntaxRecovery { .. }
            }
        ),
        "a recovered parse must be incomplete, got {:?}",
        report.verdict()
    );
    assert!(
        report.findings().is_empty(),
        "a refused parse reports no findings, and never a clean report"
    );
    assert!(
        report
            .coverage()
            .iter()
            .any(|coverage| coverage.supported() && !coverage.evaluated()),
        "a supported rule a refusal stopped from evaluating says so: {:?}",
        report.coverage()
    );
    Ok(())
}

#[test]
fn a_compiled_grammar_the_rules_cannot_read_is_unsupported()
-> Result<(), Box<dyn std::error::Error>> {
    let report = inspect(&InspectRequest::new("app.py", "def f():\n    return g()\n"));
    assert!(
        matches!(
            report.verdict(),
            Verdict::Unsupported {
                reason: UnsupportedReason::RulesNotSupportedForLanguage { .. }
            }
        ),
        "a grammar the rules do not speak is unsupported, got {:?}",
        report.verdict()
    );
    assert_eq!(report.language(), Some("python"));
    assert!(
        report
            .coverage()
            .iter()
            .all(|coverage| !coverage.supported()),
        "no rule is supported for a language the rules cannot read"
    );
    Ok(())
}

#[test]
fn an_unclaimed_artifact_has_no_compiled_grammar() -> Result<(), Box<dyn std::error::Error>> {
    let report = inspect(&InspectRequest::new("notes.lgwks-unclaimed", "whatever\n"));
    assert!(
        matches!(
            report.verdict(),
            Verdict::Unsupported {
                reason: UnsupportedReason::NoCompiledGrammar { .. }
            }
        ),
        "an extension no grammar claims is unsupported, got {:?}",
        report.verdict()
    );
    assert_eq!(report.language(), None, "no language was resolved");
    Ok(())
}

#[test]
fn unknown_and_mismatched_rule_sets_are_refused_before_any_parse()
-> Result<(), Box<dyn std::error::Error>> {
    let unknown = inspect(
        &InspectRequest::new("src/lib.rs", UNWRAP_SUBJECT).rules(RuleSet::new("example/other", 1)),
    );
    assert!(
        matches!(
            unknown.verdict(),
            Verdict::Unsupported {
                reason: UnsupportedReason::UnknownRuleSet { .. }
            }
        ),
        "an unknown rule set is refused, got {:?}",
        unknown.verdict()
    );
    let mismatched = inspect(
        &InspectRequest::new("src/lib.rs", UNWRAP_SUBJECT)
            .rules(RuleSet::new("lgwks/structural", 99)),
    );
    assert!(
        matches!(
            mismatched.verdict(),
            Verdict::Unsupported {
                reason: UnsupportedReason::RuleSetRevision {
                    found: 99,
                    supported: 1
                }
            }
        ),
        "a revision mismatch is refused by number, got {:?}",
        mismatched.verdict()
    );
    Ok(())
}

#[test]
fn a_declared_language_version_is_undecidable_not_clean() -> Result<(), Box<dyn std::error::Error>>
{
    let report =
        inspect(&InspectRequest::new("src/lib.rs", "fn f() {}\n").language_version("2021"));
    assert!(
        matches!(report.verdict(), Verdict::Undecidable { .. }),
        "a version this build cannot confirm is undecidable, got {:?}",
        report.verdict()
    );
    assert!(
        !matches!(report.verdict(), Verdict::Clean { .. }),
        "undecidable must never read as clean"
    );
    Ok(())
}

#[test]
fn every_budget_has_its_own_refusal() -> Result<(), Box<dyn std::error::Error>> {
    let wide = wide_subject();

    let too_large = inspect(
        &InspectRequest::new("src/lib.rs", UNWRAP_SUBJECT)
            .budgets(Budgets::new().with_source_bytes(4)),
    );
    assert!(
        matches!(
            too_large.verdict(),
            Verdict::Incomplete {
                reason: IncompleteReason::SourceTooLarge { .. }
            }
        ),
        "source bound: {:?}",
        too_large.verdict()
    );

    let nodes =
        inspect(&InspectRequest::new("src/lib.rs", &wide).budgets(Budgets::new().with_nodes(3)));
    assert!(
        matches!(
            nodes.verdict(),
            Verdict::Incomplete {
                reason: IncompleteReason::NodeBudgetExceeded { .. }
            }
        ),
        "node bound: {:?}",
        nodes.verdict()
    );

    let depth = inspect(
        &InspectRequest::new("src/lib.rs", UNWRAP_SUBJECT).budgets(Budgets::new().with_depth(1)),
    );
    assert!(
        matches!(
            depth.verdict(),
            Verdict::Incomplete {
                reason: IncompleteReason::DepthBudgetExceeded { .. }
            }
        ),
        "depth bound: {:?}",
        depth.verdict()
    );

    let work = inspect(
        &InspectRequest::new("src/lib.rs", UNWRAP_SUBJECT).budgets(Budgets::new().with_work(0)),
    );
    assert!(
        matches!(
            work.verdict(),
            Verdict::Incomplete {
                reason: IncompleteReason::WorkBudgetExceeded { .. }
            }
        ),
        "work bound: {:?}",
        work.verdict()
    );

    let findings =
        inspect(&InspectRequest::new("src/lib.rs", &wide).budgets(Budgets::new().with_findings(1)));
    assert!(
        matches!(
            findings.verdict(),
            Verdict::Incomplete {
                reason: IncompleteReason::FindingsBudgetExceeded { .. }
            }
        ),
        "findings bound: {:?}",
        findings.verdict()
    );

    let output = inspect(
        &InspectRequest::new("src/lib.rs", UNWRAP_SUBJECT)
            .budgets(Budgets::new().with_output_bytes(0)),
    );
    assert!(
        matches!(
            output.verdict(),
            Verdict::Incomplete {
                reason: IncompleteReason::OutputBudgetExceeded { .. }
            }
        ),
        "output bound: {:?}",
        output.verdict()
    );
    Ok(())
}

#[test]
fn the_report_round_trips_and_preserves_identity_spans_and_coverage()
-> Result<(), Box<dyn std::error::Error>> {
    let report = inspect(&InspectRequest::new("src/lib.rs", UNWRAP_SUBJECT));
    let text = report.to_json()?;
    let decoded: Inspection = lgwks_bot::json::from_str(&text)?;
    assert_eq!(
        decoded, report,
        "a report must survive a JSON round trip unchanged"
    );
    assert_eq!(
        decoded.subject_digest(),
        report.subject_digest(),
        "the input digest survives serialization"
    );
    assert_eq!(decoded.rule_revision(), 1, "the rule revision survives");
    assert_eq!(
        decoded.rule_identity(),
        "lgwks/structural",
        "the rule identity survives"
    );
    assert_eq!(
        decoded.coverage().len(),
        report.coverage().len(),
        "coverage survives serialization"
    );
    assert_eq!(
        decoded.findings().first().map(|found| found.byte_range()),
        report.findings().first().map(|found| found.byte_range()),
        "exact spans survive serialization"
    );
    Ok(())
}

#[test]
fn a_match_longer_than_the_preview_budget_is_truncated_with_its_full_span_kept()
-> Result<(), Box<dyn std::error::Error>> {
    // The macro-invocation node spans the whole `panic!(..)`, so a long message
    // pushes the matched text past `MAX_PREVIEW_BYTES`: this is the only shipped
    // rule whose match can exceed the preview budget, and it is why the budget
    // exists.
    let message = "x".repeat(MAX_PREVIEW_BYTES * 3);
    let subject = format!("fn f() {{ panic!(\"{message}\"); }}\n");
    let report = inspect(&InspectRequest::new("src/lib.rs", &subject));
    let found = report
        .findings()
        .iter()
        .find(|found| found.rule_id() == "rust/no-panic")
        .ok_or("the long panic! must be reported")?;

    assert!(
        found.preview_truncated(),
        "a match past the preview budget must report truncation"
    );
    assert_eq!(
        found.preview().chars().count(),
        MAX_PREVIEW_BYTES,
        "the bounded preview keeps exactly the budget's worth of characters"
    );
    let (start, end) = found.byte_range();
    assert!(
        end.saturating_sub(start) > MAX_PREVIEW_BYTES,
        "the exact span is the full match, not the preview: {start}..{end}"
    );
    assert!(
        subject[start..end].starts_with(found.preview()),
        "the preview is a prefix of the exact span it was cut from"
    );
    assert!(
        subject[start..end].starts_with("panic!("),
        "the span still names the matched macro invocation"
    );
    Ok(())
}

#[test]
fn an_ordinary_and_an_agent_consumer_share_one_operation() -> Result<(), Box<dyn std::error::Error>>
{
    // The ordinary Rust consumer gates a file through the public operation.
    let ordinary = inspect(&InspectRequest::new("src/lib.rs", UNWRAP_SUBJECT));
    // The agent consumer reads the same typed report; it builds no scanner, no
    // queue, no retry controller and no authority bypass of its own.
    let agent = inspect(&InspectRequest::new("src/lib.rs", UNWRAP_SUBJECT));
    assert_eq!(
        ordinary, agent,
        "the same request must produce the same report for both callers"
    );
    assert!(
        matches!(agent.verdict(), Verdict::Violations { .. }),
        "both callers see the same typed verdict"
    );
    assert!(
        !Inspection::assurance().is_empty(),
        "the assurance ceiling travels with the report"
    );
    Ok(())
}
