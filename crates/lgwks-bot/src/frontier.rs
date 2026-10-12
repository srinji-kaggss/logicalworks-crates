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
//! `Frontier::decide`, which takes `&self` and returns a `Verdict` with no
//! side effect at all. Selection returns a host only when that function says
//! `Verdict::Admit`; reservation reserves only when it says the same, and
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
//! place — `RulesState::effective` — and *recorded* in exactly one place —
//! `Frontier::promote_rules`, when a reservation acts on the verdict that
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
//! [`Frontier`]: crate::frontier::Frontier
//! [`Frontier::admit`]: crate::frontier::Frontier::admit
//! [`Frontier::complete`]: crate::frontier::Frontier::complete
//! [`Frontier::next_admissible`]: crate::frontier::Frontier::next_admissible
//! [`Frontier::observe_retry_after`]: crate::frontier::Frontier::observe_retry_after
//! [`InFlightPermit`]: crate::frontier::InFlightPermit

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

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
/// be a second one that no permit can settle. See `Issuer`.
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

    /// The politeness limits this frontier enforces.
    ///
    /// Fixed at construction -- [`Frontier::new`] is the only way one is chosen
    /// and there is no setter -- so this answers "what was this run admitted
    /// under", not "what could be raised to". A caller that wants a different
    /// budget builds a different frontier: a reservation ledger is one
    /// accounting, and changing its limits mid-flight would change what the
    /// reservations it already issued mean.
    #[must_use]
    pub fn policy(&self) -> &PolitenessPolicy {
        &self.policy
    }

    /// How many requests are in flight against `key`.
    #[must_use]
    pub fn in_flight(&self, key: &ConstraintKey) -> u32 {
        // A key never admitted against has nothing in flight.
        match self.state.get(key) {
            Some(entry) => entry.in_flight,
            None => 0,
        }
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
        // A key never used starts at the floor, as documented above.
        match self.state.get(key) {
            Some(entry) => entry.window,
            None => self.policy.min_window(),
        }
    }

    /// The rules state recorded for `key`.
    ///
    /// The state as *recorded*, not as it stands at some instant: an
    /// unreachability whose grace has expired still reads as
    /// [`RulesState::Unreachable`] until something acts on it. Pass the instant
    /// to `Self::decide` to get the verdict that expiry implies.
    #[must_use]
    pub fn rules(&self, key: &ConstraintKey) -> RulesState {
        // A key with no recorded state has had no rules fetched.
        match self.state.get(key) {
            Some(entry) => entry.rules.clone(),
            None => RulesState::NotFetched,
        }
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
        match self.state.get(key) {
            Some(entry) => entry.rules.effective(at, grace),
            None => RulesState::NotFetched,
        }
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
    /// says `Verdict::Admit`, [`Self::next_admissible`] returns a host only
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

        // Two answers, both real: a constraint on this host was saturated and
        // the fold above carries the latest deferral, or none was and `Admit`
        // is what that means. The fold is never left holding nothing that was
        // meant to be reported.
        match binding {
            Some(verdict) => verdict,
            None => Verdict::Admit,
        }
    }

    /// The deferral for `key` at `at`, or `None` when it has room.
    ///
    /// Reads `&self` and creates nothing: a constraint nobody has exercised is
    /// not yet owed a record, and `Self::decide` must stay free of the side
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
    /// The decision is `Self::decide`'s, not a second evaluation of the same
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
    /// returned `Verdict::Admit` — there is no second predicate here to
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
            let refusal = Err(CompletionError::ForeignPermit);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "verify: returning an error to the caller");
            return refusal;
        }
        if !self.reservations.contains(&permit.ordinal) {
            let refusal = Err(CompletionError::UnknownPermit);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "verify: returning an error to the caller");
            return refusal;
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
    /// `Self::decide` verdict as selection and admission; it is a view of one
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
mod tests;

// The vocabulary and the policy, each in its own file; re-exported so every
// path stays `frontier::<Name>`.
mod policy;
mod vocab;

pub use policy::PolitenessPolicy;
pub use vocab::{
    Admission, CompletionError, ConstraintKey, DeferralKind, Outcome, PolitenessError, RejectKind,
    Resolved, RulesState,
};
