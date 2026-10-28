//! The installed owner: `Host`, the state its handles share, the plan one
//! call executes, and the doors that build, read and run through it.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Instant;

use lgwks_std::hash::{Digest, Hasher};

use crate::effect::RunId;
use crate::gate::GrantSet;
use crate::registry::DomainRegistry;
use crate::rt::clock::Clock;
use crate::rt::sync::{CancellationToken, Semaphore};
use crate::script::{DEFAULT_TRAIL_STEPS, Durable, FlowError, Policy, Scope, Tenant};

use super::admit::{HighWater, LiveRuns};
use super::builder::{
    Admission, default_deadline, default_max_concurrent, default_repair_attempts,
    default_repair_spend,
};
use super::{
    DefinitionIdentity, HostBuilder, HostIdentity, HostLimits, Report, RunLedger, Task, Ticket,
    UNVERSIONED_CODEC, store,
};

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
/// `a_host_reads_back_its_limits_and_a_clone_shares_the_budget` (`tests/it/bot_front_door.rs`)
/// runs it: a host for `acme` with eight concurrent tasks and a five-second default deadline reads
/// both back, a clone sees the same eight permits, and `cancel` is observed by `is_cancelled`.
#[derive(Clone)]
pub struct Host {
    /// Everything the handles of one host share: the budget, the counters, the
    /// tenant, the stop token and the ceilings. The runtime is the process
    /// driver's, never the host's.
    pub(super) inner: Arc<Installation>,
}

/// The state one host owns, shared by every clone of its handles.
pub(super) struct Installation {
    /// This host's admission identity, unique per host for the life of the
    /// process. Nested-run accounting is keyed on it.
    pub(super) identity: HostIdentity,
    /// Who this host runs for.
    pub(super) tenant: Tenant,
    /// Cancelling this stops every run under the host, in flight or waiting.
    pub(super) token: CancellationToken,
    /// The finite ceilings this host resolves unspecified values from.
    pub(super) limits: HostLimits,
    /// The admission budget: at most `limits.max_concurrent_tasks()` top-level
    /// runs hold a permit at once. Shared with its clones by the `Arc` that
    /// owns the installation, and cloned here for [`Semaphore::acquire_owned`],
    /// which is what lets a run release its permit from any exit path.
    pub(super) admission: Arc<Semaphore>,
    /// Top-level runs holding a permit right now. Counting them here rather than
    /// in a guard keeps the count correct when a run's future is dropped while
    /// suspended.
    pub(super) in_flight: LiveRuns,
    /// The most top-level runs that ever held a permit at once.
    pub(super) peak_in_flight: HighWater,
    /// Every run admitted since the host was built, nested runs included.
    pub(super) admitted: AtomicU64,
    /// Every run refused at admission.
    pub(super) refused: AtomicU64,
    /// The clock every run's deadline is measured on.
    ///
    /// Read by the admission wait and handed to each run's root scope, so one
    /// timeline governs the whole host rather than each step choosing its own.
    pub(super) clock: Clock,
    /// The retry budget every run this host admits shares.
    ///
    /// One budget for the host, not one per run: a host that admits many runs
    /// against one failing backend holds one reserve, not one per run, so the
    /// load a retry storm adds does not scale with concurrency. Handed to each
    /// run's root scope beside the clock.
    pub(super) policy: Arc<Policy>,
    /// The durable step-record store, when one was installed.
    ///
    /// `None` is the honest local mode: no record survives the process, so no
    /// report claims a run id and no ticket exists. A host with a store in hand
    /// that behaved as though it had none would be claiming durability it does
    /// not have, which is the one thing INV-BOT-54 forbids.
    pub(super) store: Option<store::RunStore>,
    /// The declared schema id of this host's durable step values.
    ///
    /// What a [`DefinitionIdentity`] this host builds by hand starts from, so the
    /// declared schema is one setting rather than one argument per call. The
    /// default is [`UNVERSIONED_CODEC`], which says the caller declared none
    /// rather than that the values are schema-free.
    pub(super) codec: String,
    /// The durable repair ledger, when one was installed.
    ///
    /// `None` means this host cannot repair anything: a repair is decided
    /// against a run's epoch, its root budget and the set of tickets already
    /// applied to it, and a host with no ledger has none of those. Such a host
    /// refuses every repair rather than applying one unchecked, which is the
    /// difference between a repair and a widened authority.
    pub(super) ledger: Option<RunLedger>,
    /// The capabilities this host's grant set carries.
    ///
    /// The base authority every run is admitted against, before any repair. A
    /// repair never widens this: it authorizes one run, once, for the needs its
    /// ticket named, and the next run on this host is admitted against the same
    /// grant.
    pub(super) grants: GrantSet,
    /// The registry a flow's `observe` and `act` resolve against, when the
    /// caller installed one (#388).
    pub(super) domains: Option<&'static DomainRegistry>,
    /// The most root attempts one run may be charged before it is refused.
    pub(super) repair_attempts: u64,
    /// The most root spend one run may be charged before it is refused.
    pub(super) repair_spend: u64,
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
pub(super) enum Plan {
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
    pub(super) fn declared(&self) -> Option<&DefinitionIdentity> {
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
    pub(super) const fn run(&self) -> Option<RunId> {
        match *self {
            Self::Fresh | Self::Declared(_) => None,
            Self::Resume(run) | Self::ResumeUnder(run, _) => Some(run),
        }
    }
}

impl Host {
    /// Begin building a host for `tenant`.
    ///
    /// The tenant is validated by [`Tenant::new`], so a name that would corrupt
    /// every step key and every error location is refused here rather than at
    /// the first run. The caller is the trusted host: the name is a partition
    /// key it chooses, not an authenticated principal, and whoever builds with
    /// the same string runs as the same tenant.
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
            domains: None,
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
    pub(super) fn identity_for(
        &self,
        run: RunId,
        declared: &DefinitionIdentity,
    ) -> DefinitionIdentity {
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
    pub(super) fn definition_for(
        &self,
        task_name: &str,
        run: Option<&RunId>,
    ) -> DefinitionIdentity {
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
    /// `a_host_names_the_definition_every_run_records_against` (`tests/it/bot_front_door.rs`) runs it:
    /// a definition for task `nightly` at revision 3 with a declared input digest and five steps reads
    /// its revision and step count back.
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
    /// default deadline applied with [`within`](crate::script::within) — so an overrun is located at
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
    pub(super) async fn run_plan<I, O, F, Fut>(
        &self,
        task: &Task<F>,
        input: I,
        plan: Plan,
    ) -> Report<O>
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
    /// None returned: a refusal is a [`Disposition::Refused`](crate::task::Disposition::Refused) report with no
    /// step run, given when this host has no store, when the run belongs to
    /// another tenant, and when a store host cannot mint a run identity. A store
    /// that refuses the terminal record of a request is a [`Disposition::Failed`](crate::task::Disposition::Failed)
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
    /// [`Disposition::Refused`](crate::task::Disposition::Refused) report carrying [`FlowError::Incompatible`], with
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
