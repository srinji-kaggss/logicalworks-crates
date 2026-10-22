//! The assembled estate: one owned runtime, one supervisor and one host.
//!
//! [`Runtime`](crate::rt::runtime::Runtime) drives, [`Supervisor`](crate::rt::supervise::Supervisor)
//! owns concurrent work under a ceiling, and [`Host`](crate::task::Host) admits and runs
//! tenant-scoped tasks. Each already owns its piece; this module owns the assembly, so a caller
//! declares a tier once instead of wiring three builders and risking their ceilings and shutdown
//! order disagreeing.
//!
//! # Tiers
//!
//! - `Estate::in_process` is a [`Host`](crate::task::Host) alone: no runtime and no supervisor.
//!   Runs go through the host's lazily built reactor, so a caller with no async surface still runs.
//! - `Estate::supervised` adds the owned [`Runtime`](crate::rt::runtime::Runtime) and a
//!   [`Supervisor`](crate::rt::supervise::Supervisor) with the caller's in-flight ceiling, and the
//!   host admits under that same ceiling: one bound declared once. A zero bound is refused.
//! - `Estate::durable` adds the host's run store under `dir`, so runs record durable steps
//!   (INV-BOT-54).
//!
//! # Running and stopping
//!
//! `run_task` awaits a run on whatever drives it, exactly like
//! [`Host::run`](crate::task::Host::run); `run_blocking` drives it on the estate's own runtime
//! when it has one and on the host's reactor otherwise, refusing with
//! [`HostError`](crate::task::HostError) rather than deadlocking when a runtime already drives the
//! thread. `cancel` stops every run under the host and every task under the supervisor; `shutdown`
//! cancels first and then drains the supervisor to one terminal [`EstateReport`], while
//! `shutdown_blocking` is the same drain for synchronous callers. An estate dropped without either
//! still terminates its runtime in the background, so dropping inside async never panics and never
//! leaks a worker; draining stays the shutdowns' job. An in-process estate that ran through
//! `run_blocking` holds the host's lazily built reactor, so it is shut down with
//! `shutdown_blocking` from synchronous code.
//!
//! The report carries the supervisor's counters for supervised work and the host's admission
//! counts for runs: a run through `run_task` or `run_blocking` is admitted by the host, so it
//! reads on `runs_admitted`, while `supervised` stays the record of tasks placed on the supervisor
//! itself.
//!
//! # Evidence
//!
//! `tests/it/estate.rs` runs every door: the three tiers declare what they own, an empty tenant is
//! a typed refusal, a run succeeds and the shutdown report accounts for it, cancel drains to an
//! empty terminal record, two tenants share one task name without sharing anything, a store that
//! cannot open is a typed refusal, a synchronous shutdown inside a runtime is refused, a ceiling
//! past the host's bound is a typed refusal, and a zero bound is a typed refusal.
//! `src/bin/estate.rs` runs the same evening as a deployment smoke: every tier, both doors, every
//! refusal above, and the printed terminal record.

use core::fmt;
use core::num::NonZeroUsize;

use crate::journal::frame::SaturatingFrom;
use crate::rt::runtime::Runtime;
use crate::rt::supervise::{Stats, Supervisor};
use crate::script::{FlowError, Scope};
use crate::task::{Host, HostError, Report, RunStore, StoreError, Task};

/// One assembled runtime for one tenant.
///
/// The fields drop in declaration order — the supervisor's owned tasks abort before the runtime
/// they run on goes away — and the `Drop` impl terminates that runtime in the background, so an
/// estate dropped without `shutdown` leaves nothing running even inside async.
pub struct Estate {
    /// The ceiling and owner of supervised work, absent on the in-process tier.
    supervisor: Option<Supervisor>,
    /// The driver the supervised tiers run on, absent on the in-process tier.
    runtime: Option<Runtime>,
    /// The tenant-scoped admission and runner every tier carries.
    host: Host,
    /// Which constructor built this estate, as `tier` reports it.
    tier: &'static str,
}

/// The one terminal record of an estate: who ran, which tier, and two countings that agree.
///
/// `supervised` is the supervisor's drain for tasks placed on it; `runs_admitted` and
/// `runs_refused` are the host's admission counts for runs through `run_task` and `run_blocking`.
pub struct EstateReport {
    /// The tenant this estate ran for.
    tenant: String,
    /// Which constructor built the estate.
    tier: &'static str,
    /// What the supervisor drained.
    supervised: Stats,
    /// Every run the host admitted since the estate was built, nested runs included.
    runs_admitted: u64,
    /// Every run refused at the host's admission.
    runs_refused: u64,
}

/// Why an estate could not be built.
#[derive(Debug)]
#[non_exhaustive]
pub enum EstateError {
    /// The tenant name did not validate.
    InvalidTenant {
        /// What is wrong with the name.
        reason: &'static str,
    },
    /// The OS refused the estate runtime's driver resources.
    Runtime {
        /// The operating system's refusal.
        cause: std::io::Error,
    },
    /// The host refused its ceilings.
    Host {
        /// The host's refusal.
        cause: HostError,
    },
    /// The run store could not be opened or replayed.
    Store {
        /// The store's refusal.
        cause: StoreError,
    },
}

impl fmt::Display for EstateError {
    /// What was refused and why, naming the alternative.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::InvalidTenant { reason } => {
                write!(
                    formatter,
                    "lgwks_bot: the estate tenant is refused: {reason}"
                )
            }
            Self::Runtime { ref cause } => {
                write!(
                    formatter,
                    "lgwks_bot: the OS refused the estate runtime: {cause}"
                )
            }
            Self::Host { ref cause } => {
                write!(formatter, "lgwks_bot: the estate host is refused: {cause}")
            }
            Self::Store { ref cause } => {
                write!(
                    formatter,
                    "lgwks_bot: the estate run store is refused: {cause}"
                )
            }
        }
    }
}

impl std::error::Error for EstateError {
    /// The host's, store's or operating system's refusal, when there is one.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::InvalidTenant { .. } => None,
            Self::Runtime { ref cause } => Some(cause),
            Self::Host { ref cause } => Some(cause),
            Self::Store { ref cause } => Some(cause),
        }
    }
}

/// Map the one failure `Host::builder` documents to the estate's refusal.
///
/// The builder validates nothing but the tenant, so any other arm is collapsed into the same
/// typed refusal rather than grown into a second variant: the caller learns the tenant was
/// refused either way.
fn invalid_tenant(cause: FlowError) -> EstateError {
    match cause {
        FlowError::InvalidTenant { reason } => EstateError::InvalidTenant { reason },
        _ => EstateError::InvalidTenant {
            reason: "the tenant name was refused",
        },
    }
}

/// The estate's one declared bound as the host's non-zero admission ceiling.
///
/// A zero bound is refused in the builder's own shape rather than clamped into a ceiling of one:
/// a bound of zero admits nothing, and admitting one would answer a different question than the
/// caller asked.
fn admission_bound(max_in_flight: usize) -> Result<NonZeroUsize, EstateError> {
    let Some(bound) = NonZeroUsize::new(max_in_flight) else {
        let refusal = Err(EstateError::Host {
            cause: HostError::Bound {
                what: "the admission ceiling",
                value: 0,
                max: u64::saturating_from(crate::task::MAX_ADMITTED_TASKS),
            },
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "admission_bound: returning an error to the caller");
        return refusal;
    };
    Ok(bound)
}

/// One host for `tenant` admitting at most `bound` top-level runs: the ceiling the estate declares
/// once and the supervisor carries.
fn admit_host(tenant: &str, bound: NonZeroUsize) -> Result<Host, EstateError> {
    Host::builder(tenant)
        .map_err(invalid_tenant)?
        .max_concurrent_tasks(bound)
        .build()
        .map_err(|cause| EstateError::Host { cause })
}

/// One terminal record from its four facts: who ran, which tier, what the supervisor drained and
/// what the host admitted. The admission counters are read after the drain, so the two countings
/// agree with each other.
fn assemble(host: &Host, tenant: String, tier: &'static str, supervised: Stats) -> EstateReport {
    EstateReport {
        tenant,
        tier,
        supervised,
        runs_admitted: host.admission().admitted(),
        runs_refused: host.admission().refused(),
    }
}

impl Drop for Estate {
    /// Terminate the owned runtime without blocking, so an estate dropped inside async still
    /// leaves no worker behind.
    ///
    /// Draining is the shutdowns' job; this only guarantees no leak. A multi-thread runtime
    /// dropped inside async panics, so without this every async drop would be one; the background
    /// termination ends the workers without ever parking the dropping thread.
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

impl Estate {
    /// A host alone for `tenant`: no runtime, no supervisor, no store.
    ///
    /// Runs go through the host's lazily built reactor, so this tier runs anywhere the host does.
    ///
    /// # Errors
    ///
    /// [`EstateError::InvalidTenant`] naming what is wrong with `tenant`.
    pub fn in_process(tenant: &str) -> Result<Self, EstateError> {
        let host = Host::builder(tenant)
            .map_err(invalid_tenant)?
            .build()
            .map_err(|cause| EstateError::Host { cause })?;
        Ok(Self {
            supervisor: None,
            runtime: None,
            host,
            tier: "in-process",
        })
    }

    /// An owned runtime and a supervisor with ceiling `max_in_flight` around the host for `tenant`.
    ///
    /// The ceiling is declared once: the host admits at most `max_in_flight` top-level runs and
    /// the supervisor holds at most `max_in_flight` in flight.
    ///
    /// # Errors
    ///
    /// [`EstateError::InvalidTenant`] naming what is wrong with `tenant`;
    /// [`EstateError::Runtime`] when the OS refuses the runtime's driver resources;
    /// [`EstateError::Host`] for a zero bound or a ceiling outside the host's declared bound.
    pub fn supervised(tenant: &str, max_in_flight: usize) -> Result<Self, EstateError> {
        let bound = admission_bound(max_in_flight)?;
        let host = admit_host(tenant, bound)?;
        // Built after the host admits its ceilings, so a refusal leaves no runtime behind.
        let runtime = Runtime::new().map_err(|cause| EstateError::Runtime { cause })?;
        Ok(Self {
            supervisor: Some(Supervisor::new(max_in_flight)),
            runtime: Some(runtime),
            host,
            tier: "supervised",
        })
    }

    /// Like [`Estate::supervised`], with the host's run store under `dir`.
    ///
    /// Two tenants pointed at one directory never share a file: the store is named by the tenant
    /// that owns it. Running on a durable estate mints a run identity from the `ephemeral`
    /// entropy source, so the `ephemeral` feature must be on: without it the estate builds, but
    /// every run fails typed at the run rather than running unrecorded.
    ///
    /// # Errors
    ///
    /// As [`Estate::supervised`], plus [`EstateError::Store`] when the store cannot be opened or
    /// replayed.
    pub fn durable(
        tenant: &str,
        max_in_flight: usize,
        dir: impl AsRef<std::path::Path>,
    ) -> Result<Self, EstateError> {
        let bound = admission_bound(max_in_flight)?;
        // Admitted before the store is touched, so a refused ceiling leaves no directory behind.
        // The host it builds is dropped at once: only its verdict is read.
        admit_host(tenant, bound)?;
        // Built after the host admits its ceilings, so a refusal leaves no runtime behind.
        let runtime = Runtime::new().map_err(|cause| EstateError::Runtime { cause })?;
        let host = Host::builder(tenant)
            .map_err(invalid_tenant)?
            .max_concurrent_tasks(bound)
            .run_store(dir)
            .map_err(|cause| EstateError::Store { cause })?
            .build()
            .map_err(|cause| EstateError::Host { cause })?;
        Ok(Self {
            supervisor: Some(Supervisor::new(max_in_flight)),
            runtime: Some(runtime),
            host,
            tier: "durable",
        })
    }

    /// The tenant this estate runs for.
    #[must_use]
    pub fn tenant(&self) -> &str {
        self.host.tenant().as_str()
    }

    /// Which constructor built this estate: `in-process`, `supervised` or `durable`.
    #[must_use]
    pub const fn tier(&self) -> &'static str {
        self.tier
    }

    /// Whether this estate owns a runtime: every tier but the in-process one.
    #[must_use]
    pub const fn has_runtime(&self) -> bool {
        self.runtime.is_some()
    }

    /// Whether this estate owns a supervisor: every tier but the in-process one.
    #[must_use]
    pub const fn has_supervisor(&self) -> bool {
        self.supervisor.is_some()
    }

    /// The durable step-record store this estate installed, when it has one.
    ///
    /// `None` is an estate that keeps nothing across a process boundary. The handle is the host's
    /// own clone, so a caller reading from another task sees the same index.
    #[must_use]
    pub fn run_store(&self) -> Option<&RunStore> {
        self.host.run_store()
    }

    /// Await one run of `task` on `input`, exactly like [`Host::run`](crate::task::Host::run).
    ///
    /// The run is admitted by the host, so the shutdown report counts it on `runs_admitted`. It
    /// needs a driver: await it on the estate's runtime, or drive it with `run_blocking`.
    pub async fn run_task<I, O, F, Fut>(&self, task: &Task<F>, input: I) -> Report<O>
    where
        F: Fn(Scope, I) -> Fut,
        Fut: Future<Output = Result<O, FlowError>>,
    {
        self.host.run(task, input).await
    }

    /// Drive one run of `task` on `input` to completion, blocking the calling thread.
    ///
    /// The estate's own runtime drives when it has one; the host's lazily built reactor drives
    /// otherwise.
    ///
    /// # Errors
    ///
    /// [`EstateError`] is not the channel here: [`HostError::InsideRuntime`](crate::task::HostError)
    /// when a runtime already drives this thread — await `run_task` instead — and
    /// [`HostError::Runtime`](crate::task::HostError) when the OS refuses the driver's resources.
    pub fn run_blocking<I, O, F, Fut>(
        &self,
        task: &Task<F>,
        input: I,
    ) -> Result<Report<O>, HostError>
    where
        F: Fn(Scope, I) -> Fut,
        Fut: Future<Output = Result<O, FlowError>>,
    {
        if crate::rt::runtime::inside_runtime() {
            let refusal = Err(HostError::InsideRuntime);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "run_blocking: returning an error to the caller");
            return refusal;
        }
        match self.runtime.as_ref() {
            Some(runtime) => Ok(runtime.block_on(self.host.run(task, input))),
            None => self.host.block_on(task, input),
        }
    }

    /// Stop every run under the host and every task under the supervisor.
    ///
    /// Idempotent: a second call does nothing, and the tokens observe the change immediately on
    /// every thread. Stopping is not draining: `shutdown` or `shutdown_blocking` still reports
    /// how everything ended.
    pub fn cancel(&self) {
        self.host.cancel();
        if let Some(supervisor) = self.supervisor.as_ref() {
            supervisor.cancel();
        }
    }

    /// Cancel, then drain the supervisor to one terminal [`EstateReport`].
    ///
    /// Cooperative bodies get room to return on their own before the abort lands, so `cancelled`
    /// and `aborted` mean what they say; the bound on that wait is the supervisor's own.
    pub async fn shutdown(mut self) -> EstateReport {
        self.host.cancel();
        let tenant = self.host.tenant().as_str().to_owned();
        let supervised = match self.supervisor.take() {
            Some(supervisor) => supervisor.shutdown().await.stats(),
            None => Supervisor::default().stats(),
        };
        let report = assemble(&self.host, tenant, self.tier, supervised);
        // After the drain: the workers have nothing left to finish, and ending them here never
        // parks this thread, so terminating inside async is safe where dropping would panic.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
        report
    }

    /// Cancel, then drain to one terminal [`EstateReport`], blocking the calling thread.
    ///
    /// The estate's own runtime drives the drain when it has one; a runtime already driving this
    /// thread is refused rather than deadlocked.
    ///
    /// # Errors
    ///
    /// [`HostError::InsideRuntime`](crate::task::HostError) when a runtime already drives this
    /// thread — await `shutdown` instead.
    pub fn shutdown_blocking(mut self) -> Result<EstateReport, HostError> {
        if crate::rt::runtime::inside_runtime() {
            // Refused — and the estate still drops here, inside async, which is exactly what the
            // `Drop` impl is for: the runtime is terminated in the background, so no worker leaks
            // past the refusal.
            let refusal = Err(HostError::InsideRuntime);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "shutdown_blocking: returning an error to the caller");
            return refusal;
        }
        self.host.cancel();
        let tenant = self.host.tenant().as_str().to_owned();
        let supervised = match self.supervisor.take() {
            Some(supervisor) => match self.runtime.take() {
                // The constructors pair a supervisor with a runtime, so this arm is the only one
                // a caller can reach; the other reports empty rather than panicking.
                Some(runtime) => runtime.block_on(supervisor.shutdown()).stats(),
                None => Supervisor::default().stats(),
            },
            None => Supervisor::default().stats(),
        };
        let report = assemble(&self.host, tenant, self.tier, supervised);
        // After the drain: the workers have nothing left to finish.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
        Ok(report)
    }
}

impl EstateReport {
    /// The tenant the shut-down estate ran for.
    #[must_use]
    pub fn tenant(&self) -> &str {
        &self.tenant
    }

    /// Which constructor built the shut-down estate.
    #[must_use]
    pub const fn tier(&self) -> &'static str {
        self.tier
    }

    /// What the supervisor drained: one outcome per supervised task, in completion order, with the
    /// counters at the moment the drain finished.
    #[must_use]
    pub const fn supervised(&self) -> Stats {
        self.supervised
    }

    /// Every run the host admitted since the estate was built, nested runs included.
    #[must_use]
    pub const fn runs_admitted(&self) -> u64 {
        self.runs_admitted
    }

    /// Every run refused at the host's admission.
    #[must_use]
    pub const fn runs_refused(&self) -> u64 {
        self.runs_refused
    }
}
