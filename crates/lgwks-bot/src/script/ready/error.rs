//! Why a readiness signal was refused. Split from `ready.rs` (#526) so the refusal vocabulary is one file.

use std::fmt;
use std::sync::Arc;

use crate::script::error::FlowError;

use super::Generation;

/// Why a readiness signal was refused.
///
/// Every arm is a *refusal that released nothing*, and each names which of the
/// facts a caller turns on: the service never became ready, a stale instance
/// tried to release dependants, a signal came from a generation this readiness
/// never issued, a second release was attempted, or the readiness's own bound
/// was reached. [`ReadinessError::released`](super::ReadinessError::released) is `false` for all of them, which
/// is what makes a refusal safe to retry rather than a silent no-op.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReadinessError {
    /// The service failed before it was ready. No dependant was released.
    NotReady {
        /// The readiness that failed.
        what: Arc<str>,
        /// The generation that failed.
        generation: Generation,
        /// What the service said.
        reason: Arc<str>,
    },
    /// The signal came from an instance older than the one dependants are
    /// waiting on, or older than the one already released.
    ///
    /// Released nothing. This is the fence a restarted service needs: an older
    /// instance that says "ready" after a newer one has taken over must not
    /// release dependants onto a service that is gone.
    StaleGeneration {
        /// The readiness the signal was sent to.
        what: Arc<str>,
        /// The generation the readiness is currently bound to.
        expected: Generation,
        /// The generation the signal carried.
        got: Generation,
    },
    /// The signal names a generation this readiness never issued.
    ///
    /// Distinct from [`Self::StaleGeneration`] because the two are different
    /// facts: an older generation is a *previous* instance of this service, and
    /// a generation this readiness never issued is a signal from somewhere else
    /// entirely — another service's readiness, or a caller that guessed. A
    /// caller deciding whether to re-arm its service wants the first and wants
    /// the second refused.
    UnknownGeneration {
        /// The readiness the signal was sent to.
        what: Arc<str>,
        /// The generation the signal carried.
        got: Generation,
    },
    /// This readiness has already released, so a second signal at the same
    /// generation is refused.
    ///
    /// A duplicate is not a second release. The generation released first is
    /// kept, so a dependant that arrives late still reads the first release
    /// rather than being sent to a restarted instance. A signal carrying a
    /// *different* generation is [`Self::StaleGeneration`] or
    /// [`Self::UnknownGeneration`] instead: "already ready" and "that signal was
    /// from somewhere else" are different facts with different repairs.
    AlreadyReady {
        /// The readiness that is already ready.
        what: Arc<str>,
        /// The generation that was released.
        generation: Generation,
    },
    /// This readiness is already failed, so a signal is refused.
    ///
    /// The failed generation and its reason are kept, so a service that recovers
    /// by restarting must be armed with a *new* [`Readiness`](super::Readiness) rather than
    /// re-signalling a dead one.
    AlreadyFailed {
        /// The readiness that already failed.
        what: Arc<str>,
        /// The generation that failed.
        generation: Generation,
    },
    /// This readiness was shut down, so a signal is refused.
    Closed {
        /// The readiness that is shut down.
        what: Arc<str>,
        /// The generation that shut down.
        generation: Generation,
    },
    /// No dependant may be registered: the declared cap is reached.
    ///
    /// Charged **before** a slot is taken, so a refused admission never
    /// consumes capacity and a caller that reduces its dependants sees the cap
    /// again rather than a permanently exhausted readiness.
    DependantsFull {
        /// The readiness that is full.
        what: Arc<str>,
        /// The cap that was reached.
        cap: usize,
    },
    /// The readiness's name is empty or longer than [`MAX_NAME_BYTES`](super::MAX_NAME_BYTES).
    InvalidName {
        /// What is wrong with the name.
        reason: &'static str,
    },
}

impl ReadinessError {
    /// The readiness this refusal names.
    #[must_use]
    pub fn what(&self) -> &str {
        match *self {
            Self::NotReady { ref what, .. }
            | Self::StaleGeneration { ref what, .. }
            | Self::UnknownGeneration { ref what, .. }
            | Self::AlreadyReady { ref what, .. }
            | Self::AlreadyFailed { ref what, .. }
            | Self::Closed { ref what, .. }
            | Self::DependantsFull { ref what, .. } => what,
            Self::InvalidName { .. } => "",
        }
    }

    /// Whether this refusal released a dependant.
    ///
    /// `false` for every arm. It is stated rather than assumed because "the
    /// signal was refused" and "nothing happened" are different facts, and only
    /// the second one is what makes a refusal safe to retry.
    #[must_use]
    pub const fn released(&self) -> bool {
        false
    }

    /// Whether repeating this exact signal would produce the same refusal.
    ///
    /// The retry contract of a readiness, stated once. A stale signal, a signal
    /// from a generation this readiness never issued, a duplicate release and a
    /// full cap are all permanent: the world does not change between two
    /// identical calls, so an enclosing [`retry`](crate::script::retry) that treated
    /// them as retryable would loop on a stale instance forever.
    #[must_use]
    pub const fn is_permanent(&self) -> bool {
        !matches!(self, Self::NotReady { .. })
    }

    /// Locate this refusal at `at`, for a flow that reports it through `?`.
    ///
    /// The arms map to the flow's own two retryable shapes on purpose. A
    /// failure before readiness is [`FlowError::Transient`]: the service may
    /// come up on a second attempt, and refusing to retry it would make a
    /// transient outage permanent. Everything else is
    /// [`FlowError::Failed`], because every one of them repeats.
    #[must_use]
    pub fn located(self, at: Arc<str>) -> FlowError {
        match self {
            Self::NotReady {
                what,
                generation,
                reason,
            } => FlowError::Transient {
                at,
                reason: format!("{what} did not become ready ({generation}): {reason}"),
            },
            Self::StaleGeneration {
                what,
                expected,
                got,
            } => FlowError::Failed {
                at,
                reason: format!("{what} refused a stale signal: expected {expected}, got {got}"),
            },
            Self::UnknownGeneration { what, got } => FlowError::Failed {
                at,
                reason: format!("{what} refused a signal from {got}, which it never issued"),
            },
            Self::AlreadyReady { what, generation } => FlowError::Failed {
                at,
                reason: format!(
                    "{what} is already ready ({generation}); a duplicate signal is refused"
                ),
            },
            Self::AlreadyFailed { what, generation } => FlowError::Failed {
                at,
                reason: format!("{what} already failed ({generation}); arm a new readiness"),
            },
            Self::Closed { what, generation } => FlowError::Failed {
                at,
                reason: format!("{what} was shut down ({generation}); a signal is refused"),
            },
            Self::DependantsFull { what, cap } => FlowError::Failed {
                at,
                reason: format!("{what} is at its declared cap of {cap} dependants"),
            },
            Self::InvalidName { reason } => FlowError::Failed {
                at,
                reason: format!("invalid readiness name: {reason}"),
            },
        }
    }
}

impl fmt::Display for ReadinessError {
    /// The refusal's own text, located at the empty path: this is a `Display`
    /// for a `Debug` line, and a caller that wants it located in a flow uses
    /// [`ReadinessError::located`](super::ReadinessError::located).
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.clone().located(Arc::from("")).to_string())
    }
}

impl std::error::Error for ReadinessError {}

impl From<ReadinessError> for FlowError {
    /// Lets `?` carry a readiness refusal out of a flow body; the location is
    /// filled at the step boundary the same way every other flow error is.
    fn from(source: ReadinessError) -> Self {
        source.located(Arc::from(""))
    }
}
