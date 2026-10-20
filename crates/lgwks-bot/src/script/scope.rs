//! Tenants, step keys and the scope every step runs in.

use std::fmt;
use std::sync::Arc;

use lgwks_std::hash::{Digest, Hasher};

use crate::effect::{InputIdentity, RunId};
use crate::rt::clock::Clock;
use crate::rt::sync::CancellationToken;
use crate::task::{InputDigest, MAX_REQUEST_KEY_BYTES};

use super::policy::Policy;
use super::trail::{Mark, Outcome, Site, Trail};
use super::{DEFAULT_TRAIL_STEPS, FlowError, MAX_DEPTH, MAX_TENANT_BYTES};

// ── Tenant ──────────────────────────────────────────────────────────────────

/// Why an identifier — a tenant, a task name or a request key — is illegal.
///
/// The three share one validation rule (non-empty, bounded, ASCII letters,
/// digits and `-_.:`), so they share one check; each maps a fault to its own
/// [`FlowError`] variant and reason text. A copy per type is how the three
/// drift and one accepts a name the others refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NameFault {
    /// The name was empty.
    Empty,
    /// The name was longer than the caller's ceiling.
    TooLong,
    /// The name held a byte outside the allowed set.
    BadChar,
}

/// The fault in `name`, or `None` when it is a legal identifier of at most
/// `max` bytes.
pub(crate) fn name_fault(name: &str, max: usize) -> Option<NameFault> {
    if name.is_empty() {
        return Some(NameFault::Empty);
    }
    if name.len() > max {
        return Some(NameFault::TooLong);
    }
    let allowed = |byte: u8| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte);
    if !name.bytes().all(allowed) {
        return Some(NameFault::BadChar);
    }
    None
}

/// The identity a flow runs on behalf of.
///
/// Every [`Scope`] carries one, and every [`StepKey`] hashes it, so data keyed
/// by a step is keyed by its tenant whether or not the author remembered to.
/// The name is 1 to [`MAX_TENANT_BYTES`] bytes of ASCII letters, digits, and
/// `-`, `_`, `.`, `:`, which keeps it safe to put in a path, a log line, and a
/// file name without escaping.
///
/// Ordered by name, which is what lets per-tenant capacity state live in an
/// ordered map: the round scheduler reads and writes it, and an ordered map is
/// the only collection whose iteration order two processes agree on.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tenant(Arc<str>);

impl Tenant {
    /// Validate and hold a tenant name.
    ///
    /// # Errors
    ///
    /// [`FlowError::InvalidTenant`] naming what is wrong with the name.
    pub fn new(name: &str) -> Result<Self, FlowError> {
        let reason = match name_fault(name, MAX_TENANT_BYTES) {
            None => return Ok(Self(Arc::from(name))),
            Some(NameFault::Empty) => "the tenant name is empty",
            Some(NameFault::TooLong) => "the tenant name is longer than MAX_TENANT_BYTES",
            Some(NameFault::BadChar) => {
                "the tenant name may hold only ASCII letters, digits, '-', '_', '.' and ':'"
            }
        };
        Err(FlowError::InvalidTenant { reason })
    }

    /// Hold a name this crate validated at its own definition site.
    ///
    /// Crate-private on purpose. A caller builds a tenant through
    /// [`Tenant::new`], which refuses an illegal name; the crate's own reserved
    /// names — the supervisor's implicit tenant, for one — are literals this
    /// module knows are legal, and routing them through `new` would mean either
    /// an `expect` on a constant the compiler already checked by eye, or a panic
    /// path for a value that cannot be wrong.
    pub(crate) fn assumed(name: &'static str) -> Self {
        Self(Arc::from(name))
    }

    /// The validated name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Tenant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

// ── StepKey ─────────────────────────────────────────────────────────────────

/// The stable identity of one step of one run of one flow for one tenant.
///
/// BLAKE3 over the tenant, the step path and — when a host minted a run — a
/// domain label and the run id, each length-framed so no two different triples
/// can hash the same bytes. It is the same on every attempt of a
/// [`retry`](crate::script::retry)
/// and on every restart of the same run, which is what makes it usable as an
/// idempotency key for an effect the step performs: an effect sent twice under
/// one key is one effect to any receiver that deduplicates by it. Two different
/// runs of the same flow hash different keys, so a second run's effect is never
/// mistaken for the first's; a caller whose work is one effect across runs
/// names it with [`Scope::business_key`] instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StepKey(Digest);

impl StepKey {
    /// The 32 digest bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }

    /// Lowercase hex, the form to send to a receiver as an idempotency key.
    #[must_use]
    pub fn to_hex(&self) -> String {
        self.0.to_hex()
    }
}

/// The run-less [`StepKey`] of `path` for `tenant`: one definition, so a
/// reserved record a host writes and a step a body enters cannot key the same
/// path differently. The run store keys every record by this form, which is why
/// a record's bytes do not move when [`Scope::key`] gains a run id.
pub(crate) fn step_key(tenant: &str, path: &str) -> StepKey {
    let mut hasher = Hasher::new();
    hasher
        .write_framed(tenant.as_bytes())
        .write_framed(path.as_bytes());
    StepKey(hasher.finalize())
}

/// The [`StepKey`] of `path` for one run of `tenant`: a domain label and the
/// run id join the hash, so two runs of the same flow over the same input
/// never share a key while every attempt and restart of one run does
/// (issue #646).
pub(crate) fn step_key_in_run(tenant: &str, run: RunId, path: &str) -> StepKey {
    let mut hasher = Hasher::new();
    hasher
        .write_framed(STEP_KEY_RUN_DOMAIN)
        .write_framed(tenant.as_bytes())
        .write_framed(run.id().to_hex().as_bytes())
        .write_framed(path.as_bytes());
    StepKey(hasher.finalize())
}

/// The domain-separation label mixed into the in-run [`StepKey`], so no run's
/// key can hash the bytes a run-less key or a [`Scope::business_key`] hashes,
/// whatever a tenant is named (issue #646).
const STEP_KEY_RUN_DOMAIN: &[u8] = b"lgwks.bot.step-key-run.v1";

/// The domain-separation label mixed into a [`Scope::business_key`].
const BUSINESS_KEY_DOMAIN: &[u8] = b"lgwks.bot.business-key.v1";

impl fmt::Display for StepKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

// ── Scope ───────────────────────────────────────────────────────────────────

/// Where a flow is, for whom, and whether it should stop.
///
/// Cheap to clone (one reference count). Entering a step makes a child scope
/// whose cancellation token is a child of this one: cancelling a scope stops
/// everything beneath it and nothing above it.
///
/// An `each` body and a `retry` attempt are the exception: they share the
/// stop of the step around them rather than minting their own, so cancelling
/// from inside one stops the whole fan-out or retry, the way `break` ends a
/// loop. Stopping one body alone is the same as returning from it, and not
/// minting a token per item keeps a fan-out of a million items from creating
/// a million of them.
#[derive(Clone)]
pub struct Scope {
    /// Shared so a clone is a pointer copy.
    inner: Arc<ScopeInner>,
}

/// The fields behind a [`Scope`].
struct ScopeInner {
    /// Who the flow runs for.
    tenant: Tenant,
    /// The `/`-joined steps from the root to here; empty at the root.
    path: Arc<str>,
    /// How many scopes lie between the root and here.
    depth: u16,
    /// Cancelled when this scope, or any scope above it, is cancelled.
    token: CancellationToken,
    /// The fan-out and retry decisions every step under the root shares.
    policy: Arc<Policy>,
    /// The bounded record of which steps this root's steps entered.
    trail: Arc<Trail>,
    /// The clock every deadline under this root is measured on.
    ///
    /// Inherited by every descended scope and shared by clone rather than
    /// copied, so one advance from a test reaches every step of the flow and a
    /// step cannot quietly measure its deadline against a different timeline
    /// from the one its parent was admitted under.
    clock: Clock,
    /// The run this scope's records are keyed by, when a host minted one.
    run: Option<RunId>,
    /// This step's entry in the trail, which [`Scope::settle`] completes;
    /// `None` at the root, which no step entered.
    mark: Option<Arc<Mark>>,
}

impl Scope {
    /// A root scope for `tenant` with a fresh cancellation token.
    ///
    /// Its deadlines are measured on a wall clock, which is what every scope a
    /// caller did not name a clock for must do. Use [`Scope::with_clock`] to
    /// govern them from somewhere a test can move.
    #[must_use]
    pub fn root(tenant: Tenant) -> Self {
        Self::with_token(tenant, CancellationToken::new())
    }

    /// A root scope for `tenant` whose deadlines are measured on `clock`.
    ///
    /// The declared-clock form. Every step descended from this scope measures
    /// its [`within`](crate::script::within) budget on `clock`, so advancing it
    /// past a budget produces the same [`FlowError::TimedOut`]
    /// a real overrun produces — with no real wait, which is the whole point of
    /// having one declared clock rather than three implicit ones.
    ///
    /// Clones and descendants share the clock, so an advance reaches every step
    /// of the flow at once and two flows built on two clocks cannot see each
    /// other's time.
    #[must_use]
    pub fn with_clock(tenant: Tenant, clock: Clock) -> Self {
        Self::with_token_and_clock(tenant, CancellationToken::new(), clock)
    }

    /// A root scope for `tenant` that stops when `token` is cancelled.
    ///
    /// The form to use under a [`Supervisor`](crate::rt::supervise::Supervisor):
    /// pass the token its `spawn` hands the body, and the supervisor's
    /// shutdown reaches every step of the flow.
    #[must_use]
    pub fn with_token(tenant: Tenant, token: CancellationToken) -> Self {
        Self::with_token_and_trail(tenant, token, Trail::new(DEFAULT_TRAIL_STEPS))
    }

    /// A root scope for `tenant` that stops when `token` is cancelled and
    /// measures its deadlines on `clock`.
    #[must_use]
    pub fn with_token_and_clock(tenant: Tenant, token: CancellationToken, clock: Clock) -> Self {
        Self::with_token_trail_and_clock(tenant, token, Trail::new(DEFAULT_TRAIL_STEPS), clock)
    }

    /// A root scope for `tenant` that stops when `token` is cancelled and
    /// records the steps it enters into `trail`.
    ///
    /// The form [`Host`] uses: one trail per task run, so a
    /// [`Report`] can answer which steps ran without the
    /// flow carrying a second ledger. A scope built any other way gets its own
    /// ring, which is exactly right for a flow no report is claimed about.
    ///
    /// Crate-private because the trail's type is: the host is the supported way
    /// to obtain a scoped trail (DX-10), and a caller who reaches past it
    /// would be writing the second ledger this exists to remove.
    pub(crate) fn with_token_trail_and_clock(
        tenant: Tenant,
        token: CancellationToken,
        trail: Arc<Trail>,
        clock: Clock,
    ) -> Self {
        Self::rooted(tenant, token, trail, clock, None)
    }

    /// A root scope measured on `clock` whose durable steps are keyed by `run`.
    ///
    /// The form a resumable run uses: a host that installed a store mints or is
    /// handed a run id and passes it here, so every step descended from this root
    /// records against the same run without threading it by hand. `None` is the
    /// local form, and a step under it is never durable. The clock and the run
    /// are set together because they are both the host's: a resumed run whose
    /// steps were keyed to the host but timed on a fresh clock would measure its
    /// deadlines on a timeline nothing else in the run shares.
    pub(crate) fn rooted(
        tenant: Tenant,
        token: CancellationToken,
        trail: Arc<Trail>,
        clock: Clock,
        run: Option<RunId>,
    ) -> Self {
        Self {
            inner: Arc::new(ScopeInner {
                tenant,
                path: Arc::from(""),
                depth: 0,
                token,
                policy: Arc::new(Policy::for_this_machine()),
                trail,
                clock,
                run,
                mark: None,
            }),
        }
    }

    /// [`Self::with_token_trail_and_clock`] on a wall clock.
    pub(crate) fn with_token_and_trail(
        tenant: Tenant,
        token: CancellationToken,
        trail: Arc<Trail>,
    ) -> Self {
        Self::with_token_trail_and_clock(tenant, token, trail, Clock::wall())
    }

    /// Enter the named step beneath this scope.
    ///
    /// # Errors
    ///
    /// [`FlowError::Cancelled`] if this scope is already cancelled, so no new
    /// step starts after a stop; [`FlowError::TooDeep`] past [`MAX_DEPTH`].
    pub fn enter(&self, step: &str) -> Result<Self, FlowError> {
        self.enter_from(step, None)
    }

    #[doc(hidden)]
    /// [`Scope::enter`] for a step `script!` wrote at `site`, which its trail
    /// entry carries. Called by `script!`.
    ///
    /// # Errors
    ///
    /// As [`Scope::enter`].
    pub fn enter_at(&self, step: &str, site: &'static Site) -> Result<Self, FlowError> {
        self.enter_from(step, Some(site))
    }

    /// Enter the named step, from `site` when a script line produced it.
    pub(super) fn enter_from(
        &self,
        step: &str,
        site: Option<&'static Site>,
    ) -> Result<Self, FlowError> {
        self.descend(self.join(step), Stop::Own, site)
    }

    /// Enter the named step sharing this scope's stop: a `retry`'s attempts.
    pub(super) fn enter_sharing(
        &self,
        step: &str,
        site: Option<&'static Site>,
    ) -> Result<Self, FlowError> {
        self.descend(self.join(step), Stop::Shared, site)
    }

    /// Enter item `index` of the named step: one body of an [`each`](crate::script::each).
    ///
    /// # Errors
    ///
    /// As [`Scope::enter`].
    pub fn item(&self, step: &str, index: usize) -> Result<Self, FlowError> {
        self.descend(self.join(&format!("{step}#{index}")), Stop::Own, None)
    }

    #[doc(hidden)]
    /// [`Scope::item`] for one iteration of a `for` that `script!` wrote at
    /// `site`. Called by `script!`.
    ///
    /// # Errors
    ///
    /// As [`Scope::enter`].
    pub fn item_at(
        &self,
        step: &str,
        index: usize,
        site: &'static Site,
    ) -> Result<Self, FlowError> {
        self.descend(self.join(&format!("{step}#{index}")), Stop::Own, Some(site))
    }

    /// Item `index` of the step this scope is: path `<this path>#index`, so an
    /// `each` body reads `sync/each:page#39`, not `sync/each:page/each:page#39`,
    /// sharing the step's stop and its line.
    pub(super) fn numbered(&self, index: usize) -> Result<Self, FlowError> {
        self.descend(
            Arc::from(format!("{}#{index}", self.inner.path)),
            Stop::Shared,
            self.inner.mark.as_ref().and_then(|mark| mark.site()),
        )
    }

    /// A child scope at `path` with its own stop or this one's, entered from
    /// `site` when a script line produced it.
    fn descend(
        &self,
        path: Arc<str>,
        stop: Stop,
        site: Option<&'static Site>,
    ) -> Result<Self, FlowError> {
        self.checkpoint()?;
        if self.inner.depth >= MAX_DEPTH {
            let refusal = Err(FlowError::TooDeep {
                at: Arc::clone(&self.inner.path),
                limit: MAX_DEPTH,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "descend: returning an error to the caller");
            return refusal;
        }
        // Recorded where the step is entered, not where it finishes: a run that
        // dies halfway through a fan-out has still entered those items, and a
        // trail that only listed completed steps would under-report exactly the
        // run a reader most wants to see.
        let depth = self.inner.depth.saturating_add(1);
        let mark = self.inner.trail.record(&path, site, depth);
        Ok(Self {
            inner: Arc::new(ScopeInner {
                tenant: self.inner.tenant.clone(),
                path,
                depth,
                token: match stop {
                    Stop::Own => self.inner.token.child_token(),
                    Stop::Shared => self.inner.token.clone(),
                },
                policy: Arc::clone(&self.inner.policy),
                trail: Arc::clone(&self.inner.trail),
                clock: self.inner.clock.clone(),
                run: self.inner.run,
                mark: Some(mark),
            }),
        })
    }

    /// Record in the trail a step beneath this scope that enters no scope of
    /// its own (a `within`), at `path`, from `site`; the caller settles it.
    pub(super) fn mark(&self, path: &Arc<str>, site: &'static Site) -> Arc<Mark> {
        self.inner
            .trail
            .record(path, Some(site), self.inner.depth.saturating_add(1))
    }

    #[doc(hidden)]
    /// Record how this step ended, for its trail entry. The first settlement
    /// stands, and a scope no step entered records nothing. Called by
    /// `script!`.
    pub fn settle<T>(&self, result: &Result<T, FlowError>) {
        self.settle_with(|| Outcome::of(result));
    }

    /// [`Scope::settle`] with an outcome the caller derives, built only when
    /// this step has not settled yet.
    pub(super) fn settle_with(&self, outcome: impl FnOnce() -> Outcome) {
        if let Some(ref mark) = self.inner.mark {
            mark.settle(outcome);
        }
    }

    /// The decisions shared by every step under this scope's root.
    pub(super) fn policy(&self) -> &Policy {
        &self.inner.policy
    }

    /// This scope's path, shared rather than copied, for error locations.
    pub(crate) fn shared_path(&self) -> &Arc<str> {
        &self.inner.path
    }

    /// This scope's path with `segment` appended.
    pub(super) fn join(&self, segment: &str) -> Arc<str> {
        if self.inner.path.is_empty() {
            Arc::from(segment)
        } else {
            Arc::from(format!("{}/{segment}", self.inner.path))
        }
    }

    /// Who this flow runs for.
    #[must_use]
    pub fn tenant(&self) -> &Tenant {
        &self.inner.tenant
    }

    /// The run this scope's durable records are keyed by, when a host minted
    /// one.
    ///
    /// `None` for every scope built by hand — [`Scope::root`] and
    /// [`Scope::with_token`] — and for a run whose host has no store installed:
    /// neither is ever durable, and a caller can read that here rather than
    /// inferring it from a missing record.
    #[must_use]
    pub fn run(&self) -> Option<RunId> {
        self.inner.run
    }

    /// The steps from the root to here, joined by `/`.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.inner.path
    }

    /// The idempotency key for this step of this run and tenant. See
    /// [`StepKey`].
    ///
    /// Under a host that minted a run, the run id is part of the hash: two runs
    /// of the same flow never share a key, so an effect deduplicating by this
    /// key happens once per run, and every attempt and restart of one run
    /// shares the key and happens once per run. A scope built by hand has no
    /// run and keys by tenant and path alone — the form the run store records
    /// under. Enforced by `two_runs_of_one_flow_never_share_a_step_key`
    /// (`tests/it/task_front_door/limits.rs`) and
    /// `a_retry_attempts_share_one_step_key` (`tests/it/script_trail.rs`).
    #[must_use]
    pub fn key(&self) -> StepKey {
        match self.inner.run {
            Some(run) => step_key_in_run(self.inner.tenant.as_str(), run, &self.inner.path),
            None => step_key(self.inner.tenant.as_str(), &self.inner.path),
        }
    }

    /// The key the run store records this step's output under: the run-less
    /// form, so a record's bytes are what they have always been and a store
    /// written before run-scoped keys existed replays without a migration.
    pub(crate) fn record_key(&self) -> StepKey {
        step_key(self.inner.tenant.as_str(), &self.inner.path)
    }

    /// The idempotency key for one piece of business work, stable across runs.
    ///
    /// [`Scope::key`] is per run, so an effect that must happen once for a
    /// piece of work no matter which run performs it — a card charged for an
    /// order, a mail sent for a signup — deduplicates on this instead: BLAKE3
    /// over a domain label, the tenant, `name` and the input's canonical
    /// [`InputDigest`], each length-framed. Two calls with the same name and input
    /// under any two runs
    /// of any flow for one tenant return the same key; a different name or
    /// input hashes differently.
    ///
    /// `name` follows the same rule as a request key: 1 to
    /// [`MAX_REQUEST_KEY_BYTES`] bytes of ASCII letters, digits, and `-`, `_`,
    /// `.`, `:`.
    ///
    /// ```
    /// use lgwks_bot::script::{Scope, Tenant};
    ///
    /// let scope = Scope::root(Tenant::new("acme")?);
    /// let first = scope.business_key("charge-card", &9001_u32)?;
    /// let again = scope.business_key("charge-card", &9001_u32)?;
    /// assert!(first == again, "one order, one charge, whatever ran it");
    /// # Ok::<(), lgwks_bot::script::FlowError>(())
    /// ```
    ///
    /// Enforced by `sim_business_key_dedups_across_runs`
    /// (`tests/it/sim_step_key_runs.rs`).
    ///
    /// # Errors
    ///
    /// [`FlowError::InvalidBusinessKey`] when `name` is not a legal identifier.
    pub fn business_key<I: InputIdentity>(
        &self,
        name: &str,
        input: &I,
    ) -> Result<Digest, FlowError> {
        if let Some(fault) = name_fault(name, MAX_REQUEST_KEY_BYTES) {
            let reason = match fault {
                NameFault::Empty => "the business key name is empty",
                NameFault::TooLong => "the business key name is longer than MAX_REQUEST_KEY_BYTES",
                NameFault::BadChar => {
                    "the business key name may hold only ASCII letters, digits, '-', '_', '.' and ':'"
                }
            };
            let refusal = Err(FlowError::InvalidBusinessKey { reason });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "business_key: returning an error to the caller");
            return refusal;
        }
        let mut hasher = Hasher::new();
        hasher
            .write_framed(BUSINESS_KEY_DOMAIN)
            .write_framed(self.inner.tenant.as_str().as_bytes())
            .write_framed(name.as_bytes())
            .write_framed(InputDigest::of(input).as_bytes());
        Ok(hasher.finalize())
    }

    /// The token that stops this scope's work.
    #[must_use]
    pub fn token(&self) -> &CancellationToken {
        &self.inner.token
    }

    /// The clock every deadline under this scope is measured on.
    ///
    /// Inherited by each step descended from here, so a reader can ask which
    /// clock governs a `within` budget instead of assuming. A caller that needs
    /// to move time holds the [`Clock`] it built the scope with — this borrow
    /// cannot advance it.
    #[must_use]
    pub fn clock(&self) -> &Clock {
        &self.inner.clock
    }

    /// Stop this scope and every step beneath it; inside an `each` body or a
    /// `retry` attempt, that is the enclosing fan-out or retry.
    pub fn cancel(&self) {
        self.inner.token.cancel();
    }

    /// Whether this scope, or one above it, has been stopped.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.inner.token.is_cancelled()
    }

    /// Refuse to go on once stopped.
    ///
    /// # Errors
    ///
    /// [`FlowError::Cancelled`] at this scope's path.
    pub fn checkpoint(&self) -> Result<(), FlowError> {
        if self.is_cancelled() {
            let refusal = Err(FlowError::Cancelled {
                at: Arc::clone(&self.inner.path),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "checkpoint: returning an error to the caller");
            return refusal;
        }
        Ok(())
    }

    /// Require `caps` before doing work that reaches them.
    ///
    /// The step-level half of admission. A [`Scope::root`] built by hand, and any
    /// run outside a [`Host`](crate::task::Host), has no authority installed and so
    /// requires nothing — the check is vacuous rather than false, because a step
    /// outside a host has nothing to reach with either.
    ///
    /// Under a host, every capability in `caps` must be covered by the run's
    /// authority: the host's grant plus the delta of any repair this run is
    /// carrying. When any is missing, this returns
    /// [`FlowError::Blocked`] carrying **every**
    /// unmet capability, not the first — so the run's report names the whole
    /// shortfall at once and the repair ticket is written once against all of it.
    ///
    /// This is the step that blocks *after* the run has already done work, which is
    /// the case a repair exists for: the analysis before it is recorded, so a
    /// repair resumes and replays that record instead of paying for the analysis
    /// again.
    ///
    /// # Errors
    ///
    /// [`FlowError::Blocked`] naming every
    /// unmet capability, or [`FlowError::Cancelled`]
    /// if this scope is already stopped.
    pub fn require(&self, caps: &[crate::cap::Cap]) -> Result<(), FlowError> {
        self.checkpoint()?;
        let Some(shortfall) =
            super::run_store::authority().and_then(|authority| authority.shortfall(caps))
        else {
            return Ok(());
        };
        let Some(deficit) = crate::cap::Deficit::from_shortages(shortfall) else {
            return Ok(());
        };
        Err(FlowError::Blocked {
            at: Arc::clone(&self.inner.path),
            deficit: Box::new(deficit),
        })
    }
}

/// Whether a child scope can be stopped apart from its parent.
#[derive(Clone, Copy)]
enum Stop {
    /// A child token: cancelling the child leaves the parent running.
    Own,
    /// The parent's token: the child stops exactly when the parent does.
    Shared,
}

impl fmt::Debug for Scope {
    /// The tenant and path; the token has no useful rendering.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Scope")
            .field("tenant", &self.inner.tenant.as_str())
            .field("path", &self.path())
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}
