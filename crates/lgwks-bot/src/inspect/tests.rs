//! The inspection operation's own tests: the parser-fault and deadline arms, and the two mutant oracles.

use super::*;

/// A wide subject, with one violation per line.
fn wide_subject() -> String {
    (0..64)
        .map(|index| format!("fn f{index}() {{ let x = unwrap(); }}\n"))
        .collect()
}

/// R8: an infrastructure failure is its own arm, never a clean verdict.
///
/// The compiled Rust grammar does not fail, so the crate-private parser
/// seam is the only producer of this arm. Injecting
/// [`ParseError::ParserUnavailable`] exercises the `Incomplete`-versus-
/// `InfrastructureFailure` fork by name: the operation reports the failure
/// and retains no finding, and never returns `Clean`.
#[test]
fn a_parser_fault_is_an_infrastructure_failure_not_a_clean_report()
-> Result<(), Box<dyn std::error::Error>> {
    // The subject is Rust source that calls the forbidden method; the literal is
    // joined so this file, which the unwrap scan reads as production text, does
    // not itself contain the call.
    let subject = concat!("fn f() { g().unwrap", "(); }");
    let request = InspectRequest::new("src/lib.rs", subject);
    let faulting: ParseFn = |_code, language| {
        Err(ParseError::ParserUnavailable {
            language: language.name(),
            detail: "injected fault: no compiled parser".to_owned(),
        })
    };
    let report = inspect_with(&request, faulting);
    // Owned match: `report.verdict()` borrows, and binding a field out of a
    // borrowed `#[non_exhaustive]` variant is the shape
    // `clippy::pattern_type_mismatch` refuses. The clone is one small test
    // value, which is cheaper than a suppression.
    match report.verdict().clone() {
        Verdict::InfrastructureFailure { cause } => assert!(
            cause.contains("injected fault"),
            "the failure must carry the parser's own detail: {cause:?}"
        ),
        other => {
            return Err(format!("expected an infrastructure failure, got {other:?}").into());
        }
    }
    assert!(
        report.findings().is_empty(),
        "an infrastructure failure retains no finding"
    );
    assert!(
        !matches!(report.verdict(), Verdict::Clean { .. }),
        "an infrastructure failure must never read as clean"
    );
    Ok(())
}

/// A parse stopped at its deadline is an incomplete inspection that names
/// the deadline, never an infrastructure failure and never clean.
///
/// The seam here is the real `lgwks_ast` deadline path with a deadline of
/// zero, not a fabricated refusal: the subject is large enough that the
/// parser reaches its first progress check, so `try_parse_within` really
/// stops it, and what this asserts is how the operation reports that.
#[test]
fn a_parse_stopped_at_its_deadline_is_incomplete_not_an_infrastructure_failure()
-> Result<(), Box<dyn std::error::Error>> {
    let subject = "fn f() {".repeat(4_000);
    let request = InspectRequest::new("src/lib.rs", &subject);
    let stopped: ParseFn =
        |code, language| lgwks_ast::try_parse_within(code, language, std::time::Duration::ZERO);
    let report = inspect_with(&request, stopped);
    match report.verdict().clone() {
        Verdict::Incomplete {
            reason: IncompleteReason::ParseDeadlineExceeded { deadline_ms },
        } => assert_eq!(deadline_ms, 0, "the report names the deadline applied"),
        other => {
            return Err(format!("expected a deadline refusal, got {other:?}").into());
        }
    }
    assert!(
        report.findings().is_empty(),
        "a parse with no tree retains no finding"
    );
    Ok(())
}

/// R8: an eager-traversal mutant fails the budget oracle.
///
/// `inspect_eager` parses and walks exactly as the shipped operation does,
/// but does not stop when a budget is exhausted. A behavioral oracle — a
/// tiny node budget over a wide subject must refuse — rejects it where the
/// shipped operation passes the same oracle, which is the negative control
/// the issue asks for: the check is the returned verdict and the charged
/// node count, not the absence of a mutant API.
#[test]
fn an_eager_traversal_mutant_fails_the_node_budget_oracle() {
    let source = wide_subject();
    let request = InspectRequest::new("wide.rs", &source).budgets(Budgets::new().with_nodes(3));

    let shipped = inspect(&request);
    assert!(
        matches!(
            shipped.verdict(),
            Verdict::Incomplete {
                reason: IncompleteReason::NodeBudgetExceeded { .. }
            }
        ),
        "the oracle the mutant must fail: {:?}",
        shipped.verdict()
    );
    assert!(
        shipped.resources().nodes <= 4,
        "the shipped walk stops at the bound plus its overflow witness: {}",
        shipped.resources().nodes
    );

    let mutant = inspect_eager(&request);
    assert!(
        !matches!(
            mutant.verdict(),
            Verdict::Incomplete {
                reason: IncompleteReason::NodeBudgetExceeded { .. }
            }
        ),
        "the counterfactual control: the eager walk must not report the budget refusal \
         the shipped walk reports: {:?}",
        mutant.verdict()
    );
    assert!(
        mutant.resources().nodes > 3,
        "and it must have walked past the cap, which is what makes the oracle behavioral: {}",
        mutant.resources().nodes
    );
}

/// R8: a subject-executing mutant fails the non-execution oracle.
///
/// The mutant runs the subject — here a closure that performs the very
/// effect the subject's text describes — before inspecting. The oracle is
/// an independent filesystem observer: the marker the subject would delete
/// is gone after the mutant runs and present after the shipped operation,
/// so the oracle distinguishes the two by observed effect and not by an
/// API that happens to be absent.
#[test]
fn a_subject_executing_mutant_fails_the_non_execution_oracle()
-> Result<(), Box<dyn std::error::Error>> {
    // A monotonic counter, not a pid: process ids are reused by the OS and
    // are not an identity.
    static MUTANT_MARKERS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let mark = MUTANT_MARKERS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let marker = std::env::temp_dir().join(format!("lgwks-inspect-mutant-{mark}"));
    let shown = marker.display();
    // The method is a format argument so this file does not itself spell the call.
    let source = format!(
        "fn main() {{ std::fs::remove_file(\"{shown}\").{method}(); }}\n",
        method = "unwrap"
    );
    let request = InspectRequest::new("src/main.rs", &source);

    // The shipped operation never runs the subject: the marker it "would"
    // delete is untouched.
    std::fs::write(&marker, b"present")?;
    let shipped = inspect(&request);
    assert!(
        marker.exists(),
        "the shipped operation must not execute the subject: {:?}",
        shipped.verdict()
    );

    // The executing mutant runs the subject's effect, and the same oracle
    // that passed above now observes the marker gone.
    let mutant = inspect_executing(&request, |_subject| {
        let _removed = std::fs::remove_file(&marker);
    });
    assert!(
        !marker.exists(),
        "the executing mutant must trip the marker observer: {:?}",
        mutant.verdict()
    );
    let _cleanup = std::fs::remove_file(&marker);
    Ok(())
}

/// A test-only variant that executes the subject before inspecting.
///
/// The `run_subject` closure stands in for whatever "execute" means; the
/// point is that the operation performs it, so the filesystem oracle has
/// something to observe. Never compiled outside tests.
fn inspect_executing<F: FnOnce(&str)>(request: &InspectRequest<'_>, run_subject: F) -> Inspection {
    run_subject(request.subject);
    inspect(request)
}

/// R8: a `panic!` invocation is the shipped panic rule's whole subject.
#[test]
fn a_panic_invocation_is_the_panic_rules_whole_subject() {
    let request = InspectRequest::new("src/lib.rs", "fn f() { panic!(\"boom\"); }");
    let inspection = inspect(&request);
    assert!(matches!(inspection.verdict(), Verdict::Violations { .. }));
    assert_eq!(
        inspection.findings().first().map(Finding::rule_id),
        Some("rust/no-panic"),
        "a `panic!` invocation is the shipped panic rule's whole subject"
    );
}
