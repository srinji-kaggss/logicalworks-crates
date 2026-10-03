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
//! | `await ready(&db, 30s)` | [`Readiness`] | a readiness guessed by sleeping; a poll loop; a stale instance releasing dependants |
//! | `admit the model's plan:` | [`admit`] | an untrusted payload becoming an instruction; a refusal with no location, no provenance or no ceiling |
//!
//! [`admit`] is the one way a task body crosses untrusted model or tool output
//! into a run. It enters its step, charges one run-scoped [`Gate`] — a decoder, a
//! surface, an admission budget and a repair ledger — turns a
//! [`proposal::Refusal`](crate::proposal::Refusal) or a
//! [`proposal::Intervention`](crate::proposal::Intervention) into its own
//! [`FlowError`] arm carrying the [`Provenance`](crate::proposal::Provenance)
//! of the exact bytes, and records the refusal through the run store so a run
//! resumed on a fresh host reads back what this run refused. It admits a
//! [`Plan`](crate::proposal::Plan) of operation *names*; it performs nothing and
//! is not a fifth verb.
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
//! would, with no `Arc`, no `clone`, and no `'static` bound. A flow is `Send`
//! exactly when what its steps hold is: bodies are stored as their own future
//! types, never erased, so a flow can be spawned onto a multi-threaded runtime
//! and each tenant's flow runs on whichever worker is free. The bodies of one
//! `each` share that one task. Concurrency is for waiting on many things at
//! once. CPU parallelism across cores is a different tool:
//! [`join_all_bounded`](crate::rt::task::join_all_bounded) on owned inputs.
//!
//! [`each`]: crate::script::each
//! [`within`]: crate::script::within
//! [`retry`]: crate::script::retry
//! [`admit`]: crate::script::admit
//! [`Gate`]: crate::script::Gate
//! [`FlowError`]: crate::script::FlowError
//! [`Scope`]: crate::script::Scope
//! [`Scope::enter`]: crate::script::Scope::enter
//! [`Tenant`]: crate::script::Tenant
//! [`StepKey`]: crate::script::StepKey
//! [`Architecture`]: crate::script::Architecture
//! [`Readiness`]: crate::script::Readiness

mod admit;
mod control;
mod each;
mod error;
mod map;
mod policy;
mod ready;
pub(crate) mod run_store;
pub(crate) mod scope;
pub(crate) mod trail;

use std::time::Duration;

// The clock is re-exported here rather than reached through `rt::clock` because
// this is the module that *consumes* it: a flow author writing `within` needs
// the type, and the two paths to it is one more name to keep straight. The
// underlying module stays the single definition.
use crate::rt::clock as rt_clock;

pub use admit::{
    Gate, RefusalRecord, admit, intervention_of, provenance_of, read_refusal, refusal_of,
    refusal_record,
};
pub use control::{at_most, attempts, retry, within, within_on};
pub use each::each;
pub use error::{FlowError, OptionExt, ResultExt};
pub use map::{Architecture, FlowShape, StepKind, StepShape};
pub use ready::{
    FailAfterReady, Generation, MAX_DEPENDANTS, MAX_NAME_BYTES, Readiness, ReadinessError, Ready,
    ReadyOutcome,
};
pub use rt_clock::Clock;
pub use run_store::{Appended, Durable, remember};
pub use scope::{Scope, StepKey, Tenant};

/// How many step paths a scope's trail retains unless a host says otherwise.
///
/// A reported bound rather than an implicit one: a run that entered thousands
/// of `each` items and retained them all would make the report the largest
/// object in the process, which is the opposite of what a report is for. See
/// [`crate::task::Report::steps`].
pub const DEFAULT_TRAIL_STEPS: usize = 64;

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
pub(super) const POLL_BUDGET: usize = 128;
