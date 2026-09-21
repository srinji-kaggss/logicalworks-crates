//! `error` owns the bot error vocabulary and enforces INV-BOT-ERROR-TYPED:
//! every failure is a distinct typed variant carrying the capability, field,
//! domain, or condition that caused it. Variants that surface untrusted
//! runtime text carry it as a `String` by design: the boundary is typed (which
//! domain failed is always known), while the foreign payload stays a `String`
//! so a structured consumer still has the original bytes. It is escaped where
//! it is *rendered*, by the `Escaped` adapter below, so a payload cannot forge
//! a log line or carry a live terminal control into whatever reads the file.
//! There is no bare-string failure with no typed envelope, and no arm that
//! interpolates a payload without that adapter.
//!
//! The vocabulary draws one distinction that a single failure variant cannot:
//! **whether the effect happened**. [`BotError::DomainError`] means the action
//! did not take effect, so a retry is a retry. [`BotError::EffectIndeterminate`]
//! means it may have, so a retry is a possible duplicate. They are separate
//! variants rather than one variant carrying a flag because a consumer's retry
//! decision has to be readable from the type: a caller that must parse a cause
//! string to learn whether it may retry will eventually get it wrong, and the
//! price of getting it wrong is a duplicated merge, message, or process launch.
//!
//! The links above resolve at the crate root, where `BotError` is re-exported,
//! rather than in this module — see `frontier.rs` for the module whose items
//! are *not* all re-exported and which therefore needs reference definitions.

use std::fmt;

use super::cap::Cap;

/// Error from bot construction, admission, or execution.
///
/// `#[non_exhaustive]`: the vocabulary is expected to grow as domains gain
/// failure modes, and a consumer matching on it must keep a wildcard arm so a
/// new variant is a compile-time prompt there rather than a silent fallthrough.
#[derive(Debug)]
#[non_exhaustive]
pub enum BotError {
    /// A required capability was not granted.
    CapabilityDenied {
        /// The capability that was required but missing.
        required: Cap,
    },
    /// The bot spec is incomplete: a required field is missing (currently the
    /// name; an empty chain list is allowed).
    IncompleteSpec {
        /// What is missing.
        field: &'static str,
    },
    /// The serialized spec exceeds [`crate::spec::MAX_SPEC_BYTES`]. The bound is
    /// defensive: it stops a hostile or runaway manifest from allocating
    /// without limit before any validation runs.
    SpecTooLarge {
        /// Observed length in bytes.
        bytes: usize,
        /// The applied bound.
        limit: usize,
    },
    /// The serialized spec is not valid JSON for the schema.
    MalformedSpec {
        /// The parser's positional diagnostic, with control characters escaped
        /// so an untrusted field name cannot forge a log line.
        cause: String,
    },
    /// A domain action failed at runtime.
    DomainError {
        /// The domain that failed (e.g. `"gh::pr_status"`).
        domain: String,
        /// The underlying cause.
        cause: String,
    },
    /// A domain action failed *after* it may already have taken effect, so
    /// whether the effect occurred cannot be determined from the failure.
    ///
    /// The sibling of [`BotError::DomainError`], and the difference is the
    /// whole point: `DomainError` means the effect did not happen, so a retry
    /// is a retry; this means it may have happened, so a retry is a possible
    /// duplicate. A consumer that retries every error it is handed will
    /// duplicate a merge, a message, or a process launch here, and it can only
    /// avoid that if the two are distinguishable without reading the cause.
    ///
    /// Reached when a request timed out, or a connection dropped, *after* the
    /// request was sent. [`crate::verb::Execute`] is the only verb with an
    /// effect, so the variant names no verb: it is an effect by construction.
    EffectIndeterminate {
        /// The domain whose action may or may not have taken effect.
        domain: String,
        /// Why the outcome could not be settled.
        cause: String,
    },
    /// An evaluate condition failed structurally: not a false result, but an
    /// error in the condition itself.
    EvaluateError {
        /// What went wrong.
        cause: String,
    },
    /// The serialized flow exceeds [`crate::session::MAX_FLOW_BYTES`].
    FlowTooLarge {
        /// Observed length in bytes.
        bytes: usize,
        /// The applied bound.
        limit: usize,
    },
    /// The serialized flow is not valid JSON for the flow schema.
    MalformedFlow {
        /// The parser's escaped diagnostic.
        cause: String,
    },
    /// A flow transition points at a node that is not in the node map.
    InvalidTransitionTarget {
        /// The node or field that owns the transition.
        from: String,
        /// The unresolved target node id.
        target: String,
    },
    /// An ask option is missing its route.
    MissingAskRoute {
        /// The ask node id.
        node: String,
        /// The option value without a route.
        option: String,
    },
    /// A declared variable has no ask node that writes it.
    VariableNeverWritten {
        /// The declared variable name.
        name: String,
    },
    /// A flow reads a variable that was not declared.
    UndeclaredVariable {
        /// The variable name that was read.
        name: String,
    },
    /// A node in the flow document cannot be reached from the entry node.
    UnreachableNode {
        /// The unreachable node id.
        node: String,
    },
    /// The number of flow nodes exceeds the declared execution budget.
    FlowBudgetExceeded {
        /// The number of declared nodes.
        steps: usize,
        /// The declared maximum.
        budget: usize,
    },
    /// A node kind in a JSON flow is outside the closed node language.
    UnknownNodeKind {
        /// The node carrying the unknown kind.
        node: String,
        /// The unknown kind label.
        kind: String,
    },
    /// A non-terminal node has no continuation.
    MissingTransition {
        /// The node without a continuation.
        node: String,
    },
    /// A flow template is syntactically invalid.
    MalformedTemplate {
        /// The node containing the template.
        node: String,
        /// The template field being checked.
        field: &'static str,
    },
    /// A declared variable is used with a value of the wrong type.
    VariableTypeMismatch {
        /// The variable name.
        name: String,
    },
    /// A predicate could not compare values of different types.
    PredicateTypeMismatch,
    /// A predicate or interpolation needs a variable that has no value yet.
    VariableUnset {
        /// The variable name.
        name: String,
    },
    /// An answer could not be converted to the declared variable type.
    InvalidVariableValue {
        /// The variable receiving the answer.
        variable: String,
        /// The answer that failed conversion.
        value: String,
    },
    /// A session has already reached a terminal outcome.
    SessionTerminated,
    /// The current session node is not an ask node.
    SessionNotAwaitingAnswer,
    /// A resolver returned an option index outside the candidate list.
    ResolverReturnedInvalidOption {
        /// The current ask node id.
        node: String,
    },
    /// A session attempted to execute more nodes than its flow budget allows.
    SessionBudgetExceeded {
        /// The number of attempted steps.
        steps: usize,
        /// The flow's declared maximum.
        budget: usize,
    },
}

/// Render untrusted text so it cannot forge a log record.
///
/// Control characters are the whole of the problem: a newline ends the record,
/// and an ESC sequence executes in whatever terminal later reads it. Both are
/// rendered as escapes, so the payload is still complete and still readable,
/// but it cannot add a physical line or reach the terminal.
///
/// Backslash is deliberately *not* escaped, for two reasons. It cannot forge
/// anything on its own, and leaving it alone makes this pass idempotent with
/// the constructor-level escaping that `MalformedSpec` and `MalformedFlow`
/// already perform: that escaping turns a real newline into the two characters
/// `\` and `n`, which this pass then leaves exactly as it found them. Escaping
/// backslash would render them `\\n`, and each additional pass would add
/// another. The cost is a presentational ambiguity — a payload holding a
/// literal backslash-then-n reads the same as an escaped newline — and that is
/// a triage nuisance rather than a forgery, which makes it the cheaper of the
/// two prices.
///
/// Applied at the rendering site rather than in the enum, so the structured
/// payload a consumer matches on keeps the original bytes.
struct Escaped<'a>(&'a str);

impl fmt::Display for Escaped<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for glyph in self.0.chars() {
            match glyph {
                '\n' => f.write_str("\\n")?,
                '\r' => f.write_str("\\r")?,
                '\t' => f.write_str("\\t")?,
                control if control.is_control() => {
                    write!(f, "\\u{{{:x}}}", u32::from(control))?;
                }
                plain => {
                    let mut buffer = [0_u8; 4];
                    f.write_str(plain.encode_utf8(&mut buffer))?;
                }
            }
        }
        Ok(())
    }
}

impl fmt::Display for BotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The scrutinee is `*self` so each pattern's type is the enum's own
        // type rather than a reference to it, and non-`Copy` payloads are bound
        // by `ref`. `field` is `&'static str` and the two lengths are `usize`,
        // so those bind by copy from behind the deref.
        match *self {
            // `Cap` accepts any dotted name, so its text is untrusted here too
            // even though the shipped four are constants.
            Self::CapabilityDenied { ref required } => {
                write!(f, "capability denied: {}", Escaped(required.as_str()))
            }
            Self::IncompleteSpec { field } => {
                write!(f, "incomplete bot spec: missing {field}")
            }
            Self::SpecTooLarge { bytes, limit } => {
                write!(f, "bot spec is {bytes} bytes, over the {limit}-byte limit")
            }
            Self::MalformedSpec { ref cause } => {
                write!(f, "malformed bot spec: {}", Escaped(cause))
            }
            Self::DomainError {
                ref domain,
                ref cause,
            } => {
                write!(f, "{}: {}", Escaped(domain), Escaped(cause))
            }
            Self::EffectIndeterminate {
                ref domain,
                ref cause,
            } => {
                // Names the indeterminacy, not just the domain: an operator
                // reading this line has to know the effect may be live, because
                // that is what makes the retry unsafe.
                write!(
                    f,
                    "{}: may have taken effect — {}",
                    Escaped(domain),
                    Escaped(cause)
                )
            }
            Self::EvaluateError { ref cause } => {
                write!(f, "evaluate: {}", Escaped(cause))
            }
            Self::FlowTooLarge { bytes, limit } => {
                write!(f, "flow is {bytes} bytes, over the {limit}-byte limit")
            }
            Self::MalformedFlow { ref cause } => {
                write!(f, "malformed flow: {}", Escaped(cause))
            }
            Self::InvalidTransitionTarget {
                ref from,
                ref target,
            } => write!(
                f,
                "flow transition from {} targets missing node {}",
                Escaped(from),
                Escaped(target)
            ),
            Self::MissingAskRoute {
                ref node,
                ref option,
            } => write!(
                f,
                "ask node {} has no route for option {}",
                Escaped(node),
                Escaped(option)
            ),
            Self::VariableNeverWritten { ref name } => {
                write!(f, "declared variable {} is never written", Escaped(name))
            }
            Self::UndeclaredVariable { ref name } => {
                write!(f, "flow reads undeclared variable {}", Escaped(name))
            }
            Self::UnreachableNode { ref node } => {
                write!(f, "flow node {} is unreachable", Escaped(node))
            }
            Self::FlowBudgetExceeded { steps, budget } => {
                write!(
                    f,
                    "flow declares {steps} nodes over its {budget}-step budget"
                )
            }
            Self::UnknownNodeKind { ref node, ref kind } => {
                write!(
                    f,
                    "flow node {} has unknown kind {}",
                    Escaped(node),
                    Escaped(kind)
                )
            }
            Self::MissingTransition { ref node } => {
                write!(f, "flow node {} has no continuation", Escaped(node))
            }
            // `field` is `&'static str` from this crate, so it is not a payload.
            Self::MalformedTemplate { ref node, field } => {
                write!(
                    f,
                    "flow node {} has malformed {field} template",
                    Escaped(node)
                )
            }
            Self::VariableTypeMismatch { ref name } => {
                write!(f, "value for variable {} has the wrong type", Escaped(name))
            }
            Self::PredicateTypeMismatch => f.write_str("predicate values have incompatible types"),
            Self::VariableUnset { ref name } => {
                write!(f, "variable {} has no value", Escaped(name))
            }
            // `value` renders through `Debug`, which escapes control characters
            // itself; the variable name does not, so only that one is wrapped.
            Self::InvalidVariableValue {
                ref variable,
                ref value,
            } => write!(
                f,
                "value {value:?} cannot be assigned to variable {}",
                Escaped(variable)
            ),
            Self::SessionTerminated => f.write_str("session has already terminated"),
            Self::SessionNotAwaitingAnswer => f.write_str("session is not awaiting an answer"),
            Self::ResolverReturnedInvalidOption { ref node } => {
                write!(
                    f,
                    "resolver returned an invalid option for node {}",
                    Escaped(node)
                )
            }
            Self::SessionBudgetExceeded { steps, budget } => {
                write!(
                    f,
                    "session attempted {steps} steps over its {budget}-step budget"
                )
            }
        }
    }
}

impl std::error::Error for BotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        // All causes are data (`Cap`, `&'static str`, escaped `String`), never
        // a wrapped error: there is no deeper source to forward. String
        // causes are intentional here (see the module header), not a missing
        // `#[from]` impl.
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{BotError, Cap, Escaped};

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
            BotError::CapabilityDenied {
                required: Cap::new(payload.clone()),
            },
            BotError::MalformedSpec {
                cause: payload.clone(),
            },
            BotError::DomainError {
                domain: payload.clone(),
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
            BotError::VariableUnset {
                name: payload.clone(),
            },
            BotError::InvalidVariableValue {
                variable: payload.clone(),
                value: payload.clone(),
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
}
