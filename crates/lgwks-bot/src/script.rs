//! The orchestration runtime behind [`script!`](crate::script!).
//!
//! `script!` is a thin syntax layer: every block it accepts desugars into one
//! call in this module, and this module is where the semantics live. That split
//! is deliberate. Syntax is read by people and models; semantics are read by
//! the compiler, the tests, and anyone running `cargo expand`. Keeping the
//! semantics in ordinary, documented functions means the expansion of a script
//! is a short list of calls an auditor can follow, not a generated state
//! machine nobody can read.
//!
//! # What a flow cannot get wrong
//!
//! The estate's fix history (45 repositories, 2,766 fix and revert commits as
//! of 2026-09-30) is dominated by the same few orchestration defects: work with
//! no ceiling, waits with no deadline, retries that duplicate an effect,
//! identities that leak across tenants, errors that vanish, and tasks nobody
//! owns. Each primitive here makes one of those unrepresentable rather than
//! documented:
//!
//! | Block | Primitive | What it rules out |
//! |---|---|---|
//! | `each x in xs, at most N at once:` | [`each`] | unbounded fan-out; lost results; orphaned siblings after a failure |
//! | `within 2s:` | [`within`] | a wait with no deadline; a wait that ignores cancellation |
//! | `retry up to 3 times, waiting 100ms:` | [`retry`] | retry storms; retrying a permanent failure; a new identity per attempt |
//! | `together:` | `try_join!` | sequential awaits that should overlap; a failed branch that keeps running |
//! | `step name:` | [`Scope::enter`] | anonymous work with no stable key or error location |
//!
//! Every flow takes a [`Scope`] first. The scope carries the [`Tenant`], the
//! path of steps that led here, and the cancellation token, so identity,
//! location and cancellation reach every step without being threaded by hand,
//! and a [`StepKey`] is always the hash of *tenant and path*: two tenants
//! running the same flow over the same input never share a key.
//!
//! # Orchestration is the architecture
//!
//! A `script!` block also emits a constant `ARCHITECTURE` ([`Architecture`]):
//! the flows it declares and the tree of blocks inside each, with bounds,
//! deadlines, retry budgets and the flows each one runs. It is compiled from the
//! same tokens as the code, so it cannot drift from it, and it is the thing to
//! read (or diff) instead of re-deriving the orchestration from source.
//!
//! # Boundary
//!
//! [`each`] drives its bodies concurrently **on the task that awaits it**. That
//! is what lets a body borrow the flow's locals the way a Python loop body
//! would, with no `Arc`, no `clone`, and no `'static` bound; it also means a
//! flow that contains `each` is not `Send`, in the same way the four verbs are
//! not (`INV-BOT-FOUR-VERBS`). Concurrency is for waiting on many things at
//! once. CPU parallelism across cores is a different tool:
//! [`join_all_bounded`](crate::rt::task::join_all_bounded) on owned inputs.
//!
//! [`each`]: crate::script::each
//! [`within`]: crate::script::within
//! [`retry`]: crate::script::retry
//! [`Scope`]: crate::script::Scope
//! [`Scope::enter`]: crate::script::Scope::enter
//! [`Tenant`]: crate::script::Tenant
//! [`StepKey`]: crate::script::StepKey
//! [`Architecture`]: crate::script::Architecture

use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::num::{NonZeroU32, NonZeroUsize};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

use lgwks_std::hash::{Digest, Hasher};
use lgwks_std::json::Serialize;

use crate::error::{BotError, RetryClass};
use crate::rt::sync::CancellationToken;
use crate::rt::time;

/// The longest tenant name a [`Tenant`] accepts, in bytes.
///
/// A tenant name is part of every key and every error location, so it is
/// bounded like any other payload that is copied into each of them.
pub const MAX_TENANT_BYTES: usize = 128;

/// How deeply steps may nest inside one flow run.
///
/// A flow that runs itself, directly or through another flow, grows its path
/// by one segment per call. The limit turns that into a typed
/// [`FlowError::TooDeep`] instead of an ever-longer path and an eventual stack
/// overflow.
pub const MAX_DEPTH: u16 = 64;

/// The most bodies one [`each`] may have in flight at once.
///
/// `at most N at once` is required, and this caps what `N` may say: a bound of
/// a million is a bound in name only.
pub const MAX_IN_FLIGHT: usize = 65_536;

/// The most attempts one [`retry`] may make.
pub const MAX_ATTEMPTS: u32 = 1_000;

/// The longest single wait between two [`retry`] attempts, before jitter.
pub const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Bodies one poll of [`each`] may poll before it hands the executor back.
///
/// Without a ceiling, a fan-out whose bodies keep waking themselves is one
/// endless poll. That is not hypothetical: the async engine's cooperative
/// budget answers a task that has done enough work in one poll with `Pending`
/// **and an immediate wake**, so a driver that re-polls every woken body before
/// returning spins on that wake forever. It was observed as a hang of a
/// 10,000-item fan-out over timers. Returning after this many polls, with the
/// task re-woken, lets the engine reset its budget; the woken bodies stay
/// queued and are polled on the next turn. 128 matches the engine's own
/// per-poll budget.
const POLL_BUDGET: usize = 128;

// ── Tenant ──────────────────────────────────────────────────────────────────

/// The identity a flow runs on behalf of.
///
/// Every [`Scope`] carries one, and every [`StepKey`] hashes it, so data keyed
/// by a step is keyed by its tenant whether or not the author remembered to.
/// The name is 1 to [`MAX_TENANT_BYTES`] bytes of ASCII letters, digits, and
/// `-`, `_`, `.`, `:`, which keeps it safe to put in a path, a log line, and a
/// file name without escaping.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Tenant(Arc<str>);

impl Tenant {
    /// Validate and hold a tenant name.
    ///
    /// # Errors
    ///
    /// [`FlowError::InvalidTenant`] naming what is wrong with the name.
    pub fn new(name: &str) -> Result<Self, FlowError> {
        if name.is_empty() {
            return Err(FlowError::InvalidTenant {
                reason: "the tenant name is empty",
            });
        }
        if name.len() > MAX_TENANT_BYTES {
            return Err(FlowError::InvalidTenant {
                reason: "the tenant name is longer than MAX_TENANT_BYTES",
            });
        }
        let allowed = |byte: u8| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte);
        if !name.bytes().all(allowed) {
            return Err(FlowError::InvalidTenant {
                reason: "the tenant name may hold only ASCII letters, digits, '-', '_', '.' and ':'",
            });
        }
        Ok(Self(Arc::from(name)))
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
/// a [`retry`] and on every run of the same flow over the same input order,
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
    /// How many segments `path` holds.
    depth: u16,
    /// Cancelled when this scope, or any scope above it, is cancelled.
    token: CancellationToken,
}

impl Scope {
    /// A root scope for `tenant` with a fresh cancellation token.
    #[must_use]
    pub fn root(tenant: Tenant) -> Self {
        Self::with_token(tenant, CancellationToken::new())
    }

    /// A root scope for `tenant` that stops when `token` is cancelled.
    ///
    /// The form to use under a [`Supervisor`](crate::rt::supervise::Supervisor):
    /// pass the token its `spawn` hands the body, and the supervisor's
    /// shutdown reaches every step of the flow.
    #[must_use]
    pub fn with_token(tenant: Tenant, token: CancellationToken) -> Self {
        Self {
            inner: Arc::new(ScopeInner {
                tenant,
                path: Arc::from(""),
                depth: 0,
                token,
            }),
        }
    }

    /// Enter the named step beneath this scope.
    ///
    /// # Errors
    ///
    /// [`FlowError::Cancelled`] if this scope is already cancelled, so no new
    /// step starts after a stop; [`FlowError::TooDeep`] past [`MAX_DEPTH`].
    pub fn enter(&self, step: &str) -> Result<Self, FlowError> {
        self.descend(step, None)
    }

    /// Enter item `index` of the named step: one body of an [`each`].
    ///
    /// # Errors
    ///
    /// As [`Scope::enter`].
    pub fn item(&self, step: &str, index: usize) -> Result<Self, FlowError> {
        self.descend(step, Some(index))
    }

    /// The shared half of [`Scope::enter`] and [`Scope::item`].
    fn descend(&self, step: &str, index: Option<usize>) -> Result<Self, FlowError> {
        self.checkpoint()?;
        if self.inner.depth >= MAX_DEPTH {
            return Err(FlowError::TooDeep {
                at: Arc::clone(&self.inner.path),
                limit: MAX_DEPTH,
            });
        }
        let segment = match index {
            Some(position) => format!("{step}#{position}"),
            None => step.to_owned(),
        };
        Ok(Self {
            inner: Arc::new(ScopeInner {
                tenant: self.inner.tenant.clone(),
                path: self.join(&segment),
                depth: self.inner.depth.saturating_add(1),
                token: self.inner.token.child_token(),
            }),
        })
    }

    /// This scope's path with `segment` appended.
    fn join(&self, segment: &str) -> Arc<str> {
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

    /// The steps from the root to here, joined by `/`.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.inner.path
    }

    /// The idempotency key for this step and tenant. See [`StepKey`].
    #[must_use]
    pub fn key(&self) -> StepKey {
        let mut hasher = Hasher::new();
        hasher
            .write_framed(self.inner.tenant.as_str().as_bytes())
            .write_framed(self.inner.path.as_bytes());
        StepKey(hasher.finalize())
    }

    /// The token that stops this scope's work.
    #[must_use]
    pub fn token(&self) -> &CancellationToken {
        &self.inner.token
    }

    /// Stop this scope and every step beneath it.
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
            return Err(FlowError::Cancelled {
                at: Arc::clone(&self.inner.path),
            });
        }
        Ok(())
    }
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

// ── FlowError ───────────────────────────────────────────────────────────────

/// Why a flow, or one step of it, did not produce its value.
///
/// Every variant carries `at`, the step path where it happened, so a failure in
/// the fortieth item of a nested fan-out reads `sync/each@4#39/retry@5` rather
/// than a bare message. An error raised with `?` before a location is known is
/// located at the flow boundary by [`FlowError::located`].
///
/// Retry is decided by the variant, never by the message:
/// [`FlowError::is_retryable`] is the only rule [`retry`] reads.
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
    /// [`RetryClass`] is [`RetryClass::Safe`].
    Bot {
        /// Where it failed.
        at: Arc<str>,
        /// The verb's typed error, boxed so every flow `Result` stays small.
        source: Box<BotError>,
    },
    /// Steps nested past [`MAX_DEPTH`].
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
    /// [`RetryClass::Safe`] (the effect definitely did not happen). A
    /// cancellation, a permanent failure, an exhausted retry, and every
    /// validation failure are not: repeating them is a retry storm.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match *self {
            Self::TimedOut { .. } | Self::Transient { .. } => true,
            Self::Bot { ref source, .. } => source.retry_class() == RetryClass::Safe,
            Self::Cancelled { .. }
            | Self::Exhausted { .. }
            | Self::Failed { .. }
            | Self::TooDeep { .. }
            | Self::InvalidTenant { .. }
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
            | Self::Failed { ref at, .. }
            | Self::Transient { ref at, .. }
            | Self::Bot { ref at, .. }
            | Self::TooDeep { ref at, .. } => at,
            Self::InvalidTenant { .. } | Self::InvalidBound { .. } => "",
        }
    }

    /// Give an unlocated error the location `scope` names.
    ///
    /// An error that already has a location keeps it: the innermost location
    /// is the one that says where the failure happened.
    #[must_use]
    pub fn located(self, scope: &Scope) -> Self {
        self.located_at(&scope.inner.path)
    }

    /// [`FlowError::located`] against a path already in hand.
    #[must_use]
    fn located_at(mut self, path: &Arc<str>) -> Self {
        match self {
            Self::Cancelled { ref mut at }
            | Self::TimedOut { ref mut at, .. }
            | Self::Exhausted { ref mut at, .. }
            | Self::Failed { ref mut at, .. }
            | Self::Transient { ref mut at, .. }
            | Self::Bot { ref mut at, .. }
            | Self::TooDeep { ref mut at, .. } => {
                if at.is_empty() {
                    *at = Arc::clone(path);
                }
            }
            Self::InvalidTenant { .. } | Self::InvalidBound { .. } => {}
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
            Self::Failed { ref at, ref reason } => write!(formatter, "{at}: failed: {reason:?}"),
            Self::Transient { ref at, ref reason } => {
                write!(formatter, "{at}: failed (transient): {reason:?}")
            }
            Self::Bot { ref at, ref source } => write!(formatter, "{at}: {source}"),
            Self::TooDeep { ref at, limit } => {
                write!(formatter, "{at}: steps nested deeper than {limit}")
            }
            Self::InvalidTenant { reason } => write!(formatter, "invalid tenant: {reason}"),
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
            Self::Exhausted { ref last, .. } => Some(&**last),
            Self::Cancelled { .. }
            | Self::TimedOut { .. }
            | Self::Failed { .. }
            | Self::Transient { .. }
            | Self::TooDeep { .. }
            | Self::InvalidTenant { .. }
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

// ── Bounds ──────────────────────────────────────────────────────────────────

/// Check an `at most N at once` bound computed at run time.
///
/// `script!` checks a literal bound at compile time; this is the same rule for
/// a bound that is an expression.
///
/// # Errors
///
/// [`FlowError::InvalidBound`] outside `1..=MAX_IN_FLIGHT`.
pub fn at_most(limit: usize) -> Result<NonZeroUsize, FlowError> {
    NonZeroUsize::new(limit)
        .filter(|bound| bound.get() <= MAX_IN_FLIGHT)
        .ok_or(FlowError::InvalidBound {
            what: "each: at most N at once",
            value: u64::try_from(limit).unwrap_or(u64::MAX),
            max: u64::try_from(MAX_IN_FLIGHT).unwrap_or(u64::MAX),
        })
}

/// Check a `retry up to N times` budget computed at run time.
///
/// # Errors
///
/// [`FlowError::InvalidBound`] outside `1..=MAX_ATTEMPTS`.
pub fn attempts(count: u32) -> Result<NonZeroU32, FlowError> {
    NonZeroU32::new(count)
        .filter(|budget| budget.get() <= MAX_ATTEMPTS)
        .ok_or(FlowError::InvalidBound {
            what: "retry: up to N times",
            value: u64::from(count),
            max: u64::from(MAX_ATTEMPTS),
        })
}

// ── within ──────────────────────────────────────────────────────────────────

/// Run `body`, failing with [`FlowError::TimedOut`] once `limit` passes and
/// with [`FlowError::Cancelled`] as soon as `scope` is stopped.
///
/// Either way the body is dropped, which is how a Rust future is stopped: an
/// expired step does not keep running in the background.
///
/// # Errors
///
/// The body's own error, `TimedOut`, or `Cancelled`.
pub async fn within<T, Fut>(
    scope: &Scope,
    step: &str,
    limit: Duration,
    body: Fut,
) -> Result<T, FlowError>
where
    Fut: Future<Output = Result<T, FlowError>>,
{
    match scope
        .token()
        .run_until_cancelled(time::timeout(limit, body))
        .await
    {
        Some(Ok(result)) => result,
        Some(Err(_elapsed)) => Err(FlowError::TimedOut {
            at: scope.join(step),
            after: limit,
        }),
        None => Err(FlowError::Cancelled {
            at: scope.join(step),
        }),
    }
}

// ── retry ───────────────────────────────────────────────────────────────────

/// Run `body` until it succeeds, fails permanently, or spends `attempts`.
///
/// Every attempt runs in the **same** step scope, so [`Scope::key`] inside the
/// body is identical on each attempt: an effect keyed by it is one effect
/// however many times it is sent. The attempt number (from 1) is passed
/// alongside for the rare body that must know it.
///
/// Between attempts the wait doubles from `backoff`, capped at
/// [`MAX_BACKOFF`], plus up to half again of deterministic jitter drawn from
/// the step key: siblings retrying the same failure spread out instead of
/// arriving together, and the same step always waits the same time, so a
/// replay is exact. The wait races cancellation.
///
/// # Errors
///
/// The first non-retryable error as-is; [`FlowError::Exhausted`] wrapping the
/// last error when the budget runs out; [`FlowError::Cancelled`] on a stop.
pub async fn retry<T, F>(
    scope: &Scope,
    step: &str,
    attempts: NonZeroU32,
    backoff: Duration,
    mut body: F,
) -> Result<T, FlowError>
where
    F: AsyncFnMut(&Scope, u32) -> Result<T, FlowError>,
{
    let here = scope.enter(step)?;
    let key = here.key();
    let mut attempt: u32 = 1;
    loop {
        here.checkpoint()?;
        let error = match body(&here, attempt).await {
            Ok(value) => return Ok(value),
            Err(error) => error.located(&here),
        };
        if !error.is_retryable() {
            return Err(error);
        }
        if attempt >= attempts.get() {
            return Err(FlowError::Exhausted {
                at: Arc::clone(&here.inner.path),
                attempts: attempt,
                last: Box::new(error),
            });
        }
        let delay = backoff_delay(backoff, attempt, &key);
        if !delay.is_zero()
            && here
                .token()
                .run_until_cancelled(time::sleep(delay))
                .await
                .is_none()
        {
            return Err(FlowError::Cancelled {
                at: Arc::clone(&here.inner.path),
            });
        }
        attempt = attempt.saturating_add(1);
    }
}

/// The wait after failed attempt `attempt`: `base · 2^(attempt-1)`, capped at
/// [`MAX_BACKOFF`], plus `0..=½` of that again chosen by a byte of `key`.
fn backoff_delay(base: Duration, attempt: u32, key: &StepKey) -> Duration {
    if base.is_zero() {
        return Duration::ZERO;
    }
    let shift = attempt.saturating_sub(1).min(16);
    let factor = 1_u32.checked_shl(shift).unwrap_or(u32::MAX);
    let grown = base.saturating_mul(factor).min(MAX_BACKOFF);
    let byte = usize::try_from(attempt & 31)
        .ok()
        .and_then(|index| key.as_bytes().get(index).copied())
        .unwrap_or(0);
    let jitter = grown
        .saturating_mul(u32::from(byte))
        .checked_div(512)
        .unwrap_or(Duration::ZERO);
    grown.saturating_add(jitter)
}

// ── each ────────────────────────────────────────────────────────────────────

/// Run `body` once per item, at most `limit` at a time, and return the values
/// in input order.
///
/// - **Bounded.** At most `limit` bodies exist at once. Items are drawn from the
///   iterator only as slots free, so an iterator of a million items holds
///   `limit` bodies and one output vector, never a million futures.
/// - **Ordered.** Output `i` is item `i`'s value, whatever order they finish in.
/// - **Fail-fast and owned.** The first error stops the fan-out: the step's
///   token is cancelled, every body still running is dropped, no further item
///   is started, and that error is returned located at its item's path. No
///   sibling outlives the call.
/// - **Keyed.** Item `i` runs in the scope `<step>#i`, so its [`StepKey`] is
///   stable across runs over the same input order and distinct per tenant.
/// - **Woken precisely.** Each slot has its own waker; a wake re-polls that
///   body only, so one wake costs `O(1)` polls rather than a scan of every
///   body in flight.
/// - **Fair to its executor.** One poll polls at most a fixed number of
///   bodies, then yields with the task re-woken, so a fan-out never
///   monopolises its thread or spins on the engine's cooperative budget.
///
/// # Errors
///
/// The first body error, or [`FlowError::Cancelled`] when `scope` is stopped.
pub async fn each<I, T, F>(
    scope: &Scope,
    step: &str,
    limit: NonZeroUsize,
    items: I,
    body: F,
) -> Result<Vec<T>, FlowError>
where
    I: IntoIterator,
    F: AsyncFn(Scope, I::Item) -> Result<T, FlowError>,
{
    let here = scope.enter(step)?;
    let width = limit.get().min(MAX_IN_FLIGHT);
    let wakes = Arc::new(Wakes::new(width));
    let wakers: Vec<Waker> = (0..width)
        .map(|position| {
            Waker::from(Arc::new(SlotWaker {
                position,
                wakes: Arc::clone(&wakes),
            }))
        })
        .collect();
    let mut fan = Fan {
        slots: (0..width).map(|_| None).collect(),
        free: (0..width).rev().collect(),
        results: Vec::new(),
        exhausted: false,
    };
    let mut inputs = items.into_iter();
    let mut stopped = Box::pin(here.token().cancelled_owned());
    let body = &body;
    let here = &here;

    std::future::poll_fn(move |context: &mut Context<'_>| {
        wakes.set_parent(context.waker());
        if stopped.as_mut().poll(context).is_ready() {
            fan.clear();
            return Poll::Ready(Err(FlowError::Cancelled {
                at: Arc::clone(&here.inner.path),
            }));
        }
        let mut polled: usize = 0;
        loop {
            if let Err(error) = fan.admit(&mut inputs, here, step, body, &wakes) {
                fan.clear();
                return Poll::Ready(Err(error));
            }
            let ready = wakes.take_ready();
            if ready.is_empty() {
                break;
            }
            for position in ready {
                polled = polled.saturating_add(1);
                if let Err(error) = fan.poll_slot(position, &wakers) {
                    here.cancel();
                    fan.clear();
                    return Poll::Ready(Err(error));
                }
            }
            if polled >= POLL_BUDGET {
                context.waker().wake_by_ref();
                return Poll::Pending;
            }
        }
        if fan.exhausted && fan.free.len() == fan.slots.len() {
            Poll::Ready(Ok(fan.results.drain(..).flatten().collect()))
        } else {
            Poll::Pending
        }
    })
    .await
}

/// A body in flight, boxed because an async closure's future cannot be named.
type Pending<'body, T> = Pin<Box<dyn Future<Output = Result<T, FlowError>> + 'body>>;

/// One occupied slot of a fan-out.
struct Slot<'body, T> {
    /// The item's position in the input, which is its position in the output.
    index: usize,
    /// The item's scope path, for locating its error.
    at: Arc<str>,
    /// The running body.
    future: Pending<'body, T>,
}

/// The state [`each`] drives: slots, the free list, and the ordered outputs.
struct Fan<'body, T> {
    /// `limit` slots; `None` is free.
    slots: Vec<Option<Slot<'body, T>>>,
    /// Free slot positions.
    free: Vec<usize>,
    /// One entry per item admitted, filled as each finishes.
    results: Vec<Option<T>>,
    /// Whether the input iterator has ended.
    exhausted: bool,
}

impl<'body, T: 'body> Fan<'body, T> {
    /// Start bodies for new items while a slot is free and input remains.
    fn admit<I, F>(
        &mut self,
        inputs: &mut I,
        here: &Scope,
        step: &str,
        body: &'body F,
        wakes: &Wakes,
    ) -> Result<(), FlowError>
    where
        I: Iterator<Item: 'body>,
        F: AsyncFn(Scope, I::Item) -> Result<T, FlowError>,
    {
        while !self.exhausted {
            let Some(position) = self.free.pop() else {
                break;
            };
            let Some(item) = inputs.next() else {
                self.free.push(position);
                self.exhausted = true;
                break;
            };
            let index = self.results.len();
            self.results.push(None);
            let scope = here.item(step, index)?;
            let at = Arc::clone(&scope.inner.path);
            let future: Pending<'body, T> = Box::pin(body(scope, item));
            if let Some(slot) = self.slots.get_mut(position) {
                *slot = Some(Slot { index, at, future });
            }
            wakes.mark(position);
        }
        Ok(())
    }

    /// Poll the body in `position` with its own waker, keeping its value or
    /// returning its located error.
    fn poll_slot(&mut self, position: usize, wakers: &[Waker]) -> Result<(), FlowError> {
        let (Some(entry), Some(waker)) = (self.slots.get_mut(position), wakers.get(position))
        else {
            return Ok(());
        };
        let Some(slot) = entry.as_mut() else {
            return Ok(());
        };
        let mut context = Context::from_waker(waker);
        let Poll::Ready(outcome) = slot.future.as_mut().poll(&mut context) else {
            return Ok(());
        };
        let index = slot.index;
        let at = Arc::clone(&slot.at);
        *entry = None;
        self.free.push(position);
        let value = outcome.map_err(|error| error.located_at(&at))?;
        if let Some(output) = self.results.get_mut(index) {
            *output = Some(value);
        }
        Ok(())
    }

    /// Drop every body still running and admit nothing more.
    fn clear(&mut self) {
        for slot in &mut self.slots {
            *slot = None;
        }
        self.exhausted = true;
    }
}

/// Which slots have been woken, and the waker of the task driving them.
struct Wakes {
    /// Guarded together so a wake cannot land between reading the queue and
    /// registering the parent.
    state: Mutex<WakeState>,
}

/// The fields behind [`Wakes`].
struct WakeState {
    /// Slot positions woken since the last drain, each at most once.
    ready: VecDeque<usize>,
    /// Whether each position is already in `ready`.
    queued: Vec<bool>,
    /// The task that awaits the fan-out.
    parent: Option<Waker>,
}

impl Wakes {
    /// Room for `width` slots, none woken.
    fn new(width: usize) -> Self {
        Self {
            state: Mutex::new(WakeState {
                ready: VecDeque::with_capacity(width),
                queued: vec![false; width],
                parent: None,
            }),
        }
    }

    /// Take the lock. A poisoned lock still guards a consistent value: every
    /// critical section here is a few plain writes that cannot be left half
    /// done, so a panic elsewhere is not a reason to deadlock the fan-out.
    fn lock(&self) -> MutexGuard<'_, WakeState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Record the waker of the task driving the fan-out.
    fn set_parent(&self, waker: &Waker) {
        let mut state = self.lock();
        let stale = state
            .parent
            .as_ref()
            .is_none_or(|current| !current.will_wake(waker));
        if stale {
            state.parent = Some(waker.clone());
        }
    }

    /// Queue `position` for polling. Used when a body is first admitted, where
    /// the driving task is already running and needs no wake.
    fn mark(&self, position: usize) {
        enqueue(&mut self.lock(), position);
    }

    /// Queue `position` and hand back the waker of the task to wake.
    fn wake_slot(&self, position: usize) -> Option<Waker> {
        let mut state = self.lock();
        enqueue(&mut state, position);
        state.parent.clone()
    }

    /// Everything queued since the last call, with the flags cleared first so
    /// a wake during the coming polls queues its slot again.
    fn take_ready(&self) -> VecDeque<usize> {
        let mut state = self.lock();
        let ready = std::mem::take(&mut state.ready);
        for &position in &ready {
            if let Some(flag) = state.queued.get_mut(position) {
                *flag = false;
            }
        }
        ready
    }
}

/// Put `position` on the ready queue unless it is already there.
fn enqueue(state: &mut WakeState, position: usize) {
    if let Some(flag) = state.queued.get_mut(position)
        && !*flag
    {
        *flag = true;
        state.ready.push_back(position);
    }
}

/// The waker one slot's body sees: it queues the slot and wakes the parent.
struct SlotWaker {
    /// Which slot.
    position: usize,
    /// The fan-out's queue.
    wakes: Arc<Wakes>,
}

impl Wake for SlotWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if let Some(parent) = self.wakes.wake_slot(self.position) {
            parent.wake();
        }
    }
}

// ── Architecture ────────────────────────────────────────────────────────────

/// The orchestration a `script!` block declares, compiled from its tokens.
///
/// Every `script!` emits one as `ARCHITECTURE`. It is the map an agent or a
/// reviewer reads instead of re-deriving the flow graph from source, and since
/// it is built from the same tokens as the code it cannot describe a flow the
/// code does not run. [`Display`](fmt::Display) renders the tree;
/// [`Architecture::to_json`] is the machine form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(crate = "lgwks_std::json::serde")]
pub struct Architecture {
    /// The flows, in declaration order.
    flows: &'static [FlowShape],
}

impl Architecture {
    /// Assemble a map. Called by `script!`.
    #[must_use]
    pub const fn new(flows: &'static [FlowShape]) -> Self {
        Self { flows }
    }

    /// The flows, in declaration order.
    #[must_use]
    pub const fn flows(&self) -> &'static [FlowShape] {
        self.flows
    }

    /// The flow named `name`.
    #[must_use]
    pub fn flow(&self, name: &str) -> Option<&'static FlowShape> {
        self.flows.iter().find(|flow| flow.name == name)
    }

    /// The map as JSON.
    ///
    /// # Errors
    ///
    /// The encoder's error; the map holds only strings and numbers, so this
    /// does not fail in practice.
    pub fn to_json(&self) -> Result<String, lgwks_std::json::Error> {
        lgwks_std::json::to_string(self)
    }
}

impl fmt::Display for Architecture {
    /// One line per flow and per block, indented by nesting, each ending with
    /// the source line it was declared on.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for flow in self.flows {
            writeln!(formatter, "flow {}  @{}", flow.signature, flow.line)?;
            render_steps(formatter, flow.steps, 1)?;
        }
        Ok(())
    }
}

/// Write `steps` and their children at `depth`.
fn render_steps(
    formatter: &mut fmt::Formatter<'_>,
    steps: &[StepShape],
    depth: usize,
) -> fmt::Result {
    for step in steps {
        for _ in 0..depth {
            formatter.write_str("  ")?;
        }
        writeln!(formatter, "{}  @{}", step.detail, step.line)?;
        render_steps(formatter, step.steps, depth.saturating_add(1))?;
    }
    Ok(())
}

/// One flow in an [`Architecture`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(crate = "lgwks_std::json::serde")]
pub struct FlowShape {
    /// The flow's name.
    name: &'static str,
    /// `name(params) -> output`, as written.
    signature: &'static str,
    /// The source line of the `flow` header.
    line: u32,
    /// The blocks directly inside the flow.
    steps: &'static [StepShape],
}

impl FlowShape {
    /// Describe a flow. Called by `script!`.
    #[must_use]
    pub const fn new(
        name: &'static str,
        signature: &'static str,
        line: u32,
        steps: &'static [StepShape],
    ) -> Self {
        Self {
            name,
            signature,
            line,
            steps,
        }
    }

    /// The flow's name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// `name(params) -> output`, as written.
    #[must_use]
    pub const fn signature(&self) -> &'static str {
        self.signature
    }

    /// The blocks directly inside the flow.
    #[must_use]
    pub const fn steps(&self) -> &'static [StepShape] {
        self.steps
    }

    /// Every flow this one runs, anywhere inside it, in source order.
    #[must_use]
    pub fn runs(&self) -> Vec<&'static str> {
        let mut callees = Vec::new();
        collect_runs(self.steps, &mut callees);
        callees
    }
}

/// Append the callee of every `run` in `steps`, depth first.
fn collect_runs(steps: &'static [StepShape], callees: &mut Vec<&'static str>) {
    for step in steps {
        if step.kind == StepKind::Run {
            callees.push(step.subject);
        }
        collect_runs(step.steps, callees);
    }
}

/// One block inside a flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(crate = "lgwks_std::json::serde")]
pub struct StepShape {
    /// Which block.
    kind: StepKind,
    /// The block's subject: the step name, the flow a `run` calls, the bound.
    subject: &'static str,
    /// The header as written, for rendering.
    detail: &'static str,
    /// The source line of the header.
    line: u32,
    /// Blocks nested inside.
    steps: &'static [StepShape],
}

impl StepShape {
    /// Describe a block. Called by `script!`.
    #[must_use]
    pub const fn new(
        kind: StepKind,
        subject: &'static str,
        detail: &'static str,
        line: u32,
        steps: &'static [StepShape],
    ) -> Self {
        Self {
            kind,
            subject,
            detail,
            line,
            steps,
        }
    }

    /// Which block.
    #[must_use]
    pub const fn kind(&self) -> StepKind {
        self.kind
    }

    /// The step name, the callee, or the bound, by kind.
    #[must_use]
    pub const fn subject(&self) -> &'static str {
        self.subject
    }

    /// The header as written.
    #[must_use]
    pub const fn detail(&self) -> &'static str {
        self.detail
    }

    /// The source line of the header.
    #[must_use]
    pub const fn line(&self) -> u32 {
        self.line
    }

    /// Blocks nested inside.
    #[must_use]
    pub const fn steps(&self) -> &'static [StepShape] {
        self.steps
    }
}

/// The kinds of block a flow is built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(crate = "lgwks_std::json::serde")]
#[non_exhaustive]
pub enum StepKind {
    /// `each x in xs, at most N at once:`; the subject is `N`.
    Each,
    /// `within D:`; the subject is `D`.
    Within,
    /// `retry up to N times[, waiting D]:`; the subject is `N`.
    Retry,
    /// `together:`; the subject is the branch count.
    Together,
    /// `step name:`; the subject is the name.
    Step,
    /// `for x in xs:`, sequential, one scope per item; the subject is its label.
    For,
    /// `run flow(..)`; the subject is the flow.
    Run,
    /// `if cond:`.
    If,
    /// `else:` or `else if cond:`.
    Else,
    /// `fail with ..` or `fail transiently with ..`.
    Fail,
    /// `give back ..`.
    GiveBack,
}
