//! `error` owns the bot error vocabulary and enforces INV-BOT-ERROR-TYPED:
//! every failure is a distinct typed variant carrying the capability, field,
//! domain, or condition that caused it. Variants that surface untrusted
//! runtime text (`MalformedSpec`, `DomainError`, `EvaluateError`) carry it as
//! a `String` cause by design: the boundary is typed (which domain failed is
//! always known), while the foreign payload is escaped at the `Display`
//! boundary so it cannot forge a log line. There is no bare-string failure
//! with no typed envelope.

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
