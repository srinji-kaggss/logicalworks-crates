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
//! | [`Admission::Admit`] | dispatch now |
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
}

/// What the scheduler should do with a request.
///
/// Three arms, each a distinct physical action, and no fourth. See the module
/// documentation for why the reasons are typed payloads rather than arms.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Admission {
    /// Dispatch now.
    Admit,
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
fn later(current: Option<Admission>, candidate: Admission) -> Admission {
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

/// The sort key for a deferral. `None` for an admission that is not a deferral,
/// which never reaches [`later`]'s comparison in practice but is modelled
/// rather than papered over with a sentinel key.
fn deferral_order(admission: &Admission) -> Option<(Duration, DeferralKind, &ConstraintKey)> {
    match *admission {
        Admission::Defer {
            resume_at,
            kind,
            ref constraint,
        } => Some((resume_at, kind, constraint)),
        Admission::Admit | Admission::Reject { .. } => None,
    }
}

/// The backlog of hosts a run intends to contact, and the politeness state that
/// governs them.
///
/// Holds no clock. Every method that needs the time is given it, which is what
/// lets a test drive ten thousand requests through a virtual schedule without
/// touching wall-clock and get an identical result on every run.
#[derive(Debug, Clone)]
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

    /// The current window for `key`, or the policy minimum if never used.
    #[must_use]
    pub fn window(&self, key: &ConstraintKey) -> u32 {
        self.state
            .get(key)
            .map_or(self.policy.min_window(), |entry| entry.window)
    }

    /// The rules state recorded for `key`.
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
    /// Applied to the host *and* to every constraint it shares, because a server
    /// asking for a pause is asking a client to pause, not one name that happens
    /// to point at it.
    pub fn observe_retry_after(&mut self, host: &str, at: Duration, delay: Duration) {
        let until = at.saturating_add(delay);
        for key in self.server_keys_for(host) {
            let entry = self.entry(&key);
            if entry.blocked_until < until {
                entry.blocked_until = until;
            }
        }
    }

    /// Every constraint a request to `host` is subject to.
    fn keys_for(&self, host: &str) -> Vec<ConstraintKey> {
        let mut keys = vec![ConstraintKey::host(host), self.egress.clone()];
        if let Some(origin) = self.constraints.get(host) {
            keys.push(origin.clone());
        }
        keys
    }

    /// The constraints an observation *about a server* is evidence for.
    ///
    /// The server-side keys only: the host, and the origin it shares. The egress
    /// is excluded deliberately. It is the operator's own interface, and one
    /// server asking for a pause — or rate-limiting one tenant behind a shared
    /// proxy — is not evidence about the capacity of our network. Binding the
    /// egress here would let a single target halt every unrelated host in the
    /// run, which is the failure this distinction exists to prevent.
    fn server_keys_for(&self, host: &str) -> Vec<ConstraintKey> {
        let mut keys = vec![ConstraintKey::host(host)];
        if let Some(origin) = self.constraints.get(host) {
            keys.push(origin.clone());
        }
        keys
    }

    /// Decides what to do with a request to `host` at `at`.
    ///
    /// Admits only when every constraint admits, and reports the one that bound
    /// when it does not. On [`Admission::Admit`] the caller owes a matching
    /// [`Self::release`] once the request leaves flight.
    pub fn admit(&mut self, host: &str, at: Duration) -> Admission {
        if self.abandons.contains(host) {
            return Admission::Reject {
                kind: RejectKind::DoesNotExist,
            };
        }

        match self.resolutions.get(host) {
            None => {
                return Admission::Defer {
                    resume_at: at,
                    kind: DeferralKind::OriginUnmapped,
                    constraint: ConstraintKey::host(host),
                };
            }
            Some(&Resolved::Failed) => {
                return Admission::Defer {
                    resume_at: at.saturating_add(self.policy.retry_after_failure()),
                    kind: DeferralKind::ResolutionFailed,
                    constraint: ConstraintKey::host(host),
                };
            }
            Some(&Resolved::DoesNotExist) => {
                return Admission::Reject {
                    kind: RejectKind::DoesNotExist,
                };
            }
            Some(&Resolved::Origin(_)) => {}
        }

        // A site's published rules are per-host, so only the host key is
        // consulted; the origin keys carry a window, not a ruleset.
        let host_key = ConstraintKey::host(host);
        if let Err(admission) = self.rules_for(&host_key, at) {
            return admission;
        }

        let mut binding: Option<Admission> = None;
        for key in self.keys_for(host) {
            let entry = self.entry(&key);
            if entry.blocked_until > at {
                let wait = Admission::Defer {
                    resume_at: entry.blocked_until,
                    kind: DeferralKind::RateLimited,
                    constraint: key,
                };
                binding = Some(later(binding, wait));
            } else if entry.in_flight >= entry.window {
                let wait = Admission::Defer {
                    resume_at: at,
                    kind: DeferralKind::RateLimited,
                    constraint: key,
                };
                binding = Some(later(binding, wait));
            }
        }

        match binding {
            Some(deferral) => deferral,
            None => {
                for key in self.keys_for(host) {
                    let entry = self.entry(&key);
                    entry.in_flight = entry.in_flight.saturating_add(1);
                }
                Admission::Admit
            }
        }
    }

    /// Evaluates the rules for one key, returning `Err` with the verdict when
    /// they decide the request.
    fn rules_for(&mut self, key: &ConstraintKey, at: Duration) -> Result<(), Admission> {
        let grace = self.policy.unreachable_grace();
        let retry = self.policy.retry_after_failure();
        let entry = self.entry(key);
        match entry.rules.clone() {
            RulesState::Allowed | RulesState::NoRules => Ok(()),
            RulesState::Disallowed { rule } => Err(Admission::Reject {
                kind: RejectKind::Disallowed { rule },
            }),
            RulesState::NotFetched => Err(Admission::Defer {
                resume_at: at,
                kind: DeferralKind::RulesNotFetched,
                constraint: key.clone(),
            }),
            RulesState::Unreachable { since } => {
                // RFC 9309 §2.3.1.4's complete disallow, with that section's own
                // expiry: after a reasonably long period the crawler MAY treat
                // the file as unavailable, which is §2.3.1.3's allow.
                if at.saturating_sub(since) >= grace {
                    entry.rules = RulesState::NoRules;
                    Ok(())
                } else {
                    Err(Admission::Defer {
                        resume_at: at.saturating_add(retry),
                        kind: DeferralKind::OriginPolicyUnreachable,
                        constraint: key.clone(),
                    })
                }
            }
        }
    }

    /// Returns one in-flight slot against every constraint `host` belongs to.
    pub fn release(&mut self, host: &str) {
        for key in self.keys_for(host) {
            let entry = self.entry(&key);
            entry.in_flight = entry.in_flight.saturating_sub(1);
        }
    }

    /// Records that a request to `host` completed without an explicit failure
    /// signal, and grows the window once enough have in a row.
    ///
    /// `cache_hit` excludes the observation from the growth signal. A response
    /// served by an edge cache says nothing about the origin's capacity, and
    /// counting it is how a latency-driven controller ends up speeding up while
    /// the origin is saturated.
    pub fn record_success(&mut self, host: &str, cache_hit: bool) {
        if cache_hit {
            return;
        }
        // Copied out before the loop: `PolitenessPolicy` is `Copy`, and reading
        // it through `self` while `entry` holds a mutable borrow of `self` is
        // the borrow conflict this avoids.
        let policy = self.policy;
        for key in self.server_keys_for(host) {
            let entry = self.entry(&key);
            entry.successes = entry.successes.saturating_add(1);
            if entry.successes >= policy.increase_after() {
                entry.successes = 0;
                entry.window = policy.increase(entry.window);
            }
        }
    }

    /// Records an explicit failure signal, shrinking every window `host`
    /// belongs to.
    ///
    /// Shrinking the *shared* keys is the whole point: the failure is evidence
    /// about the infrastructure, so the next host behind the same origin
    /// inherits the caution.
    pub fn record_failure(&mut self, host: &str) {
        let policy = self.policy;
        for key in self.server_keys_for(host) {
            let entry = self.entry(&key);
            entry.successes = 0;
            entry.window = policy.decrease(entry.window);
        }
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
    #[must_use]
    pub fn next_admissible(&self, at: Duration) -> Option<String> {
        self.resolutions.iter().find_map(|(host, resolved)| {
            let ready = !self.abandons.contains(host)
                && matches!(*resolved, Resolved::Origin(_))
                && self.keys_for(host).iter().all(|key| {
                    self.state.get(key).is_none_or(|entry| {
                        entry.blocked_until <= at && entry.in_flight < entry.window
                    })
                });
            if ready { Some(host.clone()) } else { None }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: Duration = Duration::from_secs(3_600);
    const THIRTY_ONE_DAYS: Duration = Duration::from_secs(2_678_400);

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
            Admission::Admit | Admission::Reject { .. } => None,
        }
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
    fn rules_states_follow_rfc_9309() {
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
        assert_eq!(frontier.admit("a.example", HOUR), Admission::Admit);
        frontier.release("a.example");

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
        assert_eq!(
            frontier.admit("a.example", THIRTY_ONE_DAYS),
            Admission::Admit,
            "RFC 9309 2.3.1.4 lets a long-unreachable file be treated as unavailable"
        );
        assert_eq!(frontier.rules(&key), RulesState::NoRules);
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
            if frontier.admit(&format!("shop{index}.example"), Duration::ZERO) == Admission::Admit {
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
            if frontier.admit(&format!("h{index}.example"), Duration::ZERO) == Admission::Admit {
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

        assert_eq!(
            frontier.admit("a.example", Duration::ZERO),
            Admission::Admit
        );
        assert_eq!(
            deferral(&frontier.admit("a.example", Duration::ZERO)).map(|parts| parts.1),
            Some(DeferralKind::RateLimited),
            "a window of one admits exactly once"
        );
    }

    #[test]
    fn retry_after_binds_the_origin_not_just_the_host() {
        let mut frontier = frontier();
        ready(&mut frontier, "a.example", "origin-a");
        ready(&mut frontier, "b.example", "origin-a");

        frontier.observe_retry_after("a.example", Duration::ZERO, Duration::from_secs(30));

        assert_eq!(
            deferral(&frontier.admit("b.example", Duration::from_secs(1))),
            Some((
                Duration::from_secs(30),
                DeferralKind::RateLimited,
                ConstraintKey::origin("origin-a"),
            )),
            "a pause asked of the client binds its sibling on the same origin"
        );
        assert_eq!(
            frontier.admit("b.example", Duration::from_secs(31)),
            Admission::Admit
        );
    }

    #[test]
    fn a_retry_after_does_not_bind_the_egress() {
        let mut frontier = frontier();
        ready(&mut frontier, "a.example", "origin-a");
        ready(&mut frontier, "unrelated.example", "origin-z");

        frontier.observe_retry_after("a.example", Duration::ZERO, Duration::from_secs(30));

        assert_eq!(
            frontier.admit("unrelated.example", Duration::from_secs(1)),
            Admission::Admit,
            "one server asking for a pause is not evidence about our network's \
             capacity; binding the egress would let a single target halt every \
             unrelated host in the run"
        );
        assert_eq!(
            deferral(&frontier.admit("a.example", Duration::from_secs(1))).map(|parts| parts.2),
            Some(ConstraintKey::origin("origin-a")),
            "but it does bind the server-side keys the target actually shares"
        );
    }

    #[test]
    fn the_window_shrinks_on_failure_and_grows_on_success() {
        let mut frontier = frontier();
        ready(&mut frontier, "a.example", "origin-a");
        let key = ConstraintKey::origin("origin-a");

        for _ in 0..PolitenessPolicy::DEFAULT.increase_after() {
            frontier.record_success("a.example", false);
        }
        assert_eq!(
            frontier.window(&key),
            2,
            "eight successes add one to a window of one"
        );

        frontier.record_failure("a.example");
        assert_eq!(frontier.window(&key), 1, "2 x 70% floors at min_window");

        for _ in 0..5 {
            frontier.record_failure("a.example");
        }
        assert_eq!(
            frontier.window(&key),
            PolitenessPolicy::DEFAULT.min_window(),
            "a window of zero would admit nothing forever"
        );
    }

    #[test]
    fn a_cache_hit_does_not_grow_the_window() {
        let mut frontier = frontier();
        ready(&mut frontier, "a.example", "origin-a");
        let key = ConstraintKey::origin("origin-a");

        for _ in 0..PolitenessPolicy::DEFAULT.increase_after() {
            frontier.record_success("a.example", true);
        }
        assert_eq!(
            frontier.window(&key),
            PolitenessPolicy::DEFAULT.min_window(),
            "an edge cache answering fast says nothing about origin capacity, and \
             counting it is how a latency controller speeds up under load"
        );
    }

    #[test]
    fn ten_thousand_requests_replay_identically() {
        fn run(mut frontier: Frontier) -> (Vec<Admission>, Vec<u32>) {
            let hosts: Vec<String> = (0..20_u32)
                .map(|index| format!("h{index}.example"))
                .collect();
            let mut verdicts = Vec::new();
            let mut windows = Vec::new();
            let mut at = Duration::ZERO;
            let mut index = 0_usize;

            for step in 0..10_000_usize {
                let host = &hosts[index];
                verdicts.push(frontier.admit(host, at));
                if step.checked_rem(7) == Some(0) {
                    frontier.record_failure(host);
                } else {
                    frontier.record_success(host, step.checked_rem(3) == Some(0));
                }
                frontier.release(host);
                at = at.saturating_add(Duration::from_millis(5));
                if step.checked_rem(500) == Some(0) {
                    windows.push(frontier.window(&ConstraintKey::egress("eth0")));
                }
                index = if index.saturating_add(1) >= hosts.len() {
                    0
                } else {
                    index.saturating_add(1)
                };
            }
            (verdicts, windows)
        }

        fn build() -> Frontier {
            let mut frontier = frontier();
            for index in 0..20_u32 {
                let host = format!("h{index}.example");
                // Five hosts per origin: the shared-infrastructure case at size.
                let origin = format!("origin-{}", index.checked_rem(5).unwrap_or(0));
                ready(&mut frontier, &host, &origin);
            }
            frontier
        }

        let (first, first_windows) = run(build());
        let (second, second_windows) = run(build());

        assert_eq!(first.len(), 10_000);
        assert_eq!(
            first, second,
            "the same schedule on the same policy must produce identical verdicts"
        );
        assert_eq!(first_windows, second_windows);
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
    fn releasing_returns_every_shared_slot() {
        let mut frontier = frontier();
        ready(&mut frontier, "a.example", "origin-a");
        ready(&mut frontier, "b.example", "origin-a");

        assert_eq!(
            frontier.admit("a.example", Duration::ZERO),
            Admission::Admit
        );
        // b is behind the same origin, so the shared window is what blocks it.
        assert_eq!(
            deferral(&frontier.admit("b.example", Duration::ZERO)).map(|parts| parts.1),
            Some(DeferralKind::RateLimited)
        );

        frontier.release("a.example");
        assert_eq!(frontier.in_flight(&ConstraintKey::origin("origin-a")), 0);
        assert_eq!(frontier.in_flight(&ConstraintKey::egress("eth0")), 0);
    }
}
