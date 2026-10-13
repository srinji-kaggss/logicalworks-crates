//! Tests for the error vocabulary: the retry classes, the certainty map, and the escaping every untrusted field renders through.

use super::{BotError, DispatchCertainty, Escaped, RetryClass};
use crate::cap::{Cap, Deficit, Demand, Shortage};
use crate::session::Terminal;

/// The retry classifier a consumer writes, and the entire reason the two
/// variants exist: it reads the *variant*, never the cause string.
fn may_have_taken_effect(error: &BotError) -> bool {
    matches!(*error, BotError::EffectIndeterminate { .. })
}

#[test]
fn an_indeterminate_effect_is_distinguishable_from_a_domain_error() {
    // Same domain and byte-identical cause text: the variant is the whole
    // difference. That is the defect this closes — before it, a consumer
    // had to infer "may have happened" from an untrusted string, so the
    // only safe reading was the pessimistic one, and the pessimistic
    // reading of "did not happen" is a retry that duplicates.
    let refused = BotError::DomainError {
        domain: String::from("gh::merge"),
        certainty: DispatchCertainty::NotDelivered,
        cause: String::from("connection closed"),
    };
    let unknown = BotError::EffectIndeterminate {
        domain: String::from("gh::merge"),
        cause: String::from("connection closed"),
    };

    assert!(
        !may_have_taken_effect(&refused),
        "a domain error is a retry, not a possible duplicate: {refused}"
    );
    assert!(
        may_have_taken_effect(&unknown),
        "an indeterminate effect must not be blind-retried: {unknown}"
    );
    assert_ne!(
        refused.to_string(),
        unknown.to_string(),
        "an operator triages from these lines, so they cannot render alike"
    );
}

#[test]
fn the_retry_class_is_total_and_never_reads_a_cause() {
    // The three arms are ordered by what they permit, and every error maps
    // into exactly one of them. A wiring defect and a refused binding are
    // `Never`, a connection that never opened is `Safe`, and only an
    // unsettled effect is `RequiresEvidence` — which is the same answer the
    // variant-level classifier above gives, reached without inspecting a
    // variant.
    let cases = [
        (
            BotError::DomainError {
                domain: String::from("test::wiring"),
                certainty: DispatchCertainty::Refused,
                cause: String::from("type mismatch in execute input"),
            },
            RetryClass::Never,
        ),
        (
            BotError::DomainError {
                domain: String::from("test::read"),
                certainty: DispatchCertainty::NotDelivered,
                cause: String::from("connection reset before any byte was sent"),
            },
            RetryClass::Safe,
        ),
        (
            BotError::EffectIndeterminate {
                domain: String::from("test::merge"),
                cause: String::from("the acknowledgment never arrived"),
            },
            RetryClass::RequiresEvidence,
        ),
        (BotError::TickInsideRuntime, RetryClass::Never),
    ];

    for (error, expected) in cases {
        assert_eq!(
            error.retry_class(),
            expected,
            "the class is what the retry decision reads: {error}"
        );
        assert_eq!(
            error.dispatch_certainty().retry_class(),
            expected,
            "the two-step path must agree with the one-step path: {error}"
        );
    }
}

#[test]
fn a_refusal_and_a_wiring_defect_are_both_terminal() {
    // Two producers with two different reasons to be permanent: a binding
    // that is not installed, and a chain whose stages do not line up. Both
    // must stop the entry dead rather than spend the budget, because no
    // number of attempts changes either.
    let binding = BotError::DomainError {
        domain: String::from("gh::pr_status"),
        certainty: DispatchCertainty::Refused,
        cause: String::from("binding required"),
    };
    let wiring = BotError::DomainError {
        domain: String::from("pipeline::step"),
        certainty: DispatchCertainty::Refused,
        cause: String::from("type mismatch in pipeline step input"),
    };

    for error in [&binding, &wiring] {
        assert_eq!(error.retry_class(), RetryClass::Never);
        assert_eq!(error.dispatch_certainty(), DispatchCertainty::Refused);
    }
}

#[test]
fn an_indeterminate_effect_names_its_domain_and_the_indeterminacy() {
    let error = BotError::EffectIndeterminate {
        domain: String::from("notify::slack"),
        cause: String::from("request timed out"),
    };
    let rendered = error.to_string();

    assert!(
        rendered.contains("notify::slack"),
        "the line must name the domain that failed: {rendered}"
    );
    assert!(
        rendered.contains("request timed out"),
        "the line must carry the cause: {rendered}"
    );
    assert!(
        rendered.contains("may have taken effect"),
        "the failure alone is not enough — the line has to say the effect may be \
             live, because that is what makes a retry unsafe: {rendered}"
    );
}

/// One payload carrying every class the rendering contract has to handle:
/// LF, CR, tab, ESC, a quote, a backslash, and ordinary Unicode.
const HOSTILE: &str = "a\nb\rc\td\u{1b}[2J e\"f\\g ünïcode 日本";

/// Every variant that interpolates a `String` payload.
///
/// One row per arm, so an arm added later without `Escaped` fails here
/// rather than in whatever log sink first meets a hostile domain name.
fn hostile_arms() -> Vec<BotError> {
    let payload = String::from(HOSTILE);
    vec![
        // One row covers both payloads a deficit can carry: the capability
        // name and the domain that demanded it are both caller-supplied
        // strings, and both reach the rendering site.
        BotError::CapabilityDenied {
            deficit: Deficit::new(
                Shortage::new(
                    Cap::new(payload.clone()),
                    Some(Demand::new(payload.clone())),
                ),
                Vec::new(),
            ),
        },
        BotError::MalformedSpec {
            cause: payload.clone(),
        },
        // Both spec-refusal variants grew an untrusted payload since this
        // list was written, so each gets its own row here: `IncompleteSpec`
        // names the missing field and records why it was missing, and
        // `UnknownCondition` names the identifier and records how its
        // argument refused to parse. Neither may render a live control
        // character.
        BotError::IncompleteSpec {
            field: "target",
            cause: payload.clone(),
        },
        BotError::UnknownCondition {
            condition: payload.clone(),
            argument_parse: payload.clone(),
        },
        BotError::DomainError {
            domain: payload.clone(),
            certainty: DispatchCertainty::Refused,
            cause: payload.clone(),
        },
        BotError::EffectIndeterminate {
            domain: payload.clone(),
            cause: payload.clone(),
        },
        BotError::EvaluateError {
            cause: payload.clone(),
        },
        BotError::MalformedFlow {
            cause: payload.clone(),
        },
        BotError::InvalidTransitionTarget {
            from: payload.clone(),
            target: payload.clone(),
        },
        BotError::MissingAskRoute {
            node: payload.clone(),
            option: payload.clone(),
        },
        BotError::VariableNeverWritten {
            name: payload.clone(),
        },
        BotError::VariableReadBeforeInit {
            node: payload.clone(),
            name: payload.clone(),
        },
        BotError::UndeclaredVariable {
            name: payload.clone(),
        },
        BotError::UnreachableNode {
            node: payload.clone(),
        },
        BotError::UnknownNodeKind {
            node: payload.clone(),
            kind: payload.clone(),
        },
        BotError::MissingTransition {
            node: payload.clone(),
        },
        BotError::MalformedTemplate {
            node: payload.clone(),
            field: "label",
        },
        BotError::VariableTypeMismatch {
            name: payload.clone(),
        },
        // One row covers both terminal payloads: `HandedOff` carries the
        // node's target and `Refused` the declared reason, and both reach
        // the rendering site.
        BotError::ConflictingTerminalDeclaration {
            node: payload.clone(),
            intrinsic: Terminal::HandedOff {
                target: payload.clone(),
            },
            declared: Terminal::Refused {
                reason: payload.clone(),
            },
        },
        BotError::AskOptionNotAssignable {
            node: payload.clone(),
            variable: payload.clone(),
            option: payload.clone(),
            expected: "boolean",
            cause: payload.clone(),
        },
        BotError::VariableUnset {
            name: payload.clone(),
        },
        BotError::InvalidVariableValue {
            variable: payload.clone(),
            value: payload.clone(),
            reason: payload.clone(),
        },
        BotError::ResolverReturnedInvalidOption { node: payload },
    ]
}

#[test]
fn a_rendered_payload_cannot_forge_a_log_record() {
    // The contract the module header states, and the one `Display` did not
    // implement: it interpolated the payload directly, so a domain name
    // containing a newline produced a second physical log line and an ESC
    // reached whatever terminal later read the file.
    let error = BotError::EffectIndeterminate {
        domain: String::from(HOSTILE),
        cause: String::from(HOSTILE),
    };
    let rendered = error.to_string();

    assert!(
        !rendered.contains('\n') && !rendered.contains('\r'),
        "a payload must not add a physical log record: {rendered:?}"
    );
    assert!(
        !rendered.chars().any(char::is_control),
        "a payload must not carry a live terminal control: {rendered:?}"
    );
    assert!(
        rendered.contains("\\n") && rendered.contains("\\u{1b}"),
        "the escapes must be visible rather than stripped, because the payload \
             is the diagnostic: {rendered}"
    );
}

#[test]
fn every_untrusted_field_is_escaped() {
    for error in hostile_arms() {
        let rendered = error.to_string();
        assert!(
            !rendered.chars().any(char::is_control),
            "a variant rendered a live control character: {rendered:?}"
        );
        assert!(
            !rendered.contains('\n'),
            "a variant rendered a second physical line: {rendered:?}"
        );
    }
}

#[test]
fn ordinary_diagnostics_keep_their_meaning() {
    // The other half of the contract, and the reason backslash is left
    // alone: escaping must not cost the diagnostic its content. Ordinary
    // Unicode and punctuation pass through byte for byte.
    let error = BotError::DomainError {
        domain: String::from("gh::merge"),
        certainty: DispatchCertainty::NotDelivered,
        cause: String::from("PR #7 — \"timeout\" after 30s, ünïcode 日本 ok"),
    };
    let rendered = error.to_string();

    assert!(
        rendered.contains("PR #7 — \"timeout\" after 30s, ünïcode 日本 ok"),
        "an ordinary message must survive unaltered: {rendered}"
    );
    assert!(
        rendered.starts_with("gh::merge: "),
        "the typed boundary stays readable: {rendered}"
    );
}

#[test]
fn escaping_is_idempotent_with_constructor_level_escaping() {
    // `MalformedSpec` and `MalformedFlow` escape their payload where they
    // build it, and this pass runs again at render time over the same text.
    // If this escaped backslash the diagnostic would grow one on every hop.
    let already_escaped = Escaped("line one\\nline two").to_string();
    assert_eq!(
        already_escaped, "line one\\nline two",
        "text that is already escaped must pass through untouched"
    );

    let raw = Escaped("line one\nline two").to_string();
    assert_eq!(
        raw, "line one\\nline two",
        "and the escape it produces must be the same shape, so the two agree"
    );
}
