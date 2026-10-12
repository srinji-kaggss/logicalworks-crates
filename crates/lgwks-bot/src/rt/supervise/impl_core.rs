impl Supervisor {
    /// Create a supervisor that runs at most `max_in_flight` tasks at once.
    ///
    /// A `max_in_flight` of zero is treated as one, and one above
    /// [`Semaphore::MAX_PERMITS`] is clamped to it, so there is no argument
    /// that produces an unbounded supervisor.
    #[must_use]
    pub fn new(max_in_flight: usize) -> Self {
        Self::with_clock(max_in_flight, Clock::wall())
    }

    /// Create a supervisor whose deadlines are governed by `clock`.
    ///
    /// The same supervisor as [`Supervisor::new`] with one difference: the clock
    /// its budget checks and its process deadlines read. A wall clock is the
    /// default, so nothing in production has to drive anything; a
    /// caller-advanceable one makes every budget in this supervisor exact, so a
    /// test can exhaust a five-minute budget in one call with no real wait and
    /// observe the *same* refusal a real overrun produces.
    ///
    /// Clones share the clock rather than copying it, so one advance from a test
    /// reaches every task the supervisor started.
    #[must_use]
    pub fn with_clock(max_in_flight: usize, clock: Clock) -> Self {
        let bound = max_in_flight.clamp(1, Semaphore::MAX_PERMITS);
        Self::assembled(bound, Arc::new(Semaphore::new(bound)), clock)
    }

    /// Create a supervisor that runs at most `max_in_flight` tasks at once
    /// **and** isolates each tenant's share of them under `policy`.
    ///
    /// The additive 1.x form of [`Supervisor::new`]: the same supervisor, the
    /// same report stream, the same terminal outcomes, plus the per-tenant
    /// ceiling, queue bound and weights a [`TenancyPolicy`] declares.
    ///
    /// # What changes for a caller that installs one
    ///
    /// - **The untenanted calls are one implicit tenant.**
    ///   [`Supervisor::spawn`], [`Supervisor::try_spawn`],
    ///   [`Supervisor::spawn_repeating`], `Supervisor::spawn_process` and
    ///   `Supervisor::run_process` are charged to one reserved tenant, so a
    ///   caller that predates tenancy shares that tenant's ceiling with every
    ///   other untenanted caller instead of reaching every tenant's permit. No
    ///   signature changes, and the one behavioural difference is a *visible*
    ///   refusal rather than an unbounded wait: a full implicit-tenant queue
    ///   refuses the call and counts it in [`Stats::refused`], where an
    ///   untenanted supervisor without tenancy still waits.
    /// - **The tenant-scoped calls answer with a typed refusal.**
    ///   [`Supervisor::spawn_for`] and `Supervisor::run_process_for` name the
    ///   tenant that is at capacity and the bound it reached, so a caller can
    ///   tell one loud tenant from a stopped supervisor.
    ///
    /// A supervisor built without a policy takes the path it always took: the
    /// same semaphore, the same waits, the same refusals. That is what makes this
    /// additive rather than a behaviour change for existing callers.
    ///
    /// ```
    /// use lgwks_bot::rt::supervise::Supervisor;
    /// use lgwks_bot::rt::tenancy::TenancyPolicy;
    /// use lgwks_bot::script::Tenant;
    ///
    /// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// let quiet = Tenant::new("quiet")?;
    /// // 64 permits in total, 32 per tenant: one tenant can strand at most 32
    /// // of them, so 32 stay reachable for everyone else however loud it is.
    /// let mut supervisor = Supervisor::with_tenancy(64, TenancyPolicy::new(32, 1_024));
    /// supervisor
    ///     .spawn_for(&quiet, |_token| async { /* one unit of work */ })
    ///     .await?;
    /// let report = supervisor.shutdown().await;
    /// assert!(report.is_clean(), "the quiet tenant's task ran to completion");
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "script")]
    #[must_use]
    pub fn with_tenancy(max_in_flight: usize, policy: TenancyPolicy) -> Self {
        let bound = max_in_flight.clamp(1, Semaphore::MAX_PERMITS);
        let permits = Arc::new(Semaphore::new(bound));
        let mut supervisor = Self::assembled(bound, Arc::clone(&permits), Clock::wall());
        supervisor.tenancy = Some(Arc::new(tenancy_support::TenancyShell::new(
            permits, policy,
        )));
        supervisor
    }

    /// The one constructor, so a supervisor differs only by what its admission
    /// needs.
    ///
    /// A private constructor rather than two struct literals: a second spelling
    /// of the counters would be a second place for one of them to be forgotten,
    /// and every report a caller reads is built from these fields.
    fn assembled(bound: usize, permits: Arc<Semaphore>, clock: Clock) -> Self {
        Self {
            token: CancellationToken::new(),
            set: JoinSet::new(),
            permits,
            #[cfg(all(unix, feature = "process"))]
            cleanup_owners: Arc::new(CleanupOwners::default()),
            #[cfg(all(windows, feature = "process"))]
            live_jobs: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            identities: BTreeMap::new(),
            reports: VecDeque::new(),
            report_cap: bound,
            max_in_flight: bound,
            next_task: 0,
            spawned: 0,
            completed: 0,
            succeeded: 0,
            failed: 0,
            cancelled: 0,
            aborted: 0,
            panicked: 0,
            refused: 0,
            reports_dropped: 0,
            clock,
            #[cfg(feature = "script")]
            tenancy: None,
        }
    }

    /// The clock that governs this supervisor's deadlines.
    #[must_use]
    pub fn clock(&self) -> &Clock {
        &self.clock
    }

    /// A token that is cancelled when this supervisor is cancelled or dropped.
    ///
    /// This is the token a body should hold. It is a *child* of the
    /// supervisor's, so cancelling it (or a supervisor shutting down) stops
    /// the body, while the body stopping does not stop the supervisor.
    #[must_use]
    pub fn child_token(&self) -> CancellationToken {
        self.token.child_token()
    }

    /// The child token and the run bounds a spec declares, resolved once.
    ///
    /// Both process runners start from these values, and a second copy is a
    /// second place for the ceiling, the deadline or the token to drift.
    #[cfg(all(any(unix, windows), feature = "process"))]
    fn prepare_process(&self, spec: &ProcessSpec) -> (CancellationToken, RunBounds) {
        (
            self.child_token(),
            RunBounds {
                deadline: spec.deadline_duration(),
                out_limit: spec.stdout_capture(),
                err_limit: spec.stderr_capture(),
            },
        )
    }

    /// Whether this supervisor has been cancelled or is shutting down.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Cancel every task this supervisor owns, without waiting for them.
    ///
    /// The asynchronous counterpart that also waits, and returns what it
    /// drained, is [`Supervisor::shutdown`].
    pub fn cancel(&self) {
        self.token.cancel();
    }

    /// A snapshot of the counters.
    #[must_use]
    pub fn stats(&self) -> Stats {
        Stats {
            spawned: self.spawned,
            completed: self.completed,
            succeeded: self.succeeded,
            failed: self.failed,
            cancelled: self.cancelled,
            aborted: self.aborted,
            panicked: self.panicked,
            refused: self.refused,
            reports_dropped: self.reports_dropped,
        }
    }

    /// Number of retained Unix process-group cleanup obligations still charged
    /// to this supervisor's in-flight capacity.
    #[cfg(all(unix, feature = "process"))]
    #[must_use]
    pub fn pending_cleanup_count(&self) -> usize {
        self.cleanup_owners.pending_count()
    }

    /// A bounded, point-in-time view of what this supervisor owns right now.
    ///
    /// Reads the supervisor's own admission and reporting fields directly — the
    /// same fields [`Supervisor::stats`] and the permit pool are built from — so
    /// what it reports and what the supervisor does cannot drift apart. It
    /// allocates a `Vec` proportional to the in-flight ceiling and retains
    /// nothing afterwards.
    ///
    /// # What it deliberately does not carry
    ///
    /// Terminal outcomes stay in the report stream ([`Supervisor::next_report`]),
    /// where the retention cap already governs them. A snapshot with its own copy
    /// of them would be a second ledger, and two ledgers disagree.
    ///
    /// ```
    /// # use lgwks_bot::rt::supervise::Supervisor;
    /// # async fn example() {
    /// let mut supervisor = Supervisor::new(2);
    /// let snapshot = supervisor.snapshot();
    /// assert_eq!(snapshot.max_in_flight(), 2);
    /// assert_eq!(snapshot.free(), 2, "a fresh supervisor holds both permits");
    /// assert_eq!(snapshot.next_action(), supervisor::NextAction::Admit);
    /// # }
    /// # mod supervisor { pub use lgwks_bot::rt::supervise::NextAction; }
    /// ```
    #[must_use]
    pub fn snapshot(&self) -> SupervisorSnapshot {
        let live_limit = self.max_in_flight;
        let mut live: Vec<LiveTask> = Vec::with_capacity(self.identities.len().min(live_limit));
        let mut live_truncated: usize = 0;
        // Spawn order, from the supervisor's own identity map: the engine's join
        // order is arbitrary, and a listing that reordered itself between two
        // reads could not be asserted against.
        let mut ordered: Vec<TaskId> = self.identities.values().copied().collect();
        ordered.sort_unstable_by_key(|task| task.get());
        for (index, task) in ordered.into_iter().enumerate() {
            if index < live_limit {
                live.push(LiveTask {
                    task,
                    state: TaskState::Running { position: index },
                });
            } else {
                live_truncated = live_truncated.saturating_add(1);
            }
        }
        #[cfg(all(unix, feature = "process"))]
        let pending_cleanups = self.cleanup_owners.pending_count();
        SupervisorSnapshot {
            max_in_flight: self.max_in_flight,
            stats: self.stats(),
            live,
            live_truncated,
            cancelled: self.token.is_cancelled(),
            reports_pending: self.reports.len(),
            #[cfg(all(unix, feature = "process"))]
            pending_cleanups,
        }
    }

    /// The oldest terminal outcome this supervisor has not yet handed over.
    ///
    /// `None` means every task that has ended has been reported and read. A
    /// caller that reads this in a loop holds no history at all, which is how
    /// the cap in [`Stats::reports_dropped`] is kept from ever being reached.
    pub fn next_report(&mut self) -> Option<TaskOutcome> {
        #[cfg(all(unix, feature = "process"))]
        self.refresh_cleanup_owners();
        self.reports.pop_front()
    }

    /// Join every task that has already finished, returning how many were
    /// joined.
    ///
    /// Each entry point reaps before placing its task, which is what keeps the
    /// retained set proportional to the live bound rather than to the total
    /// number of spawns; the tenanted entry joins a bounded number per call
    /// (`REAP_PER_ADMISSION`), which keeps the same bound. It is public because a caller who has stopped spawning
    /// and wants the counters to settle should not have to spawn a task to make
    /// that happen.
    ///
    /// Every joined task produces exactly one [`TaskOutcome`], readable through
    /// [`Supervisor::next_report`].
    pub fn reap(&mut self) -> usize {
        self.reap_at_most(usize::MAX)
    }

    /// Join at most `limit` already-finished tasks, returning how many were
    /// joined.
    ///
    /// The bounded form a tenanted admission takes (`REAP_PER_ADMISSION`), so
    /// one call's bookkeeping is a constant rather than every task any tenant
    /// finished since the last call.
    fn reap_at_most(&mut self, limit: usize) -> usize {
        let mut reaped: usize = 0;
        while reaped < limit {
            let Some(joined) = self.set.try_join_next_with_id() else {
                break;
            };
            self.absorb(joined, Retention::Capped);
            reaped = reaped.saturating_add(1);
        }
        #[cfg(all(unix, feature = "process"))]
        self.refresh_cleanup_owners();
        reaped
    }

    /// Wait until every task this supervisor has placed has ended, cancelling
    /// none of them, and return how many were joined.
    ///
    /// The awaiting counterpart of [`Supervisor::reap`]: `reap` joins what has
    /// already finished, so a caller that wanted to wait through it had to poll
    /// it on a timer, and the runtime's timer rounds a short sleep up to its
    /// one-millisecond tick — a fixed cost on every wait that the task set's own
    /// completion wakeup does not have (#269). Each task is joined the moment it
    /// ends and absorbed exactly as `reap` absorbs it, so every one produces one
    /// [`TaskOutcome`] under the same retention cap.
    ///
    /// Unlike [`Supervisor::shutdown`] nothing is cancelled and the supervisor
    /// stays usable: this is the wait for "everything I started has finished",
    /// not for "stop". A body that never returns keeps this waiting; bound it
    /// with the runtime's `timeout`, or cancel, when that is possible. The wait
    /// is bounded by the in-flight ceiling, because nothing is spawned while it
    /// runs.
    pub async fn wait_idle(&mut self) -> usize {
        let mut joined: usize = 0;
        while let Some(next) = self.set.join_next_with_id().await {
            self.absorb(next, Retention::Capped);
            joined = joined.saturating_add(1);
        }
        #[cfg(all(unix, feature = "process"))]
        self.refresh_cleanup_owners();
        joined
    }

    /// Place `body` on this supervisor, waiting for a free slot if the bound is
    /// reached.
    ///
    /// `body` is called **before** anything is spawned, so it does not itself
    /// need to be `Send` or `'static`: only the future it returns does. That is
    /// what lets a call site capture borrowed state while the task it starts
    /// holds none.
    ///
    /// The permit is acquired before the spawn, so the wait is real
    /// backpressure: nothing is queued on this module's behalf.
    pub async fn spawn<F, Fut>(&mut self, body: F)
    where
        F: FnOnce(CancellationToken) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        // The fence lives in `claim`, which is why the constructor below is
        // safe to call: a cancelled supervisor never returns a permit, so the
        // body handed to it is never built. The refusal is counted there.
        let Some(lease) = self.claim().await else {
            return;
        };
        self.place_body(lease, body);
    }
    /// Place `body` on this supervisor, refusing if the bound is reached or
    /// the supervisor is cancelled.
    ///
    /// The non-blocking counterpart to [`Supervisor::spawn`]. A refusal is a
    /// value, not a silent drop, and the two refusals name different worlds:
    /// [`TrySpawnRefusal::AtCapacity`] is a healthy supervisor that is full,
    /// while [`TrySpawnRefusal::Cancelled`] is a supervisor that has stopped
    /// admitting. Either way the body constructor has not run when the refusal
    /// is returned.
    ///
    /// # Errors
    ///
    /// [`TrySpawnRefusal::AtCapacity`] when every slot is taken;
    /// [`TrySpawnRefusal::Cancelled`] once the supervisor has been cancelled.
    pub fn try_spawn<F, Fut>(&mut self, body: F) -> Result<(), TrySpawnRefusal>
    where
        F: FnOnce(CancellationToken) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let Some(lease) = self.claim_now() else {
            // `claim_now` counted the refusal; which world refused is the one
            // fact the counter cannot carry, so it is read here for the type.
            if self.token.is_cancelled() {
                let refusal = Err(TrySpawnRefusal::Cancelled);
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "try_spawn: returning an error to the caller");
                return refusal;
            }
            let refusal = Err(TrySpawnRefusal::AtCapacity);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "try_spawn: returning an error to the caller");
            return refusal;
        };
        self.place_body(lease, body);
        Ok(())
    }

    /// Place `body` on this supervisor under `tenant`, waiting for a free slot if
    /// the bound is reached.
    ///
    /// The tenant-scoped counterpart of [`Supervisor::spawn`], and the only
    /// door that *charges* a tenant: the admission this call waits for counts
    /// against `tenant`'s own ceiling under the supervisor's [`TenancyPolicy`],
    /// and the slot it is given was offered by the deficit round robin because
    /// `tenant` had work waiting when a permit freed. That is the whole
    /// contract — one tenant cannot hold more than its share of the supervisor,
    /// so it cannot starve another.
    ///
    /// `body` is called **after** the admission is granted and before the task
    /// is placed, exactly as [`Supervisor::spawn`] calls it, so it need not be
    /// `Send` or `'static` itself; only the future it returns is.
    ///
    /// # Errors
    ///
    /// [`SpawnRefused::TenantAtCapacity`] when `tenant` already holds its full
    /// queue of waiting admissions, naming the tenant and the bound;
    /// [`SpawnRefused::Cancelled`] once the supervisor has stopped admitting.
    /// Other tenants are unaffected by either refusal.
    #[cfg(feature = "script")]
    pub async fn spawn_for<F, Fut>(&mut self, tenant: &Tenant, body: F) -> Result<(), SpawnRefused>
    where
        F: FnOnce(CancellationToken) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        // A supervisor with no policy admits this exactly as `spawn` does — the
        // implicit tenant is the whole story, and there is no per-tenant ceiling
        // to charge it to.
        let Some(shell) = self.tenancy.clone() else {
            self.spawn(body).await;
            return Ok(());
        };
        let lease = match self.claim_tenanted(&shell, tenant).await {
            Ok(lease) => lease,
            Err(refusal) => {
                self.refused = self.refused.saturating_add(1);
                let refused = Err(refusal);
                lgwks_std::trace::debug!(error = ?refused.as_ref().err(), "spawn_for: returning an error to the caller");
                return refused;
            }
        };
        self.place_body(lease, body);
        Ok(())
    }

    /// How many of `tenant`'s admissions this supervisor holds, and how many it
    /// has waiting.
    ///
    /// `(in flight, queued)`, read from the same per-tenant counters admission
    /// charges and the round serves (INV-BOT-31's rule: inspection reads the
    /// admission's own state, never a second ledger). `(0, 0)` for a tenant that
    /// has never arrived, for an untenanted supervisor, and for one whose entry
    /// the scheduler has already retired as idle.
    #[cfg(feature = "script")]
    #[must_use]
    pub fn tenant_capacity(&self, tenant: &Tenant) -> (usize, usize) {
        self.tenancy
            .as_ref()
            .map_or((0, 0), |shell| shell.counts_of(tenant))
    }

    /// Cumulative admission-side cost of this supervisor's tenancy round.
    ///
    /// `(admit_ops, admit_nanos, release_ops, release_nanos)`: how many
    /// lock-hold arrival decisions the round has taken and their cumulative
    /// wall time in nanoseconds, beside how many releases completing tasks
    /// took through the same lock and theirs. The arrival half is the
    /// arrival's wait slot plus the round and the pump under the scheduler
    /// lock (#351's admission side). The wait to *take* that lock is not
    /// charged: it is another decision holding the round, counted by that
    /// decision. A tenanted admission's reap, its task spawn, and the
    /// executor's own polls are outside the counters too, so the remainder of
    /// a measured admission wait is the scheduling share. `(0, 0, 0, 0)` for a
    /// supervisor built without a [`TenancyPolicy`], which takes no round
    /// decisions at all.
    ///
    /// ```
    /// use lgwks_bot::rt::supervise::Supervisor;
    /// use lgwks_bot::rt::tenancy::TenancyPolicy;
    /// use lgwks_bot::script::Tenant;
    ///
    /// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// let quiet = Tenant::new("quiet")?;
    /// let mut supervisor = Supervisor::with_tenancy(4, TenancyPolicy::new(2, 8));
    /// supervisor
    ///     .spawn_for(&quiet, |_token| async { /* one unit of work */ })
    ///     .await?;
    /// let (arrivals, _, _, _) = supervisor.admission_cost();
    /// assert!(arrivals >= 1, "one admission took at least one arrival decision");
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "script")]
    #[must_use]
    pub fn admission_cost(&self) -> (u64, u64, u64, u64) {
        self.tenancy
            .as_ref()
            .map_or((0, 0, 0, 0), |shell| shell.decision_cost())
    }

    /// How many tenants currently hold at least one waiting admission.
    ///
    /// The load the round is carrying, for a report or an assertion about
    /// whether a fleet of tenants is actually backed up. Zero for an untenanted
    /// supervisor, which has no per-tenant queues at all.
    #[cfg(feature = "script")]
    #[must_use]
    pub fn tenants_waiting(&self) -> usize {
        self.tenancy
            .as_ref()
            .map_or(0, |shell| shell.tenants_waiting())
    }

    /// Start a task that repeats under `budget` until the budget or the
    /// supervisor ends it.
    ///
    /// This is the shape a long-running bot loop should take. The bound is a
    /// required argument, so there is no call that reads as "loop forever".
    ///
    /// The loop's [`Outcome`] is not discarded: a loop the supervisor cancelled
    /// is reported as [`TaskOutcome::Cancelled`] and one that spent its budget
    /// as [`TaskOutcome::Completed`], so the two are distinguishable from
    /// outside the task.
    pub async fn spawn_repeating<F, Fut>(&mut self, budget: Budget, body: F)
    where
        F: FnMut(u64) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let Some(permit) = self.claim().await else {
            return;
        };
        let token = self.child_token();
        let clock = self.clock.clone();
        self.place(permit, async move {
            match repeat_on(&clock, &token, budget, body).await {
                Outcome::Exhausted { .. } => TaskEnd::Completed,
                Outcome::Cancelled { .. } => TaskEnd::Cancelled,
            }
        });
    }

}
