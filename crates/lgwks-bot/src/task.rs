//! The front door: a [`crate::task::Host`] runs a [`crate::task::Task`] and returns a
//! [`crate::task::Report`].
//!
//! A consumer supplies a typed task and its body and receives a useful result
//! without implementing orchestration. Nothing here requires a trait
//! implementation per workflow, a `Box`, a `Pin`, an `Arc`, a `'static`
//! coercion, or a `Send` bound on the author's side, and nothing here spawns,
//! reaps, polls or ticks on the caller's behalf.
//!
//! # Host once, task often
//!
//! The host is the installed owner: it holds the [`crate::script::Tenant`], the root
//! [`CancellationToken`][stop], the admission budget, the default deadline and
//! the progress capacity. It is built once through a validated builder, its
//! ceilings are readable through [`crate::task::Host::limits`], and a clone of a host shares
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
//! A host admits at most [`crate::task::HostLimits::max_concurrent_tasks`] top-level runs at
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
//! [`crate::task::EffectKnowledge::None`]. The field exists so durable work can extend
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
//! - [`crate::task::Host::block_on`] owns a runtime once and **refuses** to run inside an
//!   existing runtime rather than parking a thread whose reactor the run needs.
//! - Parallelism across cores is a different tool
//!   ([`join_all_bounded`](crate::rt::task::join_all_bounded) on owned inputs).
//!   Here, concurrency is for waiting on many things at once.
//!
//! [stop]: crate::rt::sync::CancellationToken

use std::fmt;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::cap::{Cap, Deficit};
use crate::effect::RunId;
use crate::rt::task_local;
use crate::script::run_store::{Records, StoredValue};
use crate::script::{Durable, FlowError, MAX_IN_FLIGHT, ScriptTrail, Tenant, TrailEntry};

use self::name::TaskName;
use self::request::TerminalRecord;

mod admit;
mod builder;
mod definition;
mod execute;
mod host;
mod ledger;
mod name;
pub mod repair;
mod request;
mod store;
mod submit;

pub use builder::{HostBuilder, HostError, HostLimits};
pub use host::Host;

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
        Disposition::Failed | Disposition::DeadlineExceeded => {
            // A report carries an error exactly when it did not succeed
            // (`Terminal`), so a stopped run with none is a report that
            // disagrees with itself: refuse to record it rather than record an
            // empty location as though it were one.
            let error = report
                .error()
                .ok_or(RequestError::Record(FlowError::failed(
                    "a stopped run reported no error to record",
                )))?;
            Some(TerminalRecord::stopped(
                report.disposition(),
                error.at(),
                error.to_string(),
            ))
        }
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
/// boxing appears in the author's code. The body takes the run's [`Scope`](crate::script::Scope) and
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
    /// The blunt form of [`Scope::require`](crate::script::Scope::require): the
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
    /// The retained steps with their script lines and outcomes, oldest first:
    /// the same entries as `steps`, one for one.
    trail: Vec<TrailEntry>,
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

    /// The retained steps, oldest first, each with the script line that
    /// produced it and how it ended: the entries behind [`Report::steps`].
    #[must_use]
    pub fn trail(&self) -> &[TrailEntry] {
        &self.trail
    }

    /// The run read back as its script: one line per retained step, each with
    /// its source line and outcome. See [`ScriptTrail`].
    ///
    /// ```
    /// # use lgwks_bot::script::Scope;
    /// # use lgwks_bot::task::{Host, task};
    /// lgwks_bot::script! {
    ///     flow double(value: u32) -> u32:
    ///         step multiply:
    ///             value.saturating_mul(2)
    /// }
    /// let host = Host::builder("acme")?.build()?;
    /// let doubling = task("doubling", |scope: Scope, value: u32| async move {
    ///     double(&scope, value).await
    /// })?;
    /// let report = lgwks_bot::block_on(host.run(&doubling, 21));
    /// assert_eq!(report.result().ok().copied(), Some(42));
    /// let rendered = report.script_trail().to_string();
    /// assert!(rendered.contains("flow double(value: u32) -> u32  -> ok"), "{rendered}");
    /// assert!(rendered.contains("step multiply  -> ok"), "{rendered}");
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn script_trail(&self) -> ScriptTrail<'_> {
        ScriptTrail::new(&self.trail, self.dropped_steps)
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
