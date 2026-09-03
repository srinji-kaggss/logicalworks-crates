//! `error` owns the bot error vocabulary and enforces INV-BOT-ERROR-TYPED:
//! every failure carries the operation, boundary, cause, and whether it is
//! retryable.

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
    /// The bot spec is incomplete — missing name or zero chains.
    IncompleteSpec {
        /// What is missing.
        field: &'static str,
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
            Self::DomainError { domain, cause } => {
                write!(f, "{domain}: {cause}")
            }
            Self::EvaluateError { cause } => {
                write!(f, "evaluate: {cause}")
            }
        }
    }
}

impl std::error::Error for BotError {}
