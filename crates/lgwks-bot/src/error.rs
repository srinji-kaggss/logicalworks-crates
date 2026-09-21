//! `error` owns the bot error vocabulary and enforces INV-BOT-ERROR-TYPED:
//! every failure is a distinct typed variant carrying the capability, field,
//! domain, or condition that caused it. Variants that surface untrusted
//! runtime text (`MalformedSpec`, `DomainError`, `EffectIndeterminate`,
//! `EvaluateError`) carry it as a `String` cause by design: the boundary is
//! typed (which domain failed is always known), while the foreign payload is
//! escaped at the `Display` boundary so it cannot forge a log line. There is
//! no bare-string failure with no typed envelope.
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

impl fmt::Display for BotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The scrutinee is `*self` so each pattern's type is the enum's own
        // type rather than a reference to it, and non-`Copy` payloads are bound
        // by `ref`. `field` is `&'static str` and the two lengths are `usize`,
        // so those bind by copy from behind the deref.
        match *self {
            Self::CapabilityDenied { ref required } => {
                write!(f, "capability denied: {required}")
            }
            Self::IncompleteSpec { field } => {
                write!(f, "incomplete bot spec: missing {field}")
            }
            Self::SpecTooLarge { bytes, limit } => {
                write!(f, "bot spec is {bytes} bytes, over the {limit}-byte limit")
            }
            Self::MalformedSpec { ref cause } => write!(f, "malformed bot spec: {cause}"),
            Self::DomainError {
                ref domain,
                ref cause,
            } => {
                write!(f, "{domain}: {cause}")
            }
            Self::EffectIndeterminate {
                ref domain,
                ref cause,
            } => {
                // Names the indeterminacy, not just the domain: an operator
                // reading this line has to know the effect may be live, because
                // that is what makes the retry unsafe.
                write!(f, "{domain}: may have taken effect — {cause}")
            }
            Self::EvaluateError { ref cause } => {
                write!(f, "evaluate: {cause}")
            }
            Self::FlowTooLarge { bytes, limit } => {
                write!(f, "flow is {bytes} bytes, over the {limit}-byte limit")
            }
            Self::MalformedFlow { ref cause } => write!(f, "malformed flow: {cause}"),
            Self::InvalidTransitionTarget {
                ref from,
                ref target,
            } => write!(
                f,
                "flow transition from {from} targets missing node {target}"
            ),
            Self::MissingAskRoute {
                ref node,
                ref option,
            } => write!(f, "ask node {node} has no route for option {option}"),
            Self::VariableNeverWritten { ref name } => {
                write!(f, "declared variable {name} is never written")
            }
            Self::UndeclaredVariable { ref name } => {
                write!(f, "flow reads undeclared variable {name}")
            }
            Self::UnreachableNode { ref node } => write!(f, "flow node {node} is unreachable"),
            Self::FlowBudgetExceeded { steps, budget } => {
                write!(
                    f,
                    "flow declares {steps} nodes over its {budget}-step budget"
                )
            }
            Self::UnknownNodeKind { ref node, ref kind } => {
                write!(f, "flow node {node} has unknown kind {kind}")
            }
            Self::MissingTransition { ref node } => {
                write!(f, "flow node {node} has no continuation")
            }
            Self::MalformedTemplate { ref node, field } => {
                write!(f, "flow node {node} has malformed {field} template")
            }
            Self::VariableTypeMismatch { ref name } => {
                write!(f, "value for variable {name} has the wrong type")
            }
            Self::PredicateTypeMismatch => f.write_str("predicate values have incompatible types"),
            Self::VariableUnset { ref name } => write!(f, "variable {name} has no value"),
            Self::InvalidVariableValue {
                ref variable,
                ref value,
            } => write!(
                f,
                "value {value:?} cannot be assigned to variable {variable}"
            ),
            Self::SessionTerminated => f.write_str("session has already terminated"),
            Self::SessionNotAwaitingAnswer => f.write_str("session is not awaiting an answer"),
            Self::ResolverReturnedInvalidOption { ref node } => {
                write!(f, "resolver returned an invalid option for node {node}")
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
    use super::BotError;

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
}
