/// What a [`Supervisor::shutdown`] drained, and the counters behind it.
///
/// Returned rather than discarded because shutdown is exactly the moment an
/// owner decides what to do about failure: a panic that is invisible at that
/// point is a panic nobody will act on.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ShutdownReport {
    /// One outcome per task that was live or unreported when shutdown began,
    /// in completion order.
    outcomes: Vec<TaskOutcome>,
    /// The supervisor's counters at the moment it finished draining.
    stats: Stats,
    /// Keeps transferred cleanup obligations and their leases alive.
    #[cfg(all(unix, feature = "process"))]
    cleanup_owners: Arc<CleanupOwners>,
}

impl ShutdownReport {
    /// Every drained outcome, in completion order.
    #[must_use]
    pub fn outcomes(&self) -> &[TaskOutcome] {
        &self.outcomes
    }

    /// Consume the report only when no cleanup owner remains. On `Err`, the
    /// returned report still owns every pending cleanup lease and can be polled
    /// with `reap_pending_cleanups`.
    pub fn into_outcomes(self) -> Result<Vec<TaskOutcome>, Self> {
        #[cfg(all(unix, feature = "process"))]
        if self.pending_cleanup_count() > 0 {
            let refusal = Err(self);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "into_outcomes: returning an error to the caller");
            return refusal;
        }
        Ok(self.outcomes)
    }

    /// The supervisor's counters at the moment it finished draining.
    #[must_use]
    pub const fn stats(&self) -> Stats {
        self.stats
    }

    /// Every task that panicked, in completion order.
    pub fn panicked(&self) -> impl Iterator<Item = &TaskOutcome> {
        self.outcomes.iter().filter(|outcome| outcome.is_panic())
    }

    /// Whether every drained task returned normally.
    ///
    /// Cancellation counts as *not* clean, because a caller asking this
    /// question is asking whether the work finished, and a cancelled task did
    /// not. So does a supervised process that exited non-zero: it ended, and
    /// the caller's question is whether the work succeeded, not whether the
    /// task stopped. So, too, does a supervised process whose containment report
    /// is not complete (`Containment::is_complete`, behind the `process`
    /// feature) — including one that exited on its own,
    /// since nothing can say whether it left a `setsid` descendant running.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.outcomes.iter().all(|outcome| {
            outcome.is_success() && outcome.cleanup().is_none_or(CleanupReceipt::is_complete)
        }) && {
            #[cfg(all(unix, feature = "process"))]
            {
                self.pending_cleanup_count() == 0
                    && self
                        .outcomes
                        .iter()
                        .all(|outcome| outcome.containment().is_none_or(Containment::is_complete))
            }
            // A job object retains no cleanup obligation past its task: closing
            // the job kills its tree, so the containment reports are the whole
            // evidence.
            #[cfg(all(not(unix), feature = "process"))]
            {
                self.outcomes
                    .iter()
                    .all(|outcome| outcome.containment().is_none_or(Containment::is_complete))
            }
            #[cfg(not(feature = "process"))]
            {
                true
            }
        }
    }

    /// Every supervised process that did not exit successfully, in completion
    /// order.
    ///
    /// The counterpart of [`ShutdownReport::panicked`] for
    /// `Supervisor::spawn_process` (behind the `process` feature): a report
    /// whose only readable failure is a panic would leave a command that exited
    /// non-zero to be found by hand-filtering [`ShutdownReport::outcomes`].
    pub fn failed(&self) -> impl Iterator<Item = &TaskOutcome> {
        self.outcomes
            .iter()
            .filter(|outcome| outcome.is_process_failure())
    }

    /// Number of cleanup obligations still charged to the process capacity.
    #[cfg(all(unix, feature = "process"))]
    #[must_use]
    pub fn pending_cleanup_count(&self) -> usize {
        self.cleanup_owners.pending_count()
    }

    /// Make one bounded observation pass over cleanup obligations retained by
    /// this report. Each returned outcome is a terminal absence receipt.
    #[cfg(all(unix, feature = "process"))]
    pub fn reap_pending_cleanups(&mut self) -> usize {
        self.reap_pending_cleanups_with(&NATIVE_GROUP_OBSERVER)
    }

    #[cfg(all(unix, feature = "process"))]
    /// Injected-observer seam for deterministic cleanup-owner tests.
    fn reap_pending_cleanups_with(&mut self, observer: &dyn GroupObserver) -> usize {
        let settled = self.cleanup_owners.drive(observer, |_| false);
        let count = settled.len();
        self.outcomes
            .extend(
                settled
                    .into_iter()
                    .map(|(task, containment)| TaskOutcome::CleanupSettled {
                        task,
                        cleanup: settled_receipt(&containment),
                        containment,
                    }),
            );
        count
    }
}

/// What a [`Supervisor`] has done so far.
///
/// Counters saturate rather than wrap, so they stay monotonic over any lifetime
/// a process can actually reach. `completed` counts tasks that *ended*, however
/// they ended, and is split into the five ways that can happen: a caller that
/// only needs resource accounting reads it or [`Stats::in_flight`], and a
/// caller that needs to know whether the work succeeded reads
/// [`Stats::succeeded`] and cannot get the answer wrong by reading `completed`
/// instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Stats {
    /// Tasks started, including any that have since finished.
    pub spawned: u64,
    /// Tasks that have finished, however they ended.
    ///
    /// Equal to `succeeded + failed + cancelled + aborted + panicked`. It is
    /// the accounting total, not a success count.
    pub completed: u64,
    /// Tasks whose body ran to completion and returned.
    pub succeeded: u64,
    /// Supervised processes that ended without exiting successfully.
    ///
    /// Zero for a supervisor that never ran one. A process the supervisor
    /// killed because its token was cancelled counts as `cancelled` instead:
    /// this counter answers "did the command succeed", not "did the child
    /// stop".
    pub failed: u64,
    /// Tasks that observed their token and returned because of it.
    pub cancelled: u64,
    /// Tasks the runtime dropped before they finished.
    pub aborted: u64,
    /// Tasks whose body panicked.
    pub panicked: u64,
    /// Spawn attempts refused by [`Supervisor::try_spawn`] at the bound.
    pub refused: u64,
    /// Terminal outcomes not retained because the report buffer was full.
    ///
    /// The buffer is capped at the in-flight bound, so a caller that spawns
    /// without ever draining its reports loses detail — never memory — and this
    /// counter is how it learns that it did.
    pub reports_dropped: u64,
}

impl Stats {
    /// Tasks started and not yet reaped.
    ///
    /// This is accounting, not a live count: `completed` advances when a task
    /// is joined (by [`Supervisor::reap`] or the reap every spawn performs), so
    /// a task that has ended and released its permit counts here until then,
    /// and the value can briefly exceed the in-flight bound. The bound itself
    /// is enforced by the permits, which a task holds for its whole run.
    ///
    /// Saturated subtraction: `completed` cannot exceed `spawned` because both
    /// advance on the same path, but a counter pair that silently wrapped would
    /// be worse than one that pins at zero.
    #[must_use]
    pub const fn in_flight(self) -> u64 {
        self.spawned.saturating_sub(self.completed)
    }
}

/// One in-flight admission: the permit, and — when the supervisor was built
/// with a [`TenancyPolicy`] — the tenant the permit is charged to.
///
/// The permit and the tenant travel together because their lifetimes are one
/// fact: a task that ends returns its permit *to the round* rather than to the
/// pool, and the round needs to know which tenant just freed a slot. A
/// supervisor without a policy never charges a tenant, so its leases carry no
/// charge and their drop is exactly the bare permit's drop.
#[cfg(feature = "script")]
struct Lease {
    /// The pool permit, taken out of the pool when the admission was granted,
    /// and taken out of the lease only by its own drop.
    permit: Option<OwnedSemaphorePermit>,
    /// The tenant this admission is charged to, when the supervisor installed a
    /// policy. `None` for every untenanted supervisor.
    charge: Option<TenantCharge>,
}

/// A build without tenancy has no round, so its lease is the permit alone: held
/// for the task's life and returned to the pool by its own drop.
#[cfg(not(feature = "script"))]
struct Lease {
    /// The pool permit, held only so that dropping the lease releases it.
    _permit: OwnedSemaphorePermit,
}

/// The tenant half of a [`Lease`], handed to the round when the lease drops.
#[cfg(feature = "script")]
struct TenantCharge {
    /// The shell that admitted this lease.
    shell: Arc<tenancy_support::TenancyShell>,
    /// The tenant charged for the permit.
    tenant: Tenant,
}

#[cfg(feature = "script")]
impl Lease {
    /// A lease for a supervisor with no policy: dropping it returns the permit
    /// to the pool, exactly as the raw permit did.
    fn plain(permit: OwnedSemaphorePermit) -> Self {
        Self {
            permit: Some(permit),
            charge: None,
        }
    }

    /// A lease charged to `tenant`, so its drop returns the permit to the round
    /// rather than to the pool.
    fn tenanted(
        permit: OwnedSemaphorePermit,
        shell: Arc<tenancy_support::TenancyShell>,
        tenant: Tenant,
    ) -> Self {
        Self {
            permit: Some(permit),
            charge: Some(TenantCharge { shell, tenant }),
        }
    }
}

/// The plain constructor for a `sync`-only build, where there is no tenancy and
/// the lease is the permit and nothing else.
impl Lease {
    /// A lease for a supervisor with no policy.
    #[cfg(not(feature = "script"))]
    fn plain(permit: OwnedSemaphorePermit) -> Self {
        Self { _permit: permit }
    }
}

/// Only a build with tenancy has a round to hand a permit to. Without `script`
/// there is no `Drop` at all: the lease is its `permit` field, and that field's
/// own drop returns the permit to the pool.
#[cfg(feature = "script")]
impl Drop for Lease {
    /// Hand a tenanted lease's permit to the round, which decides whose waiter
    /// it serves next. An untenanted lease does nothing here: its `permit` field
    /// drops with it and the permit returns to the pool.
    fn drop(&mut self) {
        if let Some(charge) = self.charge.take()
            && let Some(permit) = self.permit.take()
        {
            charge.shell.release(&charge.tenant, permit);
        }
    }
}

/// The tenant a call that named none is charged to.
///
/// One reserved name, so the untenanted calls share a ceiling and a queue rather
/// than reaching every tenant's permit — which is what "untenanted calls behave
/// as one implicit tenant" has to mean for the accounting to be readable. The
/// leading underscore marks it as the crate's own rather than any caller's.
///
/// Only consulted when a [`TenancyPolicy`] is installed: without one there is no
/// per-tenant state at all and every call shares the single pool, exactly as it
/// always did.
#[cfg(feature = "script")]
const IMPLICIT_TENANT: &str = "_implicit";

/// The tenant an untenanted call is charged to.
#[cfg(feature = "script")]
fn implicit_tenant() -> Tenant {
    Tenant::assumed(IMPLICIT_TENANT)
}

