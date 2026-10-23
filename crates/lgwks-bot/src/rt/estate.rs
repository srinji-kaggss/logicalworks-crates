//! The assembled estate: one host, and above the in-process tier one owned runtime and supervisor.
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
//!   [`Supervisor`](crate::rt::supervise::Supervisor) with the caller's in-flight ceiling. The host
//!   admits at most that many top-level runs and the supervisor holds at most that many tasks: one
//!   number declared once, enforced by two semaphores that are each built from it. A zero bound is
//!   refused.
//! - `Estate::durable` (the `ephemeral` feature) adds the host's run store under `dir`, so runs
//!   record durable steps (INV-BOT-54). A durable run mints its identity from the `ephemeral`
//!   entropy source, so without that feature the constructor does not exist rather than building an
//!   estate that can run nothing.
//!
//! # Reaching what the estate owns
//!
//! `host` is the tenant's [`Host`](crate::task::Host): `resume`, `submit` and `repair` are its
//! doors. `supervisor` places work on the supervisor under the estate's ceiling and `handle` drives
//! it from synchronous code, so supervised work is counted on the shutdown report.
//!
//! # Running and stopping
//!
//! `run_task` awaits a run on whatever drives it, exactly like
//! [`Host::run`](crate::task::Host::run); `run_blocking` drives it on the estate's own runtime
//! when it has one and on the host's reactor otherwise, refusing with
//! [`HostError`](crate::task::HostError) rather than deadlocking when a runtime already drives the
//! thread. `cancel` stops every run under the host and every task under the supervisor; `shutdown`
//! cancels first and then drains the supervisor to one terminal [`EstateReport`], while
//! `shutdown_blocking` is the same drain for synchronous callers and borrows the estate, so the
//! refusal inside a runtime leaves it whole for `shutdown` to finish.
//!
//! An estate dropped without either releases everything it owns without parking the dropping
//! thread: the supervisor's tasks abort, then the runtime ends in the background, and the host
//! ends its reactor the same way, so dropping inside async never panics and never leaks a worker.
//! Draining stays the shutdowns' job.
//!
//! # Evidence
//!
//! `tests/it/estate.rs` runs every door, and each claim above has a test that fails when it is
//! false: work placed on the supervisor is counted and drained, cancel stops a run and a
//! supervised task, a refused synchronous shutdown keeps the estate, every tier drops inside a
//! runtime without a panic, a durable estate's recorded step is read back after a reopen, a store
//! that cannot open is a typed refusal even inside a runtime, a case variant of a tenant never
//! opens another tenant's store file, and a new store is owner-only. `tests/it/sim_estate.rs`
//! holds the ceiling at 100, 1,000 and 10,000 seeded runs and replays by seed.
//! `src/bin/estate.rs` runs the same evening as a deployment smoke and prints the terminal record.

use core::fmt;
use core::num::NonZeroUsize;

use crate::journal::frame::SaturatingFrom;
use crate::rt::runtime::{Handle, Runtime};
use crate::rt::supervise::{Stats, Supervisor};
use crate::script::{FlowError, Scope};
use crate::task::{Host, HostError, Report, RunStore, StoreError, Task};

/// One assembled runtime for one tenant.
///
/// The supervised tiers hold their supervisor and runtime together, so an estate cannot be built
/// with one and not the other. The `Drop` impl releases them in order — the supervisor's owned
/// tasks abort before the runtime they run on ends — and the host ends its own reactor when its
/// last handle drops, so an estate dropped without `shutdown` leaves nothing running even inside
/// async.
pub struct Estate {
    /// The supervisor and the driver it runs on: absent on the in-process tier, and after a
    /// shutdown has drained them.
    driven: Option<Driven>,
    /// The tenant-scoped admission and runner every tier carries.
    host: Host,
    /// Which constructor built this estate, as `tier` reports it.
    tier: &'static str,
}

/// What the supervised tiers add to the host: an owner of concurrent work and the runtime it runs
/// on.
struct Driven {
    /// The ceiling and owner of supervised work.
    supervisor: Supervisor,
    /// The driver the supervised work and `run_blocking` run on.
    runtime: Runtime,
}

impl Driven {
    /// A supervisor with ceiling `max_in_flight` on a fresh runtime.
    ///
    /// The runtime is the one thing in an estate that holds OS threads, so every constructor calls
    /// this last: a refusal before it leaves nothing running.
    fn new(max_in_flight: usize) -> Result<Self, EstateError> {
        let runtime = Runtime::new().map_err(|cause| EstateError::Runtime { cause })?;
        Ok(Self {
            supervisor: Supervisor::new(max_in_flight),
            runtime,
        })
    }

    /// Abort the supervisor's tasks, then end the runtime without waiting for it.
    ///
    /// A multi-thread runtime dropped inside async panics, so the runtime is ended in the
    /// background; draining is the shutdowns' job and this only guarantees no leak.
    fn release(self) {
        let Self {
            supervisor,
            runtime,
        } = self;
        drop(supervisor);
        runtime.shutdown_background();
    }
}

/// The one terminal record of an estate: who ran, which tier, and what each owner counted.
///
/// `supervised` is the supervisor's drain for tasks placed on it; `runs_admitted` and
/// `runs_refused` are the host's admission counts for runs through `run_task`, `run_blocking` and
/// the doors of `host`. They count different things and are not expected to be equal.
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

/// A device refusal as the estate's store refusal.
#[cfg(feature = "ephemeral")]
fn store_io(cause: std::io::Error) -> EstateError {
    EstateError::Store {
        cause: StoreError::storage(cause),
    }
}

/// Where `RunStore::open_in` puts `tenant`'s store under `dir`.
///
/// The name is `RunStore::open_in`'s; `a_case_variant_tenant_never_shares_a_store_file` reads the
/// opened store's path back and fails if the two ever disagree.
#[cfg(feature = "ephemeral")]
fn store_file(dir: &std::path::Path, tenant: &str) -> std::path::PathBuf {
    dir.join(format!("{tenant}.runstore"))
}

/// Refuse a store file that a differently cased tenant name already owns.
///
/// Tenant names are case-sensitive but the default volumes of macOS (APFS) and Windows (NTFS) are
/// not, so `acme` and `ACME` would open one file through two independent handles and the second
/// writer would be refused as corrupt on every later append. Only a file that already exists can
/// be an alias, so a first open costs one `stat`; a reopen reads the directory once. On a
/// case-sensitive volume a differently cased file is another tenant's own store, the exact path
/// does not exist, and nothing is refused.
#[cfg(feature = "ephemeral")]
fn refuse_case_twin(store: &std::path::Path) -> Result<(), EstateError> {
    if !store.exists() {
        return Ok(());
    }
    let (Some(dir), Some(name)) = (store.parent(), store.file_name()) else {
        return Ok(());
    };
    let Some(name) = name.to_str() else {
        return Ok(());
    };
    let mut twin: Option<String> = None;
    for entry in std::fs::read_dir(dir).map_err(store_io)? {
        let stored = entry.map_err(store_io)?.file_name();
        let Some(stored) = stored.to_str() else {
            continue;
        };
        if stored == name {
            return Ok(());
        }
        if stored.eq_ignore_ascii_case(name) {
            twin = Some(stored.to_owned());
        }
    }
    let Some(twin) = twin else {
        return Ok(());
    };
    let refusal = Err(store_io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!("the store file {twin:?} already holds this tenant's name in another case"),
    )));
    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "refuse_case_twin: returning an error to the caller");
    refusal
}

/// Create `dir` and any missing parent, owner-only where the platform has modes.
///
/// A directory that already exists keeps the mode its owner gave it.
#[cfg(all(feature = "ephemeral", unix))]
fn create_private_dir(dir: &std::path::Path) -> Result<(), EstateError> {
    use std::os::unix::fs::DirBuilderExt as _;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(store_io)
}

/// Create `dir` and any missing parent.
#[cfg(all(feature = "ephemeral", not(unix)))]
fn create_private_dir(dir: &std::path::Path) -> Result<(), EstateError> {
    std::fs::create_dir_all(dir).map_err(store_io)
}

/// Make a store this estate just created owner-only (INV-FS-4): step values are a tenant's data
/// and the process umask would leave the file readable by every local user.
#[cfg(all(feature = "ephemeral", unix))]
fn make_private(file: &std::path::Path) -> Result<(), EstateError> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).map_err(store_io)
}

/// Platforms without modes keep what the store was created with.
#[cfg(all(feature = "ephemeral", not(unix)))]
fn make_private(_file: &std::path::Path) -> Result<(), EstateError> {
    Ok(())
}

impl Drop for Estate {
    /// Release the supervisor and the runtime without blocking, so an estate dropped inside async
    /// still leaves no worker behind.
    ///
    /// Draining is the shutdowns' job; this only guarantees no leak. A multi-thread runtime
    /// dropped inside async panics, so without this every async drop would be one. The host's
    /// reactor is ended by the host, after this runs, for the same reason.
    fn drop(&mut self) {
        if let Some(driven) = self.driven.take() {
            driven.release();
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
            driven: None,
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
        let driven = Driven::new(max_in_flight)?;
        Ok(Self {
            driven: Some(driven),
            host,
            tier: "supervised",
        })
    }

    /// Like [`Estate::supervised`], with the host's run store under `dir`.
    ///
    /// Two tenants pointed at one directory never share a file: the store is named by the tenant
    /// that owns it, and a tenant whose name differs from an existing store's only by case is
    /// refused where the volume cannot tell the two apart. A store this call creates is
    /// owner-only, and so is a directory it creates. Only available with the `ephemeral` feature,
    /// because a durable run mints its identity from that entropy source.
    ///
    /// # Errors
    ///
    /// As [`Estate::supervised`], plus [`EstateError::Store`] when the store cannot be opened or
    /// replayed, or its name is held by another case. The runtime is built after the store, so a
    /// refused store returns this error inside async as it does outside it.
    #[cfg(feature = "ephemeral")]
    pub fn durable(
        tenant: &str,
        max_in_flight: usize,
        dir: impl AsRef<std::path::Path>,
    ) -> Result<Self, EstateError> {
        let bound = admission_bound(max_in_flight)?;
        // Admitted before the store is touched, so a refused ceiling or tenant leaves no directory
        // behind. The host it builds is dropped at once: only its verdict is read.
        admit_host(tenant, bound)?;
        let dir = dir.as_ref();
        let store = store_file(dir, tenant);
        refuse_case_twin(&store)?;
        let existed = store.exists();
        create_private_dir(dir)?;
        let host = Host::builder(tenant)
            .map_err(invalid_tenant)?
            .max_concurrent_tasks(bound)
            .run_store(dir)
            .map_err(|cause| EstateError::Store { cause })?
            .build()
            .map_err(|cause| EstateError::Host { cause })?;
        if !existed {
            make_private(&store)?;
        }
        let driven = Driven::new(max_in_flight)?;
        Ok(Self {
            driven: Some(driven),
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

    /// The host every run goes through: `resume`, `submit` and `repair` are its doors.
    ///
    /// The borrow is the estate's own host, so a run through it is admitted under the estate's
    /// ceiling and counted on its report.
    #[must_use]
    pub const fn host(&self) -> &Host {
        &self.host
    }

    /// The supervisor that owns concurrent work under the estate's ceiling, on the supervised
    /// tiers while the estate is open.
    ///
    /// `None` is the in-process tier, or an estate a shutdown has already drained. Work placed on
    /// it is cancelled by `cancel` and drained, with its outcome counted, by the shutdowns.
    #[must_use]
    pub fn supervisor(&mut self) -> Option<&mut Supervisor> {
        self.driven.as_mut().map(|driven| &mut driven.supervisor)
    }

    /// A handle that drives futures on the estate's runtime, on the supervised tiers while the
    /// estate is open.
    ///
    /// `Handle::block_on` blocks the calling thread and panics inside async code: it is for
    /// synchronous callers, as `run_blocking` is.
    #[must_use]
    pub fn handle(&self) -> Option<Handle> {
        self.driven.as_ref().map(|driven| driven.runtime.handle())
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
        match self.driven.as_ref() {
            Some(driven) => Ok(driven.runtime.block_on(self.host.run(task, input))),
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
        if let Some(driven) = self.driven.as_ref() {
            driven.supervisor.cancel();
        }
    }

    /// Cancel, then drain the supervisor to one terminal [`EstateReport`].
    ///
    /// Cooperative bodies get room to return on their own before the abort lands, so `cancelled`
    /// and `aborted` mean what they say; the bound on that wait is the supervisor's own.
    pub async fn shutdown(mut self) -> EstateReport {
        self.host.cancel();
        let supervised = match self.driven.take() {
            Some(Driven {
                supervisor,
                runtime,
            }) => {
                let stats = supervisor.shutdown().await.stats();
                // After the drain: the workers have nothing left to finish, and ending them here
                // never parks this thread, so terminating inside async is safe where dropping
                // would panic.
                runtime.shutdown_background();
                stats
            }
            None => Stats::default(),
        };
        self.report(supervised)
    }

    /// Cancel, then drain to one terminal [`EstateReport`], blocking the calling thread.
    ///
    /// The estate's own runtime drives the drain when it has one. A runtime already driving this
    /// thread is refused rather than deadlocked, and the refusal changes nothing: the estate is
    /// still whole, so `shutdown` finishes the job. After a successful drain the estate is closed:
    /// the host is cancelled, `supervisor` and `handle` answer `None`, and a second call reports
    /// the host's counters with nothing supervised.
    ///
    /// # Errors
    ///
    /// [`HostError::InsideRuntime`](crate::task::HostError) when a runtime already drives this
    /// thread — await `shutdown` instead.
    pub fn shutdown_blocking(&mut self) -> Result<EstateReport, HostError> {
        if crate::rt::runtime::inside_runtime() {
            let refusal = Err(HostError::InsideRuntime);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "shutdown_blocking: returning an error to the caller");
            return refusal;
        }
        self.host.cancel();
        let supervised = match self.driven.take() {
            Some(Driven {
                supervisor,
                runtime,
            }) => {
                let stats = runtime.block_on(supervisor.shutdown()).stats();
                // After the drain: the workers have nothing left to finish, and the join is
                // skipped so a blocking task that never returns cannot hold this call.
                runtime.shutdown_background();
                stats
            }
            None => Stats::default(),
        };
        Ok(self.report(supervised))
    }

    /// The terminal record: who ran, which tier, what the supervisor drained and what the host
    /// admitted. The admission counters are read after the drain.
    fn report(&self, supervised: Stats) -> EstateReport {
        EstateReport {
            tenant: self.host.tenant().as_str().to_owned(),
            tier: self.tier,
            supervised,
            runs_admitted: self.host.admission().admitted(),
            runs_refused: self.host.admission().refused(),
        }
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

    /// What the supervisor drained: the counters at the moment the drain finished.
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
