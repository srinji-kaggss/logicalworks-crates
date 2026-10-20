//! The validated construction of a `Host`, the ceilings it resolves, and
//! the errors building or driving one can report.

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{fmt, io};

use crate::gate::GrantSet;
use crate::journal::frame::SaturatingFrom;
use crate::registry::DomainRegistry;
use crate::rt::clock::Clock;
use crate::rt::sync::{CancellationToken, Semaphore};
use crate::script::{Policy, Tenant};

use super::admit::{HighWater, LiveRuns};
use super::host::Installation;
use super::{
    HOST_IDENTITIES, Host, MAX_ADMITTED_TASKS, MAX_PROGRESS_STEPS, MAX_TASK_DEADLINE, RunLedger,
    StoreError, store,
};

// ── HostBuilder ─────────────────────────────────────────────────────────────

/// The validated construction of one [`Host`].
///
/// `#[must_use]` because dropping a builder unused builds no host, and a host
/// that was never installed is a tenant nobody can run for.
#[must_use = "a HostBuilder builds nothing until `build` is called"]
#[derive(Debug)]
pub struct HostBuilder {
    /// Who the host runs for.
    pub(super) tenant: Tenant,
    /// The admission ceiling.
    pub(super) max_concurrent: NonZeroUsize,
    /// The default deadline every run inherits.
    pub(super) deadline: Duration,
    /// The step trail's capacity.
    pub(super) progress: usize,
    /// The clock every run's deadline is measured on.
    pub(super) clock: Clock,
    /// The base authority every run is admitted against.
    ///
    /// Checked against a task's declared [`needs`](Task::needs) at admission, as
    /// one complete pass: every unmet capability is reported together, so a
    /// blocked run names the whole shortfall and its repair ticket is written once
    /// rather than one refusal at a time.
    pub(super) grants: GrantSet,
    /// The registry a flow's domain words resolve against, when installed.
    pub(super) domains: Option<&'static DomainRegistry>,
    /// The durable step-record store, when the caller installed one.
    pub(super) store: Option<store::RunStore>,
    /// The declared schema id of this host's durable step values.
    ///
    /// The default is [`UNVERSIONED_CODEC`], which says the caller declared none
    /// rather than that the values are schema-free; [`HostBuilder::durable_codec`]
    /// changes it once, for every run this host admits.
    pub(super) codec: String,
    /// The durable repair ledger, when the caller installed one.
    pub(super) ledger: Option<RunLedger>,
    /// The most root attempts one run may be charged before it is refused.
    pub(super) repair_attempts: u64,
    /// The most root spend one run may be charged before it is refused.
    pub(super) repair_spend: u64,
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
    /// admission wait, and the [`within`](crate::script::within) around the body reads it for the body
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
    /// The default is the `script` module's [`DEFAULT_TRAIL_STEPS`](crate::script::DEFAULT_TRAIL_STEPS). A report that
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
    /// run id, [`Report::run_id`](crate::task::Report::run_id) names it, and [`Host::resume`] replays the
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
    /// wants to get the opened handle back for [`RunStore::tenant_of`](crate::task::RunStore::tenant_of) and the
    /// other read-only queries.
    pub fn store(mut self, store: store::RunStore) -> Self {
        self.store = Some(store);
        self
    }

    /// Declare the schema id this host's durable step values are written under.
    ///
    /// Every [`DefinitionIdentity`](crate::task::DefinitionIdentity) this host builds carries it, so changing the
    /// schema of a durable value is one setting here rather than an argument to
    /// every run — and a resume whose records were written under another schema
    /// is refused as [`Drift::Schema`](crate::task::Drift::Schema) rather than decoding them as this one.
    ///
    /// The convention is `lgwks.bot.schema.v1.<kind>`, the same one
    /// [`crate::effect::InputIdentity`] uses. The default is
    /// [`UNVERSIONED_CODEC`](crate::task::UNVERSIONED_CODEC), which says the caller declared none.
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

    /// Resolve every run's `observe` and `act` against `registry` (#388).
    ///
    /// The same `static` a [`BotSpec`](crate::spec::BotSpec) materializes
    /// against, so a flow and a spec that name one identifier reach one
    /// constructor. The default is none: a flow that names a domain on a host
    /// with no registry is refused at its entry, naming the absence, rather
    /// than reaching some registry the caller did not choose. A registry
    /// decides only which constructor an identifier builds; what the built
    /// domain may reach is still [`HostBuilder::grants`].
    pub fn domains(mut self, registry: &'static DomainRegistry) -> Self {
        self.domains = Some(registry);
        self
    }

    /// Install a durable repair ledger under `dir`.
    ///
    /// One file per tenant, named after it, so two tenants pointed at the same
    /// directory never share a file — the same rule the step store follows. With
    /// a ledger installed a blocked run's report carries a
    /// [`RepairTicket`](crate::task::RepairTicket) and [`Host::repair`] can decide a
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
    /// refused with [`RepairError::BudgetSpent`](crate::task::RepairError::BudgetSpent) however many times it is
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
                policy: Arc::new(Policy::for_this_host(self.clock.clone())),
                clock: self.clock,
                store: self.store,
                codec: self.codec,
                ledger: self.ledger,
                grants: self.grants,
                domains: self.domains,
                repair_attempts: self.repair_attempts,
                repair_spend: self.repair_spend,
                reactor: Mutex::new(None),
            }),
        })
    }
}

/// The admission ceiling a host uses when the builder says nothing.
pub(super) fn default_max_concurrent() -> NonZeroUsize {
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
pub(super) fn default_deadline() -> Duration {
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
pub(super) fn default_repair_attempts() -> u64 {
    8
}

/// The most root spend a run is charged before it is refused, when the builder
/// says nothing.
///
/// A multiple of [`default_repair_attempts`], so a caller that never names a spend
/// gets a budget bounded by both axes rather than one that silently governs.
pub(super) fn default_repair_spend() -> u64 {
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
    pub(super) max_concurrent: NonZeroUsize,
    /// The deadline every run inherits.
    pub(super) deadline: Duration,
    /// The step trail's capacity.
    pub(super) progress: usize,
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
    pub(super) inner: &'installation Installation,
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
