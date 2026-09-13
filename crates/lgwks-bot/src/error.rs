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
#[derive(Debug)]
pub enum BotError {
    /// A required capability was not granted.
    CapabilityDenied {
        /// The capability that was required but missing.
        required: Cap,
    },
    /// The bot spec is incomplete — a required field is missing (currently the
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
    /// An evaluate condition failed structurally (not a false result — an error
    /// in the condition itself).
    EvaluateError {
        /// What went wrong.
        cause: String,
    },
}

impl fmt::Display for BotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CapabilityDenied { required } => {
                write!(f, "capability denied: {required}")
            }
            Self::IncompleteSpec { field } => {
                write!(f, "incomplete bot spec: missing {field}")
            }
            Self::SpecTooLarge { bytes, limit } => {
                write!(f, "bot spec is {bytes} bytes, over the {limit}-byte limit")
            }
            Self::MalformedSpec { cause } => write!(f, "malformed bot spec: {cause}"),
            Self::DomainError { domain, cause } => {
                write!(f, "{domain}: {cause}")
            }
            Self::EvaluateError { cause } => {
                write!(f, "evaluate: {cause}")
            }
        }
    }
}

impl std::error::Error for BotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        // All causes are data (`Cap`, `&'static str`, escaped `String`), never
        // a wrapped error: there is no deeper source to forward. String
        // causes are intentional here — see the module header — not a missing
        // `#[from]` impl.
        None
    }
}
