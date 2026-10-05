//! Tenants, step keys and the scope every step runs in.

use std::fmt;
use std::sync::Arc;

use lgwks_std::hash::{Digest, Hasher};

use crate::effect::RunId;
use crate::rt::clock::Clock;
use crate::rt::sync::CancellationToken;

use super::policy::Policy;
use super::trail::Trail;
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

/// The stable identity of one step of one flow for one tenant.
///
/// BLAKE3 over the tenant and the step path, each length-framed so no two
/// different pairs can hash the same bytes. It is the same on every attempt of
/// a [`retry`](crate::script::retry) and on every run of the same flow over the same input order,
/// which is what makes it usable as an idempotency key for an effect the step
/// performs: an effect sent twice under one key is one effect to any receiver
/// that deduplicates by it.
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

/// The [`StepKey`] of `path` for `tenant`: one definition, so a reserved record a
/// host writes and a step a body enters cannot key the same path differently.
pub(crate) fn step_key(tenant: &str, path: &str) -> StepKey {
    let mut hasher = Hasher::new();
    hasher
        .write_framed(tenant.as_bytes())
        .write_framed(path.as_bytes());
    StepKey(hasher.finalize())
}

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
        self.descend(self.join(step), Stop::Own)
    }

    /// Enter the named step sharing this scope's stop: a `retry`'s attempts.
    pub(super) fn enter_sharing(&self, step: &str) -> Result<Self, FlowError> {
        self.descend(self.join(step), Stop::Shared)
    }

    /// Enter item `index` of the named step: one body of an [`each`](crate::script::each).
    ///
    /// # Errors
    ///
    /// As [`Scope::enter`].
    pub fn item(&self, step: &str, index: usize) -> Result<Self, FlowError> {
        self.descend(self.join(&format!("{step}#{index}")), Stop::Own)
    }

    /// Item `index` of the step this scope is: path `<this path>#index`, so an
    /// `each` body reads `sync/each:page#39`, not `sync/each:page/each:page#39`,
    /// sharing the step's stop.
    pub(super) fn numbered(&self, index: usize) -> Result<Self, FlowError> {
        self.descend(
            Arc::from(format!("{}#{index}", self.inner.path)),
            Stop::Shared,
        )
    }

    /// A child scope at `path` with its own stop or this one's.
    fn descend(&self, path: Arc<str>, stop: Stop) -> Result<Self, FlowError> {
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
        self.inner.trail.record(&path);
        Ok(Self {
            inner: Arc::new(ScopeInner {
                tenant: self.inner.tenant.clone(),
                path: Arc::clone(&path),
                depth: self.inner.depth.saturating_add(1),
                token: match stop {
                    Stop::Own => self.inner.token.child_token(),
                    Stop::Shared => self.inner.token.clone(),
                },
                policy: Arc::clone(&self.inner.policy),
                trail: Arc::clone(&self.inner.trail),
                clock: self.inner.clock.clone(),
                run: self.inner.run,
            }),
        })
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

    /// The idempotency key for this step and tenant. See [`StepKey`].
    #[must_use]
    pub fn key(&self) -> StepKey {
        step_key(self.inner.tenant.as_str(), &self.inner.path)
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
