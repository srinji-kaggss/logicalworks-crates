//! `frontier` owns politeness and admission: whether a host may be contacted
//! right now, and if not, what the scheduler should do instead.
//!
//! A bot's canonical workload is an observe-evaluate-execute loop over a set of
//! sources, which is a crawler with an action attached. Rate is the trigger for
//! most blocking, so politeness is not a concession to the target: it is the
//! dominant term in whether a run completes at all.
//!
//! # The verdict is three-valued, and every arm is a physical action
//!
//! The scheduler does not care *why* work cannot proceed; it cares what to do
//! with the work item. There are exactly three things it can do, and
//! [`Admission`] has exactly three arms to match:
//!
//! | arm | the scheduler's action |
//! |---|---|
//! | [`Admission::Admit`] | dispatch now, holding the permit that reserved the slots |
//! | [`Admission::Defer`] | keep the item alive, re-poll at `resume_at` |
//! | [`Admission::Reject`] | drop the item permanently |
//!
//! A `bool` cannot hold these — *wait* and *no* are different answers — but
//! neither can a two-arm enum, because *now*, *wait*, and *no* are three.
//!
//! # Why the reasons are typed payloads and not a fourth arm
//!
//! The obvious next move is to promote each reason to its own top-level arm,
//! and it is the wrong one. Every top-level arm is a branch the *scheduler*
//! must write, and upstream failure mechanics are unbounded: a DNS `SERVFAIL`,
//! a TLS handshake timeout, and a redirect loop on `/robots.txt` are all "we
//! could not look", and none of them changes what the scheduler does. Widening
//! the verdict once per cause is how an execution token becomes an audit log.
//!
//! So the epistemics lives in [`RulesState`] and [`Resolved`], which are
//! explicit state machines carrying the full cause, and [`Admission`] reports
//! only the operational result. The distinction is preserved — as *typed data*
//! in a closed enum, where the compiler still enumerates it — rather than as
//! control flow. This is [`crate::session::MatchTier`]'s idiom: report which
//! tier produced a result, so a wrong result is explicable and repairable.
//!
//! The property that makes this safe is that nothing provisional is spelled
//! [`Admission::Reject`]. A terminal drop and a re-queue are different actions,
//! and a consumer that has to inspect a payload to learn which one it is
//! holding has no verdict at all.
//!
//! # The admission carries its reservation, and the permit is the only key
//!
//! An admission used to be a bare unit, and the caller released it by naming the
//! host: `release("a.example")`. That is a name, not a reservation. Resolution
//! is an observation that arrives *during* a run — that is what keeps an open
//! crawl open — so by the time a request finished, `a.example` could point at
//! different infrastructure than it did when the request was admitted. Releasing
//! by name then recomputed the host's constraints from the *new* topology: it
//! returned a slot against an origin this request never used and leaked the one
//! it did, and the leak is invisible because saturating subtraction of zero
//! looks exactly like a correct release.
//!
//! So [`Admission::Admit`] carries an [`InFlightPermit`]: an opaque, non-
//! clonable record of the exact constraint keys reserved, the host as spelled
//! at admission time, and the origin that host belonged to *then*.
//! [`Frontier::complete`] consumes it, and [`Frontier::observe_retry_after`]
//! borrows it, so a response can only be attributed to the request that earned
//! it. A permit that is dropped without completion leaves its slots held, which
//! is deliberate: the handle going out of scope is not evidence that the network
//! request stopped, so the conservative direction is to keep holding.
//!
//! # One decision, two callers
//!
//! [`Frontier::next_admissible`] and [`Frontier::admit`] are two questions about
//! the same state — *who is next* and *may I dispatch this one* — and they must
//! not be able to disagree. They are one function here:
//! [`Frontier::decide`], which takes `&self` and returns a [`Verdict`] with no
//! side effect at all. Selection returns a host only when that function says
//! [`Verdict::Admit`]; reservation reserves only when it says the same, and
//! re-derives the keys itself. The alternative that shipped — a selector that
//! reimplemented the predicate by hand — omitted the rules check, so a
//! permanently disallowed lexicographically-first host was selected on every
//! turn of the loop and starved every permitted host behind it.
//!
//! # Three arrivals of one invariant
//!
//! RFC 9309 gives the same observable — "no rules were retrieved" — two
//! opposite answers, decided by *why*:
//!
//! - §2.3.1.3: a **4xx** on `/robots.txt` means it is unavailable, and "the
//!   crawler MAY access any resources on the server" → [`RulesState::NoRules`].
//! - §2.3.1.4: a **5xx** means it is unreachable, and the crawler "MUST assume
//!   complete disallow" → [`RulesState::Unreachable`].
//!
//! Name resolution splits the same way, and RFC 8020 is why: `NXDOMAIN` is
//! authoritative — the name does not exist — while `SERVFAIL` and a timeout are
//! not, and the same name may resolve on the next attempt. So
//! [`Resolved::DoesNotExist`] rejects and [`Resolved::Failed`] defers.
//!
//! And [`RulesState::NotFetched`] is a third: *we have not tried* is not *we
//! tried and failed*, which is why the failure arms carry an instant and this
//! one does not.
//!
//! That section's expiry is time-driven, and a time-driven transition is the
//! easiest place for two predicates to drift. It is computed in exactly one
//! place — [`RulesState::effective`] — and *recorded* in exactly one place —
//! [`Frontier::promote_rules`], when a reservation acts on the verdict that
//! computed it. The selector reads the same computation, so a host whose grace
//! has expired is selectable at the same instant the gate would admit it, with
//! no independently maintained approximation on either side.
//!
//! # Politeness is keyed on infrastructure, not on hostnames
//!
//! A per-host limit is blind to what the host actually is. Two hundred Shopify
//! or Cloudflare-fronted domains are two hundred "independent" polite queues
//! that are in truth one reverse proxy receiving two hundred requests a second
//! from one egress address — the shape of a Layer 7 flood, and the usual reason
//! a crawler's egress gets tarpitted across every domain at once. The converse
//! is equally wrong: an Anycast name that resolves to a rotating edge pool is
//! not a delicate single server, and serializing against it wastes capacity it
//! has.
//!
//! So a request is admitted only when *every* constraint it belongs to admits,
//! and the binding one is the one with the longest wait. This is Mercator's
//! bounded per-host queue under a global host priority queue with the per-host
//! key generalized to whatever infrastructure the request actually shares;
//! `docs/bot-on-ecs.md` §8 specifies the frontier, and this module adds the
//! infrastructure keying it does not.
//!
//! # The window is an integer, and the clock is virtual
//!
//! The closest production implementation, Scrapy's `AutoThrottle`, derives a
//! delay from observed latency:
//!
//! ```text
//! target_delay = latency / target_concurrency
//! new_delay    = max(target_delay, (slot.delay + target_delay) / 2)
//! ```
//!
//! Three problems. `target_concurrency` is a divisor and not an enforced bound,
//! so nothing actually caps concurrency. A non-200 response returns without
//! assignment, so the controller *holds* on an error rather than backing off,
//! and it never reads `Retry-After`. And latency is a confounded signal: a CDN
//! edge cache answers fast, which makes the controller *speed up*, which is
//! backwards when the origin is the thing under strain — and HTTP/2
//! multiplexing makes per-request RTT a measure of head-of-line blocking as
//! much as of load.
//!
//! This module controls an integer concurrency window instead (Little's law),
//! which self-clocks: admit while fewer than `window` requests are in flight,
//! multiply the window down on an explicit failure signal, and add one back per
//! `increase_after` consecutive successes. No float, no latency divisor, and the
//! same schedule on every machine — which is what makes a virtual clock worth
//! having in the first place.
//!
//! Every method that needs the time is given it, as a [`Duration`] of elapsed
//! virtual time, which is exactly what `bevy_time::Time<Virtual>::elapsed()`
//! returns. The policy never reads a clock; a test advances one by hand.
//!
//! Reference targets, spelled out because this module also carries an outer doc
//! comment at its declaration and bare links in that position resolve against
//! the parent scope rather than this one.
//!
//! [`Duration`]: std::time::Duration
//! [`Admission`]: crate::frontier::Admission
//! [`Admission::Admit`]: crate::frontier::Admission::Admit
//! [`Admission::Defer`]: crate::frontier::Admission::Defer
//! [`Admission::Reject`]: crate::frontier::Admission::Reject
//! [`RulesState`]: crate::frontier::RulesState
//! [`RulesState::NoRules`]: crate::frontier::RulesState::NoRules
//! [`RulesState::Unreachable`]: crate::frontier::RulesState::Unreachable
//! [`RulesState::NotFetched`]: crate::frontier::RulesState::NotFetched
//! [`Resolved`]: crate::frontier::Resolved
//! [`Resolved::DoesNotExist`]: crate::frontier::Resolved::DoesNotExist
//! [`Resolved::Failed`]: crate::frontier::Resolved::Failed

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

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
    /// [`PolitenessPolicy::unreachable_grace`].
    Unreachable {
        /// Elapsed virtual time when the failure was observed.
        since: Duration,
    },
}

impl RulesState {
    /// This state as it stands at `at`, with RFC 9309 §2.3.1.4's expiry applied.
    ///
    /// Side-effect free, and the single definition of the time-driven part of
    /// the rules state machine: [`Frontier::decide`] reads it to form a verdict
    /// and [`Frontier::promote_rules`] writes it down when a reservation acts on
    /// that verdict. Two copies of this rule — one for the selector and one for
    /// the gate — is how a host becomes selectable some finite time before it
    /// becomes admissible.
    #[must_use]
    fn effective(&self, at: Duration, grace: Duration) -> Self {
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
    /// [`Frontier::complete`] accepts, which is what stops a release from being
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

/// Tunables for the adaptive window and the rules cache.
///
/// Private fields and a validating constructor, not a bag of public integers: a
/// policy whose minimum exceeds its maximum, or whose window is zero, is a run
/// that stalls, and the place to refuse that is where it is written.
///
/// The values are constructor arguments rather than constants inside the gate,
/// because the right ones depend on the target and on the operator's
/// obligations. [`Self::DEFAULT`] is declared, not fitted, and until a
/// calibration exists (see `docs/general-bot-fold.md` §6 item 13) it is an
/// unmeasured default rather than a validated one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PolitenessPolicy {
    /// Smallest a host or origin window may shrink to.
    min_window: u32,
    /// Largest a host or origin window may grow to.
    max_window: u32,
    /// Width the egress constraint starts at.
    egress_window: u32,
    /// Consecutive successes required before a window grows by one.
    increase_after: u32,
    /// Multiplicative-decrease factor as an integer percentage.
    decrease_percent: u32,
    /// How long an unreachable rules file is honoured.
    unreachable_grace: Duration,
    /// How long to wait before re-fetching rules or re-resolving a name.
    retry_after_failure: Duration,
}

impl PolitenessPolicy {
    /// The declared default. Unmeasured — see the type documentation.
    ///
    /// The egress starts at [`Self::max_window`] rather than
    /// [`Self::min_window`], because the egress interface is the one resource
    /// the operator owns and can measure. A run that serialized itself to one
    /// request at a time against its own network would be over-polite in a way
    /// nothing adapts out of, since a window only grows on success.
    pub const DEFAULT: Self = Self {
        min_window: 1,
        max_window: 4,
        egress_window: 4,
        increase_after: 8,
        decrease_percent: 70,
        unreachable_grace: Duration::from_secs(2_592_000),
        retry_after_failure: Duration::from_secs(60),
    };

    /// Validates a policy.
    pub fn new(
        min_window: u32,
        max_window: u32,
        egress_window: u32,
        increase_after: u32,
        decrease_percent: u32,
    ) -> Result<Self, PolitenessError> {
        if min_window == 0 {
            return Err(PolitenessError::ZeroWindow);
        }
        if min_window > max_window {
            return Err(PolitenessError::InvertedWindow {
                min_window,
                max_window,
            });
        }
        if egress_window == 0 || egress_window > max_window {
            return Err(PolitenessError::InvalidEgressWindow {
                egress_window,
                max_window,
            });
        }
        if decrease_percent == 0 || decrease_percent > 100 {
            return Err(PolitenessError::InvalidDecreasePercent { decrease_percent });
        }
        Ok(Self {
            min_window,
            max_window,
            egress_window,
            increase_after,
            decrease_percent,
            ..Self::DEFAULT
        })
    }

    /// Returns the smallest a window may shrink to.
    #[must_use]
    pub const fn min_window(&self) -> u32 {
        self.min_window
    }

    /// Returns the largest a window may grow to.
    #[must_use]
    pub const fn max_window(&self) -> u32 {
        self.max_window
    }

    /// Returns the width the egress constraint starts at.
    #[must_use]
    pub const fn egress_window(&self) -> u32 {
        self.egress_window
    }

    /// Returns the consecutive successes required to grow a window.
    #[must_use]
    pub const fn increase_after(&self) -> u32 {
        self.increase_after
    }

    /// Returns the multiplicative-decrease factor as a percentage.
    #[must_use]
    pub const fn decrease_percent(&self) -> u32 {
        self.decrease_percent
    }

    /// Returns how long an unreachable rules file is honoured.
    #[must_use]
    pub const fn unreachable_grace(&self) -> Duration {
        self.unreachable_grace
    }

    /// Returns how long to wait before re-fetching rules or re-resolving a name.
    #[must_use]
    pub const fn retry_after_failure(&self) -> Duration {
        self.retry_after_failure
    }

    /// The window after a multiplicative decrease from `window`.
    ///
    /// Floors at [`Self::min_window`] and uses saturating fixed-point
    /// arithmetic, so no operation here can wrap or divide by zero.
    #[must_use]
    pub fn decrease(&self, window: u32) -> u32 {
        let scaled = window
            .saturating_mul(self.decrease_percent)
            .saturating_div(100);
        scaled.clamp(self.min_window, self.max_window)
    }

    /// The window after one more consecutive success.
    #[must_use]
    pub fn increase(&self, window: u32) -> u32 {
        window
            .saturating_add(1)
            .clamp(self.min_window, self.max_window)
    }
}

impl Default for PolitenessPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Identity of one frontier's reservation ledger.
///
/// Compared by allocation identity, never by value, so a permit minted by one
/// frontier is refused by another with the same ordinal in its ledger. This is
/// why `Frontier` is not `Clone`: copying the ledger would produce two
/// accountants for one set of reservations, and a permit — which exists only
/// once — would settle one copy while the other held its slots forever.
#[derive(Debug)]
struct Issuer;

/// A reserved admission, and the only key that releases it.
///
/// Minted by [`Frontier::admit`], consumed by [`Frontier::complete`], and
/// borrowed by [`Frontier::observe_retry_after`]. It names the host as spelled
/// when the request was admitted, the origin that host belonged to *then*, and
/// the egress — the exact keys whose slots this reservation holds.
///
/// # Why it is not `Clone`, and why `Drop` does not release
///
/// One reservation means one slot set, so one handle releases it. A second
/// handle would either double-decrement or be a lie about which request owns
/// which slot, and the type makes both unrepresentable.
///
/// Dropping a permit leaves its slots held. That is the conservative direction
/// and it is deliberate: the handle going out of scope says the *caller* has
/// stopped looking, not that the request stopped. A frontier that assumed
/// otherwise would over-admit against hosts that are still being contacted, and
/// `docs/bot-on-ecs.md` §8.2's rule — *the intent must be durable before the
/// effect* — is the same asymmetry.
#[derive(Debug)]
#[must_use = "a permit dropped without `Frontier::complete` keeps its slots reserved; dropping \
              the handle is not evidence that the request left flight"]
pub struct InFlightPermit {
    /// The ledger this permit settles against.
    issuer: Arc<Issuer>,
    /// This reservation's ordinal within the issuing frontier. Unique among
    /// live reservations, which is what makes the ledger check meaningful.
    ordinal: u64,
    /// The host, as spelled when the request was admitted.
    host: String,
    /// The origin the host belonged to at admission time, if it had one. Not
    /// re-read from the topology when the request completes.
    origin: Option<ConstraintKey>,
    /// The egress this frontier admits over.
    egress: ConstraintKey,
}

impl InFlightPermit {
    /// The host this request was admitted for, as spelled at admission time.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Whether this reservation holds a slot against `key`.
    ///
    /// Reported so a test or a diagnostic can assert on what was reserved
    /// rather than re-deriving it from the topology, which is the mistake the
    /// permit exists to prevent.
    #[must_use]
    pub fn holds(&self, key: &ConstraintKey) -> bool {
        self.keys().iter().any(|held| held == key)
    }

    /// Every constraint this reservation holds a slot against.
    ///
    /// The host, the origin it belonged to at admission time, and the egress —
    /// built from the recorded fields, never from the frontier's current maps.
    fn keys(&self) -> Vec<ConstraintKey> {
        let mut keys = Vec::with_capacity(3);
        keys.push(ConstraintKey::host(self.host.clone()));
        keys.extend(self.origin.clone());
        keys.push(self.egress.clone());
        keys
    }

    /// The constraints an observation *about the server* is evidence for: the
    /// host and the origin, without the egress.
    ///
    /// The egress is excluded deliberately. It is the operator's own interface,
    /// and one server asking for a pause — or rate-limiting one tenant behind a
    /// shared proxy — is not evidence about the capacity of our network.
    /// Binding the egress here would let a single target halt every unrelated
    /// host in the run, which is the failure this distinction exists to prevent.
    fn server_keys(&self) -> Vec<ConstraintKey> {
        let mut keys = Vec::with_capacity(2);
        keys.push(ConstraintKey::host(self.host.clone()));
        keys.extend(self.origin.clone());
        keys
    }
}

/// Two permits are equal when they name the same reservation ordinal.
///
/// The issuer is deliberately not part of equality. It is an allocation
/// identity, and folding it in would make two identical schedules run against
/// two fresh frontiers produce unequal verdicts — which is the determinism
/// [`Frontier`]'s virtual clock exists to provide. Identity is enforced where it
/// belongs instead: in [`Frontier::complete`], which refuses a permit its ledger
/// does not hold.
impl PartialEq for InFlightPermit {
    fn eq(&self, other: &Self) -> bool {
        self.ordinal == other.ordinal
    }
}

impl Eq for InFlightPermit {}

/// The side-effect-free half of [`Admission`].
///
/// The same three arms, with `Admit` carrying nothing because nothing has been
/// reserved yet. Selection cannot mint a permit — it must not take a slot — and
/// reservation must not be able to act on a decision the selector would not
/// have made, so both read this one function and each maps it to what it is
/// allowed to do with it.
#[derive(Debug)]
enum Verdict {
    /// Every constraint admits, so the caller may reserve.
    Admit,
    /// Held, with the instant and the binding constraint.
    Defer {
        /// Elapsed virtual time at which the item becomes admissible.
        resume_at: Duration,
        /// Why it was held.
        kind: DeferralKind,
        /// Which constraint bound.
        constraint: ConstraintKey,
    },
    /// Dropped for good.
    Reject {
        /// Why it is dead.
        kind: RejectKind,
    },
}

impl Verdict {
    /// This verdict as a refusal, or `None` when it admitted.
    ///
    /// `Admit` carries no refusal, so it is not representable in the output:
    /// the caller that gets `None` knows it holds a permission and not a
    /// verdict shaped like one.
    fn refusal(self) -> Option<Admission> {
        match self {
            Self::Admit => None,
            Self::Defer {
                resume_at,
                kind,
                constraint,
            } => Some(Admission::Defer {
                resume_at,
                kind,
                constraint,
            }),
            Self::Reject { kind } => Some(Admission::Reject { kind }),
        }
    }
}

/// The adaptive state of one constraint.
#[derive(Debug, Clone, PartialEq, Eq)]
struct KeyState {
    /// Requests currently permitted to be in flight.
    window: u32,
    /// Requests admitted and not yet released.
    in_flight: u32,
    /// Consecutive non-cached successes since the window last changed.
    successes: u32,
    /// Instant before which nothing may be admitted, from `Retry-After`.
    blocked_until: Duration,
    /// The rules fetched for this key, when it is a host key.
    rules: RulesState,
}

/// The later of two admissions, breaking ties on the constraint key.
///
/// A free function rather than a method so the caller can hold a mutable borrow
/// of the state map while accumulating. The comparison is a total order over
/// `(instant, kind, key)`, which is Heritrix's comparator rule applied: the
/// choice between two equally-late deferrals must not depend on iteration order.
fn later(current: Option<Verdict>, candidate: Verdict) -> Verdict {
    let Some(current) = current else {
        return candidate;
    };
    let take_candidate = match (deferral_order(&current), deferral_order(&candidate)) {
        (Some(held), Some(offered)) => offered > held,
        (None, Some(_)) => true,
        (Some(_), None) | (None, None) => false,
    };
    if take_candidate { candidate } else { current }
}

/// The sort key for a deferral. `None` for a verdict that is not a deferral,
/// which never reaches [`later`]'s comparison in practice but is modelled
/// rather than papered over with a sentinel key.
fn deferral_order(verdict: &Verdict) -> Option<(Duration, DeferralKind, &ConstraintKey)> {
    match *verdict {
        Verdict::Defer {
            resume_at,
            kind,
            ref constraint,
        } => Some((resume_at, kind, constraint)),
        Verdict::Admit | Verdict::Reject { .. } => None,
    }
}

/// The backlog of hosts a run intends to contact, and the politeness state that
/// governs them.
///
/// Holds no clock. Every method that needs the time is given it, which is what
/// lets a test drive ten thousand requests through a virtual schedule without
/// touching wall-clock and get an identical result on every run.
///
/// Not `Clone`: the reservation ledger is a single accounting, and a copy would
/// be a second one that no permit can settle. See [`Issuer`].
#[derive(Debug)]
pub struct Frontier {
    /// The policy in force.
    policy: PolitenessPolicy,
    /// The egress constraint every request in this run is subject to.
    egress: ConstraintKey,
    /// What resolution produced for each host, as a recorded observation.
    resolutions: BTreeMap<String, Resolved>,
    /// The origin key each resolved host belongs to.
    constraints: BTreeMap<String, ConstraintKey>,
    /// Per-constraint adaptive state.
    state: BTreeMap<ConstraintKey, KeyState>,
    /// Hosts abandoned for the rest of the run.
    abandons: BTreeSet<String>,
    /// This frontier's ledger identity, carried by every permit it mints.
    issuer: Arc<Issuer>,
    /// Ordinals of the reservations currently held.
    reservations: BTreeSet<u64>,
    /// The next ordinal to mint.
    next_ordinal: u64,
}

impl Frontier {
    /// A frontier over one egress interface.
    #[must_use]
    pub fn new(policy: PolitenessPolicy, egress: impl Into<String>) -> Self {
        let egress = ConstraintKey::egress(egress);
        let mut frontier = Self {
            policy,
            egress,
            resolutions: BTreeMap::new(),
            constraints: BTreeMap::new(),
            state: BTreeMap::new(),
            abandons: BTreeSet::new(),
            issuer: Arc::new(Issuer),
            reservations: BTreeSet::new(),
            // One, not zero: an ordinal of zero would be indistinguishable from
            // a default-constructed sentinel, and the first reservation of a run
            // reading as "no reservation" is the kind of quiet ambiguity this
            // module's ledger is here to remove.
            next_ordinal: 1,
        };
        let key = frontier.egress.clone();
        let width = policy.egress_window();
        frontier.entry(&key).window = width;
        frontier
    }

    /// The policy in force.
    #[must_use]
    pub fn policy(&self) -> &PolitenessPolicy {
        &self.policy
    }

    /// How many requests are in flight against `key`.
    #[must_use]
    pub fn in_flight(&self, key: &ConstraintKey) -> u32 {
        self.state.get(key).map_or(0, |entry| entry.in_flight)
    }

    /// How many reservations this frontier is holding.
    ///
    /// The ledger's own count, for a test or an operator that wants to assert
    /// conservation: every admission raises it by one and every completion
    /// lowers it by one, whatever the topology did in between.
    #[must_use]
    pub fn outstanding(&self) -> usize {
        self.reservations.len()
    }

    /// The current window for `key`, or the policy minimum if never used.
    #[must_use]
    pub fn window(&self, key: &ConstraintKey) -> u32 {
        self.state
            .get(key)
            .map_or(self.policy.min_window(), |entry| entry.window)
    }

    /// The rules state recorded for `key`.
    ///
    /// The state as *recorded*, not as it stands at some instant: an
    /// unreachability whose grace has expired still reads as
    /// [`RulesState::Unreachable`] until something acts on it. Pass the instant
    /// to [`Self::decide`] to get the verdict that expiry implies.
    #[must_use]
    pub fn rules(&self, key: &ConstraintKey) -> RulesState {
        self.state
            .get(key)
            .map_or(RulesState::NotFetched, |entry| entry.rules.clone())
    }

    /// Whether `host` is one this run has abandoned for good.
    #[must_use]
    pub fn is_abandoned(&self, host: &str) -> bool {
        self.abandons.contains(host)
    }

    /// The state for `key`, created at the window floor if this is its first
    /// use. A constraint nobody has exercised is not yet owed any confidence,
    /// so it starts at the minimum rather than the maximum.
    fn entry(&mut self, key: &ConstraintKey) -> &mut KeyState {
        let floor = self.policy.min_window();
        self.state.entry(key.clone()).or_insert_with(|| KeyState {
            window: floor,
            in_flight: 0,
            successes: 0,
            blocked_until: Duration::ZERO,
            rules: RulesState::NotFetched,
        })
    }

    /// Records what resolving `host` produced.
    ///
    /// Resolution is an observation, so this is how a run learns its topology —
    /// and it is a *recorded event*, not a snapshot taken before the run. That
    /// distinction is the whole reason an open crawl stays deterministic: a
    /// crawler discovers hostnames as it goes, so a topology fixed at `t0` could
    /// only ever describe a closed set of seeds, which is not a crawler.
    /// Recording each resolution as it happens keeps the graph open and the
    /// replay exact.
    ///
    /// A request already in flight is unaffected: its permit holds the origin it
    /// was admitted against, so re-resolving the name moves which origin the
    /// *next* admission shares without moving any current one.
    pub fn observe_resolution(&mut self, host: &str, resolved: Resolved) {
        match resolved {
            Resolved::Origin(ref origin) => {
                self.constraints
                    .insert(host.to_owned(), ConstraintKey::origin(origin.clone()));
            }
            Resolved::Failed | Resolved::DoesNotExist => {
                self.constraints.remove(host);
            }
        }
        self.resolutions.insert(host.to_owned(), resolved);
    }

    /// Records the rules fetched for `key`.
    pub fn observe_rules(&mut self, key: &ConstraintKey, rules: RulesState) {
        self.entry(key).rules = rules;
    }

    /// Records an explicit back-off the target asked for, via `Retry-After`.
    ///
    /// Bound to the permit rather than to a hostname, because the pause is
    /// evidence from one response: attributing it to whatever `host` resolves to
    /// now is how a pause earned against one origin lands on another. Applied to
    /// the permit's host and its admission-time origin — a server asking for a
    /// pause is asking a client to pause, not one name that happens to point at
    /// it — and never to the egress.
    ///
    /// Must be called before [`Self::complete`], which consumes the permit. The
    /// borrow makes any other order unwritable.
    pub fn observe_retry_after(
        &mut self,
        permit: &InFlightPermit,
        at: Duration,
        delay: Duration,
    ) -> Result<(), CompletionError> {
        self.verify(permit)?;
        let until = at.saturating_add(delay);
        for key in permit.server_keys() {
            let entry = self.entry(&key);
            if entry.blocked_until < until {
                entry.blocked_until = until;
            }
        }
        Ok(())
    }

    /// Every constraint a request to `host` is subject to, as the topology
    /// stands now. Used to reserve a *new* admission; never to settle an old
    /// one, which reads its keys from the permit.
    fn keys_for(&self, host: &str) -> Vec<ConstraintKey> {
        let mut keys = Vec::with_capacity(3);
        keys.push(ConstraintKey::host(host));
        keys.extend(self.constraints.get(host).cloned());
        keys.push(self.egress.clone());
        keys
    }

    /// The rules for `key` as they effectively stand at `at`.
    ///
    /// The stored state with RFC 9309 §2.3.1.4's expiry applied. The selector
    /// reads this, so a grace expiry is visible to selection at the same instant
    /// the gate would act on it.
    fn effective_rules(&self, key: &ConstraintKey, at: Duration) -> RulesState {
        let grace = self.policy.unreachable_grace();
        self.state.get(key).map_or(RulesState::NotFetched, |entry| {
            entry.rules.effective(at, grace)
        })
    }

    /// Writes down the expiry [`Self::effective_rules`] computes, for a
    /// reservation that acted on it.
    ///
    /// The one place a time-driven rules transition is *recorded*. It changes
    /// nothing about what the next verdict will be — `effective` is a fixed
    /// point once the grace has elapsed — so this cannot make the stored state
    /// disagree with the decision that was just taken.
    fn promote_rules(&mut self, key: &ConstraintKey, at: Duration) {
        let grace = self.policy.unreachable_grace();
        let entry = self.entry(key);
        let promoted = entry.rules.effective(at, grace);
        if promoted != entry.rules {
            entry.rules = promoted;
        }
    }

    /// Decides what to do with a request to `host` at `at`, without changing
    /// anything.
    ///
    /// The single admission predicate. [`Self::admit`] reserves only when this
    /// says [`Verdict::Admit`], [`Self::next_admissible`] returns a host only
    /// when this says the same, and [`Self::next_prerequisite`] reports the
    /// deferrals that are work rather than waits. Every arm is a physical
    /// action; there is no fourth.
    fn decide(&self, host: &str, at: Duration) -> Verdict {
        if self.abandons.contains(host) {
            return Verdict::Reject {
                kind: RejectKind::DoesNotExist,
            };
        }

        match self.resolutions.get(host) {
            None => {
                return Verdict::Defer {
                    resume_at: at,
                    kind: DeferralKind::OriginUnmapped,
                    constraint: ConstraintKey::host(host),
                };
            }
            Some(&Resolved::Failed) => {
                return Verdict::Defer {
                    resume_at: at.saturating_add(self.policy.retry_after_failure()),
                    kind: DeferralKind::ResolutionFailed,
                    constraint: ConstraintKey::host(host),
                };
            }
            Some(&Resolved::DoesNotExist) => {
                return Verdict::Reject {
                    kind: RejectKind::DoesNotExist,
                };
            }
            Some(&Resolved::Origin(_)) => {}
        }

        // A site's published rules are per-host, so only the host key is
        // consulted; the origin keys carry a window, not a ruleset.
        let host_key = ConstraintKey::host(host);
        match self.effective_rules(&host_key, at) {
            RulesState::Allowed | RulesState::NoRules => {}
            RulesState::Disallowed { rule } => {
                return Verdict::Reject {
                    kind: RejectKind::Disallowed { rule },
                };
            }
            RulesState::NotFetched => {
                return Verdict::Defer {
                    resume_at: at,
                    kind: DeferralKind::RulesNotFetched,
                    constraint: host_key,
                };
            }
            RulesState::Unreachable { .. } => {
                // §2.3.1.4's complete disallow, with that section's own expiry:
                // after a reasonably long period the crawler MAY treat the file
                // as unavailable, which is §2.3.1.3's allow. The expiry is
                // already applied by `effective_rules`, so a state that is
                // still `Unreachable` here is inside its grace.
                return Verdict::Defer {
                    resume_at: at.saturating_add(self.policy.retry_after_failure()),
                    kind: DeferralKind::OriginPolicyUnreachable,
                    constraint: host_key,
                };
            }
        }

        let mut binding: Option<Verdict> = None;
        for key in self.keys_for(host) {
            if let Some(wait) = self.saturated(&key, at) {
                binding = Some(later(binding, wait));
            }
        }

        binding.unwrap_or(Verdict::Admit)
    }

    /// The deferral for `key` at `at`, or `None` when it has room.
    ///
    /// Reads `&self` and creates nothing: a constraint nobody has exercised is
    /// not yet owed a record, and [`Self::decide`] must stay free of the side
    /// effects that would make the selector and the gate two implementations.
    fn saturated(&self, key: &ConstraintKey, at: Duration) -> Option<Verdict> {
        let entry = self.state.get(key)?;
        if entry.blocked_until > at {
            Some(Verdict::Defer {
                resume_at: entry.blocked_until,
                kind: DeferralKind::RateLimited,
                constraint: key.clone(),
            })
        } else if entry.in_flight >= entry.window {
            Some(Verdict::Defer {
                resume_at: at,
                kind: DeferralKind::RateLimited,
                constraint: key.clone(),
            })
        } else {
            None
        }
    }

    /// Decides what to do with a request to `host` at `at`.
    ///
    /// Admits only when every constraint admits, and reports the one that bound
    /// when it does not. On [`Admission::Admit`] the caller owes exactly one
    /// [`Self::complete`] of the permit it was handed, once the request leaves
    /// flight.
    ///
    /// The decision is [`Self::decide`]'s, not a second evaluation of the same
    /// question: a caller that has already asked [`Self::next_admissible`] gets
    /// the same answer here on unchanged state, which is what lets a scheduler
    /// select and dispatch without maintaining a shadow predicate of its own.
    pub fn admit(&mut self, host: &str, at: Duration) -> Admission {
        match self.decide(host, at).refusal() {
            Some(refused) => refused,
            None => self.reserve(host, at),
        }
    }

    /// Takes the slots `decide` found free and mints the permit for them.
    ///
    /// Reached only from [`Self::admit`], immediately after that same call
    /// returned [`Verdict::Admit`] — there is no second predicate here to
    /// disagree with it. What this adds is the record: the exact keys reserved,
    /// the origin as it stood at this instant, and an identity the ledger
    /// recognises when the request completes.
    fn reserve(&mut self, host: &str, at: Duration) -> Admission {
        let ordinal = self.next_ordinal;
        let Some(next) = ordinal.checked_add(1) else {
            return Admission::Reject {
                kind: RejectKind::Exhausted,
            };
        };

        // The verdict that admitted this request may have rested on RFC 9309
        // §2.3.1.4's expiry. Recording it is the other half of computing it in
        // one place: the state catches up with the decision that acted on it,
        // and `effective_rules` stays the only definition of the rule.
        let host_key = ConstraintKey::host(host);
        self.promote_rules(&host_key, at);

        let keys = self.keys_for(host);
        for key in &keys {
            let entry = self.entry(key);
            entry.in_flight = entry.in_flight.saturating_add(1);
        }

        self.next_ordinal = next;
        self.reservations.insert(ordinal);
        Admission::Admit(InFlightPermit {
            issuer: Arc::clone(&self.issuer),
            ordinal,
            host: host.to_owned(),
            origin: self.constraints.get(host).cloned(),
            egress: self.egress.clone(),
        })
    }

    /// Consumes `permit` exactly once, returns its slots, and folds the
    /// outcome into the windows it belongs to.
    ///
    /// The keys come from the permit — the host as spelled and the origin it
    /// belonged to when the request was admitted — so a resolution observed
    /// while the request was in flight cannot move the release onto an origin
    /// this request never touched. That is the whole repair: a release by name
    /// recomputed the keys from the *current* topology.
    ///
    /// Refuses a permit this frontier's ledger does not hold, rather than
    /// subtracting from zero: the two look identical in the counters, and only
    /// one of them is a correct release.
    pub fn complete(
        &mut self,
        permit: InFlightPermit,
        outcome: Outcome,
    ) -> Result<(), CompletionError> {
        self.verify(&permit)?;
        self.reservations.remove(&permit.ordinal);

        for key in permit.keys() {
            let entry = self.entry(&key);
            // Saturating, but not *hiding* anything: the ledger check above is
            // what makes a zero here mean "this reservation held no slot", and
            // a reservation that holds no slot was never minted.
            entry.in_flight = entry.in_flight.saturating_sub(1);
        }

        let policy = self.policy;
        for key in permit.server_keys() {
            let entry = self.entry(&key);
            match outcome {
                Outcome::Success => {
                    entry.successes = entry.successes.saturating_add(1);
                    if entry.successes >= policy.increase_after() {
                        entry.successes = 0;
                        entry.window = policy.increase(entry.window);
                    }
                }
                // An edge cache answering fast says nothing about origin
                // capacity, and counting it is how a latency controller speeds
                // up while the origin is saturated.
                Outcome::CacheHit => {}
                Outcome::Failure => {
                    entry.successes = 0;
                    entry.window = policy.decrease(entry.window);
                }
            }
        }
        Ok(())
    }

    /// Whether `permit` names a live reservation in this frontier's ledger.
    ///
    /// The check that turns "the caller confused two requests" into a typed
    /// refusal. Both refusals are reachable only through misuse, which is the
    /// point: the ledger exists so that misuse is loud rather than a slot that
    /// silently stays occupied for the rest of the run.
    fn verify(&self, permit: &InFlightPermit) -> Result<(), CompletionError> {
        if !Arc::ptr_eq(&permit.issuer, &self.issuer) {
            return Err(CompletionError::ForeignPermit);
        }
        if !self.reservations.contains(&permit.ordinal) {
            return Err(CompletionError::UnknownPermit);
        }
        Ok(())
    }

    /// Abandons `host` for the rest of the run.
    pub fn abandon(&mut self, host: &str) {
        self.abandons.insert(host.to_owned());
    }

    /// The next host that would be admitted at `at`, chosen in key order.
    ///
    /// The order is the sorted order of the candidate hostnames, which is
    /// stable across runs and machines. It is arbitrary in the sense Heritrix's
    /// comparator rule allows, and consistent in the sense that rule requires.
    ///
    /// Selection is the same predicate as admission: a host is returned only
    /// when [`Self::admit`] would return [`Admission::Admit`] for it on
    /// unchanged state. A host the gate will refuse or defer — disallowed,
    /// unfetched rules, unresolved, saturated — is skipped rather than returned
    /// and re-skipped forever, which is what let a permanently disallowed
    /// lexicographically-first host starve every host behind it.
    #[must_use]
    pub fn next_admissible(&self, at: Duration) -> Option<String> {
        self.resolutions
            .keys()
            .find(|host| matches!(self.decide(host, at), Verdict::Admit))
            .cloned()
    }

    /// The next host whose held-up work is a prerequisite rather than a wait.
    ///
    /// [`Self::next_admissible`] answering `None` says nothing about *why* the
    /// backlog is stuck, and the two answers schedule differently: a host that
    /// needs its name resolved or its rules fetched is work the run can do now,
    /// while a host deferred by capacity is work nothing can advance. A
    /// scheduler that treats both as "nothing to do" deadlocks on the first kind
    /// and idles correctly on the second.
    ///
    /// Returns the host and the deferral that held it, so the caller dispatches
    /// the fetch the kind names rather than guessing at it. Derives the same
    /// [`Self::decide`] verdict as selection and admission; it is a view of one
    /// predicate, not a third one.
    #[must_use]
    pub fn next_prerequisite(&self, at: Duration) -> Option<(String, DeferralKind)> {
        self.resolutions
            .keys()
            .find_map(|host| match self.decide(host, at) {
                Verdict::Defer { kind, .. } if kind.needs_prerequisite() => {
                    Some((host.clone(), kind))
                }
                Verdict::Admit | Verdict::Defer { .. } | Verdict::Reject { .. } => None,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: Duration = Duration::from_secs(3_600);
    const THIRTY_ONE_DAYS: Duration = Duration::from_secs(2_678_400);

    /// A test that cannot fail silently: every helper below returns a value or a
    /// message, and `?` carries either out to the harness.
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn frontier() -> Frontier {
        Frontier::new(PolitenessPolicy::DEFAULT, "eth0")
    }

    /// A host that is resolved, permitted, and otherwise ready to go.
    fn ready(frontier: &mut Frontier, host: &str, origin: &str) {
        frontier.observe_resolution(host, Resolved::Origin(origin.to_owned()));
        frontier.observe_rules(&ConstraintKey::host(host), RulesState::Allowed);
    }

    /// Destructures a deferral into its parts, so a test asserts on real values
    /// rather than on a pattern match that could hold for the wrong reason.
    fn deferral(admission: &Admission) -> Option<(Duration, DeferralKind, ConstraintKey)> {
        match *admission {
            Admission::Defer {
                resume_at,
                kind,
                ref constraint,
            } => Some((resume_at, kind, constraint.clone())),
            Admission::Admit(_) | Admission::Reject { .. } => None,
        }
    }

    /// Admits `host`, asserting that the frontier took the reservation, and
    /// returns the permit that releases it.
    ///
    /// A refusal comes back as an error naming what arrived instead, so a test
    /// that expected an admission fails with the verdict rather than with a
    /// panic that says nothing about which arm it got.
    fn admitted(
        frontier: &mut Frontier,
        host: &str,
        at: Duration,
    ) -> Result<InFlightPermit, Box<dyn std::error::Error>> {
        match frontier.admit(host, at) {
            Admission::Admit(permit) => Ok(permit),
            other => Err(format!("expected an admission for {host}, got {other:?}").into()),
        }
    }

    /// Admits `host` and completes it with `outcome`, asserting both halves of
    /// the exchange. The one-line form of "a request went out and came back".
    fn completed(
        frontier: &mut Frontier,
        host: &str,
        outcome: Outcome,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let permit = admitted(frontier, host, Duration::ZERO)?;
        frontier.complete(permit, outcome)?;
        Ok(())
    }

    #[test]
    fn an_unresolved_host_defers_rather_than_rejects() {
        let mut frontier = frontier();
        assert_eq!(
            deferral(&frontier.admit("a.example", Duration::ZERO)),
            Some((
                Duration::ZERO,
                DeferralKind::OriginUnmapped,
                ConstraintKey::host("a.example"),
            )),
            "a crawler discovers hosts as it goes, so an unknown host is a \
             prerequisite to resolve and not a dead item"
        );
    }

    #[test]
    fn nxdomain_rejects_and_servfail_defers() {
        let mut frontier = frontier();
        frontier.observe_resolution("gone.example", Resolved::DoesNotExist);
        frontier.observe_resolution("flaky.example", Resolved::Failed);

        assert_eq!(
            frontier.admit("gone.example", Duration::ZERO),
            Admission::Reject {
                kind: RejectKind::DoesNotExist
            },
            "RFC 8020 makes NXDOMAIN authoritative"
        );

        assert_eq!(
            deferral(&frontier.admit("flaky.example", Duration::ZERO)),
            Some((
                PolitenessPolicy::DEFAULT.retry_after_failure(),
                DeferralKind::ResolutionFailed,
                ConstraintKey::host("flaky.example"),
            )),
            "SERVFAIL is not authoritative, so it must not be terminal"
        );
    }

    #[test]
    fn rules_states_follow_rfc_9309() -> TestResult {
        let mut frontier = frontier();
        ready(&mut frontier, "a.example", "origin-a");
        let key = ConstraintKey::host("a.example");

        frontier.observe_rules(&key, RulesState::NotFetched);
        assert_eq!(
            deferral(&frontier.admit("a.example", Duration::ZERO)).map(|parts| parts.1),
            Some(DeferralKind::RulesNotFetched),
            "unfetched rules are a prerequisite, not a wait"
        );

        // 4xx: unavailable, MAY access any resources.
        frontier.observe_rules(&key, RulesState::NoRules);
        let permit = admitted(&mut frontier, "a.example", HOUR)?;
        frontier.complete(permit, Outcome::Success)?;

        // 5xx: unreachable, MUST assume complete disallow.
        frontier.observe_rules(
            &key,
            RulesState::Unreachable {
                since: Duration::ZERO,
            },
        );
        assert_eq!(
            deferral(&frontier.admit("a.example", HOUR)).map(|parts| parts.1),
            Some(DeferralKind::OriginPolicyUnreachable),
            "RFC 9309 2.3.1.4 requires assuming complete disallow"
        );

        // ...but only until a fresh file is obtained, which that section bounds.
        assert!(
            matches!(
                frontier.admit("a.example", THIRTY_ONE_DAYS),
                Admission::Admit(_)
            ),
            "RFC 9309 2.3.1.4 lets a long-unreachable file be treated as unavailable"
        );
        assert_eq!(
            frontier.rules(&key),
            RulesState::NoRules,
            "taking the admission is what records the expiry the verdict rested on"
        );
        Ok(())
    }

    #[test]
    fn disallowed_is_terminal_and_unreachable_is_not() {
        let mut frontier = frontier();
        ready(&mut frontier, "a.example", "origin-a");
        let key = ConstraintKey::host("a.example");

        frontier.observe_rules(
            &key,
            RulesState::Disallowed {
                rule: "Disallow: /private".to_owned(),
            },
        );
        assert_eq!(
            frontier.admit("a.example", Duration::ZERO),
            Admission::Reject {
                kind: RejectKind::Disallowed {
                    rule: "Disallow: /private".to_owned()
                }
            }
        );

        // The same request under an unreachable file must NOT be rejected:
        // that would turn a transient 5xx into a permanent deny-list.
        frontier.observe_rules(
            &key,
            RulesState::Unreachable {
                since: Duration::ZERO,
            },
        );
        assert_eq!(
            deferral(&frontier.admit("a.example", HOUR)).map(|parts| parts.1),
            Some(DeferralKind::OriginPolicyUnreachable),
            "unreachability is provisional and must never be spelled Reject"
        );
    }

    #[test]
    fn hosts_sharing_an_origin_share_one_window() {
        let mut frontier = frontier();
        // Eight Shopify-shaped tenants: distinct names, one proxy.
        for index in 0..8_u32 {
            ready(
                &mut frontier,
                &format!("shop{index}.example"),
                "shopify-edge",
            );
        }

        let mut admitted = 0_u32;
        for index in 0..8_u32 {
            if matches!(
                frontier.admit(&format!("shop{index}.example"), Duration::ZERO),
                Admission::Admit(_)
            ) {
                admitted = admitted.saturating_add(1);
            }
        }

        assert_eq!(
            admitted,
            PolitenessPolicy::DEFAULT.min_window(),
            "eight 'independent' hosts behind one proxy must share one window, \
             or the run is a Layer 7 flood with a polite-looking config"
        );
        assert_eq!(
            frontier.in_flight(&ConstraintKey::origin("shopify-edge")),
            PolitenessPolicy::DEFAULT.min_window()
        );
    }

    #[test]
    fn distinct_origins_are_limited_by_egress_alone() {
        let mut frontier = frontier();
        for index in 0..4_u32 {
            let host = format!("h{index}.example");
            ready(&mut frontier, &host, &format!("origin-{index}"));
        }

        let mut admitted = 0_u32;
        for index in 0..4_u32 {
            if matches!(
                frontier.admit(&format!("h{index}.example"), Duration::ZERO),
                Admission::Admit(_)
            ) {
                admitted = admitted.saturating_add(1);
            }
        }
        assert_eq!(
            admitted,
            PolitenessPolicy::DEFAULT.egress_window(),
            "four hosts on four origins are bounded by the egress, not by each other"
        );
        assert_eq!(
            deferral(&frontier.admit("h0.example", Duration::ZERO)).map(|parts| parts.2),
            Some(ConstraintKey::egress("eth0")),
            "and the binding constraint is the egress"
        );
    }

    #[test]
    fn the_binding_constraint_is_reported() {
        let mut frontier = frontier();
        ready(&mut frontier, "a.example", "origin-a");

        assert!(
            matches!(
                frontier.admit("a.example", Duration::ZERO),
                Admission::Admit(_)
            ),
            "a resolved, permitted host on a free origin and egress admits"
        );
        assert_eq!(
            deferral(&frontier.admit("a.example", Duration::ZERO)).map(|parts| parts.1),
            Some(DeferralKind::RateLimited),
            "a window of one admits exactly once"
        );
    }

    #[test]
    fn retry_after_binds_the_origin_not_just_the_host() -> TestResult {
        let mut frontier = frontier();
        ready(&mut frontier, "a.example", "origin-a");
        ready(&mut frontier, "b.example", "origin-a");

        let permit = admitted(&mut frontier, "a.example", Duration::ZERO)?;
        frontier.observe_retry_after(&permit, Duration::ZERO, Duration::from_secs(30))?;
        frontier.complete(permit, Outcome::Success)?;

        assert_eq!(
            deferral(&frontier.admit("b.example", Duration::from_secs(1))),
            Some((
                Duration::from_secs(30),
                DeferralKind::RateLimited,
                ConstraintKey::origin("origin-a"),
            )),
            "a pause asked of the client binds its sibling on the same origin"
        );
        assert!(
            matches!(
                frontier.admit("b.example", Duration::from_secs(31)),
                Admission::Admit(_)
            ),
            "and it lifts when the instant it named arrives"
        );
        Ok(())
    }

    #[test]
    fn a_retry_after_does_not_bind_the_egress() -> TestResult {
        let mut frontier = frontier();
        ready(&mut frontier, "a.example", "origin-a");
        ready(&mut frontier, "unrelated.example", "origin-z");

        let permit = admitted(&mut frontier, "a.example", Duration::ZERO)?;
        frontier.observe_retry_after(&permit, Duration::ZERO, Duration::from_secs(30))?;

        assert!(
            matches!(
                frontier.admit("unrelated.example", Duration::from_secs(1)),
                Admission::Admit(_)
            ),
            "one server asking for a pause is not evidence about our network's \
             capacity; binding the egress would let a single target halt every \
             unrelated host in the run"
        );
        assert_eq!(
            deferral(&frontier.admit("a.example", Duration::from_secs(1))).map(|parts| parts.2),
            Some(ConstraintKey::origin("origin-a")),
            "but it does bind the server-side keys the target actually shares"
        );
        Ok(())
    }

    #[test]
    fn the_window_shrinks_on_failure_and_grows_on_success() -> TestResult {
        let mut frontier = frontier();
        ready(&mut frontier, "a.example", "origin-a");
        let key = ConstraintKey::origin("origin-a");

        for _ in 0..PolitenessPolicy::DEFAULT.increase_after() {
            completed(&mut frontier, "a.example", Outcome::Success)?;
        }
        assert_eq!(
            frontier.window(&key),
            2,
            "eight successes add one to a window of one"
        );

        completed(&mut frontier, "a.example", Outcome::Failure)?;
        assert_eq!(frontier.window(&key), 1, "2 x 70% floors at min_window");

        for _ in 0..5 {
            completed(&mut frontier, "a.example", Outcome::Failure)?;
        }
        assert_eq!(
            frontier.window(&key),
            PolitenessPolicy::DEFAULT.min_window(),
            "a window of zero would admit nothing forever"
        );
        Ok(())
    }

    #[test]
    fn a_cache_hit_does_not_grow_the_window() -> TestResult {
        let mut frontier = frontier();
        ready(&mut frontier, "a.example", "origin-a");
        let key = ConstraintKey::origin("origin-a");

        for _ in 0..PolitenessPolicy::DEFAULT.increase_after() {
            completed(&mut frontier, "a.example", Outcome::CacheHit)?;
        }
        assert_eq!(
            frontier.window(&key),
            PolitenessPolicy::DEFAULT.min_window(),
            "an edge cache answering fast says nothing about origin capacity, and \
             counting it is how a latency controller speeds up under load"
        );
        assert_eq!(
            frontier.in_flight(&key),
            0,
            "a cache hit still returns the slots it held; only the growth signal \
             is excluded"
        );
        Ok(())
    }

    // ── The reservation is bound to the admission, not to the name ─────────

    #[test]
    fn releasing_after_reresolution_preserves_other_requests() -> TestResult {
        // The filed counterexample. One-request origin window, four-request
        // egress window: A and B are admitted against two origins, A's name is
        // then re-resolved onto B's origin while A's request is still running,
        // and A completes.
        let mut frontier = frontier();
        for (host, origin) in [
            ("a.example", "old"),
            ("b.example", "new"),
            ("c.example", "new"),
        ] {
            frontier.observe_resolution(host, Resolved::Origin(origin.to_owned()));
            frontier.observe_rules(&ConstraintKey::host(host), RulesState::Allowed);
        }
        let permit = admitted(&mut frontier, "a.example", Duration::ZERO)?;
        assert!(
            matches!(
                frontier.admit("b.example", Duration::ZERO),
                Admission::Admit(_)
            ),
            "two hosts on two origins are bounded by the egress, not by each other"
        );

        frontier.observe_resolution("a.example", Resolved::Origin("new".to_owned()));
        frontier.complete(permit, Outcome::Success)?;

        // Physical ownership invariants, not implementation counters.
        assert_eq!(
            frontier.in_flight(&ConstraintKey::origin("old")),
            0,
            "A's request was admitted against the old origin, so completing it \
             must return that origin's slot — not one derived from the name's \
             new topology"
        );
        assert_eq!(
            frontier.in_flight(&ConstraintKey::origin("new")),
            1,
            "B is still physically running against the new origin and still owns \
             its only slot"
        );
        assert!(
            matches!(
                frontier.admit("c.example", Duration::ZERO),
                Admission::Defer { .. }
            ),
            "the default origin window of one must never admit C while B owns it"
        );
        assert_eq!(
            frontier.outstanding(),
            1,
            "one request is still in flight, so the ledger holds exactly one \
             reservation"
        );
        Ok(())
    }

    #[test]
    fn a_resolution_failure_during_flight_returns_the_original_slots() -> TestResult {
        // A transient SERVFAIL while A is running removes the host from the
        // topology map. The permit still names the origin the request was
        // admitted against, so the release lands where the slots were taken.
        let mut frontier = frontier();
        ready(&mut frontier, "a.example", "old");
        let permit = admitted(&mut frontier, "a.example", Duration::ZERO)?;
        assert_eq!(frontier.in_flight(&ConstraintKey::origin("old")), 1);

        frontier.observe_resolution("a.example", Resolved::Failed);
        frontier.complete(permit, Outcome::Failure)?;

        assert_eq!(
            frontier.in_flight(&ConstraintKey::origin("old")),
            0,
            "a resolution failure is an observation about the name, not evidence \
             that the request stopped holding its origin's slot"
        );
        assert_eq!(
            frontier.in_flight(&ConstraintKey::egress("eth0")),
            0,
            "and the egress slot comes back with it"
        );
        assert_eq!(frontier.outstanding(), 0, "the ledger is empty again");
        Ok(())
    }

    #[test]
    fn several_in_flight_across_topology_generations_conserve_every_slot() -> TestResult {
        // Three requests admitted, the first host's name re-resolved twice while
        // all three are still running, and the completions issued out of order.
        // Every slot taken is returned exactly once, against the origin it was
        // taken from.
        let mut frontier = frontier();
        for (host, origin) in [
            ("a.example", "origin-a"),
            ("b.example", "origin-b"),
            ("c.example", "origin-c"),
        ] {
            ready(&mut frontier, host, origin);
        }
        let first = admitted(&mut frontier, "a.example", Duration::ZERO)?;
        let second = admitted(&mut frontier, "b.example", Duration::ZERO)?;
        let third = admitted(&mut frontier, "c.example", Duration::ZERO)?;
        assert_eq!(
            frontier.in_flight(&ConstraintKey::egress("eth0")),
            3,
            "three requests are physically running"
        );

        // a.example moves twice: an edge cache, a failover, a second failover.
        frontier.observe_resolution("a.example", Resolved::Origin("gen2".to_owned()));
        frontier.observe_resolution("a.example", Resolved::Origin("gen3".to_owned()));
        assert_eq!(
            frontier.in_flight(&ConstraintKey::origin("origin-a")),
            1,
            "the request holds the origin it was admitted against however many \
             times its name has moved since"
        );

        // Out of order, and against a topology the names have left behind.
        frontier.complete(second, Outcome::Success)?;
        frontier.complete(third, Outcome::Failure)?;
        frontier.complete(first, Outcome::Success)?;

        for origin in ["origin-a", "origin-b", "origin-c", "gen2", "gen3"] {
            assert_eq!(
                frontier.in_flight(&ConstraintKey::origin(origin)),
                0,
                "every origin's occupancy returns to zero once its requests have \
                 completed"
            );
        }
        assert_eq!(frontier.in_flight(&ConstraintKey::egress("eth0")), 0);
        assert_eq!(frontier.outstanding(), 0, "and no reservation is left over");

        // The other half of the same rule: a *new* request for the same name
        // keys on where that name points now, so re-resolution still moves the
        // origin the run shares — it just cannot move one that is in flight.
        let moved = admitted(&mut frontier, "a.example", Duration::ZERO)?;
        assert!(
            moved.holds(&ConstraintKey::origin("gen3")),
            "a new admission keys on the current topology"
        );
        assert!(
            !moved.holds(&ConstraintKey::origin("origin-a")),
            "and not on the one an earlier request for the same name used"
        );
        frontier.complete(moved, Outcome::Success)?;
        Ok(())
    }

    #[test]
    fn a_permit_holds_its_admission_time_keys() -> TestResult {
        let mut frontier = frontier();
        ready(&mut frontier, "a.example", "old");
        let permit = admitted(&mut frontier, "a.example", Duration::ZERO)?;

        assert!(
            permit.holds(&ConstraintKey::host("a.example")),
            "the permit holds the host it was admitted for"
        );
        assert!(
            permit.holds(&ConstraintKey::origin("old")),
            "and the origin the name resolved to at that instant"
        );
        assert!(
            permit.holds(&ConstraintKey::egress("eth0")),
            "and the egress every request in the run shares"
        );
        assert_eq!(permit.host(), "a.example");

        frontier.observe_resolution("a.example", Resolved::Origin("new".to_owned()));
        assert!(
            !permit.holds(&ConstraintKey::origin("new")),
            "a resolution observed while the request was in flight does not \
             change what the reservation holds"
        );
        Ok(())
    }

    #[test]
    fn response_attribution_follows_the_admission_not_the_current_name() -> TestResult {
        // The response's failure signal is evidence about the infrastructure the
        // request actually reached. After a remap it must still shrink that
        // origin's window, and must not touch the origin the name now names.
        let mut frontier = frontier();
        ready(&mut frontier, "a.example", "old");
        ready(&mut frontier, "b.example", "new");
        let permit = admitted(&mut frontier, "a.example", Duration::ZERO)?;
        frontier.observe_resolution("a.example", Resolved::Origin("new".to_owned()));

        for _ in 0..PolitenessPolicy::DEFAULT.increase_after() {
            completed(&mut frontier, "b.example", Outcome::Success)?;
        }
        let new_before = frontier.window(&ConstraintKey::origin("new"));
        let old_before = frontier.window(&ConstraintKey::origin("old"));

        frontier.complete(permit, Outcome::Failure)?;

        assert_eq!(
            frontier.window(&ConstraintKey::origin("old")),
            PolitenessPolicy::DEFAULT.decrease(old_before),
            "the failure shrank the origin the request was admitted against"
        );
        assert_eq!(
            frontier.window(&ConstraintKey::origin("new")),
            new_before,
            "and left the origin the name moved to untouched"
        );
        Ok(())
    }

    #[test]
    fn a_permit_from_another_frontier_is_refused() -> TestResult {
        let mut issuer = frontier();
        let mut stranger = frontier();
        ready(&mut issuer, "a.example", "origin-a");
        let permit = admitted(&mut issuer, "a.example", Duration::ZERO)?;

        assert_eq!(
            stranger.complete(permit, Outcome::Success),
            Err(CompletionError::ForeignPermit),
            "a permit is evidence only against the accounting that minted it, \
             and a second frontier holds a different ledger"
        );
        assert_eq!(
            issuer.outstanding(),
            1,
            "the refusal changed nothing: the issuer still holds the slot the \
             request occupies"
        );
        assert_eq!(
            issuer.in_flight(&ConstraintKey::origin("origin-a")),
            1,
            "and a foreign completion must not decrement it"
        );
        Ok(())
    }

    #[test]
    fn a_permit_with_no_live_reservation_is_refused() -> TestResult {
        // The same ledger, after the reservation it named has been retired by
        // the completion that consumed it. Nothing in the current public API can
        // hand back a spent permit — `complete` consumes it — so this drives the
        // ledger directly, which is the point: the check is enforced rather than
        // assumed, and the arm a future path would hit is the one a saturating
        // subtraction would have swallowed.
        let mut frontier = frontier();
        ready(&mut frontier, "a.example", "origin-a");
        let permit = admitted(&mut frontier, "a.example", Duration::ZERO)?;

        let ordinal = permit.ordinal;
        frontier.reservations.remove(&ordinal);
        assert_eq!(
            frontier.verify(&permit),
            Err(CompletionError::UnknownPermit),
            "a permit this ledger does not hold is a typed refusal, not a \
             subtraction that quietly does nothing"
        );
        assert_eq!(
            frontier.complete(permit, Outcome::Success),
            Err(CompletionError::UnknownPermit),
            "and completing it is refused before any window is touched"
        );
        Ok(())
    }

    #[test]
    fn selection_skips_hosts_the_gate_will_not_admit() {
        // The filed counterexample. Rules are per-host state that selection
        // never consulted, and because selection takes the first key in a
        // `BTreeMap`, a permanently disallowed lexicographically-first host was
        // returned on every turn and starved the ready host behind it.
        let mut frontier = frontier();
        for host in ["a.example", "b.example"] {
            frontier.observe_resolution(host, Resolved::Origin(host.to_owned()));
        }
        frontier.observe_rules(
            &ConstraintKey::host("a.example"),
            RulesState::Disallowed {
                rule: "Disallow: /".to_owned(),
            },
        );
        frontier.observe_rules(&ConstraintKey::host("b.example"), RulesState::Allowed);

        assert!(
            matches!(
                frontier.admit("a.example", Duration::ZERO),
                Admission::Reject { .. }
            ),
            "the gate refuses the disallowed host"
        );
        assert_eq!(
            frontier.next_admissible(Duration::ZERO).as_deref(),
            Some("b.example"),
            "so selection must not return it: rejection does not remove the \
             host, and a selector with its own predicate returns it forever"
        );
    }

    #[test]
    fn selection_agrees_with_admission_on_every_rules_state() -> TestResult {
        // One mixed backlog, ordered so the first host of each kind is the one a
        // divergent selector would have returned.
        let mut frontier = frontier();
        let hosts = ["a.example", "b.example", "c.example", "d.example"];
        for host in hosts {
            frontier.observe_resolution(host, Resolved::Origin(format!("origin-{host}")));
        }
        frontier.observe_rules(
            &ConstraintKey::host("a.example"),
            RulesState::Disallowed {
                rule: "Disallow: /".to_owned(),
            },
        );
        frontier.observe_rules(&ConstraintKey::host("b.example"), RulesState::NotFetched);
        frontier.observe_rules(
            &ConstraintKey::host("c.example"),
            RulesState::Unreachable {
                since: Duration::ZERO,
            },
        );
        frontier.observe_rules(&ConstraintKey::host("d.example"), RulesState::Allowed);

        let next = frontier.next_admissible(Duration::ZERO);
        assert_eq!(
            next.as_deref(),
            Some("d.example"),
            "disallowed, unfetched and unexpired-unreachable are all states the \
             gate will not admit, so none of them is selectable"
        );

        // The property the acceptance criteria state: for every host selection
        // returns, the gate admits it on unchanged state.
        let selected = next.unwrap_or_default();
        let permit = match frontier.admit(&selected, Duration::ZERO) {
            Admission::Admit(permit) => permit,
            other => {
                return Err(format!(
                    "selection returned {selected}, which the gate answered with {other:?}"
                )
                .into());
            }
        };
        assert!(permit.holds(&ConstraintKey::host("d.example")));
        Ok(())
    }

    #[test]
    fn a_lone_unpermitted_host_is_not_selected() {
        let mut frontier = frontier();
        frontier.observe_resolution("only.example", Resolved::Origin("origin".to_owned()));
        frontier.observe_rules(&ConstraintKey::host("only.example"), RulesState::NotFetched);

        assert_eq!(
            frontier.next_admissible(Duration::ZERO),
            None,
            "a host whose rules have not been fetched is a prerequisite, not an \
             admission, and returning it as admissible deadlocks the scheduler \
             that trusts the answer"
        );
        assert_eq!(
            frontier.next_prerequisite(Duration::ZERO),
            Some(("only.example".to_owned(), DeferralKind::RulesNotFetched)),
            "the same predicate reports the work that would unblock it"
        );
    }

    #[test]
    fn selection_agrees_with_admission_across_cooldowns_and_saturation() -> TestResult {
        let mut frontier = frontier();
        ready(&mut frontier, "a.example", "origin-a");
        ready(&mut frontier, "b.example", "origin-b");
        ready(&mut frontier, "c.example", "origin-c");

        // a is inside a Retry-After the target asked for.
        let permit = admitted(&mut frontier, "a.example", Duration::ZERO)?;
        frontier.observe_retry_after(&permit, Duration::ZERO, Duration::from_secs(30))?;

        // b is saturated: admitted, not completed.
        let held = admitted(&mut frontier, "b.example", Duration::ZERO)?;

        assert_eq!(
            frontier.next_admissible(Duration::from_secs(1)).as_deref(),
            Some("c.example"),
            "a host on cooldown and a host at its window are both held, so \
             selection moves past them to one the gate will take"
        );
        assert!(
            matches!(
                frontier.admit("c.example", Duration::from_secs(1)),
                Admission::Admit(_)
            ),
            "and the host selection returned is one the gate admits"
        );

        // c takes the last egress slot; nothing is admissible any more.
        frontier.complete(permit, Outcome::Success)?;
        assert_eq!(
            frontier.next_admissible(Duration::from_secs(1)),
            None,
            "with the egress saturated, nothing is admissible"
        );
        assert_eq!(
            frontier.next_prerequisite(Duration::from_secs(1)),
            None,
            "and nothing is waiting on a prerequisite either: these are waits, \
             not work the run can do"
        );

        frontier.complete(held, Outcome::Success)?;
        assert_eq!(
            frontier.next_admissible(Duration::from_secs(31)).as_deref(),
            Some("a.example"),
            "once the egress frees and the cooldown expires, the backlog is \
             selectable again — no caller-maintained shadow filtering"
        );
        Ok(())
    }

    #[test]
    fn a_grace_expiry_is_visible_to_selection_before_anything_acts_on_it() {
        // The rules expiry is time-driven, and a selector that maintained its
        // own approximation would flip at a different instant than the gate.
        let mut frontier = frontier();
        frontier.observe_resolution("a.example", Resolved::Origin("origin-a".to_owned()));
        frontier.observe_rules(
            &ConstraintKey::host("a.example"),
            RulesState::Unreachable {
                since: Duration::ZERO,
            },
        );

        let inside_grace = PolitenessPolicy::DEFAULT
            .unreachable_grace()
            .saturating_sub(Duration::from_secs(1));
        assert_eq!(
            frontier.next_admissible(inside_grace),
            None,
            "inside the grace the complete disallow still holds"
        );
        assert_eq!(
            frontier
                .next_admissible(PolitenessPolicy::DEFAULT.unreachable_grace())
                .as_deref(),
            Some("a.example"),
            "and at the boundary it lifts for selection, exactly where the gate \
             would stop deferring"
        );
        assert_eq!(
            frontier.rules(&ConstraintKey::host("a.example")),
            RulesState::Unreachable {
                since: Duration::ZERO
            },
            "selection is side-effect free: it reports the expiry without \
             writing it down"
        );
        assert!(
            matches!(
                frontier.admit("a.example", PolitenessPolicy::DEFAULT.unreachable_grace()),
                Admission::Admit(_)
            ),
            "and the two agree once the grant is acted on"
        );
    }

    #[test]
    fn selection_makes_progress_through_a_disallowed_prefix() -> TestResult {
        // A selection/admission cycle over a backlog whose first keys can never
        // be admitted. Each cycle must dispatch the next real host and never
        // hand back one the gate will refuse.
        let mut frontier = frontier();
        for host in ["a.example", "b.example", "c.example", "d.example"] {
            frontier.observe_resolution(host, Resolved::Origin(format!("origin-{host}")));
            frontier.observe_rules(
                &ConstraintKey::host(host),
                if host == "a.example" {
                    RulesState::Disallowed {
                        rule: "Disallow: /".to_owned(),
                    }
                } else {
                    RulesState::Allowed
                },
            );
        }

        let mut dispatched = Vec::new();
        let mut held = Vec::new();
        for _ in 0..3 {
            let Some(host) = frontier.next_admissible(Duration::ZERO) else {
                break;
            };
            match frontier.admit(&host, Duration::ZERO) {
                // Held, not completed: each dispatch keeps its origin's single
                // slot, so the next cycle has to move on to another host rather
                // than returning the same one.
                Admission::Admit(permit) => {
                    dispatched.push(host);
                    held.push(permit);
                }
                other => {
                    return Err(format!(
                        "selection returned {host}, which the gate answered with {other:?}"
                    )
                    .into());
                }
            }
        }

        assert_eq!(
            dispatched,
            vec!["b.example", "c.example", "d.example"],
            "three cycles dispatch three distinct ready hosts behind a host that \
             can never be dispatched"
        );
        assert_eq!(held.len(), 3, "each dispatch is still in flight");
        Ok(())
    }

    #[test]
    fn ten_thousand_requests_replay_identically() {
        /// A verdict without the permit it may carry.
        ///
        /// A permit's identity is per-frontier — that is what makes a foreign
        /// one refusable — so two runs are compared on the verdicts they
        /// produced, one per step, and permits are settled inside the run rather
        /// than recorded.
        #[derive(Debug, PartialEq, Eq)]
        enum Shape {
            /// The frontier took the reservation.
            Admit,
            /// Held, with the instant, the cause and the binding constraint.
            Defer(Duration, DeferralKind, ConstraintKey),
            /// Dropped for good.
            Reject(RejectKind),
        }

        /// What one run produced: the verdict per step, sampled windows, and the
        /// count of completions the ledger refused — zero, or the run is not
        /// evidence about anything.
        struct Replay {
            /// The verdict of each of the ten thousand steps.
            verdicts: Vec<Shape>,
            /// The egress window, sampled every five hundred steps.
            windows: Vec<u32>,
            /// Completions the frontier refused.
            refusals: usize,
        }

        fn run(mut frontier: Frontier) -> Replay {
            use std::collections::VecDeque;

            let hosts: Vec<String> = (0..20_u32)
                .map(|index| format!("h{index}.example"))
                .collect();
            let mut replay = Replay {
                verdicts: Vec::new(),
                windows: Vec::new(),
                refusals: 0,
            };
            // Up to three requests in flight at once, oldest retired first. A
            // run that completed every request before asking for the next one
            // would never saturate anything, and a replay of two runs that never
            // back off is two identical silences.
            let mut parked: VecDeque<InFlightPermit> = VecDeque::new();
            let mut at = Duration::ZERO;
            let mut index = 0_usize;

            for step in 0..10_000_usize {
                let host = &hosts[index];
                let outcome = if step.checked_rem(7) == Some(0) {
                    Outcome::Failure
                } else if step.checked_rem(3) == Some(0) {
                    Outcome::CacheHit
                } else {
                    Outcome::Success
                };
                if parked.len() >= 3 {
                    let oldest = parked.pop_front();
                    let refused =
                        oldest.is_some_and(|permit| frontier.complete(permit, outcome).is_err());
                    if refused {
                        replay.refusals = replay.refusals.saturating_add(1);
                    }
                }
                // A refusal has no reservation to settle, so only an admission
                // is parked — which is the exchange the API enforces.
                let shape = match frontier.admit(host, at) {
                    Admission::Admit(permit) => {
                        parked.push_back(permit);
                        Shape::Admit
                    }
                    Admission::Defer {
                        resume_at,
                        kind,
                        constraint,
                    } => Shape::Defer(resume_at, kind, constraint),
                    Admission::Reject { kind } => Shape::Reject(kind),
                };
                replay.verdicts.push(shape);
                at = at.saturating_add(Duration::from_millis(5));
                if step.checked_rem(500) == Some(0) {
                    replay
                        .windows
                        .push(frontier.window(&ConstraintKey::egress("eth0")));
                }
                index = if index.saturating_add(1) >= hosts.len() {
                    0
                } else {
                    index.saturating_add(1)
                };
            }
            replay
        }

        fn build() -> Frontier {
            let mut frontier = frontier();
            for index in 0..20_u32 {
                let host = format!("h{index}.example");
                // Three hosts per origin: the shared-infrastructure case at size.
                let origin = format!("origin-{}", index.checked_rem(3).unwrap_or(0));
                ready(&mut frontier, &host, &origin);
            }
            frontier
        }

        let first = run(build());
        let second = run(build());

        assert_eq!(first.verdicts.len(), 10_000);
        assert_eq!(
            first.refusals, 0,
            "every admission in the run was released by the ledger that minted \
             it, so the refusal counter is the run's own consistency check"
        );
        assert!(
            first
                .verdicts
                .iter()
                .any(|shape| matches!(*shape, Shape::Admit))
                && first
                    .verdicts
                    .iter()
                    .any(|shape| matches!(*shape, Shape::Defer(..))),
            "the schedule exercised both dispatch and back-off, so the replay \
             comparison is not two identical silences"
        );
        assert_eq!(
            first.verdicts, second.verdicts,
            "the same schedule on the same policy must produce identical verdicts"
        );
        assert_eq!(first.windows, second.windows);
    }

    #[test]
    fn permits_from_identically_driven_frontiers_compare_equal() -> TestResult {
        // The determinism the virtual clock exists for, stated on the permit
        // itself: two frontiers driven through the same script mint the same
        // reservations in the same order, so their verdicts are comparable.
        let mut first = frontier();
        let mut second = frontier();
        ready(&mut first, "a.example", "origin-a");
        ready(&mut second, "a.example", "origin-a");

        assert_eq!(
            admitted(&mut first, "a.example", Duration::ZERO)?,
            admitted(&mut second, "a.example", Duration::ZERO)?,
            "identical schedules produce identical reservations"
        );
        Ok(())
    }

    #[test]
    fn next_admissible_is_stable_and_skips_the_abandoned() {
        let mut frontier = frontier();
        ready(&mut frontier, "b.example", "origin-b");
        ready(&mut frontier, "a.example", "origin-a");
        ready(&mut frontier, "c.example", "origin-c");

        assert_eq!(
            frontier.next_admissible(Duration::ZERO).as_deref(),
            Some("a.example"),
            "selection is by key order, not by insertion order"
        );

        frontier.abandon("a.example");
        assert_eq!(
            frontier.next_admissible(Duration::ZERO).as_deref(),
            Some("b.example")
        );
        assert!(frontier.is_abandoned("a.example"));
    }

    #[test]
    fn releasing_returns_every_shared_slot() -> TestResult {
        let mut frontier = frontier();
        ready(&mut frontier, "a.example", "origin-a");
        ready(&mut frontier, "b.example", "origin-a");

        let permit = admitted(&mut frontier, "a.example", Duration::ZERO)?;
        // b is behind the same origin, so the shared window is what blocks it.
        assert_eq!(
            deferral(&frontier.admit("b.example", Duration::ZERO)).map(|parts| parts.1),
            Some(DeferralKind::RateLimited)
        );

        frontier.complete(permit, Outcome::Success)?;
        assert_eq!(frontier.in_flight(&ConstraintKey::origin("origin-a")), 0);
        assert_eq!(frontier.in_flight(&ConstraintKey::egress("eth0")), 0);
        assert_eq!(frontier.outstanding(), 0);
        Ok(())
    }
}
