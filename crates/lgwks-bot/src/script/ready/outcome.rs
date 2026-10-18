//! What a readiness settled on and what a dependant holds once it is released. Split from `ready.rs` (#526).

use std::sync::Arc;

use super::Generation;

/// What one [`Readiness`](super::Readiness) settled on.
///
/// A sum type rather than a `Result`, because it is *not* an error: a readiness
/// that failed is a fact the caller reads, and a failure after the release is
/// still a fact rather than a missing value. [`Readiness::outcome`](super::Readiness::outcome) returns
/// `Option<ReadyOutcome>` — `None` is "has not settled", the one thing no
/// variant of this type can mean.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReadyOutcome<T> {
    /// The instance released its dependants, carrying this evidence.
    Ready {
        /// The generation that was released.
        generation: Generation,
        /// The evidence the service published, shared with every dependant.
        value: Arc<T>,
    },
    /// The instance failed.
    Failed {
        /// The generation that failed.
        generation: Generation,
        /// What the service said.
        reason: Arc<str>,
    },
    /// The instance was shut down, which is a stop and not a failure.
    Closed {
        /// The generation that shut down.
        generation: Generation,
    },
}

impl<T> ReadyOutcome<T> {
    /// The generation this outcome settled at.
    #[must_use]
    pub const fn generation(&self) -> Generation {
        match *self {
            Self::Ready { generation, .. }
            | Self::Failed { generation, .. }
            | Self::Closed { generation } => generation,
        }
    }

    /// Whether the instance was released.
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        matches!(*self, Self::Ready { .. })
    }
}

/// A dependency's proof that the instance it waited on was released.
///
/// Holding one is holding a statement about a generation: it cannot be built
/// except by a wait that observed the release, so the value a dependant uses is
/// the value the readiness published for its generation rather than anything it
/// inferred from a timer or a poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ready<T> {
    /// The generation that was released.
    pub(super) generation: Generation,
    /// The evidence, shared with every other dependant.
    pub(super) value: Arc<T>,
}

impl<T> Ready<T> {
    /// The generation this release was published for, so a dependant can tell which instance of the service its evidence came from.
    #[must_use]
    pub const fn generation(&self) -> Generation {
        self.generation
    }

    /// The evidence the service published at that generation.
    ///
    /// A borrow, so a dependant reads the service's own answer rather than a
    /// copy it made and could have edited. Every dependant gets the same value:
    /// the `Arc` is what makes a thousand dependants cost one allocation rather
    /// than a thousand copies of whatever a service published.
    #[must_use]
    pub fn awaited(&self) -> &T {
        &self.value
    }

    /// The evidence, shared with the dependants that also received it.
    #[must_use]
    pub fn shared(&self) -> Arc<T> {
        Arc::clone(&self.value)
    }
}

/// A readiness that was released and then failed.
///
/// Recorded on the readiness rather than returned from [`Readiness::fail`](super::Readiness::fail),
/// because a failure after the release reaches the dependants through their
/// tokens rather than through a return value: by the time the service's body
/// calls `fail`, the dependants have already been released and are running, and
/// there is no caller left to hand a value to.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FailAfterReady {
    /// The generation that was released and then failed.
    pub(super) generation: Generation,
    /// What the service said.
    pub(super) reason: Arc<str>,
}

impl FailAfterReady {
    /// The generation that was released and then failed.
    #[must_use]
    pub const fn generation(&self) -> Generation {
        self.generation
    }

    /// What the service said.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}
