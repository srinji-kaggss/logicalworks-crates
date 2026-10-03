//! How a flow fails: located, classed and never silent.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use crate::error::{BotError, RetryClass};

use super::Scope;

// ── FlowError ───────────────────────────────────────────────────────────────

/// Why a flow, or one step of it, did not produce its value.
///
/// Every variant carries `at`, the step path where it happened, so a failure in
/// the fortieth item of a nested fan-out reads `sync/each:page#39/retry` rather
/// than a bare message. An error raised with `?` before a location is known is
/// located at the flow boundary by [`FlowError::located`].
///
/// Retry is decided by the variant, never by the message:
/// [`FlowError::is_retryable`] is the only rule [`retry`](crate::script::retry) reads.
#[derive(Debug)]
#[non_exhaustive]
pub enum FlowError {
    /// The scope was stopped before or during the step.
    Cancelled {
        /// Where the stop was observed.
        at: Arc<str>,
    },
    /// A `within` deadline passed.
    TimedOut {
        /// The `within` block that expired.
        at: Arc<str>,
        /// The deadline it declared.
        after: Duration,
    },
    /// A `retry` spent every attempt on retryable failures.
    Exhausted {
        /// The `retry` block.
        at: Arc<str>,
        /// Attempts made.
        attempts: u32,
        /// The last attempt's failure.
        last: Box<FlowError>,
    },
    /// The run's retry budget was spent: failures are systemic, and another
    /// retry would add load rather than recover. Not retryable.
    Throttled {
        /// The `retry` block that was refused a retry.
        at: Arc<str>,
        /// Attempts made before the refusal.
        attempts: u32,
        /// The last attempt's failure.
        last: Box<FlowError>,
    },
    /// A permanent failure: repeating the step gives the same answer.
    Failed {
        /// Where it failed.
        at: Arc<str>,
        /// What the step said.
        reason: String,
    },
    /// A transient failure: the same step may succeed if repeated.
    Transient {
        /// Where it failed.
        at: Arc<str>,
        /// What the step said.
        reason: String,
    },
    /// A bot verb failed. Retryable exactly when its
    /// [`RetryClass`](crate::error::RetryClass) is [`RetryClass::Safe`](crate::error::RetryClass::Safe).
    Bot {
        /// Where it failed.
        at: Arc<str>,
        /// The verb's typed error, boxed so every flow `Result` stays small.
        source: Box<BotError>,
    },
    /// Steps nested past [`MAX_DEPTH`](crate::script::MAX_DEPTH).
    TooDeep {
        /// The scope that refused to go deeper.
        at: Arc<str>,
        /// The limit.
        limit: u16,
    },
    /// A tenant name did not validate.
    InvalidTenant {
        /// What is wrong with it.
        reason: &'static str,
    },
    /// A task's logical name did not validate.
    InvalidName {
        /// What is wrong with it.
        reason: &'static str,
    },
    /// A step reached for authority this run does not have.
    ///
    /// The one flow failure that is not a defect in the step and not a transient
    /// condition: the host is willing, the work is well-formed, and the authority
    /// is missing. It is never retryable on its own, because repeating the same
    /// step against the same authority asks the same question and gets the same
    /// answer — a permanent refusal plus repeated `NotApplied` must reach a finite
    /// refusal rather than an unbounded retry loop.
    ///
    /// The unmet requirements ride with it, as a complete
    /// [`Deficit`](crate::cap::Deficit) rather than the first one, so the run's
    /// report can name every presently knowable need at once and the repair
    /// ticket is written once against the whole shortfall.
    Blocked {
        /// The step that reached for the authority.
        at: Arc<str>,
        /// Every capability this step needs and the run does not have.
        deficit: Box<crate::cap::Deficit>,
    },
    /// A bound computed at run time is outside what the block accepts.
    InvalidBound {
        /// Which bound.
        what: &'static str,
        /// The value given.
        value: u64,
        /// The largest value accepted.
        max: u64,
    },
}

impl FlowError {
    /// A permanent failure with `reason`. Not retried.
    pub fn failed(reason: impl fmt::Display) -> Self {
        Self::Failed {
            at: Arc::from(""),
            reason: reason.to_string(),
        }
    }

    /// A transient failure with `reason`. Retried by an enclosing `retry`.
    pub fn transient(reason: impl fmt::Display) -> Self {
        Self::Transient {
            at: Arc::from(""),
            reason: reason.to_string(),
        }
    }

    /// Whether repeating the step that produced this could succeed.
    ///
    /// `TimedOut` and `Transient` are; a bot error is when its retry class is
    /// [`RetryClass::Safe`](crate::error::RetryClass::Safe) (the effect definitely did not happen). A
    /// cancellation, a permanent failure, an exhausted retry, and every
    /// validation failure are not: repeating them is a retry storm.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match *self {
            Self::TimedOut { .. } | Self::Transient { .. } => true,
            Self::Bot { ref source, .. } => source.retry_class() == RetryClass::Safe,
            Self::Cancelled { .. }
            | Self::Exhausted { .. }
            | Self::Throttled { .. }
            | Self::Failed { .. }
            | Self::Blocked { .. }
            | Self::TooDeep { .. }
            | Self::InvalidTenant { .. }
            | Self::InvalidName { .. }
            | Self::InvalidBound { .. } => false,
        }
    }

    /// Whether this is a stop rather than a failure.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        matches!(*self, Self::Cancelled { .. })
    }

    /// Where it happened; empty for a failure not yet located.
    #[must_use]
    pub fn at(&self) -> &str {
        match *self {
            Self::Cancelled { ref at }
            | Self::TimedOut { ref at, .. }
            | Self::Exhausted { ref at, .. }
            | Self::Throttled { ref at, .. }
            | Self::Failed { ref at, .. }
            | Self::Transient { ref at, .. }
            | Self::Bot { ref at, .. }
            | Self::Blocked { ref at, .. }
            | Self::TooDeep { ref at, .. } => at,
            Self::InvalidTenant { .. } | Self::InvalidName { .. } | Self::InvalidBound { .. } => "",
        }
    }

    /// Give an unlocated error the location `scope` names.
    ///
    /// An error that already has a location keeps it: the innermost location
    /// is the one that says where the failure happened.
    #[must_use]
    pub fn located(self, scope: &Scope) -> Self {
        self.located_at(scope.shared_path())
    }

    /// Whether this error is a refusal for missing authority, and the whole
    /// shortfall if it is.
    ///
    /// The one question a caller asking "was this run blocked rather than broken?"
    /// needs answered, and it answers with the complete shortfall rather than the
    /// first requirement, so the report that builds a repair ticket from it can be
    /// written once against the whole set.
    #[must_use]
    pub fn deficit(&self) -> Option<&crate::cap::Deficit> {
        match *self {
            Self::Blocked { ref deficit, .. } => Some(deficit),
            _ => None,
        }
    }

    /// [`FlowError::located`] against a path already in hand.
    #[must_use]
    pub(super) fn located_at(mut self, path: &Arc<str>) -> Self {
        match self {
            Self::Cancelled { ref mut at }
            | Self::TimedOut { ref mut at, .. }
            | Self::Exhausted { ref mut at, .. }
            | Self::Throttled { ref mut at, .. }
            | Self::Failed { ref mut at, .. }
            | Self::Transient { ref mut at, .. }
            | Self::Bot { ref mut at, .. }
            | Self::Blocked { ref mut at, .. }
            | Self::TooDeep { ref mut at, .. } => {
                if at.is_empty() {
                    *at = Arc::clone(path);
                }
            }
            Self::InvalidTenant { .. } | Self::InvalidName { .. } | Self::InvalidBound { .. } => {}
        }
        self
    }
}

impl fmt::Display for FlowError {
    /// `<path>: <what happened>`. Step-supplied text is rendered with `{:?}`,
    /// which escapes control characters, so a reason cannot forge a log line.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Cancelled { ref at } => write!(formatter, "{at}: cancelled"),
            Self::TimedOut { ref at, after } => {
                write!(formatter, "{at}: timed out after {after:?}")
            }
            Self::Exhausted {
                ref at,
                attempts,
                ref last,
            } => write!(
                formatter,
                "{at}: gave up after {attempts} attempts; last: {last}"
            ),
            Self::Throttled {
                ref at,
                attempts,
                ref last,
            } => write!(
                formatter,
                "{at}: retry budget spent after {attempts} attempts; last: {last}"
            ),
            Self::Failed { ref at, ref reason } => write!(formatter, "{at}: failed: {reason:?}"),
            Self::Transient { ref at, ref reason } => {
                write!(formatter, "{at}: failed (transient): {reason:?}")
            }
            Self::Bot { ref at, ref source } => write!(formatter, "{at}: {source}"),
            Self::Blocked {
                ref at,
                ref deficit,
            } => {
                write!(formatter, "{at}: blocked: {deficit}")
            }
            Self::TooDeep { ref at, limit } => {
                write!(formatter, "{at}: steps nested deeper than {limit}")
            }
            Self::InvalidTenant { reason } => write!(formatter, "invalid tenant: {reason}"),
            Self::InvalidName { reason } => write!(formatter, "invalid task name: {reason}"),
            Self::InvalidBound { what, value, max } => {
                write!(formatter, "{what}: {value} is outside 1..={max}")
            }
        }
    }
}

impl std::error::Error for FlowError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Bot { ref source, .. } => Some(&**source),
            Self::Exhausted { ref last, .. } | Self::Throttled { ref last, .. } => Some(&**last),
            Self::Cancelled { .. }
            | Self::TimedOut { .. }
            | Self::Failed { .. }
            | Self::Transient { .. }
            | Self::Blocked { .. }
            | Self::TooDeep { .. }
            | Self::InvalidTenant { .. }
            | Self::InvalidName { .. }
            | Self::InvalidBound { .. } => None,
        }
    }
}

impl From<BotError> for FlowError {
    /// Lets `?` carry a verb's error; the location is filled at the boundary.
    fn from(source: BotError) -> Self {
        Self::Bot {
            at: Arc::from(""),
            source: Box::new(source),
        }
    }
}

impl From<std::io::Error> for FlowError {
    /// The kinds that describe a moment rather than a fact (a timeout, an
    /// interruption, a dropped connection) are transient; the rest are not.
    fn from(error: std::io::Error) -> Self {
        use std::io::ErrorKind;
        match error.kind() {
            ErrorKind::TimedOut
            | ErrorKind::Interrupted
            | ErrorKind::WouldBlock
            | ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::ConnectionRefused
            | ErrorKind::NotConnected
            | ErrorKind::BrokenPipe
            | ErrorKind::UnexpectedEof => Self::transient(error),
            _ => Self::failed(error),
        }
    }
}

/// Turn a foreign `Result` into a flow result, saying whether its failure is
/// worth repeating.
///
/// In scope inside every flow `script!` writes, so a step reads
/// `parse(text).or_fail()?` or `fetch(url).await.or_retry()?`.
pub trait ResultExt<T> {
    /// A permanent failure on `Err`.
    ///
    /// # Errors
    ///
    /// [`FlowError::Failed`] carrying the error's text.
    fn or_fail(self) -> Result<T, FlowError>;

    /// A transient failure on `Err`: an enclosing `retry` repeats the step.
    ///
    /// # Errors
    ///
    /// [`FlowError::Transient`] carrying the error's text.
    fn or_retry(self) -> Result<T, FlowError>;
}

impl<T, E: fmt::Display> ResultExt<T> for Result<T, E> {
    fn or_fail(self) -> Result<T, FlowError> {
        self.map_err(FlowError::failed)
    }

    fn or_retry(self) -> Result<T, FlowError> {
        self.map_err(FlowError::transient)
    }
}

/// Turn an absent value into a permanent failure that says what was missing.
pub trait OptionExt<T> {
    /// The value, or [`FlowError::Failed`] with `reason`.
    ///
    /// # Errors
    ///
    /// [`FlowError::Failed`] when `None`.
    fn or_fail(self, reason: &str) -> Result<T, FlowError>;
}

impl<T> OptionExt<T> for Option<T> {
    fn or_fail(self, reason: &str) -> Result<T, FlowError> {
        self.ok_or_else(|| FlowError::failed(reason))
    }
}
