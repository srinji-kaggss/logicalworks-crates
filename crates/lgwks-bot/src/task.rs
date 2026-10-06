//! The front door: a [`Host`][host] runs a [`Task`][task_type] and returns a
//! [`Report`][report].
//!
//! A consumer supplies a typed task and its body and receives a useful result
//! without implementing orchestration. Nothing here requires a trait
//! implementation per workflow, a `Box`, a `Pin`, an `Arc`, a `'static`
//! coercion, or a `Send` bound on the author's side, and nothing here spawns,
//! reaps, polls or ticks on the caller's behalf.
//!
//! # Host once, task often
//!
//! The host is the installed owner: it holds the [`Tenant`][tenant], the root
//! [`CancellationToken`][stop], the admission budget, the default deadline and
//! the progress capacity. It is built once through a validated builder, its
//! ceilings are readable through [`Host::limits`][limits], and a clone of a host shares
//! the same budget rather than minting a second one — the budget belongs to the
//! host, not to the handle.
//!
//! ```no_run
//! use lgwks_bot::script::{FlowError, Scope};
//! use lgwks_bot::task::{Host, Report, task};
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let host = Host::builder("acme")?.build()?;
//! let greeting = task("greet", |_scope: Scope, name: &str| {
//!     // The borrowed input is read before the future is built, so the future
//!     // owns what it returns.
//!     let greeting = format!("hello, {name}");
//!     async move { Ok::<_, FlowError>(greeting) }
//! })?;
//!
//! let report: Report<String> = host.run(&greeting, "world").await;
//! assert_eq!(report.disposition().label(), "Succeeded");
//! assert_eq!(report.output().map(String::as_str), Some("hello, world"));
//! # Ok(())
//! # }
//! ```
//!
//! # Admission is a budget, and a wait is not a deadlock
//!
//! A host admits at most [`HostLimits::max_concurrent_tasks`][ceiling] top-level runs at
//! once. A run that arrives when the budget is spent **waits** for a permit
//! rather than being refused: the wait is cancellable by the host's token and
//! is charged to the run's deadline, so a caller that stops the host or whose
//! deadline passes learns that instead of hanging.
//!
//! A nested run — a task body calling `host.run` again — does **not** take a
//! second permit. A task-local set of host identities travels with the future,
//! so a run charged to a host its own future already holds a permit for waits
//! on nothing at all. That is what makes the interesting configuration work:
//! depth-four nesting at an admission ceiling of one completes, where a naive
//! permit-per-run would deadlock on the first child. Two *sibling* top-level
//! runs are separate futures with separate task-local scopes, so each takes its
//! own permit and the ceiling still bounds them.
//!
//! # A report never claims an external effect happened
//!
//! A local task performs no journaled effect, so every report carries
//! [`EffectKnowledge::None`][none]. The field exists so durable work can extend
//! the vocabulary later without changing the type a caller reads, and so no
//! caller can read a successful return as proof that a remote state changed.
//!
//! # Boundaries
//!
//! - The body's future need not be `Send`: it is polled on the calling task
//!   (local mode). Inputs may borrow (`I` may be `&[u8]`).
//! - The step trail is a bounded ring. Overflow loses the declared detail and
//!   increments a counter; it never touches the disposition, the output or the
//!   located error.
//! - [`Host::block_on`][sync_entry] owns a runtime once and **refuses** to run inside an
//!   existing runtime rather than parking a thread whose reactor the run needs.
//! - Parallelism across cores is a different tool
//!   ([`join_all_bounded`](crate::rt::task::join_all_bounded) on owned inputs).
//!   Here, concurrency is for waiting on many things at once.
//!
//! [tenant]: crate::script::Tenant
//! [stop]: crate::rt::sync::CancellationToken
//! [none]: crate::task::EffectKnowledge::None
//! [host]: crate::task::Host
//! [task_type]: crate::task::Task
//! [report]: crate::task::Report
//! [limits]: crate::task::Host::limits
//! [ceiling]: crate::task::HostLimits::max_concurrent_tasks
//! [sync_entry]: crate::task::Host::block_on

use std::fmt;
use std::future::Future;
use std::io;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use lgwks_std::hash::{Digest, Hasher};

use crate::cap::{Cap, Deficit, Demand, Shortage, uncovered};
use crate::effect::{InputIdentity, RunId};
use crate::gate::GrantSet;
use crate::journal::frame::SaturatingFrom;
use crate::journal::owner::lock;
use crate::rt::clock::Clock;
use crate::rt::runtime::{Handle, Runtime};
use crate::rt::sync::{CancellationToken, OwnedSemaphorePermit, Semaphore};
use crate::rt::task_local;
use crate::script::run_store::{
    Authority as RecordsAuthority, AuthorityCheck, Records, StoredValue,
};
use crate::script::scope::step_key as reserved_step_key;
use crate::script::trail::Trail;
use crate::script::{
    Appended, DEFAULT_TRAIL_STEPS, Durable, FlowError, MAX_IN_FLIGHT, Scope, Tenant, within,
};

use self::name::TaskName;
use self::request::{
    BINDING_STEP, TERMINAL_STEP, TerminalRecord, derive_run as derive_request_run, digest_of_record,
};

mod definition;
mod ledger;
mod name;
pub mod repair;
mod request;
mod store;

pub use definition::{DefinitionIdentity, Drift, UNVERSIONED_CODEC};
pub use ledger::{Control, LeaseRefusal, RunLedger};
pub use repair::{MAX_TICKET_NEEDS, RepairError, RepairTicket};
pub use request::{
    InFlight, InputDigest, MAX_REQUEST_KEY_BYTES, RequestConflict, RequestError, RequestKey,
    Submission,
};
pub use store::{
    MAX_RECORD_BYTES, MAX_RECORDS_PER_RUN, MAX_STORE_BYTES, RunStore, StoreError, StoreLimitKind,
};

/// The most top-level runs one host admits at once.
///
/// A ceiling of a million is a ceiling in name only, so the number a caller may
/// write is itself bounded: this is the same bound [`each`](crate::script::each)
/// places on one fan-out, and the machine-size rule behind it applies unchanged.
pub const MAX_ADMITTED_TASKS: usize = MAX_IN_FLIGHT;

/// The longest default deadline one host may declare.
///
/// A day. A ceiling rather than a policy: a caller who genuinely wants longer
/// has to say so in the open, and no host is accidentally configured to wait
/// forever on work nobody is watching.
pub const MAX_TASK_DEADLINE: Duration = Duration::from_secs(24 * 60 * 60);

/// The most step paths one host's progress trail retains.
///
/// A reported bound, not an implicit one. Overflow is visible in
/// [`Report::dropped_steps`] rather than silent.
pub const MAX_PROGRESS_STEPS: usize = MAX_IN_FLIGHT;

/// The step name a task's body runs under, inside the task's own step.
///
/// The deadline and the stop are located here, so a task that overruns reads
/// `review-pr/body: timed out after 30s` rather than an error with no path.
const BODY_STEP: &str = "body";

/// Hands out every host's admission identity.
///
/// A monotonic counter, not entropy, and the choice is deliberate. This
/// identity is process-local and never persisted: it exists so a task-local set
/// can say "this future already holds a permit for *that* host". A restart
/// cannot resurrect a stale set, so there is nothing to make unpredictable —
/// while `lgwks_std::random` is a feature-gated edge this module must not
/// assume (INV-DEP-6). Zero is never handed out, so it cannot be confused with
/// "no identity".
static HOST_IDENTITIES: AtomicU64 = AtomicU64::new(1);

task_local! {
    /// The host identities whose admission permit this future already holds.
    static HELD_PERMITS: Vec<u64>;
}

/// The identity one host's admission budget is charged under.
type HostIdentity = u64;

/// The failure a report names if it carries neither an output nor an error.
///
/// Built once, because [`FlowError`] has no `const` constructor. Reached only
/// by a report built outside this module, which no path does: it names the
/// defect in the value rather than panicking, and a panic here would turn a
/// broken invariant into an abort with no account of which run was affected.
fn own_fault() -> &'static FlowError {
    OWN_FAULT
        .get_or_init(|| FlowError::failed("a report with neither an output nor a located failure"))
}

/// The storage behind [`own_fault`].
static OWN_FAULT: OnceLock<FlowError> = OnceLock::new();

// ── Disposition ─────────────────────────────────────────────────────────────

/// How a run ended.
///
/// The disposition is the run's own terminal state; it is never inferred from
/// the output's shape, and it survives a step trail that overflowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Disposition {
    /// The body returned its value. It performs no journaled effect, so this
    /// says nothing about the state of anything outside the process.
    Succeeded,
    /// The body returned an error that was neither a stop nor a timeout.
    Failed,
    /// The run was admitted and then stopped: the host was cancelled, or the
    /// body cancelled its own scope.
    Cancelled,
    /// The run's deadline expired, either while it waited for a permit or
    /// while its body was running.
    DeadlineExceeded,
    /// The run was never admitted. The host was already cancelled, or was
    /// cancelled while the run waited for a permit, so no body ever ran.
    Refused,
    /// The run was admitted against the host's budget but refused before its body
    /// started: this host's grants do not cover what the task declared it needs.
    ///
    /// Distinct from [`Refused`](Self::Refused) because it says something
    /// different. A `Refused` run was refused by the *host* — cancelled, or never
    /// admitted — and no amount of authority would change it. A `Blocked` run is
    /// the host willing and the authority missing, which is why it is the only
    /// disposition a repair can move and the only one whose
    /// [`Report::repair`] is `Some`. Its body never ran, so nothing was polled and
    /// nothing was executed.
    Blocked,
}

impl Disposition {
    /// The name of this disposition, for logs and reports.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Succeeded => "Succeeded",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
            Self::DeadlineExceeded => "DeadlineExceeded",
            Self::Refused => "Refused",
            Self::Blocked => "Blocked",
        }
    }

    /// Whether the run's body ran to a value.
    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Succeeded)
    }

    /// Whether the run was admitted and its body started.
    #[must_use]
    pub const fn was_admitted(self) -> bool {
        !matches!(self, Self::Refused)
    }
}

/// The terminal record a submission of a request running under `report` writes,
/// or `None` when the run's disposition is the host declining to finish it.
///
/// Three of the five dispositions are **the request's own verdict**, and they
/// are the three the *declared task* produces: `Succeeded` because the body
/// returned, `Failed` because the body refused, and `DeadlineExceeded` because
/// the run's deadline is part of the task the client declared — the same
/// declaration that fixed the key also fixed the budget, so the budget running
/// out answers the request rather than interrupting it.
///
/// The other two are the **host declining to finish it**: `Cancelled` is a stop
/// that arrived after admission (the host's own token, or a scope the body
/// cancelled) and `Refused` is a refusal before admission (the host was already
/// stopping, or could not admit). Neither is evidence about the request, so
/// neither is recorded as its outcome. A key whose terminal record says
/// `Cancelled` is a key no later submission can ever complete: the key *is* the
/// request's identity and the body runs at most once under it, so recording the
/// host's stop there turns one restart into a permanently poisoned request. What
/// survives instead is the receipt and the durable step records, which is
/// exactly what a resume needs (INV-BOT-100, INV-BOT-101).
///
/// The match is exhaustive, so a `Disposition` added later breaks this build
/// until its relationship to a request key is decided by hand.
///
/// Enforced by `tests/request_key.rs`
/// (`a_host_stop_mid_run_leaves_the_request_resumable`,
/// `a_refusal_before_admission_is_not_recorded_as_the_outcome`,
/// `a_failed_run_is_the_requests_recorded_outcome`) and
/// `tests/sim_request_key.rs` (`host_stops_never_poison_a_key_band_00..03`).
///
/// # Errors
///
/// [`RequestError`] when a success's output cannot be archived or a record
/// cannot be encoded.
fn terminal_for<O: Durable>(report: &Report<O>) -> Result<Option<TerminalRecord>, RequestError> {
    Ok(match report.disposition() {
        Disposition::Succeeded => {
            let output = report
                .output()
                .ok_or(RequestError::Record(FlowError::failed(
                    "a successful run reported no output to record",
                )))?;
            Some(TerminalRecord::succeeded(output)?)
        }
        Disposition::Failed | Disposition::DeadlineExceeded => Some(TerminalRecord::stopped(
            report.disposition(),
            report.error().map_or("", FlowError::at),
            report.error().map_or_else(String::new, ToString::to_string),
        )),
        Disposition::Cancelled | Disposition::Refused | Disposition::Blocked => None,
    })
}

impl fmt::Display for Disposition {
    /// The [`Disposition::label`].
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

// ── EffectKnowledge ─────────────────────────────────────────────────────────

/// What a report knows about effects outside this process.
///
/// A local task journals nothing and performs no remote call, so it reports
/// [`None`] and there is no variant that could be read as proof a remote state
/// changed. Durable work adds its own variants; a caller written against this
/// one keeps compiling, because the enum is `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EffectKnowledge {
    /// No external effect is claimed. The run performed no journaled effect, so
    /// a successful return says what the body returned and nothing more.
    None,
    /// The run's steps that called [`remember`](crate::script::remember) are
    /// recorded durably under the run's [`RunId`], so a resume replays them
    /// without re-running them.
    ///
    /// This is a statement about **this run's own step records**, not about any
    /// remote state. A step that performs an external effect still needs the
    /// effect journal, and a step whose record failed to land still re-runs — see
    /// [`Report::ticket`] for the step a run stopped at.
    StepRecords {
        /// How many records this run committed over its whole life, including
        /// the ones a previous attempt committed and this one replayed.
        records: usize,
    },
}

// ── Task ─────────────────────────────────────────────────────────────────────

/// A typed unit of work: a validated logical name and an async body.
///
/// One type parameter, so a body is stored as its own future type and never
/// erased: a task is `Send` exactly when its body is, and no allocation or
/// boxing appears in the author's code. The body takes the run's [`Scope`] and
/// the input, and returns the typed output or a located [`FlowError`].
///
/// A name is a label, not an identity: it locates steps and keys them through
/// the scope, and it is not a secret, a grant, a replay key or evidence of
/// identity by itself (DX-02).
pub struct Task<F> {
    /// The validated logical name, which is also the run's first step path.
    name: TaskName,
    /// The author's body.
    body: F,
    /// A task needs nothing by default: a body that reaches nothing declares nothing.
    ///
    /// A step that does reach something asks for it at the step that reaches,
    /// through [`Scope::require`]. That is the step
    /// that knows what the step is about to do, and it is where a repair is
    /// relevant — a run that has already analyzed something and then finds the
    /// publication blocked is the case a repair exists for, and a whole-task
    /// declaration would have refused it before the analysis ran.
    needs: Vec<Cap>,
}

impl<F> Task<F> {
    /// The task's validated name.
    #[must_use]
    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    /// The capabilities this task declares at its own admission boundary.
    ///
    /// Empty for every task that reaches nothing, which is the default and the
    /// honest answer: a task that reaches something declares it at the step that
    /// reaches rather than here, so the whole shortfall is reported where it is
    /// discovered — after the work already done, and no earlier.
    #[must_use]
    pub fn needs(&self) -> &[Cap] {
        &self.needs
    }

    /// Declare capabilities this task needs before its body runs at all.
    ///
    /// The blunt form of [`Scope::require`]: the
    /// whole task is refused at admission if any of `caps` is uncovered, so
    /// nothing runs and nothing is recorded. Use it for a task that reaches
    /// something in its very first step; use `require` in the body when the reach
    /// happens later, so the work before it survives to be replayed.
    #[must_use]
    pub fn requiring(mut self, caps: &[Cap]) -> Self {
        self.needs = caps.to_vec();
        self
    }

    /// A copy of the task's name, so a run can stamp its report without
    /// borrowing the task across its whole body.
    pub(crate) fn name_owned(&self) -> TaskName {
        self.name.clone()
    }
}

impl<F: fmt::Debug> fmt::Debug for Task<F> {
    /// The name, the needs and the body, so a task reads as what it is in a log line.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Task")
            .field("name", &self.name.as_str())
            .field("needs", &self.needs)
            .field("body", &self.body)
            .finish()
    }
}

/// Declare a task called `name` whose body is `body`.
///
/// The name is validated once, here: 1 to
/// [`MAX_TASK_NAME_BYTES`](crate::script::MAX_TENANT_BYTES) bytes of ASCII
/// letters, digits and `-`, `_`, `.`, `:`, which keeps it safe in a path, a log
/// line and a file name. Everything else about the task — the deadline, the
/// admission budget, the tenant — is the host's business, resolved from its
/// profile rather than from the task.
///
/// # Errors
///
/// [`FlowError::InvalidName`] naming what is wrong with `name`.
///
/// # Example
///
/// ```
/// use lgwks_bot::script::{FlowError, Scope};
/// use lgwks_bot::task::task;
///
/// # fn main() -> Result<(), FlowError> {
/// let sum = task("sum", |_scope: Scope, values: Vec<u32>| async move {
///     Ok::<_, FlowError>(values.iter().copied().sum::<u32>())
/// })?;
/// assert_eq!(sum.name(), "sum");
///
/// let refused = task("", |_scope: Scope, ()| async { Ok::<_, FlowError>(()) });
/// assert!(refused.is_err(), "an empty logical name is refused");
/// # Ok(())
/// # }
/// ```
pub fn task<F>(name: &str, body: F) -> Result<Task<F>, FlowError> {
    Ok(Task {
        name: TaskName::new(name)?,
        body,
        needs: Vec::new(),
    })
}

// ── Report ───────────────────────────────────────────────────────────────────

/// What one run produced: disposition, typed output, located failure, elapsed
/// time, and the bounded step trail.
///
/// The report separates the facts a caller actually needs, so no one of them is
/// inferred from another: a success is a disposition *and* an output, a failure
/// is a disposition *and* a located error, and elapsed time is measured rather
/// than derived. Overflow in the trail is counted rather than hidden, and the
/// terminal state never depends on it.
#[derive(Debug)]
pub struct Report<O> {
    /// How the run ended.
    disposition: Disposition,
    /// The typed value, present exactly when the disposition is
    /// [`Disposition::Succeeded`].
    output: Option<O>,
    /// The located failure. Present for every disposition except
    /// [`Disposition::Succeeded`].
    error: Option<FlowError>,
    /// Wall time from admission to terminal state, measured by this host.
    elapsed: Duration,
    /// The retained step paths, oldest first.
    steps: Vec<Arc<str>>,
    /// How many paths the ring dropped to stay bounded.
    dropped_steps: usize,
    /// How many paths this report could retain.
    progress_capacity: usize,
    /// What this run knows about effects outside the process.
    effects: EffectKnowledge,
    /// The tenant the run was for.
    tenant: Tenant,
    /// The task's name.
    task: TaskName,
    /// The run this report's step records are keyed by, when a store was
    /// installed. `None` for a run whose host holds only in-memory sinks, which
    /// is exactly the run that can never be resumed.
    run: Option<RunId>,
    /// Where a stopped run stopped, for a caller that wants to resume it.
    ticket: Option<Ticket>,
    /// Every unmet capability this run was admitted short of, as one shortfall.
    ///
    /// The complete answer, not the first: a run needing three capabilities this
    /// host grants none of reports all three, so the repair is written once
    /// against the whole shortfall instead of discovering it one refusal at a
    /// time.
    needs: Option<Deficit>,
    /// The repair a blocked run hands back to whoever holds the authority.
    ///
    /// `Some` exactly when `needs` is `Some` and the run is repairable — which
    /// means the host has a repair ledger to decide the repair against. A blocked
    /// run on a host with no ledger names its needs and carries no ticket, because
    /// there is nothing a ticket could be decided against.
    repair: Option<RepairTicket>,
}

impl<O> Report<O> {
    /// How the run ended.
    #[must_use]
    pub const fn disposition(&self) -> Disposition {
        self.disposition
    }

    /// The typed output, borrowed. `None` unless the run succeeded.
    #[must_use]
    pub fn output(&self) -> Option<&O> {
        self.output.as_ref()
    }

    /// The located failure, borrowed. `None` when the run succeeded.
    #[must_use]
    pub const fn error(&self) -> Option<&FlowError> {
        self.error.as_ref()
    }

    /// The output or the located failure, both borrowed.
    ///
    /// `Ok` exactly when the disposition is [`Disposition::Succeeded`].
    ///
    /// # Errors
    ///
    /// The run's located [`FlowError`].
    pub fn result(&self) -> Result<&O, &FlowError> {
        match (self.output.as_ref(), self.error.as_ref()) {
            (Some(output), _) if self.disposition.is_success() => Ok(output),
            (_, Some(error)) => Err(error),
            // Not reachable: every report below is built with the output and
            // the error agreeing with the disposition, and there is no other
            // constructor. The arm exists because a fabricated success is the
            // one thing a report must never be, and this module hands out
            // exactly one kind of report.
            _ => Err(own_fault()),
        }
    }

    /// The typed output or the located failure, by value.
    ///
    /// # Errors
    ///
    /// The run's [`FlowError`], located at the step that failed.
    pub fn into_result(self) -> Result<O, FlowError> {
        let (output, error) = (self.output, self.error);
        match (output, error) {
            (Some(output), _) if self.disposition.is_success() => Ok(output),
            (_, Some(error)) => Err(error),
            _ => Err(FlowError::failed(
                "a report with neither an output nor a located failure",
            )),
        }
    }

    /// The typed output by value, or `None` for any other disposition.
    #[must_use]
    pub fn into_output(self) -> Option<O> {
        self.output
    }

    /// How long the run took, measured by this host.
    #[must_use]
    pub const fn elapsed(&self) -> Duration {
        self.elapsed
    }

    /// The retained step paths, oldest first.
    ///
    /// The **last** [`Report::progress_capacity`] paths entered, so a
    /// truncated trail still ends where the run stopped. How much is missing is
    /// [`Report::dropped_steps`].
    #[must_use]
    pub fn steps(&self) -> &[Arc<str>] {
        &self.steps
    }

    /// How many step paths the ring dropped to stay within its capacity.
    ///
    /// Non-zero means the trail is a suffix of the run, not the whole run. It
    /// never means the disposition or the output are uncertain.
    #[must_use]
    pub const fn dropped_steps(&self) -> usize {
        self.dropped_steps
    }

    /// How many step paths this report can retain.
    #[must_use]
    pub const fn progress_capacity(&self) -> usize {
        self.progress_capacity
    }

    /// What this run knows about effects outside the process.
    ///
    /// [`EffectKnowledge::None`] for every local task: no journaled effect, so
    /// no claim a remote state changed.
    #[must_use]
    pub const fn effects(&self) -> EffectKnowledge {
        self.effects
    }

    /// The tenant this run was admitted for.
    ///
    /// Fixed at admission and not a property of the work, so a caller charging
    /// the run reads it here rather than tracking which tenant's flow it came
    /// from.
    #[must_use]
    pub fn tenant(&self) -> &Tenant {
        &self.tenant
    }

    /// The declared name of the task this run executed.
    ///
    /// Borrowed from the run rather than returned, so it cannot outlive the run
    /// it names. Two runs of the same task share it, and a task name says nothing
    /// about which tenant it ran for.
    #[must_use]
    pub fn task_name(&self) -> &str {
        self.task.as_str()
    }

    /// The path of the step this run ended in, when it ended in one.
    ///
    /// The failure's own location, which is the step that failed rather than the
    /// last step entered.
    #[must_use]
    pub fn ended_at(&self) -> Option<&str> {
        self.error
            .as_ref()
            .map(FlowError::at)
            .filter(|path| !path.is_empty())
    }

    /// The run this report's records are keyed by.
    ///
    /// `Some` exactly when the host had a store installed, and `None` otherwise.
    /// A `None` here is the whole honest statement of "this run kept nothing":
    /// there is no in-memory sink pretending to be durable, and no ticket,
    /// because there is nothing to resume from.
    #[must_use]
    pub const fn run_id(&self) -> Option<RunId> {
        self.run
    }

    /// What a caller needs to resume this run.
    ///
    /// `Some` when the run has a store, ended in anything other than
    /// [`Disposition::Succeeded`], and did not reach its own body (so the failure
    /// has no step path to name). A run that failed *inside* a step has nothing
    /// to resume to — the step re-runs under the same run id when the caller
    /// calls [`Host::resume`] — so it carries no ticket and the caller uses
    /// `run_id` directly.
    #[must_use]
    pub const fn ticket(&self) -> Option<&Ticket> {
        self.ticket.as_ref()
    }

    /// Every capability this run was admitted short of, in one shortfall.
    ///
    /// `Some` only for a [`Disposition::Blocked`] run, and then it carries **every**
    /// unmet requirement rather than the first: the repair ticket below is derived
    /// from exactly this value, so a caller reading it knows what the whole repair
    /// must cover. A run that was admitted needs nothing and reports `None`.
    #[must_use]
    pub const fn needs(&self) -> Option<&Deficit> {
        self.needs.as_ref()
    }

    /// The repair a blocked run hands back, naming the run, the tenant, the epoch
    /// it was minted at, and exactly the capabilities it needs.
    ///
    /// `Some` exactly when [`Report::needs`] is `Some` *and* this host has a repair
    /// ledger to decide a repair against. A blocked run on a host with no ledger
    /// names its needs and carries no ticket: a ticket whose budget and epoch
    /// nothing could be checked against would be an authority a caller could apply
    /// unchecked.
    #[must_use]
    pub const fn repair(&self) -> Option<&RepairTicket> {
        self.repair.as_ref()
    }
}

// ── Ticket ──────────────────────────────────────────────────────────────────

/// Everything needed to resume one stopped run.
///
/// A ticket is not a promise that resuming will succeed: the step it stopped at
/// may fail again, and the store may have lost its last record. It is a
/// statement about identity — *this* run, for *this* tenant, which stopped *here*
/// — and [`Host::resume`] takes exactly that. Holding one across a process
/// restart is the point; a run id alone would leave a caller to guess which
/// tenant owns it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ticket {
    /// The run to resume.
    run: RunId,
    /// The tenant that owns it, and the only tenant that may resume it.
    tenant: Tenant,
    /// The task that was running.
    task: TaskName,
    /// How it ended, so a reader knows whether work was interrupted or refused.
    disposition: Disposition,
    /// The step path it stopped in, when the failure had one.
    at: Option<Arc<str>>,
}

impl Ticket {
    /// The run this ticket resumes.
    #[must_use]
    pub const fn run(&self) -> RunId {
        self.run
    }

    /// The tenant that owns the run.
    #[must_use]
    pub fn tenant(&self) -> &Tenant {
        &self.tenant
    }

    /// The identity of the task whose run this outcome describes, as the caller submitted it.
    #[must_use]
    pub fn task(&self) -> &str {
        self.task.as_str()
    }

    /// How that run ended.
    #[must_use]
    pub const fn disposition(&self) -> Disposition {
        self.disposition
    }

    /// The step path the run stopped in, when the failure had one.
    #[must_use]
    pub fn stopped_at(&self) -> Option<&str> {
        self.at.as_deref()
    }
}

impl fmt::Display for Ticket {
    /// `<tenant>/<run>: <disposition> at <path>` — one line a log or a resume
    /// queue can carry.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}/{}: {}",
            self.tenant.as_str(),
            self.run.id().to_hex(),
            self.disposition.label()
        )?;
        if let Some(at) = self.at.as_deref() {
            write!(formatter, " at {at}")?;
        }
        Ok(())
    }
}

/// The receipt a claim just lost to, read back from the store.
///
/// A claim that reports another submitter's record already on the disk and a
/// lookup that then finds nothing contradict each other, so that is an
/// [`RequestError::IdCollision`] rather than a missing value.
fn recorded_receipt(
    records: &Records,
    tenant: &str,
    run: RunId,
    binding_key: crate::script::StepKey,
) -> Result<StoredValue, RequestError> {
    match records.lookup(tenant, run, binding_key)? {
        Some(held) => Ok(held),
        None => {
            let refusal = Err(RequestError::IdCollision);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "recorded_receipt: the claim reported a record the lookup cannot find");
            refusal
        }
    }
}

// ── Existing ────────────────────────────────────────────────────────────────

/// A request whose receipt is already on the disk, gathered so the reattach
/// logic takes one borrowed value rather than eight positional arguments.
struct Existing<'a> {
    /// The host's store, for the run's record count.
    store: &'a RunStore,
    /// The receipt's host records handle.
    records: &'a Records,
    /// The tenant this host runs for.
    tenant: &'a str,
    /// The derived run identity.
    run: RunId,
    /// The caller's request key.
    key: &'a RequestKey,
    /// The stored receipt record.
    held: &'a StoredValue,
    /// The caller's input digest.
    digest: InputDigest,
}

// ── Host ─────────────────────────────────────────────────────────────────────

/// The installed owner of execution and policy for one tenant.
///
/// Build it once, keep it, and run many tasks through it: the tenant, the stop
/// token, the admission budget, the default deadline and the progress capacity
/// are resolved here rather than per call site, and they are readable through
/// [`Host::limits`] and [`Host::admission`].
///
/// Cloning is a handle, not an installation. A clone shares the same admission
/// budget and the same counters, so a task body that holds a clone cannot raise
/// the ceiling it is bounded by.
///
/// # Example
///
/// ```
/// use lgwks_bot::task::Host;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let host = Host::builder("acme")?
///     .max_concurrent_tasks(std::num::NonZeroUsize::new(8).ok_or("a non-zero ceiling")?)
///     .default_deadline(std::time::Duration::from_secs(5))
///     .build()?;
///
/// assert_eq!(host.tenant().as_str(), "acme");
/// assert_eq!(host.limits().max_concurrent_tasks(), 8);
/// assert_eq!(
///     host.limits().default_deadline(),
///     std::time::Duration::from_secs(5)
/// );
/// assert_eq!(host.admission().available_permits(), 8);
///
/// let same_budget = host.clone();
/// assert_eq!(
///     same_budget.admission().available_permits(),
///     8,
///     "a clone shares the budget rather than minting one"
/// );
///
/// host.cancel();
/// assert!(host.is_cancelled());
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Host {
    /// Everything the handles of one host share: the budget, the counters, the
    /// tenant, the stop token, the ceilings, and the lazily built reactor.
    inner: Arc<Installation>,
}

/// The state one host owns, shared by every clone of its handles.
struct Installation {
    /// This host's admission identity, unique per host for the life of the
    /// process. Nested-run accounting is keyed on it.
    identity: HostIdentity,
    /// Who this host runs for.
    tenant: Tenant,
    /// Cancelling this stops every run under the host, in flight or waiting.
    token: CancellationToken,
    /// The finite ceilings this host resolves unspecified values from.
    limits: HostLimits,
    /// The admission budget: at most `limits.max_concurrent_tasks()` top-level
    /// runs hold a permit at once. Shared with its clones by the `Arc` that
    /// owns the installation, and cloned here for [`Semaphore::acquire_owned`],
    /// which is what lets a run release its permit from any exit path.
    admission: Arc<Semaphore>,
    /// Top-level runs holding a permit right now. Counting them here rather than
    /// in a guard keeps the count correct when a run's future is dropped while
    /// suspended.
    in_flight: LiveRuns,
    /// The most top-level runs that ever held a permit at once.
    peak_in_flight: HighWater,
    /// Every run admitted since the host was built, nested runs included.
    admitted: AtomicU64,
    /// Every run refused at admission.
    refused: AtomicU64,
    /// The clock every run's deadline is measured on.
    ///
    /// Read by the admission wait and handed to each run's root scope, so one
    /// timeline governs the whole host rather than each step choosing its own.
    clock: Clock,
    /// The durable step-record store, when one was installed.
    ///
    /// `None` is the honest local mode: no record survives the process, so no
    /// report claims a run id and no ticket exists. A host with a store in hand
    /// that behaved as though it had none would be claiming durability it does
    /// not have, which is the one thing INV-BOT-54 forbids.
    store: Option<store::RunStore>,
    /// The declared schema id of this host's durable step values.
    ///
    /// What a [`DefinitionIdentity`] this host builds by hand starts from, so the
    /// declared schema is one setting rather than one argument per call. The
    /// default is [`UNVERSIONED_CODEC`], which says the caller declared none
    /// rather than that the values are schema-free.
    codec: String,
    /// The durable repair ledger, when one was installed.
    ///
    /// `None` means this host cannot repair anything: a repair is decided
    /// against a run's epoch, its root budget and the set of tickets already
    /// applied to it, and a host with no ledger has none of those. Such a host
    /// refuses every repair rather than applying one unchecked, which is the
    /// difference between a repair and a widened authority.
    ledger: Option<RunLedger>,
    /// The capabilities this host's grant set carries.
    ///
    /// The base authority every run is admitted against, before any repair. A
    /// repair never widens this: it authorizes one run, once, for the needs its
    /// ticket named, and the next run on this host is admitted against the same
    /// grant.
    grants: GrantSet,
    /// The most root attempts one run may be charged before it is refused.
    repair_attempts: u64,
    /// The most root spend one run may be charged before it is refused.
    repair_spend: u64,
    /// The runtime [`Host::block_on`] drives on, built once on first use and
    /// owned for the host's lifetime: a runtime whose last owner drops it shuts
    /// down and drops every task still running on it, so holding it here is what
    /// lets a second [`Host::block_on`] drive work on the same reactor. Each call
    /// takes a fresh handle from it rather than a stored copy, so no handle can
    /// outlive the runtime it points at.
    reactor: Mutex<Option<Runtime>>,
}

/// What one call to [`Host::execute`] is asking for.
///
/// A run id and a definition identity are two different facts about a run — the
/// first says which records to look under, the second says what they have to
/// have been written under — and pairing them by enumerating four door
/// combinations is what made a drift check easy to apply to one door and forget
/// on another. One sum type carries both, and the identity a caller did not
/// declare is derived here rather than at each door.
#[derive(Debug, Clone)]
enum Plan {
    /// A new run, with or without a declared identity.
    Fresh,
    /// A fresh run under `DefinitionIdentity`.
    Declared(DefinitionIdentity),
    /// A resume of `RunId`, under whatever identity this host derives.
    Resume(RunId),
    /// A resume of `RunId` under `DefinitionIdentity`.
    ResumeUnder(RunId, DefinitionIdentity),
}

impl Plan {
    /// The identity the caller declared, or `None` when it declared none.
    ///
    /// The distinction the drift check turns on. A declared identity is a claim
    /// about what the run *is*, so a resume that contradicts it is refused. An
    /// undeclared one is the host's own derivation from the run id, which is by
    /// construction the identity that run was recorded under — comparing it would
    /// compare a run's records against themselves and fence nothing, while
    /// refusing would break every durable step that never declared anything.
    fn declared(&self) -> Option<&DefinitionIdentity> {
        // Each arm binds through a reference, because matching `self` — itself a
        // reference — against a non-`Copy` payload is the one case this workspace's
        // `pattern_type_mismatch` rule exists for.
        match self {
            &Self::Declared(ref identity) | &Self::ResumeUnder(_, ref identity) => Some(identity),
            &Self::Fresh | &Self::Resume(_) => None,
        }
    }
}

impl Plan {
    /// The run this plan adopts, or `None` for one that mints its own.
    const fn run(&self) -> Option<RunId> {
        match *self {
            Self::Fresh | Self::Declared(_) => None,
            Self::Resume(run) | Self::ResumeUnder(run, _) => Some(run),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Plan;
    use crate::effect::RunId;
    use crate::task::{Host, name::TaskName};

    /// What this module's tests report.
    ///
    /// A `Result` rather than a panic because the workspace forbids the
    /// panicking path everywhere, test code included: a unit test that unwound
    /// would report a line number inside a macro rather than which of the facts
    /// it was checking had moved.
    type TestResult = Result<(), String>;

    /// A run identity built from a literal, not from the entropy source.
    ///
    /// [`RunId::mint`] is behind the `ephemeral` feature and `ephemeral` is not a
    /// default feature, so a unit test that minted one compiled under
    /// `--all-features` and failed the workspace-default build — which is the
    /// build `cargo nextest list --workspace` performs, and therefore a test that
    /// could not be run at all on the configuration CI measures coverage over. What
    /// these tests need is *a* run id and *two* distinct ones; none of them
    /// asserts anything about how a run id is minted, which is
    /// `effect::RunId::mint`'s own contract and is exercised where that feature is
    /// on. Parsing a fixed literal is therefore the honest fixture: it needs no
    /// feature and no randomness, so the same two identities are the same two
    /// identities on every run of the suite.
    fn run_id(nibble: char) -> Result<RunId, String> {
        // A 32-character lowercase hex literal, which is the one spelling
        // `RunId::from_hex` accepts, and one distinct nibble per call so two
        // draws cannot collide and quietly make a "distinct identities" assertion
        // pass on a single value.
        let literal = format!("{nibble}{}", "0".repeat(31));
        RunId::from_hex(&literal).map_err(|error| error.to_string())
    }

    /// Every plan arm reports the run it adopts, and only the resume arms report one.
    ///
    /// The property the drift check's placement rests on: a fresh plan has no run
    /// to be incompatible with, and both resume arms have one whether or not they
    /// declared an identity.
    #[test]
    fn only_the_resume_arms_adopt_a_run() -> TestResult {
        let host = host()?;
        let task = TaskName::new("probe").map_err(|error| error.to_string())?;
        let run = run_id('1')?;
        let identity = host.definition_for(task.as_str(), None);
        assert!(Plan::Fresh.run().is_none(), "a fresh plan adopts nothing");
        assert!(
            Plan::Declared(identity.clone()).run().is_none(),
            "a declared fresh plan adopts nothing either"
        );
        assert_eq!(
            Plan::Resume(run).run(),
            Some(run),
            "a resume adopts its run"
        );
        assert_eq!(
            Plan::ResumeUnder(run, identity).run(),
            Some(run),
            "a resume under a declared identity adopts the same run"
        );
        Ok(())
    }

    /// The derived identity is stable for one run and different for another.
    ///
    /// What makes it usable as a default at all: a derived identity that moved
    /// between two attempts at the same run would make every existing durable
    /// step refuse its own resume, and one that was the same for every run would
    /// make two runs claim one definition.
    #[test]
    fn a_derived_identity_is_stable_per_run_and_distinct_across_runs() -> TestResult {
        let host = host()?;
        let first = run_id('1')?;
        let second = run_id('2')?;
        assert_eq!(
            host.definition_for("probe", Some(&first)).digest(),
            host.definition_for("probe", Some(&first)).digest(),
            "one run's derived identity must not move between two readings"
        );
        assert_ne!(
            host.definition_for("probe", Some(&first)).digest(),
            host.definition_for("probe", Some(&second)).digest(),
            "two runs must not share a derived identity"
        );
        assert_ne!(
            host.definition_for("probe", None).digest(),
            host.definition_for("probe", Some(&first)).digest(),
            "a run that has not adopted an id is not the run that has"
        );
        Ok(())
    }

    /// A host this module's tests can always build.
    fn host() -> Result<Host, String> {
        Host::builder("acme")
            .map_err(|error| error.to_string())?
            .build()
            .map_err(|error| error.to_string())
    }
}

impl Host {
    /// Begin building a host for `tenant`.
    ///
    /// The tenant is validated by [`Tenant::new`], so a name that would corrupt
    /// every step key and every error location is refused here rather than at
    /// the first run.
    ///
    /// # Errors
    ///
    /// [`FlowError::InvalidTenant`] naming what is wrong with `tenant`.
    pub fn builder(tenant: &str) -> Result<HostBuilder, FlowError> {
        Ok(HostBuilder {
            tenant: Tenant::new(tenant)?,
            max_concurrent: default_max_concurrent(),
            deadline: default_deadline(),
            progress: DEFAULT_TRAIL_STEPS,
            clock: Clock::wall(),
            store: None,
            codec: UNVERSIONED_CODEC.to_owned(),
            ledger: None,
            grants: GrantSet::empty(),
            repair_attempts: default_repair_attempts(),
            repair_spend: default_repair_spend(),
        })
    }

    /// The tenant this host runs for.
    #[must_use]
    pub fn tenant(&self) -> &Tenant {
        &self.inner.tenant
    }

    /// The finite ceilings this host resolves unspecified values from.
    ///
    /// Every field is finite, and every field is readable: a ceiling a caller
    /// cannot see is a ceiling they cannot reason about (DX-03).
    #[must_use]
    pub fn limits(&self) -> &HostLimits {
        &self.inner.limits
    }

    /// The clock every run's deadline is measured on.
    ///
    /// Read by the admission wait and by each run's root scope, so a caller can
    /// ask which timeline governs a refusal instead of assuming real time. The
    /// borrow cannot move it; a caller that holds the [`Clock`] it built the host
    /// with advances it directly.
    #[must_use]
    pub fn clock(&self) -> &Clock {
        &self.inner.clock
    }

    /// This host's live admission counters.
    #[must_use]
    pub fn admission(&self) -> Admission<'_> {
        Admission { inner: &self.inner }
    }

    /// The token that stops every run under this host.
    #[must_use]
    pub fn token(&self) -> &CancellationToken {
        &self.inner.token
    }

    /// The durable step-record store this host installed, when it has one.
    ///
    /// `None` is a host that keeps nothing across a process boundary, and every
    /// report it produces says so. The handle is a clone, so a caller that wants
    /// to inspect the store from a different task reads the same index rather
    /// than opening a second reader over the same file.
    #[must_use]
    pub fn run_store(&self) -> Option<&store::RunStore> {
        self.inner.store.as_ref()
    }

    /// The identity the run's records are already under, or `declared` when this
    /// call is the run's first attempt.
    ///
    /// One read, and every durable step under the run gets the same answer, so a
    /// resume cannot check its definition against one run and write its records
    /// under another. `None` means nothing is recorded yet, which is the first
    /// attempt's situation and not a compatibility.
    fn identity_for(&self, run: RunId, declared: &DefinitionIdentity) -> DefinitionIdentity {
        match self
            .inner
            .store
            .as_ref()
            .and_then(|store| store.definition_of(run))
        {
            Some(recorded) => recorded,
            None => declared.clone(),
        }
    }

    /// The identity a run adopts when its caller declared none.
    ///
    /// Derived from the three facts every run has — its tenant, its task name and
    /// the run id it adopted — so it is stable across attempts at one run and
    /// distinct across runs. A run that has not adopted one yet reads as fresh,
    /// which is the same derivation with the third fact set to `fresh`, and a
    /// caller that declares its own identity overrides this entirely.
    fn definition_for(&self, task_name: &str, run: Option<&RunId>) -> DefinitionIdentity {
        let mut hasher = Hasher::new();
        hasher.write_framed(b"lgwks.bot.definition.host-default");
        hasher.write_framed(self.inner.tenant.as_str().as_bytes());
        hasher.write_framed(task_name.as_bytes());
        match run {
            Some(run) => hasher.write_framed(run.id().to_hex().as_bytes()),
            None => hasher.write_framed(b"fresh"),
        };
        DefinitionIdentity::new(task_name, 0, hasher.finalize(), 1).with_codec(&self.inner.codec)
    }

    /// The definition identity every run under this host records against.
    ///
    /// The caller's half of what a resume has to agree with: the task is named
    /// here because the host knows which task it is about to run, the revision and
    /// the step count are the two facts the caller declares, and the input digest
    /// is the identity [`crate::effect::InputIdentity`] names. The schema id is
    /// the host's [`HostBuilder::durable_codec`], so the declared schema is one
    /// setting rather than one argument per run.
    ///
    /// A `None` input digest means the caller did not declare one, which is not
    /// the same as declaring some particular digest: an undeclared identity is
    /// recorded as such and compares equal to another undeclared one, so two runs
    /// that pass different inputs under it still resume — and the caller who needs
    /// that refused declares the digest.
    ///
    /// # Example
    ///
    /// ```
    /// use lgwks_bot::effect::InputIdentity;
    /// use lgwks_bot::task::Host;
    /// use lgwks_std::hash::Hasher;
    ///
    /// # fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// let host = Host::builder("acme")?.build()?;
    /// let mut hasher = Hasher::new();
    /// 7u32.write_identity(&mut hasher);
    /// let identity = host.definition("nightly", 3, Some(hasher.finalize()), 5);
    /// assert_eq!(identity.revision(), 3);
    /// assert_eq!(identity.steps(), 5);
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn definition(
        &self,
        task_name: &str,
        definition_revision: u64,
        input_digest: Option<Digest>,
        steps: usize,
    ) -> DefinitionIdentity {
        // An undeclared input is named by the codec it would be encoded with,
        // so two hosts that encode differently never share an identity.
        let input = match input_digest {
            Some(declared) => declared,
            None => {
                let mut hasher = Hasher::new();
                hasher.write_framed(b"lgwks.bot.input.undeclared");
                hasher.write_framed(self.inner.codec.as_bytes());
                hasher.finalize()
            }
        };
        DefinitionIdentity::new(task_name, definition_revision, input, steps)
            .with_codec(&self.inner.codec)
    }

    /// The durable repair ledger this host installed, when it has one.
    ///
    /// `None` is a host that cannot repair a blocked run: with no ledger there is
    /// no epoch, no root budget and no record of applied tickets, so
    /// [`Host::repair`] refuses rather than authorizing against nothing.
    #[must_use]
    pub fn run_ledger(&self) -> Option<&RunLedger> {
        self.inner.ledger.as_ref()
    }

    /// The capabilities this host admits runs against.
    ///
    /// Read-only by borrow: the grant set is the host's authority and a repair
    /// never widens it, so a caller that could hand it back could authorize a run
    /// this host was never configured to run.
    #[must_use]
    pub fn grants(&self) -> &GrantSet {
        &self.inner.grants
    }

    /// The root budget bounds this host charges a run's attempts against.
    #[must_use]
    pub fn repair_bounds(&self) -> (u64, u64) {
        (self.inner.repair_attempts, self.inner.repair_spend)
    }

    /// Stop every run under this host, including one waiting for a permit, and
    /// refuse every run started afterwards.
    ///
    /// Idempotent: a second call does nothing, and the token observes the change
    /// immediately on every thread.
    pub fn cancel(&self) {
        self.inner.token.cancel();
    }

    /// Whether this host, or a token it descends from, has been stopped.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.inner.token.is_cancelled()
    }

    /// Run `task` over `input` and report what happened.
    ///
    /// The body is polled on the calling task (local mode), so it need not be
    /// `Send` and `input` may borrow. The run happens under the child scope
    /// `tenant/<task name>`, under the host's stop token, and under the host's
    /// default deadline applied with [`within`] — so an overrun is located at
    /// `<task>/body` and the body is dropped rather than left running.
    ///
    /// # Admission
    ///
    /// A top-level run waits for one of this host's permits, cancellably and
    /// within its deadline. A run whose future already holds a permit for *this*
    /// host — a nested [`Host::run`] inside a task body — is charged to that
    /// permit and waits on nothing, which is why nesting completes at a ceiling
    /// of one. Sibling top-level runs are separate futures, take separate
    /// permits, and are bounded by
    /// [`HostLimits::max_concurrent_tasks`].
    pub async fn run<I, O, F, Fut>(&self, task: &Task<F>, input: I) -> Report<O>
    where
        F: Fn(Scope, I) -> Fut,
        Fut: Future<Output = Result<O, FlowError>>,
    {
        self.run_plan(task, input, Plan::Fresh).await
    }

    /// The one call site for a door that carries no repair: no ticket and the
    /// host's default root budget, so the public doors differ only in the
    /// [`Plan`] they name.
    async fn run_plan<I, O, F, Fut>(&self, task: &Task<F>, input: I, plan: Plan) -> Report<O>
    where
        F: Fn(Scope, I) -> Fut,
        Fut: Future<Output = Result<O, FlowError>>,
    {
        self.execute(task, input, plan, None, &GrantSet::empty(), 1)
            .await
    }

    /// [`Host::run`] under a declared definition identity.
    ///
    /// The door a durable caller uses. The identity is recorded with the run's
    /// first record, so every later [`Host::resume_under`] of that run either
    /// agrees with it exactly or is refused before any step body is polled — which
    /// is what stops an edited task, a changed input or a changed value schema
    /// from replaying values the current definition never produced. A host with
    /// no store runs the body and records nothing, so the identity is only a claim
    /// when a store exists to check it against.
    pub async fn run_under<I, O, F, Fut>(
        &self,
        definition: &DefinitionIdentity,
        task: &Task<F>,
        input: I,
    ) -> Report<O>
    where
        F: Fn(Scope, I) -> Fut,
        Fut: Future<Output = Result<O, FlowError>>,
    {
        self.run_plan(task, input, Plan::Declared(definition.clone()))
            .await
    }

    /// [`Host::run`] under a run identity the caller already holds.
    ///
    /// The form a caller uses to keep its own identity across a restart: the run
    /// id is the one it recorded, so the run's step records are found under the
    /// same key and finished steps replay. A host with no store refuses, because
    /// a run id with nothing keyed by it would claim a resume that cannot happen.
    ///
    /// This is also the door that **settles** a request a
    /// [`Host::submit`](Self::submit) left incomplete. A request with a receipt
    /// and no terminal record — because a client dropped its waiter, or because
    /// a host stop was correctly *not* recorded as the request's outcome — is
    /// completed here, and the same rule as the submission's own applies: a
    /// disposition that is the request's own verdict is recorded under
    /// `@terminal`, and a host stop is not. Without this, a request whose run
    /// was interrupted could never reach a recorded terminal, because
    /// `submit` reports an incomplete request rather than re-running it.
    ///
    /// # Errors
    ///
    /// None returned: a refusal is a [`Disposition::Refused`] report with no
    /// step run, given when this host has no store, when the run belongs to
    /// another tenant, and when a store host cannot mint a run identity. A store
    /// that refuses the terminal record of a request is a [`Disposition::Failed`]
    /// report: the request's outcome is not recorded, and saying so is better
    /// than returning a success the store never accepted.
    ///
    /// `O: Durable` for the same reason [`Host::submit`] requires it, and only
    /// [`Host::run`] — which mints an identity and writes no request record —
    /// does not: a resumed run may be settling a request, and a recorded verdict
    /// is made of an archived output.
    pub async fn resume<I, O, F, Fut>(&self, run: RunId, task: &Task<F>, input: I) -> Report<O>
    where
        O: Durable,
        F: Fn(Scope, I) -> Fut,
        Fut: Future<Output = Result<O, FlowError>>,
    {
        let started = Instant::now();
        let report = self.run_plan(task, input, Plan::Resume(run)).await;
        self.settle_request(run, task.name_owned(), started, report)
            .await
    }

    /// [`Host::resume`] under a declared definition identity.
    ///
    /// The check this door exists for: a resume whose definition, input, step
    /// count or value schema is not the one the records were written under is a
    /// [`Disposition::Refused`] report carrying [`FlowError::Incompatible`], with
    /// nothing admitted, no body constructed and no record written. A run this
    /// store holds no record for is *not* refused — a process killed before its
    /// first record left nothing to disagree with, and refusing that would make
    /// the exactly-at-least-once step unrecoverable.
    ///
    /// Settled like [`Host::resume`]: a declared-identity resume of a run that is
    /// a request records its outcome the same way, so the request's receipt does
    /// not stay open behind a door that finished the work.
    pub async fn resume_under<I, O, F, Fut>(
        &self,
        run: RunId,
        definition: &DefinitionIdentity,
        task: &Task<F>,
        input: I,
    ) -> Report<O>
    where
        O: Durable,
        F: Fn(Scope, I) -> Fut,
        Fut: Future<Output = Result<O, FlowError>>,
    {
        let started = Instant::now();
        let report = self
            .run_plan(task, input, Plan::ResumeUnder(run, definition.clone()))
            .await;
        self.settle_request(run, task.name_owned(), started, report)
            .await
    }

    /// Resume from a [`Ticket`].
    ///
    /// The ticket names the run, so this is [`Host::resume`] with the identity
    /// read off the ticket. A ticket for another tenant is a typed refusal on
    /// this host's store rather than a silent new run: the caller's ticket says
    /// the run exists and belongs elsewhere, and pretending it does not is how a
    /// cross-tenant resume becomes a fresh run that overwrites another tenant's
    /// records.
    ///
    /// # Errors
    ///
    /// [`FlowError`] when the ticket names a tenant other than this host's.
    pub async fn resume_ticket<I, O, F, Fut>(
        &self,
        ticket: &Ticket,
        task: &Task<F>,
        input: I,
    ) -> Result<Report<O>, FlowError>
    where
        F: Fn(Scope, I) -> Fut,
        Fut: Future<Output = Result<O, FlowError>>,
    {
        if ticket.tenant() != &self.inner.tenant {
            let refusal = Err(FlowError::Failed {
                at: Arc::from(ticket.task()),
                reason: format!(
                    "the ticket names tenant {:?}, not {:?}; refusing to resume another \
                 tenant's run",
                    ticket.tenant().as_str(),
                    self.inner.tenant.as_str()
                ),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "resume_ticket: returning an error to the caller");
            return refusal;
        }
        Ok(self.run_plan(task, input, Plan::Resume(ticket.run())).await)
    }

    /// Submit `task` over `input` under a caller-supplied [`RequestKey`],
    /// durably.
    ///
    /// The idempotent-request form of [`Host::run`]: the run identity is derived
    /// from this host's tenant and `key`, the input's canonical [`InputDigest`]
    /// is recorded as a durable receipt *before* any step runs, and the
    /// outcome is:
    ///
    /// - [`Submission::Executed`] for a first submission (the body ran or
    ///   resumed under the derived run, and a terminal outcome is now recorded);
    /// - [`Submission::Reattached`] when the same key with the same input had
    ///   already reached a recorded outcome: the recorded report comes back with
    ///   the body never re-run;
    /// - [`Submission::InFlight`] when a previous client submitted the key and
    ///   went away before the run reached a recorded outcome, so an effect may be
    ///   live (see [`InFlight`]).
    ///
    /// A *different* input under the same key is [`RequestError::Conflict`],
    /// carrying both digests. Distinct keys derive distinct runs and are never
    /// collapsed.
    ///
    /// # Which outcomes are recorded as the request's own
    ///
    /// A terminal outcome is recorded under `@terminal` for exactly the
    /// dispositions that are **the request's verdict**: [`Disposition::Succeeded`],
    /// [`Disposition::Failed`] and [`Disposition::DeadlineExceeded`] — the three
    /// the declared task produces, the deadline included because the same
    /// declaration that fixed the key fixed the run's budget.
    ///
    /// [`Disposition::Cancelled`] (the host's stop arrived after admission) and
    /// [`Disposition::Refused`] (the host declined before admission) are **not**
    /// recorded. They are this host declining to finish the request, not the
    /// request failing, and a key whose terminal record says either is a key no
    /// later submission can ever complete — the key *is* the request's identity,
    /// so its body runs at most once and a stop recorded as the verdict ends it
    /// for good. This call still returns [`Submission::Executed`] with the stop in
    /// the report, so the caller learns that the run was interrupted; what it
    /// does not do is turn a restart into a request that can never succeed. The
    /// receipt and every durable step record survive, which is what makes a later
    /// submission [`Submission::InFlight`] and a [`Host::resume`] under that run
    /// able to settle it with each finished step not re-run.
    ///
    /// Enforced by `tests/request_key.rs`
    /// (`a_host_stop_mid_run_leaves_the_request_resumable`,
    /// `a_refusal_before_admission_is_not_recorded_as_the_outcome`,
    /// `a_failed_run_is_the_requests_recorded_outcome`) and
    /// `tests/sim_request_key.rs` (`host_stops_never_poison_a_key_band_00..03`,
    /// `same_seed_replays_host_stops_band_00..03`).
    ///
    /// # Store required
    ///
    /// A durable submission is defined by its receipt, so a host with no store
    /// refuses with [`RequestError::NoStore`] rather than silently degrading to a
    /// plain run.
    ///
    /// # Errors
    ///
    /// [`RequestError::NoStore`], [`RequestError::Conflict`],
    /// [`RequestError::Record`], [`RequestError::Store`] or
    /// [`RequestError::IdCollision`].
    pub async fn submit<I, O, F, Fut>(
        &self,
        key: &RequestKey,
        task: &Task<F>,
        input: I,
    ) -> Result<Submission<O>, RequestError>
    where
        I: InputIdentity,
        O: Durable,
        F: Fn(Scope, I) -> Fut,
        Fut: Future<Output = Result<O, FlowError>>,
    {
        let Some(store) = self.inner.store.as_ref() else {
            let refusal = Err(RequestError::NoStore);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "submit: returning an error to the caller");
            return refusal;
        };
        let tenant = self.inner.tenant.as_str();
        let run = derive_request_run(tenant, key)?;
        let records = self.records(Some(run)).ok_or(RequestError::NoStore)?;
        let digest = InputDigest::of(&input);
        let binding_key = reserved_step_key(tenant, BINDING_STEP);
        // The identity the request's own records are written under, derived the
        // same way `execute` derives it for this run, so the binding and terminal
        // records replay under the identity the body's steps were recorded with.
        let definition = self.definition_for(task.name(), Some(&run));

        // A receipt already on the disk decides everything: a repeat of the same
        // input reattaches, and a different input is a conflict.
        if let Some(held) = records.lookup(tenant, run, binding_key)? {
            let existing = Existing {
                store,
                records: &records,
                tenant,
                run,
                key,
                held: &held,
                digest,
            };
            return self.observe_existing(existing, task);
        }

        // No receipt yet: the claim decides which submitter may run the body.
        let claimed = records
            .claim(
                tenant,
                run,
                binding_key,
                BINDING_STEP,
                &definition,
                digest.as_bytes().to_vec(),
            )
            .await?;
        match claimed {
            Appended::Recorded => {}
            Appended::AlreadyRecorded => {
                let held = recorded_receipt(&records, tenant, run, binding_key)?;
                let existing = Existing {
                    store,
                    records: &records,
                    tenant,
                    run,
                    key,
                    held: &held,
                    digest,
                };
                return self.observe_existing(existing, task);
            }
            Appended::Conflicting => {
                let held = recorded_receipt(&records, tenant, run, binding_key)?;
                let existing = digest_of_record(held.bytes())?;
                let refusal = Err(RequestError::Conflict(RequestConflict::new(
                    key.clone(),
                    existing,
                    digest,
                )));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "submit: returning an error to the caller");
                return refusal;
            }
        }

        // The body runs under the derived run, so its durable steps replay on a
        // reattach exactly as a resume's do.
        let report = self
            .execute(task, input, Plan::Resume(run), None, &GrantSet::empty(), 1)
            .await;
        // `None` for a disposition that is the host declining to finish the run,
        // and a receipt with no terminal record is precisely what a later
        // submission of the same key needs in order to be able to run it.
        if let Some(terminal) = terminal_for(&report)? {
            records
                .record(
                    tenant,
                    run,
                    reserved_step_key(tenant, TERMINAL_STEP),
                    TERMINAL_STEP,
                    &definition,
                    terminal.encode()?,
                )
                .await?;
        }
        Ok(Submission::Executed(report))
    }

    /// Observe a request whose receipt is already on the disk.
    ///
    /// The two facts this returns are the whole of T30 and T17: a recorded
    /// terminal outcome is a [`Submission::Reattached`] report reproduced
    /// without re-running the body, and a receipt with no terminal record is a
    /// [`Submission::InFlight`] report that keeps the uncertainty and the
    /// cleanup handle apart.
    fn observe_existing<O, F>(
        &self,
        existing: Existing<'_>,
        task: &Task<F>,
    ) -> Result<Submission<O>, RequestError>
    where
        O: Durable,
    {
        let Existing {
            store,
            records,
            tenant,
            run,
            key,
            held,
            digest,
        } = existing;
        let recorded = digest_of_record(held.bytes())?;
        if recorded != digest {
            let refusal = Err(RequestError::Conflict(RequestConflict::new(
                key.clone(),
                recorded,
                digest,
            )));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "observe_existing: returning an error to the caller");
            return refusal;
        }
        let terminal_key = reserved_step_key(tenant, TERMINAL_STEP);
        if let Some(head) = records.lookup(tenant, run, terminal_key)? {
            let (disposition, output, error) =
                TerminalRecord::decode(head.bytes())?.parts::<O>()?;
            let report = self.report(Terminal {
                started: Instant::now(),
                task: task.name_owned(),
                disposition,
                output,
                error,
                trail: TrailSnapshot::Empty,
                run: Some(run),
                needs: None,
                repair: None,
            });
            return Ok(Submission::Reattached(report));
        }
        Ok(Submission::InFlight(InFlight::new(
            run,
            task.name().to_owned(),
            store.record_count(run),
        )))
    }

    /// Settle a request a resume just completed, if `run` is one.
    ///
    /// The counterpart to [`Host::submit`]'s recording: `submit` records a
    /// terminal outcome for the run it just drove, and this records it for a run
    /// a resume drove, so a request interrupted by a host stop (or by a client
    /// that walked away) can reach a recorded verdict at all. Without it the key
    /// would report `InFlight` forever, which is the same permanent outcome the
    /// stop caused in the first place.
    ///
    /// A run that is **not** a request is left alone: the receipt is what makes
    /// a run a request, and an ordinary `Host::resume` of a caller-minted run id
    /// gains no reserved record from having been resumed. A run whose receipt
    /// already holds a terminal record is left alone too, so a second resume
    /// cannot overwrite a verdict already recorded.
    ///
    /// The report is returned unchanged, except that a store that refuses the
    /// record turns the run into a located failure. Recording an outcome and
    /// reporting success are one fact; a host that could not record the verdict
    /// must not hand back the value as though it had.
    ///
    /// A store refusal here is reported as a disposition rather than returned as
    /// an error because the door is [`Host::resume`], whose signature answers a
    /// `Report` for every outcome — including admission refusals, which are
    /// already reported rather than returned.
    async fn settle_request<O>(
        &self,
        run: RunId,
        task: TaskName,
        started: Instant,
        report: Report<O>,
    ) -> Report<O>
    where
        O: Durable,
    {
        // A host stop records nothing, so there is nothing here to settle, and
        // the report that says the host declined is the answer the caller needs.
        // A store that cannot be *read* while checking an outcome nobody is
        // going to write must not turn a `Refused` into a `Failed` — which is
        // also what keeps a resume of another tenant's run `Refused`: reading its
        // records is refused by the store itself, and that refusal is about the
        // caller, not about the request.
        if matches!(
            report.disposition(),
            Disposition::Refused | Disposition::Cancelled
        ) {
            return report;
        }
        let Some(records) = self.records(Some(run)) else {
            return report;
        };
        let tenant = self.inner.tenant.as_str();
        match self.unsettled(&records, tenant, run).await {
            // Either this run is not a request, or its verdict is already
            // recorded: nothing here has an opinion about either.
            Ok(false) => report,
            Err(error) => self.report(Self::unrecorded(task, started, run, error)),
            Ok(true) => {
                self.settle(records, tenant, run, task, started, report)
                    .await
            }
        }
    }

    /// Whether `run` is a request whose recorded outcome is still missing.
    ///
    /// A store read failure is `Err` rather than a `false` (INV-BOT-7): absence of
    /// evidence and a failed read are different facts, and answering "nothing to
    /// settle" to a failed read would skip the record the caller came for.
    async fn unsettled(
        &self,
        records: &Records,
        tenant: &str,
        run: RunId,
    ) -> Result<bool, FlowError> {
        // Not a request: an ordinary resume of a caller-minted run id writes no
        // reserved record, because the receipt is what makes a run a request.
        if records
            .lookup(tenant, run, reserved_step_key(tenant, BINDING_STEP))?
            .is_none()
        {
            return Ok(false);
        }
        // Already settled. A resume that repeats must not overwrite a verdict a
        // client may already have reattached to.
        Ok(records
            .lookup(tenant, run, reserved_step_key(tenant, TERMINAL_STEP))?
            .is_none())
    }

    /// Write the verdict `report` states for an unsettled request.
    ///
    /// `O: Durable` is required here rather than on [`Host::resume`] because it
    /// is required only to *archive a success*: a resume whose output is not
    /// archivable has no recorded verdict to write even when the run is a
    /// request, and saying that is this run's own answer. A stopped or failed
    /// resume records nothing at all and never reaches the archive, so the bound
    /// costs a caller nothing unless it actually submits a success.
    async fn settle<O>(
        &self,
        records: Records,
        tenant: &str,
        run: RunId,
        task: TaskName,
        started: Instant,
        report: Report<O>,
    ) -> Report<O>
    where
        O: Durable,
    {
        let Some(terminal) = terminal_for(&report).ok().flatten() else {
            // A host stop, which is not the request's verdict.
            return report;
        };
        // The identity the verdict is written under, derived as `execute` derives
        // it for this run, so the terminal record sits under the same definition
        // the body's steps were recorded with.
        let definition = self.definition_for(task.as_str(), Some(&run));
        let bytes = match terminal.encode() {
            Ok(bytes) => bytes,
            Err(error) => return self.report(Self::unrecorded(task, started, run, error)),
        };
        let stored = records
            .record(
                tenant,
                run,
                reserved_step_key(tenant, TERMINAL_STEP),
                TERMINAL_STEP,
                &definition,
                bytes,
            )
            .await;
        match stored {
            Ok(()) => report,
            Err(error) => self.report(Self::unrecorded(task, started, run, error)),
        }
    }

    /// A run whose outcome the store refused to record, located at its task.
    fn unrecorded<O>(
        task: TaskName,
        started: Instant,
        run: RunId,
        cause: FlowError,
    ) -> Terminal<O> {
        let at = Arc::from(task.as_str());
        let error = FlowError::Failed {
            at,
            reason: format!("the request's outcome was not recorded: {cause}"),
        };
        Terminal {
            started,
            task,
            disposition: disposition_of(&error),
            output: None,
            error: Some(error),
            trail: TrailSnapshot::Empty,
            run: Some(run),
            needs: None,
            repair: None,
        }
    }

    /// Repair a run blocked on unmet authority, and resume it.
    ///
    /// The door a [`RepairTicket`] is answered at. It
    /// resumes the run under the run's own id, so every step recorded before the
    /// block replays without its body being polled — the prior analysis is not
    /// rerun — and only the blocked remainder runs. That is the whole of what a
    /// repair costs and the reason it is safe to hand out more authority: the
    /// work already done is not done again.
    ///
    /// # What is refused, and in what order
    ///
    /// Nothing is applied until every check has passed, and each check leaves the
    /// run blocked with its authority unchanged:
    ///
    /// 1. [`RepairError::NoLedger`] — a host with no repair ledger has no epoch,
    ///    no root budget and no record of applied tickets, so there is nothing to
    ///    decide this repair against. Applying it unchecked would be exactly the
    ///    widen-on-replay this door exists to prevent.
    /// 2. [`RepairError::ForeignTenant`] — the ticket names another tenant's run.
    /// 3. [`RepairError::NotAuthorized`] — the grant does not cover every
    ///    capability the ticket names, so the run would stay blocked.
    /// 4. [`RepairError::OverWide`] — the grant carries any capability the ticket
    ///    does not name, shipped or custom. Refused rather than narrowed, so what
    ///    the caller believes was granted cannot exceed what was asked.
    /// 5. [`RepairError::StaleEpoch`] / [`RepairError::AlreadyApplied`] /
    ///    [`RepairError::BudgetSpent`] — decided by the ledger's one ordered step,
    ///    against the run's *current* epoch, so two repairs delivered together
    ///    cannot both apply.
    ///
    /// Only after all five does the run resume, and it resumes with exactly the
    /// ticket's needs added — the authority applied is built from the ticket, not
    /// taken from `grant`, so it cannot exceed the request even in principle. The
    /// repair is charged against the run's root budget exactly as any other
    /// attempt is, so an authorized repair consumes budget rather than resetting
    /// it (T13).
    ///
    /// # Errors
    ///
    /// Every [`RepairError`] above. On success the returned [`Report`] is the run's
    /// own report — it is the same run, resumed, not a second run — so its output,
    /// its disposition and its step trail describe what the repaired run did.
    ///
    /// # A repaired request is settled
    ///
    /// A run [`Host::submit`] started and that blocked records no verdict
    /// (`Blocked` is not the request's outcome: a repair can still move it), so
    /// the repair is the attempt that reaches one, and it settles the request
    /// exactly as [`Host::resume`] does. Without that, a blocked request repaired
    /// to success would still answer [`Submission::InFlight`] to every later
    /// submission of its key. `O: Durable` for the same reason as on
    /// [`Host::resume`]: a recorded verdict is made of an archived output.
    pub async fn repair<I, O, F, Fut>(
        &self,
        ticket: &RepairTicket,
        grant: &GrantSet,
        task: &Task<F>,
        input: I,
        spend: u64,
    ) -> Result<Report<O>, RepairError>
    where
        O: Durable,
        F: Fn(Scope, I) -> Fut,
        Fut: Future<Output = Result<O, FlowError>>,
    {
        let started = Instant::now();
        let ledger = self.inner.ledger.as_ref().ok_or(RepairError::NoLedger)?;
        if ticket.tenant() != self.inner.tenant.as_str() {
            let refusal = Err(RepairError::ForeignTenant {
                ticket: ticket.tenant().to_owned(),
                host: self.inner.tenant.as_str().to_owned(),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "repair: returning an error to the caller");
            return refusal;
        }
        // The grant is checked against the ticket *before* the run is admitted and
        // before the ledger is charged, so a denied repair costs nothing at all:
        // no budget, no epoch, no step.
        ticket.check_grant(grant)?;
        // A run the ledger has never seen has no epoch and no budget, so its
        // authority cannot be attributed; refused rather than minted.
        let control = ledger
            .control(ticket.run())
            .ok_or_else(|| RepairError::UnknownRun {
                run: ticket.run().id().to_hex(),
            })?;
        if control.epoch() != ticket.epoch() {
            let refusal = Err(RepairError::StaleEpoch {
                current: control.epoch(),
                offered: ticket.epoch(),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "repair: returning an error to the caller");
            return refusal;
        }
        let delta = ticket.delta();
        let report = self
            .execute(
                task,
                input,
                Plan::Resume(ticket.run()),
                Some(ticket),
                &delta,
                spend,
            )
            .await;
        Ok(self
            .settle_request(ticket.run(), task.name_owned(), started, report)
            .await)
    }

    /// The one body of a run, shared by [`Host::run`], [`Host::resume`] and
    /// [`Host::repair`].
    ///
    /// Split so there is exactly one place a run mints or adopts its identity,
    /// checks its authority, builds its scope, installs the store and assembles
    /// its report — a second copy of any of those is a second definition of what
    /// a resume means. `repair` is the one door that widens authority, and it is a
    /// parameter rather than a branch, so the admission check below cannot be
    /// bypassed by a caller that happens to hold a ticket.
    ///
    /// The root budget is charged for **every** run, repair or not, and it is
    /// charged *before* the body so a run that spends its budget to zero reaches
    /// the refusal rather than running once more. That is what makes a permanent
    /// refusal plus repeated `NotApplied` finite: the budget is carried in the
    /// run's own ledger, so a fresh process finds it where the last one left it.
    ///
    /// `plan` carries the two facts a caller may declare — the run id to resume
    /// and the definition identity it asserts — as one sum type, so the drift
    /// check has one placement rather than one per door.
    async fn execute<I, O, F, Fut>(
        &self,
        task: &Task<F>,
        input: I,
        plan: Plan,
        repair: Option<&RepairTicket>,
        grant_delta: &GrantSet,
        spend: u64,
    ) -> Report<O>
    where
        F: Fn(Scope, I) -> Fut,
        Fut: Future<Output = Result<O, FlowError>>,
    {
        let started = Instant::now();
        let task_name = task.name_owned();
        let resume = plan.run();
        // The definition this run records under. A run that declared none gets a
        // per-run identity derived from the tenant, the task and the run id, which
        // is stable across attempts at one run and therefore resumes exactly as a
        // run under `run` already does — a declared identity is an addition, never
        // a precondition.
        // A resume's identity names the run it is *replacing* only for its first
        // attempt: a caller that runs the same task on the same input twice gets
        // two runs and two identities, which is right — they are two pieces of
        // work. Declaring one identity for both and resuming the first would then
        // be refused, which is the opposite of what a caller means. So the
        // identity a run is recorded under is its own first attempt's, and a
        // resume is measured against that.
        // Read before the match consumes it: the drift check below needs to know
        // whether the caller declared this identity or the host derived it, and
        // only the first is a claim about the run.
        let declared = plan.declared().cloned();
        let definition = match declared.clone() {
            Some(identity) => identity,
            None => self.definition_for(task_name.as_str(), resume.as_ref()),
        };

        // A resume this store knows belongs to another tenant is refused before
        // admission, so the refusal is a `Refused` with no body and no step rather
        // than a record written under the wrong tenant. A run the store has
        // *never seen* is not refused: a process killed before its first record
        // left no evidence at all, and refusing that resume would make the
        // exactly-at-least-once step unrecoverable. The store still refuses to
        // *read* another tenant's records; this only governs whether the run id
        // may be adopted.
        if let Some(run) = resume
            && let Some(store) = self.inner.store.as_ref()
            && let Some(owner) = store.tenant_of(run)
            && owner != self.inner.tenant.as_str()
        {
            let error = StoreError::ForeignTenant {
                owner,
                asked: self.inner.tenant.as_str().to_owned(),
            };
            return self.refuse(started, task_name, error.to_string());
        }
        // What this call's records will be *written* under, decided before
        // admission. The run a call adopts is not always the run it records
        // against — a fresh run mints one of its own below, and it is *that*
        // run's records this call writes — so a resume reads the identity its own
        // records already carry rather than the one its derivation would name.
        // The steps need that: a durable step must be able to *find* the records
        // it wrote under the identity the run was admitted with, whichever
        // declaration the caller makes now.
        let recorded_definition = match resume {
            Some(adopted) => self.identity_for(adopted, &definition),
            None => definition.clone(),
        };
        // The drift check, before admission and before the body is ever
        // constructed. Replaying a value the current definition never produced and
        // re-running a step whose effect already landed are the two answers on
        // either side of this, so the run is refused rather than picked between,
        // and the refusal names the axis that disagrees so the caller knows which
        // repair it needs. Nothing is written and no permit is taken.
        //
        // Only a *declared* identity is checked. The recorded one is what the
        // steps below write and look up under, so comparing a run's records
        // against it would agree by construction and fence nothing.
        if let (Some(run), Some(store), Some(declared)) =
            (resume, self.inner.store.as_ref(), declared.as_ref())
            && let Err(error) = store.check_definition(run, declared)
        {
            // Every refusal is refused; only the *kind* differs, and the kind is
            // the store's own. A drift is the flow's typed `Incompatible` carrying
            // its axis; every other store refusal — an unreadable device, a
            // ceiling, a foreign tenant — reaches the caller as `FlowError::Store`
            // wrapping the store's own error. Matching on `Incompatible` alone
            // once let a read failure through as though the check had passed,
            // which is INV-BOT-7 in the host's own pre-flight: a store that could
            // not be read admitted the run and let the step below discover it,
            // having already told the caller nothing.
            let located = match error {
                StoreError::Incompatible { ref drift, .. } => {
                    FlowError::incompatible(task_name.as_str(), drift.clone())
                }
                other => FlowError::Store {
                    at: Arc::from(task_name.as_str()),
                    source: Box::new(other),
                },
            };
            self.inner.refused.fetch_add(1, Ordering::Relaxed);
            return self.report(Terminal {
                started,
                task: task_name,
                disposition: Disposition::Refused,
                output: None,
                error: Some(located),
                trail: TrailSnapshot::Empty,
                run: Some(run),
                needs: None,
                repair: None,
            });
        }
        // A resume on a host with nothing to replay from would run every step
        // again under a caller's belief that finished steps are kept. That is the
        // silent downgrade a resume exists to rule out, so it is refused
        // before any step runs.
        if let Some(run) = resume
            && self.inner.store.is_none()
        {
            return self.refuse(
                started,
                task_name,
                format!(
                    "this host has no run store, so run {} has nothing to resume from",
                    run.id().to_hex()
                ),
            );
        }

        // The run identity, decided before admission so a refused run still has
        // the identity a caller can ask about — a run refused at admission ran
        // nothing, and a run id attached to it names a run with no records.
        let run = self.adopt_run(resume);
        // A host given a store promised durable steps. A build that cannot mint
        // the identity those steps are keyed by refuses rather than running
        // them unrecorded and reporting a run nobody can resume.
        if run.is_none() && self.inner.store.is_some() {
            return self.refuse(
                started,
                task_name,
                String::from(
                    "this host has a run store but could not mint a run identity \
                     (the `ephemeral` feature is off or its entropy source failed); \
                     no step ran unrecorded",
                ),
            );
        }

        // The authority check, at the admission boundary and as one complete pass.
        // Only the task's own declaration is checked here, and it is the blunt
        // form: a task that reaches in its *first* step declares it with
        // [`Task::requiring`] and is refused before its body, so nothing runs and
        // nothing is recorded. A task that reaches later declares nothing here and
        // asks at the step that reaches, through [`Scope::require`], so the work
        // before the block survives to be replayed by a repair.
        //
        // The base grant and this run's repair delta are checked *together* and
        // never one replacing the other: a repair adds authority for one run, and
        // every capability the host already had still holds.
        let covers = |cap: &Cap| self.inner.grants.grants(cap) || grant_delta.grants(cap);
        let shortfall = Deficit::from_shortages(uncovered(
            task.needs(),
            covers,
            Some(&Demand::new(task_name.as_str())),
        ));
        if let Some(deficit) = shortfall {
            // Blocked before admission and before the body: nothing was polled and
            // nothing executed, and the report names *every* unmet need plus the
            // repair ticket that closes exactly those. Both come from this one
            // `deficit`, so they cannot disagree about what the run was missing.
            return self.blocked(started, task_name, run, deficit);
        }

        // Admission first, so a run that never starts is never counted as one
        // and never leaves a step in the trail.
        let permit = match self.admit(started, &task_name).await {
            Ok(permit) => permit,
            Err(failure) => {
                let disposition = failure.disposition();
                if disposition == Disposition::Refused {
                    self.inner.refused.fetch_add(1, Ordering::Relaxed);
                }
                return self.report(Terminal {
                    started,
                    task: task_name,
                    disposition,
                    output: None,
                    error: Some(failure.into_error()),
                    trail: TrailSnapshot::Empty,
                    run,
                    needs: None,
                    repair: None,
                });
            }
        };
        // Dropped on every exit path from here on, including a future the caller
        // drops while this run is suspended: the budget returns to what it was,
        // the in-flight count falls, and no body outlives the call.
        let _admission = self.charge(permit);

        // The root budget is charged here, after the run was admitted and confirmed
        // to have authority, but before the body runs. A repair is charged exactly
        // like any other attempt — that is what makes an authorized repair *distinct*
        // rather than a reset (T13). The refusal is reported as a terminal report
        // rather than an error, because the run genuinely happened and its state is
        // exactly this.
        if let Some(run) = run
            && let Some(ledger) = self.inner.ledger.as_ref()
            && let Err(refusal) = ledger
                .charge(
                    self.inner.tenant.as_str(),
                    run,
                    repair.map(RepairTicket::stamp).as_ref(),
                    spend,
                    self.inner.repair_attempts,
                    self.inner.repair_spend,
                )
                .await
        {
            return self.report(Terminal {
                started,
                task: task_name,
                disposition: Disposition::Refused,
                output: None,
                error: Some(FlowError::failed(format_args!(
                    "the run's root budget refused this attempt: {refusal}"
                ))),
                trail: TrailSnapshot::Empty,
                run: Some(run),
                needs: None,
                repair: None,
            });
        }

        let trail = Trail::new(self.inner.limits.progress_capacity());
        // The host's clock, not a fresh one: a run's admission wait and its
        // body's `within` budget are two readings of the same timeline, and
        // minting a second clock here would make a run that is admitted late
        // look to its own body as if it had started on time.
        let root = Scope::rooted(
            self.inner.tenant.clone(),
            self.inner.token.child_token(),
            Arc::clone(&trail),
            self.inner.clock.clone(),
            run,
        );
        let scope = match root.enter(task_name.as_str()) {
            Ok(scope) => scope,
            // The task's own step could not be entered: the name validated at
            // declaration, so this is the depth ceiling or a stop that arrived
            // in the instant between admission and entry. Either way the body
            // never ran, and the report says so rather than inventing a trail.
            Err(error) => {
                return self.report(Terminal {
                    started,
                    task: task_name,
                    disposition: disposition_of(&error),
                    output: None,
                    error: Some(error),
                    trail: TrailSnapshot::Empty,
                    run,
                    needs: None,
                    repair: None,
                });
            }
        };

        // A nested run's identity joins the set its parent established, so a
        // child two levels down sees every ancestor's host and is charged the
        // same way. Sibling runs have their own scopes and their own sets.
        //
        // Outside a scope there is no held set, and an empty one is the correct
        // reading: this run holds nothing it inherited, so it is charged for
        // everything it does itself. `Vec::new()` rather than a discard of the
        // not-entered error, which carries no permit information at all.
        let held = match HELD_PERMITS.try_with(Clone::clone) {
            Ok(held) => held,
            Err(not_entered) => {
                lgwks_std::trace::debug!(
                    ?not_entered,
                    "submit: no enclosing scope holds a permit set"
                );
                Vec::new()
            }
        };
        let charged = if held.contains(&self.inner.identity) {
            held
        } else {
            let mut charged = held;
            charged.push(self.inner.identity);
            charged
        };
        let body = (task.body)(scope.clone(), input);
        let deadline = self.inner.limits.default_deadline();
        // Everything from here to the report is one `Box`: this frame is on the
        // stack of every *nested* run, and depth-four nesting at an admission
        // ceiling of one is a shipped property (T04). Holding the body future,
        // both task-local scopes, the deadline wrapper and the report assembly
        // inline makes each level's frame large enough that the default test
        // thread's stack overflows at the depths the invariant names. One
        // pointer per level is the difference between that and not.
        // The store is installed for exactly this future and its descendants, so
        // a durable step records against *this* run and a nested run under the
        // same host inherits both. With no store the scope is entered with no
        // run at all, which is what makes "no durability" the default rather
        // than a claim.
        // One composition for all three task-local scopes rather than one per
        // combination of "has a store" and "has a repair": two alternative
        // nestings in one future carry both in its frame, which is what pushed a
        // two-review test past a 2 MiB thread stack, and a third would have
        // widened the same frame again. Boxed, so a run nested in another run's
        // body costs the parent a pointer rather than its frame.
        let outcome = Box::pin(crate::script::run_store::with_authority(
            Some(self.authority(grant_delta)),
            crate::script::run_store::within(
                self.records(run),
                &recorded_definition,
                HELD_PERMITS.scope(charged, within(&scope, BODY_STEP, deadline, body)),
            ),
        ))
        .await;

        let snapshot = trail.snapshot();
        let (disposition, output, error, needs) = match outcome {
            Ok(value) => (Disposition::Succeeded, Some(value), None, None),
            // A step that reached for authority it does not have blocks the run,
            // and its own shortfall rides out with it: the report's needs and the
            // repair ticket are both built from this one value, so they cannot
            // disagree about what the run was missing. Every other failure is an
            // ordinary located error.
            Err(error) => {
                let located = error.located(&scope);
                let deficit = located.deficit().cloned();
                match deficit {
                    Some(deficit) => (Disposition::Blocked, None, Some(located), Some(deficit)),
                    None => (disposition_of(&located), None, Some(located), None),
                }
            }
        };
        // A blocked run mints its repair ticket from the same shortfall the report
        // carries, and only when the host holds a ledger to decide a later repair
        // against. A blocked run *inside* a repair mints none: a repair that could
        // not close the shortfall hands back the run it was given rather than a
        // fresh ticket for the same block, which would be a way to retry a denied
        // repair by asking again.
        let repair = match (
            needs.as_ref(),
            run,
            self.inner.ledger.as_ref(),
            repair.is_some(),
        ) {
            (Some(deficit), Some(run), Some(ledger), false) => {
                let epoch = ledger.control(run).map_or(0, |control| control.epoch());
                Some(RepairTicket::of_deficit(
                    run,
                    self.inner.tenant.as_str(),
                    epoch,
                    deficit,
                ))
            }
            _ => None,
        };
        self.report(Terminal {
            started,
            task: task_name,
            disposition,
            output,
            error,
            trail: TrailSnapshot::Taken(snapshot),
            run,
            needs,
            repair,
        })
    }

    /// A run whose body never ran and whose step trail is therefore empty.
    ///
    /// The one assembly point for the two reports that describe a run the host
    /// declined before a body could start — [`refuse`](Self::refuse) and
    /// [`blocked`](Self::blocked) — because the parts they share are facts rather
    /// than style: no output exists, no step was entered, and the failure is
    /// located at the task because there is no step to locate it in. The two
    /// differ only in their disposition, their run id, and whether unmet
    /// authority rides along.
    fn not_admitted<O>(&self, started: Instant, task: TaskName, declined: Declined) -> Report<O> {
        let Declined {
            disposition,
            reason,
            run,
            needs,
            repair,
        } = declined;
        let at = Arc::from(task.as_str());
        self.report(Terminal {
            started,
            task,
            disposition,
            output: None,
            error: Some(FlowError::Failed { at, reason }),
            trail: TrailSnapshot::Empty,
            run,
            needs,
            repair,
        })
    }

    /// A run refused before admission: `Refused`, no body, no step, no run id,
    /// and the reason located at the task.
    fn refuse<O>(&self, started: Instant, task: TaskName, reason: String) -> Report<O> {
        self.inner.refused.fetch_add(1, Ordering::Relaxed);
        self.not_admitted(started, task, Declined::plain(Disposition::Refused, reason))
    }

    /// A run blocked on unmet authority: `Blocked`, no body, no step, every unmet
    /// need, and — when this host has a ledger to decide a repair against — the
    /// repair ticket naming exactly those needs.
    ///
    /// One assembly point for the two facts a caller acts on, because they come
    /// from the same [`Deficit`] and a report that named a different set in its
    /// needs and its ticket would be the one thing a caller could not detect.
    fn blocked<O>(
        &self,
        started: Instant,
        task: TaskName,
        run: Option<RunId>,
        deficit: Deficit,
    ) -> Report<O> {
        // A ticket is minted only for a run whose epoch the ledger knows and for a
        // run that is *not* already carrying a repair: a repair that could not
        // close the shortfall hands back the run it was given rather than minting a
        // fresh ticket for the same block, which would be a way to retry a denied
        // repair by asking again.
        let repair = match (run, self.inner.ledger.as_ref()) {
            (Some(run), Some(_ledger)) => {
                let epoch = self
                    .inner
                    .ledger
                    .as_ref()
                    .and_then(|ledger| ledger.control(run))
                    .map_or(0, |control| control.epoch());
                Some(RepairTicket::of_deficit(
                    run,
                    self.inner.tenant.as_str(),
                    epoch,
                    &deficit,
                ))
            }
            _ => None,
        };
        self.not_admitted(
            started,
            task,
            Declined {
                disposition: Disposition::Blocked,
                reason: String::from("this host's authority does not cover what this task needs"),
                run,
                needs: Some(deficit),
                repair,
            },
        )
    }

    /// The run identity for a run, given the one a resume supplied.
    ///
    /// A resume keeps its own identity. A *fresh* run mints one only when a store
    /// is installed **and** the crate was built with the `ephemeral` feature,
    /// which is the estate's one entropy source. Without a store there is
    /// nothing to key a record by, so a minted id would be an identity nothing
    /// is stored under; without `ephemeral` there is no source to mint from, and
    /// INV-RANDOM-ONE-SOURCE refuses a cheaper substitute. Without a store the
    /// run proceeds with no run id; with a store and no identity, `execute`
    /// refuses rather than run steps the store promised to keep.
    fn adopt_run(&self, resume: Option<RunId>) -> Option<RunId> {
        match (self.inner.store.is_some(), resume) {
            (true, Some(run)) => Some(run),
            (true, None) => self.mint_run(),
            (false, _) => None,
        }
    }

    /// A fresh run identity, or `None` when this build cannot mint one.
    ///
    /// The `ephemeral` gate is not incidental: it is the only feature that turns
    /// on the estate's entropy source, and a run id derived from a clock or a
    /// counter would make two runs indistinguishable — exactly what
    /// `lgwks_std::random`'s invariant exists to refuse.
    #[cfg(feature = "ephemeral")]
    fn mint_run(&self) -> Option<RunId> {
        RunId::mint().ok()
    }

    /// Without the entropy feature there is no source to mint from.
    ///
    /// A build that wants a durable fresh run enables `ephemeral`; a build that
    /// only resumes supplied run ids needs nothing. Returning `None` keeps the
    /// report honest rather than minting an identity the crate cannot vouch for.
    #[cfg(not(feature = "ephemeral"))]
    fn mint_run(&self) -> Option<RunId> {
        None
    }

    /// The authority this run's steps consult: the host's grant plus the delta of
    /// any repair this run carries.
    ///
    /// Always installed, including for a host that grants nothing. A step outside
    /// any host has *no* authority and so requires nothing; a step under a host
    /// that grants nothing has an authority covering nothing and is refused for
    /// whatever it asked about. That distinction — nobody checked versus checked
    /// and refused — is the whole reason this is installed unconditionally rather
    /// than left absent when the grant set is empty.
    fn authority(&self, grant_delta: &GrantSet) -> RecordsAuthority {
        RecordsAuthority(Arc::new(RunAuthority {
            base: self.inner.grants.clone(),
            delta: grant_delta.clone(),
        }))
    }

    /// The erased record store for this run, when there is one.
    fn records(&self, run: Option<RunId>) -> Option<Records> {
        // A store with no run cannot key a record, and a run with no store has
        // nowhere to put one; both pairs mean the same thing — this run is not
        // durable — so they are refused here rather than at the first step.
        let store = self.inner.store.as_ref()?;
        run?;
        Some(Records(Arc::new(store.clone())))
    }

    /// Run `task` over `input` from synchronous code, driving it on a runtime
    /// this host owns.
    ///
    /// The host builds its reactor once and reuses it, so repeated calls do not
    /// pay a runtime per call. Inside an existing runtime this **refuses**
    /// rather than parking the calling thread: parking a thread that owns a
    /// reactor is a deadlock, not a slow call, so it returns
    /// [`HostError::InsideRuntime`] — the same verdict, and for the same
    /// reason, as the crate's synchronous tick. Await [`Host::run`] there
    /// instead.
    ///
    /// # Errors
    ///
    /// [`HostError::InsideRuntime`] when a runtime is already driving this
    /// thread; [`HostError::Runtime`] when the OS refuses the runtime's driver
    /// resources.
    pub fn block_on<I, O, F, Fut>(&self, task: &Task<F>, input: I) -> Result<Report<O>, HostError>
    where
        F: Fn(Scope, I) -> Fut,
        Fut: Future<Output = Result<O, FlowError>>,
    {
        // Checked before the reactor is taken, so the refusal cannot leave a
        // freshly built runtime behind for a caller that never used it.
        if lgwks_deps::tokio::runtime::Handle::try_current().is_ok() {
            let refusal = Err(HostError::InsideRuntime);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "block_on: returning an error to the caller");
            return refusal;
        }
        let handle = self.reactor()?;
        Ok(handle.block_on(self.run(task, input)))
    }

    /// This host's reactor, built on first use and kept for every later call.
    fn reactor(&self) -> Result<Handle, HostError> {
        let mut slot = self.reactor_slot();
        if let Some(runtime) = slot.as_ref() {
            return Ok(runtime.handle());
        }
        let runtime = Runtime::new().map_err(|cause| HostError::Runtime { cause })?;
        let handle = runtime.handle();
        *slot = Some(runtime);
        Ok(handle)
    }

    /// The reactor slot's lock.
    ///
    /// Poisoning recovers rather than propagating: the slot holds a
    /// [`Runtime`], whose `Drop` is infallible, so no panic path can leave a
    /// half-installed reactor behind.
    fn reactor_slot(&self) -> MutexGuard<'_, Option<Runtime>> {
        lock(&self.inner.reactor)
    }

    /// Wait for one of this host's permits, bounded by the run's deadline and
    /// cancellable by the host's stop.
    ///
    /// A nested run — one whose future already holds a permit for this host —
    /// acquires nothing and is charged to the permit its ancestor holds. That is
    /// the whole of the no-deadlock argument: a parent awaiting a child never
    /// holds the last permit while waiting, because it never waits for one.
    async fn admit(&self, started: Instant, task: &TaskName) -> Result<Permit, AdmissionFailure> {
        // Both refusals here are distinct facts and both are reported as
        // `Refused`, never `Cancelled`: a run that never held a permit never ran
        // a body, so no step was entered and there is nothing that could have
        // been stopped mid-way. A `Cancelled` disposition means the run was
        // admitted and then stopped, which is a different thing to tell a
        // reader — and the reason this returns a [`Permit`] or a *timeout* is
        // that only a run which entered its body can be cancelled inside it.
        // Outside every scope a run holds nothing, so it is not nested.
        let nested = matches!(
            HELD_PERMITS.try_with(|held| held.contains(&self.inner.identity)),
            Ok(true)
        );
        if nested {
            return Ok(Permit::Charged);
        }
        if self.inner.token.is_cancelled() {
            let refusal = Err(AdmissionFailure::Refused(Refused {
                at: Arc::from(task.as_str()),
            }));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "admit: returning an error to the caller");
            return refusal;
        }
        // The wait for a permit is bounded in real time, charged to the run's
        // deadline. It is a wait on other runs releasing capacity — a real
        // event no logical clock can hurry — so the host's declared clock
        // governs the body's `within`, not this queue.
        let deadline = self.inner.limits.default_deadline();
        let remaining = deadline.saturating_sub(started.elapsed());
        let waiting = self
            .inner
            .token
            .run_until_cancelled(Semaphore::acquire_owned(Arc::clone(&self.inner.admission)));
        match crate::rt::time::timeout(remaining, waiting).await {
            Ok(Some(Ok(permit))) => Ok(Permit::Held(permit)),
            // The semaphore is closed only if a `Host` closed it, and it never
            // does. Typed rather than panicked because the invariant that makes
            // this unreachable is a claim, not a proof, and a budget that is
            // closed is a fact the caller needs rather than a panic.
            Ok(Some(Err(_closed))) => Err(AdmissionFailure::Failed(FlowError::failed(
                "the host's admission budget is closed",
            ))),
            Ok(None) => Err(AdmissionFailure::Refused(Refused {
                at: Arc::from(task.as_str()),
            })),
            Err(_elapsed) => Err(AdmissionFailure::DeadlineExceeded(FlowError::TimedOut {
                at: Arc::from(BODY_STEP),
                after: deadline,
            })),
        }
    }

    /// Build the report for a terminal state.
    ///
    /// The single place a [`Report`] is assembled, because the field that must
    /// agree with the disposition — an output exactly when it succeeded, an
    /// error exactly when it did not — is the field a hand-written literal at
    /// three call sites would get wrong, and a report that disagrees with itself
    /// is the one thing a caller cannot detect from the type.
    fn report<O>(&self, terminal: Terminal<O>) -> Report<O> {
        let Terminal {
            started,
            task,
            disposition,
            output,
            error,
            trail,
            run,
            needs,
            repair,
        } = terminal;
        let (steps, dropped_steps) = match trail {
            TrailSnapshot::Taken((paths, dropped)) => (paths, dropped),
            TrailSnapshot::Empty => (Vec::new(), 0),
        };
        // The two claims a report makes about durability, both derived from one
        // fact — whether this host holds a store — so they cannot disagree. A
        // report with a store says how many records it has; one without says
        // `None`, which is the honest "nothing survived" rather than a count of
        // zero records that reads like a run that did nothing.
        let effects = match (run, self.inner.store.as_ref()) {
            (Some(run), Some(store)) => EffectKnowledge::StepRecords {
                records: store.record_count(run),
            },
            _ => EffectKnowledge::None,
        };
        // A ticket names where a run stopped, so it exists for a stopped run
        // that has a store. A succeeded run has nothing to resume; a run with no
        // store has nothing to resume *from*. Both are absent, and a reader can
        // tell the two apart by `run_id`, which is the field that says whether
        // the run was durable at all.
        let ticket = run.and_then(|run| {
            if disposition.is_success() {
                return None;
            }
            Some(Ticket {
                run,
                tenant: self.inner.tenant.clone(),
                task: task.clone(),
                disposition,
                at: error
                    .as_ref()
                    .map(FlowError::at)
                    .filter(|path| !path.is_empty())
                    .map(Arc::from),
            })
        });
        Report {
            disposition,
            output,
            error,
            elapsed: started.elapsed(),
            steps,
            dropped_steps,
            progress_capacity: self.inner.limits.progress_capacity(),
            effects,
            tenant: self.inner.tenant.clone(),
            task,
            run,
            ticket,
            needs,
            repair,
        }
    }

    /// Move the counters for a run that has just taken a permit, and hand back the
    /// guard that undoes them.
    ///
    /// The guard is what makes the counters exact. A run that finishes normally,
    /// a run whose body fails, a run that is cancelled and a run whose future
    /// the caller drops while suspended all release through it, so `in_flight`
    /// returns to zero without any path having to remember to decrement it.
    fn charge(&self, permit: Permit) -> AdmissionCharge<'_> {
        let held = matches!(permit, Permit::Held(_));
        if held {
            let live = self.inner.in_flight.raise();
            self.inner.peak_in_flight.raise_to(live);
        }
        // Every run is counted, admitted or charged: this is the number of tasks
        // the host actually ran, which is not the number of permits it took.
        self.inner.admitted.fetch_add(1, Ordering::Relaxed);
        AdmissionCharge {
            installation: &self.inner,
            permit,
            held,
        }
    }
}

impl Default for LiveRuns {
    /// A count of zero.
    fn default() -> Self {
        Self(AtomicUsize::new(0))
    }
}

/// A high-water mark over an atomic counter.
///
/// The two operations a ceiling needs — raise to a new maximum, and lower by
/// one — are each written once here rather than at both call sites, so the
/// ordering and the "discard the previous value" shape cannot drift between the
/// path that takes a permit and the path that releases it.
struct HighWater(AtomicUsize);

impl HighWater {
    /// Record that `live` tasks are in flight now.
    ///
    /// Relaxed throughout: these are counters read for reporting and for one
    /// assertion, not guards for anyone else's memory.
    fn raise_to(&self, live: usize) {
        // Nothing closes an atomic, so the substitution always happens; the
        // previous value is bound and never read, which is how a returned value
        // is discarded without `let _ =` — the shape `let_underscore_must_use`
        // exists to refuse — or `drop` on a `Copy` value, which does nothing.
        let _previous = self
            .0
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |peak| {
                (live > peak).then_some(live)
            });
    }

    /// The most tasks that were ever in flight at once.
    fn get(&self) -> usize {
        self.0.load(Ordering::Relaxed)
    }
}

/// One admitted run's claim on its host's budget.
///
/// Holds the permit a top-level run took and counts it down on drop. A nested
/// run holds nothing: it is charged to a permit its ancestor already holds, so
/// its drop has no permit to release and no count to undo.
struct AdmissionCharge<'installation> {
    /// Where the counters live.
    installation: &'installation Installation,
    /// The permit, for a run that took one.
    permit: Permit,
    /// Whether `permit` is a real claim on the budget.
    held: bool,
}

impl Drop for AdmissionCharge<'_> {
    /// Release the permit and undo the in-flight count.
    ///
    /// Infallible: releasing a permit and decrementing a counter cannot fail, and
    /// a panic in a destructor during an unwind would abort the process.
    fn drop(&mut self) {
        // Taking the permit out and dropping it is the release: an
        // `OwnedSemaphorePermit` returns its slot when it is dropped, and this
        // reads the field the type exists to hold, so nothing here is a
        // no-op that only looks like a release.
        if let Permit::Held(permit) = std::mem::replace(&mut self.permit, Permit::Charged) {
            drop(permit);
        }
        if self.held {
            // The count was raised before this guard was built and nothing else
            // lowers it, so the substitution always succeeds. It is written
            // through the same [`LiveRuns`] the raise path uses, so the two
            // cannot drift on ordering or on how the previous value is handled.
            self.installation.in_flight.release_one();
        }
    }
}

impl Default for HighWater {
    /// A mark of zero.
    fn default() -> Self {
        Self(AtomicUsize::new(0))
    }
}

/// The live count of top-level runs holding a permit.
///
/// One type for the raise and the release, because a count that is written in
/// two places with two orderings is a count whose two paths can disagree — and
/// the disagreement is invisible until the ceiling stops bounding anything.
struct LiveRuns(AtomicUsize);

impl LiveRuns {
    /// Record that one more run holds a permit, and return the live count.
    fn raise(&self) -> usize {
        self.0.fetch_add(1, Ordering::Relaxed).saturating_add(1)
    }

    /// Record that one fewer run holds a permit.
    fn release_one(&self) {
        // `checked_sub` is what makes a double release a refusal rather than a
        // wrap to `usize::MAX`: the closure returns `None`, the fetch reports
        // it, and the counter is left alone for a reader to see. Nothing closes
        // an atomic, so the previous value is bound and never read rather than
        // discarded with `let _ =`, the shape `let_underscore_must_use` refuses.
        let _previous = self
            .0
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |live| {
                live.checked_sub(1)
            });
    }

    /// How many runs hold a permit right now.
    fn get(&self) -> usize {
        self.0.load(Ordering::Relaxed)
    }
}

/// A run the host declined to admit, before any body ran.
///
/// Its own type rather than a `FlowError`, because the disposition it maps to
/// is not the one a located failure maps to: a run that never held a permit
/// never entered a step, so there is no step to be cancelled *at*. Reporting it
/// as `Cancelled` would tell a reader its body had started and then stopped.
#[derive(Debug)]
struct Refused {
    /// The task it was refused for, which is the only location it has.
    at: Arc<str>,
}

/// Why a run never reached its body, and the disposition each reason reports.
#[derive(Debug)]
enum AdmissionFailure {
    /// The host was stopped, or stopped while this run waited.
    Refused(Refused),
    /// The budget could not be acquired at all.
    Failed(FlowError),
    /// The run's deadline passed while it waited for a permit.
    DeadlineExceeded(FlowError),
}

impl AdmissionFailure {
    /// The disposition this admission failure reports.
    const fn disposition(&self) -> Disposition {
        match *self {
            Self::Refused(_) => Disposition::Refused,
            Self::DeadlineExceeded(_) => Disposition::DeadlineExceeded,
            Self::Failed(_) => Disposition::Failed,
        }
    }

    /// The located failure this admission failure reports.
    fn into_error(self) -> FlowError {
        match self {
            Self::Refused(Refused { at }) => FlowError::Failed {
                at,
                reason: String::from("the host refused to admit this run"),
            },
            Self::Failed(error) | Self::DeadlineExceeded(error) => error,
        }
    }
}

/// The disposition a run's failure is reported as.
fn disposition_of(error: &FlowError) -> Disposition {
    match *error {
        FlowError::Cancelled { .. } => Disposition::Cancelled,
        FlowError::TimedOut { .. } => Disposition::DeadlineExceeded,
        _ => Disposition::Failed,
    }
}

/// What a run ended with, assembled in one place.
///
/// Its own struct rather than eight parameters: the fields that must agree —
/// an output exactly when it succeeded, an error exactly when it did not — are
/// the ones a hand-written positional call gets wrong, and a report that
/// disagrees with itself is the one thing a caller cannot detect from the type.
struct Terminal<O> {
    /// When the run started, for the elapsed measurement.
    started: Instant,
    /// The task that ran.
    task: TaskName,
    /// How it ended.
    disposition: Disposition,
    /// Its output, when it succeeded.
    output: Option<O>,
    /// Its located failure, when it did not.
    error: Option<FlowError>,
    /// Its step trail.
    trail: TrailSnapshot,
    /// The run id its records are keyed by.
    run: Option<RunId>,
    /// The unmet capabilities this run was admitted short of.
    needs: Option<Deficit>,
    /// The repair ticket a blocked run hands back.
    repair: Option<RepairTicket>,
}

/// How a host declined a run before its body started.
///
/// The half of a [`Terminal`] that varies between the two reports describing a
/// declined run, gathered so `Host::not_admitted` takes one argument for the
/// refusal rather than five positional ones that could be silently transposed at
/// a call site.
struct Declined {
    /// How the run ended.
    disposition: Disposition,
    /// Why, located at the task because no step ran.
    reason: String,
    /// The run id whose records are keyed by it.
    run: Option<RunId>,
    /// The unmet capabilities, for a blocked run.
    needs: Option<Deficit>,
    /// The repair ticket a blocked run hands back.
    repair: Option<RepairTicket>,
}

impl Declined {
    /// A decline carrying no run identity and no unmet authority.
    fn plain(disposition: Disposition, reason: String) -> Self {
        Self {
            disposition,
            reason,
            run: None,
            needs: None,
            repair: None,
        }
    }
}

/// The step paths a report carries.
///
/// A run that entered its task step has a ring to read and passes
/// `Taken`; a run refused before that has no ring at all and passes `Empty`. The
/// two are different facts rather than an `Option` over a snapshot: "no steps"
/// and "no trail to report" must not look the same in a report a reader is
/// reasoning from.
enum TrailSnapshot {
    /// The ring's contents, and how many paths it dropped.
    Taken((Vec<Arc<str>>, usize)),
    /// No ring existed: the run was refused before its scope was built.
    Empty,
}

// ── Authority ───────────────────────────────────────────────────────────────

/// The authority a run's steps consult, erased for the task-local it rides in.
///
/// A handle rather than the concrete type for the same reason [`Records`] is: it
/// is installed as a `dyn` behind a task-local, and the script module must not
/// depend on [`Host`].
/// One run's authority: the host's base grant plus this run's repair delta.
///
/// The two are kept apart rather than merged, because merging loses the question
/// the repair door asks. A repair *adds* authority for one run and never widens
/// the host's, so a step's coverage is "either", and a reader of this type can see
/// which half answered.
struct RunAuthority {
    /// What the host admits every run against.
    base: GrantSet,
    /// What this run's repair authorized, limited to its ticket's needs.
    delta: GrantSet,
}

impl AuthorityCheck for RunAuthority {
    /// Every capability in `required` this run's authority does not cover.
    ///
    /// Checked against the base and the delta together, and named once each: a
    /// capability the base already covers is not reported as short however many
    /// times the delta repeats it, so the shortfall is what the run genuinely
    /// lacks rather than what its repair happened to mention.
    fn uncovered(&self, required: &[Cap]) -> Vec<Shortage> {
        uncovered(
            required,
            |cap| self.base.grants(cap) || self.delta.grants(cap),
            None,
        )
    }
}

/// What a run holds for the length of its body.
///
/// `Held` owns budget permit and releases it on drop; `Charged` holds
/// nothing, because a nested run shares its ancestor's permit. Both are dropped
/// on every exit path, so a suspended run the caller abandons returns its permit
/// and its in-flight count with it.
enum Permit {
    /// This run took a permit out of the host's budget.
    Held(OwnedSemaphorePermit),
    /// This run is charged to a permit its future already holds.
    Charged,
}

// ── HostBuilder ─────────────────────────────────────────────────────────────

/// The validated construction of one [`Host`].
///
/// `#[must_use]` because dropping a builder unused builds no host, and a host
/// that was never installed is a tenant nobody can run for.
#[must_use = "a HostBuilder builds nothing until `build` is called"]
#[derive(Debug)]
pub struct HostBuilder {
    /// Who the host runs for.
    tenant: Tenant,
    /// The admission ceiling.
    max_concurrent: NonZeroUsize,
    /// The default deadline every run inherits.
    deadline: Duration,
    /// The step trail's capacity.
    progress: usize,
    /// The clock every run's deadline is measured on.
    clock: Clock,
    /// The base authority every run is admitted against.
    ///
    /// Checked against a task's declared [`needs`](Task::needs) at admission, as
    /// one complete pass: every unmet capability is reported together, so a
    /// blocked run names the whole shortfall and its repair ticket is written once
    /// rather than one refusal at a time.
    grants: GrantSet,
    /// The durable step-record store, when the caller installed one.
    store: Option<store::RunStore>,
    /// The declared schema id of this host's durable step values.
    ///
    /// The default is [`UNVERSIONED_CODEC`], which says the caller declared none
    /// rather than that the values are schema-free; [`HostBuilder::durable_codec`]
    /// changes it once, for every run this host admits.
    codec: String,
    /// The durable repair ledger, when the caller installed one.
    ledger: Option<RunLedger>,
    /// The most root attempts one run may be charged before it is refused.
    repair_attempts: u64,
    /// The most root spend one run may be charged before it is refused.
    repair_spend: u64,
}

impl HostBuilder {
    /// Admit at most `tasks` top-level runs at once, from
    /// [`MAX_ADMITTED_TASKS`].
    ///
    /// The default is 64 per available core, capped at
    /// [`MAX_ADMITTED_TASKS`]: a body spends its life waiting, so this is a
    /// ceiling on outstanding work rather than on CPU parallelism.
    pub fn max_concurrent_tasks(mut self, tasks: NonZeroUsize) -> Self {
        self.max_concurrent = tasks;
        self
    }

    /// Give every run `deadline` unless it declares its own.
    ///
    /// The default is 30 seconds; the ceiling is [`MAX_TASK_DEADLINE`].
    pub fn default_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }

    /// Measure every run's deadline on `clock`.
    ///
    /// The clock is shared by every run this host admits and by every step
    /// scope under it, so one advance from a test reaches the whole host. It
    /// is the host's declared timeline: [`Host::run`] reads it for the run's
    /// admission wait, and the [`within`] around the body reads it for the body
    /// itself.
    ///
    /// The default is a wall clock, so a host in production never has to drive
    /// anything. A caller-advanceable clock instead makes the run's whole
    /// deadline schedule — admission *and* body — exact and free to exhaust:
    /// advancing past [`HostBuilder::default_deadline`] produces the same
    /// `FlowError::TimedOut` a real overrun produces, with no real wait.
    pub fn clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// Retain `steps` step paths in each report's trail, from
    /// [`MAX_PROGRESS_STEPS`].
    ///
    /// The default is the `script` module's [`DEFAULT_TRAIL_STEPS`]. A report that
    /// drops paths counts them, so this is a bound on detail rather than on
    /// correctness.
    pub fn progress_capacity(mut self, steps: usize) -> Self {
        self.progress = steps;
        self
    }

    /// Install a durable, file-backed per-step record store under `dir`.
    ///
    /// One file per tenant, named after it, so two tenants pointed at the same
    /// directory never share a file. With a store installed, every step that
    /// calls [`remember`](crate::script::remember) is recorded against this host's
    /// run id, [`Report::run_id`] names it, and [`Host::resume`] replays the
    /// recorded steps without polling their futures.
    ///
    /// The store is opened — and its existing records replayed — here, so a
    /// refusal (an unreadable directory, a foreign file, a corrupt frame) happens
    /// at installation rather than on the first step of a run that has already
    /// claimed to be durable.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the store cannot be opened or replayed.
    pub fn run_store(mut self, dir: impl AsRef<std::path::Path>) -> Result<Self, StoreError> {
        self.store = Some(store::RunStore::open_in(
            dir.as_ref(),
            self.tenant.as_str(),
        )?);
        Ok(self)
    }

    /// Install a store this caller already opened.
    ///
    /// For a host whose store is opened once and shared, and for a caller that
    /// wants to get the opened handle back for [`RunStore::tenant_of`] and the
    /// other read-only queries.
    pub fn store(mut self, store: store::RunStore) -> Self {
        self.store = Some(store);
        self
    }

    /// Declare the schema id this host's durable step values are written under.
    ///
    /// Every [`DefinitionIdentity`] this host builds carries it, so changing the
    /// schema of a durable value is one setting here rather than an argument to
    /// every run — and a resume whose records were written under another schema
    /// is refused as [`Drift::Schema`] rather than decoding them as this one.
    ///
    /// The convention is `lgwks.bot.schema.v1.<kind>`, the same one
    /// [`crate::effect::InputIdentity`] uses. The default is
    /// [`UNVERSIONED_CODEC`], which says the caller declared none.
    #[must_use = "a builder that declares nothing is the host the caller already had"]
    pub fn durable_codec(mut self, codec: &str) -> Self {
        codec.clone_into(&mut self.codec);
        self
    }

    /// Admit runs against `grants`.
    ///
    /// The default is [`GrantSet::empty`]: a host grants nothing unless the
    /// caller says so. There is deliberately no `grant_all` — a host that reached
    /// everything by default would make the admission check above vacuous, and
    /// the whole point of declaring a task's needs is that a missing capability
    /// is *reported* rather than silently supplied.
    pub fn grants(mut self, grants: GrantSet) -> Self {
        self.grants = grants;
        self
    }

    /// Install a durable repair ledger under `dir`.
    ///
    /// One file per tenant, named after it, so two tenants pointed at the same
    /// directory never share a file — the same rule the step store follows. With
    /// a ledger installed a blocked run's report carries a
    /// [`RepairTicket`] and [`Host::repair`] can decide a
    /// repair against the run's epoch, its root budget and the tickets already
    /// applied to it.
    ///
    /// The ledger is opened — and its entries replayed — here, so a refusal
    /// happens at installation rather than on the first repair of a run that has
    /// already claimed to be repairable.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the ledger cannot be opened or replayed.
    pub fn repair_ledger(mut self, dir: impl AsRef<std::path::Path>) -> Result<Self, StoreError> {
        self.ledger = Some(RunLedger::open_in(dir.as_ref(), self.tenant.as_str())?);
        Ok(self)
    }

    /// Install a ledger this caller already opened, and share it.
    ///
    /// For a host whose ledger is opened once and handed to several hosts of the
    /// same tenant, and for a caller that wants the handle back to read the
    /// budget a run has been charged.
    pub fn ledger(mut self, ledger: RunLedger) -> Self {
        self.ledger = Some(ledger);
        self
    }

    /// Charge at most `attempts` attempts and `spend` against a run's root
    /// budget.
    ///
    /// The root budget bounds the whole life of a run: every attempt and every
    /// repair spends it, and nothing refills it. A run that exhausts it is
    /// refused with [`RepairError::BudgetSpent`] however many times it is
    /// delivered a ticket, which is what makes a permanent refusal plus repeated
    /// `NotApplied` reach a finite answer rather than retry forever. A `spend` of
    /// zero means attempts alone bound the run.
    pub fn repair_bounds(mut self, attempts: u64, spend: u64) -> Self {
        self.repair_attempts = attempts;
        self.repair_spend = spend;
        self
    }

    /// Install the host.
    ///
    /// # Errors
    ///
    /// [`HostError::Bound`] for a ceiling outside the declared bound, including a
    /// zero deadline, which would expire every run before its body starts.
    pub fn build(self) -> Result<Host, HostError> {
        let tasks = u64::saturating_from(self.max_concurrent.get());
        let tasks_ceiling = u64::saturating_from(MAX_ADMITTED_TASKS);
        if tasks > tasks_ceiling {
            let refusal = Err(HostError::Bound {
                what: "the admission ceiling",
                value: tasks,
                max: tasks_ceiling,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build: returning an error to the caller");
            return refusal;
        }
        if self.deadline.is_zero() {
            let refusal = Err(HostError::Bound {
                what: "the default deadline",
                value: 0,
                max: MAX_TASK_DEADLINE.as_secs(),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build: returning an error to the caller");
            return refusal;
        }
        if self.deadline > MAX_TASK_DEADLINE {
            let refusal = Err(HostError::Bound {
                what: "the default deadline",
                value: self.deadline.as_secs(),
                max: MAX_TASK_DEADLINE.as_secs(),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build: returning an error to the caller");
            return refusal;
        }
        let steps = u64::saturating_from(self.progress);
        let steps_ceiling = u64::saturating_from(MAX_PROGRESS_STEPS);
        if steps > steps_ceiling {
            let refusal = Err(HostError::Bound {
                what: "the progress capacity",
                value: steps,
                max: steps_ceiling,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build: returning an error to the caller");
            return refusal;
        }
        Ok(Host {
            inner: Arc::new(Installation {
                identity: HOST_IDENTITIES.fetch_add(1, Ordering::Relaxed),
                tenant: self.tenant,
                token: CancellationToken::new(),
                limits: HostLimits {
                    max_concurrent: self.max_concurrent,
                    deadline: self.deadline,
                    progress: self.progress,
                },
                admission: Arc::new(Semaphore::new(self.max_concurrent.get())),
                in_flight: LiveRuns::default(),
                peak_in_flight: HighWater::default(),
                admitted: AtomicU64::new(0),
                refused: AtomicU64::new(0),
                clock: self.clock,
                store: self.store,
                codec: self.codec,
                ledger: self.ledger,
                grants: self.grants,
                repair_attempts: self.repair_attempts,
                repair_spend: self.repair_spend,
                reactor: Mutex::new(None),
            }),
        })
    }
}

/// The admission ceiling a host uses when the builder says nothing.
fn default_max_concurrent() -> NonZeroUsize {
    /// The bodies one core may have waiting at once.
    const PER_CORE: NonZeroUsize = NonZeroUsize::MIN.saturating_add(63);
    /// [`MAX_ADMITTED_TASKS`] in the non-zero type the default is clamped in.
    const CEILING: NonZeroUsize =
        NonZeroUsize::MIN.saturating_add(MAX_ADMITTED_TASKS.saturating_sub(1));
    // A host that cannot count its cores is sized as one core: the smallest
    // machine the default has to fit, rather than a guess at a bigger one.
    let cores = match std::thread::available_parallelism() {
        Ok(cores) => cores,
        Err(uncounted) => {
            lgwks_std::trace::debug!(?uncounted, "default_max_concurrent: sizing for one core");
            NonZeroUsize::MIN
        }
    };
    cores.saturating_mul(PER_CORE).min(CEILING)
}

/// The deadline a host applies when the builder says nothing.
///
/// Finite and modest: long enough for a call that waits on a network or a
/// human, short enough that a task nobody is watching stops holding its permit.
/// A ceiling with no default is a ceiling nobody honours.
fn default_deadline() -> Duration {
    Duration::from_secs(30)
}

/// The most root attempts a run is charged before it is refused, when the builder
/// says nothing.
///
/// Eight: enough for a run to be retried across a few transport failures and to
/// take one authorized repair, and few enough that a run that keeps failing
/// reaches the refusal rather than continuing. A default rather than infinity is
/// the whole point — an unbounded root budget is the unlimited-retry loop T13
/// names, spelled with a larger number.
fn default_repair_attempts() -> u64 {
    8
}

/// The most root spend a run is charged before it is refused, when the builder
/// says nothing.
///
/// A multiple of [`default_repair_attempts`], so a caller that never names a spend
/// gets a budget bounded by both axes rather than one that silently governs.
fn default_repair_spend() -> u64 {
    default_repair_attempts().saturating_mul(8)
}

// ── HostLimits ───────────────────────────────────────────────────────────────

/// The finite ceilings one host resolves unspecified values from.
///
/// Every field is finite by default and readable here, which is the whole point
/// of the type: a ceiling the caller cannot see is a ceiling the caller cannot
/// reason about when a run is refused or a trail is truncated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostLimits {
    /// The admission ceiling.
    max_concurrent: NonZeroUsize,
    /// The deadline every run inherits.
    deadline: Duration,
    /// The step trail's capacity.
    progress: usize,
}

impl HostLimits {
    /// The most top-level runs admitted at once.
    #[must_use]
    pub const fn max_concurrent_tasks(&self) -> usize {
        self.max_concurrent.get()
    }

    /// The deadline a run inherits unless it declares its own.
    #[must_use]
    pub const fn default_deadline(&self) -> Duration {
        self.deadline
    }

    /// The step paths one report's trail retains.
    #[must_use]
    pub const fn progress_capacity(&self) -> usize {
        self.progress
    }
}

// ── Admission ───────────────────────────────────────────────────────────────

/// A snapshot of one host's live admission counters.
///
/// Read through [`Host::admission`]. Every number here is about **permits**: a
/// nested run is charged to its ancestor's permit, so it is counted in
/// [`Admission::admitted`] and not in [`Admission::in_flight`], which is exactly
/// why depth-four nesting does not need a ceiling of four.
#[derive(Clone, Copy)]
pub struct Admission<'installation> {
    /// Where the counters live.
    inner: &'installation Installation,
}

impl fmt::Debug for Admission<'_> {
    /// The counters, and not the installation: the point of this snapshot is
    /// the numbers, and rendering the reactor a host owns would make a log line
    /// say nothing useful.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Admission")
            .field("available_permits", &self.available_permits())
            .field("in_flight", &self.in_flight())
            .field("peak_in_flight", &self.peak_in_flight())
            .field("admitted", &self.admitted())
            .field("refused", &self.refused())
            .finish()
    }
}

impl Admission<'_> {
    /// Permits still free. Equals the ceiling when nothing is in flight.
    #[must_use]
    pub fn available_permits(&self) -> usize {
        self.inner.admission.available_permits()
    }

    /// Top-level runs holding a permit right now.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.inner.in_flight.get()
    }

    /// The most top-level runs that ever held a permit at once.
    ///
    /// The high-water mark the admission ceiling is measured against: a run that
    /// pushed this above [`HostLimits::max_concurrent_tasks`] would be a
    /// ceiling that is not one.
    #[must_use]
    pub fn peak_in_flight(&self) -> usize {
        self.inner.peak_in_flight.get()
    }

    /// Every run admitted since the host was built, nested runs included.
    #[must_use]
    pub fn admitted(&self) -> u64 {
        self.inner.admitted.load(Ordering::Relaxed)
    }

    /// Every run refused at admission.
    #[must_use]
    pub fn refused(&self) -> u64 {
        self.inner.refused.load(Ordering::Relaxed)
    }
}

// ── HostError ────────────────────────────────────────────────────────────────

/// Why a host could not be installed, or could not be driven synchronously.
#[derive(Debug)]
#[non_exhaustive]
pub enum HostError {
    /// A ceiling outside the declared bound, including a zero deadline.
    Bound {
        /// Which ceiling.
        what: &'static str,
        /// The value given.
        value: u64,
        /// The largest value accepted.
        max: u64,
    },
    /// The synchronous entry was called inside a running async runtime.
    ///
    /// Parking the calling thread and the run's own timers and I/O never
    /// complete, so this refuses rather than deadlocking. Await [`Host::run`]
    /// instead.
    InsideRuntime,
    /// The OS refused the runtime's driver resources.
    Runtime {
        /// The operating system's refusal.
        cause: io::Error,
    },
}

impl fmt::Display for HostError {
    /// What was refused and why, naming the alternative.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Bound { what, value, max } => {
                write!(formatter, "{what}: {value} is outside 1..={max}")
            }
            Self::InsideRuntime => formatter.write_str(
                "the synchronous entry cannot park a thread an async runtime is driving; \
                 await `Host::run` on that runtime, or call it outside it",
            ),
            Self::Runtime { ref cause } => {
                write!(formatter, "the host could not build a runtime: {cause}")
            }
        }
    }
}

impl std::error::Error for HostError {
    /// The operating system's refusal, when there is one.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Bound { .. } | Self::InsideRuntime => None,
            Self::Runtime { ref cause } => Some(cause),
        }
    }
}
