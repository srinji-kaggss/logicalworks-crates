//! The frontier's vocabulary: the keys a limit binds, what resolution and rules report, and the verdicts and refusals the gate hands back.

use std::time::Duration;

use super::InFlightPermit;

/// A key that a politeness limit is enforced against.
///
/// Ordered, so a frontier iterates its keys in a stable order and two runs of
/// the same schedule make the same selection. This is Heritrix's rule from
/// `WorkQueue::compareTo` — *the ordering may be arbitrary, but it must be
/// consistent and stable over time* — and it is why these are sorted containers
/// rather than hash maps.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum ConstraintKey {
    /// One hostname.
    Host(String),
    /// One origin the hostname resolves to: an address, a subnet, or an
    /// autonomous system. Several hosts share it, which is the point.
    Origin(String),
    /// One local egress interface. Every request uses it, so it is the limit
    /// that makes a whole run polite rather than each host within it.
    Egress(String),
}

impl ConstraintKey {
    /// The key for one hostname.
    #[must_use]
    pub fn host(host: impl Into<String>) -> Self {
        Self::Host(host.into())
    }

    /// The key for one origin.
    #[must_use]
    pub fn origin(origin: impl Into<String>) -> Self {
        Self::Origin(origin.into())
    }

    /// The key for one egress interface.
    #[must_use]
    pub fn egress(egress: impl Into<String>) -> Self {
        Self::Egress(egress.into())
    }
}

impl std::fmt::Display for ConstraintKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::Host(ref host) => write!(formatter, "host:{host}"),
            Self::Origin(ref origin) => write!(formatter, "origin:{origin}"),
            Self::Egress(ref egress) => write!(formatter, "egress:{egress}"),
        }
    }
}

/// What a host's name resolution produced, as a recorded observation.
///
/// Three-valued because the network answers in three ways. Absence from the map
/// is a fourth state — *not yet resolved* — and it is deliberately not spelled
/// here: "we have not looked" is not an observation, and giving it a variant
/// would let a caller record a resolution that never happened.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Resolved {
    /// The name resolved to infrastructure this run can key on.
    Origin(String),
    /// Resolution failed in a way that is not authoritative. RFC 8020 makes
    /// this the non-terminal arm: `SERVFAIL`, a timeout, or a transport error
    /// may answer differently on the next attempt.
    Failed,
    /// The name does not exist. `NXDOMAIN` is authoritative, so this is
    /// terminal in a way [`Self::Failed`] is not.
    DoesNotExist,
}

/// The state of the rules that govern one host, as fetched.
///
/// This is where the epistemics lives; [`Admission`] only reports what it means
/// for the scheduler. The variants and their normative sources are the module
/// documentation's three arrivals.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum RulesState {
    /// No fetch has been attempted. Distinct from [`Self::Unreachable`]: this
    /// arm carries no instant because nothing has been observed yet.
    #[default]
    NotFetched,
    /// The rules were fetched and permit this request.
    Allowed,
    /// The rules were fetched and forbid it. Terminal: the rule text is the
    /// server's, and is carried verbatim so the rejection is explicable.
    Disallowed {
        /// The matching rule line, as published.
        rule: String,
    },
    /// The rules are unavailable (4xx). RFC 9309 §2.3.1.3: the crawler MAY
    /// access any resources on the server.
    NoRules,
    /// The rules are unreachable (5xx). RFC 9309 §2.3.1.4: the crawler MUST
    /// assume complete disallow — but only *until a fresh, valid file is
    /// obtained*, so this arm carries the instant the failure was observed and
    /// is promotable to [`Self::NoRules`] after
    /// [`PolitenessPolicy::unreachable_grace`](super::PolitenessPolicy::unreachable_grace).
    Unreachable {
        /// Elapsed virtual time when the failure was observed.
        since: Duration,
    },
}

impl RulesState {
    /// This state as it stands at `at`, with RFC 9309 §2.3.1.4's expiry applied.
    ///
    /// Side-effect free, and the single definition of the time-driven part of
    /// the rules state machine: `Frontier::decide` reads it to form a verdict
    /// and `Frontier::promote_rules` writes it down when a reservation acts on
    /// that verdict. Two copies of this rule — one for the selector and one for
    /// the gate — is how a host becomes selectable some finite time before it
    /// becomes admissible.
    #[must_use]
    pub(super) fn effective(&self, at: Duration, grace: Duration) -> Self {
        match *self {
            Self::Unreachable { since } if at.saturating_sub(since) >= grace => Self::NoRules,
            ref unchanged => unchanged.clone(),
        }
    }
}

/// Why a request was held rather than dispatched or dropped.
///
/// A closed enum rather than a string, so a caller branches on a cause the
/// compiler enumerates and a new cause cannot arrive silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum DeferralKind {
    /// The server stated when to come back, via `Retry-After`. An explicit
    /// instruction, and the only kind whose instant the target chose.
    RetryAfter,
    /// Our own concurrency window is saturated or backing off.
    RateLimited,
    /// The rules are unreachable, so the request is held under RFC 9309
    /// §2.3.1.4's complete disallow. Provisional, unlike
    /// [`RejectKind::Disallowed`].
    OriginPolicyUnreachable,
    /// The rules have not been fetched. The prerequisite is a fetch.
    RulesNotFetched,
    /// The host has not been resolved in this run. The prerequisite is a
    /// resolution.
    OriginUnmapped,
    /// Resolution failed non-authoritatively. Unlike
    /// [`RejectKind::DoesNotExist`], the same name may resolve next time.
    ResolutionFailed,
}

impl DeferralKind {
    /// Whether this deferral is work the run can do rather than a wait.
    ///
    /// The distinction is the scheduler's: a prerequisite is scheduled now — a
    /// name to resolve, a rules file to fetch — while a wait is a permission to
    /// touch nothing until something else finishes. Kept here rather than left
    /// to the caller so that adding a deferral kind is a compile error at one
    /// exhaustive match instead of a silently-misclassified host.
    #[must_use]
    pub const fn needs_prerequisite(&self) -> bool {
        match *self {
            Self::OriginUnmapped
            | Self::ResolutionFailed
            | Self::RulesNotFetched
            | Self::OriginPolicyUnreachable => true,
            Self::RetryAfter | Self::RateLimited => false,
        }
    }
}

/// Why a request was dropped for good.
///
/// Terminal by construction: every arm means the item is dead. Nothing
/// provisional belongs in this enum, which is why unreachability and
/// non-authoritative resolution failure are deferrals.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RejectKind {
    /// The published rules forbid this request.
    Disallowed {
        /// The matching rule line, as published.
        rule: String,
    },
    /// The name does not exist. RFC 8020 makes `NXDOMAIN` authoritative.
    DoesNotExist,
    /// The frontier can no longer mint a permit identity, so nothing further can
    /// be admitted.
    ///
    /// Unreachable in any real run: an identity is a `u64` and a frontier would
    /// have to take 2^64 reservations — one per nanosecond for 584 years — to
    /// arrive here. It is modelled rather than saturated because a reused
    /// identity would release a slot belonging to a different request, which is
    /// the class of mistake this module's permit ledger exists to make
    /// unrepresentable.
    Exhausted,
}

/// What the scheduler should do with a request.
///
/// Three arms, each a distinct physical action, and no fourth. See the module
/// documentation for why the reasons are typed payloads rather than arms.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Admission {
    /// Dispatch now, and complete the permit exactly once when the request
    /// leaves flight.
    ///
    /// The payload is not decoration. It is the only thing
    /// [`Frontier::complete`](super::Frontier::complete) accepts, which is what stops a release from being
    /// attributed to whatever the host's name resolves to later.
    Admit(InFlightPermit),
    /// Keep the item and re-poll at `resume_at`.
    ///
    /// `resume_at` equal to the instant passed in means *no instant is
    /// knowable* — the window frees when an in-flight request completes — and
    /// the caller must re-poll rather than spin. It is deliberately not a
    /// guessed instant: a fabricated wake-up would be a measurement that was
    /// never taken.
    Defer {
        /// Elapsed virtual time at which the item becomes admissible, absent a
        /// new observation.
        resume_at: Duration,
        /// Why it was held.
        kind: DeferralKind,
        /// Which constraint bound. Reported rather than inferred, so a schedule
        /// that stalls is explicable — the same idiom as
        /// [`crate::session::MatchTier`] — and it is what makes the composite
        /// case, several limits with one binding, readable.
        constraint: ConstraintKey,
    },
    /// Drop the item permanently.
    Reject {
        /// Why it is dead.
        kind: RejectKind,
    },
}

/// What a completed request produced.
///
/// Three arms rather than a `cache_hit` boolean beside a success flag, because
/// the third is not a variety of the second: an edge cache answering fast says
/// nothing about the origin's capacity, and treating it as evidence is how a
/// latency-driven controller speeds up while the origin is saturated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Outcome {
    /// The origin answered and the request succeeded. Counts toward the growth
    /// signal.
    Success,
    /// The request succeeded and was served by an edge cache. Its slots are
    /// returned like any other completion, and it is excluded from the growth
    /// signal.
    CacheHit,
    /// The request failed with an explicit failure signal: the windows it
    /// belonged to shrink, so the next host behind the same origin inherits the
    /// caution.
    Failure,
}

/// Why a completion was refused.
///
/// A typed refusal rather than saturating subtraction. Releasing a slot is only
/// correct for the reservation that took it, and a frontier that cannot find
/// the reservation must say so: subtracting from zero looks identical to a
/// correct release, so the mistake surfaces as a slot that is occupied forever
/// and a run that is quietly slower than its policy says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CompletionError {
    /// The permit was not minted by this frontier.
    ///
    /// A permit is evidence only against the accounting that issued it. Every
    /// frontier keeps its own ledger, so presenting another frontier's permit
    /// here means the caller has confused two runs, and neither ledger can
    /// settle it.
    ForeignPermit,
    /// The permit names a reservation this frontier does not hold.
    ///
    /// Two ways to reach it, and they are the same observation: the request was
    /// already completed, or it was never admitted here. Neither is a reason to
    /// decrement somebody else's slot.
    UnknownPermit,
}

impl std::fmt::Display for CompletionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::ForeignPermit => formatter.write_str(
                "the permit was minted by a different frontier, so no reservation here matches it",
            ),
            Self::UnknownPermit => formatter.write_str(
                "no live reservation matches the permit: it was already completed, or never \
                 admitted here",
            ),
        }
    }
}

impl std::error::Error for CompletionError {}

/// Why a policy was refused.
///
/// A window of zero admits nothing, and a decrease factor outside `0..=100`
/// either freezes the window or inverts its direction. Both are configuration
/// mistakes that would surface as a run that never progresses, so they are
/// refused where they are made rather than discovered as a stall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PolitenessError {
    /// The minimum window was zero.
    ZeroWindow,
    /// The minimum exceeded the maximum.
    InvertedWindow {
        /// The declared minimum.
        min_window: u32,
        /// The declared maximum.
        max_window: u32,
    },
    /// The egress window was zero or above the maximum.
    InvalidEgressWindow {
        /// The declared width.
        egress_window: u32,
        /// The declared maximum.
        max_window: u32,
    },
    /// The decrease factor was zero or above one hundred.
    InvalidDecreasePercent {
        /// The declared factor.
        decrease_percent: u32,
    },
}

impl std::fmt::Display for PolitenessError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::ZeroWindow => formatter.write_str("a window of zero admits nothing"),
            Self::InvertedWindow {
                min_window,
                max_window,
            } => write!(
                formatter,
                "minimum window {min_window} exceeds maximum window {max_window}"
            ),
            Self::InvalidEgressWindow {
                egress_window,
                max_window,
            } => write!(
                formatter,
                "egress window {egress_window} is zero or above the maximum {max_window}"
            ),
            Self::InvalidDecreasePercent { decrease_percent } => write!(
                formatter,
                "decrease factor {decrease_percent} is outside 1..=100"
            ),
        }
    }
}

impl std::error::Error for PolitenessError {}
